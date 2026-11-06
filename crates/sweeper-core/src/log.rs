//! Append-only local activity log. Records what happened to which path,
//! never file contents. Nothing leaves the machine.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::Env;

/// Writer for `~/Library/Logs/Sweeper Kit/sweeper-kit.log`.
#[derive(Debug, Clone)]
pub struct ActionLog {
    path: PathBuf,
}

impl ActionLog {
    pub fn new(env: &Env) -> ActionLog {
        ActionLog {
            path: env.home.join("Library/Logs/Sweeper Kit/sweeper-kit.log"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `<unix-secs>\t<action>\t<path>`. Logging is best effort: a
    /// failure to write the log must never block the user's action.
    pub fn record(&self, action: &str, path: &Path) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "{secs}\t{}\t{}\n",
            sanitize(action),
            sanitize(&path.to_string_lossy())
        );
        if let Some(dir) = self.path.parent() {
            if fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Keeps each record on one line.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::current();
        env.home = dir.path().to_path_buf();
        let log = ActionLog::new(&env);
        log.record("trash", Path::new("/a/b\nc"));
        log.record("delete", Path::new("/x"));
        let text = fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("trash\t/a/b c"));
    }
}
