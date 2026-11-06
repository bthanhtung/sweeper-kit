//! Builds `sweeper-helper` for the target being compiled and stages it as the
//! Tauri sidecar `binaries/sweeper-helper-<target triple>`, which
//! `bundle.externalBin` ships at `Contents/MacOS/sweeper-helper`.
//!
//! The helper is built into its own target directory below `OUT_DIR`: the
//! outer cargo holds the lock on the workspace target directory while this
//! script runs, so a nested build there would wait for itself.
//!
//! For `--target universal-apple-darwin` Tauri expects
//! `binaries/sweeper-helper-universal-apple-darwin`; CI creates it with
//! `lipo` (see `.github/workflows/build.yml`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER: &str = "sweeper-helper";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../sweeper-helper/src");
    println!("cargo:rerun-if-changed=../sweeper-helper/Cargo.toml");
    println!("cargo:rerun-if-changed=../sweeper-core/src");
    println!("cargo:rerun-if-changed=../sweeper-core/Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    if let Err(e) = stage_helper() {
        panic!("could not build {HELPER}: {e}");
    }
    tauri_build::build()
}

fn stage_helper() -> Result<(), String> {
    let var = |name: &str| env::var(name).map_err(|_| format!("{name} is not set"));
    let target = var("TARGET")?;
    let release = var("PROFILE")? == "release";
    let manifest_dir = PathBuf::from(var("CARGO_MANIFEST_DIR")?);
    let helper_target_dir = PathBuf::from(var("OUT_DIR")?).join("helper-target");

    let mut cargo = Command::new(var("CARGO")?);
    cargo
        .args(["build", "--package", HELPER, "--target", &target])
        .arg("--manifest-path")
        .arg(manifest_dir.join("../../Cargo.toml"))
        .arg("--target-dir")
        .arg(&helper_target_dir);
    if release {
        cargo.arg("--release");
    }
    let status = cargo
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build failed ({status})"));
    }

    let built = helper_target_dir
        .join(&target)
        .join(if release { "release" } else { "debug" })
        .join(HELPER);
    let sidecar = manifest_dir
        .join("binaries")
        .join(format!("{HELPER}-{target}"));
    copy_if_changed(&built, &sidecar)
}

/// Copies `from` to `to` unless `to` already has the same bytes, so an
/// unchanged helper keeps its timestamp and does not retrigger bundling.
fn copy_if_changed(from: &Path, to: &Path) -> Result<(), String> {
    let bytes = fs::read(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    if fs::read(to).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let dir = to.parent().ok_or("sidecar path has no parent")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    fs::write(to, &bytes).map_err(|e| format!("cannot write {}: {e}", to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", to.display()))?;
    }
    Ok(())
}
