//! Finds data in `~/Library` that belongs to apps which are no longer
//! installed. Ownership is decided conservatively: anything that might still
//! belong to an installed or running app is left alone.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::catalog::AppInfo;
use crate::env::Env;
use crate::fsutil::tree_size;
use crate::history::RemovedApps;
use crate::path_policy::PathPolicy;
use crate::remover::{PlanItem, RemovalPlan};
use crate::rules::{Category, RulesError};
use crate::scanner::Item;
use crate::system::{is_valid_bundle_id, Processes, Spotlight};

const BUNDLED: &str = include_str!("../../../rules/macos-orphans.json");
const SUPPORTED_VERSION: u32 = 1;
/// Locations where a plain folder name (no identifier) is considered at all.
const NAME_LOCATIONS: [&str; 2] = ["application_support", "caches"];
const CONTAINERS_BASE: &str = "~/Library/Containers";
const CONTAINER_METADATA: &str = ".com.apple.containermanagerd.metadata.plist";

/// One `~/Library` folder searched for orphans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanLocation {
    pub id: String,
    #[serde(default = "default_category")]
    pub category: Category,
    /// Directory to list, always starting with `~/Library/`.
    pub base: String,
    /// Entry-name suffixes that are stripped to get the candidate key; an
    /// entry matching none of them is ignored.
    pub suffixes: Vec<String>,
}

fn default_category() -> Category {
    Category::AppData
}

/// A validated description of where to look and what never to touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRules {
    pub version: u32,
    pub locations: Vec<OrphanLocation>,
    /// Case-insensitive entry names that are never orphans.
    #[serde(default)]
    pub exclude_names: Vec<String>,
    /// Case-insensitive entry-name prefixes that are never orphans.
    #[serde(default)]
    pub exclude_prefixes: Vec<String>,
}

impl OrphanRules {
    /// The rules shipped with the crate.
    pub fn bundled() -> OrphanRules {
        OrphanRules::from_json(BUNDLED).expect("bundled rules/macos-orphans.json is valid")
    }

    /// Parses and validates a rule set.
    pub fn from_json(json: &str) -> Result<OrphanRules, RulesError> {
        let rules: OrphanRules =
            serde_json::from_str(json).map_err(|e| RulesError::Parse(e.to_string()))?;
        rules.validate()?;
        Ok(rules)
    }

    /// Reads and validates a user rules file.
    pub fn load(path: &Path) -> Result<OrphanRules, RulesError> {
        let text = std::fs::read_to_string(path).map_err(|e| RulesError::Io(e.to_string()))?;
        OrphanRules::from_json(&text)
    }

    /// Returns `self` overlaid with `user`: same location id replaces, new ids
    /// append, exclusions are added.
    pub fn merge(mut self, user: OrphanRules) -> OrphanRules {
        for loc in user.locations {
            match self.locations.iter_mut().find(|l| l.id == loc.id) {
                Some(slot) => *slot = loc,
                None => self.locations.push(loc),
            }
        }
        extend_unique(&mut self.exclude_names, user.exclude_names);
        extend_unique(&mut self.exclude_prefixes, user.exclude_prefixes);
        self
    }

    fn validate(&self) -> Result<(), RulesError> {
        if self.version != SUPPORTED_VERSION {
            return Err(RulesError::UnsupportedVersion(self.version));
        }
        let mut seen = HashSet::new();
        for loc in &self.locations {
            let bad = |reason: &str| RulesError::Invalid {
                id: loc.id.clone(),
                reason: reason.to_string(),
            };
            if loc.id.is_empty() || !seen.insert(loc.id.as_str()) {
                return Err(bad("id must be non-empty and unique"));
            }
            let rest = loc
                .base
                .strip_prefix("~/Library/")
                .ok_or_else(|| bad("base must start with ~/Library/"))?;
            if rest.is_empty()
                || Path::new(rest)
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(bad("base must not contain '..', '.' or empty segments"));
            }
            if loc.category == Category::AppBundle {
                return Err(bad("app_bundle is not a valid location category"));
            }
            if loc.suffixes.is_empty() {
                return Err(bad("suffixes must not be empty"));
            }
            if loc.suffixes.iter().any(|s| s.contains('/')) {
                return Err(bad("suffixes must not contain '/'"));
            }
        }
        for (id, list) in [
            ("exclude_names", &self.exclude_names),
            ("exclude_prefixes", &self.exclude_prefixes),
        ] {
            if list.iter().any(|s| s.is_empty() || s.contains('/')) {
                return Err(RulesError::Invalid {
                    id: id.to_string(),
                    reason: "entries must be non-empty and must not contain '/'".to_string(),
                });
            }
        }
        Ok(())
    }
}

