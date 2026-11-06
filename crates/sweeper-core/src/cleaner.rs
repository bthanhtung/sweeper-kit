//! Cache and junk cleaner: finds regenerable files (caches, logs, old
//! downloads, developer leftovers) from data-driven rules and removes the
//! selected ones through the same policy-checked pipeline as the uninstaller.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::catalog::AppInfo;
use crate::env::Env;
use crate::fsutil::tree_size;
use crate::log::ActionLog;
use crate::path_policy::{PathPolicy, LIBRARY_ROOT_DIRS};
use crate::remover::{PlanItem, Progress, RemovalMode, RemovalPlan, Removed, Remover, UndoLog};
use crate::system::{is_valid_bundle_id, CommandRunner, Processes, Trasher};
use crate::uninstall::StepResult;

const BUNDLED: &str = include_str!("../../../rules/macos-cleaner.json");
const SUPPORTED_VERSION: u32 = 1;

/// Final path segments that hold personal browser data (compared
/// case-insensitively). No rule may target them, and the scanner never lists
/// them, whatever a wildcard matches.
const PERSONAL_DATA_NAMES: [&str; 14] = [
    "history",
    "cookies",
    "login data",
    "web data",
    "bookmarks",
    "favicons",
    "places.sqlite",
    "cookies.sqlite",
    "key4.db",
    "logins.json",
    "formhistory.sqlite",
    "top sites",
    "preferences",
    "local state",
];

/// Housekeeping files that are never worth listing.
const IGNORED_CHILDREN: [&str; 2] = [".DS_Store", ".localized"];

const REAL_TOOLS: RealToolLocator = RealToolLocator;

fn is_personal_data(name: &str) -> bool {
    let lower = name.to_lowercase();
    PERSONAL_DATA_NAMES.contains(&lower.as_str())
}

/// External tools the cleaner can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Xcrun,
    Brew,
    Docker,
}

impl Tool {
    /// Fixed absolute locations; `PATH` is never consulted.
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Tool::Xcrun => &["/usr/bin/xcrun"],
            Tool::Brew => &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"],
            Tool::Docker => &[
                "/usr/local/bin/docker",
                "/opt/homebrew/bin/docker",
                "/Applications/Docker.app/Contents/Resources/bin/docker",
            ],
        }
    }
}

/// Finds installed tools.
pub trait ToolLocator: Send + Sync {
    fn find(&self, tool: Tool) -> Option<PathBuf>;
}

/// Looks for tools at their fixed absolute paths.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealToolLocator;

impl ToolLocator for RealToolLocator {
    fn find(&self, tool: Tool) -> Option<PathBuf> {
        tool.candidates()
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }
}

/// A command run instead of deleting files. The argument list is fixed here
/// and never built from rule data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanAction {
    SimctlDeleteUnavailable,
    BrewCleanup,
    DockerPrune,
}

impl CleanAction {
    pub fn tool(self) -> Tool {
        match self {
            CleanAction::SimctlDeleteUnavailable => Tool::Xcrun,
            CleanAction::BrewCleanup => Tool::Brew,
            CleanAction::DockerPrune => Tool::Docker,
        }
    }

    /// Arguments after the program.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            CleanAction::SimctlDeleteUnavailable => &["simctl", "delete", "unavailable"],
            CleanAction::BrewCleanup => &["cleanup", "-s"],
            CleanAction::DockerPrune => &["system", "prune", "-f"],
        }
    }

    /// Full command line for the tool found at `program`.
    pub fn argv(self, program: &Path) -> Vec<String> {
        let mut argv = vec![program.to_string_lossy().into_owned()];
        argv.extend(self.args().iter().map(|a| a.to_string()));
        argv
    }

    /// Human-readable name for reports.
    pub fn label(self) -> &'static str {
        match self {
            CleanAction::SimctlDeleteUnavailable => "Delete unavailable simulators",
            CleanAction::BrewCleanup => "Homebrew cleanup",
            CleanAction::DockerPrune => "Docker prune",
        }
    }

    fn id(self) -> &'static str {
        match self {
            CleanAction::SimctlDeleteUnavailable => "simctl_delete_unavailable",
            CleanAction::BrewCleanup => "brew_cleanup",
            CleanAction::DockerPrune => "docker_prune",
        }
    }
}

