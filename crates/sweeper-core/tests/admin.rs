//! System scope: `SystemPolicy`, the helper manifest protocol, its execution
//! and the `osascript` invocation. Everything runs against a fake system root
//! and a `RecordingRunner`; there is no root and no real `osascript`.

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};

use sweeper_core::testing::{write_file, Fixture, RecordingRunner};
use sweeper_core::{
    admin_invocation, execute_manifest, restore_admin_entries, run_admin, validate_manifest,
    AdminError, AdminManifest, AdminOp, AdminResult, CommandOutput, OpResult, PolicyError,
    RestoreError, SystemPolicy, UndoEntry, UndoLog, MAX_MANIFEST_BYTES, MAX_OPS,
};

const LAUNCHCTL: &str = "/bin/launchctl";
const PKGUTIL: &str = "/usr/sbin/pkgutil";
const OSASCRIPT: &str = "/usr/bin/osascript";

/// A fake machine with the system folders and one user (`/Users/alice`).
struct World {
    fx: Fixture,
    policy: SystemPolicy,
    uid: u32,
}

impl World {
    fn new() -> World {
        let fx = Fixture::new();
        for dir in [
            "Library/Application Support",
            "Library/Caches",
            "Library/Preferences",
            "Library/LaunchAgents",
            "Library/LaunchDaemons",
            "Library/PrivilegedHelperTools",
            "Library/Logs",
            "Applications/Utilities",
            "Users/alice",
        ] {
            fs::create_dir_all(fx.sys(dir)).unwrap();
        }
        let uid = fs::metadata(fx.sys("Users/alice")).unwrap().uid();
        let policy = SystemPolicy::new(&fx.env, &[]);
        World { fx, policy, uid }
    }

    fn sys(&self, rel: &str) -> PathBuf {
        self.fx.sys(rel)
    }

    /// Creates a 100-byte file below the fake system root.
    fn file(&self, rel: &str) -> PathBuf {
        let p = self.sys(rel);
        write_file(&p, 100);
        p
    }

    fn manifest(&self, ops: Vec<AdminOp>) -> AdminManifest {
        AdminManifest {
            version: 1,
            batch_id: "batch1".into(),
            user_uid: self.uid,
            user_home: "/Users/alice".into(),
            ops,
        }
    }

    fn staging(&self) -> PathBuf {
        self.policy.staging_dir().to_path_buf()
    }

    fn execute(&self, m: &AdminManifest, runner: &RecordingRunner) -> AdminResult {
        execute_manifest(m, &self.policy, &self.fx.env.system_root, runner)
    }
}

fn mv(path: &Path) -> AdminOp {
    AdminOp::MoveToStaging {
        path: path.to_path_buf(),
    }
}

fn is_invalid<T: std::fmt::Debug>(r: Result<T, AdminError>) -> bool {
    matches!(r, Err(AdminError::Invalid(_)))
}

// ---------------------------------------------------------------- policy

#[test]
fn system_policy_accepts_children_of_library_dirs_and_direct_app_bundles() {
    let w = World::new();
    for rel in [
        "Library/Caches/com.foo.app",
        "Library/Preferences/com.foo.app.plist",
        "Library/LaunchAgents/com.foo.app.plist",
        "Library/LaunchDaemons/com.foo.app.plist",
        "Library/PrivilegedHelperTools/com.foo.app.helper",
        "Library/Logs/com.foo.app",
        "Library/Application Support/com.foo.app/deep/file.bin",
        "Applications/Foo.app/Contents/Info.plist",
    ] {
        w.file(rel);
    }
    for rel in [
        "Library/Application Support/com.foo.app",
        "Library/Caches/com.foo.app",
        "Library/Preferences/com.foo.app.plist",
        "Library/LaunchAgents/com.foo.app.plist",
        "Library/LaunchDaemons/com.foo.app.plist",
        "Library/PrivilegedHelperTools/com.foo.app.helper",
        "Library/Logs/com.foo.app",
        "Library/Application Support/com.foo.app/deep/file.bin",
        "Applications/Foo.app",
    ] {
        assert!(
            w.policy.check(&w.sys(rel)).is_ok(),
            "{rel} should be allowed"
        );
    }
    fs::create_dir_all(w.sys("Applications/Utilities/Tool.app")).unwrap();
    assert!(w
        .policy
        .check(&w.sys("Applications/Utilities/Tool.app"))
        .is_ok());
}

