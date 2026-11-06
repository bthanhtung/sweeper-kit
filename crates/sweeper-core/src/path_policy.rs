//! The single gatekeeper deciding whether a path may be removed.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::catalog::read_bundle_id;
use crate::env::Env;

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("path is outside the locations Sweeper Kit may modify")]
    OutsideAllowedRoots,
    #[error("path is off limits: {0}")]
    Denied(String),
    #[error("symbolic link points outside the allowed locations")]
    SymlinkEscape,
    #[error("path is on the protected list")]
    Protected,
    #[error("path does not exist")]
    NotFound,
    #[error("cannot inspect path: {0}")]
    Io(String),
}

fn io_err(e: io::Error) -> PolicyError {
    if e.kind() == io::ErrorKind::NotFound {
        PolicyError::NotFound
    } else {
        PolicyError::Io(e.to_string())
    }
}

/// Resolves symlinks of the longest existing ancestor and appends the rest,
/// so roots that do not exist yet still compare correctly.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            return tail.iter().rev().fold(c, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Decides which paths may be removed. Build once per [`Env`], call
/// [`PathPolicy::check`] before every mutation.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    roots: Vec<PathBuf>,
    raw_roots: Vec<PathBuf>,
    absolute_denies: Vec<(PathBuf, &'static str)>,
    containers: PathBuf,
    group_containers: PathBuf,
    data_dir: PathBuf,
    protected_paths: Vec<PathBuf>,
    protected_ids: HashSet<String>,
    self_id: String,
}

impl PathPolicy {
    /// `protected` mixes absolute paths (or `~/...`) and bundle identifiers.
    pub fn new(env: &Env, protected: &[String]) -> PathPolicy {
        let home = canonicalize_lenient(&env.home);
        let lib = home.join("Library");
        let mut raw_roots: Vec<PathBuf> = env.app_dirs.clone();
        let mut roots: Vec<PathBuf> = env
            .app_dirs
            .iter()
            .map(|p| canonicalize_lenient(p))
            .collect();
        for sub in [
            "Application Support",
            "Containers",
            "Group Containers",
            "Application Scripts",
            "Caches",
            "HTTPStorages",
            "WebKit",
            "Preferences",
            "Cookies",
            "Saved Application State",
            "Logs",
            "LaunchAgents",
        ] {
            roots.push(lib.join(sub));
            raw_roots.push(env.library().join(sub));
        }

        let mut absolute_denies: Vec<(PathBuf, &'static str)> =
            ["/System", "/usr", "/bin", "/sbin"]
                .iter()
                .map(|p| (canonicalize_lenient(Path::new(p)), "system directory"))
                .collect();
        for d in [
            "Documents",
            "Desktop",
            "Pictures",
            "Movies",
            "Music",
            "Downloads",
        ] {
            absolute_denies.push((home.join(d), "personal folder"));
        }
        for d in ["Keychains", "Mail", "Messages", "Mobile Documents"] {
            absolute_denies.push((lib.join(d), "private data"));
        }

        let mut protected_paths = Vec::new();
        let mut protected_ids = HashSet::new();
        for entry in protected {
            if let Some(rest) = entry.strip_prefix("~/") {
                protected_paths.push(home.join(rest));
                protected_paths.push(env.home.join(rest));
            } else if entry.starts_with('/') {
                protected_paths.push(PathBuf::from(entry));
                protected_paths.push(canonicalize_lenient(Path::new(entry)));
            } else if !entry.is_empty() {
                protected_ids.insert(entry.clone());
            }
        }

        PathPolicy {
            roots,
            raw_roots,
            absolute_denies,
            containers: lib.join("Containers"),
            group_containers: lib.join("Group Containers"),
            data_dir: canonicalize_lenient(&env.data_dir),
            protected_paths,
            protected_ids,
            self_id: env.self_bundle_id.clone(),
        }
    }

    /// Validates `path` and returns its canonical form (parent canonicalized,
    /// final component kept). Symlinks are never followed for the final
    /// component, but their target must also be allowed.
    pub fn check(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(PolicyError::OutsideAllowedRoots);
        }
        let (parent, name) = match (path.parent(), path.file_name()) {
            (Some(p), Some(n)) => (p, n),
            _ => return Err(PolicyError::OutsideAllowedRoots),
        };
        let canonical = std::fs::canonicalize(parent).map_err(io_err)?.join(name);
        let meta = std::fs::symlink_metadata(&canonical).map_err(io_err)?;

        match self.check_resolved(&canonical) {
            Err(PolicyError::OutsideAllowedRoots) if self.has_escaping_ancestor(parent) => {
                return Err(PolicyError::SymlinkEscape)
            }
            other => other?,
        }
        if meta.file_type().is_symlink() {
            let target =
                std::fs::canonicalize(&canonical).map_err(|_| PolicyError::SymlinkEscape)?;
            self.check_resolved(&target)
                .map_err(|_| PolicyError::SymlinkEscape)?;
        }
        Ok(canonical)
    }