/// How an entry is removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanMode {
    /// Move to the Trash (undoable).
    #[default]
    Trash,
    /// Delete permanently; only allowed for items already in the Trash.
    DeleteOnly,
}

/// Failure to load or validate cleaner rules.
#[derive(Debug, thiserror::Error)]
pub enum CleanRulesError {
    #[error("rules file is not valid: {0}")]
    Parse(String),
    #[error("unsupported rules version {0}")]
    UnsupportedVersion(u32),
    #[error("rule '{id}' is invalid: {reason}")]
    Invalid { id: String, reason: String },
    #[error("cannot read rules file: {0}")]
    Io(String),
}

/// One thing the cleaner can remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanItemRule {
    pub id: String,
    pub title: String,
    /// App whose running state blocks cleaning.
    pub app_bundle_id: Option<String>,
    /// Globs starting with `~/`; `*` matches within one path segment.
    #[serde(default)]
    pub paths: Vec<String>,
    /// List each child of the matched directories instead of the match itself.
    #[serde(default)]
    pub each_child: bool,
    /// Child names never listed with `each_child`.
    #[serde(default)]
    pub exclude_children: Vec<String>,
    #[serde(default)]
    pub default_checked: bool,
    /// Only entries not modified for this long; the cleaner options override
    /// the number.
    pub min_age_days: Option<u32>,
    /// Command to run instead of removing paths.
    pub action: Option<CleanAction>,
    #[serde(default)]
    pub mode: CleanMode,
}

/// A titled group of items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanCategoryRule {
    pub id: String,
    pub title: String,
    pub items: Vec<CleanItemRule>,
}

/// A validated list of cleaner rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanRules {
    pub version: u32,
    pub categories: Vec<CleanCategoryRule>,
}

impl CleanRules {
    /// The rules shipped with the crate.
    pub fn bundled() -> CleanRules {
        CleanRules::from_json(BUNDLED).expect("bundled rules/macos-cleaner.json is valid")
    }

    /// Parses and validates a rule set.
    pub fn from_json(json: &str) -> Result<CleanRules, CleanRulesError> {
        let rules: CleanRules =
            serde_json::from_str(json).map_err(|e| CleanRulesError::Parse(e.to_string()))?;
        rules.validate()?;
        Ok(rules)
    }

    /// Reads and validates a user rules file.
    pub fn load(path: &Path) -> Result<CleanRules, CleanRulesError> {
        let text = std::fs::read_to_string(path).map_err(|e| CleanRulesError::Io(e.to_string()))?;
        CleanRules::from_json(&text)
    }

    /// Returns `self` overlaid with `user`: same ids replace, new ids append.
    pub fn merge(mut self, user: CleanRules) -> CleanRules {
        for category in user.categories {
            match self.categories.iter_mut().find(|c| c.id == category.id) {
                Some(slot) => {
                    slot.title = category.title;
                    for item in category.items {
                        match slot.items.iter_mut().find(|i| i.id == item.id) {
                            Some(existing) => *existing = item,
                            None => slot.items.push(item),
                        }
                    }
                }
                None => self.categories.push(category),
            }
        }
        self
    }

