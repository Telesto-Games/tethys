//! Use cases: what the app asks the core to do.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::debug::{self, FindEditorError, ProcessInfo, SourceBreakpoint};
use crate::diff::{self, FileDiff};
use crate::domain::{AdapterKind, AgentProfile, DomainError, Project, ProjectError, SessionId};
use crate::ports::{
    AgentHost, AgentSession, ConfigStore, DebugEventSink, DebugSession, Debugger, EngineLocator,
    EventSink, PortError, ProcessLauncher, SourceControl,
};
use crate::scm::{WorkingCopyInfo, WorkingCopyStatus};
use crate::unreal::{self, Configuration, ToolError};

/// Lines of unchanged context shown around each change in a diff.
pub const DIFF_CONTEXT: usize = 3;

/// The changed files of the working copy at `root`.
pub fn working_copy_status(
    scm: &dyn SourceControl,
    root: &Path,
) -> Result<WorkingCopyStatus, PortError> {
    Ok(WorkingCopyStatus::new(scm.status(root)?))
}

/// Picks the source control managing `root` from `candidates`: the one whose
/// working copy root is nearest, so a git repository inside an SVN working
/// copy (or the reverse) goes to the inner one. Ties go to the earlier one.
pub fn detect_source_control(
    candidates: &[Arc<dyn SourceControl>],
    root: &Path,
) -> Option<Arc<dyn SourceControl>> {
    candidates
        .iter()
        .filter_map(|scm| {
            let depth = scm.working_copy_root(root)?.components().count();
            Some((Reverse(depth), scm))
        })
        .min_by_key(|(depth, _)| *depth)
        .map(|(_, scm)| scm.clone())
}

/// What the Files pane shows about source control: the provider and where the
/// working copy points, or why there's nothing to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScmSummary {
    /// The project folder isn't managed by any of these source controls.
    NotUnderControl { providers: Vec<&'static str> },
    Ready {
        provider: &'static str,
        info: WorkingCopyInfo,
    },
    Failed {
        provider: &'static str,
        error: String,
    },
}

/// Summarises the working copy at `root` for display.
pub fn scm_summary(scm: &dyn SourceControl, root: &Path) -> ScmSummary {
    let provider = scm.name();
    if !scm.is_working_copy(root) {
        return ScmSummary::NotUnderControl {
            providers: vec![provider],
        };
    }
    match scm.info(root) {
        Ok(info) => ScmSummary::Ready { provider, info },
        Err(e) => ScmSummary::Failed {
            provider,
            error: e.to_string(),
        },
    }
}

/// Diffs a file's current text against its BASE version. A file with no
/// base (unversioned or added) diffs against empty text.
pub fn diff_against_base(
    scm: &dyn SourceControl,
    file: &Path,
    current: &str,
) -> Result<FileDiff, PortError> {
    let base = scm.base_text(file)?.unwrap_or_default();
    Ok(diff::diff_lines(&base, current, DIFF_CONTEXT))
}

/// How many recent projects to remember.
pub const MAX_RECENT_PROJECTS: usize = 10;

