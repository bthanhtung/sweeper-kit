//! Administrator (system scope) operations: the manifest protocol shared by
//! the app and the privileged helper, the `osascript` invocation that elevates
//! the helper, and the code the helper runs as root.
//!
//! # Trust model
//!
//! The app builds an [`AdminManifest`] and runs `sweeper-helper` through
//! `osascript ... with administrator privileges`; macOS asks the user for
//! their password once per invocation. The helper is the only code that runs
//! as root. It trusts nothing in the manifest: [`validate_manifest`] checks
//! every operation against [`SystemPolicy`] before anything happens, and
//! [`execute_manifest`] checks each path again immediately before touching it.
//!
//! # macOS limitations
//!
//! * `osascript` elevation shows a generic prompt and cannot prove which
//!   binary will run; the helper must live in a location only administrators
//!   can write (the signed app bundle in `/Applications`). A signed
//!   `SMAppService` daemon with an XPC interface would avoid the repeated
//!   password prompt and is the intended future option.
//! * Items are moved with `rename(2)` only. A copy fallback is deliberately
//!   absent, so an item on another volume than `/Library` is reported as
//!   failed instead of being copied as root.
//! * Items under SIP-protected locations and files protected by TCC may fail
//!   even as root; the failure is reported per operation.
//! * Staged items are root-owned and unreadable by the app, so their
//!   presence cannot be verified without the helper.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::path_policy::SystemPolicy;
use crate::remover::{new_id, RestoreError, UndoLog};
use crate::system::CommandRunner;

/// Version of the manifest format.
pub const MANIFEST_VERSION: u32 = 1;
/// Most operations one manifest may hold.
pub const MAX_OPS: usize = 500;
/// Largest serialized manifest, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Absolute path of `launchctl`; the helper never consults `PATH`.
pub const LAUNCHCTL: &str = "/bin/launchctl";
/// Absolute path of `pkgutil`; the helper never consults `PATH`.
pub const PKGUTIL: &str = "/usr/sbin/pkgutil";
const OSASCRIPT: &str = "/usr/bin/osascript";
/// `launchctl bootout` exit codes meaning the service was not loaded.
const LAUNCHCTL_NOT_LOADED: [i32; 2] = [3, 113];
const MAX_BATCH_ID: usize = 64;
const MAX_PKG_ID: usize = 255;
const MAX_ERROR_TEXT: usize = 300;

/// Why an administrator request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminError {
    #[error("Administrator authorization was cancelled")]
    Cancelled,
    #[error("invalid administrator request: {0}")]
    Invalid(String),
    #[error("the privileged helper is missing: {}", .0.display())]
    HelperMissing(PathBuf),
    #[error("cannot ask for administrator authorization: {0}")]
    Launch(String),
    #[error("the privileged helper failed: {0}")]
    Helper(String),
    #[error("the privileged helper returned an unexpected answer: {0}")]
    BadOutput(String),
}

/// One privileged operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminOp {
    /// Moves `path` to `<staging>/<batch_id>/<n>-<file name>` and reports
    /// where it went.
    MoveToStaging { path: PathBuf },
    /// Moves a staged item back to `original`; refuses if `original` exists.
    RestoreFromStaging {
        staging_path: PathBuf,
        original: PathBuf,
    },
    /// `launchctl bootout` for a plist in `/Library/LaunchDaemons` (domain
    /// `system`) or `/Library/LaunchAgents` (domain `gui/<user_uid>`).
    Bootout { plist: PathBuf },
    /// `pkgutil --forget <pkg_id>`.
    ForgetReceipt { pkg_id: String },
}

/// Everything the helper is asked to do in one authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminManifest {
    pub version: u32,
    /// Names the staging sub-folder; `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
    pub batch_id: String,
    /// The user the work is done for; used as the `gui/<uid>` domain.
    pub user_uid: u32,
    /// `/Users/<name>`; its owner must be `user_uid`.
    pub user_home: PathBuf,
    pub ops: Vec<AdminOp>,
}

