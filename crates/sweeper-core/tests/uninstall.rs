mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use sweeper_core::testing::{FakeProcesses, FakeSpotlight, FakeTrash, Fixture, RecordingRunner};
use sweeper_core::{
    brew_zap_command, AppSource, CommandOutput, CommandRunner, LeftoverScanner, PathPolicy,
    RealSpotlight, RemovalMode, Remover, RuleSet, RunningApp, Spotlight, UndoLog, Uninstaller,
};

const FOO: &str = "com.foo.app";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

struct Shared(Arc<RecordingRunner>);

impl CommandRunner for Shared {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.0.run(program, args)
    }
}

fn running(pid: i32) -> RunningApp {
    RunningApp {
        pid,
        path: "/Applications/Foo.app".into(),
        name: "Foo".into(),
    }
}

/// Wires an `Uninstaller` over a fixture and hands it to `body`.
fn with_uninstaller<R>(
    fx: &Fixture,
    runner: &RecordingRunner,
    processes: &FakeProcesses,
    body: impl FnOnce(&Uninstaller<'_>, &sweeper_core::AppInfo) -> R,
    bundle_id: &str,
) -> R {
    let apps = installed(fx);
    let app = app_by_id(&apps, bundle_id);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let scanner = LeftoverScanner::new(&fx.env, &rules, &policy, &spot);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let u = Uninstaller::new(
        &fx.env, &policy, runner, processes, &remover, &scanner, &apps,
    );
    body(&u, &app)
}

fn populated() -> Fixture {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    touch_dir(&fx, "Library/Application Support/Foo");
    touch(&fx, "Library/Caches/com.foo.app/c.bin");
    touch(&fx, "Library/Preferences/com.foo.app.plist");
    touch(&fx, "Library/LaunchAgents/com.foo.app.helper.plist");
    fx
}

#[test]
fn full_uninstall_runs_system_steps_in_order_and_leaves_nothing() {
    let fx = populated();
    let runner = RecordingRunner::new();
    let report = with_uninstaller(
        &fx,
        &runner,
        &FakeProcesses::new(),
        |u, _| {
            let scan = scan(&fx, FOO);
            u.run(&scan, RemovalMode::Trash, &mut |_| {})
        },
        FOO,
    );

    let plist = fx
        .lib("LaunchAgents")
        .join("com.foo.app.helper.plist")
        .display()
        .to_string();
    let app = fx.apps_dir.join("Foo.app").display().to_string();
    let calls = runner.calls();
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            strs(&["launchctl", "bootout", "gui/501", &plist]),
            strs(&["defaults", "delete", FOO]),
            strs(&[LSREGISTER, "-u", &app]),
            strs(&[
                "osascript",
                "-e",
                "on run argv",
                "-e",
                "tell application \"System Events\" to delete (every login item whose path is item 1 of argv)",
                "-e",
                "end run",
                &app
            ]),
        ]
    );
    assert!(
        report.removal.failed.is_empty(),
        "{:?}",
        report.removal.failed
    );
    assert_eq!(report.removal.removed.len(), 5);
    assert!(report.remaining.is_empty(), "{:?}", report.remaining);
    assert!(report.steps.iter().all(|s| s.ok), "{:?}", report.steps);
    assert!(!fx.apps_dir.join("Foo.app").exists());
    assert_eq!(UndoLog::for_env(&fx.env).entries().unwrap().len(), 5);
}

#[test]
fn step_failures_are_warnings_and_do_not_abort() {
    let fx = populated();
    let runner = RecordingRunner::new();
    let fail = |stderr: &str| CommandOutput {
        status: 1,
        stdout: String::new(),
        stderr: stderr.into(),
    };
    runner.respond("launchctl", fail("Boot-out failed: 5: Input/output error"));
    runner.respond("osascript", fail("not authorized"));
    runner.respond("defaults", fail("Domain com.foo.app does not exist"));
    let report = with_uninstaller(
        &fx,
        &runner,
        &FakeProcesses::new(),
        |u, _| u.run(&scan(&fx, FOO), RemovalMode::Delete, &mut |_| {}),
        FOO,
    );
    let ok = |name: &str| {
        report
            .steps
            .iter()
            .find(|s| s.name.starts_with(name))
            .unwrap()
            .ok
    };
    assert!(!ok("Unload"));
    assert!(ok("Clear"), "'does not exist' is not an error");
    assert!(!ok("Remove login"));
    assert!(ok("Remove files"));
    assert!(report.remaining.is_empty());
}

#[test]
fn launchctl_not_loaded_is_not_a_warning() {
    let fx = populated();
    let runner = RecordingRunner::new();
    runner.respond(
        "launchctl",
        CommandOutput {
            status: 113,
            stdout: String::new(),
            stderr: "Could not find service".into(),
        },
    );
    let report = with_uninstaller(
        &fx,
        &runner,
        &FakeProcesses::new(),
        |u, _| u.run(&scan(&fx, FOO), RemovalMode::Delete, &mut |_| {}),
        FOO,
    );
    assert!(report.steps.iter().all(|s| s.ok));
}

