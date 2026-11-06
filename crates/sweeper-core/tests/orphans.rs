mod common;

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use common::*;
use sweeper_core::testing::{FakeProcesses, FakeSpotlight, FakeTrash, Fixture};
use sweeper_core::{
    Confidence, OrphanFinder, OrphanRules, OrphanScan, PathPolicy, RemovalMode, RemovedApps,
    Remover, RunningApp, UndoLog,
};

fn find_with(
    fx: &Fixture,
    spotlight: &FakeSpotlight,
    processes: &FakeProcesses,
    history: &RemovedApps,
) -> OrphanScan {
    let apps = installed(fx);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = OrphanRules::bundled();
    OrphanFinder::new(&fx.env, &rules, &policy, spotlight, processes, history)
        .scan(&apps, &AtomicBool::new(false))
}

fn find(fx: &Fixture) -> OrphanScan {
    find_with(
        fx,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &RemovedApps::for_env(&fx.env),
    )
}

fn keys(scan: &OrphanScan) -> Vec<String> {
    let mut k: Vec<String> = scan.groups.iter().map(|g| g.key.clone()).collect();
    k.sort();
    k
}

fn group<'a>(scan: &'a OrphanScan, key: &str) -> &'a sweeper_core::OrphanGroup {
    scan.groups
        .iter()
        .find(|g| g.key == key)
        .unwrap_or_else(|| panic!("no group {key}: {:?}", keys(scan)))
}

/// Records `bundle_id` as uninstalled by Sweeper Kit (the app is not on disk).
fn record_removed(fx: &Fixture, name: &str, bundle_id: &str, history: &RemovedApps) {
    fx.install(name, bundle_id, "1.0");
    let app = app_by_id(&installed(fx), bundle_id);
    std::fs::remove_dir_all(&app.path).unwrap();
    history.record(&app).unwrap();
}

#[test]
fn data_of_installed_apps_is_not_orphaned() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    touch_dir(&fx, "Library/Caches/com.foo.app");
    touch(&fx, "Library/Preferences/com.foo.app.helper.plist");
    touch_dir(&fx, "Library/Containers/com.foo.app.ShareExtension");
    touch_dir(&fx, "Library/Caches/com.foo");
    touch_dir(&fx, "Library/Caches/com.bar.gone");
    let scan = find(&fx);
    assert_eq!(keys(&scan), vec!["com.bar.gone"]);
    let g = group(&scan, "com.bar.gone");
    assert_eq!(g.confidence, Confidence::Medium);
    assert!(g.items.iter().all(|i| !i.checked));
}

#[test]
fn boundary_is_a_dot_not_a_text_prefix() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    touch_dir(&fx, "Library/Caches/com.foo.application");
    touch_dir(&fx, "Library/Caches/com.foo.app-helper");
    let scan = find(&fx);
    assert_eq!(
        keys(&scan),
        vec!["com.foo.app-helper", "com.foo.application"]
    );
}

#[test]
fn history_turns_medium_into_checked_high() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.bar.gone");
    touch(&fx, "Library/Preferences/com.bar.gone.plist");
    let history = RemovedApps::for_env(&fx.env);
    let before = find_with(
        &fx,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    );
    assert_eq!(
        group(&before, "com.bar.gone").confidence,
        Confidence::Medium
    );

    record_removed(&fx, "Gone", "com.bar.gone", &history);

    let after = find_with(
        &fx,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    );
    let g = group(&after, "com.bar.gone");
    assert_eq!(g.confidence, Confidence::High);
    assert_eq!(g.display_name, "Gone");
    assert_eq!(g.items.len(), 2);
    assert!(g.items.iter().all(|i| i.checked));
    assert_eq!(after.plan(None).items.len(), 2);
}

