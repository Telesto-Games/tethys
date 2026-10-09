//! Debugging the Unreal Editor: what the `Debugger` port talks about, and the
//! Unreal-specific decisions around it (which process is the editor, which
//! module owns a source file, where a frame's source is on this machine).

use std::path::{Component, Path, PathBuf};

/// A process that could be debugged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The executable's file name, e.g. `UnrealEditor.exe`.
    pub exe: String,
    /// Free text from the OS that includes the command line, used to tell
    /// editors apart. May be empty.
    pub details: String,
}

/// Chosen by Tethys, so the UI and the debugger agree on which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BreakpointId(pub u32);

/// A breakpoint as the debugger should set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBreakpoint {
    pub id: BreakpointId,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    /// The Unreal module whose DLL holds the code (see [`module_for_source`]).
    pub module: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakpointState {
    /// Not sent to a debugger yet.
    Unset,
    /// In the code; it will stop there.
    Bound,
    /// Waiting for its module to load.
    Pending,
    Failed(String),
}

/// One frame of a call stack, innermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    /// `Module!Function+0x1a`, or an address when there are no symbols.
    pub function: String,
    pub file: Option<PathBuf>,
    /// 1-based.
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// A breakpoint Tethys set.
    Breakpoint(BreakpointId),
    /// A breakpoint instruction in the program: UE's `check` and `ensure`
    /// (with a debugger attached), or `__debugbreak()`.
    DebugBreak,
    /// Any other exception, described by the debugger, e.g.
    /// "Access violation - code c0000005 (first chance)".
    Exception(String),
    /// The user paused the program.
    Pause,
}

impl StopReason {
    pub fn describe(&self) -> String {
        match self {
            StopReason::Breakpoint(_) => "Breakpoint".into(),
            StopReason::DebugBreak => "Debug break (check, ensure or __debugbreak)".into(),
            StopReason::Exception(e) => e.clone(),
            StopReason::Pause => "Paused".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEnd {
    /// Detached on request; the program keeps running.
    Detached,
    /// The program exited.
    Exited,
    Failed(String),
}

/// Pushed by a debug session from its own thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEvent {
    /// Attached and about to let the program run.
    Attached,
    Running,
    Stopped {
        reason: StopReason,
        /// OS thread id of the thread shown.
        thread: u32,
        frames: Vec<StackFrame>,
    },
    /// The state of every breakpoint, after each change.
    Breakpoints(Vec<(BreakpointId, BreakpointState)>),
    /// The session is over; no more events follow.
    Ended(DebugEnd),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FindEditorError {
    #[error("no Unreal Editor is running. Launch the editor first")]
    NoneRunning,
    #[error("{0} Unreal Editors are running and none of them has {1} open")]
    Ambiguous(usize, String),
}

/// Whether `exe` is an Unreal Editor executable (any configuration), not the
/// commandlet runner.
pub fn is_editor_exe(exe: &str) -> bool {
    let exe = exe.to_ascii_lowercase();
    exe.starts_with("unrealeditor") && exe.ends_with(".exe") && !exe.starts_with("unrealeditor-cmd")
}

/// The editor that has `uproject` open: one whose command line names it, or
/// the only editor running.
pub fn find_editor<'a>(
    processes: &'a [ProcessInfo],
    uproject: &Path,
) -> Result<&'a ProcessInfo, FindEditorError> {
    let editors: Vec<&ProcessInfo> = processes.iter().filter(|p| is_editor_exe(&p.exe)).collect();
    let wanted = uproject.to_string_lossy().to_ascii_lowercase();
    let file_name = uproject
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let names = |p: &&&ProcessInfo, needle: &str| {
        !needle.is_empty() && p.details.to_ascii_lowercase().contains(needle)
    };
    // The full path is best; a bare file name catches relative command lines.
    if let Some(p) = editors.iter().find(|p| names(p, &wanted)) {
        return Ok(p);
    }
    let by_name: Vec<_> = editors.iter().filter(|p| names(p, &file_name)).collect();
    if let [only] = by_name.as_slice() {
        return Ok(only);
    }
    match editors.as_slice() {
        [] => Err(FindEditorError::NoneRunning),
        [only] => Ok(only),
        many => Err(FindEditorError::Ambiguous(many.len(), file_name)),
    }
}

/// The Unreal module that compiles `file`: the nearest folder above it that
/// holds `<Folder>.Build.cs`. `exists` checks a path on disk.
pub fn module_for_source(file: &Path, exists: impl Fn(&Path) -> bool) -> Option<String> {
    file.ancestors().skip(1).find_map(|dir| {
        let name = dir.file_name()?.to_str()?;
        exists(&dir.join(format!("{name}.Build.cs"))).then(|| name.to_string())
    })
}

/// Whether a loaded image (file name without extension) is `module`'s editor
/// DLL: `UnrealEditor-<Module>` or `UnrealEditor-<Module>-<Platform>-<Config>`.
pub fn is_module_image(module: &str, image_stem: &str) -> bool {
    let stem = image_stem.to_ascii_lowercase();
    let base = format!("unrealeditor-{}", module.to_ascii_lowercase());
    stem == base
        || stem
            .strip_prefix(&base)
            .is_some_and(|rest| rest.starts_with('-') && rest.matches('-').count() == 2)
}

/// Where a frame's source file is on this machine. Engines from the Launcher
/// record paths on Epic's build machines (`D:\build\++UE5\Sync\Engine\…`); if
/// `file` doesn't exist and has an `Engine` folder in it, the same path under
/// the local `engine_root` is tried.
pub fn local_source(file: &Path, engine_root: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if exists(file) {
        return file.to_path_buf();
    }
    let components: Vec<Component> = file.components().collect();
    let engine = components
        .iter()
        .rposition(|c| c.as_os_str().eq_ignore_ascii_case("Engine"));
    if let Some(i) = engine {
        let mut local = engine_root.to_path_buf();
        local.extend(&components[i..]);
        if exists(&local) {
            return local;
        }
    }
    file.to_path_buf()
}

/// A breakpoint the user set, with its last known state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    pub state: BreakpointState,
}