#[test]
fn system_policy_refuses_apple_and_sweeper_kit_data() {
    let w = World::new();
    for rel in [
        "Library/Apple/Updates/x",
        "Library/Application Support/Apple/x",
        "Library/Application Support/APPLE/x",
        "Library/LaunchDaemons/com.apple.thing.plist",
        "Library/Preferences/com.apple.finder.plist",
        "Library/Caches/com.apple.Safari/cache.db",
        "Library/Application Support/Sweeper Kit/Staging/batch/0-x",
        "Library/Application Support/SWEEPER KIT/other",
        "Library/Logs/Sweeper Kit/helper.log",
    ] {
        w.file(rel);
        let err = w.policy.check(&w.sys(rel)).unwrap_err();
        assert!(
            matches!(
                err,
                PolicyError::Denied(_) | PolicyError::OutsideAllowedRoots
            ),
            "{rel}: {err:?}"
        );
    }
    // The staging folder and its parent are refused outright.
    for dir in [
        w.staging(),
        w.sys("Library/Application Support/Sweeper Kit"),
    ] {
        assert!(matches!(w.policy.check(&dir), Err(PolicyError::Denied(_))));
    }
}

#[test]
fn system_policy_refuses_apple_bundles_by_identifier_and_protected_ids() {
    let w = World::new();
    sweeper_core::testing::make_app(&w.sys("Applications"), "Safari", "com.apple.Safari", "1");
    sweeper_core::testing::make_app(
        &w.sys("Applications"),
        "Sweeper Kit",
        sweeper_core::SELF_BUNDLE_ID,
        "1",
    );
    sweeper_core::testing::make_app(&w.sys("Applications"), "Keep", "com.keep.me", "1");
    sweeper_core::testing::make_app(&w.sys("Applications"), "Foo", "com.foo.app", "1");
    assert!(matches!(
        w.policy.check(&w.sys("Applications/Safari.app")),
        Err(PolicyError::Denied(_))
    ));
    assert!(matches!(
        w.policy.check(&w.sys("Applications/Sweeper Kit.app")),
        Err(PolicyError::Denied(_))
    ));
    let strict = SystemPolicy::new(&w.fx.env, &["com.keep.me".to_string()]);
    assert_eq!(
        strict.check(&w.sys("Applications/Keep.app")),
        Err(PolicyError::Protected)
    );
    assert!(w.policy.check(&w.sys("Applications/Keep.app")).is_ok());
    assert!(strict.check(&w.sys("Applications/Foo.app")).is_ok());
}

#[test]
fn system_policy_refuses_symlink_escapes() {
    let w = World::new();
    let outside = w.fx.tmp.path().join("outside");
    write_file(&outside.join("precious.txt"), 10);
    let outside = fs::canonicalize(&outside).unwrap();

    // A link inside an allowed folder pointing out of it.
    symlink(
        outside.join("precious.txt"),
        w.sys("Library/Caches/com.foo.app"),
    )
    .unwrap();
    assert_eq!(
        w.policy.check(&w.sys("Library/Caches/com.foo.app")),
        Err(PolicyError::SymlinkEscape)
    );

    // A linked parent folder.
    symlink(&outside, w.sys("Library/Caches/linked")).unwrap();
    assert_eq!(
        w.policy.check(&w.sys("Library/Caches/linked/precious.txt")),
        Err(PolicyError::SymlinkEscape)
    );
    assert!(outside.join("precious.txt").exists());
}

#[test]
fn system_policy_refuses_paths_outside_the_allowed_dirs() {
    let w = World::new();
    for rel in [
        "Library/Caches",
        "Library/Foo/x",
        "Library/Frameworks/x",
        "etc/hosts",
        "System/Library/x",
        "usr/local/bin/tool",
        "Users/alice/Library/Caches/x",
        "Applications/notes.txt",
        "Applications/Foo.app/Contents",
        "Applications/Utilities/Tool.app/Contents/MacOS",
        "Applications/Nested/Foo.app",
    ] {
        if !w.sys(rel).exists() {
            w.file(rel);
        }
        assert!(
            w.policy.check(&w.sys(rel)).is_err(),
            "{rel} should be refused"
        );
    }
    assert!(w.policy.check(Path::new("relative/path")).is_err());
    assert!(w
        .policy
        .check(&w.sys("Library/Caches/../../etc/hosts"))
        .is_err());
    assert!(w.policy.check(&w.sys("Library/Caches/missing")).is_err());
}

