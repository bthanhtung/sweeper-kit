//! Installer package receipts (`pkgutil`) that belong to an app.
//!
//! Finding receipts only reads the receipt database and needs no privileges;
//! forgetting one (`pkgutil --forget`) needs administrator access and goes
//! through [`crate::admin::AdminOp::ForgetReceipt`].

use std::collections::BTreeSet;
use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::admin::{is_valid_pkg_id, PKGUTIL};
use crate::catalog::AppInfo;
use crate::system::{is_valid_bundle_id, CommandRunner};

/// An installer receipt that looks like it belongs to an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub pkg_id: String,
    /// Empty when `pkgutil` does not report one.
    pub version: String,
    /// Empty when `pkgutil` does not report one.
    pub install_location: String,
    /// Number of files the package installed, if `pkgutil` listed them.
    pub files_count: Option<usize>,
}

/// Receipts whose identifier is the app's bundle identifier or a child of it
/// (`<bundle id>.<anything>`), also with the developer's team id in front when
/// known. The match is on whole `.`-separated components, so `com.foo.app`
/// never matches `com.foo.application`. Apple receipts are never returned.
pub fn find_receipts(runner: &dyn CommandRunner, app: &AppInfo) -> Vec<Receipt> {
    let id = app.bundle_id.as_str();
    if app.is_apple || !is_valid_bundle_id(id) || id.to_ascii_lowercase().starts_with("com.apple.")
    {
        return Vec::new();
    }
    let mut prefixes = vec![id.to_string()];
    if let Some(team) = app.team_id.as_deref().filter(|t| is_valid_bundle_id(t)) {
        prefixes.push(format!("{team}.{id}"));
    }
    let Ok(listing) = runner.run(PKGUTIL, &["--pkgs"]) else {
        return Vec::new();
    };
    if !listing.success() {
        return Vec::new();
    }
    let matching: BTreeSet<&str> = listing
        .stdout
        .lines()
        .map(str::trim)
        .filter(|pkg| is_valid_pkg_id(pkg))
        .filter(|pkg| prefixes.iter().any(|p| is_same_or_child(pkg, p)))
        .collect();
    matching
        .into_iter()
        .filter_map(|pkg| describe(runner, pkg))
        .collect()
}

/// `id` equals `prefix` or continues it after a `.`.
fn is_same_or_child(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

fn describe(runner: &dyn CommandRunner, pkg_id: &str) -> Option<Receipt> {
    let info = runner
        .run(PKGUTIL, &["--pkg-info-plist", pkg_id])
        .ok()
        .filter(|o| o.success())?;
    let dict = plist::Value::from_reader(Cursor::new(info.stdout.as_bytes()))
        .ok()
        .and_then(plist::Value::into_dictionary);
    let text = |key: &str| {
        dict.as_ref()
            .and_then(|d| d.get(key))
            .and_then(|v| v.as_string())
            .unwrap_or_default()
            .to_string()
    };
    let files_count = runner
        .run(PKGUTIL, &["--files", pkg_id])
        .ok()
        .filter(|o| o.success())
        .map(|o| o.stdout.lines().filter(|l| !l.trim().is_empty()).count());
    Some(Receipt {
        pkg_id: pkg_id.to_string(),
        version: text("pkg-version"),
        install_location: text("install-location"),
        files_count,
    })
}
