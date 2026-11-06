//! Application context, persisted settings and shared Tauri state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use serde::{Deserialize, Serialize};
use sweeper_core::{
    cleaner_roots, is_valid_bundle_id, ActionLog, AppInfo, CleanOptions, CleanRules, CleanScan,
    CommandRunner, Env, PathPolicy, Processes, RealProcesses, RealRunner, RealSpotlight,
    RealToolLocator, RealTrasher, RuleSet, Spotlight, ToolLocator, Trasher,
};

const SETTINGS_FILE: &str = "settings.json";
const USER_RULES_FILE: &str = "rules.json";
const USER_CLEAN_RULES_FILE: &str = "cleaner-rules.json";
/// Valid range of [`Settings::downloads_age_days`] (ten years at most).
pub const DOWNLOADS_AGE_RANGE: std::ops::RangeInclusive<u32> = 1..=3650;
/// Largest accepted [`Settings::min_item_size_mb`] (1 TB).
pub const MAX_MIN_ITEM_SIZE_MB: u64 = 1_000_000;
const BYTES_PER_MB: u64 = 1_000_000;
/// Folders that can never be added as an app folder.
const FORBIDDEN_FOLDERS: [&str; 4] = ["/System", "/usr", "/bin", "/sbin"];
const MAX_LIST_LEN: usize = 256;
const MAX_ENTRY_LEN: usize = 1024;

/// User preferences, stored as JSON in the data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub extra_app_folders: Vec<PathBuf>,
    /// Absolute paths (or `~/...`) and bundle identifiers that are never removed.
    pub protected: Vec<String>,
    pub delete_immediately: bool,
    /// The cleaner hides entries smaller than this many megabytes (0 = none hidden).
    pub min_item_size_mb: u64,
    /// Downloads older than this many days are offered by the cleaner.
    pub downloads_age_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            extra_app_folders: Vec::new(),
            protected: Vec::new(),
            delete_immediately: false,
            min_item_size_mb: 0,
            downloads_age_days: 30,
        }
    }
}

impl Settings {
    /// Scan options for the cleaner.
    pub fn clean_options(&self) -> CleanOptions {
        CleanOptions {
            downloads_age_days: self.downloads_age_days,
            min_size: self.min_item_size_mb.saturating_mul(BYTES_PER_MB),
        }
    }
}

/// Everything the command functions need. Tests build one from doubles.
pub struct AppContext {
    pub env: Env,
    pub policy: PathPolicy,
    pub rules: RuleSet,
    pub settings: Settings,
    pub runner: Box<dyn CommandRunner>,
    pub spotlight: Box<dyn Spotlight>,
    pub processes: Box<dyn Processes>,
    pub trasher: Box<dyn Trasher>,
    /// Bundled cleaner rules merged with the user's `cleaner-rules.json`.
    pub clean_rules: CleanRules,
    pub tools: Box<dyn ToolLocator>,
    /// The normal policy plus the roots the cleaner rules need (`~/.npm`, ...).
    pub cleaner_policy: PathPolicy,
    pub log: ActionLog,
    /// Last list produced by `list_apps`; used when scanning and uninstalling.
    pub installed: Mutex<Vec<AppInfo>>,
    /// Why the user rules file was ignored, if it was.
    pub rules_warning: Option<String>,
    /// Why the user cleaner rules file was ignored, if it was.
    pub clean_rules_warning: Option<String>,
    default_app_dirs: Vec<PathBuf>,
}