#[test]
fn plain_names_are_compared_with_installed_apps() {
    let fx = Fixture::new();
    fx.install("Notion", "notion.id", "1.0");
    touch_dir(&fx, "Library/Application Support/Notion");
    touch_dir(&fx, "Library/Application Support/Notion Calendar");
    touch_dir(&fx, "Library/Caches/notion");
    let scan = find(&fx);
    assert_eq!(keys(&scan), vec!["Notion Calendar"]);
    let g = group(&scan, "Notion Calendar");
    assert_eq!(g.confidence, Confidence::Low);
    assert!(g.items.iter().all(|i| !i.checked));
    assert!(scan.plan(None).items.is_empty());
    assert_eq!(scan.selected_size(), 0);
}

#[test]
fn plain_names_only_count_in_support_and_caches() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/WebKit/Whatever");
    touch_dir(&fx, "Library/Containers/Whatever");
    touch_dir(&fx, "Library/Application Support/What Ever.cache");
    touch_dir(&fx, "Library/Caches/Whatever");
    assert_eq!(keys(&find(&fx)), vec!["Whatever"]);
}

#[test]
fn system_and_vendor_data_is_never_listed() {
    let fx = Fixture::new();
    for p in [
        "Library/Caches/com.apple.Safari",
        "Library/Application Support/com.apple.sharedfilelist",
        "Library/Application Support/MobileSync",
        "Library/Application Support/mobilesync2",
        "Library/Application Support/AddressBook",
        "Library/Application Support/Google",
        "Library/Application Support/discord-canary",
        "Library/Application Support/Sweeper Kit",
        "Library/Application Support/.hidden",
        "Library/Caches/dev.sweeperkit.app",
        "Library/Containers/com.apple.Notes",
        "Library/HTTPStorages/com.apple.Safari",
    ] {
        touch_dir(&fx, p);
    }
    touch(&fx, "Library/Preferences/com.apple.finder.plist");
    touch(&fx, "Library/Preferences/.GlobalPreferences.plist");
    touch(&fx, "Library/LaunchAgents/com.apple.something.plist");
    // Only the lookalike that is not in any exclusion remains.
    assert_eq!(keys(&find(&fx)), vec!["mobilesync2"]);
}

#[test]
fn apple_containers_are_skipped_by_metadata() {
    let fx = Fixture::new();
    let meta = |dir: &str, id: &str| {
        let p = fx.lib("Containers").join(dir);
        std::fs::create_dir_all(&p).unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("MCMMetadataIdentifier".into(), id.into());
        plist::Value::Dictionary(d)
            .to_file_xml(p.join(".com.apple.containermanagerd.metadata.plist"))
            .unwrap();
    };
    meta("com.third.party", "com.third.party");
    meta("com.disguised.thing", "com.apple.hidden");
    let broken = fx.lib("Containers/com.broken.meta");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join(".com.apple.containermanagerd.metadata.plist"),
        b"not a plist",
    )
    .unwrap();
    touch_dir(&fx, "Library/Containers/com.nometa.app");
    assert_eq!(keys(&find(&fx)), vec!["com.nometa.app", "com.third.party"]);
}

#[test]
fn running_process_or_spotlight_copy_means_not_orphan() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.run.ning");
    touch_dir(&fx, "Library/Caches/com.spot.light");
    touch_dir(&fx, "Library/Caches/com.really.gone");
    let processes = FakeProcesses::new();
    processes.add(
        "com.run.ning",
        RunningApp {
            pid: 7,
            path: "/Volumes/X/Run.app".into(),
            name: "Run".into(),
        },
        false,
    );
    let mut spotlight = FakeSpotlight::default();
    spotlight
        .copies
        .insert("com.spot.light".into(), vec!["/Volumes/Y/Spot.app".into()]);
    let history = RemovedApps::for_env(&fx.env);
    let scan = find_with(&fx, &spotlight, &processes, &history);
    assert_eq!(keys(&scan), vec!["com.really.gone"]);
}

