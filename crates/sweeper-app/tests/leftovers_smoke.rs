//! End-to-end tests of the leftovers and history command layer in a fake home
//! with test doubles: nothing outside the temporary directory is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sweeper_app::commands;
use sweeper_app::state::{AppContext, Settings};
use sweeper_core::testing::{
    write_file, FakeProcesses, FakeSpotlight, FakeTrash, Fixture, RecordingRunner,
};
use sweeper_core::{Confidence, OrphanScan, RuleSet};

struct Harness {
    fx: Fixture,
    ctx: AppContext,
}

struct Files {
    gone_cache: PathBuf,
    gone_prefs: PathBuf,
    notion: PathBuf,
    foo_cache: PathBuf,
    protected: PathBuf,
}

fn harness() -> (Harness, Files) {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    let files = Files {
        gone_cache: fx.lib("Caches/com.bar.gone"),
        gone_prefs: fx.lib("Preferences/com.bar.gone.plist"),
        notion: fx.lib("Application Support/Notion Calendar"),
        foo_cache: fx.lib("Caches/com.foo.app"),
        protected: fx.lib("Caches/com.baz.kept"),
    };
    write_file(&files.gone_cache.join("blob.bin"), 3000);
    write_file(&files.gone_prefs, 200);
    write_file(&files.notion.join("data.db"), 1500);
    write_file(&files.foo_cache.join("blob.bin"), 900);
    write_file(&files.protected.join("blob.bin"), 700);

    fs::create_dir_all(&fx.env.data_dir).expect("data dir");
    fs::write(
        fx.env.data_dir.join("removed-apps.json"),
        r#"[{"bundle_id":"com.bar.gone","name":"Bar","removed_at":1700000000,"app_path":"/Applications/Bar.app"}]"#,
    )
    .expect("history record");

    let mut ctx = AppContext::new(
        fx.env.clone(),
        RuleSet::bundled(),
        Box::new(RecordingRunner::new()),
        Box::new(FakeSpotlight::default()),
        Box::new(FakeProcesses::new()),
        Box::new(FakeTrash::new(fx.trash_dir.clone())),
    );
    let settings = Settings {
        protected: vec![files.protected.display().to_string()],
        ..Settings::default()
    };
    commands::save_settings(&mut ctx, &settings).expect("save settings");
    (Harness { fx, ctx }, files)
}

fn scan(h: &Harness) -> OrphanScan {
    commands::list_apps(&h.ctx);
    commands::scan_leftovers(&h.ctx, &AtomicBool::new(false)).expect("scan")
}

fn group<'a>(scan: &'a OrphanScan, key: &str) -> Option<&'a sweeper_core::OrphanGroup> {
    scan.groups.iter().find(|g| g.key == key)
}

fn noop(_: sweeper_core::Progress) {}

#[test]
fn scan_ranks_groups_and_checks_only_high_confidence() {
    let (h, files) = harness();
    let scan = scan(&h);

    let gone = group(&scan, "com.bar.gone").expect("history app is listed");
    assert_eq!(gone.confidence, Confidence::High);
    assert_eq!(gone.display_name, "Bar");
    assert_eq!(gone.items.len(), 2);
    assert!(gone.items.iter().all(|i| i.checked));

    let notion = group(&scan, "Notion Calendar").expect("name match is listed");
    assert_eq!(notion.confidence, Confidence::Low);
    assert!(notion.items.iter().all(|i| !i.checked));

    assert!(
        group(&scan, "com.foo.app").is_none(),
        "installed app is owned"
    );
    let kept = group(&scan, "com.baz.kept").expect("protected item is listed");
    assert!(kept.items.iter().all(|i| i.refused.is_some() && !i.checked));
    assert!(files.foo_cache.exists());

    let confidences: Vec<_> = scan.groups.iter().map(|g| g.confidence).collect();
    let mut sorted = confidences.clone();
    sorted.sort();
    assert_eq!(confidences, sorted, "high confidence first");
}

#[test]
fn plan_ignores_unknown_and_refused_paths() {
    let (h, files) = harness();
    let scan = scan(&h);
    let selected = vec![
        files.gone_cache.clone(),
        files.protected.clone(),
        h.fx.env.home.join("Documents/precious.txt"),
    ];
    let plan = commands::plan_leftovers(&scan, &selected).expect("plan");
    let paths: Vec<_> = plan.items.iter().map(|i| i.path.clone()).collect();
    assert_eq!(paths, vec![files.gone_cache.clone()]);
    assert_eq!(plan.total_size, plan.items[0].size);

    let nothing = vec![files.protected.clone(), PathBuf::from("/etc/hosts")];
    assert!(commands::plan_leftovers(&scan, &nothing).is_err());
    assert!(commands::plan_leftovers(&scan, &[]).is_err());
}

