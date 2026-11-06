//! Small file-system helpers shared by the catalog and the scanner.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Total size in bytes of `path`, never following symlinks. Unreadable
/// entries count as zero. Returns `None` when `cancel` was raised.
pub(crate) fn tree_size(path: &Path, cancel: &AtomicBool) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&current) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Some(total)
}