/// The breakpoints of one window. They outlive debug sessions.
#[derive(Debug, Default)]
pub struct Breakpoints {
    items: Vec<Breakpoint>,
    next_id: u32,
}

impl Breakpoints {
    pub fn all(&self) -> &[Breakpoint] {
        &self.items
    }

    /// Adds a breakpoint at `file:line`, or removes the one already there.
    /// Returns whether one was added.
    pub fn toggle(&mut self, file: &Path, line: u32) -> bool {
        if let Some(i) = self
            .items
            .iter()
            .position(|b| b.line == line && same_path(&b.file, file))
        {
            self.items.remove(i);
            return false;
        }
        self.next_id += 1;
        self.items.push(Breakpoint {
            id: BreakpointId(self.next_id),
            file: file.to_path_buf(),
            line,
            state: BreakpointState::Unset,
        });
        true
    }

    pub fn remove(&mut self, id: BreakpointId) {
        self.items.retain(|b| b.id != id);
    }

    /// Lines with a breakpoint in `file`, sorted.
    pub fn lines_in(&self, file: &Path) -> Vec<u32> {
        let mut lines: Vec<u32> = self
            .items
            .iter()
            .filter(|b| same_path(&b.file, file))
            .map(|b| b.line)
            .collect();
        lines.sort_unstable();
        lines
    }

    /// Records states reported by the debugger.
    pub fn set_states(&mut self, states: &[(BreakpointId, BreakpointState)]) {
        for (id, state) in states {
            if let Some(b) = self.items.iter_mut().find(|b| b.id == *id) {
                b.state = state.clone();
            }
        }
    }

    /// Forgets debugger state, e.g. after detaching.
    pub fn reset_states(&mut self) {
        for b in &mut self.items {
            b.state = BreakpointState::Unset;
        }
    }

