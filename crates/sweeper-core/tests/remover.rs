mod common;

use std::os::unix::fs::symlink;

use common::*;
use sweeper_core::testing::{FakeTrash, Fixture};
use sweeper_core::{
    PathPolicy, PlanItem, RemovalMode, RemovalPlan, Remover, RestoreError, UndoLog,
};

fn plan_of(paths: &[&std::path::Path]) -> RemovalPlan {
    let items: Vec<PlanItem> = paths
        .iter()
        .map(|p| PlanItem {
            path: p.to_path_buf(),
            size: 100,
        })
        .collect();
    RemovalPlan {
        total_size: items.iter().map(|i| i.size).sum(),
        items,
    }
}

#[test]
fn plan_contains_only_checked_non_refused_items() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    touch(&fx, "Library/Caches/com.foo.app");
    touch(&fx, "Library/Logs/com.foo.app");
    let outside = outside_dir(&fx);
    symlink(&outside, fx.lib("Application Support").join("com.foo.app")).unwrap();
    let mut scan = scan(&fx, "com.foo.app");
    for g in &mut scan.groups {
        for i in &mut g.items {
            if i.path.ends_with("Logs/com.foo.app") {
                i.checked = false;
            } else {
                i.checked = true; // even the refused symlink: it must still be excluded
            }
        }
    }
    let plan = RemovalPlan::from_scan(&scan);
    let names: Vec<_> = rel(
        &fx,
        &plan
            .items
            .iter()
            .map(|i| i.path.clone())
            .collect::<Vec<_>>(),
    );
    assert_eq!(plan.items.len(), 2, "{names:?}");
    assert!(plan.total_size > 0);
}

#[test]
fn trash_mode_moves_items_and_records_undo_entries() {
    let fx = Fixture::new();
    let cache = touch_dir(&fx, "Library/Caches/com.foo.app");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));

    let mut seen = Vec::new();
    let report = remover.execute(&plan_of(&[&cache]), RemovalMode::Trash, &mut |p| {
        seen.push((p.done, p.total))
    });

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(seen, vec![(1, 1)]);
    assert!(!cache.exists());
    let location = report.removed[0].trash_location.clone().unwrap();
    assert!(location.starts_with(&fx.trash_dir) && location.join("data.bin").exists());

    let entries = remover.undo_log().entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].original, cache);
    assert_eq!(entries[0].trash_location, location);
    assert!(entries[0].timestamp > 0 && !entries[0].batch_id.is_empty());
    assert!(fx.env.data_dir.join("undo-log.json").exists());
    assert!(!fx.env.data_dir.join("undo-log.json.tmp").exists());
}

#[test]
fn restore_puts_item_back_and_refuses_to_overwrite() {
    let fx = Fixture::new();
    let cache = touch_dir(&fx, "Library/Caches/com.foo.app");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let undo = UndoLog::for_env(&fx.env);
    let remover = Remover::new(&fx.env, &policy, &trash, undo.clone());
    remover.execute(&plan_of(&[&cache]), RemovalMode::Trash, &mut |_| {});
    let id = undo.entries().unwrap()[0].id.clone();

    // The original path is taken again: restore must refuse.
    touch(&fx, "Library/Caches/com.foo.app/new.txt");
    assert!(matches!(
        undo.restore(&id),
        Err(RestoreError::OriginalExists(_))
    ));
    assert_eq!(undo.entries().unwrap().len(), 1);

    std::fs::remove_dir_all(&cache).unwrap();
    assert_eq!(undo.restore(&id).unwrap(), cache);
    assert!(cache.join("data.bin").exists());
    assert!(undo.entries().unwrap().is_empty());
    assert!(matches!(
        undo.restore(&id),
        Err(RestoreError::UnknownEntry(_))
    ));
}

#[test]
fn restore_refuses_when_trash_item_is_gone() {
    let fx = Fixture::new();
    let cache = touch(&fx, "Library/Caches/com.foo.app");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let undo = UndoLog::for_env(&fx.env);
    let remover = Remover::new(&fx.env, &policy, &trash, undo.clone());
    let report = remover.execute(&plan_of(&[&cache]), RemovalMode::Trash, &mut |_| {});
    std::fs::remove_file(report.removed[0].trash_location.as_ref().unwrap()).unwrap();
    let id = undo.entries().unwrap()[0].id.clone();
    assert!(matches!(
        undo.restore(&id),
        Err(RestoreError::TrashItemMissing(_))
    ));
}

