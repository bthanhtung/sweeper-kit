mod common;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use common::*;
use sweeper_core::testing::{
    write_file, FakeProcesses, FakeToolLocator, FakeTrash, Fixture, RecordingRunner,
};
use sweeper_core::{
    cleaner_roots, AppInfo, CacheScanner, CleanAction, CleanEntry, CleanMode, CleanOptions,
    CleanPlan, CleanRules, CleanScan, Cleaner, CommandOutput, PathPolicy, PlanItem, RunningApp,
    Tool, UndoLog,
};

const XCRUN: &str = "/usr/bin/xcrun";
const BREW: &str = "/opt/homebrew/bin/brew";
const DOCKER: &str = "/usr/local/bin/docker";

fn all_tools() -> FakeToolLocator {
    FakeToolLocator::new()
        .with(Tool::Xcrun, XCRUN)
        .with(Tool::Brew, BREW)
        .with(Tool::Docker, DOCKER)
}

fn policy_for(fx: &Fixture, rules: &CleanRules) -> PathPolicy {
    PathPolicy::new(&fx.env, &[]).with_extra_roots(&cleaner_roots(&fx.env, rules))
}

struct Setup {
    processes: FakeProcesses,
    tools: FakeToolLocator,
    runner: RecordingRunner,
    options: CleanOptions,
    installed: Vec<AppInfo>,
}

impl Setup {
    fn new() -> Setup {
        Setup {
            processes: FakeProcesses::new(),
            tools: FakeToolLocator::new(),
            runner: RecordingRunner::new(),
            options: CleanOptions::default(),
            installed: Vec::new(),
        }
    }

    fn scan_with_policy(&self, fx: &Fixture, policy: &PathPolicy) -> CleanScan {
        let rules = CleanRules::bundled();
        CacheScanner::new(&fx.env, &rules, policy, &self.processes, &self.tools)
            .with_runner(&self.runner)
            .scan(&self.installed, &self.options, &AtomicBool::new(false))
    }

    fn scan(&self, fx: &Fixture) -> CleanScan {
        let policy = policy_for(fx, &CleanRules::bundled());
        self.scan_with_policy(fx, &policy)
    }
}

fn entry<'a>(scan: &'a CleanScan, path: &Path) -> &'a CleanEntry {
    scan.entries()
        .find(|e| e.path.as_deref() == Some(path))
        .unwrap_or_else(|| panic!("{} not listed", path.display()))
}

fn listed(scan: &CleanScan, path: &Path) -> bool {
    scan.entries().any(|e| e.path.as_deref() == Some(path))
}

fn category<'a>(scan: &'a CleanScan, id: &str) -> Option<&'a sweeper_core::CleanCategory> {
    scan.categories.iter().find(|c| c.id == id)
}

fn age_file(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

// ---- rules ----

#[test]
fn bundled_rules_load() {
    let rules = CleanRules::bundled();
    let ids: Vec<&str> = rules.categories.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "user_caches",
            "logs",
            "browser_caches",
            "developer",
            "trash",
            "old_downloads"
        ]
    );
}

#[test]
fn browser_paths_end_in_cache_dirs() {
    let rules = CleanRules::bundled();
    let browsers = rules
        .categories
        .iter()
        .find(|c| c.id == "browser_caches")
        .unwrap();
    let allowed = [
        "Cache",
        "Code Cache",
        "GPUCache",
        "CacheStorage",
        "cache2",
        "com.apple.Safari",
        "Arc",
    ];
    for path in browsers.items.iter().flat_map(|i| &i.paths) {
        let last = path.rsplit('/').next().unwrap();
        assert!(allowed.contains(&last), "{path} ends in {last}");
    }
}

fn rule_json(path: &str) -> String {
    format!(
        r#"{{"version":1,"categories":[{{"id":"c","title":"C","items":[
            {{"id":"i","title":"I","paths":["{path}"],"default_checked":true}}]}}]}}"#
    )
}