#[test]
fn system_policy_destination_check_allows_missing_targets_only_inside_roots() {
    let w = World::new();
    assert!(w
        .policy
        .check_destination(&w.sys("Library/LaunchDaemons/com.foo.app.plist"))
        .is_ok());
    assert!(w
        .policy
        .check_destination(&w.sys("Library/Application Support/com.foo.app/new/dir"))
        .is_ok());
    for rel in [
        "Library/Apple/x",
        "Library/LaunchDaemons/com.apple.x.plist",
        "Library/Application Support/Sweeper Kit/Staging/b/x",
        "etc/hosts",
        "Library/Caches",
    ] {
        assert!(w.policy.check_destination(&w.sys(rel)).is_err(), "{rel}");
    }
}

#[test]
fn staging_check_requires_exactly_batch_and_item_below_the_staging_dir() {
    let w = World::new();
    let staging = w.staging();
    assert!(w.policy.check_staging(&staging.join("b/0-x")).is_ok());
    for bad in [
        staging.clone(),
        staging.join("b"),
        staging.join("b/c/d"),
        staging.join("../0-x"),
        w.sys("Library/Caches/0-x"),
        PathBuf::from("relative"),
    ] {
        assert!(w.policy.check_staging(&bad).is_err(), "{}", bad.display());
    }
}

// ------------------------------------------------------------ validation

fn valid_manifest(w: &World) -> AdminManifest {
    let plist = w.file("Library/LaunchDaemons/com.foo.app.helper.plist");
    let agent = w.file("Library/LaunchAgents/com.foo.app.agent.plist");
    let cache = w.file("Library/Caches/com.foo.app/c.bin");
    w.manifest(vec![
        AdminOp::Bootout {
            plist: plist.clone(),
        },
        AdminOp::Bootout { plist: agent },
        mv(&plist),
        mv(&cache),
        AdminOp::RestoreFromStaging {
            staging_path: w.staging().join("batch0/0-old"),
            original: w.sys("Library/Application Support/com.foo.app"),
        },
        AdminOp::ForgetReceipt {
            pkg_id: "com.foo.app.pkg".into(),
        },
    ])
}

#[test]
fn validate_manifest_accepts_a_valid_manifest() {
    let w = World::new();
    let m = valid_manifest(&w);
    assert_eq!(validate_manifest(&m, &w.policy), Ok(()));
}

#[test]
fn validate_manifest_rejects_too_many_ops_and_oversized_manifests() {
    let w = World::new();
    let file = w.file("Library/Caches/com.foo.app");
    let m = w.manifest(vec![mv(&file); MAX_OPS]);
    assert_eq!(validate_manifest(&m, &w.policy), Ok(()));
    let m = w.manifest(vec![mv(&file); MAX_OPS + 1]);
    assert!(is_invalid(validate_manifest(&m, &w.policy)));
    assert!(is_invalid(validate_manifest(
        &w.manifest(vec![]),
        &w.policy
    )));

    let huge = PathBuf::from(format!("/{}", "a".repeat(MAX_MANIFEST_BYTES)));
    assert!(is_invalid(validate_manifest(
        &w.manifest(vec![mv(&huge)]),
        &w.policy
    )));
    let json = format!(
        r#"{{"version":1,"batch_id":"b","user_uid":1,"user_home":"/Users/alice","ops":[],"pad":"{}"}}"#,
        "x".repeat(MAX_MANIFEST_BYTES)
    );
    assert!(is_invalid(sweeper_core::parse_manifest(&json)));
}

