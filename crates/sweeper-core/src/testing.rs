//! Test doubles and fixtures. Nothing here touches the real home directory,
//! Trash, or system tools.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tempfile::TempDir;

use crate::cleaner::{Tool, ToolLocator};
use crate::env::{Env, SELF_BUNDLE_ID};
use crate::system::{CommandOutput, CommandRunner, Processes, RunningApp, Spotlight, Trasher};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records every command and answers from a script.
#[derive(Default)]
pub struct RecordingRunner {
    calls: Mutex<Vec<Vec<String>>>,
    responses: Mutex<HashMap<String, io::Result<CommandOutput>>>,
    /// Answers keyed by the full `program arg...` list; they win over `responses`.
    exact: Mutex<HashMap<Vec<String>, CommandOutput>>,
}

impl RecordingRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the answer for every invocation of `program`.
    /// Unscripted programs succeed with empty output.
    pub fn respond(&self, program: &str, output: CommandOutput) {
        lock(&self.responses).insert(program.to_string(), Ok(output));
    }

    /// Scripts the answer for exactly `program args...`; takes precedence over
    /// [`RecordingRunner::respond`].
    pub fn respond_args(&self, program: &str, args: &[&str], output: CommandOutput) {
        let mut key = vec![program.to_string()];
        key.extend(args.iter().map(|a| a.to_string()));
        lock(&self.exact).insert(key, output);
    }

    /// Makes `program` fail to start.
    pub fn fail_to_start(&self, program: &str) {
        lock(&self.responses).insert(
            program.to_string(),
            Err(io::Error::new(io::ErrorKind::NotFound, "not found")),
        );
    }

    /// Every call as `[program, args...]`, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        lock(&self.calls).clone()
    }

    /// Argument lists of every call to `program`.
    pub fn calls_of(&self, program: &str) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c[0] == program)
            .map(|c| c[1..].to_vec())
            .collect()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().map(|a| a.to_string()));
        let scripted = lock(&self.exact).get(&call).cloned();
        lock(&self.calls).push(call);
        if let Some(output) = scripted {
            return Ok(output);
        }
        match lock(&self.responses).get(program) {
            Some(Ok(o)) => Ok(o.clone()),
            Some(Err(e)) => Err(io::Error::new(e.kind(), e.to_string())),
            None => Ok(CommandOutput::default()),
        }
    }
}

/// A Trash inside a temporary directory.
pub struct FakeTrash {
    pub trash_dir: PathBuf,
    failing: Mutex<HashMap<PathBuf, String>>,
}

impl FakeTrash {
    pub fn new(trash_dir: PathBuf) -> Self {
        fs::create_dir_all(&trash_dir).expect("create fake trash");
        FakeTrash {
            trash_dir,
            failing: Mutex::new(HashMap::new()),
        }
    }

    /// Makes trashing or deleting `path` fail with `reason`.
    pub fn fail_on(&self, path: &Path, reason: &str) {
        lock(&self.failing).insert(path.to_path_buf(), reason.to_string());
    }

    fn check_failure(&self, path: &Path) -> io::Result<()> {
        match lock(&self.failing).get(path) {
            Some(reason) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                reason.clone(),
            )),
            None => Ok(()),
        }
    }
}

impl Trasher for FakeTrash {
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        self.check_failure(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let mut target = self.trash_dir.join(name);
        let mut n = 1;
        while fs::symlink_metadata(&target).is_ok() {
            target = self
                .trash_dir
                .join(format!("{} {n}", name.to_string_lossy()));
            n += 1;
        }
        fs::rename(path, &target)?;
        Ok(target)
    }

    fn delete(&self, path: &Path) -> io::Result<()> {
        self.check_failure(path)?;
        crate::system::RealTrasher.delete(path)
    }
}

/// Spotlight answers from tables.
#[derive(Default)]
pub struct FakeSpotlight {
    pub copies: HashMap<String, Vec<PathBuf>>,
    pub last_used: HashMap<PathBuf, SystemTime>,
}

impl Spotlight for FakeSpotlight {
    fn copies_of(&self, bundle_id: &str) -> Vec<PathBuf> {
        self.copies.get(bundle_id).cloned().unwrap_or_default()
    }

    fn last_used(&self, paths: &[PathBuf]) -> HashMap<PathBuf, SystemTime> {
        paths
            .iter()
            .filter_map(|p| self.last_used.get(p).map(|t| (p.clone(), *t)))
            .collect()
    }
}

/// A scripted process table.
#[derive(Default)]
pub struct FakeProcesses {
    running: Mutex<Vec<(String, RunningApp)>>,
    /// Instances that ignore `terminate` and only die on `force_terminate`.
    stubborn: Mutex<HashSet<i32>>,
    terminated: Mutex<Vec<i32>>,
    forced: Mutex<Vec<i32>>,
}

