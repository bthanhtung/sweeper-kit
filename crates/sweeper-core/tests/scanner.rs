mod common;

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use common::*;
use sweeper_core::testing::{make_app, FakeSpotlight, Fixture};
use sweeper_core::{
    apply_preset, AppCatalog, Category, LeftoverScanner, PathPolicy, Preset, RuleSet,
};

const FOO: &str = "com.foo.app";

#[test]
fn exact_bundle_id_matches_every_location() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    let expected = [
        "Library/Application Support/com.foo.app",
        "Library/Containers/com.foo.app",
        "Library/Group Containers/group.com.foo.app",
        "Library/Application Scripts/com.foo.app",
        "Library/Caches/com.foo.app",
        "Library/HTTPStorages/com.foo.app",
        "Library/HTTPStorages/com.foo.app.binarycookies",
        "Library/WebKit/com.foo.app",
        "Library/Caches/com.apple.nsurlsessiond/Downloads/com.foo.app",
        "Library/Preferences/com.foo.app.plist",
        "Library/Preferences/ByHost/com.foo.app.ABCDEF.plist",
        "Library/Cookies/com.foo.app.binarycookies",
        "Library/Saved Application State/com.foo.app.savedState",
        "Library/Logs/com.foo.app",
        "Library/Logs/DiagnosticReports/Foo-2024-01-01.ips",
        "Library/LaunchAgents/com.foo.app.helper.plist",
    ];
    for e in expected {
        touch_dir(&fx, e);
    }
    let result = scan(&fx, FOO);
    let found = rel(&fx, &leftover_paths(&result));
    for e in expected {
        assert!(
            found.contains(&e.to_string()),
            "missing {e}; found {found:?}"
        );
    }
    assert_eq!(found.len(), expected.len(), "unexpected extras: {found:?}");
    assert_eq!(result.groups[0].category, Category::AppBundle);
    assert_eq!(result.groups[0].items[0].path, fx.apps_dir.join("Foo.app"));
    assert!(result.groups[0].items[0].checked);
    assert!(result.total_size > 0);
}

#[test]
fn bundle_id_boundaries() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    for e in [
        "Library/Preferences/com.foo.app.plist",
        "Library/Preferences/com.foo.app.helper.plist",
        "Library/Preferences/com.foo.application.plist",
        "Library/Containers/com.foo.application",
    ] {
        touch(&fx, e);
    }
    let found = rel(&fx, &leftover_paths(&scan(&fx, FOO)));
    assert_eq!(
        found,
        vec![
            "Library/Preferences/com.foo.app.helper.plist",
            "Library/Preferences/com.foo.app.plist"
        ]
    );
}

#[test]
fn longer_installed_id_claims_its_files() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    fx.install("Foo Helper", "com.foo.app.helper", "1.0");
    touch(&fx, "Library/Preferences/com.foo.app.plist");
    touch(&fx, "Library/Preferences/com.foo.app.helper.plist");
    touch(&fx, "Library/LaunchAgents/com.foo.app.helper.plist");

    let foo = rel(&fx, &leftover_paths(&scan(&fx, FOO)));
    assert_eq!(foo, vec!["Library/Preferences/com.foo.app.plist"]);

    let helper = rel(&fx, &leftover_paths(&scan(&fx, "com.foo.app.helper")));
    assert_eq!(
        helper,
        vec![
            "Library/LaunchAgents/com.foo.app.helper.plist",
            "Library/Preferences/com.foo.app.helper.plist"
        ]
    );
}

#[test]
fn similar_names_do_not_collide() {
    let fx = Fixture::new();
    fx.install("Notion", "notion.id", "1.0");
    fx.install("Notion Calendar", "com.cron.electron", "1.0");
    for e in [
        "Library/Application Support/Notion",
        "Library/Caches/Notion",
        "Library/Logs/Notion",
        "Library/Logs/DiagnosticReports/Notion-2024.ips",
        "Library/Preferences/notion.id.plist",
        "Library/Application Support/Notion Calendar",
        "Library/Caches/Notion Calendar",
        "Library/Logs/Notion Calendar",
        "Library/Logs/DiagnosticReports/Notion Calendar-2024.ips",
        "Library/Preferences/com.cron.electron.plist",
    ] {
        touch_dir(&fx, e);
    }
    let notion = rel(&fx, &leftover_paths(&scan(&fx, "notion.id")));
    assert_eq!(notion.len(), 5, "{notion:?}");
    assert!(notion
        .iter()
        .all(|p| !p.contains("Calendar") && !p.contains("cron")));

    let cal = rel(&fx, &leftover_paths(&scan(&fx, "com.cron.electron")));
    assert_eq!(cal.len(), 5, "{cal:?}");
    assert!(cal
        .iter()
        .all(|p| p.contains("Calendar") || p.contains("cron")));
}

