//! The single gatekeeper deciding whether a path may be removed.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// `~/Library` subfolders that are removable roots.
pub(crate) const LIBRARY_ROOT_DIRS: [&str; 12] = [
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
];

/// Decides which paths may be removed. Build once per [`Env`], call
/// [`PathPolicy::check`] before every mutation.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    home: PathBuf,
    roots: Vec<PathBuf>,
    raw_roots: Vec<PathBuf>,
    extra_roots: Vec<PathBuf>,
    raw_extra_roots: Vec<PathBuf>,
    rejected_extra_roots: Vec<PathBuf>,
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
        for sub in LIBRARY_ROOT_DIRS {
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
            home,
            roots,
            raw_roots,
            extra_roots: Vec::new(),
            raw_extra_roots: Vec::new(),
            rejected_extra_roots: Vec::new(),
            absolute_denies,
            containers: lib.join("Containers"),
            group_containers: lib.join("Group Containers"),
            data_dir: canonicalize_lenient(&env.data_dir),
            protected_paths,
            protected_ids,
            self_id: env.self_bundle_id.clone(),
        }
    }

    /// Adds removable roots outside the normal ones (e.g. `~/.npm`). A root
    /// must lie strictly inside the home folder and must neither be inside nor
    /// contain a hard-denied area (Keychains, Mail, Documents, ...); others are
    /// ignored and listed by [`PathPolicy::rejected_extra_roots`]. Paths inside
    /// an extra root that sits in the Downloads folder are allowed; every other
    /// check still applies.
    pub fn with_extra_roots(mut self, roots: &[PathBuf]) -> PathPolicy {
        for raw in roots {
            let canonical = canonicalize_lenient(raw);
            if !self.is_valid_extra_root(&canonical) {
                if !self.rejected_extra_roots.contains(raw) {
                    self.rejected_extra_roots.push(raw.clone());
                }
                continue;
            }
            if !self.extra_roots.contains(&canonical) {
                self.extra_roots.push(canonical);
            }
            if !self.raw_extra_roots.contains(raw) {
                self.raw_extra_roots.push(raw.clone());
            }
        }
        self
    }

    /// Extra roots that were refused by [`PathPolicy::with_extra_roots`].
    pub fn rejected_extra_roots(&self) -> &[PathBuf] {
        &self.rejected_extra_roots
    }

    fn is_valid_extra_root(&self, canonical: &Path) -> bool {
        let downloads = self.home.join("Downloads");
        let mut hard = self
            .absolute_denies
            .iter()
            .map(|(dir, _)| dir)
            .filter(|dir| **dir != downloads)
            .chain(std::iter::once(&self.data_dir));
        canonical.starts_with(&self.home)
            && canonical != self.home
            && !is_photos_library(canonical)
            && !hard.any(|d| canonical.starts_with(d) || d.starts_with(canonical))
    }

    fn all_roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.roots.iter().chain(self.extra_roots.iter())
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
            self.raw_roots
                .iter()
                .chain(self.raw_extra_roots.iter())
                .any(|r| a.starts_with(r))
                && std::fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink())
        })
    }

    fn check_resolved(&self, canonical: &Path) -> Result<(), PolicyError> {
        self.check_denied(canonical)?;
        if self.all_roots().any(|r| r == canonical) {
            return Err(PolicyError::OutsideAllowedRoots);
        }
        if !self.all_roots().any(|r| canonical.starts_with(r)) {
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
            if canonical.starts_with(dir) && !self.opened_by_extra_root(canonical, dir) {
                return Err(PolicyError::Denied(format!("{} ({why})", dir.display())));
            }
        }
        if canonical.starts_with(&self.data_dir) {
            return Err(PolicyError::Denied("Sweeper Kit's own data".into()));
        }
        if is_photos_library(canonical) {
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

    /// True if an extra root lying inside the denied folder `dir` contains
    /// `canonical`. Hard-denied folders never hold a valid extra root.
    fn opened_by_extra_root(&self, canonical: &Path, dir: &Path) -> bool {
        self.extra_roots
            .iter()
            .any(|r| r.starts_with(dir) && canonical.starts_with(r))
    }

    /// Refuses `.app` bundles of Apple, Sweeper Kit itself or protected ids.
    fn check_bundles(&self, canonical: &Path) -> Result<(), PolicyError> {
        for a in canonical.ancestors() {
            if self.all_roots().any(|r| r == a) {
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

/// `/Library` subfolders whose children may be removed with administrator
/// access.
pub(crate) const SYSTEM_LIBRARY_ROOT_DIRS: [&str; 7] = [
    "Application Support",
    "Caches",
    "Preferences",
    "LaunchAgents",
    "LaunchDaemons",
    "PrivilegedHelperTools",
    "Logs",
];

/// Directories below the system root that hold `*.app` bundles the helper may remove.
const SYSTEM_APP_DIRS: [&str; 2] = ["Applications", "Applications/Utilities"];

/// Where the helper keeps items moved out of `/Library` until the user
/// confirms they are not needed (relative to the system root).
pub(crate) const STAGING_REL: &str = "Library/Application Support/Sweeper Kit/Staging";

/// Decides which system-scope paths the privileged helper may modify. It is
/// deliberately much narrower than [`PathPolicy`]: only children of a few
/// `/Library` folders and direct `*.app` children of `/Applications` pass.
///
/// macOS file systems are case-insensitive by default, so every deny rule is
/// compared with Unicode lowercasing. APFS folds more than that (and
/// normalizes), so the Apple and Sweeper Kit folders are additionally denied by
/// identity: a candidate whose path or any existing ancestor is the same
/// directory (device and inode) is refused however it is spelled.
#[derive(Debug, Clone)]
pub struct SystemPolicy {
    system_root: PathBuf,
    roots: Vec<PathBuf>,
    raw_roots: Vec<PathBuf>,
    app_dirs: Vec<PathBuf>,
    denied: Vec<(PathBuf, &'static str)>,
    denied_ids: Arc<Mutex<Vec<DeniedIdentity>>>,
    staging: PathBuf,
    launch_daemons: PathBuf,
    launch_agents: PathBuf,
    protected_paths: Vec<PathBuf>,
    protected_ids: HashSet<String>,
    self_id: String,
}

impl SystemPolicy {
    /// `protected` mixes absolute paths and bundle identifiers, like
    /// [`PathPolicy::new`]; `~/` entries are ignored because they never lie in
    /// system scope.
    pub fn new(env: &Env, protected: &[String]) -> SystemPolicy {
        let system_root = canonicalize_lenient(&env.system_root);
        let lib = system_root.join("Library");
        let raw_lib = env.system_root.join("Library");
        let mut roots = Vec::new();
        let mut raw_roots = Vec::new();
        for sub in SYSTEM_LIBRARY_ROOT_DIRS {
            roots.push(lib.join(sub));
            raw_roots.push(raw_lib.join(sub));
        }
        let mut app_dirs = Vec::new();
        for sub in SYSTEM_APP_DIRS {
            app_dirs.push(system_root.join(sub));
            raw_roots.push(env.system_root.join(sub));
        }
        let staging = system_root.join(STAGING_REL);
        let denied = vec![
            (lib.join("Apple"), "Apple files"),
            (lib.join("Application Support/Apple"), "Apple files"),
            (
                lib.join("Application Support/Sweeper Kit"),
                "Sweeper Kit's own data",
            ),
            (lib.join("Logs/Sweeper Kit"), "Sweeper Kit's own data"),
            (
                canonicalize_lenient(&env.data_dir),
                "Sweeper Kit's own data",
            ),
        ];
        let mut protected_paths = Vec::new();
        let mut protected_ids = HashSet::new();
        for entry in protected {
            if entry.starts_with('/') {
                protected_paths.push(PathBuf::from(entry));
                protected_paths.push(canonicalize_lenient(Path::new(entry)));
            } else if !entry.is_empty() && !entry.starts_with("~/") {
                protected_ids.insert(entry.clone());
            }
        }
        // The first three entries are the Apple and Sweeper Kit folders under
        // the system root.
        let denied_ids = denied[..3]
            .iter()
            .map(|(dir, why)| DeniedIdentity {
                dir: dir.clone(),
                why,
                id: dir_identity(dir),
            })
            .collect();
        SystemPolicy {
            denied_ids: Arc::new(Mutex::new(denied_ids)),
            launch_daemons: lib.join("LaunchDaemons"),
            launch_agents: lib.join("LaunchAgents"),
            system_root,
            roots,
            raw_roots,
            app_dirs,
            denied,
            staging,
            protected_paths,
            protected_ids,
            self_id: env.self_bundle_id.clone(),
        }
    }

    /// Canonical system root (`/` on a real Mac).
    pub fn system_root(&self) -> &Path {
        &self.system_root
    }

    /// Canonical `<root>/Library/Application Support/Sweeper Kit/Staging`.
    pub fn staging_dir(&self) -> &Path {
        &self.staging
    }

    /// Canonical `<root>/Library/LaunchDaemons`.
    pub fn launch_daemons_dir(&self) -> &Path {
        &self.launch_daemons
    }

    /// Canonical `<root>/Library/LaunchAgents`.
    pub fn launch_agents_dir(&self) -> &Path {
        &self.launch_agents
    }

    /// Validates an existing path and returns its canonical form. Symlinks
    /// are never followed for the final component, but their target must also
    /// be allowed.
    pub fn check(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let (parent, name) = split_absolute(path)?;
        let canonical = std::fs::canonicalize(parent).map_err(io_err)?.join(name);
        let meta = std::fs::symlink_metadata(&canonical).map_err(io_err)?;
        self.check_in_context(&canonical, parent)?;
        if meta.file_type().is_symlink() {
            let target =
                std::fs::canonicalize(&canonical).map_err(|_| PolicyError::SymlinkEscape)?;
            self.check_resolved(&target)
                .map_err(|_| PolicyError::SymlinkEscape)?;
        }
        Ok(canonical)
    }

    /// Like [`SystemPolicy::check`] for a path that does not have to exist
    /// yet, such as the destination of a restore. Only the parent is
    /// resolved.
    pub fn check_destination(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let (parent, name) = split_absolute(path)?;
        let canonical = canonicalize_lenient(parent).join(name);
        self.check_in_context(&canonical, parent)?;
        Ok(canonical)
    }

    /// Validates a path inside the staging directory, which must be exactly
    /// `<staging>/<batch>/<item>`. The path need not exist.
    pub fn check_staging(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let (parent, name) = split_absolute(path)?;
        let canonical = canonicalize_lenient(parent).join(name);
        let depth = canonical
            .strip_prefix(&self.staging)
            .map(|rel| rel.components().count())
            .unwrap_or(0);
        if depth == 2 {
            Ok(canonical)
        } else {
            Err(PolicyError::OutsideAllowedRoots)
        }
    }

    /// Refuses bundle identifiers of Apple, Sweeper Kit itself or protected apps.
    pub fn check_bundle_id(&self, id: &str) -> Result<(), PolicyError> {
        if id == self.self_id {
            return Err(PolicyError::Denied("Sweeper Kit itself".into()));
        }
        if id.to_ascii_lowercase().starts_with("com.apple.") {
            return Err(PolicyError::Denied("Apple app".into()));
        }
        if self.protected_ids.contains(id) {
            return Err(PolicyError::Protected);
        }
        Ok(())
    }

    fn check_in_context(&self, canonical: &Path, raw_parent: &Path) -> Result<(), PolicyError> {
        match self.check_resolved(canonical) {
            Err(PolicyError::OutsideAllowedRoots) if self.has_escaping_ancestor(raw_parent) => {
                Err(PolicyError::SymlinkEscape)
            }
            other => other,
        }
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
        let in_root = self
            .roots
            .iter()
            .any(|r| canonical.starts_with(r) && canonical != r);
        let app_child = canonical
            .parent()
            .is_some_and(|p| self.app_dirs.iter().any(|d| d == p))
            && canonical
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("app"));
        if !in_root && !app_child {
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

    /// True if `path` or one of its existing ancestors is the Apple or
    /// Sweeper Kit folder itself (same device and inode), whatever the
    /// spelling. Folders that did not exist at construction are looked up
    /// again here.
    pub fn denied_by_identity(&self, path: &Path) -> bool {
        self.denied_identity(path).is_some()
    }

    fn denied_identity(&self, path: &Path) -> Option<String> {
        let mut ids = self.denied_ids.lock().unwrap_or_else(|e| e.into_inner());
        for entry in ids.iter_mut() {
            if entry.id.is_none() {
                entry.id = dir_identity(&entry.dir);
            }
        }
        if ids.iter().all(|e| e.id.is_none()) {
            return None;
        }
        for ancestor in path.ancestors() {
            let Some(id) = std::fs::symlink_metadata(ancestor)
                .ok()
                .and_then(|m| identity_of(&m))
            else {
                continue;
            };
            if let Some(hit) = ids.iter().find(|e| e.id == Some(id)) {
                return Some(format!("{} ({})", hit.dir.display(), hit.why));
            }
        }
        None
    }

    fn check_denied(&self, canonical: &Path) -> Result<(), PolicyError> {
        for (dir, why) in &self.denied {
            if starts_with_ci(canonical, dir) {
                return Err(PolicyError::Denied(format!("{} ({why})", dir.display())));
            }
        }
        if let Some(what) = self.denied_identity(canonical) {
            return Err(PolicyError::Denied(what));
        }
        let relative = canonical
            .strip_prefix(&self.system_root)
            .unwrap_or(canonical);
        let apple = relative.components().any(|c| {
            c.as_os_str()
                .to_str()
                .map_or(true, |s| s.to_lowercase().starts_with("com.apple."))
        });
        if apple {
            return Err(PolicyError::Denied("Apple item".into()));
        }
        Ok(())
    }

    /// Refuses `.app` bundles of Apple, Sweeper Kit itself or protected ids.
    fn check_bundles(&self, canonical: &Path) -> Result<(), PolicyError> {
        for a in canonical.ancestors() {
            if self
                .roots
                .iter()
                .chain(self.app_dirs.iter())
                .any(|r| r == a)
            {
                break;
            }
            if !a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) {
                continue;
            }
            if let Some(id) = read_bundle_id(a) {
                self.check_bundle_id(&id)?;
            }
        }
        Ok(())
    }
}

/// Splits an absolute path without `.`/`..` into parent and final component.
fn split_absolute(path: &Path) -> Result<(&Path, &OsStr), PolicyError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(PolicyError::OutsideAllowedRoots);
    }
    match (path.parent(), path.file_name()) {
        (Some(p), Some(n)) => Ok((p, n)),
        _ => Err(PolicyError::OutsideAllowedRoots),
    }
}

/// A folder that must never be touched, tracked by (device, inode).
#[derive(Debug)]
struct DeniedIdentity {
    dir: PathBuf,
    why: &'static str,
    id: Option<(u64, u64)>,
}

/// (device, inode) of an existing directory, not following a final symlink.
fn dir_identity(dir: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(dir)
        .ok()
        .filter(|m| m.is_dir())
        .and_then(|m| identity_of(&m))
}

#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity_of(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// `starts_with` on whole components, ignoring case (Unicode lowercasing).
fn starts_with_ci(path: &Path, base: &Path) -> bool {
    let mut p = path.components();
    base.components().all(|b| {
        p.next().is_some_and(|c| {
            c.as_os_str().to_string_lossy().to_lowercase()
                == b.as_os_str().to_string_lossy().to_lowercase()
        })
    })
}

fn is_photos_library(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.to_ascii_lowercase().ends_with(".photoslibrary"))
    })
}

/// First path component of `path` below `base`, if `path` is inside `base`.
fn first_below<'a>(path: &'a Path, base: &Path) -> Option<&'a str> {
    let rel = path.strip_prefix(base).ok()?;
    rel.components()
        .next()
        .map(Component::as_os_str)
        .and_then(OsStr::to_str)
}