#[test]
fn rules_targeting_personal_browser_data_are_rejected() {
    for name in [
        "Default/History",
        "Default/Cookies",
        "Default/Login Data",
        "Default/login data",
        "Default/Web Data",
        "Default/Bookmarks",
        "Default/Favicons",
        "Default/Top Sites",
        "Default/Preferences",
        "Local State",
        "Profiles/x/places.sqlite",
        "Profiles/x/cookies.sqlite",
        "Profiles/x/key4.db",
        "Profiles/x/logins.json",
        "Profiles/x/formhistory.sqlite",
    ] {
        let path = format!("~/Library/Application Support/Browser/{name}");
        assert!(
            CleanRules::from_json(&rule_json(&path)).is_err(),
            "{path} was accepted"
        );
    }
    assert!(CleanRules::from_json(&rule_json("~/Library/Caches/Browser/Default/Cache")).is_ok());
}

#[test]
fn malformed_rules_are_rejected() {
    for path in [
        "/Library/Caches/x",
        "Library/Caches/x",
        "~/Library/../Documents",
        "~/Library/**/x",
        "~/Library//x",
        "~/",
        "~/Library/Caches/[a]",
    ] {
        assert!(
            CleanRules::from_json(&rule_json(path)).is_err(),
            "{path} was accepted"
        );
    }
    assert!(CleanRules::from_json(&rule_json("~/Library/Logs/*.log")).is_ok());

    let unknown_action = r#"{"version":1,"categories":[{"id":"c","title":"C","items":[
        {"id":"i","title":"I","action":"rm_rf"}]}]}"#;
    let unknown_field = r#"{"version":1,"categories":[{"id":"c","title":"C","items":[
        {"id":"i","title":"I","paths":["~/x"],"surprise":true}]}]}"#;
    let action_with_paths = r#"{"version":1,"categories":[{"id":"c","title":"C","items":[
        {"id":"i","title":"I","action":"brew_cleanup","paths":["~/x"]}]}]}"#;
    let delete_elsewhere = r#"{"version":1,"categories":[{"id":"c","title":"C","items":[
        {"id":"i","title":"I","paths":["~/Library/Caches/x"],"mode":"delete_only"}]}]}"#;
    let bad_version = r#"{"version":2,"categories":[]}"#;
    let duplicate = r#"{"version":1,"categories":[{"id":"c","title":"C","items":[
        {"id":"i","title":"I","paths":["~/a"]},{"id":"i","title":"J","paths":["~/b"]}]}]}"#;
    for json in [
        unknown_action,
        unknown_field,
        action_with_paths,
        delete_elsewhere,
        bad_version,
        duplicate,
    ] {
        assert!(CleanRules::from_json(json).is_err(), "{json}");
    }
}

#[test]
fn merge_replaces_by_id_and_appends_new() {
    let base = CleanRules::bundled();
    let user = CleanRules::from_json(
        r#"{"version":1,"categories":[
            {"id":"developer","title":"Dev","items":[
                {"id":"npm","title":"My npm","paths":["~/.npm/_cacache"],"default_checked":false},
                {"id":"bun","title":"Bun","paths":["~/.bun/install/cache"],"default_checked":true}]},
            {"id":"mine","title":"Mine","items":[{"id":"x","title":"X","paths":["~/.x"]}]}]}"#,
    )
    .unwrap();
    let merged = base.clone().merge(user);
    assert_eq!(merged.categories.len(), base.categories.len() + 1);
    let dev = merged
        .categories
        .iter()
        .find(|c| c.id == "developer")
        .unwrap();
    let base_dev = base
        .categories
        .iter()
        .find(|c| c.id == "developer")
        .unwrap();
    assert_eq!(dev.items.len(), base_dev.items.len() + 1);
    let npm = dev.items.iter().find(|i| i.id == "npm").unwrap();
    assert_eq!(npm.title, "My npm");
    assert!(!npm.default_checked);
    assert!(dev.items.iter().any(|i| i.id == "bun"));
}

#[test]
fn cleaner_roots_cover_only_locations_outside_the_library_roots() {
    let fx = Fixture::new();
    let roots = cleaner_roots(&fx.env, &CleanRules::bundled());
    let home = &fx.env.home;
    for expected in [
        ".npm",
        ".cargo/registry",
        ".gradle",
        "Downloads",
        ".Trash",
        "Library/pnpm",
        "Library/Developer/Xcode/DerivedData",
        "Library/Developer/CoreSimulator",
    ] {
        assert!(roots.contains(&home.join(expected)), "missing {expected}");
    }
    assert!(!roots.iter().any(|r| r.starts_with(fx.lib("Caches"))));
    assert!(!roots
        .iter()
        .any(|r| r.starts_with(fx.lib("Application Support"))));
}