#[test]
fn validate_manifest_rejects_bad_package_ids() {
    let w = World::new();
    for id in [
        "",
        "-rf",
        ".hidden",
        "com.foo app",
        "com.foo;rm",
        "com.foo/../x",
        "com.apple.pkg.X",
        "COM.APPLE.pkg.X",
        "é",
    ] {
        let m = w.manifest(vec![AdminOp::ForgetReceipt { pkg_id: id.into() }]);
        assert!(is_invalid(validate_manifest(&m, &w.policy)), "{id:?}");
    }
    let too_long = "a".repeat(256);
    let m = w.manifest(vec![AdminOp::ForgetReceipt { pkg_id: too_long }]);
    assert!(is_invalid(validate_manifest(&m, &w.policy)));
    let ok = "a".repeat(255);
    let m = w.manifest(vec![AdminOp::ForgetReceipt { pkg_id: ok }]);
    assert_eq!(validate_manifest(&m, &w.policy), Ok(()));
}

#[test]
fn validate_manifest_rejects_bad_user_and_batch() {
    let w = World::new();
    let ops = vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app".into(),
    }];
    let check = |edit: &dyn Fn(&mut AdminManifest)| {
        let mut m = w.manifest(ops.clone());
        edit(&mut m);
        validate_manifest(&m, &w.policy)
    };
    assert_eq!(check(&|_| {}), Ok(()));
    for home in [
        "/Users/alice/Library",
        "/Users",
        "/Users/",
        "/Users/../etc",
        "/Users/bob",
        "/var/root",
        "Users/alice",
        "/tmp/Users/alice",
    ] {
        assert!(is_invalid(check(&|m| m.user_home = home.into())), "{home}");
    }
    assert!(is_invalid(check(&|m| m.user_uid = w.uid + 1)));
    assert!(is_invalid(check(&|m| m.user_uid = 0)));
    assert!(is_invalid(check(&|m| m.version = 2)));
    for batch in ["", ".x", "a/b", "../x", "a b", &"a".repeat(65)] {
        assert!(
            is_invalid(check(&|m| m.batch_id = batch.into())),
            "{batch:?}"
        );
    }
}

#[test]
fn validate_manifest_rejects_paths_outside_policy_and_staging() {
    let w = World::new();
    let ok_file = w.file("Library/Caches/com.foo.app");
    let hosts = w.file("etc/hosts");
    let apple = w.file("Library/LaunchDaemons/com.apple.x.plist");
    let cache_plist = w.file("Library/Preferences/com.foo.app.plist");
    let not_plist = w.file("Library/LaunchDaemons/com.foo.app.helper");
    let bad_ops = [
        mv(&hosts),
        mv(&apple),
        mv(&w.sys("Library/Caches/missing")),
        AdminOp::Bootout { plist: cache_plist },
        AdminOp::Bootout { plist: not_plist },
        AdminOp::Bootout {
            plist: apple.clone(),
        },
        AdminOp::RestoreFromStaging {
            staging_path: w.sys("Library/Caches/com.foo.app"),
            original: w.sys("Library/Caches/restored"),
        },
        AdminOp::RestoreFromStaging {
            staging_path: w.staging().join("b/0-x"),
            original: hosts.clone(),
        },
        AdminOp::RestoreFromStaging {
            staging_path: w.staging().join("b/0-x"),
            original: w.sys("Library/Apple/x"),
        },
    ];
    for op in bad_ops {
        // One good operation next to a bad one: all or nothing.
        let m = w.manifest(vec![mv(&ok_file), op.clone()]);
        assert!(
            is_invalid(validate_manifest(&m, &w.policy)),
            "{op:?} should be rejected"
        );
    }
}

// ------------------------------------------------------------- execution

#[test]
fn move_to_staging_moves_the_item_and_reports_the_staging_path() {
    let w = World::new();
    let plist = w.file("Library/LaunchDaemons/com.foo.app.helper.plist");
    let dir = w.file("Library/Application Support/com.foo.app/data.bin");
    let dir = dir.parent().unwrap().to_path_buf();
    let m = w.manifest(vec![mv(&plist), mv(&dir)]);
    let result = w.execute(&m, &RecordingRunner::new());

    assert_eq!(result.ops.len(), 2);
    let staging = w.staging().join("batch1");
    let first = result.ops[0].staging_path.clone().unwrap();
    assert!(result.ops[0].ok, "{:?}", result.ops[0]);
    assert_eq!(first, staging.join("0-com.foo.app.helper.plist"));
    assert!(first.exists());
    assert!(!plist.exists());
    let second = result.ops[1].staging_path.clone().unwrap();
    assert_eq!(second, staging.join("1-com.foo.app"));
    assert!(second.join("data.bin").exists());
    assert!(!dir.exists());

    // Staging folders are private and every action is logged.
    let mode = fs::metadata(&staging).unwrap().mode() & 0o777;
    assert_eq!(mode & 0o077, 0, "staging dir mode {mode:o}");
    let log = fs::read_to_string(w.sys("Library/Logs/Sweeper Kit/helper.log")).unwrap();
    assert_eq!(log.lines().count(), 2);
    assert!(log.lines().all(|l| l.split('\t').nth(1) == Some("stage")));
}

