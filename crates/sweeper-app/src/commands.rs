//! Command layer: plain functions over an [`AppContext`] plus the thin
//! `#[tauri::command]` wrappers. Every filesystem decision is made by
//! `sweeper-core`; nothing here touches files except the settings store.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

use serde::Serialize;
use sweeper_core::{
    apply_preset as core_apply_preset, brew_zap_command as core_brew_zap_command, compute_sizes,
    find_receipts, full_disk_access, restore_admin_entries, team_id, AppCatalog, AppInfo,
    CacheScanner, CleanPlan, CleanReport, CleanScan, Cleaner, FdaStatus, LeftoverScanner,
    OrphanFinder, OrphanScan, Preset, Progress, QuitOutcome, Receipt, RemovalMode, RemovalPlan,
    RemovalReport, RemovedApp, RemovedApps, Remover, RestoreError, RunningApp, ScanResult,
    StepResult, UndoBatch, UndoEntry, UndoLog, UninstallReport, Uninstaller,
};

use crate::state::{lock, save_settings as store_settings, AppContext, AppState, Settings};

/// Error message sent when an operation was superseded; the UI ignores it.
pub const CANCELLED: &str = "cancelled";
/// How long an app gets to quit before the UI offers to force it.
pub const QUIT_GRACE: Duration = Duration::from_secs(5);
/// Error when the cleaner has no scan to work from.
const NO_SCAN: &str = "Scan first.";
/// Error when the selection contains nothing that can be removed.
const NOTHING_SELECTED: &str = "Nothing selected that can be removed. Scan again.";
/// Why administrator operations are disabled when the helper is missing.
const ADMIN_UNAVAILABLE: &str =
    "The privileged helper is missing (development build), so administrator operations are disabled";
/// Apps sized per batch, so sizes reach the UI progressively.
const SIZE_BATCH: usize = 8;

/// Payload of the `app-sizes` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SizeUpdate {
    pub path: PathBuf,
    pub size: u64,
}

/// Result of restoring one history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreOutcome {
    pub id: String,
    pub ok: bool,
    /// The restored path on success, the reason on failure.
    pub path_or_error: String,
}

// ---- plain functions -------------------------------------------------------

/// Installed apps over the default and extra app folders. Sizes are filled in
/// separately by [`app_sizes`].
pub fn list_apps(ctx: &AppContext) -> Vec<AppInfo> {
    let apps = AppCatalog::scan(&ctx.env, &*ctx.spotlight);
    *lock(&ctx.installed) = apps.clone();
    apps
}

/// Computes bundle sizes in batches and reports each as it is known.
pub fn app_sizes(apps: &[AppInfo], cancel: &AtomicBool, emit: &mut dyn FnMut(SizeUpdate)) {
    for batch in apps.chunks(SIZE_BATCH) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let mut batch = batch.to_vec();
        compute_sizes(&mut batch, cancel);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        for app in batch {
            if let Some(size) = app.size {
                emit(SizeUpdate {
                    path: app.path,
                    size,
                });
            }
        }
    }
}