fn extend_unique(list: &mut Vec<String>, extra: Vec<String>) {
    for s in extra {
        if !list.iter().any(|e| e.eq_ignore_ascii_case(&s)) {
            list.push(s);
        }
    }
}

/// How sure we are that a group is abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The app was uninstalled with Sweeper Kit.
    High,
    /// Identifier-shaped name that no installed or running app uses.
    Medium,
    /// Plain folder name that no installed app resembles; may be anything.
    Low,
}

/// All leftovers of one former app (or one unexplained folder name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanGroup {
    /// Bundle identifier or folder name.
    pub key: String,
    /// The app's name when the history knows it, else the key.
    pub display_name: String,
    pub confidence: Confidence,
    /// Ordered by path. Only [`Confidence::High`] items are ever checked.
    pub items: Vec<Item>,
    /// Combined size of every item, checked or not.
    pub size: u64,
}

/// Result of [`OrphanFinder::scan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanScan {
    /// High confidence first, then medium, then low; larger first within each.
    pub groups: Vec<OrphanGroup>,
    /// Locations that could not be read (path, reason).
    pub inaccessible: Vec<(PathBuf, String)>,
    pub total_size: u64,
}

impl OrphanScan {
    /// All items of all groups.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flat_map(|g| g.items.iter())
    }

    /// Combined size of the checked items.
    pub fn selected_size(&self) -> u64 {
        self.items()
            .filter(|i| i.checked)
            .map(|i| i.size)
            .fold(0, u64::saturating_add)
    }

    /// Builds the removal plan. `None` takes the checked items; `Some` takes
    /// exactly those paths that appear in this scan and were not refused, so
    /// unknown paths can never reach the remover.
    pub fn plan(&self, selected_paths: Option<&[PathBuf]>) -> RemovalPlan {
        let wanted: Option<HashSet<&PathBuf>> = selected_paths.map(|p| p.iter().collect());
        let items: Vec<PlanItem> = self
            .items()
            .filter(|i| i.refused.is_none())
            .filter(|i| match &wanted {
                None => i.checked,
                Some(set) => set.contains(&i.path),
            })
            .map(|i| PlanItem {
                path: i.path.clone(),
                size: i.size,
            })
            .collect();
        let total_size = items.iter().map(|i| i.size).fold(0, u64::saturating_add);
        RemovalPlan { items, total_size }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    BundleId,
    PlainName,
}

struct Candidate {
    path: PathBuf,
    key: String,
    kind: KeyKind,
    category: Category,
}

/// What the installed apps claim, in lowercase.
struct Owners {
    ids: HashSet<String>,
    /// Every proper `.`-prefix of an installed id (`com.foo` for `com.foo.app`).
    id_prefixes: HashSet<String>,
    names: HashSet<String>,
}

impl Owners {
    fn new(installed: &[AppInfo], self_id: &str) -> Owners {
        let mut owners = Owners {
            ids: HashSet::new(),
            id_prefixes: HashSet::new(),
            names: HashSet::new(),
        };
        for id in installed
            .iter()
            .map(|a| a.bundle_id.as_str())
            .chain([self_id])
            .filter(|id| !id.is_empty())
        {
            let id = id.to_lowercase();
            for (i, _) in id.match_indices('.') {
                owners.id_prefixes.insert(id[..i].to_string());
            }
            if let Some((_, last)) = id.rsplit_once('.') {
                owners.names.insert(last.to_string());
            }
            owners.ids.insert(id);
        }
        for app in installed {
            owners.names.insert(app.name.to_lowercase());
            if let Some(stem) = app.path.file_stem().and_then(|s| s.to_str()) {
                owners.names.insert(stem.to_lowercase());
            }
        }
        owners
    }

    /// Equal to, a child of, or a vendor prefix of an installed identifier,
    /// always at a `.` boundary.
    fn owns_id(&self, key: &str) -> bool {
        let key = key.to_lowercase();
        self.id_prefixes.contains(&key) || dot_prefixes(&key).any(|p| self.ids.contains(p))
    }

    fn owns_name(&self, key: &str) -> bool {
        self.names.contains(&key.to_lowercase())
    }
}

/// `a.b.c` yields `a`, `a.b` and `a.b.c`.
fn dot_prefixes(id: &str) -> impl Iterator<Item = &str> {
    id.match_indices('.')
        .map(|(i, _)| &id[..i])
        .chain(std::iter::once(id))
}

