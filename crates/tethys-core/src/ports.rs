//! Ports: traits the core owns and adapters implement.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::domain::{AdapterKind, AgentProfile, Project, SessionId};
use crate::scm::FileStatus;
use crate::unreal::{CommandSpec, Configuration};

pub type PortError = Box<dyn std::error::Error + Send + Sync>;
pub type PortResult<T> = Result<T, PortError>;

/// Turns a project's `EngineAssociation` into an engine install path.
pub trait EngineLocator {
    /// `uproject` is the project file, used to find native (in-tree) engines.
    fn locate(&self, association: &str, uproject: &Path) -> PortResult<PathBuf>;
}

/// Agent profiles and recent projects.
pub trait ConfigStore {
    /// Configured agent profiles. Empty when nothing is configured.
    fn agent_profiles(&self) -> PortResult<Vec<AgentProfile>>;
    /// Recently opened `.uproject` paths, most recent first.
    fn recent_projects(&self) -> PortResult<Vec<PathBuf>>;
    fn set_recent_projects(&self, projects: &[PathBuf]) -> PortResult<()>;
    /// The build configuration last chosen, for builds and editor launches.
    fn build_configuration(&self) -> PortResult<Configuration>;
    fn set_build_configuration(&self, configuration: Configuration) -> PortResult<()>;
}

/// A source control system (Subversion now; Perforce or git later).
///
/// Read-only: Tethys shows what changed but never commits, reverts or updates.
pub trait SourceControl: Send + Sync {
    /// Display name, e.g. "Subversion".
    fn name(&self) -> &'static str;
    /// Whether `dir` is inside a working copy this system manages.
    fn is_working_copy(&self, dir: &Path) -> bool;
    /// Changed and unversioned files under `root`. Paths are absolute.
    fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>>;
    /// The file as last checked out (BASE), or `None` if it has no base
    /// (unversioned or newly added).
    fn base_text(&self, file: &Path) -> PortResult<Option<String>>;
}

/// Starts programs that outlive Tethys, like the Unreal Editor.
pub trait ProcessLauncher {
    fn spawn_detached(&self, command: &CommandSpec, cwd: &Path) -> PortResult<()>;
}

/// Receives session events; called from any thread.
pub type EventSink = Arc<dyn Fn(SessionId, SessionEvent) + Send + Sync>;

/// Starts agent sessions in a project.
pub trait AgentHost {
    fn kind(&self) -> AdapterKind;
    fn start(
        &self,
        id: SessionId,
        profile: &AgentProfile,
        project: &Project,
        events: EventSink,
    ) -> PortResult<Box<dyn AgentSession>>;
}

/// One running agent process. Dropping it stops the agent.
pub trait AgentSession: Send {
    fn id(&self) -> SessionId;
    fn status(&self) -> SessionStatus;
    fn stop(&mut self) -> PortResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Running,
    Exited(i32),
}

/// Pushed by sessions; the UI doesn't poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Started,
    TitleChanged(String),
    Bell,
    Exited(i32),
}
