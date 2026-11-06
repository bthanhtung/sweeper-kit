//! Administrator scope through the command layer: scan marking, one-prompt
//! uninstall with receipts, cancellation, a missing helper and restore.
//! `/usr/bin/osascript` is scripted and never executed; the helper is a dummy
//! file that is never run.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use sweeper_app::commands;
use sweeper_app::state::{resolve_helper, AppContext};
use sweeper_core::testing::{
    write_file, FakeProcesses, FakeSpotlight, FakeTrash, Fixture, RecordingRunner,
};
use sweeper_core::{
    admin_invocation, parse_manifest, AdminOp, AdminResult, CommandOutput, CommandRunner, OpResult,
    RuleSet, ScanResult,
};

const BUNDLE_ID: &str = "com.foo.app";
const OSASCRIPT: &str = "/usr/bin/osascript";
const PKGUTIL: &str = "/usr/sbin/pkgutil";
const PKG_ID: &str = "com.foo.app.pkg";
const PKG_INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>pkg-version</key><string>2.1</string>
<key>install-location</key><string>/</string>
</dict></plist>"#;
const CANCELLED_STDERR: &str = "execution error: User canceled. (-128)";

/// How the scripted `osascript` answers.
#[derive(Clone, Copy)]
enum Prompt {
    /// Every operation succeeds.
    Allow,
    /// The user presses Cancel.
    Cancel,
}

/// Records everything and plays the helper: for `osascript` it reads the
/// manifest from argv and reports every operation as done.
struct AdminRunner {
    inner: Arc<RecordingRunner>,
    prompt: Prompt,
    staging: PathBuf,
}

impl CommandRunner for AdminRunner {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let recorded = self.inner.run(program, args)?;
        if program != OSASCRIPT {
            return Ok(recorded);
        }
        if let Prompt::Cancel = self.prompt {
            return Ok(CommandOutput {
                status: 1,
                stdout: String::new(),
                stderr: CANCELLED_STDERR.into(),
            });
        }
        let manifest = parse_manifest(args.last().expect("manifest argument")).expect("manifest");
        let ops = manifest
            .ops
            .iter()
            .enumerate()
            .map(|(n, op)| OpResult {
                ok: true,
                detail: "done".into(),
                staging_path: match op {
                    AdminOp::MoveToStaging { path } => {
                        Some(self.staging.join(&manifest.batch_id).join(format!(
                            "{n}-{}",
                            path.file_name().unwrap().to_string_lossy()
                        )))
                    }
                    _ => None,
                },
            })
            .collect();
        Ok(CommandOutput {
            status: 0,
            stdout: serde_json::to_string(&AdminResult { ops }).unwrap(),
            stderr: String::new(),
        })
    }
}

struct Harness {
    fx: Fixture,
    ctx: AppContext,
    runner: Arc<RecordingRunner>,
    app: PathBuf,
    helper: PathBuf,
    daemon: PathBuf,
}

fn harness(prompt: Prompt, with_helper: bool) -> Harness {
    let fx = Fixture::new();
    let app = fx.install("Foo", BUNDLE_ID, "1.0");
    for leftover in [
        "Library/Caches/com.foo.app/cache.bin",
        "Library/Preferences/com.foo.app.plist",
        "Library/Application Support/Foo/data.bin",
    ] {
        write_file(&fx.env.home.join(leftover), 100);
    }
    let daemon = fx.sys("Library/LaunchDaemons/com.foo.app.helper.plist");
    write_file(&daemon, 50);
    let helper = fx.tmp.path().join("sweeper-helper");
    write_file(&helper, 10);

    let inner = Arc::new(RecordingRunner::new());
    inner.respond_args(
        PKGUTIL,
        &["--pkgs"],
        CommandOutput {
            status: 0,
            stdout: format!("{PKG_ID}\ncom.other.pkg\n"),
            stderr: String::new(),
        },
    );
    inner.respond_args(
        PKGUTIL,
        &["--pkg-info-plist", PKG_ID],
        CommandOutput {
            status: 0,
            stdout: PKG_INFO.into(),
            stderr: String::new(),
        },
    );
    let staging = fx.sys("Library/Application Support/Sweeper Kit/Staging");
    let ctx = AppContext::new(
        fx.env.clone(),
        RuleSet::bundled(),
        Box::new(AdminRunner {
            inner: inner.clone(),
            prompt,
            staging,
        }),
        Box::new(FakeSpotlight::default()),
        Box::new(FakeProcesses::new()),
        Box::new(FakeTrash::new(fx.trash_dir.clone())),
    )
    .with_helper(with_helper.then(|| helper.clone()));
    Harness {
        fx,
        ctx,
        runner: inner,
        app,
        helper,
        daemon,
    }
}