#[test]
fn delete_mode_removes_permanently_without_undo_entry() {
    let fx = Fixture::new();
    let dir = touch_dir(&fx, "Library/Caches/com.foo.app");
    let file = touch(&fx, "Library/Logs/com.foo.app");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let report = remover.execute(&plan_of(&[&dir, &file]), RemovalMode::Delete, &mut |_| {});
    assert_eq!(report.removed.len(), 2);
    assert!(report.removed.iter().all(|r| r.trash_location.is_none()));
    assert!(!dir.exists() && !file.exists());
    assert!(std::fs::read_dir(&fx.trash_dir).unwrap().next().is_none());
    assert!(remover.undo_log().entries().unwrap().is_empty());
}

#[test]
fn failures_are_reported_with_reason_and_do_not_stop_the_run() {
    let fx = Fixture::new();
    let a = touch(&fx, "Library/Caches/a");
    let b = touch(&fx, "Library/Caches/b");
    let gone = fx.lib("Caches").join("gone");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    trash.fail_on(&a, "operation not permitted");
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let report = remover.execute(&plan_of(&[&a, &gone, &b]), RemovalMode::Delete, &mut |_| {});
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.failed.len(), 2);
    assert!(report.failed[0].1.contains("operation not permitted"));
    assert!(report.failed[1].1.contains("does not exist"));
    assert!(a.exists() && !b.exists());
}

#[test]
fn policy_is_rechecked_right_before_removal() {
    let fx = Fixture::new();
    let victim = touch_dir(&fx, "Library/Caches/com.foo.app");
    let outside = outside_dir(&fx);
    let policy = PathPolicy::new(&fx.env, &[]);
    let plan = plan_of(&[&victim]);
    // Between scan and execute the folder is swapped for a symlink.
    std::fs::remove_dir_all(&victim).unwrap();
    symlink(&outside, &victim).unwrap();

    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    for mode in [RemovalMode::Trash, RemovalMode::Delete] {
        let report = remover.execute(&plan, mode, &mut |_| {});
        assert!(report.removed.is_empty());
        assert_eq!(report.failed.len(), 1);
    }
    assert!(outside.join("precious.txt").exists());
    assert!(std::fs::symlink_metadata(&victim).is_ok());
}

#[test]
fn protected_paths_are_never_removed() {
    let fx = Fixture::new();
    let keep = touch(&fx, "Library/Caches/keep");
    let policy = PathPolicy::new(&fx.env, &[keep.display().to_string()]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    let report = remover.execute(&plan_of(&[&keep]), RemovalMode::Delete, &mut |_| {});
    assert_eq!(report.failed.len(), 1);
    assert!(keep.exists());
}

#[test]
fn action_log_records_paths_only() {
    let fx = Fixture::new();
    let f = touch(&fx, "Library/Caches/log-me");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let remover = Remover::new(&fx.env, &policy, &trash, UndoLog::for_env(&fx.env));
    remover.execute(&plan_of(&[&f]), RemovalMode::Delete, &mut |_| {});
    let text =
        std::fs::read_to_string(fx.env.home.join("Library/Logs/Sweeper Kit/sweeper-kit.log"))
            .unwrap();
    assert!(text.contains("\tdelete\t") && text.contains("log-me"));
}

/// Trashes `paths` and returns the undo log.
fn trashed(fx: &Fixture, paths: &[&std::path::Path]) -> UndoLog {
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    let undo = UndoLog::for_env(&fx.env);
    let remover = Remover::new(&fx.env, &policy, &trash, undo.clone());
    remover.execute(&plan_of(paths), RemovalMode::Trash, &mut |_| {});
    undo
}

#[test]
fn batches_group_entries_and_detect_missing_trash_items() {
    let fx = Fixture::new();
    let a = touch(&fx, "Library/Caches/a");
    let b = touch(&fx, "Library/Caches/b");
    let undo = trashed(&fx, &[&a, &b]);
    let c = touch(&fx, "Library/Caches/c");
    let policy = PathPolicy::new(&fx.env, &[]);
    let trash = FakeTrash::new(fx.trash_dir.clone());
    Remover::new(&fx.env, &policy, &trash, undo.clone()).execute(
        &plan_of(&[&c]),
        RemovalMode::Trash,
        &mut |_| {},
    );

    let batches = undo.batches().unwrap();
    assert_eq!(batches.len(), 2);
    // Batches from the same second keep the log order, newest first.
    assert_eq!(batches[0].entries.len(), 1);
    assert_eq!(batches[0].entries[0].entry.original, c);
    assert_eq!(batches[1].entries.len(), 2);
    assert_eq!(batches[1].size, 200);
    assert!(batches.iter().flat_map(|b| &b.entries).all(|e| e.in_trash));
    assert!(batches[0].timestamp >= batches[1].timestamp);

    let gone = batches[1].entries[0].entry.trash_location.clone();
    std::fs::remove_file(&gone).unwrap();
    let batches = undo.batches().unwrap();
    let missing: Vec<_> = batches[1].entries.iter().filter(|e| !e.in_trash).collect();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].entry.trash_location, gone);
}

