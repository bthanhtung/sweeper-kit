mod common;

use std::sync::atomic::AtomicBool;
use std::time::{Duration, UNIX_EPOCH};

use common::*;
use sweeper_core::testing::{write_file, FakeSpotlight, Fixture, RecordingRunner};
use sweeper_core::{compute_sizes, team_id, AppCatalog, AppInfo, AppSource, CommandOutput};

#[test]
fn reads_name_version_and_bundle_id() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "2.3.4");
    let apps = installed(&fx);
    assert_eq!(apps.len(), 1);
    let app = &apps[0];
    assert_eq!(app.name, "Foo");
    assert_eq!(app.version.as_deref(), Some("2.3.4"));
    assert_eq!(app.bundle_id, "com.foo.app");
    assert_eq!(app.path, fx.apps_dir.join("Foo.app"));
    assert_eq!(app.source, AppSource::Other);
    assert!(!app.is_apple && app.team_id.is_none() && app.size.is_none());
}

#[test]
fn display_name_wins_and_file_stem_is_the_fallback() {
    let fx = Fixture::new();
    let path = fx.install("Fancy", "com.fancy", "1.0");
    let mut info = plist::Dictionary::new();
    info.insert("CFBundleIdentifier".into(), "com.fancy".into());
    info.insert("CFBundleName".into(), "Plain".into());
    info.insert("CFBundleDisplayName".into(), "Fancy Display".into());
    plist::Value::Dictionary(info)
        .to_file_xml(path.join("Contents/Info.plist"))
        .unwrap();
    assert_eq!(installed(&fx)[0].name, "Fancy Display");

    let mut info = plist::Dictionary::new();
    info.insert("CFBundleIdentifier".into(), "com.fancy".into());
    plist::Value::Dictionary(info)
        .to_file_xml(path.join("Contents/Info.plist"))
        .unwrap();
    assert_eq!(installed(&fx)[0].name, "Fancy");
}

#[test]
fn detects_app_store_receipt() {
    let fx = Fixture::new();
    let app = fx.install("Store", "com.store.app", "1.0");
    write_file(&app.join("Contents/_MASReceipt/receipt"), 8);
    assert_eq!(installed(&fx)[0].source, AppSource::AppStore);
}

#[test]
fn detects_homebrew_caskroom() {
    let fx = Fixture::new();
    fx.install("Brewed", "com.brewed", "1.0");
    fx.install("Manual", "com.manual", "1.0");
    std::fs::create_dir_all(fx.env.caskroom_dirs[0].join("brewed-cask/1.0/Brewed.app")).unwrap();
    let apps = installed(&fx);
    assert_eq!(
        app_by_id(&apps, "com.brewed").source,
        AppSource::Homebrew {
            cask: "brewed-cask".into()
        }
    );
    assert_eq!(app_by_id(&apps, "com.manual").source, AppSource::Other);
}

#[test]
fn unreadable_and_non_app_entries_are_skipped() {
    let fx = Fixture::new();
    fx.install("Good", "com.good", "1.0");
    std::fs::create_dir_all(fx.apps_dir.join("NoPlist.app/Contents")).unwrap();
    write_file(&fx.apps_dir.join("Garbage.app/Contents/Info.plist"), 20);
    write_file(&fx.apps_dir.join("readme.txt"), 5);
    std::fs::create_dir_all(fx.apps_dir.join("Folder")).unwrap();
    let apps = installed(&fx);
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].bundle_id, "com.good");
}

#[test]
fn duplicate_dirs_and_symlinked_apps_are_deduplicated() {
    let mut fx = Fixture::new();
    let app = fx.install("Foo", "com.foo.app", "1.0");
    let second = fx.tmp.path().join("Apps2");
    std::fs::create_dir_all(&second).unwrap();
    std::os::unix::fs::symlink(&app, second.join("Foo.app")).unwrap();
    fx.env.app_dirs.push(fx.apps_dir.clone());
    fx.env.app_dirs.push(second);
    assert_eq!(
        AppCatalog::scan(&fx.env, &FakeSpotlight::default()).len(),
        1
    );
}

#[test]
fn missing_app_dirs_are_ignored() {
    let mut fx = Fixture::new();
    fx.env.app_dirs.push(fx.tmp.path().join("does-not-exist"));
    assert!(AppCatalog::scan(&fx.env, &FakeSpotlight::default()).is_empty());
}

#[test]
fn apple_apps_are_flagged() {
    let fx = Fixture::new();
    fx.install("Calc", "com.apple.calculator", "1.0");
    assert!(installed(&fx)[0].is_apple);
}

#[test]
fn last_opened_comes_from_spotlight_and_json_round_trips() {
    let fx = Fixture::new();
    let path = fx.install("Foo", "com.foo.app", "1.0");
    let mut spot = FakeSpotlight::default();
    let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    spot.last_used.insert(path, when);
    let apps = AppCatalog::scan(&fx.env, &spot);
    assert_eq!(apps[0].last_opened, Some(when));

    let json = serde_json::to_string(&apps[0]).unwrap();
    assert!(json.contains("1700000000"));
    let back: AppInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back, apps[0]);
}

#[test]
fn sizes_are_computed_in_parallel_and_respect_cancel() {
    let fx = Fixture::new();
    let app = fx.install("Foo", "com.foo.app", "1.0");
    write_file(&app.join("Contents/Resources/big.bin"), 5000);
    let mut apps = installed(&fx);
    compute_sizes(&mut apps, &AtomicBool::new(false));
    assert!(apps[0].size.unwrap() >= 5000);

    let mut apps = installed(&fx);
    compute_sizes(&mut apps, &AtomicBool::new(true));
    assert_eq!(apps[0].size, None);
}

#[test]
fn team_id_is_parsed_from_codesign_stderr() {
    let runner = RecordingRunner::new();
    runner.respond(
        "codesign",
        CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: "Identifier=com.foo.app\nTeamIdentifier=ABCDE12345\nSealed Resources=none\n"
                .into(),
        },
    );
    let path = std::path::Path::new("/Applications/Foo.app");
    assert_eq!(team_id(path, &runner).as_deref(), Some("ABCDE12345"));
    assert_eq!(
        runner.calls()[0],
        vec!["codesign", "-dv", "--verbose=2", "/Applications/Foo.app"]
    );

    runner.respond(
        "codesign",
        CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: "TeamIdentifier=not set\n".into(),
        },
    );
    assert_eq!(team_id(path, &runner), None);
    runner.fail_to_start("codesign");
    assert_eq!(team_id(path, &runner), None);
}

#[test]
fn group_container_matching_uses_the_team_id() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    touch(&fx, "Library/Group Containers/ABCDE12345.shared/x");
    let mut apps = installed(&fx);
    assert!(leftover_paths(&scan(&fx, "com.foo.app")).is_empty());

    apps[0].team_id = Some("ABCDE12345".into());
    let policy = sweeper_core::PathPolicy::new(&fx.env, &[]);
    let rules = sweeper_core::RuleSet::bundled();
    let spot = FakeSpotlight::default();
    let result = sweeper_core::LeftoverScanner::new(&fx.env, &rules, &policy, &spot).scan(
        &apps[0],
        &apps,
        &AtomicBool::new(false),
    );
    assert_eq!(
        rel(&fx, &leftover_paths(&result)),
        vec!["Library/Group Containers/ABCDE12345.shared"]
    );
}