#[test]
fn bundled_cleaner_roots_are_all_accepted_by_the_policy() {
    let fx = Fixture::new();
    let rules = CleanRules::bundled();
    let policy = policy_for(&fx, &rules);
    assert!(policy.rejected_extra_roots().is_empty());
}

// ---- scan ----

#[test]
fn browser_scan_lists_only_cache_dirs() {
    let fx = Fixture::new();
    let chrome = "Library/Application Support/Google/Chrome";
    let cache = touch_dir(&fx, &format!("{chrome}/Default/Cache"));
    let history = touch(&fx, &format!("{chrome}/Default/History"));
    let cookies = touch(&fx, &format!("{chrome}/Default/Cookies"));
    let code = touch_dir(&fx, &format!("{chrome}/Profile 1/Code Cache"));
    let scan = Setup::new().scan(&fx);

    let browsers = category(&scan, "browser_caches").unwrap();
    let paths: Vec<_> = browsers
        .entries
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&cache) && paths.contains(&code));
    assert!(!listed(&scan, &history) && !listed(&scan, &cookies));
    let e = entry(&scan, &cache);
    assert!(e.checked && e.refused.is_none());
    assert_eq!(e.title, "Google Chrome — Default/Cache");
    assert_eq!(e.size, Some(100));
}

#[test]
fn wildcard_children_never_list_personal_data() {
    let fx = Fixture::new();
    let history = touch(&fx, "Library/Caches/History");
    let ok = touch(&fx, "Library/Caches/com.acme.app/a.bin");
    let scan = Setup::new().scan(&fx);
    assert!(!listed(&scan, &history));
    assert!(listed(&scan, ok.parent().unwrap()));
}

#[test]
fn user_caches_list_children_with_names_and_exclusions() {
    let fx = Fixture::new();
    fx.install("Acme", "com.acme.app", "1.0");
    let acme = touch_dir(&fx, "Library/Caches/com.acme.app");
    let unknown = touch_dir(&fx, "Library/Caches/com.unknown.tool");
    let apple = touch_dir(&fx, "Library/Caches/com.apple.Maps");
    let loose = touch(&fx, "Library/Caches/loose.bin");
    for covered in [
        "Google",
        "Firefox",
        "com.apple.Safari",
        "Homebrew",
        "pip",
        "Yarn",
        "CocoaPods",
        "go-build",
        "com.apple.nsurlsessiond",
        "Microsoft Edge",
    ] {
        touch_dir(&fx, &format!("Library/Caches/{covered}"));
    }
    let mut setup = Setup::new();
    setup.installed = installed(&fx);
    let scan = setup.scan(&fx);

    let caches = category(&scan, "user_caches").unwrap();
    let mut got: Vec<_> = caches
        .entries
        .iter()
        .filter_map(|e| e.path.clone())
        .collect();
    got.sort();
    let mut want = vec![acme.clone(), unknown.clone(), apple.clone(), loose.clone()];
    want.sort();
    assert_eq!(rel(&fx, &got), rel(&fx, &want));

    assert_eq!(entry(&scan, &acme).title, "Acme");
    assert!(entry(&scan, &acme).checked);
    assert_eq!(entry(&scan, &unknown).title, "com.unknown.tool");
    assert!(entry(&scan, &unknown).checked);
    assert!(!entry(&scan, &apple).checked);
    assert!(entry(&scan, &apple).refused.is_none());
    assert!(entry(&scan, &loose).checked);
    assert_eq!(caches.size, 100 * 4);
    assert_eq!(scan.total_size, scan_total(&scan));
}

fn scan_total(scan: &CleanScan) -> u64 {
    scan.entries().map(|e| e.size.unwrap_or(0)).sum()
}

#[test]
fn own_cache_and_logs_are_not_listed() {
    let fx = Fixture::new();
    let own = touch_dir(&fx, "Library/Caches/dev.sweeperkit.app");
    let own_logs = touch_dir(&fx, "Library/Logs/Sweeper Kit");
    let reports = touch_dir(&fx, "Library/Logs/DiagnosticReports");
    let other = touch(&fx, "Library/Logs/other.log");
    let scan = Setup::new().scan(&fx);
    assert!(!listed(&scan, &own) && !listed(&scan, &own_logs));
    assert!(listed(&scan, &other));
    let e = entry(&scan, &reports);
    assert_eq!(e.title, "Crash reports");
}