fn scan(h: &Harness) -> ScanResult {
    commands::list_apps(&h.ctx);
    commands::scan_app(&h.ctx, &h.app, &AtomicBool::new(false)).expect("scan")
}

#[test]
fn scan_marks_the_daemon_as_requiring_admin() {
    let h = harness(Prompt::Allow, true);
    let scan = scan(&h);
    let daemon = scan
        .items()
        .find(|i| i.path == h.daemon)
        .expect("daemon found");
    assert!(daemon.requires_admin && daemon.refused.is_none() && daemon.checked);
    let cache = scan
        .items()
        .find(|i| i.path.ends_with("Caches/com.foo.app"))
        .expect("cache found");
    assert!(!cache.requires_admin);
    assert!(commands::admin_available(&h.ctx));
}

#[test]
fn receipts_lists_only_the_apps_packages() {
    let h = harness(Prompt::Allow, true);
    commands::list_apps(&h.ctx);
    let receipts = commands::receipts(&h.ctx, &h.app).expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].pkg_id, PKG_ID);
    assert_eq!(receipts[0].version, "2.1");
    assert_eq!(receipts[0].install_location, "/");
}

#[test]
fn uninstall_with_receipt_asks_for_authorization_once() {
    let h = harness(Prompt::Allow, true);
    let scan = scan(&h);
    let report = commands::uninstall(&h.ctx, &scan, &[PKG_ID.into()], &mut |_| {}).expect("run");

    let step = report.admin.expect("admin step");
    assert!(step.ok, "{}", step.detail);
    assert!(
        report.removal.failed.is_empty(),
        "{:?}",
        report.removal.failed
    );
    assert!(
        !h.fx.lib("Caches/com.foo.app").exists(),
        "user items trashed"
    );

    let calls = h.runner.calls_of(OSASCRIPT);
    assert_eq!(calls.len(), 1, "one password prompt");
    let args: Vec<&str> = calls[0].iter().map(String::as_str).collect();
    let manifest = parse_manifest(args.last().unwrap()).expect("manifest");
    let mut expected = admin_invocation(&h.helper, &manifest);
    assert_eq!(expected.remove(0), OSASCRIPT);
    assert_eq!(calls[0], expected, "argv shape");
    assert!(manifest.ops.contains(&AdminOp::Bootout {
        plist: h.daemon.clone()
    }));
    assert!(manifest.ops.contains(&AdminOp::MoveToStaging {
        path: h.daemon.clone()
    }));
    assert!(manifest.ops.contains(&AdminOp::ForgetReceipt {
        pkg_id: PKG_ID.into()
    }));

    let entries = commands::undo_entries(&h.ctx).expect("undo");
    let admin: Vec<_> = entries.iter().filter(|e| e.admin).collect();
    assert_eq!(admin.len(), 1);
    assert_eq!(admin[0].original, h.daemon);
}

#[test]
fn unknown_receipt_ids_are_rejected_before_anything_runs() {
    let h = harness(Prompt::Allow, true);
    let scan = scan(&h);
    for bad in ["com.other.pkg", "com.foo.app.unlisted", "--forget"] {
        let err = commands::uninstall(&h.ctx, &scan, &[bad.into()], &mut |_| {}).unwrap_err();
        assert!(err.contains("not an installer receipt"), "{err}");
    }
    assert!(h.runner.calls_of(OSASCRIPT).is_empty());
    assert!(h.app.exists() && h.fx.lib("Caches/com.foo.app").exists());
}

