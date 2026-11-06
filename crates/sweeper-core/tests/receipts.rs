mod common;

use common::*;
use sweeper_core::testing::{Fixture, RecordingRunner};
use sweeper_core::{find_receipts, AppInfo, CommandOutput};

const PKGUTIL: &str = "/usr/sbin/pkgutil";

fn out(stdout: &str) -> CommandOutput {
    CommandOutput {
        stdout: stdout.into(),
        ..CommandOutput::default()
    }
}

fn info_plist(version: &str, location: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>install-location</key><string>{location}</string>
<key>pkg-version</key><string>{version}</string>
<key>pkgid</key><string>ignored</string>
</dict></plist>"#
    )
}

fn app(bundle_id: &str, team: Option<&str>) -> (Fixture, AppInfo) {
    let fx = Fixture::new();
    fx.install("Foo", bundle_id, "1.0");
    let mut app = app_by_id(&installed(&fx), bundle_id);
    app.team_id = team.map(String::from);
    (fx, app)
}

fn script_pkgs(runner: &RecordingRunner, pkgs: &[&str]) {
    runner.respond_args(PKGUTIL, &["--pkgs"], out(&pkgs.join("\n")));
    for pkg in pkgs {
        runner.respond_args(
            PKGUTIL,
            &["--pkg-info-plist", pkg],
            out(&info_plist("2.5", "Applications")),
        );
        runner.respond_args(
            PKGUTIL,
            &["--files", pkg],
            out("Foo.app\nFoo.app/Contents\n\n"),
        );
    }
}

#[test]
fn matches_the_bundle_id_and_its_dotted_children_only() {
    let (_fx, app) = app("com.foo.app", None);
    let runner = RecordingRunner::new();
    script_pkgs(
        &runner,
        &[
            "com.foo.app",
            "com.foo.app.pkg",
            "com.foo.application",
            "com.foo.app-extra",
            "com.foo",
            "xcom.foo.app",
            "com.apple.pkg.X",
            "com.other.thing",
        ],
    );
    let receipts = find_receipts(&runner, &app);
    let ids: Vec<&str> = receipts.iter().map(|r| r.pkg_id.as_str()).collect();
    assert_eq!(ids, vec!["com.foo.app", "com.foo.app.pkg"]);
    assert_eq!(receipts[0].version, "2.5");
    assert_eq!(receipts[0].install_location, "Applications");
    assert_eq!(receipts[0].files_count, Some(2));
}

#[test]
fn runs_pkgutil_by_absolute_path_with_fixed_arguments() {
    let (_fx, app) = app("com.foo.app", None);
    let runner = RecordingRunner::new();
    script_pkgs(&runner, &["com.foo.app.pkg", "com.other"]);
    find_receipts(&runner, &app);
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        runner.calls(),
        vec![
            strs(&[PKGUTIL, "--pkgs"]),
            strs(&[PKGUTIL, "--pkg-info-plist", "com.foo.app.pkg"]),
            strs(&[PKGUTIL, "--files", "com.foo.app.pkg"]),
        ]
    );
}

#[test]
fn team_prefixed_receipts_need_the_full_bundle_id() {
    let (_fx, app) = app("com.foo.app", Some("ABCDE12345"));
    let runner = RecordingRunner::new();
    script_pkgs(
        &runner,
        &[
            "ABCDE12345.com.foo.app",
            "ABCDE12345.com.foo.app.installer",
            "ABCDE12345.other",
            "ABCDE12345.com.foo.application",
            "OTHERTEAM1.com.foo.app",
        ],
    );
    let ids: Vec<String> = find_receipts(&runner, &app)
        .into_iter()
        .map(|r| r.pkg_id)
        .collect();
    assert_eq!(
        ids,
        vec!["ABCDE12345.com.foo.app", "ABCDE12345.com.foo.app.installer"]
    );

    // Without a known team the team-prefixed form is not guessed.
    let (_fx2, no_team) = app_without_team();
    assert!(find_receipts(&runner, &no_team).is_empty());
}

fn app_without_team() -> (Fixture, AppInfo) {
    app("com.foo.app", None)
}

#[test]
fn never_returns_apple_receipts_and_never_calls_pkgutil_for_apple_apps() {
    let (_fx, apple) = app("com.apple.Foo", None);
    let runner = RecordingRunner::new();
    script_pkgs(&runner, &["com.apple.Foo", "com.apple.Foo.pkg"]);
    assert!(find_receipts(&runner, &apple).is_empty());
    assert!(runner.calls().is_empty());

    let (_fx, mut flagged) = app("com.foo.app", None);
    flagged.is_apple = true;
    assert!(find_receipts(&runner, &flagged).is_empty());
}

#[test]
fn ignores_unsafe_ids_and_tolerates_missing_info() {
    let (_fx, app) = app("com.foo.app", None);
    let runner = RecordingRunner::new();
    runner.respond_args(
        PKGUTIL,
        &["--pkgs"],
        out("com.foo.app.ok\ncom.foo.app.bad id\ncom.foo.app.;rm\n-com.foo.app\n"),
    );
    // No scripted plist: the receipt is still listed, with empty details.
    let receipts = find_receipts(&runner, &app);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].pkg_id, "com.foo.app.ok");
    assert_eq!(receipts[0].version, "");
    assert_eq!(receipts[0].install_location, "");
}

#[test]
fn pkgutil_failures_give_no_receipts() {
    let (_fx, app) = app("com.foo.app", None);
    let runner = RecordingRunner::new();
    runner.fail_to_start(PKGUTIL);
    assert!(find_receipts(&runner, &app).is_empty());

    let runner = RecordingRunner::new();
    runner.respond(
        PKGUTIL,
        CommandOutput {
            status: 1,
            stdout: "com.foo.app\n".into(),
            stderr: "boom".into(),
        },
    );
    assert!(find_receipts(&runner, &app).is_empty());

    // A receipt whose info cannot be read is skipped.
    let runner = RecordingRunner::new();
    runner.respond_args(PKGUTIL, &["--pkgs"], out("com.foo.app\n"));
    runner.respond_args(
        PKGUTIL,
        &["--pkg-info-plist", "com.foo.app"],
        CommandOutput {
            status: 1,
            ..CommandOutput::default()
        },
    );
    assert!(find_receipts(&runner, &app).is_empty());
}