#[test]
fn rescan_reports_items_that_could_not_be_removed() {
    let fx = populated();
    let stuck = fx.lib("Caches").join("com.foo.app");
    let apps = installed(&fx);
    let app = app_by_id(&apps, FOO);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let scanner = LeftoverScanner::new(&fx.env, &rules, &policy, &spot);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    trash.fail_on(&stuck, "in use");
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let (runner, procs) = (RecordingRunner::new(), FakeProcesses::new());
    let u = Uninstaller::new(&fx.env, &policy, &runner, &procs, &remover, &scanner, &apps);
    let report = u.run(&scan(&fx, FOO), RemovalMode::Delete, &mut |_| {});
    assert_eq!(report.removal.failed.len(), 1);
    assert_eq!(report.remaining.len(), 1);
    assert_eq!(report.remaining[0].0, stuck);
    assert_eq!(app.bundle_id, FOO);
}

#[test]
fn defaults_never_runs_for_invalid_bundle_ids() {
    let fx = Fixture::new();
    fx.install("Odd", "bad id;$(rm)", "1.0");
    let runner = RecordingRunner::new();
    let report = with_uninstaller(
        &fx,
        &runner,
        &FakeProcesses::new(),
        |u, _| u.run(&scan(&fx, "bad id;$(rm)"), RemovalMode::Delete, &mut |_| {}),
        "bad id;$(rm)",
    );
    assert!(runner.calls_of("defaults").is_empty());
    assert!(report
        .steps
        .iter()
        .any(|s| s.name.starts_with("Clear") && !s.ok));
}

#[test]
fn mdfind_only_receives_valid_bundle_ids() {
    let runner = Arc::new(RecordingRunner::new());
    runner.respond(
        "mdfind",
        CommandOutput {
            status: 0,
            stdout: "/Applications/Foo.app\n/Applications/Foo.app/Contents/Helper.txt\n".into(),
            stderr: String::new(),
        },
    );
    let spotlight = RealSpotlight::with_runner(Box::new(Shared(runner.clone())));
    for bad in ["", "com.foo'; rm -rf ~; '", "-x", "com foo", ".hidden"] {
        assert!(spotlight.copies_of(bad).is_empty());
    }
    assert!(
        runner.calls().is_empty(),
        "invalid ids must not reach mdfind"
    );

    let found = spotlight.copies_of(FOO);
    assert_eq!(
        found,
        vec![std::path::PathBuf::from("/Applications/Foo.app")]
    );
    assert_eq!(
        runner.calls(),
        vec![vec![
            "mdfind".to_string(),
            "kMDItemCFBundleIdentifier == 'com.foo.app'".to_string()
        ]]
    );
}

#[test]
fn mdls_batch_parses_dates_and_null() {
    let runner = Arc::new(RecordingRunner::new());
    runner.respond(
        "mdls",
        CommandOutput {
            status: 0,
            stdout: "2024-05-01 10:11:12 +0000\0(null)\0".into(),
            stderr: String::new(),
        },
    );
    let spotlight = RealSpotlight::with_runner(Box::new(Shared(runner.clone())));
    let paths = vec!["/A.app".into(), "/B.app".into()];
    let used = spotlight.last_used(&paths);
    assert_eq!(used.len(), 1);
    assert!(used.contains_key(&paths[0]));
    assert_eq!(
        runner.calls()[0],
        vec![
            "mdls",
            "-raw",
            "-name",
            "kMDItemLastUsedDate",
            "/A.app",
            "/B.app"
        ]
    );
}

#[test]
fn running_guard_quit_and_force() {
    let fx = populated();
    let procs = FakeProcesses::new();
    procs.add(FOO, running(100), false);
    procs.add(FOO, running(101), true);
    procs.add("com.other", running(200), false);
    let runner = RecordingRunner::new();

    with_uninstaller(
        &fx,
        &runner,
        &procs,
        |u, _| {
            let scan = scan(&fx, FOO);
            let instances = u.running_instances(&scan);
            assert_eq!(
                instances.iter().map(|a| a.pid).collect::<Vec<_>>(),
                vec![100, 101]
            );

            let polite = u.quit(&instances, Duration::from_millis(250), false);
            assert_eq!(
                polite.exited.iter().map(|a| a.pid).collect::<Vec<_>>(),
                vec![100]
            );
            assert_eq!(
                polite
                    .still_running
                    .iter()
                    .map(|a| a.pid)
                    .collect::<Vec<_>>(),
                vec![101]
            );
            assert!(polite.forced.is_empty());
            assert!(procs.forced().is_empty(), "no force without permission");

            let forced = u.quit(&polite.still_running, Duration::from_millis(250), true);
            assert_eq!(
                forced.forced.iter().map(|a| a.pid).collect::<Vec<_>>(),
                vec![101]
            );
            assert!(forced.still_running.is_empty());
        },
        FOO,
    );
    assert_eq!(procs.forced(), vec![101]);
    assert!(procs.is_running_for_test(200));
}