#[test]
fn missing_helper_leaves_admin_items_remaining_with_a_reason() {
    let h = harness(Prompt::Allow, false);
    assert!(!commands::admin_available(&h.ctx));
    let scan = scan(&h);
    let report = commands::uninstall(&h.ctx, &scan, &[PKG_ID.into()], &mut |_| {}).expect("run");

    assert!(h.runner.calls_of(OSASCRIPT).is_empty());
    assert!(h.daemon.exists());
    let (_, why) = report
        .remaining
        .iter()
        .find(|(path, _)| *path == h.daemon)
        .expect("daemon remains");
    assert!(why.contains("administrator"), "{why}");
    assert!(!report.admin.expect("admin step").ok);
    assert!(!h.app.exists(), "user items are still removed");
}

#[test]
fn cancelled_authorization_is_reported_and_user_items_are_trashed() {
    let h = harness(Prompt::Cancel, true);
    let scan = scan(&h);
    let report = commands::uninstall(&h.ctx, &scan, &[], &mut |_| {}).expect("run");

    let step = report.admin.expect("admin step");
    assert!(!step.ok);
    assert!(step
        .detail
        .contains("Administrator authorization was cancelled"));
    assert!(h.daemon.exists());
    assert!(report.remaining.iter().any(|(path, _)| *path == h.daemon));
    assert!(!h.app.exists());
    assert!(!h.fx.lib("Caches/com.foo.app").exists());
    assert_eq!(h.runner.calls_of(OSASCRIPT).len(), 1);
}

#[test]
fn restoring_admin_entries_uses_one_authorization() {
    let h = harness(Prompt::Allow, true);
    let scan = scan(&h);
    commands::uninstall(&h.ctx, &scan, &[], &mut |_| {}).expect("run");
    assert_eq!(h.runner.calls_of(OSASCRIPT).len(), 1);

    let entries = commands::undo_entries(&h.ctx).expect("undo");
    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let outcomes = commands::restore(&h.ctx, &ids);

    assert_eq!(outcomes.len(), ids.len());
    assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
    assert_eq!(
        outcomes.iter().map(|o| &o.id).collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>(),
        "answered in request order"
    );
    let calls = h.runner.calls_of(OSASCRIPT);
    assert_eq!(calls.len(), 2, "exactly one more prompt for the restore");
    let manifest = parse_manifest(calls[1].last().unwrap()).expect("manifest");
    assert_eq!(manifest.ops.len(), 1);
    assert!(matches!(
        &manifest.ops[0],
        AdminOp::RestoreFromStaging { original, .. } if *original == h.daemon
    ));
    assert!(h.app.exists(), "trash items came back");
    assert!(commands::undo_entries(&h.ctx).unwrap().is_empty());
}

#[test]
fn restoring_admin_entries_without_the_helper_fails_cleanly() {
    let mut h = harness(Prompt::Allow, true);
    let scan = scan(&h);
    commands::uninstall(&h.ctx, &scan, &[], &mut |_| {}).expect("run");
    let admin_id = commands::undo_entries(&h.ctx)
        .unwrap()
        .into_iter()
        .find(|e| e.admin)
        .expect("admin entry")
        .id;
    h.ctx.helper = None;
    let outcomes = commands::restore(&h.ctx, &[admin_id]);
    assert!(!outcomes[0].ok);
    assert!(outcomes[0].path_or_error.contains("helper"));
    assert_eq!(h.runner.calls_of(OSASCRIPT).len(), 1, "no new prompt");
}

#[test]
fn helper_is_resolved_only_as_a_regular_file_next_to_the_executable() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("sweeper-kit");
    assert_eq!(resolve_helper(&exe), None, "missing");

    let helper = dir.path().join("sweeper-helper");
    write_file(&helper, 10);
    assert_eq!(resolve_helper(&exe), Some(helper.clone()));

    fs::remove_file(&helper).unwrap();
    let target = dir.path().join("elsewhere");
    write_file(&target, 10);
    std::os::unix::fs::symlink(&target, &helper).unwrap();
    assert_eq!(resolve_helper(&exe), None, "symlink");

    fs::remove_file(&helper).unwrap();
    fs::create_dir(&helper).unwrap();
    assert_eq!(resolve_helper(&exe), None, "directory");
}
