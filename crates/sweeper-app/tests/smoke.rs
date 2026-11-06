//! End-to-end smoke tests of the command layer on a dummy app, standing in
//! for a UI test (XCUITest is unavailable and Tauri WebDriver has no macOS
//! support). Everything runs in a temporary fake home with test doubles.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use sweeper_app::commands;
use sweeper_app::state::{AppContext, Settings};
use sweeper_core::testing::{
    write_file, FakeProcesses, FakeSpotlight, FakeTrash, Fixture, RecordingRunner,
};
use sweeper_core::{CommandOutput, CommandRunner, FdaStatus, Preset, RuleSet, RunningApp};

const BUNDLE_ID: &str = "com.foo.app";

/// Lets the test keep a handle on the runner owned by the context.
struct SharedRunner(Arc<RecordingRunner>);

impl CommandRunner for SharedRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.0.run(program, args)
    }
}

struct Harness {
    fx: Fixture,
    ctx: AppContext,
    runner: Arc<RecordingRunner>,
    app: PathBuf,
}

fn harness(running: bool) -> Harness {
    let fx = Fixture::new();
    let app = fx.install("Foo", BUNDLE_ID, "1.0");
    for leftover in [
        "Library/Caches/com.foo.app/cache.bin",
        "Library/Preferences/com.foo.app.plist",
        "Library/LaunchAgents/com.foo.app.helper.plist",
        "Library/Saved Application State/com.foo.app.savedState/windows.plist",
        "Library/Application Support/Foo/data.bin",
    ] {
        write_file(&fx.env.home.join(leftover), 100);
    }
    let runner = Arc::new(RecordingRunner::new());
    let processes = FakeProcesses::new();
    if running {
        processes.add(
            BUNDLE_ID,
            RunningApp {
                pid: 4242,
                path: app.clone(),
                name: "Foo".into(),
            },
            false,
        );
    }
    let ctx = AppContext::new(
        fx.env.clone(),
        RuleSet::bundled(),
        Box::new(SharedRunner(runner.clone())),
        Box::new(FakeSpotlight::default()),
        Box::new(processes),
        Box::new(FakeTrash::new(fx.trash_dir.clone())),
    );
    Harness {
        fx,
        ctx,
        runner,
        app,
    }
}

#[test]
fn uninstall_flow_end_to_end() {
    let h = harness(true);
    let ctx = &h.ctx;

    // The list contains the app and sizes arrive afterwards.
    let apps = commands::list_apps(ctx);
    let listed = apps
        .iter()
        .find(|a| a.bundle_id == BUNDLE_ID)
        .expect("Foo is listed");
    let mut sizes = Vec::new();
    commands::app_sizes(&apps, &AtomicBool::new(false), &mut |u| sizes.push(u));
    assert!(sizes.iter().any(|u| u.path == listed.path && u.size > 0));

    // Scan: everything is checked and the dry run matches the selection.
    let scan = commands::scan_app(ctx, &h.app, &AtomicBool::new(false)).expect("scan");
    assert!(scan.items().count() >= 5, "app bundle plus leftovers");
    let plan = commands::plan_removal(&scan);
    assert_eq!(plan.total_size, scan.selected_size());
    assert_eq!(plan.items.len(), scan.items().filter(|i| i.checked).count());
    assert!(plan.items.iter().any(|i| i.path == h.app));

    // Presets change the dry run.
    let app_only = commands::apply_preset(scan.clone(), Preset::AppOnly);
    assert_eq!(commands::plan_removal(&app_only).items.len(), 1);

    // The running instance is found and quits politely.
    assert_eq!(commands::running_instances(ctx, &scan).len(), 1);
    let outcome = commands::quit_apps(ctx, &scan, false);
    assert_eq!(outcome.exited.len(), 1);
    assert!(outcome.still_running.is_empty());
    assert!(commands::running_instances(ctx, &scan).is_empty());

    // Uninstall reports progress and leaves nothing behind.
    let mut progress = Vec::new();
    let report = commands::uninstall(ctx, &scan, &mut |p| progress.push(p)).expect("uninstall");
    assert_eq!(progress.len(), plan.items.len());
    assert!(
        report.removal.failed.is_empty(),
        "{:?}",
        report.removal.failed
    );
    assert!(report.remaining.is_empty(), "{:?}", report.remaining);
    assert!(!h.app.exists());
    assert!(
        h.fx.trash_dir.join("Foo.app").exists(),
        "bundle is in the Trash"
    );
    assert!(!h.fx.lib("Caches/com.foo.app").exists());

    // Undo log and system steps.
    let undo = commands::undo_entries(ctx).expect("undo entries");
    assert_eq!(undo.len(), plan.items.len());
    let boots = h.runner.calls_of("launchctl");
    assert!(boots.iter().any(|c| c[0] == "bootout"), "{boots:?}");
    let log = fs::read_to_string(ctx.log.path()).expect("action log");
    assert!(log.contains("uninstall") && log.contains("trash"));
}

