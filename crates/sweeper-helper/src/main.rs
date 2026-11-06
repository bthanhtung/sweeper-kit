//! `sweeper-helper`: the only part of Sweeper Kit that runs as root.
//!
//! Usage: `sweeper-helper '<manifest json>'`, started by the app through
//! `osascript ... with administrator privileges` (see
//! `sweeper_core::admin_invocation`). The helper:
//!
//! * takes exactly one argument and reads nothing else: no environment
//!   variables, no `PATH`, no network, no standard input;
//! * refuses to run unless its effective user id is 0;
//! * validates the whole manifest against the system path policy before it
//!   changes anything, then executes the operations in order;
//! * prints one JSON `AdminResult` line on standard output.
//!
//! Exit codes: `0` the manifest was valid and was executed (failures of
//! single operations are in the result), `1` the result could not be
//! printed, `2` invalid input, `3` not running as root.
//!
//! Actions are logged (action and path only) to
//! `/Library/Logs/Sweeper Kit/helper.log`.

use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use sweeper_core::admin::{LAUNCHCTL, PKGUTIL};
use sweeper_core::{
    execute_manifest, is_root, parse_manifest, validate_manifest, CommandOutput, CommandRunner,
    Env, SystemPolicy, SELF_BUNDLE_ID,
};

const EXIT_FAILED: u8 = 1;
const EXIT_INVALID: u8 = 2;
const EXIT_NOT_ROOT: u8 = 3;

/// Runs the two programs the helper needs by absolute path, with an empty
/// environment and no standard input. Any other program is refused.
struct AbsoluteRunner;

impl CommandRunner for AbsoluteRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        if program != LAUNCHCTL && program != PKGUTIL {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "program is not allowed",
            ));
        }
        let out = Command::new(program)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .output()?;
        Ok(CommandOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// The environment of the real machine, built without reading `$HOME`.
fn system_env() -> Env {
    let nowhere = PathBuf::from("/var/empty");
    Env {
        home: nowhere.clone(),
        app_dirs: Vec::new(),
        caskroom_dirs: Vec::new(),
        uid: 0,
        self_bundle_id: SELF_BUNDLE_ID.to_string(),
        data_dir: nowhere,
        system_root: PathBuf::from("/"),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(arg), None) = (args.next(), args.next()) else {
        eprintln!("usage: sweeper-helper <manifest-json>");
        return ExitCode::from(EXIT_INVALID);
    };
    if !is_root() {
        eprintln!("sweeper-helper must run as root");
        return ExitCode::from(EXIT_NOT_ROOT);
    }
    let Some(json) = arg.to_str() else {
        eprintln!("the manifest is not valid UTF-8");
        return ExitCode::from(EXIT_INVALID);
    };
    let env = system_env();
    let policy = SystemPolicy::new(&env, &[]);
    let manifest =
        match parse_manifest(json).and_then(|m| validate_manifest(&m, &policy).map(|_| m)) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_INVALID);
            }
        };
    let result = execute_manifest(&manifest, &policy, &env.system_root, &AbsoluteRunner);
    match serde_json::to_string(&result) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot print the result: {e}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_refuses_other_programs() {
        for program in ["rm", "/bin/rm", "launchctl", "/usr/bin/osascript"] {
            let err = AbsoluteRunner.run(program, &[]).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{program}");
        }
    }

    #[test]
    fn system_env_does_not_depend_on_home() {
        let env = system_env();
        assert_eq!(env.system_root, PathBuf::from("/"));
        assert_eq!(env.uid, 0);
    }
}