#[test]
fn execution_reports_each_failure_and_continues() {
    let w = World::new();
    let gone = w.file("Library/Caches/gone");
    let keep = w.file("Library/Caches/keep");
    let m = w.manifest(vec![mv(&gone), mv(&keep)]);
    fs::remove_file(&gone).unwrap();
    // Validation fails for the vanished item, so nothing at all is touched.
    let result = w.execute(&m, &RecordingRunner::new());
    assert!(result.ops.iter().all(|o| !o.ok));
    assert!(keep.exists());

    // A path that disappears after validation fails on its own.
    let a = w.file("Library/Caches/a");
    let b = w.file("Library/Caches/b");
    let m = w.manifest(vec![mv(&a), mv(&a), mv(&b)]);
    let result = w.execute(&m, &RecordingRunner::new());
    assert!(result.ops[0].ok);
    assert!(!result.ops[1].ok);
    assert!(result.ops[2].ok, "{:?}", result.ops[2]);
}

#[test]
fn invalid_manifest_changes_nothing() {
    let w = World::new();
    let file = w.file("Library/Caches/com.foo.app");
    let m = w.manifest(vec![
        mv(&file),
        AdminOp::ForgetReceipt {
            pkg_id: "bad id".into(),
        },
    ]);
    let runner = RecordingRunner::new();
    let result = w.execute(&m, &runner);
    assert_eq!(result.ops.len(), 2);
    assert!(result.ops.iter().all(|o| !o.ok));
    assert!(file.exists());
    assert!(runner.calls().is_empty());
    assert!(!w.staging().exists());
}

#[test]
fn restore_from_staging_puts_the_item_back_and_never_overwrites() {
    let w = World::new();
    let original = w.file("Library/LaunchDaemons/com.foo.app.helper.plist");
    let moved = w.execute(&w.manifest(vec![mv(&original)]), &RecordingRunner::new());
    let staged = moved.ops[0].staging_path.clone().unwrap();
    assert!(!original.exists());

    // The original path is taken again: refused, staged item untouched.
    write_file(&original, 5);
    let restore = |staged: &Path| {
        w.manifest(vec![AdminOp::RestoreFromStaging {
            staging_path: staged.to_path_buf(),
            original: original.clone(),
        }])
    };
    let refused = w.execute(&restore(&staged), &RecordingRunner::new());
    assert!(!refused.ops[0].ok);
    assert!(refused.ops[0].detail.contains("already exists"));
    assert!(staged.exists());
    assert_eq!(fs::metadata(&original).unwrap().len(), 5);

    // Free again: restored, and the empty batch folder is cleaned up.
    fs::remove_file(&original).unwrap();
    let done = w.execute(&restore(&staged), &RecordingRunner::new());
    assert!(done.ops[0].ok, "{:?}", done.ops[0]);
    assert_eq!(fs::metadata(&original).unwrap().len(), 100);
    assert!(!staged.exists());
    assert!(!w.staging().join("batch1").exists());

    // A second restore finds nothing in staging.
    let again = w.execute(&restore(&staged), &RecordingRunner::new());
    assert!(!again.ops[0].ok);
}

#[test]
fn restore_recreates_a_missing_parent_inside_the_allowed_root() {
    let w = World::new();
    let nested = w.file("Library/Application Support/com.foo.app/sub/data.bin");
    let tree = w.sys("Library/Application Support/com.foo.app");
    let moved = w.execute(&w.manifest(vec![mv(&tree)]), &RecordingRunner::new());
    let staged = moved.ops[0].staging_path.clone().unwrap();
    let m = w.manifest(vec![AdminOp::RestoreFromStaging {
        staging_path: staged,
        original: tree.clone(),
    }]);
    assert!(w.execute(&m, &RecordingRunner::new()).ops[0].ok);
    assert!(nested.exists());
}