#[test]
fn symlink_pointing_outside_home_is_refused_and_unchecked() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    symlink(&outside, fx.lib("Caches").join("com.bar.gone")).unwrap();
    let history = RemovedApps::for_env(&fx.env);
    record_removed(&fx, "Gone", "com.bar.gone", &history);
    let scan = find_with(
        &fx,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    );
    let item = &group(&scan, "com.bar.gone").items[0];
    assert!(item.refused.is_some() && !item.checked);
    assert!(scan.plan(None).items.is_empty());
    assert!(scan
        .plan(Some(std::slice::from_ref(&item.path)))
        .items
        .is_empty());
    assert!(outside.join("precious.txt").exists());
}

#[test]
fn items_of_one_key_are_grouped_across_locations() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.bar.gone");
    touch(&fx, "Library/Preferences/com.bar.gone.plist");
    touch_dir(
        &fx,
        "Library/Saved Application State/com.bar.gone.savedState",
    );
    touch_dir(&fx, "Library/HTTPStorages/com.bar.gone");
    touch(&fx, "Library/HTTPStorages/com.bar.gone.binarycookies");
    touch(&fx, "Library/LaunchAgents/com.bar.gone.plist");
    touch_dir(&fx, "Library/WebKit/com.bar.gone");
    touch_dir(&fx, "Library/Application Support/com.bar.gone");
    touch(&fx, "Library/Caches/com.small.one");
    let scan = find(&fx);
    assert_eq!(scan.groups.len(), 2);
    let g = group(&scan, "com.bar.gone");
    assert_eq!(g.items.len(), 8);
    assert_eq!(g.size, 800);
    assert_eq!(scan.total_size, 900);
    // Larger group first within the same confidence.
    assert_eq!(scan.groups[0].key, "com.bar.gone");
    let paths: Vec<PathBuf> = g.items.iter().map(|i| i.path.clone()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
}

#[test]
fn groups_sort_by_confidence_then_size() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Application Support/Lowly");
    touch(&fx, "Library/Caches/com.med.ium");
    touch_dir(&fx, "Library/Caches/com.hi.gh");
    let history = RemovedApps::for_env(&fx.env);
    record_removed(&fx, "Hi", "com.hi.gh", &history);
    let scan = find_with(
        &fx,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    );
    let order: Vec<_> = scan.groups.iter().map(|g| g.confidence).collect();
    assert_eq!(
        order,
        vec![Confidence::High, Confidence::Medium, Confidence::Low]
    );
}

#[test]
fn plan_with_selection_ignores_unknown_and_refused_paths() {
    let fx = Fixture::new();
    let a = touch_dir(&fx, "Library/Caches/com.bar.gone");
    let b = touch_dir(&fx, "Library/Caches/com.baz.gone");
    let outside = outside_dir(&fx);
    let linked = fx.lib("Caches").join("com.link.gone");
    symlink(&outside, &linked).unwrap();
    let scan = find(&fx);
    let stranger = fx.lib("Caches").join("not-in-scan");
    std::fs::create_dir_all(&stranger).unwrap();

    let plan = scan.plan(Some(&[
        a.clone(),
        linked.clone(),
        stranger.clone(),
        a.clone(),
    ]));
    let paths: Vec<_> = plan.items.iter().map(|i| i.path.clone()).collect();
    assert_eq!(paths, vec![a.clone()]);
    assert_eq!(plan.total_size, 100);
    assert!(scan.plan(None).items.is_empty(), "medium is never checked");

    // The existing remover executes the plan (Trash + undo entry).
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let report = remover.execute(&plan, RemovalMode::Trash, &mut |_| {});
    assert_eq!(report.removed.len(), 1);
    assert!(!a.exists() && b.exists() && stranger.exists());
    assert_eq!(remover.undo_log().entries().unwrap().len(), 1);
}

#[test]
fn cancelled_scan_returns_early() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/com.bar.gone");
    let apps = installed(&fx);
    let policy = PathPolicy::new(&fx.env, &[]);
    let rules = OrphanRules::bundled();
    let history = RemovedApps::for_env(&fx.env);
    let scan = OrphanFinder::new(
        &fx.env,
        &rules,
        &policy,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    )
    .scan(&apps, &AtomicBool::new(true));
    assert!(scan.groups.is_empty());
}