#[test]
fn downloads_are_filtered_by_age_and_unchecked() {
    let fx = Fixture::new();
    let old = touch(&fx, "Downloads/old.zip");
    let fresh = touch(&fx, "Downloads/new.zip");
    age_file(&old, 60);
    let mut setup = Setup::new();
    let scan = setup.scan(&fx);

    let e = entry(&scan, &old);
    assert!(!e.checked);
    assert!(e.refused.is_none());
    assert!(!listed(&scan, &fresh));

    setup.options.downloads_age_days = 90;
    assert!(!listed(&setup.scan(&fx), &old));
    setup.options.downloads_age_days = 0;
    let all = setup.scan(&fx);
    assert!(listed(&all, &old) && listed(&all, &fresh));
}

#[test]
fn downloads_are_refused_without_the_extra_root() {
    let fx = Fixture::new();
    let old = touch(&fx, "Downloads/old.zip");
    age_file(&old, 60);
    let setup = Setup::new();
    let plain = PathPolicy::new(&fx.env, &[]);
    let scan = setup.scan_with_policy(&fx, &plain);
    let e = entry(&scan, &old);
    assert!(!e.checked);
    assert!(e.refused.as_deref().unwrap().contains("personal folder"));
}

#[test]
fn trash_entries_are_delete_only_and_unchecked() {
    let fx = Fixture::new();
    let junk = touch(&fx, ".Trash/junk.bin");
    let scan = Setup::new().scan(&fx);
    let e = entry(&scan, &junk);
    assert_eq!(e.mode, CleanMode::DeleteOnly);
    assert!(!e.checked);
    assert!(e.refused.is_none());
    assert_eq!(category(&scan, "trash").unwrap().entries.len(), 1);
}

#[test]
fn min_size_drops_small_entries_but_not_actions() {
    let fx = Fixture::new();
    let big = fx.lib("Caches/com.big.app/blob");
    write_file(&big, 5_000);
    let small = touch(&fx, "Library/Caches/com.small.app/blob");
    let mut setup = Setup::new();
    setup.tools = all_tools();
    setup.options.min_size = 1_000;
    let scan = setup.scan(&fx);
    assert!(listed(&scan, big.parent().unwrap()));
    assert!(!listed(&scan, small.parent().unwrap()));
    assert!(scan.entries().any(|e| e.action.is_some()));
}

#[test]
fn running_app_blocks_its_entries() {
    let fx = Fixture::new();
    let chrome_cache = touch_dir(
        &fx,
        "Library/Application Support/Google/Chrome/Default/Cache",
    );
    let slack = touch_dir(&fx, "Library/Caches/com.tinyspeck.slackmacgap");
    let idle = touch_dir(&fx, "Library/Caches/com.idle.app");
    let setup = Setup::new();
    let app = |pid, name: &str| RunningApp {
        pid,
        path: PathBuf::from("/Applications/x.app"),
        name: name.to_string(),
    };
    setup
        .processes
        .add("com.google.Chrome", app(10, "Google Chrome"), false);
    setup
        .processes
        .add("com.tinyspeck.slackmacgap", app(11, "Slack"), false);
    let scan = setup.scan(&fx);

    let e = entry(&scan, &chrome_cache);
    assert!(!e.checked);
    assert_eq!(
        e.warning.as_deref(),
        Some("Google Chrome is running — close it first")
    );
    let s = entry(&scan, &slack);
    assert!(!s.checked);
    assert_eq!(
        s.warning.as_deref(),
        Some("Slack is running — close it first")
    );
    let i = entry(&scan, &idle);
    assert!(i.checked && i.warning.is_none());
}