#[test]
fn bootout_uses_the_right_launchctl_domain() {
    let w = World::new();
    let daemon = w.file("Library/LaunchDaemons/com.foo.app.helper.plist");
    let agent = w.file("Library/LaunchAgents/com.foo.app.agent.plist");
    let runner = RecordingRunner::new();
    let m = w.manifest(vec![
        AdminOp::Bootout {
            plist: daemon.clone(),
        },
        AdminOp::Bootout {
            plist: agent.clone(),
        },
    ]);
    let result = w.execute(&m, &runner);
    assert!(result.ops.iter().all(|o| o.ok), "{result:?}");
    assert_eq!(
        runner.calls(),
        vec![
            vec![
                LAUNCHCTL.to_string(),
                "bootout".into(),
                "system".into(),
                daemon.display().to_string()
            ],
            vec![
                LAUNCHCTL.to_string(),
                "bootout".into(),
                format!("gui/{}", w.uid),
                agent.display().to_string()
            ],
        ]
    );
    // Booting out never removes the file.
    assert!(daemon.exists() && agent.exists());
}

#[test]
fn bootout_treats_not_loaded_as_success_and_reports_other_failures() {
    let w = World::new();
    let daemon = w.file("Library/LaunchDaemons/com.foo.app.helper.plist");
    let m = w.manifest(vec![AdminOp::Bootout { plist: daemon }]);
    let runner = RecordingRunner::new();
    runner.respond(
        LAUNCHCTL,
        CommandOutput {
            status: 113,
            ..CommandOutput::default()
        },
    );
    let result = w.execute(&m, &runner);
    assert!(result.ops[0].ok);
    assert!(result.ops[0].detail.contains("not loaded"));

    runner.respond(
        LAUNCHCTL,
        CommandOutput {
            status: 5,
            stderr: "Boot-out failed: 5".into(),
            ..CommandOutput::default()
        },
    );
    let result = w.execute(&m, &runner);
    assert!(!result.ops[0].ok);
    assert!(result.ops[0].detail.contains("Boot-out failed"));
}

#[test]
fn forget_receipt_runs_pkgutil_with_an_argument_array() {
    let w = World::new();
    let runner = RecordingRunner::new();
    let m = w.manifest(vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app.pkg".into(),
    }]);
    let result = w.execute(&m, &runner);
    assert!(result.ops[0].ok);
    assert_eq!(
        runner.calls(),
        vec![vec![
            PKGUTIL.to_string(),
            "--forget".into(),
            "com.foo.app.pkg".into()
        ]]
    );
    runner.fail_to_start(PKGUTIL);
    assert!(!w.execute(&m, &runner).ops[0].ok);
}

#[test]
fn staging_refuses_a_planted_symlink_or_foreign_directory() {
    let w = World::new();
    let outside = w.fx.tmp.path().join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    // Someone replaced the staging folder by a link before the helper ran.
    fs::create_dir_all(w.staging().parent().unwrap()).unwrap();
    symlink(&outside, w.staging()).unwrap();
    let file = w.file("Library/Caches/com.foo.app");
    let result = w.execute(&w.manifest(vec![mv(&file)]), &RecordingRunner::new());
    assert!(!result.ops[0].ok);
    assert!(file.exists());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

// ------------------------------------------------------------ invocation

#[test]
fn admin_invocation_has_the_exact_osascript_argv() {
    let w = World::new();
    let m = w.manifest(vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app".into(),
    }]);
    let helper = Path::new("/Applications/Sweeper Kit.app/Contents/MacOS/sweeper-helper");
    let argv = admin_invocation(helper, &m);
    assert_eq!(
        argv,
        vec![
            OSASCRIPT.to_string(),
            "-e".into(),
            "on run argv".into(),
            "-e".into(),
            "do shell script (quoted form of item 1 of argv) & \" \" & (quoted form of item 2 of argv) with administrator privileges".into(),
            "-e".into(),
            "end run".into(),
            helper.display().to_string(),
            serde_json::to_string(&m).unwrap(),
        ]
    );
}

