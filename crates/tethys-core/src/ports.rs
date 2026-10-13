//! Ports: traits the core owns and adapters implement.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::automation::{AutomationTest, TestEvent, TestInstance};
use crate::debug::{DebugEvent, ProcessInfo, SourceBreakpoint};
use crate::domain::{AdapterKind, AgentProfile, Project, SessionId};
use crate::scm::{Commit, FileStatus, WorkingCopyInfo};
use crate::unreal::{CommandSpec, Configuration};
use crate::update::{Release, Version};

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
    /// Extra arguments for every editor launch, as typed (see `unreal::split_args`).
    /// [`crate::unreal::DEFAULT_EDITOR_ARGS`] until the user sets them.
    fn editor_args(&self) -> PortResult<String>;
    fn set_editor_args(&self, args: &str) -> PortResult<()>;
    /// Extra arguments for the headless editor that runs tests, as typed.
    /// [`crate::unreal::DEFAULT_TEST_EDITOR_ARGS`] until the user sets them.
    fn test_editor_args(&self) -> PortResult<String>;
    fn set_test_editor_args(&self, args: &str) -> PortResult<()>;
    /// Whether the main area lists its tabs down the side instead of along the top.
    fn vertical_tabs(&self) -> PortResult<bool>;
    fn set_vertical_tabs(&self, vertical: bool) -> PortResult<()>;
    /// The release the user chose not to be offered again, if any.
    fn skipped_version(&self) -> PortResult<Option<Version>>;
    fn set_skipped_version(&self, version: Option<Version>) -> PortResult<()>;
}

/// A source control system (Subversion and git now; Perforce later).
///
/// Tethys never commits, reverts or updates. The only working-copy changes it
/// makes are renames and deletes the user asks for, so history follows a
/// renamed file and a deletion shows as one.
pub trait SourceControl: Send + Sync {
    /// Display name, e.g. "Subversion".
    fn name(&self) -> &'static str;
    /// The root of the working copy this system manages that contains `dir`,
    /// if any. Only looks at the disk, so it's cheap enough for the UI thread.
    fn working_copy_root(&self, dir: &Path) -> Option<PathBuf>;
    /// Whether `dir` is inside a working copy this system manages.
    fn is_working_copy(&self, dir: &Path) -> bool {
        self.working_copy_root(dir).is_some()
    }
    /// What `base_text` returns, for display: "BASE" for Subversion.
    fn base_label(&self) -> &'static str {
        "BASE"
    }
    /// Where the working copy at `root` points (URL, branch, revision).
    fn info(&self, root: &Path) -> PortResult<WorkingCopyInfo>;
    /// Changed and unversioned files under `root`. Paths are absolute.
    fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>>;
    /// The file as last checked out (BASE, or HEAD for git), or `None` if it
    /// has no base (unversioned or newly added).
    fn base_text(&self, file: &Path) -> PortResult<Option<String>>;
    /// Whether the file or folder at `path` is under source control (a
    /// folder: anything in it is).
    fn is_versioned(&self, path: &Path) -> PortResult<bool>;
    /// Renames a versioned file or folder, on disk and in source control.
    fn rename(&self, from: &Path, to: &Path) -> PortResult<()>;
    /// Deletes a versioned file or folder, on disk and in source control,
    /// local edits included.
    fn delete(&self, path: &Path) -> PortResult<()>;
    /// The commits that changed `file`, newest first, at most `limit`,
    /// following it back through renames.
    fn history(&self, file: &Path, limit: usize) -> PortResult<Vec<Commit>>;
    /// `file`'s text before and after `commit`, or `None` where it didn't
    /// exist (added or deleted by the commit).
    fn change(&self, file: &Path, commit: &Commit) -> PortResult<(Option<String>, Option<String>)>;
}

/// Starts programs that outlive Tethys, like the Unreal Editor.
pub trait ProcessLauncher {
    fn spawn_detached(&self, command: &CommandSpec, cwd: &Path) -> PortResult<()>;
}

/// Asks the running Unreal Editor to Live Code: recompile changed C++ and
/// patch it into the running editor.
pub trait LiveCoding: Send + Sync {
    /// Starts a Live Coding compile. Progress shows in the editor's own Live
    /// Coding window. Fails if no editor is running.
    fn compile(&self) -> PortResult<()>;
}

