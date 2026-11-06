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