    /// What to send the debugger. `exists` checks a path on disk.
    pub fn requests(&self, exists: impl Fn(&Path) -> bool) -> Vec<SourceBreakpoint> {
        self.items
            .iter()
            .map(|b| SourceBreakpoint {
                id: b.id,
                file: b.file.clone(),
                line: b.line,
                module: module_for_source(&b.file, &exists),
            })
            .collect()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    // Windows paths are case-insensitive.
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, exe: &str, details: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            exe: exe.into(),
            details: details.into(),
        }
    }

    #[test]
    fn recognises_editor_executables() {
        assert!(is_editor_exe("UnrealEditor.exe"));
        assert!(is_editor_exe("UnrealEditor-Win64-DebugGame.exe"));
        assert!(!is_editor_exe("UnrealEditor-Cmd.exe"));
        assert!(!is_editor_exe("UnrealTraceServer.exe"));
    }

    #[test]
    fn picks_the_editor_with_the_project_open() {
        let processes = [
            process(
                1,
                "UnrealEditor.exe",
                r"Command Line: UnrealEditor.exe D:\a\A.uproject",
            ),
            process(
                2,
                "UnrealEditor.exe",
                r#"Command Line: "UnrealEditor.exe" "D:\G\Game.uproject""#,
            ),
            process(3, "notepad.exe", r"D:\G\Game.uproject"),
        ];
        let found = find_editor(&processes, Path::new(r"d:\g\GAME.uproject")).unwrap();
        assert_eq!(found.pid, 2);
    }

    #[test]
    fn matches_a_relative_project_by_file_name() {
        let processes = [
            process(
                1,
                "UnrealEditor.exe",
                "Command Line: UnrealEditor.exe Other.uproject",
            ),
            process(
                2,
                "UnrealEditor.exe",
                "Command Line: UnrealEditor.exe Game.uproject",
            ),
        ];
        let found = find_editor(&processes, Path::new(r"D:\G\Game.uproject")).unwrap();
        assert_eq!(found.pid, 2);
    }

    #[test]
    fn falls_back_to_the_only_editor() {
        let processes = [process(7, "UnrealEditor-Win64-DebugGame.exe", "")];
        let found = find_editor(&processes, Path::new(r"D:\G\Game.uproject")).unwrap();
        assert_eq!(found.pid, 7);
    }

    #[test]
    fn reports_no_editor_or_too_many() {
        let game = Path::new(r"D:\G\Game.uproject");
        assert_eq!(
            find_editor(&[process(1, "cmd.exe", "")], game),
            Err(FindEditorError::NoneRunning)
        );
        let two = [
            process(1, "UnrealEditor.exe", "x"),
            process(2, "UnrealEditor.exe", "y"),
        ];
        assert!(matches!(
            find_editor(&two, game),
            Err(FindEditorError::Ambiguous(2, _))
        ));
    }

    #[test]
    fn finds_the_module_by_its_build_cs() {
        let exists = |p: &Path| {
            p == Path::new(r"D:\G\Source\Game\Game.Build.cs")
                || p == Path::new(r"D:\G\Plugins\Fx\Source\FxRuntime\FxRuntime.Build.cs")
        };
        assert_eq!(
            module_for_source(Path::new(r"D:\G\Source\Game\Private\Ai\Brain.cpp"), exists),
            Some("Game".into())
        );
        assert_eq!(
            module_for_source(
                Path::new(r"D:\G\Plugins\Fx\Source\FxRuntime\Public\Fx.h"),
                exists
            ),
            Some("FxRuntime".into())
        );
        assert_eq!(
            module_for_source(Path::new(r"D:\G\Config\DefaultGame.ini"), exists),
            None
        );
    }

    #[test]
    fn matches_module_images_in_any_configuration() {
        assert!(is_module_image("Game", "UnrealEditor-Game"));
        assert!(is_module_image("game", "UnrealEditor-Game-Win64-DebugGame"));
        assert!(!is_module_image("Game", "UnrealEditor-GameTools"));
        assert!(!is_module_image("Game", "UnrealEditor-Game-Tools"));
        assert!(!is_module_image("Game", "UnrealEditor-Game-Win64"));
    }

    #[test]
    fn maps_build_machine_engine_paths_to_the_local_engine() {
        let local =
            Path::new(r"C:\UE_5.8\Engine\Source\Runtime\Core\Private\Misc\AssertionMacros.cpp");
        let exists = |p: &Path| p == local;
        let remote = Path::new(
            r"D:\build\++UE5\Sync\Engine\Source\Runtime\Core\Private\Misc\AssertionMacros.cpp",
        );
        assert_eq!(local_source(remote, Path::new(r"C:\UE_5.8"), exists), local);
        // Files that exist, or can't be mapped, are left alone.
        assert_eq!(local_source(local, Path::new(r"C:\Other"), exists), local);
        let game = Path::new(r"D:\G\Source\Game\Game.cpp");
        assert_eq!(local_source(game, Path::new(r"C:\UE_5.8"), exists), game);
    }

    #[test]
    fn toggling_adds_then_removes() {
        let mut bps = Breakpoints::default();
        let file = Path::new(r"D:\G\Source\Game\Game.cpp");
        assert!(bps.toggle(file, 10));
        assert!(bps.toggle(file, 4));
        assert_eq!(
            bps.lines_in(Path::new(r"d:\g\source\game\GAME.cpp")),
            vec![4, 10]
        );
        assert!(!bps.toggle(file, 10));
        assert_eq!(bps.lines_in(file), vec![4]);
        // Ids aren't reused, so late debugger reports can't hit a new breakpoint.
        assert!(bps.toggle(file, 10));
        assert_eq!(bps.all()[1].id, BreakpointId(3));
    }

    #[test]
    fn requests_carry_the_module_and_states_update() {
        let mut bps = Breakpoints::default();
        let file = Path::new(r"D:\G\Source\Game\Game.cpp");
        bps.toggle(file, 12);
        let requests = bps.requests(|p| p == Path::new(r"D:\G\Source\Game\Game.Build.cs"));
        assert_eq!(requests[0].module.as_deref(), Some("Game"));
        assert_eq!(requests[0].line, 12);

        bps.set_states(&[(BreakpointId(1), BreakpointState::Bound)]);
        assert_eq!(bps.all()[0].state, BreakpointState::Bound);
        bps.reset_states();
        assert_eq!(bps.all()[0].state, BreakpointState::Unset);
    }
}
