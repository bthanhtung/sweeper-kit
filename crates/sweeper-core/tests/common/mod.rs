#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sweeper_core::testing::{write_file, FakeSpotlight, Fixture};
use sweeper_core::{AppCatalog, AppInfo, LeftoverScanner, PathPolicy, RuleSet, ScanResult};

/// Installed apps of the fixture (no Spotlight data).
pub fn installed(fx: &Fixture) -> Vec<AppInfo> {
    AppCatalog::scan(&fx.env, &FakeSpotlight::default())
}

pub fn app_by_id(apps: &[AppInfo], bundle_id: &str) -> AppInfo {
    apps.iter()
        .find(|a| a.bundle_id == bundle_id)
        .unwrap_or_else(|| panic!("app {bundle_id} not installed"))
        .clone()
}

/// Scans leftovers of `bundle_id` with the bundled rules and a given policy.
pub fn scan_with(
    fx: &Fixture,
    bundle_id: &str,
    protected: &[String],
    spotlight: &FakeSpotlight,
) -> ScanResult {
    let apps = installed(fx);
    let app = app_by_id(&apps, bundle_id);
    let policy = PathPolicy::new(&fx.env, protected);
    let rules = RuleSet::bundled();
    LeftoverScanner::new(&fx.env, &rules, &policy, spotlight).scan(
        &app,
        &apps,
        &AtomicBool::new(false),
    )
}

pub fn scan(fx: &Fixture, bundle_id: &str) -> ScanResult {
    scan_with(fx, bundle_id, &[], &FakeSpotlight::default())
}

/// Paths of all items except the app bundle itself.
pub fn leftover_paths(scan: &ScanResult) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = scan
        .items()
        .filter(|i| i.path != scan.app.path)
        .map(|i| i.path.clone())
        .collect();
    v.sort();
    v
}

/// Paths relative to the fake home, for readable assertions.
pub fn rel(fx: &Fixture, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| {
            p.strip_prefix(&fx.env.home)
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect()
}

/// Creates a file (parents included) under the fake home.
pub fn touch(fx: &Fixture, rel_path: &str) -> PathBuf {
    let p = fx.env.home.join(rel_path);
    write_file(&p, 100);
    p
}

/// Creates a directory containing one file under the fake home.
pub fn touch_dir(fx: &Fixture, rel_path: &str) -> PathBuf {
    let p = fx.env.home.join(rel_path);
    write_file(&p.join("data.bin"), 100);
    p
}

pub fn outside_dir(fx: &Fixture) -> PathBuf {
    let p = fx.tmp.path().join("outside");
    let p = std::fs::create_dir_all(&p)
        .map(|_| std::fs::canonicalize(&p).unwrap())
        .unwrap();
    write_file(&p.join("precious.txt"), 10);
    p
}

pub fn is_inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}
