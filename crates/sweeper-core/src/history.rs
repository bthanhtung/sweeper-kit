//! Remembers apps that Sweeper Kit uninstalled, so a later orphan scan can
//! tell their leftovers apart from data of unknown origin.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::catalog::AppInfo;
use crate::env::Env;

/// One uninstalled app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedApp {
    pub bundle_id: String,
    pub name: String,
    /// Unix seconds.
    pub removed_at: u64,
    /// Where the bundle used to be.
    pub app_path: PathBuf,
}

/// JSON file listing uninstalled apps.
#[derive(Debug, Clone)]
pub struct RemovedApps {
    path: PathBuf,
}

impl RemovedApps {
    /// History at an explicit file path.
    pub fn new(path: PathBuf) -> RemovedApps {
        RemovedApps { path }
    }

    /// History at `data_dir/removed-apps.json`.
    pub fn for_env(env: &Env) -> RemovedApps {
        RemovedApps::new(env.data_dir.join("removed-apps.json"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All entries, oldest first. A missing file is an empty history.
    pub fn entries(&self) -> io::Result<Vec<RemovedApp>> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// True if the app was uninstalled by Sweeper Kit. An unreadable history
    /// counts as "not known", which only lowers confidence.
    pub fn contains(&self, bundle_id: &str) -> bool {
        self.entries()
            .is_ok_and(|e| e.iter().any(|r| r.bundle_id == bundle_id))
    }

    /// Records `app`, replacing an earlier entry with the same identifier.
    /// Apple apps and apps without an identifier are never recorded.
    pub fn record(&self, app: &AppInfo) -> io::Result<()> {
        if app.is_apple || app.bundle_id.is_empty() {
            return Ok(());
        }
        let mut entries = self.entries()?;
        entries.retain(|e| e.bundle_id != app.bundle_id);
        entries.push(RemovedApp {
            bundle_id: app.bundle_id.clone(),
            name: app.name.clone(),
            removed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            app_path: app.path.clone(),
        });
        self.write(&entries)
    }

    /// Drops the entry for `bundle_id`, e.g. after its leftovers were cleaned.
    pub fn forget(&self, bundle_id: &str) -> io::Result<()> {
        let mut entries = self.entries()?;
        let before = entries.len();
        entries.retain(|e| e.bundle_id != bundle_id);
        if entries.len() == before {
            return Ok(());
        }
        self.write(&entries)
    }

    /// Temporary file plus rename, so readers never see a partial file.
    fn write(&self, entries: &[RemovedApp]) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("history path has no parent"))?;
        fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(entries).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, &self.path)
    }
}