    fn validate(&self) -> Result<(), CleanRulesError> {
        if self.version != SUPPORTED_VERSION {
            return Err(CleanRulesError::UnsupportedVersion(self.version));
        }
        let mut categories = HashSet::new();
        for category in &self.categories {
            let invalid = |reason: &str| CleanRulesError::Invalid {
                id: category.id.clone(),
                reason: reason.to_string(),
            };
            if !valid_id(&category.id) || !categories.insert(category.id.as_str()) {
                return Err(invalid("id must be unique, non-empty and contain no '/'"));
            }
            if category.title.is_empty() {
                return Err(invalid("title must not be empty"));
            }
            let mut items = HashSet::new();
            for item in &category.items {
                let id = format!("{}/{}", category.id, item.id);
                validate_item(item).map_err(|reason| CleanRulesError::Invalid {
                    id: id.clone(),
                    reason,
                })?;
                if !items.insert(item.id.as_str()) {
                    return Err(CleanRulesError::Invalid {
                        id,
                        reason: "id is used twice in the category".into(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/')
}

fn validate_item(item: &CleanItemRule) -> Result<(), String> {
    if !valid_id(&item.id) {
        return Err("id must be non-empty and contain no '/'".into());
    }
    if item.title.is_empty() {
        return Err("title must not be empty".into());
    }
    if let Some(id) = &item.app_bundle_id {
        if !is_valid_bundle_id(id) {
            return Err("app_bundle_id is not a valid bundle identifier".into());
        }
    }
    if item
        .exclude_children
        .iter()
        .any(|c| c.is_empty() || c.contains('/'))
    {
        return Err("exclude_children must be plain names".into());
    }
    if item.action.is_some() {
        if !item.paths.is_empty() || item.each_child || item.min_age_days.is_some() {
            return Err("an item with an action has no paths".into());
        }
        if item.mode != CleanMode::Trash {
            return Err("an item with an action has no mode".into());
        }
        return Ok(());
    }
    if item.paths.is_empty() {
        return Err("paths must not be empty".into());
    }
    for pattern in &item.paths {
        validate_pattern(pattern).map_err(|reason| format!("path '{pattern}': {reason}"))?;
        if item.mode == CleanMode::DeleteOnly
            && pattern != "~/.Trash"
            && !pattern.starts_with("~/.Trash/")
        {
            return Err("delete_only is only allowed inside ~/.Trash".into());
        }
    }
    Ok(())
}

/// Checks the glob grammar: `~/` prefix, plain segments, `*` inside a segment.
fn validate_pattern(pattern: &str) -> Result<(), &'static str> {
    let rest = pattern.strip_prefix("~/").ok_or("must start with ~/")?;
    let mut last = "";
    for segment in rest.split('/') {
        if segment.is_empty() {
            return Err("must not contain empty segments");
        }
        if segment == "." || segment == ".." {
            return Err("must not contain '.' or '..'");
        }
        if segment.contains("**") {
            return Err("'**' is not supported");
        }
        if segment.contains(['?', '[', ']', '{', '}', '\\']) {
            return Err("only '*' is supported as a wildcard");
        }
        last = segment;
    }
    if is_personal_data(last) {
        return Err("targets personal browser data");
    }
    Ok(())
}

/// Number of leading segments without a wildcard, and the total.
fn static_segments(pattern: &str) -> (usize, usize) {
    let rest = pattern.strip_prefix("~/").unwrap_or(pattern);
    let total = rest.split('/').count();
    let fixed = rest.split('/').take_while(|s| !s.contains('*')).count();
    (fixed, total)
}

/// Matches one path segment against a pattern whose only wildcard is `*`.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Folders the cleaner needs beyond the normal removable roots, for
/// [`PathPolicy::with_extra_roots`]. For a wildcard rule this is the fixed
/// prefix before the first `*`; for a rule naming one folder it is the
/// parent; with `each_child` it is the folder itself.
pub fn cleaner_roots(env: &Env, rules: &CleanRules) -> Vec<PathBuf> {
    let lib = env.library();
    let normal: Vec<PathBuf> = LIBRARY_ROOT_DIRS
        .iter()
        .map(|d| lib.join(d))
        .chain(env.app_dirs.iter().cloned())
        .collect();
    let mut roots: Vec<PathBuf> = Vec::new();
    for item in rules.categories.iter().flat_map(|c| &c.items) {
        for pattern in &item.paths {
            if validate_pattern(pattern).is_err() {
                continue;
            }
            let rest = pattern.strip_prefix("~/").unwrap_or(pattern);
            let (fixed, total) = static_segments(pattern);
            let keep = if fixed == total && !item.each_child {
                fixed.saturating_sub(1)
            } else {
                fixed
            };
            let root = rest
                .split('/')
                .take(keep)
                .fold(env.home.clone(), |acc, s| acc.join(s));
            if !normal.iter().any(|n| root.starts_with(n)) && !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

/// User-tunable scan settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanOptions {
    /// Age threshold for items with `min_age_days` (old downloads).
    pub downloads_age_days: u32,
    /// Entries smaller than this many bytes are not listed (actions excepted).
    pub min_size: u64,
}

impl Default for CleanOptions {
    fn default() -> Self {
        CleanOptions {
            downloads_age_days: 30,
            min_size: 0,
        }
    }
}

/// One selectable line: a file or folder, or an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanEntry {
    /// Stable across scans: `category/item/path` (`category/item` for actions).
    pub id: String,
    pub title: String,
    pub path: Option<PathBuf>,
    pub action: Option<CleanAction>,
    /// `None` when the size is unknown (e.g. Docker not answering).
    pub size: Option<u64>,
    pub checked: bool,
    /// Why the entry is unchecked although its rule is on by default.
    pub warning: Option<String>,
    /// Set when the path policy forbids removal; such entries are never checked.
    pub refused: Option<String>,
    pub mode: CleanMode,
}

/// Entries of one rule category, largest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanCategory {
    pub id: String,
    pub title: String,
    pub entries: Vec<CleanEntry>,
    pub size: u64,
}

/// Everything the scan found. Categories without entries are omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanScan {
    pub categories: Vec<CleanCategory>,
    /// Locations that could not be read (path, reason).
    pub inaccessible: Vec<(PathBuf, String)>,
    pub total_size: u64,
}

impl CleanScan {
    /// All entries of all categories.
    pub fn entries(&self) -> impl Iterator<Item = &CleanEntry> {
        self.categories.iter().flat_map(|c| c.entries.iter())
    }
}

/// An entry before sizing and the policy check.
struct Pending {
    category: usize,
    id: String,
    title: String,
    path: Option<PathBuf>,
    action: Option<CleanAction>,
    size: Option<u64>,
    checked: bool,
    warning: Option<String>,
    mode: CleanMode,
}

/// Mutable state of one scan.
struct ScanState<'a> {
    apps: HashMap<&'a str, &'a AppInfo>,
    seen: HashSet<PathBuf>,
    /// Bundle id -> display name while running.
    running: HashMap<String, Option<String>>,
    inaccessible: Vec<(PathBuf, String)>,
    pending: Vec<Pending>,
}

/// Scans the locations described by [`CleanRules`].
pub struct CacheScanner<'a> {
    env: &'a Env,
    rules: &'a CleanRules,
    policy: &'a PathPolicy,
    processes: &'a dyn Processes,
    tools: &'a dyn ToolLocator,
    runner: Option<&'a dyn CommandRunner>,
}

impl<'a> CacheScanner<'a> {
    pub fn new(
        env: &'a Env,
        rules: &'a CleanRules,
        policy: &'a PathPolicy,
        processes: &'a dyn Processes,
        tools: &'a dyn ToolLocator,
    ) -> Self {
        CacheScanner {
            env,
            rules,
            policy,
            processes,
            tools,
            runner: None,
        }
    }

    /// Lets the scan ask Docker how much space `prune` would free.
    pub fn with_runner(mut self, runner: &'a dyn CommandRunner) -> Self {
        self.runner = Some(runner);
        self
    }

    /// Lists cleanable entries. `installed` supplies display names for cache
    /// folders named after a bundle id. Raising `cancel` stops early and
    /// returns what was found so far.
    pub fn scan(
        &self,
        installed: &[AppInfo],
        options: &CleanOptions,
        cancel: &AtomicBool,
    ) -> CleanScan {
        let mut state = ScanState {
            apps: installed
                .iter()
                .map(|a| (a.bundle_id.as_str(), a))
                .collect(),
            seen: HashSet::new(),
            running: HashMap::new(),
            inaccessible: Vec::new(),
            pending: Vec::new(),
        };
        'rules: for (index, category) in self.rules.categories.iter().enumerate() {
            for item in &category.items {
                if cancel.load(Ordering::Relaxed) {
                    break 'rules;
                }
                match item.action {
                    Some(action) => {
                        self.collect_action(index, &category.id, item, action, &mut state)
                    }
                    None => self.collect_paths(index, &category.id, item, options, &mut state),
                }
            }
        }

        let policy = self.policy;
        let sized: Vec<(usize, CleanEntry)> = std::mem::take(&mut state.pending)
            .into_par_iter()
            .map(|p| (p.category, finish(p, policy, cancel)))
            .collect();

        let mut categories: Vec<CleanCategory> = self
            .rules
            .categories
            .iter()
            .map(|c| CleanCategory {
                id: c.id.clone(),
                title: c.title.clone(),
                entries: Vec::new(),
                size: 0,
            })
            .collect();
        for (index, entry) in sized {
            let too_small = entry.action.is_none() && entry.size.unwrap_or(0) < options.min_size;
            if !too_small {
                categories[index].entries.push(entry);
            }
        }
        categories.retain(|c| !c.entries.is_empty());
        for category in &mut categories {
            category.entries.sort_by(|a, b| {
                b.size
                    .cmp(&a.size)
                    .then_with(|| a.title.cmp(&b.title))
                    .then_with(|| a.id.cmp(&b.id))
            });
            category.size = category
                .entries
                .iter()
                .map(|e| e.size.unwrap_or(0))
                .fold(0, u64::saturating_add);
        }
        let total_size = categories
            .iter()
            .map(|c| c.size)
            .fold(0, u64::saturating_add);
        let mut inaccessible = state.inaccessible;
        inaccessible.sort();
        inaccessible.dedup();
        CleanScan {
            categories,
            inaccessible,
            total_size,
        }
    }

    fn collect_action(
        &self,
        category: usize,
        category_id: &str,
        item: &CleanItemRule,
        action: CleanAction,
        state: &mut ScanState<'_>,
    ) {
        let Some(program) = self.tools.find(action.tool()) else {
            return;
        };
        let size = match action {
            CleanAction::DockerPrune => self.docker_reclaimable(&program),
            _ => None,
        };
        let warning = self.running_warning(item, state);
        state.pending.push(Pending {
            category,
            id: format!("{category_id}/{}", item.id),
            title: item.title.clone(),
            path: None,
            action: Some(action),
            size,
            checked: item.default_checked && warning.is_none(),
            warning,
            mode: CleanMode::Trash,
        });
    }

    fn collect_paths(
        &self,
        category: usize,
        category_id: &str,
        item: &CleanItemRule,
        options: &CleanOptions,
        state: &mut ScanState<'_>,
    ) {
        let item_warning = self.running_warning(item, state);
        let max_age = item
            .min_age_days
            .map(|_| Duration::from_secs(u64::from(options.downloads_age_days) * 86_400));
        let now = SystemTime::now();

        for pattern in &item.paths {
            if validate_pattern(pattern).is_err() {
                continue;
            }
            let (fixed, total) = static_segments(pattern);
            for matched in self.expand(pattern, state) {
                let mut found: Vec<(PathBuf, String, Option<String>, bool)> = Vec::new();
                if item.each_child {
                    for (name, path) in self.children(&matched, item, state) {
                        let title = state
                            .apps
                            .get(name.as_str())
                            .map_or_else(|| name.clone(), |a| a.name.clone());
                        let warning = self.child_warning(&name, &title, state);
                        let apple = name.starts_with("com.apple.");
                        found.push((path, title, warning, !apple));
                    }
                } else {
                    let title = if fixed < total {
                        let label: PathBuf = matched
                            .strip_prefix(&self.env.home)
                            .map(|r| r.components().skip(fixed).collect())
                            .unwrap_or_default();
                        format!("{} — {}", item.title, label.display())
                    } else {
                        item.title.clone()
                    };
                    found.push((matched, title, None, true));
                }
                for (path, title, child_warning, allowed) in found {
                    if let Some(age) = max_age {
                        if !older_than(&path, age, now) {
                            continue;
                        }
                    }
                    if !state.seen.insert(path.clone()) {
                        continue;
                    }
                    let warning = item_warning.clone().or(child_warning);
                    state.pending.push(Pending {
                        category,
                        id: format!("{category_id}/{}/{}", item.id, path.display()),
                        title,
                        checked: item.default_checked && allowed && warning.is_none(),
                        warning,
                        path: Some(path),
                        action: None,
                        size: None,
                        mode: item.mode,
                    });
                }
            }
        }
    }

    /// Existing paths matching a validated pattern.
    fn expand(&self, pattern: &str, state: &mut ScanState<'_>) -> Vec<PathBuf> {
        let rest = pattern.strip_prefix("~/").unwrap_or(pattern);
        let mut current = vec![self.env.home.clone()];
        for segment in rest.split('/') {
            if !segment.contains('*') {
                current.iter_mut().for_each(|p| p.push(segment));
                continue;
            }
            let mut next = Vec::new();
            for dir in &current {
                for (name, path) in list_dir(dir, &mut state.inaccessible) {
                    if segment_matches(segment, &name) {
                        next.push(path);
                    }
                }
            }
            current = next;
        }
        current.retain(|p| std::fs::symlink_metadata(p).is_ok());
        current
    }

    /// Children of a real directory (a symlink is not followed) that may be listed.
    fn children(
        &self,
        dir: &Path,
        item: &CleanItemRule,
        state: &mut ScanState<'_>,
    ) -> Vec<(String, PathBuf)> {
        if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
            return Vec::new();
        }
        list_dir(dir, &mut state.inaccessible)
            .into_iter()
            .filter(|(name, _)| {
                name != &self.env.self_bundle_id
                    && !IGNORED_CHILDREN.contains(&name.as_str())
                    && !is_personal_data(name)
                    && !item.exclude_children.contains(name)
            })
            .collect()
    }

    fn running_warning(&self, item: &CleanItemRule, state: &mut ScanState<'_>) -> Option<String> {
        let id = item.app_bundle_id.as_deref()?;
        self.running_message(id, &item.title, state)
    }

    /// A cache folder named after a running app's bundle id.
    fn child_warning(&self, name: &str, title: &str, state: &mut ScanState<'_>) -> Option<String> {
        if !name.contains('.') || !is_valid_bundle_id(name) {
            return None;
        }
        self.running_message(name, title, state)
    }

    fn running_message(
        &self,
        bundle_id: &str,
        fallback: &str,
        state: &mut ScanState<'_>,
    ) -> Option<String> {
        let name = state
            .running
            .entry(bundle_id.to_string())
            .or_insert_with(|| {
                let apps = self.processes.running(bundle_id);
                let first = apps.first()?;
                Some(first.name.clone())
            })
            .clone()?;
        let name = if name.is_empty() { fallback } else { &name };
        Some(format!("{name} is running — close it first"))
    }

    /// Bytes `docker system prune` could free, from `docker system df`.
    fn docker_reclaimable(&self, docker: &Path) -> Option<u64> {
        let out = self
            .runner?
            .run(
                docker.to_str()?,
                &["system", "df", "--format", "{{json .}}"],
            )
            .ok()?;
        if !out.success() {
            return None;
        }
        parse_docker_reclaimable(&out.stdout)
    }
}

/// Applies the policy and measures the entry.
fn finish(p: Pending, policy: &PathPolicy, cancel: &AtomicBool) -> CleanEntry {
    let (size, refused) = match &p.path {
        Some(path) => (
            Some(tree_size(path, cancel).unwrap_or(0)),
            policy.check(path).err().map(|e| e.to_string()),
        ),
        None => (p.size, None),
    };
    CleanEntry {
        checked: p.checked && refused.is_none(),
        id: p.id,
        title: p.title,
        path: p.path,
        action: p.action,
        size,
        warning: p.warning,
        refused,
        mode: p.mode,
    }
}

/// Names and paths of a directory's entries. A missing directory is empty;
/// a permission problem is recorded.
fn list_dir(dir: &Path, inaccessible: &mut Vec<(PathBuf, String)>) -> Vec<(String, PathBuf)> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            inaccessible.push((
                dir.to_path_buf(),
                "Permission denied — grant Full Disk Access to include this location".into(),
            ));
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

/// True if the entry itself was last modified at least `age` ago.
fn older_than(path: &Path, age: Duration, now: SystemTime) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .is_some_and(|elapsed| elapsed >= age)
}