/// Finds leftovers of apps that are gone.
pub struct OrphanFinder<'a> {
    env: &'a Env,
    rules: &'a OrphanRules,
    policy: &'a PathPolicy,
    spotlight: &'a dyn Spotlight,
    processes: &'a dyn Processes,
    history: &'a RemovedApps,
    names: HashSet<String>,
    prefixes: Vec<String>,
}

impl<'a> OrphanFinder<'a> {
    pub fn new(
        env: &'a Env,
        rules: &'a OrphanRules,
        policy: &'a PathPolicy,
        spotlight: &'a dyn Spotlight,
        processes: &'a dyn Processes,
        history: &'a RemovedApps,
    ) -> Self {
        OrphanFinder {
            env,
            rules,
            policy,
            spotlight,
            processes,
            history,
            names: rules
                .exclude_names
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            prefixes: rules
                .exclude_prefixes
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
    }

    /// Lists orphaned data given the currently `installed` apps. Raising
    /// `cancel` stops the scan early and returns what was found so far.
    pub fn scan(&self, installed: &[AppInfo], cancel: &AtomicBool) -> OrphanScan {
        let owners = Owners::new(installed, &self.env.self_bundle_id);
        let mut inaccessible = Vec::new();
        let mut candidates = Vec::new();
        for loc in &self.rules.locations {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            self.collect(loc, &owners, &mut candidates, &mut inaccessible);
        }
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
        let candidates = self.drop_owned_by_activity(candidates, cancel);

        let history: HashMap<String, String> = self
            .history
            .entries()
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.bundle_id.to_lowercase(), e.name))
            .collect();
        let items: Vec<(Candidate, Item)> = candidates
            .into_par_iter()
            .filter(|_| !cancel.load(Ordering::Relaxed))
            .map(|c| {
                let known = known_app(&history, &c);
                let item = self.build_item(&c, known.is_some(), cancel);
                (c, item)
            })
            .collect();

        let groups = group_items(items, &history);
        let total_size = groups.iter().map(|g| g.size).fold(0, u64::saturating_add);
        OrphanScan {
            groups,
            inaccessible,
            total_size,
        }
    }

    fn collect(
        &self,
        loc: &OrphanLocation,
        owners: &Owners,
        out: &mut Vec<Candidate>,
        inaccessible: &mut Vec<(PathBuf, String)>,
    ) {
        let base = self.env.home.join(loc.base.trim_start_matches("~/"));
        let entries = match std::fs::read_dir(&base) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                inaccessible.push((base, describe_io_error(&e)));
                return;
            }
        };
        let plain_allowed = NAME_LOCATIONS.contains(&loc.id.as_str());
        let is_containers = loc.base == CONTAINERS_BASE;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(key) = strip_suffix(name, &loc.suffixes) else {
                continue;
            };
            if self.is_excluded(name, key) {
                continue;
            }
            let kind = if is_bundle_shaped(key) {
                KeyKind::BundleId
            } else if plain_allowed && !key.contains('.') {
                KeyKind::PlainName
            } else {
                continue;
            };
            let owned = match kind {
                KeyKind::BundleId => owners.owns_id(key),
                KeyKind::PlainName => owners.owns_name(key),
            };
            if owned || (is_containers && !container_is_third_party(&entry.path())) {
                continue;
            }
            out.push(Candidate {
                path: entry.path(),
                key: key.to_string(),
                kind,
                category: loc.category,
            });
        }
    }

    fn is_excluded(&self, entry: &str, key: &str) -> bool {
        let entry = entry.to_lowercase();
        self.names.contains(&entry)
            || self.names.contains(&key.to_lowercase())
            || self.prefixes.iter().any(|p| entry.starts_with(p.as_str()))
    }

    /// Drops identifier candidates whose app is running or still on disk
    /// elsewhere. Each distinct identifier is asked once, in parallel.
    fn drop_owned_by_activity(
        &self,
        candidates: Vec<Candidate>,
        cancel: &AtomicBool,
    ) -> Vec<Candidate> {
        let mut unique: HashMap<String, String> = HashMap::new();
        for c in candidates.iter().filter(|c| c.kind == KeyKind::BundleId) {
            unique
                .entry(c.key.to_lowercase())
                .or_insert_with(|| c.key.clone());
        }
        let active: HashSet<String> = unique
            .into_par_iter()
            .filter(|(_, id)| {
                !cancel.load(Ordering::Relaxed)
                    && (!self.processes.running(id).is_empty()
                        || !self.spotlight.copies_of(id).is_empty())
            })
            .map(|(lower, _)| lower)
            .collect();
        candidates
            .into_iter()
            .filter(|c| c.kind != KeyKind::BundleId || !active.contains(&c.key.to_lowercase()))
            .collect()
    }

    fn build_item(&self, c: &Candidate, in_history: bool, cancel: &AtomicBool) -> Item {
        let refused = self.policy.check(&c.path).err().map(|e| e.to_string());
        let reason = match (c.kind, in_history) {
            (KeyKind::BundleId, true) => "Left behind by an app you uninstalled with Sweeper Kit",
            (KeyKind::BundleId, false) => "No installed or running app uses this identifier",
            (KeyKind::PlainName, _) => {
                "No installed app has this name — it may belong to something else"
            }
        };
        Item {
            checked: in_history && c.kind == KeyKind::BundleId && refused.is_none(),
            path: c.path.clone(),
            size: tree_size(&c.path, cancel).unwrap_or(0),
            category: c.category,
            reason: reason.to_string(),
            refused,
            shared: false,
        }
    }
}