#[test]
fn hostile_paths_and_json_stay_single_argv_items() {
    let w = World::new();
    let evil = PathBuf::from("/tmp/it's'; rm -rf / #\"$(touch x)`id`\n/helper");
    let m = w.manifest(vec![
        mv(&PathBuf::from("/Library/Caches/a'b\"c; rm -rf / #")),
        AdminOp::ForgetReceipt { pkg_id: "x".into() },
    ]);
    let argv = admin_invocation(&evil, &m);
    assert_eq!(argv.len(), 9);
    assert_eq!(argv[7], evil.display().to_string());
    let back: AdminManifest = serde_json::from_str(&argv[8]).unwrap();
    assert_eq!(back, m);
    // The only text that is not data is the fixed script.
    assert!(!argv[4].contains("rm -rf"));
}

fn helper_file(w: &World) -> PathBuf {
    let p = w.fx.tmp.path().join("sweeper-helper");
    write_file(&p, 10);
    p
}

fn answer(ops: Vec<OpResult>) -> CommandOutput {
    CommandOutput {
        status: 0,
        stdout: format!("{}\n", serde_json::to_string(&AdminResult { ops }).unwrap()),
        stderr: String::new(),
    }
}

fn op_ok(detail: &str) -> OpResult {
    OpResult {
        ok: true,
        detail: detail.into(),
        staging_path: None,
    }
}

#[test]
fn run_admin_parses_the_helper_answer_with_one_invocation() {
    let w = World::new();
    let helper = helper_file(&w);
    let m = w.manifest(vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app".into(),
    }]);
    let runner = RecordingRunner::new();
    runner.respond(OSASCRIPT, answer(vec![op_ok("com.foo.app")]));
    let result = run_admin(&runner, &helper, &m).unwrap();
    assert_eq!(result.ops, vec![op_ok("com.foo.app")]);
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], admin_invocation(&helper, &m));
}

#[test]
fn run_admin_maps_cancel_and_other_failures() {
    let w = World::new();
    let helper = helper_file(&w);
    let m = w.manifest(vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app".into(),
    }]);
    let runner = RecordingRunner::new();
    runner.respond(
        OSASCRIPT,
        CommandOutput {
            status: 1,
            stderr: "0:42: execution error: User canceled. (-128)".into(),
            ..CommandOutput::default()
        },
    );
    assert_eq!(run_admin(&runner, &helper, &m), Err(AdminError::Cancelled));

    runner.respond(
        OSASCRIPT,
        CommandOutput {
            status: 1,
            stderr: "execution error: sweeper-helper must run as root (3)".into(),
            ..CommandOutput::default()
        },
    );
    assert!(matches!(
        run_admin(&runner, &helper, &m),
        Err(AdminError::Helper(msg)) if msg.contains("must run as root")
    ));

    runner.respond(OSASCRIPT, answer(vec![]));
    assert!(matches!(
        run_admin(&runner, &helper, &m),
        Err(AdminError::BadOutput(_))
    ));
    runner.respond(
        OSASCRIPT,
        CommandOutput {
            stdout: "not json".into(),
            ..CommandOutput::default()
        },
    );
    assert!(matches!(
        run_admin(&runner, &helper, &m),
        Err(AdminError::BadOutput(_))
    ));
    runner.fail_to_start(OSASCRIPT);
    assert!(matches!(
        run_admin(&runner, &helper, &m),
        Err(AdminError::Launch(_))
    ));
}

#[test]
fn run_admin_requires_an_existing_absolute_helper_and_sane_limits() {
    let w = World::new();
    let m = w.manifest(vec![AdminOp::ForgetReceipt {
        pkg_id: "com.foo.app".into(),
    }]);
    let runner = RecordingRunner::new();
    for helper in [
        PathBuf::from("sweeper-helper"),
        w.fx.tmp.path().join("missing"),
        w.fx.tmp.path().to_path_buf(),
    ] {
        assert_eq!(
            run_admin(&runner, &helper, &m),
            Err(AdminError::HelperMissing(helper.clone()))
        );
    }
    let helper = helper_file(&w);
    let too_many = w.manifest(vec![
        AdminOp::ForgetReceipt { pkg_id: "a".into() };
        MAX_OPS + 1
    ]);
    assert!(is_invalid(run_admin(&runner, &helper, &too_many)));
    assert!(runner.calls().is_empty());
}

// ------------------------------------------------------- undo and restore

