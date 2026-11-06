//! Orchestrates a full uninstall: stop background items, remove files, clean
//! up system registrations and verify what is left.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::catalog::AppInfo;
use crate::env::Env;
use crate::history::RemovedApps;
use crate::path_policy::PathPolicy;
use crate::remover::{Progress, RemovalMode, RemovalPlan, RemovalReport, Remover};
use crate::scanner::{LeftoverScanner, ScanResult};
use crate::system::{is_valid_bundle_id, CommandRunner, Processes, RunningApp};

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Time given to a force-terminated process to disappear.
const FORCE_WAIT: Duration = Duration::from_secs(2);
/// `launchctl bootout` exit codes meaning the service was not loaded.
const LAUNCHCTL_NOT_LOADED: [i32; 2] = [3, 113];

/// Result of one uninstall step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl StepResult {
    fn new(name: &str, ok: bool, detail: impl Into<String>) -> Self {
        StepResult {
            name: name.to_string(),
            ok,
            detail: detail.into(),
        }
    }
}

/// Outcome of [`Uninstaller::quit`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuitOutcome {
    /// Quit after a polite request.
    pub exited: Vec<RunningApp>,
    /// Had to be force-terminated.
    pub forced: Vec<RunningApp>,
    /// Still running (not forced, or force failed).
    pub still_running: Vec<RunningApp>,
}

/// Everything that happened during [`Uninstaller::run`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    pub steps: Vec<StepResult>,
    pub removal: RemovalReport,
    /// Items a fresh scan still finds, with a reason.
    pub remaining: Vec<(PathBuf, String)>,
}

/// Runs uninstalls.
pub struct Uninstaller<'a> {
    env: &'a Env,
    policy: &'a PathPolicy,
    runner: &'a dyn CommandRunner,
    processes: &'a dyn Processes,
    remover: &'a Remover<'a>,
    scanner: &'a LeftoverScanner<'a>,
    installed: &'a [AppInfo],
    history: RemovedApps,
}

impl<'a> Uninstaller<'a> {
    /// `installed` is the current app list, used by the final rescan to avoid
    /// claiming other apps' files.
    pub fn new(
        env: &'a Env,
        policy: &'a PathPolicy,
        runner: &'a dyn CommandRunner,
        processes: &'a dyn Processes,
        remover: &'a Remover<'a>,
        scanner: &'a LeftoverScanner<'a>,
        installed: &'a [AppInfo],
    ) -> Self {
        Uninstaller {
            env,
            policy,
            runner,
            processes,
            remover,
            scanner,
            installed,
            history: RemovedApps::for_env(env),
        }
    }

    /// Records uninstalled apps in `history` instead of the default file in
    /// the data directory.
    pub fn with_history(mut self, history: RemovedApps) -> Self {
        self.history = history;
        self
    }

    /// Running instances of the scanned app, covering every copy with its
    /// bundle identifier. The UI asks the user before quitting them.
    pub fn running_instances(&self, scan: &ScanResult) -> Vec<RunningApp> {
        self.processes.running(&scan.app.bundle_id)
    }

    /// Asks `apps` to quit, waits up to `grace`, then force-terminates the
    /// survivors only if `allow_force`.
    pub fn quit(&self, apps: &[RunningApp], grace: Duration, allow_force: bool) -> QuitOutcome {
        for app in apps {
            self.processes.terminate(app.pid);
        }
        let mut alive = self.wait_until_gone(apps, grace);
        let mut outcome = QuitOutcome {
            exited: apps
                .iter()
                .filter(|a| !alive.contains(a))
                .cloned()
                .collect(),
            ..QuitOutcome::default()
        };
        if allow_force && !alive.is_empty() {
            for app in &alive {
                self.processes.force_terminate(app.pid);
            }
            let still = self.wait_until_gone(&alive, FORCE_WAIT);
            outcome.forced = alive
                .iter()
                .filter(|a| !still.contains(a))
                .cloned()
                .collect();
            alive = still;
        }
        outcome.still_running = alive;
        outcome
    }