#[test]
fn two_thousand_entries_scan_quickly() {
    let fx = Fixture::new();
    for i in 0..1000 {
        touch(&fx, &format!("Library/Caches/com.vendor{i}.app/c.bin"));
        touch(&fx, &format!("Library/Preferences/com.vendor{i}.app.plist"));
    }
    let started = Instant::now();
    let scan = find(&fx);
    let elapsed = started.elapsed();
    assert_eq!(scan.groups.len(), 1000);
    assert!(elapsed.as_secs_f64() < 3.0, "took {elapsed:?}");
}

#[test]
fn rules_validation_and_merge() {
    let bundled = OrphanRules::bundled();
    assert_eq!(bundled.locations.len(), 8);
    let json = |base: &str, extra: &str| {
        format!(
            r#"{{"version":1,"locations":[{{"id":"x","base":"{base}","suffixes":[""]}}]{extra}}}"#
        )
    };
    assert!(OrphanRules::from_json(&json("~/Library/Foo", "")).is_ok());
    assert!(OrphanRules::from_json(&json("/etc", "")).is_err());
    assert!(OrphanRules::from_json(&json("~/Library/../Documents", "")).is_err());
    assert!(OrphanRules::from_json(&json("~/Library/", "")).is_err());
    assert!(OrphanRules::from_json(&json("~/Library/Foo", r#","exclude_names":[""]"#)).is_err());
    assert!(OrphanRules::from_json(r#"{"version":2,"locations":[]}"#).is_err());

    let user = OrphanRules::from_json(&json(
        "~/Library/Foo",
        r#","exclude_names":["Keep Me","google"],"exclude_prefixes":["acme."]"#,
    ))
    .unwrap();
    let merged = bundled.merge(user);
    assert_eq!(merged.locations.len(), 9);
    assert!(merged.exclude_names.iter().any(|n| n == "Keep Me"));
    let google = merged
        .exclude_names
        .iter()
        .filter(|n| n.eq_ignore_ascii_case("google"))
        .count();
    assert_eq!(google, 1);
    assert!(merged.exclude_prefixes.iter().any(|p| p == "acme."));
}

#[test]
fn user_exclusions_are_respected() {
    let fx = Fixture::new();
    touch_dir(&fx, "Library/Caches/acme.tool");
    touch_dir(&fx, "Library/Caches/com.bar.gone");
    let user =
        OrphanRules::from_json(r#"{"version":1,"locations":[],"exclude_prefixes":["ACME."]}"#)
            .unwrap();
    let rules = OrphanRules::bundled().merge(user);
    let apps = installed(&fx);
    let policy = PathPolicy::new(&fx.env, &[]);
    let history = RemovedApps::for_env(&fx.env);
    let scan = OrphanFinder::new(
        &fx.env,
        &rules,
        &policy,
        &FakeSpotlight::default(),
        &FakeProcesses::new(),
        &history,
    )
    .scan(&apps, &AtomicBool::new(false));
    assert_eq!(keys(&scan), vec!["com.bar.gone"]);
}

#[test]
fn history_roundtrip_replace_and_forget() {
    let fx = Fixture::new();
    fx.install("Gone", "com.bar.gone", "1.0");
    let app = app_by_id(&installed(&fx), "com.bar.gone");
    let history = RemovedApps::for_env(&fx.env);
    assert!(history.entries().unwrap().is_empty());
    history.record(&app).unwrap();
    history.record(&app).unwrap();
    assert_eq!(history.entries().unwrap().len(), 1);
    assert!(history.contains("com.bar.gone"));
    history.forget("com.bar.gone").unwrap();
    assert!(!history.contains("com.bar.gone"));
    history.forget("com.bar.gone").unwrap();
    assert!(!fx.env.data_dir.join("removed-apps.json.tmp").exists());
}
