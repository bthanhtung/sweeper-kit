//! Data-driven description of where an app leaves files in `~/Library`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

const BUNDLED: &str = include_str!("../../../rules/macos.json");
const SUPPORTED_VERSION: u32 = 1;

/// Kind of a removable item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    AppBundle,
    AppData,
    Caches,
    Preferences,
    CookiesState,
    Logs,
    BackgroundItems,
}

/// How a directory entry is matched against an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// Entry name equals `id + suffix`.
    Id,
    /// Entry starts with the id, followed by `.` or the end, and ends with a suffix.
    IdPrefix,
    /// Case-insensitive `name + suffix` (names shorter than 3 characters never match).
    Name,
    /// Either [`MatchKind::Id`] or [`MatchKind::Name`].
    IdOrName,
    /// Group Containers: `<team>.<anything>`, `group.<id>` or `<team>.<id>`; never by name.
    TeamOrId,
    /// Case-sensitive: starts with `<name>-`, `<name>_` or `<name>.`.
    NamePrefix,
    /// The id appears in the name delimited by `.` (or the string ends).
    ContainsId,
}

/// One place to look for leftovers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub id: String,
    pub category: Category,
    /// Directory to list, always starting with `~/Library/`.
    pub base: String,
    #[serde(rename = "match")]
    pub matcher: MatchKind,
    pub suffixes: Vec<String>,
    pub default_checked: bool,
}

/// Which attribute of the app produced a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBy {
    Id,
    Name,
    /// A team-wide entry (`<team>.<other>`) that sibling apps from the same
    /// developer may share.
    Team,
}

/// The identity attributes of an app used for matching.
#[derive(Debug, Clone, Copy)]
pub struct AppIdent<'a> {
    pub bundle_id: &'a str,
    pub name: &'a str,
    pub team_id: Option<&'a str>,
}

/// Failure to load or validate a rule set.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("rules file is not valid: {0}")]
    Parse(String),
    #[error("unsupported rules version {0}")]
    UnsupportedVersion(u32),
    #[error("rule '{id}' is invalid: {reason}")]
    Invalid { id: String, reason: String },
    #[error("cannot read rules file: {0}")]
    Io(String),
}

/// A validated list of [`Location`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    pub version: u32,
    pub locations: Vec<Location>,
}

impl RuleSet {
    /// The rules shipped with the crate.
    pub fn bundled() -> RuleSet {
        RuleSet::from_json(BUNDLED).expect("bundled rules/macos.json is valid")
    }

    /// Parses and validates a rule set.
    pub fn from_json(json: &str) -> Result<RuleSet, RulesError> {
        let set: RuleSet =
            serde_json::from_str(json).map_err(|e| RulesError::Parse(e.to_string()))?;
        set.validate()?;
        Ok(set)
    }

    /// Reads and validates a user rules file.
    pub fn load(path: &Path) -> Result<RuleSet, RulesError> {
        let text = std::fs::read_to_string(path).map_err(|e| RulesError::Io(e.to_string()))?;
        RuleSet::from_json(&text)
    }

    /// Returns `self` overlaid with `user`: same id replaces, new ids append.
    pub fn merge(mut self, user: RuleSet) -> RuleSet {
        for loc in user.locations {
            match self.locations.iter_mut().find(|l| l.id == loc.id) {
                Some(slot) => *slot = loc,
                None => self.locations.push(loc),
            }
        }
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
        Ok(())
    }
}

impl Location {
    /// Absolute directory to list for the given home.
    pub fn resolve_base(&self, home: &Path) -> PathBuf {
        home.join(self.base.trim_start_matches("~/"))
    }

    /// Tests a directory entry name against this location's strategy.
    pub fn match_entry(&self, entry: &str, app: &AppIdent<'_>) -> Option<MatchBy> {
        let id = app.bundle_id;
        let by_id = || self.match_id(entry, id).then_some(MatchBy::Id);
        let by_name = || self.match_name(entry, app.name).then_some(MatchBy::Name);
        match self.matcher {
            MatchKind::Id => by_id(),
            MatchKind::IdPrefix => self.match_id_prefix(entry, id).then_some(MatchBy::Id),
            MatchKind::Name => by_name(),
            MatchKind::IdOrName => by_id().or_else(by_name),
            MatchKind::TeamOrId => self.match_team(entry, app),
            MatchKind::NamePrefix => self
                .match_name_prefix(entry, app.name)
                .then_some(MatchBy::Name),
            MatchKind::ContainsId => self.match_contains(entry, id).then_some(MatchBy::Id),
        }
    }

