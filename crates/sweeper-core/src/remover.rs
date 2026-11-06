//! Executes a removal plan with a policy re-check before every mutation and
//! records Trash moves so they can be undone.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::log::ActionLog;
use crate::path_policy::PathPolicy;
use crate::scanner::ScanResult;
use crate::system::Trasher;

/// One path scheduled for removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub path: PathBuf,
    pub size: u64,
}

/// The checked, non-refused items of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlan {
    pub items: Vec<PlanItem>,
    pub total_size: u64,
}

impl RemovalPlan {
    /// Collects every checked item that the policy did not refuse, including
    /// other copies the user chose to check.
    pub fn from_scan(scan: &ScanResult) -> RemovalPlan {
        let items: Vec<PlanItem> = scan
            .items()
            .chain(scan.other_copies.iter())
            .filter(|i| i.checked && i.refused.is_none())
            .map(|i| PlanItem {
                path: i.path.clone(),
                size: i.size,
            })
            .collect();
        let total_size = items.iter().map(|i| i.size).fold(0, u64::saturating_add);
        RemovalPlan { items, total_size }
    }
}

/// Where removed items go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemovalMode {
    /// Move to the Trash (undoable).
    Trash,
    /// Delete permanently.
    Delete,
}

/// Progress callback payload, sent after each item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub path: PathBuf,
}

/// An item that was removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub original: PathBuf,
    /// Present in [`RemovalMode::Trash`].
    pub trash_location: Option<PathBuf>,
    pub size: u64,
}

/// Outcome of [`Remover::execute`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalReport {
    pub removed: Vec<Removed>,
    /// Items left in place, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Non-fatal problems, e.g. the undo log could not be written.
    pub warnings: Vec<String>,
}

/// Removes items safely.
pub struct Remover<'a> {
    env: &'a Env,
    policy: &'a PathPolicy,
    trasher: &'a dyn Trasher,
    undo: UndoLog,
}

impl<'a> Remover<'a> {
    pub fn new(
        env: &'a Env,
        policy: &'a PathPolicy,
        trasher: &'a dyn Trasher,
        undo: UndoLog,
    ) -> Self {
        Remover {
            env,
            policy,
            trasher,
            undo,
        }
    }

    pub fn undo_log(&self) -> &UndoLog {
        &self.undo
    }

    /// Removes each plan item. The policy is evaluated again right before
    /// each mutation, so a path swapped for a symlink after scanning is
    /// caught.
    pub fn execute(
        &self,
        plan: &RemovalPlan,
        mode: RemovalMode,
        progress: &mut dyn FnMut(Progress),
    ) -> RemovalReport {
        let log = ActionLog::new(self.env);
        let batch_id = new_id();
        let total = plan.items.len();
        let mut report = RemovalReport::default();

        for (index, item) in plan.items.iter().enumerate() {
            match self.remove_one(item, mode, &batch_id, &log, &mut report.warnings) {
                Ok(removed) => report.removed.push(removed),
                Err(reason) => report.failed.push((item.path.clone(), reason)),
            }
            progress(Progress {
                done: index + 1,
                total,
                path: item.path.clone(),
            });
        }
        report
    }

    fn remove_one(
        &self,
        item: &PlanItem,
        mode: RemovalMode,
        batch_id: &str,
        log: &ActionLog,
        warnings: &mut Vec<String>,
    ) -> Result<Removed, String> {
        let path = self.policy.check(&item.path).map_err(|e| e.to_string())?;
        match mode {
            RemovalMode::Trash => {
                let location = self.trasher.trash(&path).map_err(|e| e.to_string())?;
                log.record("trash", &path);
                if let Err(e) = self.undo.append(&path, &location, item.size, batch_id) {
                    warnings.push(format!(
                        "{} was moved to the Trash but could not be added to the undo list: {e}",
                        path.display()
                    ));
                }
                Ok(Removed {
                    original: path,
                    trash_location: Some(location),
                    size: item.size,
                })
            }
            RemovalMode::Delete => {
                self.trasher.delete(&path).map_err(|e| e.to_string())?;
                log.record("delete", &path);
                Ok(Removed {
                    original: path,
                    trash_location: None,
                    size: item.size,
                })
            }
        }
    }
}

/// One undoable Trash move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: String,
    /// Unix seconds.
    pub timestamp: u64,
    pub original: PathBuf,
    /// Where the item is now: the Trash, or for `admin` entries the privileged
    /// staging folder.
    pub trash_location: PathBuf,
    pub size: u64,
    pub batch_id: String,
    /// The item was moved by the privileged helper into the root-owned staging
    /// folder. Only the helper can move it back (see
    /// [`crate::admin::restore_admin_entries`]); [`UndoLog::restore`] refuses.
    #[serde(default)]
    pub admin: bool,
}