#[test]
fn developer_items_appear_only_when_present() {
    let fx = Fixture::new();
    let scan = Setup::new().scan(&fx);
    assert!(category(&scan, "developer").is_none());

    let npm = touch_dir(&fx, ".npm/_cacache");
    let derived = touch_dir(&fx, "Library/Developer/Xcode/DerivedData/App-abc");
    let archives = touch_dir(&fx, "Library/Developer/Xcode/Archives/2024-01-01");
    let devices = touch_dir(&fx, "Library/Developer/Xcode/iOS DeviceSupport/17.0");
    let sim = touch_dir(&fx, "Library/Developer/CoreSimulator/Caches");
    let cargo = touch_dir(&fx, ".cargo/registry/cache");
    let scan = Setup::new().scan(&fx);
    let dev = category(&scan, "developer").unwrap();
    assert_eq!(dev.entries.len(), 6);
    assert!(entry(&scan, &npm).checked);
    assert!(entry(&scan, &derived).checked);
    assert!(entry(&scan, &sim).checked);
    assert!(entry(&scan, &cargo).checked);
    assert!(!entry(&scan, &archives).checked);
    assert!(!entry(&scan, &devices).checked);
    assert!(scan.entries().all(|e| e.refused.is_none()));
}

#[test]
fn symlinked_cache_is_refused_and_not_followed() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    std::fs::create_dir_all(fx.env.home.join(".npm")).unwrap();
    let link = fx.env.home.join(".npm/_cacache");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let scan = Setup::new().scan(&fx);
    let e = entry(&scan, &link);
    assert!(!e.checked);
    assert!(e.refused.as_deref().unwrap().contains("symbolic link"));
    assert!(e.size.unwrap() < 100, "symlink target must not be sized");
}

#[test]
fn actions_need_their_tools() {
    let fx = Fixture::new();
    let scan = Setup::new().scan(&fx);
    assert!(scan.entries().all(|e| e.action.is_none()));

    let mut setup = Setup::new();
    setup.tools = all_tools();
    let scan = setup.scan(&fx);
    let actions: Vec<_> = scan.entries().filter_map(|e| e.action).collect();
    assert_eq!(actions.len(), 3);
    let by = |a| scan.entries().find(|e| e.action == Some(a)).unwrap();
    assert!(by(CleanAction::SimctlDeleteUnavailable).checked);
    assert!(!by(CleanAction::BrewCleanup).checked);
    let docker = by(CleanAction::DockerPrune);
    assert!(!docker.checked);
    assert_eq!(docker.path, None);
    assert_eq!(docker.size, None);

    let only_brew = FakeToolLocator::new().with(Tool::Brew, BREW);
    let mut setup = Setup::new();
    setup.tools = only_brew;
    let scan = setup.scan(&fx);
    let actions: Vec<_> = scan.entries().filter_map(|e| e.action).collect();
    assert_eq!(actions, [CleanAction::BrewCleanup]);
}

#[test]
fn docker_size_comes_from_system_df() {
    let fx = Fixture::new();
    let mut setup = Setup::new();
    setup.tools = all_tools();
    setup.runner.respond(
        DOCKER,
        CommandOutput {
            status: 0,
            stdout: "{\"Type\":\"Images\",\"Reclaimable\":\"2GB (50%)\"}\n\
                     {\"Type\":\"Containers\",\"Reclaimable\":\"500MB (100%)\"}\n"
                .into(),
            stderr: String::new(),
        },
    );
    let scan = setup.scan(&fx);
    let docker = scan
        .entries()
        .find(|e| e.action == Some(CleanAction::DockerPrune))
        .unwrap();
    assert_eq!(docker.size, Some(2_500_000_000));
    assert_eq!(
        setup.runner.calls_of(DOCKER),
        [["system", "df", "--format", "{{json .}}"]]
    );
}

#[test]
fn entry_ids_are_stable_across_scans() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.acme.app");
    let setup = Setup::new();
    let ids = |s: &CleanScan| s.entries().map(|e| e.id.clone()).collect::<Vec<_>>();
    let first = ids(&setup.scan(&fx));
    assert_eq!(first, ids(&setup.scan(&fx)));
    assert!(first[0].starts_with("user_caches/library_caches/"));
}

#[test]
fn cancelled_scan_returns_early() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.acme.app");
    let rules = CleanRules::bundled();
    let policy = policy_for(&fx, &rules);
    let setup = Setup::new();
    let scan = CacheScanner::new(&fx.env, &rules, &policy, &setup.processes, &setup.tools).scan(
        &[],
        &setup.options,
        &AtomicBool::new(true),
    );
    assert!(scan.categories.is_empty());
}

#[test]
fn scanning_two_thousand_entries_is_fast() {
    let fx = Fixture::new();
    for i in 0..2_000 {
        write_file(&fx.lib(&format!("Caches/com.vendor.app{i}/data.bin")), 64);
    }
    let setup = Setup::new();
    let start = Instant::now();
    let scan = setup.scan(&fx);
    let elapsed = start.elapsed();
    assert_eq!(category(&scan, "user_caches").unwrap().entries.len(), 2_000);
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
}