impl AdminManifest {
    /// A manifest for the current user with a fresh batch id.
    pub fn new(env: &Env, ops: Vec<AdminOp>) -> AdminManifest {
        AdminManifest {
            version: MANIFEST_VERSION,
            batch_id: new_id(),
            user_uid: env.uid,
            user_home: env.home.clone(),
            ops,
        }
    }
}

/// Outcome of one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResult {
    pub ok: bool,
    pub detail: String,
    /// Set by a successful [`AdminOp::MoveToStaging`].
    pub staging_path: Option<PathBuf>,
}

impl OpResult {
    fn done(detail: impl Into<String>) -> OpResult {
        OpResult {
            ok: true,
            detail: detail.into(),
            staging_path: None,
        }
    }

    fn failed(detail: impl Into<String>) -> OpResult {
        OpResult {
            ok: false,
            detail: detail.into(),
            staging_path: None,
        }
    }
}

/// The helper's answer: one result per operation, in manifest order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminResult {
    pub ops: Vec<OpResult>,
}

// ---------------------------------------------------------------------------
// Invocation (runs in the app)
// ---------------------------------------------------------------------------

/// The `osascript` command line that runs `helper` with `manifest` as root.
///
/// The helper path and the manifest JSON are separate argv items of the
/// AppleScript `run` handler and are quoted by AppleScript's
/// `quoted form of`; no path is ever concatenated into a command string.
pub fn admin_invocation(helper: &Path, manifest: &AdminManifest) -> Vec<String> {
    let json = serde_json::to_string(manifest).unwrap_or_default();
    [
        OSASCRIPT,
        "-e",
        "on run argv",
        "-e",
        "do shell script (quoted form of item 1 of argv) & \" \" & (quoted form of item 2 of argv) with administrator privileges",
        "-e",
        "end run",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([helper.to_string_lossy().into_owned(), json])
    .collect()
}

/// Runs `manifest` through the helper: one invocation, one password prompt.
/// Cancelling the prompt gives [`AdminError::Cancelled`].
pub fn run_admin(
    runner: &dyn CommandRunner,
    helper: &Path,
    manifest: &AdminManifest,
) -> Result<AdminResult, AdminError> {
    if !helper.is_absolute() || helper.to_str().is_none() || !helper.is_file() {
        return Err(AdminError::HelperMissing(helper.to_path_buf()));
    }
    check_limits(manifest)?;
    if serde_json::to_string(manifest).is_err() {
        return Err(AdminError::Invalid("paths must be valid UTF-8".into()));
    }
    let argv = admin_invocation(helper, manifest);
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let out = runner
        .run(&argv[0], &args)
        .map_err(|e| AdminError::Launch(e.to_string()))?;
    if !out.success() {
        // AppleScript reports the user pressing Cancel as error -128.
        return Err(if out.stderr.contains("(-128)") {
            AdminError::Cancelled
        } else {
            AdminError::Helper(shorten(out.stderr.trim()))
        });
    }
    let result: AdminResult = serde_json::from_str(out.stdout.trim())
        .map_err(|e| AdminError::BadOutput(e.to_string()))?;
    if result.ops.len() != manifest.ops.len() {
        return Err(AdminError::BadOutput(format!(
            "expected {} results, got {}",
            manifest.ops.len(),
            result.ops.len()
        )));
    }
    Ok(result)
}

fn shorten(text: &str) -> String {
    let mut s: String = text.chars().take(MAX_ERROR_TEXT).collect();
    if text.chars().count() > MAX_ERROR_TEXT {
        s.push('…');
    }
    s
}

/// An undo entry id with where it was restored to, or why it was not.
pub type RestoreOutcome = (String, Result<PathBuf, RestoreError>);

/// Restores admin-removed undo entries `ids` through the helper (one
/// authorization) and drops the entries that came back. Ids that are unknown
/// or not admin entries are reported without asking for authorization.
pub fn restore_admin_entries(
    runner: &dyn CommandRunner,
    helper: &Path,
    env: &Env,
    undo: &UndoLog,
    ids: &[String],
) -> Result<Vec<RestoreOutcome>, AdminError> {
    let entries = undo
        .entries()
        .map_err(|e| AdminError::Invalid(format!("cannot read the undo list: {e}")))?;
    let mut results: Vec<RestoreOutcome> = Vec::new();
    let mut ops = Vec::new();
    let mut slots = Vec::new();
    for id in ids {
        match entries.iter().find(|e| e.id == *id) {
            None => results.push((id.clone(), Err(RestoreError::UnknownEntry(id.clone())))),
            Some(e) if !e.admin => results.push((id.clone(), Err(RestoreError::InvalidEntry))),
            Some(e) => {
                slots.push((results.len(), e.original.clone()));
                results.push((id.clone(), Err(RestoreError::InvalidEntry)));
                ops.push(AdminOp::RestoreFromStaging {
                    staging_path: e.trash_location.clone(),
                    original: e.original.clone(),
                });
            }
        }
    }
    if ops.is_empty() {
        return Ok(results);
    }
    let answer = run_admin(runner, helper, &AdminManifest::new(env, ops))?;
    let mut restored = Vec::new();
    for ((slot, original), op) in slots.into_iter().zip(answer.ops) {
        if op.ok {
            restored.push(results[slot].0.clone());
            results[slot].1 = Ok(original);
        } else {
            results[slot].1 = Err(RestoreError::Io(op.detail));
        }
    }
    if let Err(e) = undo.forget(&restored) {
        for (id, result) in &mut results {
            if restored.contains(id) {
                *result = Err(RestoreError::Io(format!(
                    "restored, but the undo list could not be updated: {e}"
                )));
            }
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Validation (runs in the helper, and defensively in the app)
// ---------------------------------------------------------------------------

/// Parses manifest JSON, enforcing the size limit before parsing.
pub fn parse_manifest(json: &str) -> Result<AdminManifest, AdminError> {
    if json.len() > MAX_MANIFEST_BYTES {
        return Err(AdminError::Invalid("manifest is too large".into()));
    }
    serde_json::from_str(json).map_err(|e| AdminError::Invalid(format!("malformed manifest: {e}")))
}

fn check_limits(m: &AdminManifest) -> Result<(), AdminError> {
    if m.ops.is_empty() {
        return Err(AdminError::Invalid("no operations".into()));
    }
    if m.ops.len() > MAX_OPS {
        return Err(AdminError::Invalid(format!(
            "too many operations ({}, at most {MAX_OPS})",
            m.ops.len()
        )));
    }
    let size = serde_json::to_vec(m).map_or(usize::MAX, |v| v.len());
    if size > MAX_MANIFEST_BYTES {
        return Err(AdminError::Invalid("manifest is too large".into()));
    }
    Ok(())
}

/// Checks the whole manifest before anything is executed: all or nothing.
pub fn validate_manifest(m: &AdminManifest, policy: &SystemPolicy) -> Result<(), AdminError> {
    check_limits(m)?;
    let invalid = |why: String| AdminError::Invalid(why);
    if m.version != MANIFEST_VERSION {
        return Err(invalid(format!(
            "unsupported manifest version {}",
            m.version
        )));
    }
    if !is_valid_batch_id(&m.batch_id) {
        return Err(invalid("batch id has unexpected characters".into()));
    }
    validate_user(m, policy)?;
    for (i, op) in m.ops.iter().enumerate() {
        validate_op(op, policy).map_err(|why| invalid(format!("operation {}: {why}", i + 1)))?;
    }
    Ok(())
}

/// `user_home` must be `/Users/<name>` and owned by `user_uid` (never root).
fn validate_user(m: &AdminManifest, policy: &SystemPolicy) -> Result<(), AdminError> {
    let bad = |why: &str| AdminError::Invalid(why.to_string());
    if m.user_uid == 0 {
        return Err(bad("user id must not be root"));
    }
    let name = m
        .user_home
        .strip_prefix("/Users")
        .ok()
        .and_then(|rest| {
            let mut parts = rest.components();
            match (parts.next(), parts.next()) {
                (Some(std::path::Component::Normal(n)), None) => Some(n.to_os_string()),
                _ => None,
            }
        })
        .ok_or_else(|| bad("user home must be /Users/<name>"))?;
    let home = policy.system_root().join("Users").join(name);
    let owner = fs::symlink_metadata(&home)
        .ok()
        .filter(|meta| meta.is_dir())
        .and_then(|meta| sys::owner_uid(&meta));
    if owner != Some(m.user_uid) {
        return Err(bad("user home is not owned by the given user id"));
    }
    Ok(())
}

fn validate_op(op: &AdminOp, policy: &SystemPolicy) -> Result<(), String> {
    match op {
        AdminOp::MoveToStaging { path } => {
            policy.check(path).map(|_| ()).map_err(|e| e.to_string())
        }
        AdminOp::RestoreFromStaging {
            staging_path,
            original,
        } => {
            policy
                .check_staging(staging_path)
                .map_err(|_| "staging path is outside the staging folder".to_string())?;
            policy
                .check_destination(original)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        AdminOp::Bootout { plist } => launch_plist(policy, plist).map(|_| ()),
        AdminOp::ForgetReceipt { pkg_id } => {
            if is_valid_pkg_id(pkg_id) {
                Ok(())
            } else {
                Err("package id is not allowed".into())
            }
        }
    }
}

/// Canonical path of a launchd plist and whether it is a daemon (`system`
/// domain) rather than an agent (`gui/<uid>` domain).
fn launch_plist(policy: &SystemPolicy, plist: &Path) -> Result<(PathBuf, bool), String> {
    let canonical = policy.check(plist).map_err(|e| e.to_string())?;
    if canonical.extension().and_then(|e| e.to_str()) != Some("plist") {
        return Err("not a property list".into());
    }
    match canonical.parent() {
        Some(p) if p == policy.launch_daemons_dir() => Ok((canonical, true)),
        Some(p) if p == policy.launch_agents_dir() => Ok((canonical, false)),
        _ => Err("not in /Library/LaunchDaemons or /Library/LaunchAgents".into()),
    }
}

fn is_valid_batch_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_BATCH_ID
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$`, and never an Apple receipt.
pub fn is_valid_pkg_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_PKG_ID
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !id.to_ascii_lowercase().starts_with("com.apple.")
}

// ---------------------------------------------------------------------------
// Execution (runs in the helper as root)
// ---------------------------------------------------------------------------

/// Validates and executes `m`. Nothing is touched when validation fails: then
/// every operation reports the reason. Otherwise operations run in order and
/// each reports its own result; a failure does not stop the next one.
///
/// `system_root` is `/` for the real helper and a temporary directory in
/// tests; it must be the root `policy` was built for.
pub fn execute_manifest(
    m: &AdminManifest,
    policy: &SystemPolicy,
    system_root: &Path,
    runner: &dyn CommandRunner,
) -> AdminResult {
    execute_manifest_with_hook(m, policy, system_root, runner, &|_| {})
}

/// [`execute_manifest`] with a test seam: `after_check(index)` runs for each
/// move or restore after its paths were checked and before anything is
/// renamed, so tests can swap a directory in that window.
#[doc(hidden)]
pub fn execute_manifest_with_hook(
    m: &AdminManifest,
    policy: &SystemPolicy,
    system_root: &Path,
    runner: &dyn CommandRunner,
    after_check: &dyn Fn(usize),
) -> AdminResult {
    let refuse_all = |why: String| AdminResult {
        ops: m
            .ops
            .iter()
            .map(|_| OpResult::failed(why.clone()))
            .collect(),
    };
    if let Err(e) = validate_manifest(m, policy) {
        return refuse_all(e.to_string());
    }
    if fs::canonicalize(system_root).ok().as_deref() != Some(policy.system_root()) {
        return refuse_all("system root does not match the policy".into());
    }
    let exec = Executor {
        policy,
        runner,
        manifest: m,
        log: HelperLog::new(system_root),
        after_check,
    };
    AdminResult {
        ops: m
            .ops
            .iter()
            .enumerate()
            .map(|(i, op)| exec.run(i, op).unwrap_or_else(OpResult::failed))
            .collect(),
    }
}

struct Executor<'a> {
    policy: &'a SystemPolicy,
    runner: &'a dyn CommandRunner,
    manifest: &'a AdminManifest,
    log: HelperLog,
    after_check: &'a dyn Fn(usize),
}

impl Executor<'_> {
    fn run(&self, index: usize, op: &AdminOp) -> Result<OpResult, String> {
        match op {
            AdminOp::MoveToStaging { path } => self.move_to_staging(index, path),
            AdminOp::RestoreFromStaging {
                staging_path,
                original,
            } => self.restore(index, staging_path, original),
            AdminOp::Bootout { plist } => self.bootout(plist),
            AdminOp::ForgetReceipt { pkg_id } => self.forget(pkg_id),
        }
    }

    fn move_to_staging(&self, index: usize, path: &Path) -> Result<OpResult, String> {
        let source = self.policy.check(path).map_err(|e| e.to_string())?;
        let name = source
            .file_name()
            .ok_or_else(|| "path has no file name".to_string())?;
        let source_parent = source
            .parent()
            .ok_or_else(|| "path has no parent".to_string())?;
        // Identity of the item that was checked; the move must find the same one.
        let identity = fs::symlink_metadata(&source)
            .ok()
            .and_then(|meta| sys::identity(&meta))
            .ok_or_else(|| "cannot identify the item".to_string())?;
        let batch_dir = self.staging_batch_dir()?;
        let target_name = std::ffi::OsString::from(format!("{index}-{}", name.to_string_lossy()));
        let target = batch_dir.join(&target_name);
        if fs::symlink_metadata(&target).is_ok() {
            return Err("staging target already exists".into());
        }
        (self.after_check)(index);
        sys::checked_rename(
            self.policy.system_root(),
            (source_parent, name, identity),
            (&batch_dir, &target_name),
        )
        .map_err(|e| move_error(&e))?;
        self.log.record("stage", &source);
        Ok(OpResult {
            ok: true,
            detail: target.display().to_string(),
            staging_path: Some(target),
        })
    }

    /// `<staging>/<batch id>`, created root-only (0700). Every level must be
    /// a real directory owned by the effective user and not writable by others.
    fn staging_batch_dir(&self) -> Result<PathBuf, String> {
        let staging = self.policy.staging_dir();
        let batch = staging.join(&self.manifest.batch_id);
        if let Some(own_dir) = staging.parent() {
            ensure_dir(own_dir, 0o700)?;
        }
        ensure_dir(staging, 0o700)?;
        ensure_dir(&batch, 0o700)?;
        Ok(batch)
    }

    fn restore(
        &self,
        index: usize,
        staging_path: &Path,
        original: &Path,
    ) -> Result<OpResult, String> {
        let staged = self
            .policy
            .check_staging(staging_path)
            .map_err(|e| e.to_string())?;
        let batch_dir = staged
            .parent()
            .ok_or_else(|| "staging path has no parent".to_string())?;
        let staged_name = staged
            .file_name()
            .ok_or_else(|| "staging path has no file name".to_string())?;
        // Every level down to the batch folder must be private and ours.
        let staging = self.policy.staging_dir();
        if let Some(own_dir) = staging.parent() {
            verify_dir(own_dir)?;
        }
        verify_dir(staging)?;
        verify_dir(batch_dir)?;
        let identity = fs::symlink_metadata(&staged)
            .ok()
            .and_then(|meta| sys::identity(&meta))
            .ok_or_else(|| "item is no longer in staging".to_string())?;
        let target = self
            .policy
            .check_destination(original)
            .map_err(|e| e.to_string())?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(format!(
                "{} already exists; it was not overwritten",
                target.display()
            ));
        }
        if let Some(parent) = target.parent() {
            if fs::symlink_metadata(parent).is_err() {
                sys::create_dirs(parent, 0o755).map_err(|e| e.to_string())?;
                // The parent now exists, so resolve and check the target again.
                self.policy
                    .check_destination(original)
                    .map_err(|e| e.to_string())?;
            }
        }
        let target_parent = target
            .parent()
            .ok_or_else(|| "destination has no parent".to_string())?;
        let target_name = target
            .file_name()
            .ok_or_else(|| "destination has no file name".to_string())?;
        (self.after_check)(index);
        sys::checked_rename(
            self.policy.system_root(),
            (batch_dir, staged_name, identity),
            (target_parent, target_name),
        )
        .map_err(|e| move_error(&e))?;
        self.log.record("restore", &target);
        // Removes the batch folder once empty; a non-empty one stays.
        let _ = fs::remove_dir(batch_dir);
        Ok(OpResult::done(target.display().to_string()))
    }

    fn bootout(&self, plist: &Path) -> Result<OpResult, String> {
        let (canonical, is_daemon) = launch_plist(self.policy, plist)?;
        let domain = if is_daemon {
            "system".to_string()
        } else {
            format!("gui/{}", self.manifest.user_uid)
        };
        let plist_arg = canonical.to_string_lossy();
        let out = self
            .runner
            .run(LAUNCHCTL, &["bootout", &domain, &plist_arg])
            .map_err(|e| e.to_string())?;
        self.log.record("bootout", &canonical);
        if out.success() {
            Ok(OpResult::done(plist_arg))
        } else if LAUNCHCTL_NOT_LOADED.contains(&out.status) {
            Ok(OpResult::done(format!("{plist_arg}: was not loaded")))
        } else {
            Err(shorten(out.stderr.trim()))
        }
    }

    fn forget(&self, pkg_id: &str) -> Result<OpResult, String> {
        let out = self
            .runner
            .run(PKGUTIL, &["--forget", pkg_id])
            .map_err(|e| e.to_string())?;
        self.log.record("forget-receipt", Path::new(pkg_id));
        if out.success() {
            Ok(OpResult::done(pkg_id))
        } else {
            Err(shorten(out.stderr.trim()))
        }
    }
}

fn move_error(e: &io::Error) -> String {
    if sys::is_cross_device(e) {
        "the item is on a different volume than the staging folder; it was not moved".into()
    } else {
        e.to_string()
    }
}

/// Creates `dir` (and missing parents) with `mode`, then verifies it.
fn ensure_dir(dir: &Path, mode: u32) -> Result<(), String> {
    if fs::symlink_metadata(dir).is_err() {
        sys::create_dirs(dir, mode).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    verify_dir(dir)
}

/// A real directory owned by the effective user and not writable by others.
fn verify_dir(dir: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let trusted = meta.is_dir()
        && sys::owner_uid(&meta) == Some(sys::euid())
        && sys::mode(&meta) & 0o022 == 0;
    if trusted {
        Ok(())
    } else {
        Err(format!("{} is not a private directory", dir.display()))
    }
}

/// Best-effort log at `<root>/Library/Logs/Sweeper Kit/helper.log`: action and
/// path only.
struct HelperLog {
    path: PathBuf,
}

impl HelperLog {
    fn new(system_root: &Path) -> HelperLog {
        HelperLog {
            path: system_root.join("Library/Logs/Sweeper Kit/helper.log"),
        }
    }

    fn record(&self, action: &str, path: &Path) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let clean = |s: &str| -> String {
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        };
        let line = format!("{secs}\t{action}\t{}\n", clean(&path.to_string_lossy()));
        let Some(dir) = self.path.parent() else {
            return;
        };
        if ensure_dir(dir, 0o755).is_err() {
            return;
        }
        if let Ok(mut file) = sys::open_log(&self.path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

// ---------------------------------------------------------------------------
// Detecting items that need administrator access
// ---------------------------------------------------------------------------

/// True if removing the app bundle at `bundle` needs administrator access:
/// its folder is not writable by the current user or root owns the bundle.
pub(crate) fn bundle_needs_admin(bundle: &Path) -> bool {
    let root_owned = fs::symlink_metadata(bundle)
        .ok()
        .and_then(|meta| sys::owner_uid(&meta))
        == Some(0);
    root_owned || bundle.parent().is_some_and(|p| !sys::is_writable(p))
}

/// Platform specifics, kept in one place. Off Unix the helper can never run
/// as root and every ownership check fails closed.
#[cfg(unix)]
mod sys {
    use std::ffi::{CString, OsStr};
    use std::fs::{DirBuilder, File, Metadata, OpenOptions};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::Path;

    pub fn is_writable(path: &Path) -> bool {
        let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
        unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
    }

    pub fn owner_uid(meta: &Metadata) -> Option<u32> {
        Some(meta.uid())
    }

    pub fn mode(meta: &Metadata) -> u32 {
        meta.mode()
    }

    pub fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    pub fn is_cross_device(e: &io::Error) -> bool {
        e.raw_os_error() == Some(libc::EXDEV)
    }

    /// (device, inode) of a file.
    pub fn identity(meta: &Metadata) -> Option<(u64, u64)> {
        Some((meta.dev(), meta.ino()))
    }

    /// Moves `src_name` in `src_dir` to `dst_name` in `dst_dir` without
    /// trusting any path: both directories are opened component by component
    /// from `root` with `O_NOFOLLOW`, the source must still be the item
    /// recorded during validation (`expected` device and inode), and the
    /// rename is done between the two directory descriptors. A swapped
    /// directory or symlink fails the move and nothing is moved. An existing
    /// destination is never replaced: on macOS the rename itself refuses to
    /// replace it (`RENAME_EXCL`); elsewhere only the check before the rename
    /// guards this, so a file created in between could be replaced (the helper
    /// only runs on macOS).
    pub fn checked_rename(
        root: &Path,
        (src_dir, src_name, expected): (&Path, &OsStr, (u64, u64)),
        (dst_dir, dst_name): (&Path, &OsStr),
    ) -> io::Result<()> {
        let replaced = |e: io::Error| match e.raw_os_error() {
            Some(libc::ELOOP | libc::ENOTDIR) => io::Error::other(
                "a parent folder was replaced by a link or file; nothing was moved",
            ),
            _ => e,
        };
        let src = fd::open_dir_below(root, src_dir).map_err(replaced)?;
        let dst = fd::open_dir_below(root, dst_dir).map_err(replaced)?;
        let found = fd::stat_at(&src, src_name)?;
        if found != expected {
            return Err(io::Error::other(
                "the item changed after it was checked; nothing was moved",
            ));
        }
        // Friendlier early error only; the exclusive rename below is what
        // guarantees nothing is overwritten.
        if fd::stat_at(&dst, dst_name).is_ok() {
            return Err(destination_exists());
        }
        fd::rename_at_no_replace(&src, src_name, &dst, dst_name).map_err(|e| {
            if e.raw_os_error() == Some(libc::EEXIST) {
                destination_exists()
            } else {
                e
            }
        })
    }

    fn destination_exists() -> io::Error {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination already exists; nothing was moved",
        )
    }

    /// Thin safe wrappers over the descriptor-relative system calls.
    mod fd {
        use std::ffi::{CString, OsStr};
        use std::io;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        use std::path::{Component, Path};

        /// An open directory; the descriptor is closed when it is dropped.
        pub struct Dir(OwnedFd);

        fn c_name(name: &OsStr) -> io::Result<CString> {
            CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
        }

        fn wrap(fd: libc::c_int) -> io::Result<Dir> {
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is a freshly opened descriptor that nothing else owns.
            Ok(Dir(unsafe { OwnedFd::from_raw_fd(fd) }))
        }

        /// Opens `root`, which the caller has already resolved (it may be
        /// reached through links, e.g. `/`).
        fn open_root(root: &Path) -> io::Result<Dir> {
            let c = c_name(root.as_os_str())?;
            // SAFETY: `c` is a valid NUL-terminated string that outlives the call.
            wrap(unsafe {
                libc::open(
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            })
        }

        /// Opens the directory `name` inside `parent`, refusing symlinks
        /// (`ELOOP`) and non-directories (`ENOTDIR`).
        fn open_child(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
            let c = c_name(name)?;
            // SAFETY: `parent` is an open descriptor and `c` a valid C string.
            wrap(unsafe {
                libc::openat(
                    parent.0.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            })
        }

        /// Opens `dir`, which must lie below `root`, one component at a time
        /// without following any symlink below `root`.
        pub fn open_dir_below(root: &Path, dir: &Path) -> io::Result<Dir> {
            let rel = dir
                .strip_prefix(root)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let mut cur = open_root(root)?;
            for part in rel.components() {
                match part {
                    Component::Normal(name) => cur = open_child(&cur, name)?,
                    _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
                }
            }
            Ok(cur)
        }

        /// (device, inode) of `name` inside `dir`, not following a symlink.
        #[allow(clippy::unnecessary_cast)] // field widths differ per platform
        pub fn stat_at(dir: &Dir, name: &OsStr) -> io::Result<(u64, u64)> {
            let c = c_name(name)?;
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `dir` is open, `c` is a valid C string and `st` is writable
            // memory of the right type.
            let rc = unsafe {
                libc::fstatat(
                    dir.0.as_raw_fd(),
                    c.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fstatat succeeded, so `st` is initialized.
            let st = unsafe { st.assume_init() };
            Ok((st.st_dev as u64, st.st_ino as u64))
        }

        /// Atomic rename between two open directories that fails with `EEXIST`
        /// instead of replacing an existing destination.
        #[cfg(target_os = "macos")]
        pub fn rename_at_no_replace(
            from_dir: &Dir,
            from: &OsStr,
            to_dir: &Dir,
            to: &OsStr,
        ) -> io::Result<()> {
            let (from, to) = (c_name(from)?, c_name(to)?);
            // SAFETY: both descriptors are open and both names are valid C strings.
            let rc = unsafe {
                libc::renameatx_np(
                    from_dir.0.as_raw_fd(),
                    from.as_ptr(),
                    to_dir.0.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_EXCL,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        /// Plain `renameat(2)`: no exclusive rename is used on this platform,
        /// so the caller's earlier existence check is the only guard and a
        /// racing file could be replaced.
        #[cfg(not(target_os = "macos"))]
        pub fn rename_at_no_replace(
            from_dir: &Dir,
            from: &OsStr,
            to_dir: &Dir,
            to: &OsStr,
        ) -> io::Result<()> {
            let (from, to) = (c_name(from)?, c_name(to)?);
            // SAFETY: both descriptors are open and both names are valid C strings.
            let rc = unsafe {
                libc::renameat(
                    from_dir.0.as_raw_fd(),
                    from.as_ptr(),
                    to_dir.0.as_raw_fd(),
                    to.as_ptr(),
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    pub fn create_dirs(path: &Path, mode: u32) -> io::Result<()> {
        DirBuilder::new().recursive(true).mode(mode).create(path)
    }

    /// Opens the log for appending without following a symlink.
    pub fn open_log(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
}

#[cfg(not(unix))]
mod sys {
    use std::ffi::OsStr;
    use std::fs::{File, Metadata, OpenOptions};
    use std::io;
    use std::path::Path;

    pub fn is_writable(path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| !m.permissions().readonly())
    }

    pub fn owner_uid(_meta: &Metadata) -> Option<u32> {
        None
    }

    pub fn mode(_meta: &Metadata) -> u32 {
        0o700
    }

    pub fn euid() -> u32 {
        u32::MAX
    }

    pub fn is_cross_device(_e: &io::Error) -> bool {
        false
    }

    pub fn identity(_meta: &Metadata) -> Option<(u64, u64)> {
        None
    }

    /// Unsupported off Unix: the helper never runs there.
    pub fn checked_rename(
        _root: &Path,
        _src: (&Path, &OsStr, (u64, u64)),
        _dst: (&Path, &OsStr),
    ) -> io::Result<()> {
        Err(io::Error::other("moving items is not supported here"))
    }

    pub fn create_dirs(path: &Path, _mode: u32) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    pub fn open_log(path: &Path) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// True when the process runs with effective user id 0.
pub fn is_root() -> bool {
    sys::euid() == 0
}