    fn stems<'a>(&'a self, entry: &'a str) -> impl Iterator<Item = &'a str> {
        self.suffixes
            .iter()
            .filter_map(move |s| entry.strip_suffix(s.as_str()))
    }

    fn match_id(&self, entry: &str, id: &str) -> bool {
        !id.is_empty() && self.stems(entry).any(|s| s == id)
    }

    fn match_name(&self, entry: &str, name: &str) -> bool {
        name.chars().count() >= 3
            && self
                .stems(entry)
                .any(|s| s.to_lowercase() == name.to_lowercase())
    }

    fn match_id_prefix(&self, entry: &str, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        match entry.strip_prefix(id) {
            Some(rest) => {
                (rest.is_empty() || rest.starts_with('.'))
                    && self.suffixes.iter().any(|s| rest.ends_with(s.as_str()))
            }
            None => false,
        }
    }

    fn match_name_prefix(&self, entry: &str, name: &str) -> bool {
        if name.chars().count() < 3 || !self.suffixes.iter().any(|s| entry.ends_with(s.as_str())) {
            return false;
        }
        match entry.strip_prefix(name) {
            Some(rest) => rest.starts_with(['-', '_', '.']),
            None => false,
        }
    }

    /// `group.<id>` and `<team>.<id>` are [`MatchBy::Id`]; any other entry under
    /// the app's own team is [`MatchBy::Team`], which is shared with siblings.
    fn match_team(&self, entry: &str, app: &AppIdent<'_>) -> Option<MatchBy> {
        let id = app.bundle_id;
        if id.is_empty() {
            return None;
        }
        let mut best = None;
        for stem in self.stems(entry) {
            if stem.strip_prefix("group.") == Some(id) {
                return Some(MatchBy::Id);
            }
            let Some((team, rest)) = stem.split_once('.') else {
                continue;
            };
            let own_team = app.team_id == Some(team);
            if rest == id && (own_team || looks_like_team_id(team)) {
                return Some(MatchBy::Id);
            }
            if own_team && !rest.is_empty() {
                best = Some(MatchBy::Team);
            }
        }
        best
    }

    fn match_contains(&self, entry: &str, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        self.stems(entry).any(|stem| {
            stem.match_indices(id).any(|(i, m)| {
                let before_ok = i == 0 || stem[..i].ends_with('.');
                let after = &stem[i + m.len()..];
                before_ok && (after.is_empty() || after.starts_with('.'))
            })
        })
    }
}