#[test]
fn delete_immediately_bypasses_the_trash() {
    let mut h = harness(false);
    let settings = Settings {
        delete_immediately: true,
        ..Settings::default()
    };
    commands::save_settings(&mut h.ctx, &settings).expect("save");
    commands::list_apps(&h.ctx);
    let scan = commands::scan_app(&h.ctx, &h.app, &AtomicBool::new(false)).expect("scan");
    let report = commands::uninstall(&h.ctx, &scan, &mut |_| {}).expect("uninstall");
    assert!(report.remaining.is_empty());
    assert!(!h.fx.trash_dir.join("Foo.app").exists());
    assert!(commands::undo_entries(&h.ctx).expect("undo").is_empty());
}

#[test]
fn apple_apps_cannot_be_uninstalled() {
    let h = harness(false);
    let apple = h.fx.install("Calc", "com.apple.calculator", "1.0");
    commands::list_apps(&h.ctx);
    let scan = commands::scan_app(&h.ctx, &apple, &AtomicBool::new(false)).expect("scan");
    assert!(commands::plan_removal(&scan).items.is_empty());
    assert!(commands::uninstall(&h.ctx, &scan, &mut |_| {}).is_err());
    assert!(apple.exists());
}

#[test]
fn scan_rejects_non_bundles_and_cancellation_is_reported() {
    let h = harness(false);
    let not_app = h.fx.tmp.path().join("notes.txt");
    write_file(&not_app, 1);
    assert!(commands::scan_app(&h.ctx, &not_app, &AtomicBool::new(false)).is_err());
    let err = commands::scan_app(&h.ctx, &h.app, &AtomicBool::new(true)).unwrap_err();
    assert_eq!(err, commands::CANCELLED);
}

#[test]
fn app_outside_app_folders_is_scanned_but_refused() {
    let h = harness(false);
    let downloads = h.fx.tmp.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    let dropped = sweeper_core::testing::make_app(&downloads, "Dropped", "com.dropped.app", "2.0");
    commands::list_apps(&h.ctx);
    let scan = commands::scan_app(&h.ctx, &dropped, &AtomicBool::new(false)).expect("scan");
    assert_eq!(scan.app.bundle_id, "com.dropped.app");
    assert!(
        commands::plan_removal(&scan).items.is_empty(),
        "outside allowed roots"
    );
}

