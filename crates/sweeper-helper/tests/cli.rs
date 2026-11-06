//! Exit codes of the helper binary. The tests never rely on being root: when
//! they happen to run as root the manifests are invalid, so nothing is changed.

use std::process::{Command, Output};

fn helper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sweeper-helper"))
        .args(args)
        .output()
        .expect("run sweeper-helper")
}

#[test]
fn no_argument_is_invalid_input() {
    let out = helper(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn more_than_one_argument_is_invalid_input() {
    assert_eq!(helper(&["{}", "{}"]).status.code(), Some(2));
}

#[test]
fn refuses_to_run_without_root_and_rejects_garbage_with_it() {
    let out = helper(&["not json"]);
    let expected = if sweeper_core::is_root() { 2 } else { 3 };
    assert_eq!(out.status.code(), Some(expected));
    assert!(out.stdout.is_empty());
}