#[test]
fn restore_many_reports_each_entry_separately() {
    let fx = Fixture::new();
    let a = touch(&fx, "Library/Caches/a");
    let b = touch(&fx, "Library/Caches/b");
    let c = touch(&fx, "Library/Caches/c");
    let undo = trashed(&fx, &[&a, &b, &c]);
    let entries = undo.entries().unwrap();
    let find = |p: &std::path::Path| entries.iter().find(|e| e.original == p).unwrap().clone();
    // b's original is taken again, c's Trash item is gone.
    touch(&fx, "Library/Caches/b");
    std::fs::remove_file(find(&c).trash_location).unwrap();

    let policy = PathPolicy::new(&fx.env, &[]);
    let (ia, ib, ic) = (find(&a).id, find(&b).id, find(&c).id);
    let ids = vec![ia.clone(), ib, ic, "nope".to_string(), ia.clone()];
    let results = undo.restore_many(&ids, &policy);
    assert_eq!(results.len(), 5);
    assert_eq!(results[0].1, Ok(a.clone()));
    assert!(matches!(results[1].1, Err(RestoreError::OriginalExists(_))));
    assert!(matches!(
        results[2].1,
        Err(RestoreError::TrashItemMissing(_))
    ));
    assert!(matches!(results[3].1, Err(RestoreError::UnknownEntry(_))));
    assert!(matches!(results[4].1, Err(RestoreError::UnknownEntry(_))));
    assert!(a.exists());
    let left: Vec<_> = undo.entries().unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(left.len(), 2);
    assert!(!left.contains(&ia));
}

#[test]
fn restore_many_recreates_parents_only_inside_allowed_roots() {
    let fx = Fixture::new();
    let nested = touch(&fx, "Library/Application Support/Foo/deep/file.bin");
    let undo = trashed(&fx, &[&nested]);
    std::fs::remove_dir_all(fx.lib("Application Support/Foo")).unwrap();
    let policy = PathPolicy::new(&fx.env, &[]);
    let id = undo.entries().unwrap()[0].id.clone();
    let results = undo.restore_many(std::slice::from_ref(&id), &policy);
    assert_eq!(results[0].1, Ok(nested.clone()));
    assert!(nested.exists());

    // An entry pointing into a personal folder is refused and leaves no folders behind.
    let docs_file = fx.env.home.join("Documents/Secret/note.txt");
    let src = fx.trash_dir.join("note.txt");
    std::fs::write(&src, b"x").unwrap();
    let log = serde_json::json!([{
        "id": "evil", "timestamp": 1, "original": docs_file,
        "trash_location": src, "size": 1, "batch_id": "b"
    }]);
    std::fs::write(undo.path(), log.to_string()).unwrap();
    let results = undo.restore_many(&["evil".to_string()], &policy);
    assert!(results[0].1.is_err());
    assert!(src.exists());
    assert!(!fx.env.home.join("Documents").exists());
    assert_eq!(undo.entries().unwrap().len(), 1);
}

#[test]
fn forget_drops_entries_without_touching_files() {
    let fx = Fixture::new();
    let a = touch(&fx, "Library/Caches/a");
    let b = touch(&fx, "Library/Caches/b");
    let undo = trashed(&fx, &[&a, &b]);
    let entries = undo.entries().unwrap();
    let ids = vec![entries[0].id.clone(), "unknown".to_string()];
    assert_eq!(undo.forget(&ids).unwrap(), 1);
    assert_eq!(undo.entries().unwrap().len(), 1);
    assert!(entries[0].trash_location.exists());
    assert_eq!(undo.forget(&ids).unwrap(), 0);
}

#[test]
fn old_format_undo_log_still_loads() {
    let fx = Fixture::new();
    let undo = UndoLog::for_env(&fx.env);
    std::fs::create_dir_all(&fx.env.data_dir).unwrap();
    std::fs::write(
        undo.path(),
        r#"[{"id":"1-0","timestamp":100,"original":"/tmp/o","trash_location":"/tmp/t","size":5,"batch_id":"b1"}]"#,
    )
    .unwrap();
    assert_eq!(undo.entries().unwrap().len(), 1);
    let batches = undo.batches().unwrap();
    assert_eq!(batches[0].batch_id, "b1");
    assert!(!batches[0].entries[0].in_trash);
}