/// Finds the app at `path`: in the cached list, otherwise as a one-off built
/// from its Info.plist by cataloguing its parent folder.
fn resolve_app(ctx: &AppContext, path: &Path) -> Result<AppInfo, String> {
    let is_app = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    if !path.is_absolute() || !is_app || !path.is_dir() {
        return Err(format!("{} is not an application bundle", path.display()));
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let same = |a: &AppInfo| {
        a.path == path || std::fs::canonicalize(&a.path).is_ok_and(|p| p == canonical)
    };
    if let Some(app) = ctx.installed_apps().into_iter().find(same) {
        return Ok(app);
    }
    let parent = path.parent().ok_or("The app has no parent folder")?;
    let mut env = ctx.env.clone();
    env.app_dirs = vec![parent.to_path_buf()];
    AppCatalog::scan(&env, &*ctx.spotlight)
        .into_iter()
        .find(same)
        .ok_or_else(|| format!("{} has no readable Info.plist", path.display()))
}

/// Scans leftovers of the app at `path` with the Everything preset applied.
pub fn scan_app(ctx: &AppContext, path: &Path, cancel: &AtomicBool) -> Result<ScanResult, String> {
    let mut app = resolve_app(ctx, path)?;
    if app.team_id.is_none() {
        app.team_id = team_id(&app.path, &*ctx.runner);
    }
    let installed = ctx.installed_apps();
    let scanner = LeftoverScanner::new(&ctx.env, &ctx.rules, &ctx.policy, &*ctx.spotlight)
        .with_system_policy(ctx.system_policy.clone());
    let mut result = scanner.scan(&app, &installed, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    core_apply_preset(&mut result, Preset::Everything);
    Ok(result)
}

pub fn apply_preset(mut scan: ScanResult, preset: Preset) -> ScanResult {
    core_apply_preset(&mut scan, preset);
    scan
}

/// Dry run: exactly what an uninstall would remove.
pub fn plan_removal(scan: &ScanResult) -> RemovalPlan {
    RemovalPlan::from_scan(scan)
}

fn with_uninstaller<R>(
    ctx: &AppContext,
    forget_receipts: Vec<String>,
    body: impl FnOnce(&Uninstaller<'_>) -> R,
) -> R {
    let installed = ctx.installed_apps();
    let scanner = LeftoverScanner::new(&ctx.env, &ctx.rules, &ctx.policy, &*ctx.spotlight)
        .with_system_policy(ctx.system_policy.clone());
    let remover = Remover::new(
        &ctx.env,
        &ctx.policy,
        &*ctx.trasher,
        UndoLog::for_env(&ctx.env),
    );
    let uninstaller = Uninstaller::new(
        &ctx.env,
        &ctx.policy,
        &*ctx.runner,
        &*ctx.processes,
        &remover,
        &scanner,
        &installed,
    )
    .with_system_policy(ctx.system_policy.clone())
    .with_forget_receipts(forget_receipts);
    let uninstaller = match &ctx.helper {
        Some(helper) => uninstaller.with_admin_helper(helper),
        None => uninstaller,
    };
    body(&uninstaller)
}

/// Whether the privileged helper is present, i.e. administrator operations
/// can be offered.
pub fn admin_available(ctx: &AppContext) -> bool {
    ctx.helper.is_some()
}

/// Installer receipts of the app at `path`. The app is looked up in the
/// server-side catalog copy; nothing from the UI is trusted.
pub fn receipts(ctx: &AppContext, path: &Path) -> Result<Vec<Receipt>, String> {
    let mut app = resolve_app(ctx, path)?;
    if app.team_id.is_none() {
        app.team_id = team_id(&app.path, &*ctx.runner);
    }
    Ok(find_receipts(&*ctx.runner, &app))
}

pub fn running_instances(ctx: &AppContext, scan: &ScanResult) -> Vec<RunningApp> {
    with_uninstaller(ctx, Vec::new(), |u| u.running_instances(scan))
}

/// Quits the running instances of the scanned app, waiting [`QUIT_GRACE`].
/// The instances are looked up again here so the UI cannot name other processes.
pub fn quit_apps(ctx: &AppContext, scan: &ScanResult, allow_force: bool) -> QuitOutcome {
    quit_apps_within(ctx, scan, allow_force, QUIT_GRACE)
}

pub fn quit_apps_within(
    ctx: &AppContext,
    scan: &ScanResult,
    allow_force: bool,
    grace: Duration,
) -> QuitOutcome {
    with_uninstaller(ctx, Vec::new(), |u| {
        let apps = u.running_instances(scan);
        for app in &apps {
            ctx.log
                .record(if allow_force { "force-quit" } else { "quit" }, &app.path);
        }
        u.quit(&apps, grace, allow_force)
    })
}

/// Removes the checked items (Trash unless the setting says otherwise).
/// `forget_receipts` must be package ids that [`receipts`] finds for the app;
/// anything else rejects the whole request before anything is removed.
pub fn uninstall(
    ctx: &AppContext,
    scan: &ScanResult,
    forget_receipts: &[String],
    progress: &mut dyn FnMut(Progress),
) -> Result<UninstallReport, String> {
    if scan.app.is_apple {
        return Err("Apple apps are read-only and cannot be uninstalled".into());
    }
    let mut forget: Vec<String> = Vec::new();
    if !forget_receipts.is_empty() {
        let known = receipts(ctx, &scan.app.path)?;
        for id in forget_receipts {
            if !known.iter().any(|r| r.pkg_id == *id) {
                return Err(format!("{id} is not an installer receipt of this app"));
            }
            if !forget.contains(id) {
                forget.push(id.clone());
            }
        }
    }
    let mode = if ctx.settings.delete_immediately {
        RemovalMode::Delete
    } else {
        RemovalMode::Trash
    };
    ctx.log.record("uninstall", &scan.app.path);
    Ok(with_uninstaller(ctx, forget, |u| {
        u.run(scan, mode, progress)
    }))
}

/// The cask of `app` as looked up in the installed list, never trusting the UI.
fn installed_copy(ctx: &AppContext, app: &AppInfo) -> AppInfo {
    ctx.installed_apps()
        .into_iter()
        .find(|a| a.path == app.path && a.bundle_id == app.bundle_id)
        .unwrap_or_else(|| app.clone())
}

pub fn brew_zap_command(ctx: &AppContext, app: &AppInfo) -> Option<Vec<String>> {
    core_brew_zap_command(&installed_copy(ctx, app))
}

pub fn brew_zap(ctx: &AppContext, app: &AppInfo) -> StepResult {
    let app = installed_copy(ctx, app);
    ctx.log.record("brew-zap", &app.path);
    with_uninstaller(ctx, Vec::new(), |u| u.run_brew_zap(&app))
}

pub fn fda_status(ctx: &AppContext) -> FdaStatus {
    full_disk_access(&ctx.env)
}

/// Validates `path` for "Show in Finder": it must pass the path policy or be
/// one of the app folders. Returns the path to reveal.
pub fn reveal_target(ctx: &AppContext, path: &Path) -> Result<PathBuf, String> {
    if ctx.env.app_dirs.iter().any(|d| d == path) {
        return Ok(path.to_path_buf());
    }
    ctx.policy
        .check(path)
        .or_else(|_| ctx.cleaner_policy.check(path))
        .map_err(|e| e.to_string())
}

pub fn get_settings(ctx: &AppContext) -> Settings {
    ctx.settings.clone()
}

pub fn save_settings(ctx: &mut AppContext, settings: &Settings) -> Result<Settings, String> {
    store_settings(ctx, settings)
}

/// Items moved to the Trash by earlier uninstalls.
pub fn undo_entries(ctx: &AppContext) -> Result<Vec<UndoEntry>, String> {
    UndoLog::for_env(&ctx.env)
        .entries()
        .map_err(|e| format!("Could not read the undo list: {e}"))
}

// ---- cleaner ---------------------------------------------------------------

/// Scans for cleanable entries. Returns [`CANCELLED`] when superseded.
pub fn scan_cleaner(ctx: &AppContext, cancel: &AtomicBool) -> Result<CleanScan, String> {
    let scanner = CacheScanner::new(
        &ctx.env,
        &ctx.clean_rules,
        &ctx.cleaner_policy,
        &*ctx.processes,
        &*ctx.tools,
    )
    .with_runner(&*ctx.runner);
    let scan = scanner.scan(&ctx.installed_apps(), &ctx.settings.clean_options(), cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    Ok(scan)
}

/// `scan` with exactly the entries in `selected` checked. Unknown ids are an
/// error; refused entries stay unchecked. Only ids come from the UI.
fn select_entries(scan: &CleanScan, selected: &[String]) -> Result<CleanScan, String> {
    let known: HashSet<&str> = scan.entries().map(|e| e.id.as_str()).collect();
    if let Some(unknown) = selected.iter().find(|id| !known.contains(id.as_str())) {
        return Err(format!("Unknown item: {unknown}. Scan again."));
    }
    let wanted: HashSet<&str> = selected.iter().map(String::as_str).collect();
    let mut scan = scan.clone();
    for entry in scan
        .categories
        .iter_mut()
        .flat_map(|c| c.entries.iter_mut())
    {
        entry.checked = entry.refused.is_none() && wanted.contains(entry.id.as_str());
    }
    Ok(scan)
}

/// Dry run: exactly what cleaning the `selected` entry ids would do.
pub fn plan_clean(scan: &CleanScan, selected: &[String]) -> Result<CleanPlan, String> {
    Ok(CleanPlan::from_scan(&select_entries(scan, selected)?))
}

/// Cleans the `selected` entry ids of `scan`. Everything goes to the Trash
/// except the Trash category, which core deletes permanently.
pub fn clean(
    ctx: &AppContext,
    scan: &CleanScan,
    selected: &[String],
    progress: &mut dyn FnMut(Progress),
) -> Result<CleanReport, String> {
    let plan = plan_clean(scan, selected)?;
    ctx.log.record("clean", &ctx.env.home);
    let cleaner = Cleaner::new(
        &ctx.env,
        &ctx.cleaner_policy,
        &*ctx.trasher,
        UndoLog::for_env(&ctx.env),
        &*ctx.runner,
    )
    .with_tools(&*ctx.tools);
    Ok(cleaner.execute(&plan, progress))
}

// ---- leftovers -------------------------------------------------------------

/// Finds data of apps that are no longer installed. Returns [`CANCELLED`]
/// when superseded.
pub fn scan_leftovers(ctx: &AppContext, cancel: &AtomicBool) -> Result<OrphanScan, String> {
    let history = RemovedApps::for_env(&ctx.env);
    let finder = OrphanFinder::new(
        &ctx.env,
        &ctx.orphan_rules,
        &ctx.policy,
        &*ctx.spotlight,
        &*ctx.processes,
        &history,
    );
    let scan = finder.scan(&ctx.installed_apps(), cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    Ok(scan)
}

/// Dry run: exactly what removing the `selected` paths would do. Paths that
/// are not in `scan` or were refused are ignored.
pub fn plan_leftovers(scan: &OrphanScan, selected: &[PathBuf]) -> Result<RemovalPlan, String> {
    let plan = scan.plan(Some(selected));
    if plan.items.is_empty() {
        return Err(NOTHING_SELECTED.into());
    }
    Ok(plan)
}

/// Removes the `selected` paths of `scan` (Trash unless the setting says otherwise).
pub fn remove_leftovers(
    ctx: &AppContext,
    scan: &OrphanScan,
    selected: &[PathBuf],
    progress: &mut dyn FnMut(Progress),
) -> Result<RemovalReport, String> {
    let plan = plan_leftovers(scan, selected)?;
    let mode = if ctx.settings.delete_immediately {
        RemovalMode::Delete
    } else {
        RemovalMode::Trash
    };
    ctx.log.record("remove-leftovers", &ctx.env.home);
    let remover = Remover::new(
        &ctx.env,
        &ctx.policy,
        &*ctx.trasher,
        UndoLog::for_env(&ctx.env),
    );
    Ok(remover.execute(&plan, mode, progress))
}

// ---- history ---------------------------------------------------------------

/// Trash moves grouped by run, newest first.
pub fn history(ctx: &AppContext) -> Result<Vec<UndoBatch>, String> {
    UndoLog::for_env(&ctx.env)
        .batches()
        .map_err(|e| format!("Could not read the history: {e}"))
}

/// Moves the entries `ids` back to where they came from. Entries removed with
/// administrator access are restored together through the helper (one
/// password prompt); the others from the Trash.
pub fn restore(ctx: &AppContext, ids: &[String]) -> Vec<RestoreOutcome> {
    let log = UndoLog::for_env(&ctx.env);
    ctx.log.record("restore", log.path());
    let admin_ids: HashSet<String> = log
        .entries()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.admin && ids.contains(&e.id))
        .map(|e| e.id)
        .collect();
    let (admin_ids, normal_ids): (Vec<String>, Vec<String>) =
        ids.iter().cloned().partition(|id| admin_ids.contains(id));

    let mut results = log.restore_many(&normal_ids, &ctx.policy);
    if !admin_ids.is_empty() {
        results.extend(restore_admin(ctx, &log, &admin_ids));
    }
    // Answer in the order requested.
    ids.iter()
        .filter_map(|id| {
            let at = results.iter().position(|(r, _)| r == id)?;
            Some(results.swap_remove(at))
        })
        .map(|(id, result)| match result {
            Ok(path) => RestoreOutcome {
                id,
                ok: true,
                path_or_error: path.display().to_string(),
            },
            Err(e) => RestoreOutcome {
                id,
                ok: false,
                path_or_error: e.to_string(),
            },
        })
        .collect()
}

/// Restores admin entries with one authorization; every entry fails with the
/// same reason when the helper is missing or the request is refused.
fn restore_admin(
    ctx: &AppContext,
    log: &UndoLog,
    ids: &[String],
) -> Vec<(String, Result<PathBuf, RestoreError>)> {
    let failure = |why: String| {
        ids.iter()
            .map(|id| (id.clone(), Err(RestoreError::Io(why.clone()))))
            .collect()
    };
    let Some(helper) = &ctx.helper else {
        return failure(ADMIN_UNAVAILABLE.into());
    };
    ctx.log.record("restore-admin", log.path());
    restore_admin_entries(&*ctx.runner, helper, &ctx.env, log, ids)
        .unwrap_or_else(|e| failure(e.to_string()))
}

/// Drops entries whose Trash item is gone. Entries still in the Trash are
/// refused: forgetting them would remove the only way to restore them from
/// the app. Returns how many entries were dropped.
pub fn forget_history(ctx: &AppContext, ids: &[String]) -> Result<usize, String> {
    let batches = history(ctx)?;
    let statuses: Vec<_> = batches.iter().flat_map(|b| b.entries.iter()).collect();
    for id in ids {
        match statuses.iter().find(|s| s.entry.id == *id) {
            None => return Err(format!("Unknown history entry: {id}")),
            Some(s) if s.in_trash => {
                return Err(format!(
                    "{} is still in the Trash and cannot be removed from the history",
                    s.entry.original.display()
                ))
            }
            Some(_) => {}
        }
    }
    let log = UndoLog::for_env(&ctx.env);
    ctx.log.record("forget-history", log.path());
    log.forget(ids)
        .map_err(|e| format!("Could not update the history: {e}"))
}

/// Apps Sweeper Kit uninstalled, oldest first.
pub fn removed_apps(ctx: &AppContext) -> Result<Vec<RemovedApp>, String> {
    RemovedApps::for_env(&ctx.env)
        .entries()
        .map_err(|e| format!("Could not read the removed apps history: {e}"))
}

pub fn forget_removed_app(ctx: &AppContext, bundle_id: &str) -> Result<(), String> {
    RemovedApps::for_env(&ctx.env)
        .forget(bundle_id)
        .map_err(|e| format!("Could not update the removed apps history: {e}"))
}

// ---- Tauri wrappers --------------------------------------------------------

type Ctx = Arc<RwLock<AppContext>>;

fn read(ctx: &Ctx) -> RwLockReadGuard<'_, AppContext> {
    ctx.read().unwrap_or_else(|e| e.into_inner())
}

/// Runs `work` on the blocking pool so the UI never waits on the filesystem.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("Background task failed: {e}"))
}

pub mod wrappers {
    use tauri::{AppHandle, Emitter, State};
    use tauri_plugin_opener::OpenerExt;

    use super::*;

    #[tauri::command]
    pub async fn list_apps(
        app: AppHandle,
        state: State<'_, AppState>,
    ) -> Result<Vec<AppInfo>, String> {
        let ctx = state.ctx.clone();
        let apps = blocking(move || super::list_apps(&read(&ctx))).await?;
        let cancel = state.sizes_cancel.renew();
        let for_sizes = apps.clone();
        tauri::async_runtime::spawn_blocking(move || {
            app_sizes(&for_sizes, &cancel, &mut |update| {
                let _ = app.emit("app-sizes", update);
            });
        });
        Ok(apps)
    }

    #[tauri::command]
    pub async fn scan_app(state: State<'_, AppState>, path: PathBuf) -> Result<ScanResult, String> {
        let ctx = state.ctx.clone();
        let cancel = state.scan_cancel.renew();
        blocking(move || super::scan_app(&read(&ctx), &path, &cancel)).await?
    }

    #[tauri::command]
    pub fn apply_preset(scan: ScanResult, preset: Preset) -> ScanResult {
        super::apply_preset(scan, preset)
    }

    #[tauri::command]
    pub fn plan_removal(scan: ScanResult) -> RemovalPlan {
        super::plan_removal(&scan)
    }

    #[tauri::command]
    pub async fn running_instances(
        state: State<'_, AppState>,
        scan: ScanResult,
    ) -> Result<Vec<RunningApp>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::running_instances(&read(&ctx), &scan)).await
    }

    #[tauri::command]
    pub async fn quit_apps(
        state: State<'_, AppState>,
        scan: ScanResult,
        allow_force: bool,
    ) -> Result<QuitOutcome, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::quit_apps(&read(&ctx), &scan, allow_force)).await
    }

    #[tauri::command]
    pub async fn uninstall(
        app: AppHandle,
        state: State<'_, AppState>,
        scan: ScanResult,
        forget_receipts: Vec<String>,
    ) -> Result<UninstallReport, String> {
        let ctx = state.ctx.clone();
        blocking(move || {
            super::uninstall(&read(&ctx), &scan, &forget_receipts, &mut |p| {
                let _ = app.emit("uninstall-progress", p);
            })
        })
        .await?
    }

    #[tauri::command]
    pub async fn receipts(
        state: State<'_, AppState>,
        path: PathBuf,
    ) -> Result<Vec<Receipt>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::receipts(&read(&ctx), &path)).await?
    }

    #[tauri::command]
    pub fn admin_available(state: State<'_, AppState>) -> bool {
        super::admin_available(&read(&state.ctx))
    }

    #[tauri::command]
    pub async fn brew_zap_command(
        state: State<'_, AppState>,
        app: AppInfo,
    ) -> Result<Option<Vec<String>>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::brew_zap_command(&read(&ctx), &app)).await
    }

    #[tauri::command]
    pub async fn brew_zap(state: State<'_, AppState>, app: AppInfo) -> Result<StepResult, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::brew_zap(&read(&ctx), &app)).await
    }

    #[tauri::command]
    pub async fn fda_status(state: State<'_, AppState>) -> Result<FdaStatus, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::fda_status(&read(&ctx))).await
    }

    #[tauri::command]
    pub fn open_fda_settings(app: AppHandle) -> Result<(), String> {
        app.opener()
            .open_url(sweeper_core::FDA_SETTINGS_URL, None::<&str>)
            .map_err(|e| format!("Could not open System Settings: {e}"))
    }

    #[tauri::command]
    pub fn reveal_in_finder(
        app: AppHandle,
        state: State<'_, AppState>,
        path: PathBuf,
    ) -> Result<(), String> {
        let target = reveal_target(&read(&state.ctx), &path)?;
        app.opener()
            .reveal_item_in_dir(target)
            .map_err(|e| format!("Could not reveal the item: {e}"))
    }

    #[tauri::command]
    pub fn get_settings(state: State<'_, AppState>) -> Settings {
        super::get_settings(&read(&state.ctx))
    }

    #[tauri::command]
    pub async fn save_settings(
        state: State<'_, AppState>,
        settings: Settings,
    ) -> Result<Settings, String> {
        let ctx = state.ctx.clone();
        blocking(move || {
            let mut ctx = ctx.write().unwrap_or_else(|e| e.into_inner());
            super::save_settings(&mut ctx, &settings)
        })
        .await?
    }

    #[tauri::command]
    pub async fn undo_entries(state: State<'_, AppState>) -> Result<Vec<UndoEntry>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::undo_entries(&read(&ctx))).await?
    }

    #[tauri::command]
    pub fn rules_warning(state: State<'_, AppState>) -> Option<String> {
        read(&state.ctx).rules_warning.clone()
    }

    #[tauri::command]
    pub fn clean_rules_warning(state: State<'_, AppState>) -> Option<String> {
        read(&state.ctx).clean_rules_warning.clone()
    }

    #[tauri::command]
    pub async fn scan_cleaner(state: State<'_, AppState>) -> Result<CleanScan, String> {
        let ctx = state.ctx.clone();
        let cancel = state.clean_cancel.renew();
        let scan = blocking(move || super::scan_cleaner(&read(&ctx), &cancel)).await??;
        *lock(&state.clean_scan) = Some(scan.clone());
        Ok(scan)
    }

    #[tauri::command]
    pub fn plan_clean(
        state: State<'_, AppState>,
        selected: Vec<String>,
    ) -> Result<CleanPlan, String> {
        let stored = lock(&state.clean_scan);
        let scan = stored.as_ref().ok_or(NO_SCAN)?;
        super::plan_clean(scan, &selected)
    }

    #[tauri::command]
    pub async fn clean(
        app: AppHandle,
        state: State<'_, AppState>,
        selected: Vec<String>,
    ) -> Result<CleanReport, String> {
        // Taking the scan makes a second request fail instead of cleaning twice.
        let scan = lock(&state.clean_scan).take().ok_or(NO_SCAN)?;
        let ctx = state.ctx.clone();
        let for_work = scan.clone();
        let result = blocking(move || {
            super::clean(&read(&ctx), &for_work, &selected, &mut |p| {
                let _ = app.emit("clean-progress", p);
            })
        })
        .await?;
        if result.is_err() {
            // Nothing ran (invalid selection): keep the scan for a retry.
            lock(&state.clean_scan).get_or_insert(scan);
        }
        result
    }

    #[tauri::command]
    pub fn orphan_rules_warning(state: State<'_, AppState>) -> Option<String> {
        read(&state.ctx).orphan_rules_warning.clone()
    }

    #[tauri::command]
    pub async fn scan_leftovers(state: State<'_, AppState>) -> Result<OrphanScan, String> {
        let ctx = state.ctx.clone();
        let cancel = state.leftovers_cancel.renew();
        let scan = blocking(move || super::scan_leftovers(&read(&ctx), &cancel)).await??;
        *lock(&state.leftover_scan) = Some(scan.clone());
        Ok(scan)
    }

    #[tauri::command]
    pub fn plan_leftovers(
        state: State<'_, AppState>,
        selected: Vec<PathBuf>,
    ) -> Result<RemovalPlan, String> {
        let stored = lock(&state.leftover_scan);
        let scan = stored.as_ref().ok_or(NO_SCAN)?;
        super::plan_leftovers(scan, &selected)
    }

    #[tauri::command]
    pub async fn remove_leftovers(
        app: AppHandle,
        state: State<'_, AppState>,
        selected: Vec<PathBuf>,
    ) -> Result<RemovalReport, String> {
        // Taking the scan makes a second request fail instead of removing twice.
        let scan = lock(&state.leftover_scan).take().ok_or(NO_SCAN)?;
        let ctx = state.ctx.clone();
        let for_work = scan.clone();
        let result = blocking(move || {
            super::remove_leftovers(&read(&ctx), &for_work, &selected, &mut |p| {
                let _ = app.emit("leftovers-progress", p);
            })
        })
        .await?;
        if result.is_err() {
            // Nothing ran (empty selection): keep the scan for a retry.
            lock(&state.leftover_scan).get_or_insert(scan);
        }
        result
    }

    #[tauri::command]
    pub async fn history(state: State<'_, AppState>) -> Result<Vec<UndoBatch>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::history(&read(&ctx))).await?
    }

    #[tauri::command]
    pub async fn restore(
        state: State<'_, AppState>,
        ids: Vec<String>,
    ) -> Result<Vec<RestoreOutcome>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::restore(&read(&ctx), &ids)).await
    }

    #[tauri::command]
    pub async fn forget_history(
        state: State<'_, AppState>,
        ids: Vec<String>,
    ) -> Result<usize, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::forget_history(&read(&ctx), &ids)).await?
    }

    #[tauri::command]
    pub async fn removed_apps(state: State<'_, AppState>) -> Result<Vec<RemovedApp>, String> {
        let ctx = state.ctx.clone();
        blocking(move || super::removed_apps(&read(&ctx))).await?
    }

    #[tauri::command]
    pub async fn forget_removed_app(
        state: State<'_, AppState>,
        bundle_id: String,
    ) -> Result<(), String> {
        let ctx = state.ctx.clone();
        blocking(move || super::forget_removed_app(&read(&ctx), &bundle_id)).await?
    }

    /// The app handed over before the UI started listening, if any.
    #[tauri::command]
    pub fn take_pending_open(state: State<'_, AppState>) -> Option<PathBuf> {
        lock(&state.pending_open).take()
    }
}
