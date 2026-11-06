//! Boundaries to the operating system: commands, Spotlight, running
//! processes, the Trash and permissions. Each boundary is a trait so the rest
//! of the crate is testable without touching the real machine.
//!
//! The real implementations target macOS. On other platforms the Trash and
//! process backends return [`io::ErrorKind::Unsupported`] / nothing; Windows
//! and Linux backends arrive in later milestones.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::env::Env;

/// Deep link to the Full Disk Access pane of System Settings.
pub const FDA_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Result of a finished command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code, or -1 when terminated by a signal.
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Runs external programs. Implementations must never invoke a shell.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

/// Runs programs with `std::process::Command` and an argument array.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let out = Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Bundle identifiers are interpolated into a Spotlight query and passed to
/// `defaults`; only `[A-Za-z0-9][A-Za-z0-9.-]*` is ever accepted.
pub fn is_valid_bundle_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Spotlight metadata queries.
pub trait Spotlight: Send + Sync {
    /// All `.app` bundles on disk with this bundle identifier.
    fn copies_of(&self, bundle_id: &str) -> Vec<PathBuf>;
    /// `kMDItemLastUsedDate` for each path that has one.
    fn last_used(&self, paths: &[PathBuf]) -> HashMap<PathBuf, SystemTime>;
}

/// Spotlight through the `mdfind` and `mdls` command line tools.
pub struct RealSpotlight {
    runner: Box<dyn CommandRunner>,
}

impl RealSpotlight {
    pub fn new() -> Self {
        Self::with_runner(Box::new(RealRunner))
    }

    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        RealSpotlight { runner }
    }
}

impl Default for RealSpotlight {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps argument lists well below the kernel limit.
const MDLS_BATCH: usize = 100;

impl Spotlight for RealSpotlight {
    fn copies_of(&self, bundle_id: &str) -> Vec<PathBuf> {
        if !is_valid_bundle_id(bundle_id) {
            return Vec::new();
        }
        let query = format!("kMDItemCFBundleIdentifier == '{bundle_id}'");
        match self.runner.run("mdfind", &[&query]) {
            Ok(out) if out.success() => out
                .stdout
                .lines()
                .map(PathBuf::from)
                .filter(|p| {
                    p.is_absolute() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn last_used(&self, paths: &[PathBuf]) -> HashMap<PathBuf, SystemTime> {
        let mut result = HashMap::new();
        for chunk in paths.chunks(MDLS_BATCH) {
            let owned: Vec<String> = chunk
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let mut args: Vec<&str> = vec!["-raw", "-name", "kMDItemLastUsedDate"];
            args.extend(owned.iter().map(String::as_str));
            let Ok(out) = self.runner.run("mdls", &args) else {
                continue;
            };
            if !out.success() {
                continue;
            }
            // With -raw, one value per file, separated by NUL (newline as fallback).
            let sep = if out.stdout.contains('\0') {
                '\0'
            } else {
                '\n'
            };
            for (path, value) in chunk.iter().zip(out.stdout.split(sep)) {
                if let Some(t) = parse_mdls_date(value) {
                    result.insert(path.clone(), t);
                }
            }
        }
        result
    }
}

/// Parses `2024-05-01 10:11:12 +0000`; `(null)` and garbage give `None`.
pub(crate) fn parse_mdls_date(value: &str) -> Option<SystemTime> {
    let mut parts = value.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let offset = parts.next().unwrap_or("+0000");
    let mut d = date.split('-').map(|n| n.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|n| n.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let off: i64 = offset.get(1..5)?.parse().ok()?;
    let off_secs = sign * ((off / 100) * 3600 + (off % 100) * 60);
    // Days since epoch from a proleptic Gregorian date.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss - off_secs;
    if secs >= 0 {
        Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

/// A running application instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningApp {
    pub pid: i32,
    pub path: PathBuf,
    pub name: String,
}

/// Inspects and terminates running applications.
pub trait Processes: Send + Sync {
    /// Instances whose bundle identifier equals `bundle_id`.
    fn running(&self, bundle_id: &str) -> Vec<RunningApp>;
    /// True while the process exists and has not terminated.
    fn is_running(&self, pid: i32) -> bool;
    /// Politely asks the process to quit; true if the request was delivered.
    fn terminate(&self, pid: i32) -> bool;
    /// Kills the process; true if the request was delivered.
    fn force_terminate(&self, pid: i32) -> bool;
}

/// Process control through `NSRunningApplication` (macOS only).
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProcesses;

#[cfg(target_os = "macos")]
impl Processes for RealProcesses {
    fn running(&self, bundle_id: &str) -> Vec<RunningApp> {
        use objc2_app_kit::NSWorkspace;
        let workspace = NSWorkspace::sharedWorkspace();
        workspace
            .runningApplications()
            .iter()
            .filter(|app| {
                app.bundleIdentifier()
                    .is_some_and(|id| id.to_string() == bundle_id)
            })
            .map(|app| RunningApp {
                pid: app.processIdentifier(),
                path: app
                    .bundleURL()
                    .and_then(|u| u.path())
                    .map(|p| PathBuf::from(p.to_string()))
                    .unwrap_or_default(),
                name: app
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            })
            .collect()
    }

    fn is_running(&self, pid: i32) -> bool {
        use objc2_app_kit::NSRunningApplication;
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| !app.isTerminated())
    }

    fn terminate(&self, pid: i32) -> bool {
        use objc2_app_kit::NSRunningApplication;
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| app.terminate())
    }

    fn force_terminate(&self, pid: i32) -> bool {
        use objc2_app_kit::NSRunningApplication;
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| app.forceTerminate())
    }
}

#[cfg(not(target_os = "macos"))]
impl Processes for RealProcesses {
    fn running(&self, _bundle_id: &str) -> Vec<RunningApp> {
        Vec::new()
    }

    fn is_running(&self, _pid: i32) -> bool {
        false
    }

    fn terminate(&self, _pid: i32) -> bool {
        false
    }

    fn force_terminate(&self, _pid: i32) -> bool {
        false
    }
}

/// Moves items to the Trash or deletes them.
pub trait Trasher: Send + Sync {
    /// Moves `path` to the Trash and returns where it ended up.
    fn trash(&self, path: &Path) -> io::Result<PathBuf>;
    /// Permanently deletes `path` without following symlinks.
    fn delete(&self, path: &Path) -> io::Result<()>;
}

/// Trash via `NSFileManager` (macOS only); permanent delete via `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealTrasher;

impl Trasher for RealTrasher {
    #[cfg(target_os = "macos")]
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};
        let path_str = path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path is not valid UTF-8")
        })?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
        let mut resulting = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
            .map_err(|e| io::Error::other(e.localizedDescription().to_string()))?;
        resulting
            .and_then(|u| u.path())
            .map(|p| PathBuf::from(p.to_string()))
            .ok_or_else(|| io::Error::other("Trash did not report the new location"))
    }