/// Why a restore was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreError {
    #[error("no undo entry with id {0}")]
    UnknownEntry(String),
    #[error("{0} already exists; it was not overwritten")]
    OriginalExists(PathBuf),
    #[error("{0} is no longer in the Trash")]
    TrashItemMissing(PathBuf),
    #[error("undo entry has an invalid path")]
    InvalidEntry,
    #[error("this item was removed with administrator access and can only be restored with it")]
    RequiresAdmin,
    #[error("cannot restore: {0}")]
    Io(String),
}

/// JSON file listing items moved to the Trash.
#[derive(Debug, Clone)]
pub struct UndoLog {
    path: PathBuf,
}

impl UndoLog {
    /// Log at an explicit file path.
    pub fn new(path: PathBuf) -> UndoLog {
        UndoLog { path }
    }

    /// Log at `data_dir/undo-log.json`.
    pub fn for_env(env: &Env) -> UndoLog {
        UndoLog::new(env.data_dir.join("undo-log.json"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All entries, oldest first. A missing file is an empty log.
    pub fn entries(&self) -> io::Result<Vec<UndoEntry>> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn append(
        &self,
        original: &Path,
        trash_location: &Path,
        size: u64,
        batch_id: &str,
    ) -> io::Result<()> {
        self.append_entry(original, trash_location, size, batch_id, false)
    }

    /// Records an item the privileged helper moved to `staging_path`.
    pub(crate) fn append_admin(
        &self,
        original: &Path,
        staging_path: &Path,
        size: u64,
        batch_id: &str,
    ) -> io::Result<()> {
        self.append_entry(original, staging_path, size, batch_id, true)
    }

    fn append_entry(
        &self,
        original: &Path,
        trash_location: &Path,
        size: u64,
        batch_id: &str,
        admin: bool,
    ) -> io::Result<()> {
        let mut entries = self.entries()?;
        entries.push(UndoEntry {
            id: new_id(),
            timestamp: unix_now(),
            original: original.to_path_buf(),
            trash_location: trash_location.to_path_buf(),
            size,
            batch_id: batch_id.to_string(),
            admin,
        });
        self.write(&entries)
    }

    /// Writes to a temporary file in the same directory, then renames it over
    /// the log so readers never see a partial file.
    fn write(&self, entries: &[UndoEntry]) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("undo log path has no parent"))?;
        fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(entries).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, &self.path)
    }

    /// Moves the Trash item back to its original location and drops the
    /// entry. Never overwrites an existing item.
    pub fn restore(&self, id: &str) -> Result<PathBuf, RestoreError> {
        let io_err = |e: io::Error| RestoreError::Io(e.to_string());
        let mut entries = self.entries().map_err(io_err)?;
        let index = entries
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| RestoreError::UnknownEntry(id.to_string()))?;
        let original = restore_entry(&entries[index], None)?;
        entries.remove(index);
        self.write(&entries).map_err(io_err)?;
        Ok(original)
    }

    /// Entries grouped by batch, newest batch first. `in_trash` tells whether
    /// the item can still be restored.
    pub fn batches(&self) -> io::Result<Vec<UndoBatch>> {
        let mut batches: Vec<UndoBatch> = Vec::new();
        for entry in self.entries()? {
            // The staging folder is root-only, so the app cannot look inside;
            // the helper reports a missing item when the restore is attempted.
            let in_trash = entry.admin || fs::symlink_metadata(&entry.trash_location).is_ok();
            let status = UndoEntryStatus { entry, in_trash };
            match batches
                .iter_mut()
                .find(|b| b.batch_id == status.entry.batch_id)
            {
                Some(batch) => {
                    batch.timestamp = batch.timestamp.max(status.entry.timestamp);
                    batch.size = batch.size.saturating_add(status.entry.size);
                    batch.entries.push(status);
                }
                None => batches.push(UndoBatch {
                    batch_id: status.entry.batch_id.clone(),
                    timestamp: status.entry.timestamp,
                    size: status.entry.size,
                    entries: vec![status],
                }),
            }
        }
        // Stable sort keeps the log order for batches with equal timestamps.
        batches.reverse();
        batches.sort_by_key(|b| std::cmp::Reverse(b.timestamp));
        Ok(batches)
    }

    /// Restores several entries with one log rewrite. Each restore is
    /// independent and refuses on its own; a missing parent folder is
    /// re-created only inside the locations `policy` allows.
    pub fn restore_many(
        &self,
        ids: &[String],
        policy: &PathPolicy,
    ) -> Vec<(String, Result<PathBuf, RestoreError>)> {
        let io_err = |e: io::Error| RestoreError::Io(e.to_string());
        let mut entries = match self.entries() {
            Ok(e) => e,
            Err(e) => {
                return ids
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            Err(io_err(io::Error::new(e.kind(), e.to_string()))),
                        )
                    })
                    .collect()
            }
        };
        let mut results = Vec::with_capacity(ids.len());
        let mut restored = HashSet::new();
        for id in ids {
            let entry = entries
                .iter()
                .find(|e| e.id == *id && !restored.contains(&e.id));
            let result = match entry {
                Some(entry) => restore_entry(entry, Some(policy)),
                None => Err(RestoreError::UnknownEntry(id.clone())),
            };
            if result.is_ok() {
                restored.insert(id.clone());
            }
            results.push((id.clone(), result));
        }
        if !restored.is_empty() {
            entries.retain(|e| !restored.contains(&e.id));
            if let Err(e) = self.write(&entries) {
                // The items are back in place; only the bookkeeping failed.
                for (_, result) in results.iter_mut().filter(|(_, r)| r.is_ok()) {
                    *result = Err(io_err(io::Error::new(
                        e.kind(),
                        format!("restored, but the undo list could not be updated: {e}"),
                    )));
                }
            }
        }
        results
    }

    /// Drops entries from the log without touching any file, for items that
    /// are no longer in the Trash. Returns how many entries were dropped.
    pub fn forget(&self, ids: &[String]) -> io::Result<usize> {
        let mut entries = self.entries()?;
        let before = entries.len();
        entries.retain(|e| !ids.contains(&e.id));
        let dropped = before - entries.len();
        if dropped > 0 {
            self.write(&entries)?;
        }
        Ok(dropped)
    }
}

