//! End-to-end tests of the cleaner command layer in a fake home with test
//! doubles: nothing outside the temporary directory is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sweeper_app::commands;
use sweeper_app::state::{AppContext, Settings};
use sweeper_core::testing::{
    write_file, FakeProcesses, FakeSpotlight, FakeToolLocator, FakeTrash, Fixture, RecordingRunner,
};
use sweeper_core::{CleanEntry, CleanMode, CleanScan, CommandOutput, CommandRunner, RuleSet, Tool};

const BREW: &str = "/opt/homebrew/bin/brew";

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
}

struct Files {
    chrome_cache: PathBuf,
    chrome_history: PathBuf,
    app_cache: PathBuf,
    trashed: PathBuf,
    old_download: PathBuf,
    new_download: PathBuf,
}

fn context(fx: &Fixture, runner: Arc<RecordingRunner>) -> AppContext {
    AppContext::new(
        fx.env.clone(),
        RuleSet::bundled(),
        Box::new(SharedRunner(runner)),
        Box::new(FakeSpotlight::default()),
        Box::new(FakeProcesses::new()),
        Box::new(FakeTrash::new(fx.trash_dir.clone())),
    )
    .with_tools(Box::new(FakeToolLocator::new().with(Tool::Brew, BREW)))
}

fn harness() -> (Harness, Files) {
    let fx = Fixture::new();
    let home = fx.env.home.clone();
    let chrome = home.join("Library/Application Support/Google/Chrome/Default");
    let files = Files {
        chrome_cache: chrome.join("Cache"),
        chrome_history: chrome.join("History"),
        app_cache: home.join("Library/Caches/com.example.tool"),
        trashed: home.join(".Trash/old-notes.txt"),
        old_download: home.join("Downloads/old.zip"),
        new_download: home.join("Downloads/new.zip"),
    };
    write_file(&files.chrome_cache.join("data_0"), 4000);
    write_file(&files.chrome_history, 700);
    write_file(&files.app_cache.join("blob.bin"), 3000);
    write_file(&files.trashed, 500);
    write_file(&files.old_download, 800);
    write_file(&files.new_download, 900);
    let sixty_days = SystemTime::now() - Duration::from_secs(60 * 24 * 3600);
    fs::File::options()
        .write(true)
        .open(&files.old_download)
        .and_then(|f| f.set_modified(sixty_days))
        .expect("age the old download");

    let runner = Arc::new(RecordingRunner::new());
    let ctx = context(&fx, runner.clone());
    (Harness { fx, ctx, runner }, files)
}

fn scan(h: &Harness) -> CleanScan {
    commands::list_apps(&h.ctx);
    commands::scan_cleaner(&h.ctx, &AtomicBool::new(false)).expect("scan")
}

fn entry_for<'a>(scan: &'a CleanScan, path: &Path) -> Option<&'a CleanEntry> {
    scan.entries().find(|e| e.path.as_deref() == Some(path))
}

fn id_for(scan: &CleanScan, path: &Path) -> String {
    entry_for(scan, path)
        .unwrap_or_else(|| panic!("{} is listed", path.display()))
        .id
        .clone()
}

fn brew_id(scan: &CleanScan) -> String {
    scan.entries()
        .find(|e| e.action.is_some() && e.id.contains("homebrew_cleanup"))
        .expect("brew action is listed")
        .id
        .clone()
}

#[test]
fn scan_lists_caches_but_never_personal_browser_data() {
    let (h, f) = harness();
    let scan = scan(&h);
    assert!(entry_for(&scan, &f.chrome_cache).is_some_and(|e| e.checked));
    assert!(entry_for(&scan, &f.app_cache).is_some_and(|e| e.checked));
    assert!(entry_for(&scan, &f.chrome_history).is_none());
    assert!(!scan
        .entries()
        .any(|e| e.path.as_deref().is_some_and(|p| p.ends_with("History"))));
    // Unchecked by default: Trash (delete only), old downloads, the brew action.
    let trashed = entry_for(&scan, &f.trashed).expect("trash item");
    assert_eq!(trashed.mode, CleanMode::DeleteOnly);
    assert!(!trashed.checked);
    assert!(entry_for(&scan, &f.old_download).is_some_and(|e| !e.checked));
    assert!(entry_for(&scan, &f.new_download).is_none(), "too recent");
}

