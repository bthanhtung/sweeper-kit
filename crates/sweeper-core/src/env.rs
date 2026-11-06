//! Process environment description. Everything that touches the file system
//! takes an [`Env`] so tests can run against a temporary fake home.

use std::path::PathBuf;

/// Bundle identifier of Sweeper Kit itself; the app never removes its own data.
pub const SELF_BUNDLE_ID: &str = "dev.sweeperkit.app";

/// Locations and identity the library operates on.
#[derive(Debug, Clone)]
pub struct Env {
    /// User home directory.
    pub home: PathBuf,
    /// Directories scanned for `*.app` bundles (user scope).
    pub app_dirs: Vec<PathBuf>,
    /// Homebrew Caskroom directories used to detect cask installs.
    pub caskroom_dirs: Vec<PathBuf>,
    /// Numeric user id, used for `launchctl bootout gui/<uid>`.
    pub uid: u32,
    /// Bundle identifier of Sweeper Kit; its bundle and data are protected.
    pub self_bundle_id: String,
    /// Sweeper Kit data directory (undo log); always protected.
    pub data_dir: PathBuf,
    /// Root of the machine-wide file system (`/`). System-scope locations
    /// such as `/Library/LaunchDaemons` resolve below it; tests point it at a
    /// temporary directory.
    pub system_root: PathBuf,
}

impl Env {
    /// Builds the environment of the current user from `$HOME`.
    ///
    /// If `$HOME` is unset or relative the home becomes `/var/empty`, which is
    /// never writable, so no leftover location resolves to anything removable.
    pub fn current() -> Env {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/var/empty"));
        Env {
            app_dirs: vec![
                PathBuf::from("/Applications"),
                PathBuf::from("/Applications/Utilities"),
                home.join("Applications"),
            ],
            caskroom_dirs: vec![
                PathBuf::from("/opt/homebrew/Caskroom"),
                PathBuf::from("/usr/local/Caskroom"),
            ],
            uid: uid_of(&home),
            self_bundle_id: SELF_BUNDLE_ID.to_string(),
            data_dir: home.join("Library/Application Support/Sweeper Kit"),
            system_root: PathBuf::from("/"),
            home,
        }
    }

    /// `~/Library`.
    pub fn library(&self) -> PathBuf {
        self.home.join("Library")
    }
}

#[cfg(unix)]
fn uid_of(home: &std::path::Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(home).map(|m| m.uid()).unwrap_or(0)
}

#[cfg(not(unix))]
fn uid_of(_home: &std::path::Path) -> u32 {
    0
}