impl AppContext {
    /// Loads settings and the optional user rules file from `env.data_dir`.
    /// `rules` is the base rule set the user rules are merged into.
    pub fn new(
        env: Env,
        rules: RuleSet,
        runner: Box<dyn CommandRunner>,
        spotlight: Box<dyn Spotlight>,
        processes: Box<dyn Processes>,
        trasher: Box<dyn Trasher>,
    ) -> AppContext {
        let default_app_dirs = env.app_dirs.clone();
        let mut settings = load_settings(&env);
        // Folders may have disappeared since they were saved.
        settings.extra_app_folders.retain(|p| p.is_dir());
        let (rules, rules_warning) = load_rules(&env, rules);
        let (clean_rules, clean_rules_warning) = load_clean_rules(&env);
        let mut ctx = AppContext {
            log: ActionLog::new(&env),
            policy: PathPolicy::new(&env, &[]),
            cleaner_policy: PathPolicy::new(&env, &[]),
            clean_rules,
            tools: Box::new(RealToolLocator),
            env,
            rules,
            settings: Settings::default(),
            runner,
            spotlight,
            processes,
            trasher,
            installed: Mutex::new(Vec::new()),
            rules_warning,
            clean_rules_warning,
            default_app_dirs,
        };
        ctx.apply_settings(settings);
        for root in ctx.cleaner_policy.rejected_extra_roots() {
            ctx.log.record("rejected-cleaner-root", root);
        }
        ctx
    }

    /// Uses another tool locator for the cleaner actions (tests).
    pub fn with_tools(mut self, tools: Box<dyn ToolLocator>) -> AppContext {
        self.tools = tools;
        self
    }

    /// The context used by the running application.
    pub fn production() -> AppContext {
        AppContext::new(
            Env::current(),
            RuleSet::bundled(),
            Box::new(RealRunner),
            Box::new(RealSpotlight::new()),
            Box::new(RealProcesses),
            Box::new(RealTrasher),
        )
    }

    /// Makes `settings` effective: app folders and the protected list.
    pub fn apply_settings(&mut self, settings: Settings) {
        let mut dirs = self.default_app_dirs.clone();
        for extra in &settings.extra_app_folders {
            if !dirs.contains(extra) {
                dirs.push(extra.clone());
            }
        }
        self.env.app_dirs = dirs;
        self.policy = PathPolicy::new(&self.env, &settings.protected);
        self.cleaner_policy = PathPolicy::new(&self.env, &settings.protected)
            .with_extra_roots(&cleaner_roots(&self.env, &self.clean_rules));
        self.settings = settings;
    }

    pub fn settings_path(&self) -> PathBuf {
        self.env.data_dir.join(SETTINGS_FILE)
    }