#[test]
fn plan_uses_exactly_the_selected_ids() {
    let (h, f) = harness();
    let scan = scan(&h);
    let selected = vec![
        id_for(&scan, &f.chrome_cache),
        id_for(&scan, &f.trashed),
        brew_id(&scan),
    ];
    let plan = commands::plan_clean(&scan, &selected).expect("plan");
    assert_eq!(plan.trash_items.len(), 1);
    assert_eq!(plan.trash_items[0].path, f.chrome_cache);
    assert_eq!(plan.delete_items.len(), 1);
    assert_eq!(plan.delete_items[0].path, f.trashed);
    assert_eq!(plan.actions.len(), 1);
    let sized = |p: &Path| entry_for(&scan, p).and_then(|e| e.size).unwrap();
    assert_eq!(plan.total_size, sized(&f.chrome_cache) + sized(&f.trashed));

    // The selection replaces the scan's default checks: nothing selected, empty plan.
    let none = commands::plan_clean(&scan, &[]).expect("empty plan");
    assert!(none.trash_items.is_empty() && none.delete_items.is_empty() && none.actions.is_empty());
    assert_eq!(none.total_size, 0);
}

#[test]
fn unknown_ids_are_an_error() {
    let (h, f) = harness();
    let scan = scan(&h);
    let selected = vec![id_for(&scan, &f.chrome_cache), "caches/x/etc/passwd".into()];
    assert!(commands::plan_clean(&scan, &selected).is_err());
    let err = commands::clean(&h.ctx, &scan, &selected, &mut |_| {}).unwrap_err();
    assert!(err.contains("Unknown item"), "{err}");
    assert!(f.chrome_cache.exists(), "nothing ran");
    assert!(h.runner.calls().is_empty());
}

#[test]
fn refused_entries_are_ignored_when_selected() {
    let (h, f) = harness();
    let mut scan = scan(&h);
    let id = id_for(&scan, &f.app_cache);
    for e in scan
        .categories
        .iter_mut()
        .flat_map(|c| c.entries.iter_mut())
    {
        if e.id == id {
            e.refused = Some("test".into());
        }
    }
    let plan = commands::plan_clean(&scan, &[id]).expect("plan");
    assert!(plan.trash_items.is_empty());
}

#[test]
fn clean_trashes_caches_deletes_trash_items_and_runs_brew() {
    let (h, f) = harness();
    let scan = scan(&h);
    let selected = vec![
        id_for(&scan, &f.chrome_cache),
        id_for(&scan, &f.app_cache),
        id_for(&scan, &f.trashed),
        brew_id(&scan),
    ];
    let mut progress = Vec::new();
    let report =
        commands::clean(&h.ctx, &scan, &selected, &mut |p| progress.push(p)).expect("clean");

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.removed.len(), 3);
    assert!(report.freed > 0);
    assert_eq!(progress.len(), 4);
    assert!(progress.iter().all(|p| p.total == 4));

    // Caches are in the (fake) Trash with undo entries; the Trash item is gone for good.
    assert!(!f.chrome_cache.exists() && !f.app_cache.exists());
    assert!(h.fx.trash_dir.join("Cache").exists());
    assert!(h.fx.trash_dir.join("com.example.tool").exists());
    assert!(!f.trashed.exists());
    let undo = commands::undo_entries(&h.ctx).expect("undo");
    assert_eq!(undo.len(), 2);
    assert!(undo.iter().any(|u| u.original == f.chrome_cache));

    // Brew ran with exactly these arguments.
    assert_eq!(h.runner.calls_of(BREW), [vec!["cleanup", "-s"]]);
    assert_eq!(report.actions.len(), 1);
    assert!(report.actions[0].ok);

    // Personal data and recent downloads are untouched.
    assert!(f.chrome_history.exists());
    assert!(f.new_download.exists());
    assert!(f.old_download.exists(), "not selected");

    let log = fs::read_to_string(h.ctx.log.path()).expect("action log");
    assert!(log.contains("clean"));
}