// ---- execute ----

struct Run {
    fx: Fixture,
    trash: FakeTrash,
    runner: RecordingRunner,
    tools: FakeToolLocator,
}

impl Run {
    fn new() -> Run {
        let fx = Fixture::new();
        let trash = FakeTrash::new(fx.env.home.join("TrashBin"));
        Run {
            fx,
            trash,
            runner: RecordingRunner::new(),
            tools: all_tools(),
        }
    }

    fn execute(&self, plan: &CleanPlan) -> (sweeper_core::CleanReport, Vec<usize>) {
        let rules = CleanRules::bundled();
        let policy = policy_for(&self.fx, &rules);
        let cleaner = Cleaner::new(
            &self.fx.env,
            &policy,
            &self.trash,
            UndoLog::for_env(&self.fx.env),
            &self.runner,
        )
        .with_tools(&self.tools);
        let mut steps = Vec::new();
        let report = cleaner.execute(plan, &mut |p| steps.push(p.done));
        (report, steps)
    }
}

fn plan_item(path: &Path) -> PlanItem {
    PlanItem {
        path: path.to_path_buf(),
        size: 100,
    }
}

#[test]
fn plan_contains_only_checked_unrefused_entries() {
    let fx = Fixture::new();
    let checked = touch_dir(&fx, "Library/Caches/com.acme.app");
    let apple = touch_dir(&fx, "Library/Caches/com.apple.Maps");
    let junk = touch(&fx, ".Trash/junk.bin");
    let old = touch(&fx, "Downloads/old.zip");
    age_file(&old, 60);
    let mut setup = Setup::new();
    setup.tools = all_tools();
    let policy = PathPolicy::new(&fx.env, &[])
        .with_extra_roots(&cleaner_roots(&fx.env, &CleanRules::bundled()));
    let mut scan = setup.scan_with_policy(&fx, &policy);
    for category in &mut scan.categories {
        for e in &mut category.entries {
            if e.path.as_deref() == Some(junk.as_path()) {
                e.checked = true;
            }
        }
    }
    let plan = CleanPlan::from_scan(&scan);
    assert_eq!(
        plan.trash_items.iter().map(|i| &i.path).collect::<Vec<_>>(),
        [&checked]
    );
    assert_eq!(plan.delete_items, [plan_item(&junk)]);
    assert_eq!(plan.actions, [CleanAction::SimctlDeleteUnavailable]);
    assert_eq!(plan.total_size, 200);
    assert!(apple.exists());

    // A refused entry never reaches the plan even when checked.
    let plain = PathPolicy::new(&fx.env, &[]);
    let mut refused_scan = setup.scan_with_policy(&fx, &plain);
    for category in &mut refused_scan.categories {
        for e in &mut category.entries {
            e.checked = true;
        }
    }
    let plan = CleanPlan::from_scan(&refused_scan);
    assert!(plan.trash_items.iter().all(|i| i.path != old));
    assert!(plan.delete_items.iter().all(|i| i.path != junk));
}

#[test]
fn execute_trashes_with_undo_entries() {
    let run = Run::new();
    let a = touch_dir(&run.fx, "Library/Caches/com.acme.app");
    let b = touch_dir(&run.fx, ".npm/_cacache");
    let plan = CleanPlan {
        trash_items: vec![plan_item(&a), plan_item(&b)],
        total_size: 200,
        ..CleanPlan::default()
    };
    let (report, steps) = run.execute(&plan);

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.removed.len(), 2);
    assert_eq!(report.freed, 200);
    assert_eq!(steps, [1, 2]);
    assert!(!a.exists() && !b.exists());
    assert!(run.trash.trash_dir.join("com.acme.app").exists());
    let undo = UndoLog::for_env(&run.fx.env).entries().unwrap();
    assert_eq!(undo.len(), 2);
    assert!(undo.iter().any(|u| u.original == a));
}