#[test]
fn same_name_different_ids_is_ambiguous_and_skipped() {
    let fx = Fixture::new();
    fx.install("Twin", "com.a.twin", "1.0");
    let other = fx.tmp.path().join("Applications").join("Sub");
    std::fs::create_dir_all(&other).unwrap();
    make_app(&other, "Twin", "com.b.twin", "1.0");
    let mut env = fx.env.clone();
    env.app_dirs.push(other);
    touch(&fx, "Library/Caches/Twin");
    touch(&fx, "Library/Caches/com.a.twin");

    let apps = AppCatalog::scan(&env, &FakeSpotlight::default());
    let app = app_by_id(&apps, "com.a.twin");
    let policy = PathPolicy::new(&env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let result = LeftoverScanner::new(&env, &rules, &policy, &spot).scan(
        &app,
        &apps,
        &AtomicBool::new(false),
    );
    assert_eq!(
        rel(&fx, &leftover_paths(&result)),
        vec!["Library/Caches/com.a.twin"]
    );
}

#[test]
fn no_bare_substring_matches() {
    let fx = Fixture::new();
    fx.install("Chrome", "com.google.Chrome", "1.0");
    for e in [
        "Library/Application Support/MyChromeThing",
        "Library/Caches/MyChromeThing",
        "Library/Caches/chromeish",
        "Library/Logs/MyChromeThing.log",
        "Library/Preferences/com.google.ChromeCanary.plist",
        "Library/Containers/com.google.Chrome2",
    ] {
        touch(&fx, e);
    }
    assert!(leftover_paths(&scan(&fx, "com.google.Chrome")).is_empty());
}

#[test]
fn name_match_is_case_insensitive_for_folders() {
    let fx = Fixture::new();
    fx.install("Chrome", "com.google.Chrome", "1.0");
    touch_dir(&fx, "Library/Application Support/chrome");
    let found = rel(&fx, &leftover_paths(&scan(&fx, "com.google.Chrome")));
    assert_eq!(found, vec!["Library/Application Support/chrome"]);
}

#[test]
fn other_copies_exclude_selected_and_are_unchecked() {
    let fx = Fixture::new();
    let app = fx.install("Foo", FOO, "1.0");
    let sub = fx.apps_dir.join("Sub");
    std::fs::create_dir_all(&sub).unwrap();
    let copy = make_app(&sub, "Foo", FOO, "0.9");
    let mut spot = FakeSpotlight::default();
    spot.copies
        .insert(FOO.into(), vec![app.clone(), copy.clone()]);
    let result = scan_with(&fx, FOO, &[], &spot);
    assert_eq!(result.other_copies.len(), 1);
    assert_eq!(result.other_copies[0].path, copy);
    assert!(!result.other_copies[0].checked);
}

#[test]
fn presets_select_expected_categories() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    touch(&fx, "Library/Caches/com.foo.app");
    touch(&fx, "Library/Preferences/com.foo.app.plist");
    let outside = outside_dir(&fx);
    std::os::unix::fs::symlink(&outside, fx.lib("Application Support").join(FOO)).unwrap();
    let mut result = scan(&fx, FOO);

    let checked = |r: &sweeper_core::ScanResult| -> Vec<Category> {
        r.items()
            .filter(|i| i.checked)
            .map(|i| i.category)
            .collect()
    };
    apply_preset(&mut result, Preset::AppOnly);
    assert_eq!(checked(&result), vec![Category::AppBundle]);
    apply_preset(&mut result, Preset::AppAndCaches);
    assert_eq!(
        checked(&result),
        vec![Category::AppBundle, Category::Caches]
    );
    apply_preset(&mut result, Preset::Everything);
    assert_eq!(
        checked(&result).len(),
        3,
        "refused symlink item must stay unchecked"
    );
    assert!(result.items().any(|i| !i.checked && i.refused.is_some()));
    assert!(result.other_copies.iter().all(|i| !i.checked));
    assert!(result.selected_size() > 0);
}