/// Receives debug events; called from the debugger's thread.
pub type DebugEventSink = Arc<dyn Fn(DebugEvent) + Send + Sync>;

/// Attaches a native debugger to a running program (DbgEng now).
pub trait Debugger: Send + Sync {
    /// Processes that could be debugged.
    fn processes(&self) -> PortResult<Vec<ProcessInfo>>;
    /// Attaches to `pid`, with `breakpoints` set before it runs on. Events,
    /// starting with `Attached`, go to `events`.
    fn attach(
        &self,
        pid: u32,
        breakpoints: Vec<SourceBreakpoint>,
        events: DebugEventSink,
    ) -> PortResult<Box<dyn DebugSession>>;
    /// Starts `command` in `cwd` under the debugger, with `breakpoints` set
    /// before it runs, so breakpoints in startup code are hit.
    fn launch(
        &self,
        command: &CommandSpec,
        cwd: &Path,
        breakpoints: Vec<SourceBreakpoint>,
        events: DebugEventSink,
    ) -> PortResult<Box<dyn DebugSession>>;
}

/// One attached program. Every call returns at once; results arrive as
/// events. Dropping it detaches: it never kills the program.
pub trait DebugSession: Send {
    /// The debugged process.
    fn pid(&self) -> u32;
    /// Replaces every breakpoint with `breakpoints`.
    fn set_breakpoints(&self, breakpoints: Vec<SourceBreakpoint>);
    /// Continues after a stop.
    fn resume(&self);
    fn pause(&self);
    /// Detaches, leaving the program running. Ends with `Ended(Detached)`.
    fn detach(&self);
}

/// Receives test events; called from the test session's thread.
pub type TestEventSink = Arc<dyn Fn(TestEvent) + Send + Sync>;

/// Reaches Unreal Editors' automation workers, to list and run their tests
/// (the UDP message bus now, as Session Frontend does).
pub trait TestRunner: Send + Sync {
    /// Editors on this machine that can run tests. Takes a few seconds.
    fn instances(&self) -> PortResult<Vec<TestInstance>>;
    /// Connects to a running editor's automation worker. Events, ending with
    /// `Connected` and `Tests` once it answers, go to `events`.
    fn connect(
        &self,
        instance: &TestInstance,
        events: TestEventSink,
    ) -> PortResult<Box<dyn TestSession>>;
    /// Starts `command` (a headless editor) in `cwd` and connects to it once
    /// it's up. The editor stops when the session ends.
    fn launch(
        &self,
        command: &CommandSpec,
        cwd: &Path,
        events: TestEventSink,
    ) -> PortResult<Box<dyn TestSession>>;
}

/// A connection to one editor's automation worker. Every call returns at
/// once; results arrive as events. Dropping it disconnects.
pub trait TestSession: Send {
    /// Lists the tests again, e.g. after a build.
    fn refresh(&self);
    /// Runs `tests` one after another. Ignored while a run is going.
    fn run(&self, tests: Vec<AutomationTest>);
    /// Stops the run after the current test.
    fn stop(&self);
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

/// Where released builds come from (GitHub Releases now).
pub trait UpdateSource: Send + Sync {
    /// The latest published release, or `None` if there isn't one.
    fn latest(&self) -> PortResult<Option<Release>>;
    /// Downloads the release's exe to `dest` and checks its SHA-256. On any
    /// failure, including a hash mismatch, `dest` is removed.
    fn download(&self, release: &Release, dest: &Path) -> PortResult<()>;
}

/// Replaces the running program with a downloaded build.
pub trait SelfInstaller: Send + Sync {
    /// Where to download the new exe: next to the current one, so the swap
    /// is a rename on the same volume.
    fn staging_path(&self) -> PortResult<PathBuf>;
    /// Makes `new_exe` the program that runs next time, and removes it.
    fn install(&self, new_exe: &Path) -> PortResult<()>;
    /// Starts the installed program with `args`, independent of this process.
    fn relaunch(&self, args: &[String]) -> PortResult<()>;
}
