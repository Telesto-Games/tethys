//! Use cases: what the app asks the core to do.

use std::path::{Path, PathBuf};

use crate::domain::{AdapterKind, AgentProfile, DomainError, Project, ProjectError, SessionId};
use crate::ports::{
    AgentHost, AgentSession, ConfigStore, EngineLocator, EventSink, PortError, ProcessLauncher,
};
use crate::unreal::{self, Configuration, ToolError};

/// How many recent projects to remember.
pub const MAX_RECENT_PROJECTS: usize = 10;

/// Reads and parses a `.uproject`, then resolves its engine.
///
/// An unresolved engine is not an error: the project still opens and
/// `Project::engine` says what went wrong.
pub fn open_project(path: &Path, locator: &dyn EngineLocator) -> Result<Project, ProjectError> {
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

/// Starts the Unreal Editor on the project, detached from Tethys.
pub fn launch_editor(
    project: &Project,
    configuration: Configuration,
    launcher: &dyn ProcessLauncher,
) -> Result<(), ToolRunError> {
    let cmd = unreal::editor_command(project, configuration)?;
    launcher
        .spawn_detached(&cmd, project.root())
        .map_err(|source| ToolRunError::Launch {
            program: cmd.program.display().to_string(),
            source,
        })
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
            let project = if path.is_file() {
                open_project(&path, locator).map_err(|e| e.to_string())
            } else {
                Err("file not found".into())
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

/// Configured agent profiles, falling back to the built-in Claude profile.
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
        profiles.push(AgentProfile::claude());
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
    fn forget_and_list_recent_projects() {
        let good = temp_uproject("Good.uproject", r#"{"EngineAssociation":"5.6"}"#);
        let missing = PathBuf::from(r"Z:\gone\Gone.uproject");
        let config = FakeConfig::default();
        remember_project(&config, &missing).unwrap();
        remember_project(&config, &good).unwrap();

        let recent = recent_projects(&config, &FakeLocator);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].project.as_ref().unwrap().name(), "Good");
        assert_eq!(recent[1].project.as_ref().unwrap_err(), "file not found");

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
    fn profiles_fall_back_to_claude_and_drop_invalid() {
        let mut bad = AgentProfile::claude();
        bad.adapter = AdapterKind::Acp;
        let config = FakeConfig {
            profiles: vec![bad],
            ..Default::default()
        };
        let (profiles, problems) = agent_profiles(&config);
        assert_eq!(profiles, [AgentProfile::claude()]);
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

    #[test]
    fn launch_editor_runs_detached_in_project_root() {
        let launcher = FakeLauncher::default();
        launch_editor(&project(), Configuration::Development, &launcher).unwrap();
        let calls = launcher.0.lock().unwrap();
        assert_eq!(
            calls[0].0.program,
            Path::new(r"D:\UE\Engine\Binaries\Win64\UnrealEditor.exe")
        );
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