/// Sums the `Reclaimable` column of `docker system df --format '{{json .}}'`.
fn parse_docker_reclaimable(stdout: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut found = false;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(bytes) = value
            .get("Reclaimable")
            .and_then(|v| v.as_str())
            .and_then(|s| s.split_whitespace().next())
            .and_then(parse_docker_size)
        else {
            continue;
        };
        total = total.saturating_add(bytes);
        found = true;
    }
    found.then_some(total)
}

/// Parses sizes such as `1.2GB`, `512kB`, `0B`.
fn parse_docker_size(text: &str) -> Option<u64> {
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let number: f64 = text[..split].parse().ok()?;
    let factor = match text[split..].to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = number * factor;
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

/// The checked, non-refused entries of a scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanPlan {
    pub trash_items: Vec<PlanItem>,
    /// Permanent deletes; only items inside the Trash are ever accepted.
    pub delete_items: Vec<PlanItem>,
    pub actions: Vec<CleanAction>,
    pub total_size: u64,
}

impl CleanPlan {
    pub fn from_scan(scan: &CleanScan) -> CleanPlan {
        let mut plan = CleanPlan::default();
        for entry in scan.entries().filter(|e| e.checked && e.refused.is_none()) {
            let size = entry.size.unwrap_or(0);
            match (&entry.path, entry.action) {
                (Some(path), _) => {
                    let item = PlanItem {
                        path: path.clone(),
                        size,
                    };
                    match entry.mode {
                        CleanMode::Trash => plan.trash_items.push(item),
                        CleanMode::DeleteOnly => plan.delete_items.push(item),
                    }
                }
                (None, Some(action)) if !plan.actions.contains(&action) => {
                    plan.actions.push(action)
                }
                _ => continue,
            }
            plan.total_size = plan.total_size.saturating_add(size);
        }
        plan
    }
}