trait RunningProbe {
    fn is_running_for_test(&self, pid: i32) -> bool;
}

impl RunningProbe for FakeProcesses {
    fn is_running_for_test(&self, pid: i32) -> bool {
        sweeper_core::Processes::is_running(self, pid)
    }
}

#[test]
fn brew_zap_command_is_built_only_for_valid_casks() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    let mut app = installed(&fx).remove(0);
    assert_eq!(brew_zap_command(&app), None);

    app.source = AppSource::Homebrew {
        cask: "foo-app@beta".into(),
    };
    assert_eq!(
        brew_zap_command(&app).unwrap(),
        ["brew", "uninstall", "--cask", "--zap", "foo-app@beta"]
    );
    for bad in ["", "-rf", "Foo", "foo bar", "foo;ls", "../x"] {
        app.source = AppSource::Homebrew { cask: bad.into() };
        assert_eq!(brew_zap_command(&app), None, "{bad:?}");
    }
}

#[test]
fn run_brew_zap_uses_the_runner() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    let runner = RecordingRunner::new();
    with_uninstaller(
        &fx,
        &runner,
        &FakeProcesses::new(),
        |u, app| {
            assert!(!u.run_brew_zap(app).ok);
            let mut cask_app = app.clone();
            cask_app.source = AppSource::Homebrew { cask: "foo".into() };
            assert!(u.run_brew_zap(&cask_app).ok);
        },
        FOO,
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0][0].ends_with("brew"));
    assert_eq!(calls[0][1..], ["uninstall", "--cask", "--zap", "foo"]);
}

/// Uninstalls `bundle_id` (Trash mode) recording into `history`.
fn uninstall_recording(
    fx: &Fixture,
    bundle_id: &str,
    history: sweeper_core::RemovedApps,
    trash_failure: Option<&str>,
) -> sweeper_core::UninstallReport {
    let apps = installed(fx);
    let app = app_by_id(&apps, bundle_id);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let scanner = LeftoverScanner::new(&fx.env, &rules, &policy, &spot);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    if let Some(reason) = trash_failure {
        trash.fail_on(&app.path, reason);
    }
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let runner = RecordingRunner::new();
    let processes = FakeProcesses::new();
    let u = Uninstaller::new(
        &fx.env, &policy, &runner, &processes, &remover, &scanner, &apps,
    )
    .with_history(history);
    let scan = scanner.scan(&app, &apps, &std::sync::atomic::AtomicBool::new(false));
    u.run(&scan, RemovalMode::Trash, &mut |_| {})
}

#[test]
fn uninstall_records_the_app_in_the_removal_history() {
    let fx = populated();
    let history = sweeper_core::RemovedApps::for_env(&fx.env);
    let report = uninstall_recording(&fx, FOO, history.clone(), None);
    assert!(
        report.removal.warnings.is_empty(),
        "{:?}",
        report.removal.warnings
    );
    let entries = history.entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].bundle_id, FOO);
    assert_eq!(entries[0].name, "Foo");
    assert!(entries[0].removed_at > 0);
    assert_eq!(entries[0].app_path, fx.apps_dir.join("Foo.app"));
}

#[test]
fn apple_apps_are_never_recorded() {
    let fx = Fixture::new();
    fx.install("Safari", "com.apple.Safari", "17.0");
    let history = sweeper_core::RemovedApps::for_env(&fx.env);
    uninstall_recording(&fx, "com.apple.Safari", history.clone(), None);
    assert!(history.entries().unwrap().is_empty());
    // Recording directly is a no-op as well.
    let apple = app_by_id(&installed(&fx), "com.apple.Safari");
    history.record(&apple).unwrap();
    assert!(!history.contains("com.apple.Safari"));
}

#[test]
fn history_is_not_written_when_the_bundle_was_not_removed() {
    let fx = populated();
    let history = sweeper_core::RemovedApps::for_env(&fx.env);
    let report = uninstall_recording(&fx, FOO, history.clone(), Some("operation not permitted"));
    assert!(report
        .removal
        .failed
        .iter()
        .any(|(p, _)| *p == fx.apps_dir.join("Foo.app")));
    assert!(history.entries().unwrap().is_empty());
}

#[test]
fn history_write_failure_is_only_a_warning() {
    let fx = populated();
    // A file where the history's parent directory should be.
    let blocker = fx.tmp.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let history = sweeper_core::RemovedApps::new(blocker.join("removed-apps.json"));
    let report = uninstall_recording(&fx, FOO, history, None);
    assert!(report.removal.failed.is_empty());
    assert!(report
        .removal
        .warnings
        .iter()
        .any(|w| w.contains("removal history")));
    assert!(!fx.apps_dir.join("Foo.app").exists());
}