#[test]
fn team_wide_group_container_is_never_pre_checked() {
    const WORD: &str = "com.microsoft.Word";
    const TEAM: &str = "UBF8T346G9";
    let fx = Fixture::new();
    fx.install("Word", WORD, "1.0");
    for e in [
        "Library/Group Containers/UBF8T346G9.Office",
        "Library/Group Containers/UBF8T346G9.com.microsoft.Word",
        "Library/Group Containers/group.com.microsoft.Word",
    ] {
        touch_dir(&fx, e);
    }
    let apps = installed(&fx);
    let mut app = app_by_id(&apps, WORD);
    app.team_id = Some(TEAM.into());
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let mut result = LeftoverScanner::new(&fx.env, &rules, &policy, &spot).scan(
        &app,
        &apps,
        &AtomicBool::new(false),
    );

    let find = |r: &sweeper_core::ScanResult, name: &str| {
        r.items()
            .find(|i| i.path.ends_with(name))
            .unwrap_or_else(|| panic!("{name} not found"))
            .clone()
    };
    let exact = find(&result, "group.com.microsoft.Word");
    assert!(exact.checked && exact.reason.contains("bundle identifier"));
    let exact_team = find(&result, "UBF8T346G9.com.microsoft.Word");
    assert!(exact_team.checked && exact_team.reason.contains("bundle identifier"));
    let shared = find(&result, "UBF8T346G9.Office");
    assert!(!shared.checked && shared.shared);
    assert_eq!(
        shared.reason,
        "team container — may be shared with other apps from this developer"
    );

    apply_preset(&mut result, Preset::Everything);
    let shared = find(&result, "UBF8T346G9.Office");
    assert!(!shared.checked, "preset must not check a team container");
    assert!(find(&result, "group.com.microsoft.Word").checked);
}

#[test]
fn scan_json_round_trips() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    touch(&fx, "Library/Caches/com.foo.app");
    let result = scan(&fx, FOO);
    let json = serde_json::to_string(&result).unwrap();
    let back: sweeper_core::ScanResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, result);
}

#[test]
fn cancelled_scan_returns_without_panicking() {
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    touch(&fx, "Library/Caches/com.foo.app");
    let apps = installed(&fx);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let cancel = AtomicBool::new(true);
    let result =
        LeftoverScanner::new(&fx.env, &rules, &policy, &spot).scan(&apps[0], &apps, &cancel);
    assert_eq!(result.groups.len(), 1);
}

#[cfg(unix)]
#[test]
fn unreadable_location_is_reported_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.install("Foo", FOO, "1.0");
    let dir = fx.lib("WebKit");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = scan(&fx, FOO);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Running as root can read anything; only assert when the read really failed.
    if !result.inaccessible.is_empty() {
        assert_eq!(result.inaccessible[0].0, dir);
    }
}

#[test]
fn scanning_2000_library_entries_is_fast() {
    let fx = Fixture::new();
    for i in 0..30 {
        fx.install(
            &format!("App{i:02}"),
            &format!("com.vendor.app{i:02}"),
            "1.0",
        );
    }
    for i in 0..800 {
        touch(&fx, &format!("Library/Caches/com.other.thing{i}/c.bin"));
    }
    for i in 0..600 {
        touch(&fx, &format!("Library/Application Support/Thing {i}/s.bin"));
    }
    for i in 0..600 {
        touch(&fx, &format!("Library/Preferences/com.other.pref{i}.plist"));
    }
    touch(&fx, "Library/Caches/com.vendor.app07/hit.bin");

    let start = Instant::now();
    let result = scan(&fx, "com.vendor.app07");
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(3), "scan took {elapsed:?}");
    assert_eq!(leftover_paths(&result).len(), 1);
}