/// Outcome of [`Cleaner::execute`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanReport {
    pub removed: Vec<Removed>,
    pub actions: Vec<StepResult>,
    /// Items left in place, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Bytes of the removed items (commands report no size).
    pub freed: u64,
    /// Non-fatal problems, e.g. the undo log could not be written.
    pub warnings: Vec<String>,
}

/// Executes a [`CleanPlan`].
pub struct Cleaner<'a> {
    env: &'a Env,
    policy: &'a PathPolicy,
    trasher: &'a dyn Trasher,
    undo: UndoLog,
    runner: &'a dyn CommandRunner,
    tools: &'a dyn ToolLocator,
}

impl<'a> Cleaner<'a> {
    pub fn new(
        env: &'a Env,
        policy: &'a PathPolicy,
        trasher: &'a dyn Trasher,
        undo: UndoLog,
        runner: &'a dyn CommandRunner,
    ) -> Self {
        Cleaner {
            env,
            policy,
            trasher,
            undo,
            runner,
            tools: &REAL_TOOLS,
        }
    }

    /// Uses another tool locator for actions (default: fixed system paths).
    pub fn with_tools(mut self, tools: &'a dyn ToolLocator) -> Self {
        self.tools = tools;
        self
    }

    /// Trashes, deletes, then runs the actions. Every path is checked by the
    /// policy again right before it is touched.
    pub fn execute(&self, plan: &CleanPlan, progress: &mut dyn FnMut(Progress)) -> CleanReport {
        let total = plan.trash_items.len() + plan.delete_items.len() + plan.actions.len();
        let remover = Remover::new(self.env, self.policy, self.trasher, self.undo.clone());
        let mut report = CleanReport::default();
        let mut done = 0;

        let trash = remover.execute(
            &removal_plan(plan.trash_items.clone()),
            RemovalMode::Trash,
            &mut |p| {
                progress(Progress {
                    done: done + p.done,
                    total,
                    path: p.path,
                })
            },
        );
        done += plan.trash_items.len();
        report.absorb(trash);

        let (deletable, refused) = self.split_deletable(&plan.delete_items);
        for (path, reason) in refused {
            done += 1;
            progress(Progress {
                done,
                total,
                path: path.clone(),
            });
            report.failed.push((path, reason));
        }
        let count = deletable.len();
        let deleted = remover.execute(&removal_plan(deletable), RemovalMode::Delete, &mut |p| {
            progress(Progress {
                done: done + p.done,
                total,
                path: p.path,
            })
        });
        done += count;
        report.absorb(deleted);

        let log = ActionLog::new(self.env);
        for action in &plan.actions {
            let (step, program) = self.run_action(*action, &log);
            done += 1;
            progress(Progress {
                done,
                total,
                path: program,
            });
            report.actions.push(step);
        }
        report.freed = report
            .removed
            .iter()
            .map(|r| r.size)
            .fold(0, u64::saturating_add);
        report
    }