fn admin_entry(id: &str, original: &Path, staged: &Path, admin: bool) -> UndoEntry {
    UndoEntry {
        id: id.into(),
        timestamp: 100,
        original: original.to_path_buf(),
        trash_location: staged.to_path_buf(),
        size: 100,
        batch_id: "batch1".into(),
        admin,
    }
}

#[test]
fn admin_undo_entries_refuse_a_normal_restore() {
    let w = World::new();
    let staged = w.file("staged/0-x");
    let log = UndoLog::new(w.fx.tmp.path().join("undo.json"));
    let original = w.sys("Library/Caches/x");
    fs::write(
        log.path(),
        serde_json::to_vec(&[admin_entry("e1", &original, &staged, true)]).unwrap(),
    )
    .unwrap();
    assert_eq!(log.restore("e1"), Err(RestoreError::RequiresAdmin));
    assert!(staged.exists() && !original.exists());
    let results = log.restore_many(
        &["e1".to_string()],
        &sweeper_core::PathPolicy::new(&w.fx.env, &[]),
    );
    assert_eq!(results[0].1, Err(RestoreError::RequiresAdmin));
    // The batch listing never claims an admin item is missing.
    let batches = log.batches().unwrap();
    assert!(batches[0].entries[0].in_trash);
    assert!(batches[0].entries[0].entry.admin);
}

#[test]
fn old_undo_entries_without_the_admin_flag_still_load() {
    let w = World::new();
    let path = w.fx.tmp.path().join("undo.json");
    fs::write(
        &path,
        r#"[{"id":"1","timestamp":1,"original":"/a","trash_location":"/b","size":1,"batch_id":"x"}]"#,
    )
    .unwrap();
    let entries = UndoLog::new(path).entries().unwrap();
    assert!(!entries[0].admin);
}

#[test]
fn restore_admin_entries_uses_one_invocation_and_forgets_restored_entries() {
    let w = World::new();
    let helper = helper_file(&w);
    let log = UndoLog::new(w.fx.tmp.path().join("undo.json"));
    let a = w.sys("Library/Caches/a");
    let b = w.sys("Library/Caches/b");
    let staged = w.staging().join("batch1/0-a");
    fs::write(
        log.path(),
        serde_json::to_vec(&[
            admin_entry("a", &a, &staged, true),
            admin_entry("b", &b, &staged, true),
            admin_entry("plain", &a, &staged, false),
        ])
        .unwrap(),
    )
    .unwrap();
    let runner = RecordingRunner::new();
    runner.respond(
        OSASCRIPT,
        answer(vec![
            op_ok("restored"),
            OpResult {
                ok: false,
                detail: "item is no longer in staging".into(),
                staging_path: None,
            },
        ]),
    );
    let ids: Vec<String> = ["a", "b", "plain", "nope"].map(String::from).to_vec();
    let results = restore_admin_entries(&runner, &helper, &w.fx.env, &log, &ids).unwrap();

    assert_eq!(runner.calls().len(), 1);
    assert_eq!(results[0].1, Ok(a));
    assert!(matches!(&results[1].1, Err(RestoreError::Io(m)) if m.contains("no longer")));
    assert_eq!(results[2].1, Err(RestoreError::InvalidEntry));
    assert_eq!(results[3].1, Err(RestoreError::UnknownEntry("nope".into())));
    let left: Vec<String> = log.entries().unwrap().into_iter().map(|e| e.id).collect();
    assert_eq!(left, vec!["b", "plain"]);

    // Nothing to restore: no prompt at all.
    let quiet = RecordingRunner::new();
    let none = restore_admin_entries(&quiet, &helper, &w.fx.env, &log, &["plain".into()]).unwrap();
    assert!(none[0].1.is_err());
    assert!(quiet.calls().is_empty());

    // A cancelled prompt is an error, and the entries stay.
    runner.respond(
        OSASCRIPT,
        CommandOutput {
            status: 1,
            stderr: "User canceled. (-128)".into(),
            ..CommandOutput::default()
        },
    );
    let err = restore_admin_entries(&runner, &helper, &w.fx.env, &log, &["b".into()]);
    assert_eq!(err, Err(AdminError::Cancelled));
    assert_eq!(log.entries().unwrap().len(), 2);
}