#[test]
fn settings_round_trip_and_extra_folders_are_listed() {
    let mut h = harness(false);
    let extra = h.fx.tmp.path().join("MoreApps");
    fs::create_dir_all(&extra).unwrap();
    sweeper_core::testing::make_app(&extra, "Bar", "com.bar.app", "1.0");
    let extra = fs::canonicalize(&extra).unwrap();

    let wanted = Settings {
        extra_app_folders: vec![extra.clone()],
        protected: vec!["com.keep.me".into(), "~/Documents".into()],
        delete_immediately: true,
    };
    let saved = commands::save_settings(&mut h.ctx, &wanted).expect("save");
    assert_eq!(saved, wanted);
    assert!(commands::list_apps(&h.ctx)
        .iter()
        .any(|a| a.bundle_id == "com.bar.app"));

    // A new context (a restart) reads the same settings back.
    let reloaded = AppContext::new(
        h.fx.env.clone(),
        RuleSet::bundled(),
        Box::new(RecordingRunner::new()),
        Box::new(FakeSpotlight::default()),
        Box::new(FakeProcesses::new()),
        Box::new(FakeTrash::new(h.fx.trash_dir.clone())),
    );
    assert_eq!(commands::get_settings(&reloaded), wanted);
    assert!(reloaded.env.app_dirs.contains(&extra));
}

#[test]
fn invalid_settings_are_rejected_and_not_stored() {
    let mut h = harness(false);
    let missing = h.fx.tmp.path().join("does-not-exist");
    let file = h.fx.tmp.path().join("a-file");
    write_file(&file, 1);
    for bad in [
        PathBuf::from("relative/dir"),
        missing,
        file,
        PathBuf::from("/System/Library"),
        PathBuf::from("/usr/bin"),
    ] {
        let settings = Settings {
            extra_app_folders: vec![bad.clone()],
            ..Settings::default()
        };
        assert!(
            commands::save_settings(&mut h.ctx, &settings).is_err(),
            "{} should be rejected",
            bad.display()
        );
    }
    let bad_protected = Settings {
        protected: vec!["not a bundle id!".into()],
        ..Settings::default()
    };
    assert!(commands::save_settings(&mut h.ctx, &bad_protected).is_err());
    assert!(!h.ctx.settings_path().exists());
    assert_eq!(commands::get_settings(&h.ctx), Settings::default());
}

#[test]
fn user_rules_are_merged_and_bad_rules_are_reported() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.env.data_dir).unwrap();
    fs::write(fx.env.data_dir.join("rules.json"), "{ not json").unwrap();
    let ctx = AppContext::new(
        fx.env.clone(),
        RuleSet::bundled(),
        Box::new(RecordingRunner::new()),
        Box::new(FakeSpotlight::default()),
        Box::new(FakeProcesses::new()),
        Box::new(FakeTrash::new(fx.trash_dir.clone())),
    );
    assert!(ctx.rules_warning.is_some());
    assert_eq!(ctx.rules, RuleSet::bundled());
}

#[test]
fn reveal_is_limited_to_policy_approved_paths_and_app_folders() {
    let h = harness(false);
    assert!(commands::reveal_target(&h.ctx, &h.app).is_ok());
    assert!(commands::reveal_target(&h.ctx, &h.fx.apps_dir).is_ok());
    assert!(commands::reveal_target(&h.ctx, std::path::Path::new("/System/Library")).is_err());
    assert!(commands::reveal_target(&h.ctx, std::path::Path::new("relative")).is_err());
}

#[test]
fn fda_status_is_reported() {
    let h = harness(false);
    // The fake home has no TCC database, so the probe cannot tell.
    assert_eq!(commands::fda_status(&h.ctx), FdaStatus::Unknown);
}

#[test]
fn brew_zap_uses_the_installed_cask() {
    let h = harness(false);
    let cask = h.fx.env.caskroom_dirs[0].join("foo/1.0");
    fs::create_dir_all(&cask).unwrap();
    fs::create_dir_all(cask.join("Foo.app")).unwrap();
    let apps = commands::list_apps(&h.ctx);
    let app = apps.iter().find(|a| a.bundle_id == BUNDLE_ID).unwrap();
    let command = commands::brew_zap_command(&h.ctx, app).expect("cask command");
    assert_eq!(command, ["brew", "uninstall", "--cask", "--zap", "foo"]);
    assert!(commands::brew_zap(&h.ctx, app).ok);
    assert!(h
        .runner
        .calls()
        .iter()
        .any(|c| c.contains(&"--zap".to_string())));
}