    #[cfg(not(target_os = "macos"))]
    fn trash(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Trash is only supported on macOS in this version",
        ))
    }

    fn delete(&self, path: &Path) -> io::Result<()> {
        let meta = std::fs::symlink_metadata(path)?;
        if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
    }
}

/// Whether Sweeper Kit may read protected locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FdaStatus {
    Granted,
    Denied,
    Unknown,
}

/// Probes Full Disk Access by opening the TCC database read-only. macOS does
/// not expose a direct API; a permission error means access is missing, a
/// missing file means the probe cannot tell.
pub fn full_disk_access(env: &Env) -> FdaStatus {
    let probe = env
        .home
        .join("Library/Application Support/com.apple.TCC/TCC.db");
    match std::fs::File::open(probe) {
        Ok(_) => FdaStatus::Granted,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => FdaStatus::Denied,
        Err(_) => FdaStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_id_validation() {
        assert!(is_valid_bundle_id("com.foo.app-1"));
        assert!(!is_valid_bundle_id(""));
        assert!(!is_valid_bundle_id(".com"));
        assert!(!is_valid_bundle_id("com.foo'; rm"));
        assert!(!is_valid_bundle_id("com foo"));
        assert!(!is_valid_bundle_id("com.fo\u{f6}"));
    }

    #[test]
    fn mdls_dates() {
        let t = parse_mdls_date("2024-05-01 10:11:12 +0000").unwrap();
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_714_558_272
        );
        let t = parse_mdls_date("2024-05-01 12:11:12 +0200").unwrap();
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_714_558_272
        );
        assert!(parse_mdls_date("(null)").is_none());
        assert!(parse_mdls_date("").is_none());
    }

    #[test]
    fn fda_unknown_without_tcc_db() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::current();
        env.home = dir.path().to_path_buf();
        assert_eq!(full_disk_access(&env), FdaStatus::Unknown);
    }
}