/// Name of the uninstalled app this candidate belongs to, if the history knows
/// it (exact identifier or a `.`-boundary child such as a helper).
fn known_app<'h>(history: &'h HashMap<String, String>, c: &Candidate) -> Option<&'h String> {
    if c.kind != KeyKind::BundleId {
        return None;
    }
    let key = c.key.to_lowercase();
    let found = dot_prefixes(&key).find_map(|p| history.get(p));
    found
}

fn group_items(
    items: Vec<(Candidate, Item)>,
    history: &HashMap<String, String>,
) -> Vec<OrphanGroup> {
    let mut by_key: HashMap<String, OrphanGroup> = HashMap::new();
    for (c, item) in items {
        let group = by_key.entry(c.key.to_lowercase()).or_insert_with(|| {
            let known = known_app(history, &c);
            let confidence = match (c.kind, known) {
                (KeyKind::BundleId, Some(_)) => Confidence::High,
                (KeyKind::BundleId, None) => Confidence::Medium,
                (KeyKind::PlainName, _) => Confidence::Low,
            };
            OrphanGroup {
                display_name: known.cloned().unwrap_or_else(|| c.key.clone()),
                key: c.key.clone(),
                confidence,
                items: Vec::new(),
                size: 0,
            }
        });
        group.size = group.size.saturating_add(item.size);
        group.items.push(item);
    }
    let mut groups: Vec<OrphanGroup> = by_key.into_values().collect();
    for g in &mut groups {
        g.items.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups.sort_by(|a, b| {
        a.confidence
            .cmp(&b.confidence)
            .then(b.size.cmp(&a.size))
            .then_with(|| a.key.cmp(&b.key))
    });
    groups
}

/// Entry name without the longest matching non-empty suffix, or unchanged if
/// the empty suffix is allowed. `None` when no suffix fits or nothing remains.
fn strip_suffix<'n>(name: &'n str, suffixes: &[String]) -> Option<&'n str> {
    let stripped = suffixes
        .iter()
        .filter(|s| !s.is_empty())
        .filter_map(|s| name.strip_suffix(s.as_str()))
        .min_by_key(|k| k.len())
        .or_else(|| suffixes.iter().any(String::is_empty).then_some(name));
    stripped.filter(|k| !k.is_empty())
}

/// `com.foo.app`-like: valid characters, at least two non-empty segments and
/// not just numbers (`1.0.2`).
fn is_bundle_shaped(key: &str) -> bool {
    is_valid_bundle_id(key)
        && key.split('.').count() >= 2
        && key.split('.').all(|s| !s.is_empty())
        && !key.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// False for containers owned by Apple, and also when the metadata cannot be
/// read or parsed, so an unclear case is never offered as an orphan.
fn container_is_third_party(dir: &Path) -> bool {
    let meta = dir.join(CONTAINER_METADATA);
    let bytes = match std::fs::read(&meta) {
        Ok(b) => b,
        Err(e) => return e.kind() == io::ErrorKind::NotFound,
    };
    let Ok(value) = plist::Value::from_reader(io::Cursor::new(bytes)) else {
        return false;
    };
    let id = value
        .as_dictionary()
        .and_then(|d| d.get("MCMMetadataIdentifier"))
        .and_then(|v| v.as_string());
    match id {
        Some(id) => !id.to_lowercase().starts_with("com.apple."),
        None => true,
    }
}

fn describe_io_error(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        "Permission denied — grant Full Disk Access to include this location".into()
    } else {
        e.to_string()
    }
}