    fn wait_until_gone(&self, apps: &[RunningApp], timeout: Duration) -> Vec<RunningApp> {
        let deadline = Instant::now() + timeout;
        loop {
            let alive: Vec<RunningApp> = apps
                .iter()
                .filter(|a| self.processes.is_running(a.pid))
                .cloned()
                .collect();
            if alive.is_empty() || Instant::now() >= deadline {
                return alive;
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    /// Performs the uninstall of the checked items in `scan`.
    pub fn run(
        &self,
        scan: &ScanResult,
        mode: RemovalMode,
        progress: &mut dyn FnMut(Progress),
    ) -> UninstallReport {
        let mut steps = Vec::new();
        let plan = RemovalPlan::from_scan(scan);
        let agents_dir = self.env.home.join("Library/LaunchAgents");

        for item in &plan.items {
            if item.path.parent() == Some(agents_dir.as_path()) {
                steps.push(self.unload_agent(&item.path));
            }
        }

        let mut removal = self.remover.execute(&plan, mode, progress);
        steps.push(StepResult::new(
            "Remove files",
            removal.failed.is_empty(),
            format!(
                "{} removed, {} failed",
                removal.removed.len(),
                removal.failed.len()
            ),
        ));

        let app = &scan.app;
        let bundle_removed = removal
            .removed
            .iter()
            .any(|r| same_item(&r.original, &app.path));
        let prefs_removed = removal.removed.iter().any(|r| {
            r.original
                .parent()
                .is_some_and(|p| p.ends_with("Library/Preferences"))
        });
        if (bundle_removed || prefs_removed) && !app.is_apple {
            steps.push(self.defaults_delete(&app.bundle_id));
        }
        if bundle_removed && !app.is_apple {
            // Lets a later orphan scan recognise this app's leftovers.
            if let Err(e) = self.history.record(app) {
                removal.warnings.push(format!(
                    "could not add {} to the removal history: {e}",
                    app.name
                ));
            }
        }
        if bundle_removed {
            steps.push(self.lsregister_unregister(&app.path));
            steps.push(self.remove_login_item(&app.path));
        }

        let remaining = self.rescan(scan);
        UninstallReport {
            steps,
            removal,
            remaining,
        }
    }

    fn unload_agent(&self, plist: &Path) -> StepResult {
        let name = "Unload background item";
        let domain = format!("gui/{}", self.env.uid);
        let plist_arg = plist.to_string_lossy();
        match self
            .runner
            .run("launchctl", &["bootout", &domain, &plist_arg])
        {
            Ok(o) if o.success() => StepResult::new(name, true, plist_arg),
            Ok(o) if LAUNCHCTL_NOT_LOADED.contains(&o.status) => {
                StepResult::new(name, true, format!("{plist_arg}: was not loaded"))
            }
            Ok(o) => StepResult::new(name, false, format!("{plist_arg}: {}", o.stderr.trim())),
            Err(e) => StepResult::new(name, false, format!("{plist_arg}: {e}")),
        }
    }

    fn defaults_delete(&self, bundle_id: &str) -> StepResult {
        let name = "Clear cached preferences";
        if !is_valid_bundle_id(bundle_id) {
            return StepResult::new(name, false, "bundle identifier has unexpected characters");
        }
        match self.runner.run("defaults", &["delete", bundle_id]) {
            Ok(o) if o.success() || o.stderr.contains("does not exist") => {
                StepResult::new(name, true, bundle_id)
            }
            Ok(o) => StepResult::new(name, false, o.stderr.trim()),
            Err(e) => StepResult::new(name, false, e.to_string()),
        }
    }

    fn lsregister_unregister(&self, app: &Path) -> StepResult {
        let name = "Unregister from Launch Services";
        let path = app.to_string_lossy();
        match self.runner.run(LSREGISTER, &["-u", &path]) {
            Ok(o) if o.success() => StepResult::new(name, true, path),
            Ok(o) => StepResult::new(name, false, o.stderr.trim()),
            Err(e) => StepResult::new(name, false, e.to_string()),
        }
    }

    /// Removes the app's login item through System Events. `SMAppService`
    /// can only manage the calling app's own login items, so AppleScript is
    /// the closest safe option; macOS may ask for Automation permission, and
    /// a failure is reported as a warning only.
    fn remove_login_item(&self, app: &Path) -> StepResult {
        let name = "Remove login item";
        let path = app.to_string_lossy();
        let args = [
            "-e",
            "on run argv",
            "-e",
            "tell application \"System Events\" to delete (every login item whose path is item 1 of argv)",
            "-e",
            "end run",
            &path,
        ];
        match self.runner.run("osascript", &args) {
            Ok(o) if o.success() => StepResult::new(name, true, path),
            Ok(o) => StepResult::new(name, false, o.stderr.trim()),
            Err(e) => StepResult::new(name, false, e.to_string()),
        }
    }

    fn rescan(&self, scan: &ScanResult) -> Vec<(PathBuf, String)> {
        let cancel = AtomicBool::new(false);
        let fresh = self.scanner.scan(&scan.app, self.installed, &cancel);
        fresh
            .items()
            .map(|i| {
                let why = i.refused.clone().unwrap_or_else(|| i.reason.clone());
                (i.path.clone(), why)
            })
            .collect()
    }

    /// Runs `brew uninstall --cask --zap <cask>` for a Homebrew-installed app.
    pub fn run_brew_zap(&self, app: &AppInfo) -> StepResult {
        let name = "Homebrew zap";
        let Some(command) = brew_zap_command(app) else {
            return StepResult::new(name, false, "not installed by a Homebrew cask");
        };
        let program = brew_program();
        let args: Vec<&str> = command[1..].iter().map(String::as_str).collect();
        match self.runner.run(&program, &args) {
            Ok(o) if o.success() => StepResult::new(name, true, command.join(" ")),
            Ok(o) => StepResult::new(name, false, o.stderr.trim()),
            Err(e) => StepResult::new(name, false, e.to_string()),
        }
    }

    /// Exposes the policy used for removal (for diagnostics in the UI).
    pub fn policy(&self) -> &PathPolicy {
        self.policy
    }
}

/// `["brew","uninstall","--cask","--zap",<cask>]` for Homebrew apps whose
/// cask name is safe to pass on the command line.
pub fn brew_zap_command(app: &AppInfo) -> Option<Vec<String>> {
    match &app.source {
        crate::catalog::AppSource::Homebrew { cask } if is_valid_cask_name(cask) => Some(
            ["brew", "uninstall", "--cask", "--zap", cask]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        _ => None,
    }
}

fn is_valid_cask_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '@' | '.' | '_' | '+' | '-')
        })
}

/// GUI apps do not inherit the shell PATH, so prefer the known brew prefixes.
fn brew_program() -> String {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .find(|p| Path::new(p).exists())
        .map_or_else(|| "brew".to_string(), |p| p.to_string())
}

/// True if `removed` (canonical) is the item at `original` (as scanned).
fn same_item(removed: &Path, original: &Path) -> bool {
    let canonical = original
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .zip(original.file_name())
        .map(|(p, n)| p.join(n));
    removed == original || canonical.as_deref() == Some(removed)
}
