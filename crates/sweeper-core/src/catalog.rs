//! Discovery of installed applications.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::fsutil::tree_size;
use crate::system::{CommandRunner, Spotlight};

/// How an app was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppSource {
    AppStore,
    Homebrew { cask: String },
    Other,
}

/// An installed application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub path: PathBuf,
    pub name: String,
    pub version: Option<String>,
    pub bundle_id: String,
    /// Filled lazily by [`team_id`].
    pub team_id: Option<String>,
    /// Filled by [`compute_sizes`].
    pub size: Option<u64>,
    #[serde(with = "unix_secs")]
    pub last_opened: Option<SystemTime>,
    pub source: AppSource,
    pub is_apple: bool,
}

mod unix_secs {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        let secs = t.and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()));
        secs.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        let secs = Option::<u64>::deserialize(d)?;
        Ok(secs.map(|s| UNIX_EPOCH + Duration::from_secs(s)))
    }
}

/// Reads `Contents/Info.plist` of an app bundle.
pub(crate) fn read_info_plist(app: &Path) -> Option<plist::Dictionary> {
    plist::Value::from_file(app.join("Contents/Info.plist"))
        .ok()?
        .into_dictionary()
}

/// Bundle identifier of the app at `app`, if its Info.plist is readable.
pub(crate) fn read_bundle_id(app: &Path) -> Option<String> {
    read_info_plist(app)?
        .get("CFBundleIdentifier")?
        .as_string()
        .map(str::to_owned)
}

/// Lists installed apps.
pub struct AppCatalog;

impl AppCatalog {
    /// Enumerates `*.app` one level deep in each app dir. Unreadable or
    /// malformed bundles are skipped. Sorted by name.
    pub fn scan(env: &Env, spotlight: &dyn Spotlight) -> Vec<AppInfo> {
        let casks = cask_index(&env.caskroom_dirs);
        let mut seen = HashSet::new();
        let mut apps = Vec::new();
        for dir in &env.app_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
                .collect();
            paths.sort();
            for path in paths {
                let Ok(canonical) = std::fs::canonicalize(&path) else {
                    continue;
                };
                if !seen.insert(canonical) {
                    continue;
                }
                if let Some(app) = read_app(&path, &casks) {
                    apps.push(app);
                }
            }
        }
        let paths: Vec<PathBuf> = apps.iter().map(|a| a.path.clone()).collect();
        let used = spotlight.last_used(&paths);
        for app in &mut apps {
            app.last_opened = used.get(&app.path).copied();
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps
    }
}

/// Maps an app file name (`Foo.app`) to the cask that installed it.
fn cask_index(caskrooms: &[PathBuf]) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for room in caskrooms {
        let Ok(casks) = std::fs::read_dir(room) else {
            continue;
        };
        for cask in casks.flatten() {
            let cask_name = cask.file_name().to_string_lossy().into_owned();
            let Ok(versions) = std::fs::read_dir(cask.path()) else {
                continue;
            };
            for version in versions.flatten() {
                let Ok(items) = std::fs::read_dir(version.path()) else {
                    continue;
                };
                for item in items.flatten() {
                    let file = item.file_name().to_string_lossy().into_owned();
                    if file.to_ascii_lowercase().ends_with(".app") {
                        index.entry(file).or_insert_with(|| cask_name.clone());
                    }
                }
            }
        }
    }
    index
}

fn read_app(path: &Path, casks: &HashMap<String, String>) -> Option<AppInfo> {
    let info = read_info_plist(path)?;
    let text = |key: &str| {
        info.get(key)
            .and_then(|v| v.as_string())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let bundle_id = text("CFBundleIdentifier")?;
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    let name = text("CFBundleDisplayName")
        .or_else(|| text("CFBundleName"))
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let source = if path.join("Contents/_MASReceipt/receipt").exists() {
        AppSource::AppStore
    } else if let Some(cask) = casks.get(&file_name) {
        AppSource::Homebrew { cask: cask.clone() }
    } else {
        AppSource::Other
    };
    Some(AppInfo {
        path: path.to_path_buf(),
        name,
        version: text("CFBundleShortVersionString"),
        is_apple: bundle_id.starts_with("com.apple."),
        bundle_id,
        team_id: None,
        size: None,
        last_opened: None,
        source,
    })
}

/// Computes bundle sizes in parallel. Apps stay `None` if `cancel` is raised.
pub fn compute_sizes(apps: &mut [AppInfo], cancel: &AtomicBool) {
    apps.par_iter_mut().for_each(|app| {
        app.size = tree_size(&app.path, cancel);
    });
}

/// Reads the code-signing team identifier with `codesign`. Called lazily when
/// the user selects an app, since it spawns a process.
pub fn team_id(app: &Path, runner: &dyn CommandRunner) -> Option<String> {
    let path = app.to_string_lossy();
    let out = runner
        .run("codesign", &["-dv", "--verbose=2", path.as_ref()])
        .ok()?;
    out.stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_owned)
}
