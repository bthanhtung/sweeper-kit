//! Finds the files an app leaves behind in the user's Library.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::admin::bundle_needs_admin;
use crate::catalog::AppInfo;
use crate::env::Env;
use crate::fsutil::tree_size;
use crate::path_policy::{PathPolicy, SystemPolicy};
use crate::rules::{AppIdent, Category, Location, MatchBy, RuleSet, Scope};
use crate::system::Spotlight;

const APPLE_REASON: &str = "Apple app — read only";
const SYSTEM_NAME_NOTE: &str = " matches the app name only — not selected automatically";
const TEAM_REASON: &str = "team container — may be shared with other apps from this developer";

/// One removable file or folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub path: PathBuf,
    pub size: u64,
    pub checked: bool,
    pub category: Category,
    /// Why this item was attributed to the app.
    pub reason: String,
    /// Set when the path policy forbids removal; such items are never checked.
    pub refused: Option<String>,
    /// Entry that may belong to something else: a team-wide entry that other
    /// apps from the same developer may need, or a system-scope entry matched
    /// by app name only. Never checked by a preset.
    pub shared: bool,
    /// Removing the item needs administrator access: it lives in `/Library`,
    /// or its bundle sits where the current user cannot write.
    #[serde(default)]
    pub requires_admin: bool,
}

/// Items of one category, ordered by path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub category: Category,
    pub items: Vec<Item>,
}

/// Everything found for one app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanResult {
    pub app: AppInfo,
    /// App bundle first, then leftovers in a fixed category order.
    pub groups: Vec<Group>,
    /// Other bundles with the same identifier; never checked by default.
    pub other_copies: Vec<Item>,
    /// Locations that could not be read (path, reason).
    pub inaccessible: Vec<(PathBuf, String)>,
    /// Combined size of every item in `groups`, checked or not.
    pub total_size: u64,
}

impl ScanResult {
    /// All items of all groups.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|g| g.items.iter())
    }

    /// Combined size of the checked items in `groups`.
    pub fn selected_size(&self) -> u64 {
        self.items().filter(|i| i.checked).map(|i| i.size).sum()
    }
}

/// Quick selections for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    AppOnly,
    AppAndCaches,
    Everything,
}

/// Re-checks items according to `preset`. Refused, shared and other-copy items
/// are never checked.
pub fn apply_preset(result: &mut ScanResult, preset: Preset) {
    for group in &mut result.groups {
        let wanted = match preset {
            Preset::AppOnly => group.category == Category::AppBundle,
            Preset::AppAndCaches => {
                matches!(group.category, Category::AppBundle | Category::Caches)
            }
            Preset::Everything => true,
        };
        for item in &mut group.items {
            item.checked = wanted && item.refused.is_none() && !item.shared;
        }
    }
    for item in &mut result.other_copies {
        item.checked = false;
    }
}

const CATEGORY_ORDER: [Category; 7] = [
    Category::AppBundle,
    Category::AppData,
    Category::Preferences,
    Category::Caches,
    Category::CookiesState,
    Category::Logs,
    Category::BackgroundItems,
];

struct Candidate {
    path: PathBuf,
    category: Category,
    reason: String,
    default_checked: bool,
    shared: bool,
    /// Found in a system-scope location.
    system: bool,
    requires_admin: bool,
}

/// Scans the Library locations described by a [`RuleSet`].
pub struct LeftoverScanner<'a> {
    env: &'a Env,
    rules: &'a RuleSet,
    policy: &'a PathPolicy,
    system_policy: SystemPolicy,
    spotlight: &'a dyn Spotlight,
}

impl<'a> LeftoverScanner<'a> {
    pub fn new(
        env: &'a Env,
        rules: &'a RuleSet,
        policy: &'a PathPolicy,
        spotlight: &'a dyn Spotlight,
    ) -> Self {
        LeftoverScanner {
            env,
            rules,
            policy,
            system_policy: SystemPolicy::new(env, &[]),
            spotlight,
        }
    }

    /// Uses `policy` for system-scope items instead of the default one, which
    /// has no protected paths or bundle identifiers.
    pub fn with_system_policy(mut self, policy: SystemPolicy) -> Self {
        self.system_policy = policy;
        self
    }

    /// Finds leftovers of `app`. `installed` is used to avoid claiming files
    /// that belong to a different installed app. Raising `cancel` stops the
    /// scan early and returns what was found so far.
    pub fn scan(&self, app: &AppInfo, installed: &[AppInfo], cancel: &AtomicBool) -> ScanResult {
        let ident = ident_of(app);
        let others: Vec<&AppInfo> = installed
            .iter()
            .filter(|o| o.bundle_id != app.bundle_id)
            .collect();
        let mut inaccessible = Vec::new();
        // The bundle is listed only while it exists, so a rescan after
        // removal does not report it as remaining.
        let mut candidates = Vec::new();
        if std::fs::symlink_metadata(&app.path).is_ok() {
            candidates.push(Candidate {
                path: app.path.clone(),
                category: Category::AppBundle,
                reason: "Application bundle".into(),
                default_checked: true,
                shared: false,
                system: false,
                requires_admin: bundle_needs_admin(&app.path),
            });
        }

        for loc in &self.rules.locations {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            self.collect(loc, &ident, &others, &mut candidates, &mut inaccessible);
        }

        let mut items = self.build_items(candidates, app.is_apple, cancel);
        let other_copies = self.other_copies(app, cancel);

        let mut groups = Vec::new();
        for category in CATEGORY_ORDER {
            let mut in_group: Vec<Item> = Vec::new();
            let mut rest = Vec::new();
            for item in items.drain(..) {
                if item.category == category {
                    in_group.push(item);
                } else {
                    rest.push(item);
                }
            }
            items = rest;
            if category != Category::AppBundle {
                in_group.sort_by(|a, b| a.path.cmp(&b.path));
            }
            if !in_group.is_empty() {
                groups.push(Group {
                    category,
                    items: in_group,
                });
            }
        }
        let total_size = groups
            .iter()
            .flat_map(|g| &g.items)
            .map(|i| i.size)
            .fold(0u64, u64::saturating_add);
        ScanResult {
            app: app.clone(),
            groups,
            other_copies,
            inaccessible,
            total_size,
        }
    }