    /// True if an ancestor lexically inside an allowed root is a symlink.
    fn has_escaping_ancestor(&self, parent: &Path) -> bool {
        parent.ancestors().any(|a| {
            self.raw_roots.iter().any(|r| a.starts_with(r))
                && std::fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink())
        })
    }

    fn check_resolved(&self, canonical: &Path) -> Result<(), PolicyError> {
        self.check_denied(canonical)?;
        if self.roots.iter().any(|r| r == canonical) {
            return Err(PolicyError::OutsideAllowedRoots);
        }
        if !self.roots.iter().any(|r| canonical.starts_with(r)) {
            return Err(PolicyError::OutsideAllowedRoots);
        }
        if self
            .protected_paths
            .iter()
            .any(|p| canonical.starts_with(p))
        {
            return Err(PolicyError::Protected);
        }
        self.check_bundles(canonical)
    }

    fn check_denied(&self, canonical: &Path) -> Result<(), PolicyError> {
        for (dir, why) in &self.absolute_denies {
            if canonical.starts_with(dir) {
                return Err(PolicyError::Denied(format!("{} ({why})", dir.display())));
            }
        }
        if canonical.starts_with(&self.data_dir) {
            return Err(PolicyError::Denied("Sweeper Kit's own data".into()));
        }
        if canonical.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| s.to_ascii_lowercase().ends_with(".photoslibrary"))
        }) {
            return Err(PolicyError::Denied("Photos library".into()));
        }
        if let Some(first) = first_below(canonical, &self.containers) {
            if first.starts_with("com.apple.") {
                return Err(PolicyError::Denied("Apple container".into()));
            }
        }
        if let Some(first) = first_below(canonical, &self.group_containers) {
            if first.starts_with("group.com.apple.") || first.contains(".com.apple.") {
                return Err(PolicyError::Denied("Apple group container".into()));
            }
        }
        Ok(())
    }

    /// Refuses `.app` bundles of Apple, Sweeper Kit itself or protected ids.
    fn check_bundles(&self, canonical: &Path) -> Result<(), PolicyError> {
        for a in canonical.ancestors() {
            if self.roots.iter().any(|r| r == a) {
                break;
            }
            let is_app = a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
            if !is_app {
                continue;
            }
            if let Some(id) = read_bundle_id(a) {
                if id == self.self_id {
                    return Err(PolicyError::Denied("Sweeper Kit itself".into()));
                }
                if id.starts_with("com.apple.") {
                    return Err(PolicyError::Denied("Apple app".into()));
                }
                if self.protected_ids.contains(&id) {
                    return Err(PolicyError::Protected);
                }
            }
        }
        Ok(())
    }
}

/// First path component of `path` below `base`, if `path` is inside `base`.
fn first_below<'a>(path: &'a Path, base: &Path) -> Option<&'a str> {
    let rel = path.strip_prefix(base).ok()?;
    rel.components()
        .next()
        .map(Component::as_os_str)
        .and_then(OsStr::to_str)
}