#[test]
fn unselected_entries_stay_even_if_checked_by_default() {
    let (h, f) = harness();
    let scan = scan(&h);
    assert!(entry_for(&scan, &f.app_cache).is_some_and(|e| e.checked));
    let selected = vec![id_for(&scan, &f.chrome_cache)];
    commands::clean(&h.ctx, &scan, &selected, &mut |_| {}).expect("clean");
    assert!(f.app_cache.exists());
    assert!(!f.chrome_cache.exists());
}

#[test]
fn settings_round_trip_with_cleaner_fields_and_validation() {
    let (mut h, _f) = harness();
    let wanted = Settings {
        min_item_size_mb: 5,
        downloads_age_days: 90,
        ..Settings::default()
    };
    let saved = commands::save_settings(&mut h.ctx, &wanted).expect("save");
    assert_eq!(saved, wanted);
    assert_eq!(h.ctx.settings.clean_options().min_size, 5_000_000);
    assert_eq!(h.ctx.settings.clean_options().downloads_age_days, 90);

    let reloaded = context(&h.fx, Arc::new(RecordingRunner::new()));
    assert_eq!(commands::get_settings(&reloaded), wanted);

    for bad in [0, 3651] {
        let settings = Settings {
            downloads_age_days: bad,
            ..wanted.clone()
        };
        assert!(
            commands::save_settings(&mut h.ctx, &settings).is_err(),
            "{bad} days should be rejected"
        );
    }
    let too_big = Settings {
        min_item_size_mb: u64::MAX,
        ..wanted.clone()
    };
    assert!(commands::save_settings(&mut h.ctx, &too_big).is_err());
    assert_eq!(commands::get_settings(&h.ctx), wanted, "unchanged");
}

#[test]
fn settings_files_from_before_the_cleaner_still_load() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.env.data_dir).unwrap();
    fs::write(
        fx.env.data_dir.join("settings.json"),
        r#"{"extra_app_folders": [], "protected": ["com.keep.me"], "delete_immediately": true}"#,
    )
    .unwrap();
    let ctx = context(&fx, Arc::new(RecordingRunner::new()));
    let settings = commands::get_settings(&ctx);
    assert_eq!(settings.protected, ["com.keep.me"]);
    assert!(settings.delete_immediately);
    assert_eq!(settings.min_item_size_mb, 0);
    assert_eq!(settings.downloads_age_days, 30);
}

#[test]
fn bad_user_cleaner_rules_are_reported_and_ignored() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.env.data_dir).unwrap();
    fs::write(fx.env.data_dir.join("cleaner-rules.json"), "{ not json").unwrap();
    let ctx = context(&fx, Arc::new(RecordingRunner::new()));
    assert!(ctx.clean_rules_warning.is_some());
    assert_eq!(ctx.clean_rules, sweeper_core::CleanRules::bundled());
}

#[test]
fn cancelled_scan_is_reported() {
    let (h, _f) = harness();
    let err = commands::scan_cleaner(&h.ctx, &AtomicBool::new(true)).unwrap_err();
    assert_eq!(err, commands::CANCELLED);
}

#[test]
fn reveal_accepts_cleaner_locations() {
    let (h, f) = harness();
    assert!(commands::reveal_target(&h.ctx, &f.old_download).is_ok());
    assert!(commands::reveal_target(&h.ctx, &f.trashed).is_ok());
}