    /// Splits permanent deletes into those inside the Trash and the rest.
    fn split_deletable(&self, items: &[PlanItem]) -> (Vec<PlanItem>, Vec<(PathBuf, String)>) {
        let trash = self.env.home.join(".Trash");
        let trash = std::fs::canonicalize(&trash).unwrap_or(trash);
        let mut ok = Vec::new();
        let mut refused = Vec::new();
        for item in items {
            match self.policy.check(&item.path) {
                Ok(path) if path.starts_with(&trash) => ok.push(item.clone()),
                Ok(_) => refused.push((
                    item.path.clone(),
                    "permanent delete is only allowed inside the Trash".to_string(),
                )),
                Err(e) => refused.push((item.path.clone(), e.to_string())),
            }
        }
        (ok, refused)
    }

    fn run_action(&self, action: CleanAction, log: &ActionLog) -> (StepResult, PathBuf) {
        let name = action.label().to_string();
        let fail = |detail: String, program: PathBuf| {
            (
                StepResult {
                    name: name.clone(),
                    ok: false,
                    detail,
                },
                program,
            )
        };
        let Some(program) = self.tools.find(action.tool()) else {
            return fail(
                "the tool is not installed".into(),
                PathBuf::from(action.id()),
            );
        };
        let Some(program_str) = program.to_str() else {
            return fail("tool path is not valid UTF-8".into(), program);
        };
        log.record(action.id(), &program);
        match self.runner.run(program_str, action.args()) {
            Ok(out) if out.success() => (
                StepResult {
                    name,
                    ok: true,
                    detail: first_line(&out.stdout).unwrap_or_else(|| "done".into()),
                },
                program,
            ),
            Ok(out) => {
                let detail = first_line(&out.stderr)
                    .unwrap_or_else(|| format!("exited with status {}", out.status));
                fail(detail, program)
            }
            Err(e) => fail(e.to_string(), program),
        }
    }
}

