//! Sweeper Kit core: finds installed apps, locates the files they leave behind
//! and removes them safely. Pure Rust, offline, no network access.
//!
//! Safety model: every mutation goes through [`PathPolicy::check`], which is
//! evaluated again immediately before each removal.

pub mod catalog;
pub mod cleaner;
pub mod env;
mod fsutil;
pub mod history;
pub mod log;
pub mod orphans;
pub mod path_policy;
pub mod remover;
pub mod rules;
pub mod scanner;
pub mod system;
pub mod uninstall;

/// Test doubles and fixtures. Enable the `testing` feature to use them from
/// other crates.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use catalog::{compute_sizes, team_id, AppCatalog, AppInfo, AppSource};
pub use cleaner::{
    cleaner_roots, CacheScanner, CleanAction, CleanCategory, CleanCategoryRule, CleanEntry,
    CleanItemRule, CleanMode, CleanOptions, CleanPlan, CleanReport, CleanRules, CleanRulesError,
    CleanScan, Cleaner, RealToolLocator, Tool, ToolLocator,
};
pub use env::{Env, SELF_BUNDLE_ID};
pub use history::{RemovedApp, RemovedApps};
pub use log::ActionLog;
pub use orphans::{Confidence, OrphanFinder, OrphanGroup, OrphanLocation, OrphanRules, OrphanScan};
pub use path_policy::{PathPolicy, PolicyError};
pub use remover::{
    PlanItem, Progress, RemovalMode, RemovalPlan, RemovalReport, Removed, Remover, RestoreError,
    UndoBatch, UndoEntry, UndoEntryStatus, UndoLog,
};
pub use rules::{Category, Location, MatchKind, RuleSet, RulesError};
pub use scanner::{apply_preset, Group, Item, LeftoverScanner, Preset, ScanResult};
pub use system::{
    full_disk_access, is_valid_bundle_id, CommandOutput, CommandRunner, FdaStatus, Processes,
    RealProcesses, RealRunner, RealSpotlight, RealTrasher, RunningApp, Spotlight, Trasher,
    FDA_SETTINGS_URL,
};
pub use uninstall::{brew_zap_command, QuitOutcome, StepResult, UninstallReport, Uninstaller};