impl FakeProcesses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, bundle_id: &str, app: RunningApp, stubborn: bool) {
        if stubborn {
            lock(&self.stubborn).insert(app.pid);
        }
        lock(&self.running).push((bundle_id.to_string(), app));
    }

    /// Pids that received a polite terminate request.
    pub fn terminated(&self) -> Vec<i32> {
        lock(&self.terminated).clone()
    }

    /// Pids that were force-terminated.
    pub fn forced(&self) -> Vec<i32> {
        lock(&self.forced).clone()
    }

    fn remove(&self, pid: i32) -> bool {
        let mut running = lock(&self.running);
        let before = running.len();
        running.retain(|(_, a)| a.pid != pid);
        running.len() != before
    }
}

impl Processes for FakeProcesses {
    fn running(&self, bundle_id: &str) -> Vec<RunningApp> {
        lock(&self.running)
            .iter()
            .filter(|(id, _)| id == bundle_id)
            .map(|(_, a)| a.clone())
            .collect()
    }

    fn is_running(&self, pid: i32) -> bool {
        lock(&self.running).iter().any(|(_, a)| a.pid == pid)
    }

    fn terminate(&self, pid: i32) -> bool {
        lock(&self.terminated).push(pid);
        if lock(&self.stubborn).contains(&pid) {
            return true;
        }
        self.remove(pid)
    }

    fn force_terminate(&self, pid: i32) -> bool {
        lock(&self.forced).push(pid);
        self.remove(pid)
    }
}

/// Tool locations from a table; unlisted tools are "not installed".
#[derive(Default)]
pub struct FakeToolLocator {
    tools: HashMap<Tool, PathBuf>,
}

impl FakeToolLocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pretends `tool` is installed at `path`.
    pub fn with(mut self, tool: Tool, path: &str) -> Self {
        self.tools.insert(tool, PathBuf::from(path));
        self
    }
}

impl ToolLocator for FakeToolLocator {
    fn find(&self, tool: Tool) -> Option<PathBuf> {
        self.tools.get(&tool).cloned()
    }
}

/// Writes a minimal `.app` bundle with an Info.plist and returns its path.
pub fn make_app(dir: &Path, name: &str, bundle_id: &str, version: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS")).expect("create bundle");
    let mut info = plist::Dictionary::new();
    info.insert("CFBundleIdentifier".into(), bundle_id.into());
    info.insert("CFBundleName".into(), name.into());
    info.insert("CFBundleShortVersionString".into(), version.into());
    plist::Value::Dictionary(info)
        .to_file_xml(contents.join("Info.plist"))
        .expect("write Info.plist");
    fs::write(contents.join("MacOS").join(name), vec![0u8; 1024]).expect("write binary");
    app
}

/// Writes `size` zero bytes to `path`, creating parent directories.
pub fn write_file(path: &Path, size: usize) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    fs::write(path, vec![0u8; size]).expect("write file");
}

/// A fake machine: home, Applications, Caskroom and Trash in a temp dir.
pub struct Fixture {
    pub tmp: TempDir,
    pub env: Env,
    pub apps_dir: PathBuf,
    pub trash_dir: PathBuf,
}

impl Fixture {
    pub fn new() -> Fixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let home = root.join("home");
        let apps_dir = root.join("Applications");
        let caskroom = root.join("Caskroom");
        let trash_dir = home.join(".Trash");
        for sub in [
            "Application Support",
            "Containers",
            "Group Containers",
            "Application Scripts",
            "Caches",
            "HTTPStorages",
            "WebKit",
            "Preferences",
            "Cookies",
            "Saved Application State",
            "Logs",
            "LaunchAgents",
        ] {
            fs::create_dir_all(home.join("Library").join(sub)).expect("create library dir");
        }
        fs::create_dir_all(&apps_dir).expect("create apps dir");
        fs::create_dir_all(&caskroom).expect("create caskroom");
        fs::create_dir_all(root.join("system")).expect("create fake system root");
        let env = Env {
            app_dirs: vec![apps_dir.clone()],
            caskroom_dirs: vec![caskroom],
            uid: 501,
            self_bundle_id: SELF_BUNDLE_ID.to_string(),
            data_dir: home.join("Library/Application Support/Sweeper Kit"),
            system_root: root.join("system"),
            home,
        };
        Fixture {
            tmp,
            env,
            apps_dir,
            trash_dir,
        }
    }

    /// `<sub>` below the fake system root, e.g. `sys("Library/LaunchDaemons")`.
    pub fn sys(&self, sub: &str) -> PathBuf {
        self.env.system_root.join(sub)
    }

    /// `~/Library/<sub>` in the fake home.
    pub fn lib(&self, sub: &str) -> PathBuf {
        self.env.home.join("Library").join(sub)
    }

    /// Installs a dummy app into the fake Applications directory.
    pub fn install(&self, name: &str, bundle_id: &str, version: &str) -> PathBuf {
        make_app(&self.apps_dir, name, bundle_id, version)
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