#[test]
fn remove_moves_to_trash_and_history_restores_and_forgets() {
    let (h, files) = harness();
    let scan = scan(&h);
    let selected = vec![files.gone_cache.clone(), files.gone_prefs.clone()];
    let mut progress = Vec::new();
    let report = commands::remove_leftovers(&h.ctx, &scan, &selected, &mut |p| progress.push(p))
        .expect("remove");

    assert_eq!(report.removed.len(), 2);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(progress.len(), 2);
    assert!(!files.gone_cache.exists() && !files.gone_prefs.exists());
    assert!(files.notion.exists(), "unselected items stay");
    for removed in &report.removed {
        let location = removed.trash_location.as_ref().expect("in trash");
        assert!(location.starts_with(&h.fx.trash_dir) && location.exists());
    }

    // History shows the batch with every item restorable.
    let batches = commands::history(&h.ctx).expect("history");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].entries.len(), 2);
    assert!(batches[0].entries.iter().all(|e| e.in_trash));

    // Forgetting an item that is still in the Trash is refused.
    let ids: Vec<String> = batches[0]
        .entries
        .iter()
        .map(|e| e.entry.id.clone())
        .collect();
    assert!(commands::forget_history(&h.ctx, &ids[..1]).is_err());
    assert_eq!(commands::history(&h.ctx).unwrap()[0].entries.len(), 2);

    // Restore one entry; an unknown id fails on its own.
    let first = &batches[0].entries[0].entry;
    let outcomes = commands::restore(&h.ctx, &[first.id.clone(), "nope".into()]);
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].ok, "{}", outcomes[0].path_or_error);
    assert_eq!(Path::new(&outcomes[0].path_or_error), first.original);
    assert!(!outcomes[1].ok);
    assert!(first.original.exists());

    // Delete the other item from the Trash: it can be forgotten, not restored.
    let second = &batches[0].entries[1].entry;
    fs::remove_dir_all(&second.trash_location)
        .or_else(|_| fs::remove_file(&second.trash_location))
        .expect("empty trash item");
    let batches = commands::history(&h.ctx).expect("history");
    assert_eq!(batches[0].entries.len(), 1);
    assert!(!batches[0].entries[0].in_trash);
    let second_id = std::slice::from_ref(&second.id);
    let gone = commands::restore(&h.ctx, second_id);
    assert!(!gone[0].ok);
    assert_eq!(
        commands::forget_history(&h.ctx, second_id).expect("forget"),
        1
    );
    assert!(commands::history(&h.ctx).expect("history").is_empty());
    assert!(commands::forget_history(&h.ctx, second_id).is_err());
}

#[test]
fn delete_immediately_leaves_no_history() {
    let (mut h, files) = harness();
    let settings = Settings {
        delete_immediately: true,
        ..h.ctx.settings.clone()
    };
    commands::save_settings(&mut h.ctx, &settings).expect("save settings");
    let scan = scan(&h);
    let report = commands::remove_leftovers(
        &h.ctx,
        &scan,
        std::slice::from_ref(&files.gone_cache),
        &mut noop,
    )
    .expect("remove");
    assert_eq!(report.removed.len(), 1);
    assert!(report.removed[0].trash_location.is_none());
    assert!(!files.gone_cache.exists());
    assert!(commands::history(&h.ctx).expect("history").is_empty());
}

#[test]
fn rescan_after_removal_finds_nothing_of_the_removed_group() {
    let (h, files) = harness();
    let first = scan(&h);
    let selected = vec![files.gone_cache.clone(), files.gone_prefs.clone()];
    commands::remove_leftovers(&h.ctx, &first, &selected, &mut noop).expect("remove");
    let second = scan(&h);
    assert!(group(&second, "com.bar.gone").is_none());
}

#[test]
fn removed_apps_round_trip() {
    let (h, _files) = harness();
    let apps = commands::removed_apps(&h.ctx).expect("removed apps");
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].bundle_id, "com.bar.gone");
    assert_eq!(apps[0].name, "Bar");

    commands::forget_removed_app(&h.ctx, "com.bar.gone").expect("forget");
    assert!(commands::removed_apps(&h.ctx)
        .expect("removed apps")
        .is_empty());
    // Forgetting again is harmless.
    commands::forget_removed_app(&h.ctx, "com.bar.gone").expect("forget again");

    // Without the history entry the same data drops to medium confidence.
    let scan = scan(&h);
    let gone = group(&scan, "com.bar.gone").expect("still listed");
    assert_eq!(gone.confidence, Confidence::Medium);
    assert!(gone.items.iter().all(|i| !i.checked));
}