    /// Snapshot of the cached app list.
    pub fn installed_apps(&self) -> Vec<AppInfo> {
        lock(&self.installed).clone()
    }
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn load_settings(env: &Env) -> Settings {
    fs::read(env.data_dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn load_rules(env: &Env, base: RuleSet) -> (RuleSet, Option<String>) {
    let path = env.data_dir.join(USER_RULES_FILE);
    if !path.exists() {
        return (base, None);
    }
    match RuleSet::load(&path) {
        Ok(user) => (base.merge(user), None),
        Err(e) => (base, Some(format!("Ignored {}: {e}", path.display()))),
    }
}

fn load_clean_rules(env: &Env) -> (CleanRules, Option<String>) {
    let base = CleanRules::bundled();
    let path = env.data_dir.join(USER_CLEAN_RULES_FILE);
    if !path.exists() {
        return (base, None);
    }
    match CleanRules::load(&path) {
        Ok(user) => (base.merge(user), None),
        Err(e) => (base, Some(format!("Ignored {}: {e}", path.display()))),
    }
}

/// Checks `settings` and returns the normalized form to store.
pub fn validate_settings(settings: &Settings) -> Result<Settings, String> {
    if settings.extra_app_folders.len() > MAX_LIST_LEN || settings.protected.len() > MAX_LIST_LEN {
        return Err("Too many entries".into());
    }
    if !DOWNLOADS_AGE_RANGE.contains(&settings.downloads_age_days) {
        return Err(format!(
            "Old downloads must be older than {} to {} days",
            DOWNLOADS_AGE_RANGE.start(),
            DOWNLOADS_AGE_RANGE.end()
        ));
    }
    if settings.min_item_size_mb > MAX_MIN_ITEM_SIZE_MB {
        return Err(format!(
            "Minimum item size must be at most {MAX_MIN_ITEM_SIZE_MB} MB"
        ));
    }
    let mut folders: Vec<PathBuf> = Vec::new();
    for folder in &settings.extra_app_folders {
        let canonical = validate_folder(folder)?;
        if !folders.contains(&canonical) {
            folders.push(canonical);
        }
    }
    let mut protected: Vec<String> = Vec::new();
    for entry in &settings.protected {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.len() > MAX_ENTRY_LEN || entry.chars().any(char::is_control) {
            return Err(format!("Invalid protected entry: {entry}"));
        }
        let is_path = entry.starts_with('/') || entry.starts_with("~/");
        if !is_path && !is_valid_bundle_id(entry) {
            return Err(format!(
                "\"{entry}\" is neither an absolute path nor a bundle identifier"
            ));
        }
        if !protected.iter().any(|p| p == entry) {
            protected.push(entry.to_string());
        }
    }
    Ok(Settings {
        extra_app_folders: folders,
        protected,
        delete_immediately: settings.delete_immediately,
        min_item_size_mb: settings.min_item_size_mb,
        downloads_age_days: settings.downloads_age_days,
    })
}

fn validate_folder(folder: &Path) -> Result<PathBuf, String> {
    if !folder.is_absolute() {
        return Err(format!("{} is not an absolute path", folder.display()));
    }
    let canonical = fs::canonicalize(folder)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| format!("{} is not an existing folder", folder.display()))?;
    for candidate in [folder, canonical.as_path()] {
        if FORBIDDEN_FOLDERS
            .iter()
            .any(|forbidden| candidate.starts_with(forbidden))
        {
            return Err(format!(
                "{} is a system folder and cannot be used",
                folder.display()
            ));
        }
    }
    Ok(canonical)
}

/// Validates, stores (atomically) and applies `settings`.
pub fn save_settings(ctx: &mut AppContext, settings: &Settings) -> Result<Settings, String> {
    let settings = validate_settings(settings)?;
    write_atomic(
        &ctx.settings_path(),
        &serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Could not save settings: {e}"))?;
    ctx.log.record("save-settings", &ctx.settings_path());
    ctx.apply_settings(settings.clone());
    Ok(settings)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("settings path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Cancel flag of the operation currently running; starting a new one
/// cancels the previous.
#[derive(Default)]
pub struct CancelSlot(Mutex<Option<Arc<AtomicBool>>>);

impl CancelSlot {
    /// Cancels the running operation and returns the flag for the next one.
    pub fn renew(&self) -> Arc<AtomicBool> {
        let mut slot = lock(&self.0);
        if let Some(old) = slot.as_ref() {
            old.store(true, Ordering::Relaxed);
        }
        let flag = Arc::new(AtomicBool::new(false));
        *slot = Some(flag.clone());
        flag
    }
}

/// Tauri-managed state.
pub struct AppState {
    pub ctx: Arc<RwLock<AppContext>>,
    pub scan_cancel: CancelSlot,
    pub sizes_cancel: CancelSlot,
    pub clean_cancel: CancelSlot,
    /// Last cleaner scan; plans are always rebuilt from it, never from the UI.
    pub clean_scan: Mutex<Option<CleanScan>>,
    /// An app opened before the UI was ready to listen for `open-app`.
    pub pending_open: Mutex<Option<PathBuf>>,
}

impl AppState {
    pub fn new(ctx: AppContext) -> AppState {
        AppState {
            ctx: Arc::new(RwLock::new(ctx)),
            scan_cancel: CancelSlot::default(),
            sizes_cancel: CancelSlot::default(),
            clean_cancel: CancelSlot::default(),
            clean_scan: Mutex::new(None),
            pending_open: Mutex::new(None),
        }
    }
}