/// Apple team identifiers are 10 uppercase alphanumeric characters.
fn looks_like_team_id(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(m: MatchKind, suffixes: &[&str]) -> Location {
        Location {
            id: "x".into(),
            category: Category::Caches,
            base: "~/Library/Caches".into(),
            matcher: m,
            suffixes: suffixes.iter().map(|s| s.to_string()).collect(),
            default_checked: true,
        }
    }

    fn app<'a>(id: &'a str, name: &'a str, team: Option<&'a str>) -> AppIdent<'a> {
        AppIdent {
            bundle_id: id,
            name,
            team_id: team,
        }
    }

    #[test]
    fn bundled_rules_are_valid() {
        assert_eq!(RuleSet::bundled().locations.len(), 15);
    }

    #[test]
    fn id_prefix_boundaries() {
        let l = loc(MatchKind::IdPrefix, &[".plist"]);
        let a = app("com.foo.app", "Foo", None);
        assert!(l.match_entry("com.foo.app.plist", &a).is_some());
        assert!(l.match_entry("com.foo.app.helper.plist", &a).is_some());
        assert!(l.match_entry("com.foo.application.plist", &a).is_none());
        assert!(l.match_entry("com.foo.app.helper", &a).is_none());
    }

    #[test]
    fn short_names_never_match() {
        let l = loc(MatchKind::Name, &[""]);
        assert!(l.match_entry("ab", &app("a.b", "ab", None)).is_none());
        assert!(l
            .match_entry("CHROME", &app("a.b", "Chrome", None))
            .is_some());
        assert!(l
            .match_entry("MyChromeThing", &app("a.b", "Chrome", None))
            .is_none());
    }

    #[test]
    fn team_matching() {
        let l = loc(MatchKind::TeamOrId, &[""]);
        let a = app("com.foo.app", "Foo", Some("ABCDE12345"));
        assert!(l.match_entry("ABCDE12345.shared", &a).is_some());
        assert!(l.match_entry("group.com.foo.app", &a).is_some());
        assert!(l.match_entry("OTHERTEAM1.shared", &a).is_none());
        let b = app("com.foo.app", "Foo", None);
        assert!(l.match_entry("ABCDE12345.com.foo.app", &b).is_some());
        assert!(l.match_entry("ABCDE12345.shared", &b).is_none());
    }

    #[test]
    fn team_wide_containers_are_not_id_matches() {
        let l = loc(MatchKind::TeamOrId, &[""]);
        let a = app("com.microsoft.Word", "Word", Some("UBF8T346G9"));
        assert_eq!(
            l.match_entry("UBF8T346G9.com.microsoft.Word", &a),
            Some(MatchBy::Id)
        );
        assert_eq!(
            l.match_entry("group.com.microsoft.Word", &a),
            Some(MatchBy::Id)
        );
        assert_eq!(l.match_entry("UBF8T346G9.Office", &a), Some(MatchBy::Team));
        assert_eq!(l.match_entry("OTHERTEAM1.Office", &a), None);
    }

    #[test]
    fn contains_id_boundaries() {
        let l = loc(MatchKind::ContainsId, &[".plist"]);
        let a = app("com.foo.app", "Foo", None);
        assert!(l.match_entry("com.foo.app.helper.plist", &a).is_some());
        assert!(l.match_entry("x.com.foo.app.plist", &a).is_some());
        assert!(l.match_entry("com.foo.application.plist", &a).is_none());
        assert!(l.match_entry("xcom.foo.app.plist", &a).is_none());
    }

    #[test]
    fn name_prefix_is_case_sensitive() {
        let l = loc(MatchKind::NamePrefix, &[""]);
        let a = app("a.b", "Notion", None);
        assert!(l.match_entry("Notion-2024.ips", &a).is_some());
        assert!(l.match_entry("notion-2024.ips", &a).is_none());
        assert!(l.match_entry("Notion Calendar-2024.ips", &a).is_none());
    }

    #[test]
    fn merge_replaces_and_appends() {
        let mut user = RuleSet::bundled();
        user.locations.truncate(1);
        user.locations[0].default_checked = false;
        let mut extra = user.locations[0].clone();
        extra.id = "extra".into();
        user.locations.push(extra);
        let merged = RuleSet::bundled().merge(user);
        assert_eq!(merged.locations.len(), 16);
        assert!(!merged.locations[0].default_checked);
        assert_eq!(merged.locations[15].id, "extra");
    }

    #[test]
    fn invalid_rules_rejected() {
        let mut set = RuleSet::bundled();
        set.locations[0].base = "/etc".into();
        assert!(RuleSet::from_json(&serde_json::to_string(&set).unwrap()).is_err());
        let mut set = RuleSet::bundled();
        set.locations[0].base = "~/Library/../Documents".into();
        assert!(RuleSet::from_json(&serde_json::to_string(&set).unwrap()).is_err());
        let json = |cat: &str, m: &str| {
            format!(
                r#"{{"version":1,"locations":[{{"id":"a","category":"{cat}","base":"~/Library/X","match":"{m}","suffixes":[""],"default_checked":true}}]}}"#
            )
        };
        assert!(RuleSet::from_json(&json("caches", "id")).is_ok());
        assert!(RuleSet::from_json(&json("bogus", "id")).is_err());
        assert!(RuleSet::from_json(&json("caches", "nope")).is_err());
        assert!(RuleSet::from_json(&json("app_bundle", "id")).is_err());
        assert!(RuleSet::from_json(r#"{"version":2,"locations":[]}"#).is_err());
    }
}