/// Reads and parses a `.uproject`, then resolves its engine. A folder opens
/// as a plain folder project.
///
/// An unresolved engine is not an error: the project still opens and
/// `Project::engine` says what went wrong.
pub fn open_project(path: &Path, locator: &dyn EngineLocator) -> Result<Project, ProjectError> {
    if path.is_dir() {
        return Ok(Project::folder(path));
    }
    let json = std::fs::read_to_string(path).map_err(|source| ProjectError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut project = Project::parse(path, &json)?;
    project.engine = locator
        .locate(&project.engine_association, &project.path)
        .map_err(|e| e.to_string());
    // Missing Source is normal for Blueprint-only projects.
    if let Ok(entries) = std::fs::read_dir(project.root().join("Source")) {
        let names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        project.targets = unreal::targets_from_file_names(names.iter().map(String::as_str));
    }
    Ok(project)
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRunError {
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Start(#[from] StartError),
    #[error("can't launch {program}: {source}")]
    Launch { program: String, source: PortError },
}

/// Starts an editor build as a terminal session, so its output is visible.
pub fn start_build(
    hosts: &[&dyn AgentHost],
    id: SessionId,
    project: &Project,
    configuration: Configuration,
    events: EventSink,
) -> Result<Box<dyn AgentSession>, ToolRunError> {
    let cmd = unreal::build_command(project, configuration)?;
    let profile = AgentProfile {
        id: "build".into(),
        name: format!(
            "Build {} {}",
            unreal::editor_target(project),
            configuration.as_str()
        ),
        adapter: AdapterKind::Terminal,
        command: cmd.program.display().to_string(),
        args: cmd.args,
    };
    Ok(start_session(hosts, id, &profile, project, events)?)
}

/// Starts a plain shell (no agent) in the project folder as a terminal session.
pub fn start_terminal(
    hosts: &[&dyn AgentHost],
    id: SessionId,
    shell: &str,
    project: &Project,
    events: EventSink,
) -> Result<Box<dyn AgentSession>, StartError> {
    let profile = AgentProfile {
        id: "terminal".into(),
        name: "Terminal".into(),
        adapter: AdapterKind::Terminal,
        command: shell.into(),
        args: vec![],
    };
    start_session(hosts, id, &profile, project, events)
}

/// Starts the Unreal Editor on the project, detached from Tethys, with the
/// user's extra `editor_args`.
pub fn launch_editor(
    project: &Project,
    configuration: Configuration,
    editor_args: &[String],
    launcher: &dyn ProcessLauncher,
) -> Result<(), ToolRunError> {
    let cmd = unreal::editor_command(project, configuration, editor_args)?;
    launcher
        .spawn_detached(&cmd, project.root())
        .map_err(|source| ToolRunError::Launch {
            program: cmd.program.display().to_string(),
            source,
        })
}

#[derive(Debug, thiserror::Error)]
pub enum DebugStartError {
    #[error(transparent)]
    Find(#[from] FindEditorError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("can't list processes: {0}")]
    List(PortError),
    #[error("can't attach to {exe} (process {pid}): {source}")]
    Attach {
        exe: String,
        pid: u32,
        source: PortError,
    },
    #[error("can't start {program} under the debugger: {source}")]
    Launch { program: String, source: PortError },
}

/// How to start debugging the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugStart {
    /// Attach if this project's editor is running, otherwise launch it.
    Auto,
    Attach,
    Launch,
}

/// The debugged editor: which process, and whether Tethys started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Debuggee {
    pub process: ProcessInfo,
    pub launched: bool,
}

/// Starts debugging the Unreal Editor for `project`: attaching to the one that
/// has it open, or launching one under the debugger (in `configuration`).
/// `breakpoints` are set before the editor runs on.
pub fn debug_editor(
    debugger: &dyn Debugger,
    project: &Project,
    configuration: Configuration,
    editor_args: &[String],
    how: DebugStart,
    breakpoints: Vec<SourceBreakpoint>,
    events: DebugEventSink,
) -> Result<(Debuggee, Box<dyn DebugSession>), DebugStartError> {
    let running = match how {
        DebugStart::Launch => None,
        DebugStart::Attach | DebugStart::Auto => {
            let processes = debugger.processes().map_err(DebugStartError::List)?;
            match debug::find_editor(&processes, &project.path) {
                Ok(editor) => Some(editor.clone()),
                Err(FindEditorError::NoneRunning) if how == DebugStart::Auto => None,
                Err(e) => return Err(e.into()),
            }
        }
    };
    if let Some(editor) = running {
        let session = debugger
            .attach(editor.pid, breakpoints, events)
            .map_err(|source| DebugStartError::Attach {
                exe: editor.exe.clone(),
                pid: editor.pid,
                source,
            })?;
        let debuggee = Debuggee {
            process: editor,
            launched: false,
        };
        return Ok((debuggee, session));
    }
    let cmd = unreal::editor_command(project, configuration, editor_args)?;
    let session = debugger
        .launch(&cmd, project.root(), breakpoints, events)
        .map_err(|source| DebugStartError::Launch {
            program: cmd.program.display().to_string(),
            source,
        })?;
    let exe = cmd
        .program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let debuggee = Debuggee {
        process: ProcessInfo {
            pid: session.pid(),
            exe,
            details: String::new(),
        },
        launched: true,
    };
    Ok((debuggee, session))
}

/// Moves `path` to the front of the recent projects list and saves it.
pub fn remember_project(config: &dyn ConfigStore, path: &Path) -> Result<(), PortError> {
    let recent = config.recent_projects().unwrap_or_default();
    config.set_recent_projects(&push_recent(recent, path))
}

/// Removes `path` from the recent projects list.
pub fn forget_project(config: &dyn ConfigStore, path: &Path) -> Result<(), PortError> {
    let mut recent = config.recent_projects()?;
    recent.retain(|p| !same_path(p, path));
    config.set_recent_projects(&recent)
}

/// A recent project, loaded for display. `project` is the error message if it
/// can no longer be opened (moved, deleted, or broken).
#[derive(Debug)]
pub struct RecentProject {
    pub path: PathBuf,
    pub project: Result<Project, String>,
}

/// Recent projects, most recent first, each opened to show its details.
pub fn recent_projects(
    config: &dyn ConfigStore,
    locator: &dyn EngineLocator,
) -> Vec<RecentProject> {
    config
        .recent_projects()
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            let project = if path.exists() {
                open_project(&path, locator).map_err(|e| e.to_string())
            } else {
                Err("not found".into())
            };
            RecentProject { path, project }
        })
        .collect()
}