impl CleanReport {
    fn absorb(&mut self, r: crate::remover::RemovalReport) {
        self.removed.extend(r.removed);
        self.failed.extend(r.failed);
        self.warnings.extend(r.warnings);
    }
}

fn removal_plan(items: Vec<PlanItem>) -> RemovalPlan {
    let total_size = items.iter().map(|i| i.size).fold(0, u64::saturating_add);
    RemovalPlan { items, total_size }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_wildcards() {
        assert!(segment_matches("*", "anything"));
        assert!(segment_matches("*.log", "a.log"));
        assert!(!segment_matches("*.log", "a.logx"));
        assert!(segment_matches("a*c*e", "abcde"));
        assert!(!segment_matches("a*c*e", "abde"));
        assert!(segment_matches("exact", "exact"));
        assert!(!segment_matches("exact", "exactly"));
    }

    #[test]
    fn docker_sizes() {
        assert_eq!(parse_docker_size("1.5GB"), Some(1_500_000_000));
        assert_eq!(parse_docker_size("0B"), Some(0));
        assert_eq!(parse_docker_size("12kB"), Some(12_000));
        assert_eq!(parse_docker_size("lots"), None);
        let df = "{\"Reclaimable\":\"1GB (50%)\"}\n{\"Reclaimable\":\"500MB (10%)\"}\nnoise";
        assert_eq!(parse_docker_reclaimable(df), Some(1_500_000_000));
        assert_eq!(parse_docker_reclaimable("nothing"), None);
    }
}