#[test]
fn execute_deletes_trash_items_permanently() {
    let run = Run::new();
    let junk = touch(&run.fx, ".Trash/junk.bin");
    let plan = CleanPlan {
        delete_items: vec![plan_item(&junk)],
        total_size: 100,
        ..CleanPlan::default()
    };
    let (report, _) = run.execute(&plan);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].trash_location, None);
    assert!(!junk.exists());
    assert!(UndoLog::for_env(&run.fx.env).entries().unwrap().is_empty());
}

#[test]
fn permanent_delete_outside_the_trash_is_refused() {
    let run = Run::new();
    let cache = touch_dir(&run.fx, "Library/Caches/com.acme.app");
    let docs = touch(&run.fx, "Documents/thesis.txt");
    let plan = CleanPlan {
        delete_items: vec![plan_item(&cache), plan_item(&docs)],
        ..CleanPlan::default()
    };
    let (report, steps) = run.execute(&plan);
    assert!(report.removed.is_empty());
    assert_eq!(report.failed.len(), 2);
    assert!(cache.exists() && docs.exists());
    assert_eq!(steps, [1, 2]);
}

#[test]
fn refused_paths_are_never_touched() {
    let run = Run::new();
    let docs = touch(&run.fx, "Documents/thesis.txt");
    let keychain = touch(&run.fx, "Library/Keychains/login.keychain-db");
    let downloads_root = run.fx.env.home.join("Downloads");
    std::fs::create_dir_all(&downloads_root).unwrap();
    let outside = outside_dir(&run.fx);
    let escape = run.fx.lib("Caches/escape");
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    let plan = CleanPlan {
        trash_items: vec![
            plan_item(&docs),
            plan_item(&keychain),
            plan_item(&downloads_root),
            plan_item(&escape),
        ],
        ..CleanPlan::default()
    };
    let (report, _) = run.execute(&plan);
    assert!(report.removed.is_empty());
    assert_eq!(report.failed.len(), 4);
    assert!(docs.exists() && keychain.exists() && downloads_root.exists());
    assert!(outside.join("precious.txt").exists());
    assert!(std::fs::symlink_metadata(&escape).is_ok());
}

#[test]
fn actions_run_with_exact_argv() {
    let run = Run::new();
    let plan = CleanPlan {
        actions: vec![
            CleanAction::SimctlDeleteUnavailable,
            CleanAction::BrewCleanup,
            CleanAction::DockerPrune,
        ],
        ..CleanPlan::default()
    };
    let (report, steps) = run.execute(&plan);
    assert_eq!(
        run.runner.calls(),
        [
            vec![XCRUN, "simctl", "delete", "unavailable"],
            vec![BREW, "cleanup", "-s"],
            vec![DOCKER, "system", "prune", "-f"],
        ]
    );
    assert!(report.actions.iter().all(|s| s.ok));
    assert_eq!(steps, [1, 2, 3]);
    assert_eq!(
        CleanAction::BrewCleanup.argv(Path::new(BREW)),
        [BREW, "cleanup", "-s"]
    );
}

#[test]
fn failing_and_missing_actions_are_reported() {
    let mut run = Run::new();
    run.runner.respond(
        BREW,
        CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "\nError: boom\nmore".into(),
        },
    );
    run.tools = FakeToolLocator::new().with(Tool::Brew, BREW);
    let plan = CleanPlan {
        actions: vec![CleanAction::BrewCleanup, CleanAction::DockerPrune],
        ..CleanPlan::default()
    };
    let (report, _) = run.execute(&plan);
    assert_eq!(report.actions.len(), 2);
    assert!(!report.actions[0].ok);
    assert_eq!(report.actions[0].detail, "Error: boom");
    assert!(!report.actions[1].ok);
    assert!(run.runner.calls_of("/usr/local/bin/docker").is_empty());
}

#[test]
fn end_to_end_scan_plan_execute() {
    let run = Run::new();
    let cache = touch_dir(&run.fx, "Library/Caches/com.acme.app");
    let derived = touch_dir(&run.fx, "Library/Developer/Xcode/DerivedData/App-abc");
    let keep = touch(&run.fx, "Documents/keep.txt");
    let rules = CleanRules::bundled();
    let policy = policy_for(&run.fx, &rules);
    let setup = Setup::new();
    let scan = setup.scan_with_policy(&run.fx, &policy);
    let plan = CleanPlan::from_scan(&scan);
    let (report, _) = run.execute(&plan);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!cache.exists() && !derived.exists());
    assert!(keep.exists());
    assert_eq!(report.freed, 200);
}