fn push_recent(mut recent: Vec<PathBuf>, path: &Path) -> Vec<PathBuf> {
    recent.retain(|p| !same_path(p, path));
    recent.insert(0, path.to_path_buf());
    recent.truncate(MAX_RECENT_PROJECTS);
    recent
}

fn same_path(a: &Path, b: &Path) -> bool {
    // Windows paths are case-insensitive.
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Configured agent profiles, falling back to the built-ins (Claude Code, opencode).
/// Invalid profiles are dropped and reported.
pub fn agent_profiles(config: &dyn ConfigStore) -> (Vec<AgentProfile>, Vec<String>) {
    let mut problems = Vec::new();
    let configured = config.agent_profiles().unwrap_or_else(|e| {
        problems.push(format!("can't load agent config: {e}"));
        Vec::new()
    });
    let mut profiles: Vec<_> = configured
        .into_iter()
        .filter(|p| match p.validate() {
            Ok(()) => true,
            Err(e) => {
                problems.push(e.to_string());
                false
            }
        })
        .collect();
    if profiles.is_empty() {
        profiles = AgentProfile::builtins();
    }
    (profiles, problems)
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Invalid(#[from] DomainError),
    #[error("no host for the {0:?} adapter")]
    NoHost(crate::AdapterKind),
    #[error("can't start {name}: {source}")]
    Spawn { name: String, source: PortError },
}

/// Starts `profile` in `project` on the host matching its adapter.
pub fn start_session(
    hosts: &[&dyn AgentHost],
    id: SessionId,
    profile: &AgentProfile,
    project: &Project,
    events: EventSink,
) -> Result<Box<dyn AgentSession>, StartError> {
    profile.validate()?;
    let host = hosts
        .iter()
        .find(|h| h.kind() == profile.adapter)
        .ok_or(StartError::NoHost(profile.adapter))?;
    host.start(id, profile, project, events)
        .map_err(|source| StartError::Spawn {
            name: profile.name.clone(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::AdapterKind;
    use crate::ports::{PortResult, SessionEvent, SessionStatus};

    struct FakeLocator;
    impl EngineLocator for FakeLocator {
        fn locate(&self, association: &str, _: &Path) -> PortResult<PathBuf> {
            match association {
                "5.6" => Ok(PathBuf::from(r"C:\UE_5.6")),
                _ => Err("unknown engine".into()),
            }
        }
    }

    #[derive(Default)]
    struct FakeConfig {
        profiles: Vec<AgentProfile>,
        recent: RefCell<Vec<PathBuf>>,
    }
    impl ConfigStore for FakeConfig {
        fn agent_profiles(&self) -> PortResult<Vec<AgentProfile>> {
            Ok(self.profiles.clone())
        }
        fn recent_projects(&self) -> PortResult<Vec<PathBuf>> {
            Ok(self.recent.borrow().clone())
        }
        fn set_recent_projects(&self, projects: &[PathBuf]) -> PortResult<()> {
            *self.recent.borrow_mut() = projects.to_vec();
            Ok(())
        }
        fn build_configuration(&self) -> PortResult<Configuration> {
            Ok(Configuration::Development)
        }
        fn set_build_configuration(&self, _: Configuration) -> PortResult<()> {
            Ok(())
        }
        fn editor_args(&self) -> PortResult<String> {
            Ok(String::new())
        }
        fn set_editor_args(&self, _: &str) -> PortResult<()> {
            Ok(())
        }
        fn vertical_tabs(&self) -> PortResult<bool> {
            Ok(false)
        }
        fn set_vertical_tabs(&self, _: bool) -> PortResult<()> {
            Ok(())
        }
        fn skipped_version(&self) -> PortResult<Option<crate::update::Version>> {
            Ok(None)
        }
        fn set_skipped_version(&self, _: Option<crate::update::Version>) -> PortResult<()> {
            Ok(())
        }
    }

    struct FakeSession(SessionId);
    impl AgentSession for FakeSession {
        fn id(&self) -> SessionId {
            self.0
        }
        fn status(&self) -> SessionStatus {
            SessionStatus::Running
        }
        fn stop(&mut self) -> PortResult<()> {
            Ok(())
        }
    }

    struct FakeHost(AdapterKind);
    impl AgentHost for FakeHost {
        fn kind(&self) -> AdapterKind {
            self.0
        }
        fn start(
            &self,
            id: SessionId,
            _: &AgentProfile,
            _: &Project,
            events: EventSink,
        ) -> PortResult<Box<dyn AgentSession>> {
            events(id, SessionEvent::Started);
            Ok(Box::new(FakeSession(id)))
        }
    }

    fn temp_uproject(name: &str, json: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tethys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn open_project_resolves_engine() {
        let path = temp_uproject("Launcher.uproject", r#"{"EngineAssociation":"5.6"}"#);
        let p = open_project(&path, &FakeLocator).unwrap();
        assert_eq!(p.engine, Ok(PathBuf::from(r"C:\UE_5.6")));
    }

    #[test]
    fn open_project_keeps_unresolved_engine_as_message() {
        let path = temp_uproject("Unknown.uproject", r#"{"EngineAssociation":"4.1"}"#);
        let p = open_project(&path, &FakeLocator).unwrap();
        assert_eq!(p.engine, Err("unknown engine".to_string()));
    }

    #[test]
    fn open_project_reports_missing_file() {
        let err = open_project(Path::new(r"Z:\nope\Missing.uproject"), &FakeLocator);
        assert!(matches!(err, Err(ProjectError::Read { .. })));
    }

    #[test]
    fn open_project_opens_a_folder_as_is() {
        let dir = std::env::temp_dir().join(format!("tethys-folder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = open_project(&dir, &FakeLocator).unwrap();
        assert!(!p.is_unreal());
        assert_eq!(p.root(), dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forget_and_list_recent_projects() {
        let good = temp_uproject("Good.uproject", r#"{"EngineAssociation":"5.6"}"#);
        let missing = PathBuf::from(r"Z:\gone\Gone.uproject");
        let config = FakeConfig::default();
        remember_project(&config, &missing).unwrap();
        remember_project(&config, &good).unwrap();

        let recent = recent_projects(&config, &FakeLocator);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].project.as_ref().unwrap().name(), "Good");
        assert_eq!(recent[1].project.as_ref().unwrap_err(), "not found");

        forget_project(&config, Path::new(r"z:\GONE\gone.uproject")).unwrap();
        assert_eq!(*config.recent.borrow(), [good]);
    }

    #[test]
    fn recent_projects_dedupe_and_cap() {
        let config = FakeConfig::default();
        for i in 0..12 {
            remember_project(&config, Path::new(&format!(r"C:\p{i}.uproject"))).unwrap();
        }
        remember_project(&config, Path::new(r"C:\P5.uproject")).unwrap();
        let recent = config.recent.borrow();
        assert_eq!(recent.len(), MAX_RECENT_PROJECTS);
        assert_eq!(recent[0], Path::new(r"C:\P5.uproject"));
        assert_eq!(
            recent
                .iter()
                .filter(|p| same_path(p, Path::new(r"C:\p5.uproject")))
                .count(),
            1
        );
    }

    #[test]
    fn profiles_fall_back_to_builtins_and_drop_invalid() {
        let mut bad = AgentProfile::claude();
        bad.adapter = AdapterKind::Acp;
        let config = FakeConfig {
            profiles: vec![bad],
            ..Default::default()
        };
        let (profiles, problems) = agent_profiles(&config);
        assert_eq!(profiles, AgentProfile::builtins());
        assert_eq!(profiles[0], AgentProfile::claude());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn start_session_picks_host_by_adapter() {
        let project = Project::parse("G.uproject", "{}").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let sink: EventSink = Arc::new(move |id, e| sink_seen.lock().unwrap().push((id, e)));

        let acp = FakeHost(AdapterKind::Acp);
        let terminal = FakeHost(AdapterKind::Terminal);
        let session = start_session(
            &[&acp, &terminal],
            SessionId(7),
            &AgentProfile::claude(),
            &project,
            sink.clone(),
        )
        .unwrap();
        assert_eq!(session.id(), SessionId(7));
        assert_eq!(
            *seen.lock().unwrap(),
            [(SessionId(7), SessionEvent::Started)]
        );

        let err = start_session(
            &[&acp],
            SessionId(8),
            &AgentProfile::claude(),
            &project,
            sink,
        );
        assert!(matches!(
            err,
            Err(StartError::NoHost(AdapterKind::Terminal))
        ));
    }
}

#[cfg(test)]
mod tool_tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::ports::{PortResult, SessionStatus};
    use crate::unreal::CommandSpec;

    #[derive(Default)]
    struct FakeLauncher(Mutex<Vec<(CommandSpec, PathBuf)>>);
    impl ProcessLauncher for FakeLauncher {
        fn spawn_detached(&self, command: &CommandSpec, cwd: &Path) -> PortResult<()> {
            self.0
                .lock()
                .unwrap()
                .push((command.clone(), cwd.to_path_buf()));
            Ok(())
        }
    }

    struct Session(SessionId);
    impl AgentSession for Session {
        fn id(&self) -> SessionId {
            self.0
        }
        fn status(&self) -> SessionStatus {
            SessionStatus::Running
        }
        fn stop(&mut self) -> PortResult<()> {
            Ok(())
        }
    }

    /// Records the profile it was asked to start.
    #[derive(Default)]
    struct RecordingHost(Mutex<Option<AgentProfile>>);
    impl AgentHost for RecordingHost {
        fn kind(&self) -> AdapterKind {
            AdapterKind::Terminal
        }
        fn start(
            &self,
            id: SessionId,
            profile: &AgentProfile,
            _: &Project,
            _: EventSink,
        ) -> PortResult<Box<dyn AgentSession>> {
            *self.0.lock().unwrap() = Some(profile.clone());
            Ok(Box::new(Session(id)))
        }
    }

    fn project() -> Project {
        let mut p = Project::parse(r"D:\g\Game.uproject", "{}").unwrap();
        p.engine = Ok(PathBuf::from(r"D:\UE"));
        p.modules = vec!["Game".into()];
        p.targets = vec!["Game".into(), "GameEditor".into()];
        p
    }

    /// Records what it was asked to do: `attach:<pid>` or `launch:<program>`.
    #[derive(Default)]
    struct FakeDebugger(Vec<ProcessInfo>, Mutex<Vec<String>>);
    struct FakeDebugSession(u32);
    impl DebugSession for FakeDebugSession {
        fn pid(&self) -> u32 {
            self.0
        }
        fn set_breakpoints(&self, _: Vec<SourceBreakpoint>) {}
        fn resume(&self) {}
        fn pause(&self) {}
        fn detach(&self) {}
    }
    impl Debugger for FakeDebugger {
        fn processes(&self) -> PortResult<Vec<ProcessInfo>> {
            Ok(self.0.clone())
        }
        fn attach(
            &self,
            pid: u32,
            _: Vec<SourceBreakpoint>,
            _: DebugEventSink,
        ) -> PortResult<Box<dyn DebugSession>> {
            self.1.lock().unwrap().push(format!("attach:{pid}"));
            Ok(Box::new(FakeDebugSession(pid)))
        }
        fn launch(
            &self,
            command: &CommandSpec,
            cwd: &Path,
            _: Vec<SourceBreakpoint>,
            _: DebugEventSink,
        ) -> PortResult<Box<dyn DebugSession>> {
            assert_eq!(cwd, Path::new(r"D:\g"));
            let call = format!(
                "launch:{} {}",
                command.program.display(),
                command.args.join(" ")
            );
            self.1.lock().unwrap().push(call);
            Ok(Box::new(FakeDebugSession(77)))
        }
    }

    fn editor(pid: u32, uproject: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            exe: "UnrealEditor.exe".into(),
            details: format!("UnrealEditor.exe {uproject}"),
        }
    }

    fn debug(debugger: &FakeDebugger, how: DebugStart) -> Result<Debuggee, DebugStartError> {
        let started = debug_editor(
            debugger,
            &project(),
            Configuration::DebugGame,
            &["-ModelContextProtocolStartServer".to_string()],
            how,
            Vec::new(),
            Arc::new(|_| {}),
        );
        started.map(|(debuggee, _)| debuggee)
    }

    #[test]
    fn debugging_attaches_to_the_projects_running_editor() {
        let debugger = FakeDebugger(
            vec![
                editor(5, r"D:\o\Other.uproject"),
                editor(9, r"D:\g\Game.uproject"),
            ],
            Mutex::default(),
        );
        let debuggee = debug(&debugger, DebugStart::Auto).unwrap();
        assert_eq!(debuggee.process.pid, 9);
        assert!(!debuggee.launched);
        assert_eq!(*debugger.1.lock().unwrap(), vec!["attach:9"]);
    }

    #[test]
    fn debugging_launches_the_editor_when_none_is_running() {
        let debugger = FakeDebugger::default();
        let debuggee = debug(&debugger, DebugStart::Auto).unwrap();
        assert!(debuggee.launched);
        assert_eq!(debuggee.process.pid, 77);
        assert_eq!(debuggee.process.exe, "UnrealEditor-Win64-DebugGame.exe");
        assert_eq!(
            *debugger.1.lock().unwrap(),
            vec![
                r"launch:D:\UE\Engine\Binaries\Win64\UnrealEditor-Win64-DebugGame.exe D:\g\Game.uproject -ModelContextProtocolStartServer"
            ]
        );
    }

    #[test]
    fn explicit_attach_and_launch_do_only_that() {
        let none = FakeDebugger::default();
        assert!(matches!(
            debug(&none, DebugStart::Attach),
            Err(DebugStartError::Find(FindEditorError::NoneRunning))
        ));
        let running = FakeDebugger(vec![editor(9, r"D:\g\Game.uproject")], Mutex::default());
        assert!(debug(&running, DebugStart::Launch).unwrap().launched);
    }

    #[test]
    fn launch_editor_runs_detached_in_project_root() {
        let launcher = FakeLauncher::default();
        let args = vec!["-log".to_string()];
        launch_editor(&project(), Configuration::Development, &args, &launcher).unwrap();
        let calls = launcher.0.lock().unwrap();
        assert_eq!(
            calls[0].0.program,
            Path::new(r"D:\UE\Engine\Binaries\Win64\UnrealEditor.exe")
        );
        assert_eq!(calls[0].0.args, [r"D:\g\Game.uproject", "-log"]);
        assert_eq!(calls[0].1, Path::new(r"D:\g"));
    }

    #[test]
    fn build_runs_build_bat_in_a_terminal_session() {
        let host = RecordingHost::default();
        let sink: EventSink = Arc::new(|_, _| {});
        start_build(
            &[&host],
            SessionId(1),
            &project(),
            Configuration::DebugGame,
            sink,
        )
        .unwrap();
        let profile = host.0.lock().unwrap().clone().unwrap();
        assert_eq!(profile.name, "Build GameEditor DebugGame");
        assert_eq!(profile.args[2], "DebugGame");
        assert_eq!(profile.command, r"D:\UE\Engine\Build\BatchFiles\Build.bat");
        assert_eq!(profile.args[0], "GameEditor");
    }

    #[test]
    fn terminal_runs_the_shell_without_an_agent() {
        let host = RecordingHost::default();
        let sink: EventSink = Arc::new(|_, _| {});
        start_terminal(&[&host], SessionId(1), "pwsh", &project(), sink).unwrap();
        let profile = host.0.lock().unwrap().clone().unwrap();
        assert_eq!(profile.name, "Terminal");
        assert_eq!(profile.command, "pwsh");
        assert!(profile.args.is_empty());
    }

    #[test]
    fn open_project_finds_targets() {
        let dir = std::env::temp_dir().join(format!("tethys-targets-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Source")).unwrap();
        std::fs::write(dir.join("Source/GameEditor.Target.cs"), "").unwrap();
        std::fs::write(dir.join("Game.uproject"), "{}").unwrap();
        struct NoEngine;
        impl EngineLocator for NoEngine {
            fn locate(&self, _: &str, _: &Path) -> PortResult<PathBuf> {
                Err("none".into())
            }
        }
        let p = open_project(&dir.join("Game.uproject"), &NoEngine).unwrap();
        assert_eq!(p.targets, ["GameEditor"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod scm_tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::diff::LineKind;
    use crate::ports::PortResult;
    use crate::scm::{ChangeKind, FileStatus, WorkingCopyInfo};

    struct FakeScm;
    impl SourceControl for FakeScm {
        fn name(&self) -> &'static str {
            "Fake"
        }
        fn working_copy_root(&self, dir: &Path) -> Option<PathBuf> {
            Some(dir.to_path_buf())
        }
        fn info(&self, _: &Path) -> PortResult<WorkingCopyInfo> {
            Ok(WorkingCopyInfo {
                url: "https://host/svn/game/trunk".into(),
                branch: Some("^/trunk".into()),
                revision: Some("712".into()),
                last_change: None,
            })
        }
        fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>> {
            Ok(vec![FileStatus {
                path: root.join("a.cpp"),
                kind: ChangeKind::Modified,
            }])
        }
        fn base_text(&self, file: &Path) -> PortResult<Option<String>> {
            Ok(file.ends_with("a.cpp").then(|| "one\ntwo\n".to_string()))
        }
    }

    #[test]
    fn summarises_the_working_copy() {
        match scm_summary(&FakeScm, Path::new(r"D:p")) {
            ScmSummary::Ready { provider, info } => {
                assert_eq!(provider, "Fake");
                assert_eq!(info.branch.as_deref(), Some("^/trunk"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Claims the working copy rooted at a fixed folder.
    struct RootedScm(&'static str, &'static str);
    impl SourceControl for RootedScm {
        fn name(&self) -> &'static str {
            self.0
        }
        fn working_copy_root(&self, dir: &Path) -> Option<PathBuf> {
            dir.starts_with(self.1).then(|| PathBuf::from(self.1))
        }
        fn info(&self, _: &Path) -> PortResult<WorkingCopyInfo> {
            unimplemented!()
        }
        fn status(&self, _: &Path) -> PortResult<Vec<FileStatus>> {
            unimplemented!()
        }
        fn base_text(&self, _: &Path) -> PortResult<Option<String>> {
            unimplemented!()
        }
    }

    #[test]
    fn detects_the_nearest_working_copy() {
        let scms: Vec<Arc<dyn SourceControl>> = vec![
            Arc::new(RootedScm("Git", r"D:\svn\game")),
            Arc::new(RootedScm("Subversion", r"D:\svn")),
        ];
        let name = |root: &str| detect_source_control(&scms, Path::new(root)).map(|s| s.name());
        assert_eq!(name(r"D:\svn\game\Proj"), Some("Git"));
        assert_eq!(name(r"D:\svn\other"), Some("Subversion"));
        assert_eq!(name(r"D:\elsewhere"), None);

        let tied: Vec<Arc<dyn SourceControl>> = vec![
            Arc::new(RootedScm("Git", r"D:\p")),
            Arc::new(RootedScm("Subversion", r"D:\p")),
        ];
        let first = detect_source_control(&tied, Path::new(r"D:\p\Proj")).unwrap();
        assert_eq!(first.name(), "Git");
    }

    #[test]
    fn status_and_diff_go_through_the_port() {
        let root = PathBuf::from(r"D:\p");
        let status = working_copy_status(&FakeScm, &root).unwrap();
        assert_eq!(status.of(&root.join("a.cpp")), Some(ChangeKind::Modified));

        let d = diff_against_base(&FakeScm, &root.join("a.cpp"), "one\n2\n").unwrap();
        assert_eq!((d.added, d.removed), (1, 1));

        // No base: everything is added.
        let d = diff_against_base(&FakeScm, &root.join("new.cpp"), "x\ny\n").unwrap();
        assert!(d.hunks[0].lines.iter().all(|l| l.kind == LineKind::Added));
    }
}