    fn collect(
        &self,
        loc: &Location,
        ident: &AppIdent<'_>,
        others: &[&AppInfo],
        out: &mut Vec<Candidate>,
        inaccessible: &mut Vec<(PathBuf, String)>,
    ) {
        let system = loc.scope == Scope::System;
        let base = loc.resolve(self.env);
        let entries = match std::fs::read_dir(&base) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                inaccessible.push((base, describe_io_error(&e)));
                return;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(by) = loc.match_entry(name, ident) else {
                continue;
            };
            if belongs_to_other(loc, name, by, ident, others) {
                continue;
            }
            let (reason, shared) = match by {
                MatchBy::Id => (format!("Matches bundle identifier in {}", loc.base), false),
                // Machine-wide folders are shared by every user, so a match
                // on the name alone is never selected automatically.
                MatchBy::Name if system => (format!("{}{SYSTEM_NAME_NOTE}", loc.base), true),
                MatchBy::Name => (format!("Matches app name in {}", loc.base), false),
                MatchBy::Team => (TEAM_REASON.to_string(), true),
            };
            out.push(Candidate {
                path: entry.path(),
                category: loc.category,
                reason,
                default_checked: loc.default_checked && !shared,
                shared,
                system,
                requires_admin: system,
            });
        }
    }

    /// Applies the policy, sizes the items in parallel and fixes check state.
    fn build_items(
        &self,
        candidates: Vec<Candidate>,
        is_apple: bool,
        cancel: &AtomicBool,
    ) -> Vec<Item> {
        candidates
            .into_par_iter()
            .map(|c| {
                let refused = if is_apple {
                    Some(APPLE_REASON.to_string())
                } else {
                    self.refusal(&c)
                };
                let size = tree_size(&c.path, cancel).unwrap_or(0);
                Item {
                    checked: c.default_checked && refused.is_none(),
                    path: c.path,
                    size,
                    category: c.category,
                    reason: c.reason,
                    refused,
                    shared: c.shared,
                    requires_admin: c.requires_admin,
                }
            })
            .collect()
    }

    /// Why the policy forbids removing `c`. System-scope items, and bundles
    /// that need administrator access, must pass [`SystemPolicy`] because the
    /// privileged helper applies it again before touching them.
    fn refusal(&self, c: &Candidate) -> Option<String> {
        if c.system {
            return self
                .system_policy
                .check(&c.path)
                .err()
                .map(|e| e.to_string());
        }
        let user = self.policy.check(&c.path).err();
        if user.is_none() && c.requires_admin {
            return self
                .system_policy
                .check(&c.path)
                .err()
                .map(|e| e.to_string());
        }
        user.map(|e| e.to_string())
    }

    fn other_copies(&self, app: &AppInfo, cancel: &AtomicBool) -> Vec<Item> {
        let own = std::fs::canonicalize(&app.path).unwrap_or_else(|_| app.path.clone());
        let mut seen = vec![own];
        let mut paths = Vec::new();
        for p in self.spotlight.copies_of(&app.bundle_id) {
            let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if !seen.contains(&key) {
                seen.push(key);
                paths.push(p);
            }
        }
        let candidates = paths
            .into_iter()
            .map(|path| Candidate {
                requires_admin: bundle_needs_admin(&path),
                path,
                category: Category::AppBundle,
                reason: "Another copy of this app".into(),
                default_checked: false,
                shared: false,
                system: false,
            })
            .collect();
        self.build_items(candidates, app.is_apple, cancel)
    }
}

fn ident_of(app: &AppInfo) -> AppIdent<'_> {
    AppIdent {
        bundle_id: &app.bundle_id,
        name: &app.name,
        team_id: app.team_id.as_deref(),
    }
}

/// True if a different installed app claims the entry more specifically: a
/// longer matching identifier, or the same exact name when we matched by name.
fn belongs_to_other(
    loc: &Location,
    entry: &str,
    ours: MatchBy,
    ident: &AppIdent<'_>,
    others: &[&AppInfo],
) -> bool {
    others
        .iter()
        .any(|o| match loc.match_entry(entry, &ident_of(o)) {
            Some(MatchBy::Id) => o.bundle_id.len() > ident.bundle_id.len(),
            Some(MatchBy::Name) => ours == MatchBy::Name,
            // Team-wide entries are shared, so a sibling does not claim them.
            Some(MatchBy::Team) | None => false,
        })
}

fn describe_io_error(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        "Permission denied — grant Full Disk Access to include this location".into()
    } else {
        e.to_string()
    }
}