/// An [`UndoEntry`] together with whether its Trash item still exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoEntryStatus {
    pub entry: UndoEntry,
    pub in_trash: bool,
}

/// All entries created by one removal run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoBatch {
    pub batch_id: String,
    /// Unix seconds of the newest entry.
    pub timestamp: u64,
    pub entries: Vec<UndoEntryStatus>,
    pub size: u64,
}

/// Moves one Trash item back. With a `policy`, a missing parent folder is
/// re-created one level at a time and each new level must be allowed.
fn restore_entry(entry: &UndoEntry, policy: Option<&PathPolicy>) -> Result<PathBuf, RestoreError> {
    let io_err = |e: io::Error| RestoreError::Io(e.to_string());
    if entry.admin {
        return Err(RestoreError::RequiresAdmin);
    }
    let sane = |p: &Path| {
        p.is_absolute()
            && p.components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
    };
    if !sane(&entry.original) || !sane(&entry.trash_location) {
        return Err(RestoreError::InvalidEntry);
    }
    if fs::symlink_metadata(&entry.trash_location).is_err() {
        return Err(RestoreError::TrashItemMissing(entry.trash_location.clone()));
    }
    if fs::symlink_metadata(&entry.original).is_ok() {
        return Err(RestoreError::OriginalExists(entry.original.clone()));
    }
    if let Some(parent) = entry.original.parent() {
        match policy {
            Some(policy) => create_parents(parent, policy)?,
            None => fs::create_dir_all(parent).map_err(io_err)?,
        }
    }
    fs::rename(&entry.trash_location, &entry.original).map_err(io_err)?;
    Ok(entry.original.clone())
}

/// Creates the missing folders of `dir` top-down. A level the policy does not
/// allow is removed again together with the ones created before it.
fn create_parents(dir: &Path, policy: &PathPolicy) -> Result<(), RestoreError> {
    let mut missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|a| fs::symlink_metadata(a).is_err())
        .collect();
    missing.reverse();
    let mut created: Vec<&Path> = Vec::new();
    for level in missing {
        let made = fs::create_dir(level).map_err(|e| RestoreError::Io(e.to_string()));
        let allowed = made.is_ok() && policy.check(level).is_ok();
        if made.is_ok() {
            created.push(level);
        }
        if let Err(e) = made {
            roll_back(&created);
            return Err(e);
        }
        if !allowed {
            roll_back(&created);
            return Err(RestoreError::InvalidEntry);
        }
    }
    Ok(())
}

fn roll_back(created: &[&Path]) {
    for dir in created.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unique within a machine: nanosecond clock plus a process-wide counter.
pub(crate) fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}
