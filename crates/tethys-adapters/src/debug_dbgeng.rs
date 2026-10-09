//! `Debugger` on DbgEng, the engine behind WinDbg and cdb. It ships with
//! Windows (`System32\dbgeng.dll`), so Tethys redistributes nothing.
//!
//! DbgEng's objects aren't thread-safe, so each session runs on a thread of
//! its own that makes every call, and the UI talks to it over a channel. See
//! "Debugging the editor" in `docs/design.md`.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use tethys_core::debug::{DebugEvent, ProcessInfo, SourceBreakpoint};
use tethys_core::ports::{DebugEventSink, DebugSession, Debugger, PortResult};

/// How long dropping a session waits for the detach.
const DETACH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct DbgEng;

enum Command {
    SetBreakpoints(Vec<SourceBreakpoint>),
    Resume,
    Pause,
    Detach,
}

struct Session {
    commands: Sender<Command>,
    /// Disconnects when the session's thread ends.
    done: Receiver<()>,
}

impl DebugSession for Session {
    fn set_breakpoints(&self, breakpoints: Vec<SourceBreakpoint>) {
        let _ = self.commands.send(Command::SetBreakpoints(breakpoints));
    }

    fn resume(&self) {
        let _ = self.commands.send(Command::Resume);
    }

    fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
    }

    fn detach(&self) {
        let _ = self.commands.send(Command::Detach);
    }
}

impl Drop for Session {
    /// Detaches and waits for it, so a closing window never leaves breakpoints
    /// in the editor.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Detach);
        let _ = self.done.recv_timeout(DETACH_TIMEOUT);
    }
}

impl Debugger for DbgEng {
    fn processes(&self) -> PortResult<Vec<ProcessInfo>> {
        engine::processes()
    }

    /// Blocks until the attach completes, which can take a few seconds for
    /// the editor, so call it off the UI thread.
    fn attach(&self, pid: u32, events: DebugEventSink) -> PortResult<Box<dyn DebugSession>> {
        let (commands, received) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let (done_tx, done) = mpsc::channel::<()>();
        thread::Builder::new()
            .name(format!("tethys-debug-{pid}"))
            .spawn(move || {
                let _done = done_tx;
                let engine = match engine::Engine::attach(pid) {
                    Ok(engine) => {
                        let _ = ready_tx.send(Ok(()));
                        engine
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let end = engine.run(&received, &*events);
                events(DebugEvent::Ended(end));
            })?;
        match ready.recv() {
            Ok(Ok(())) => Ok(Box::new(Session { commands, done })),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("the debugger thread stopped unexpectedly".into()),
        }
    }
}

#[cfg(not(windows))]
mod engine {
    use std::sync::mpsc::Receiver;

    use tethys_core::debug::{DebugEnd, DebugEvent, ProcessInfo};
    use tethys_core::ports::PortResult;

    use super::Command;

    pub fn processes() -> PortResult<Vec<ProcessInfo>> {
        Err("debugging is only available on Windows".into())
    }

    pub struct Engine;

    impl Engine {
        pub fn attach(_: u32) -> Result<Self, String> {
            Err("debugging is only available on Windows".into())
        }

        pub fn run(self, _: &Receiver<Command>, _: &dyn Fn(DebugEvent)) -> DebugEnd {
            DebugEnd::Detached
        }
    }
}

#[cfg(windows)]
mod engine {
    use std::ffi::{CString, c_void};
    use std::mem::ManuallyDrop;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, TryRecvError};

    use tethys_core::debug::{
        self, BreakpointId, BreakpointState, DebugEnd, DebugEvent, ProcessInfo, SourceBreakpoint,
        StackFrame, StopReason,
    };
    use tethys_core::ports::PortResult;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, S_FALSE};
    use windows::Win32::System::Diagnostics::Debug::DebugBreakProcess;
    use windows::Win32::System::Diagnostics::Debug::Extensions::*;
    use windows::Win32::System::Diagnostics::Debug::SYMOPT_LOAD_LINES;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};
    use windows::core::{Interface, PCSTR};

    use super::Command;

    /// `int 3`: a breakpoint instruction, including the OS's break-in thread.
    const STATUS_BREAKPOINT: i32 = 0x8000_0003_u32 as i32;
    /// The same, from a 32-bit program under WOW64.
    const STATUS_WX86_BREAKPOINT: i32 = 0x4000_001F;
    /// The function the OS runs in a new thread to break into a running
    /// process, on attach and for `break_in`. Stops in it are ours.
    const BREAK_IN_THREAD: &str = "DbgUiRemoteBreakin";
    const INFINITE: u32 = u32::MAX;
    /// How long a wait lasts while the program runs, before checking for commands.
    const POLL_MS: u32 = 100;
    const MAX_FRAMES: usize = 128;

    fn failed(context: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |e| format!("{context}: {}", e.message())
    }

    /// Reads a NUL-terminated ANSI string out of `buf`.
    fn c_text(buf: &[u8]) -> String {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    pub fn processes() -> PortResult<Vec<ProcessInfo>> {
        // SAFETY: plain DbgEng calls on objects this function owns.
        unsafe {
            let client: IDebugClient = DebugCreate()?;
            let mut count = 0;
            client.GetRunningProcessSystemIds(0, None, Some(&mut count))?;
            // Room for processes that start in between.
            let mut ids = vec![0u32; count as usize + 64];
            client.GetRunningProcessSystemIds(0, Some(&mut ids), Some(&mut count))?;
            ids.truncate(count as usize);

            let quick = DEBUG_PROC_DESC_NO_PATHS
                | DEBUG_PROC_DESC_NO_SERVICES
                | DEBUG_PROC_DESC_NO_MTS_PACKAGES
                | DEBUG_PROC_DESC_NO_COMMAND_LINE
                | DEBUG_PROC_DESC_NO_SESSION_ID
                | DEBUG_PROC_DESC_NO_USER_NAME;
            let full = DEBUG_PROC_DESC_NO_SERVICES | DEBUG_PROC_DESC_NO_MTS_PACKAGES;
            let mut processes = Vec::new();
            for pid in ids {
                // Processes that exited, or that we may not open, are skipped.
                let Ok((exe, _)) = describe(&client, pid, quick) else {
                    continue;
                };
                // Reading command lines is slow, so only for the editors.
                let details = if debug::is_editor_exe(&exe) {
                    describe(&client, pid, full)
                        .map(|(_, d)| d)
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                processes.push(ProcessInfo { pid, exe, details });
            }
            Ok(processes)
        }
    }

    unsafe fn describe(
        client: &IDebugClient,
        pid: u32,
        flags: u32,
    ) -> windows::core::Result<(String, String)> {
        let mut exe = vec![0u8; 520];
        let mut details = vec![0u8; 16 * 1024];
        // SAFETY: the buffers outlive the call. Too-long text is truncated.
        unsafe {
            client.GetRunningProcessDescription(
                0,
                pid,
                flags,
                Some(&mut exe),
                None,
                Some(&mut details),
                None,
            )?;
        }
        Ok((c_text(&exe), c_text(&details)))
    }

    /// What the program did when a wait returned.
    enum Wait {
        TimedOut,
        Event,
        Exited,
    }

    /// What an event means for the user.
    enum Seen {
        /// Ours or uninteresting: carry on.
        Ignore,
        Stop(DebugEvent),
        Exited,
    }

    pub struct Engine {
        client: IDebugClient,
        control: IDebugControl,
        symbols: IDebugSymbols3,
        system: IDebugSystemObjects,
        /// For breaking in. DbgEng's own `SetInterrupt` only works from
        /// another thread while this one waits.
        process: HANDLE,
        wanted: Vec<SourceBreakpoint>,
        /// Breakpoint ids currently set in the engine.
        applied: Vec<u32>,
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            // SAFETY: `process` was opened by `attach` and is closed once.
            let _ = unsafe { CloseHandle(self.process) };
        }
    }

    impl Engine {
        /// Attaches and waits for the attach break. Returns stopped there.
        pub fn attach(pid: u32) -> Result<Self, String> {
            // SAFETY: DbgEng calls on objects this thread owns.
            unsafe {
                let client: IDebugClient = DebugCreate().map_err(failed("can't start DbgEng"))?;
                let control: IDebugControl = client.cast().map_err(failed("DbgEng"))?;
                let symbols: IDebugSymbols3 = client.cast().map_err(failed("DbgEng"))?;
                let system: IDebugSystemObjects = client.cast().map_err(failed("DbgEng"))?;
                let process = OpenProcess(PROCESS_ALL_ACCESS, false, pid)
                    .map_err(failed("can't open the process"))?;
                let engine = Engine {
                    client,
                    control,
                    symbols,
                    system,
                    process,
                    wanted: Vec::new(),
                    applied: Vec::new(),
                };
                engine
                    .control
                    .AddEngineOptions(DEBUG_ENGOPT_INITIAL_BREAK)
                    .map_err(failed("DbgEng"))?;
                engine
                    .symbols
                    .AddSymbolOptions(SYMOPT_LOAD_LINES)
                    .map_err(failed("DbgEng"))?;
                engine
                    .client
                    .AttachProcess(0, pid, DEBUG_ATTACH_DEFAULT)
                    .map_err(failed("attach failed"))?;
                match engine.wait(INFINITE)? {
                    Wait::Event => {}
                    Wait::Exited => return Err("the process exited while attaching".into()),
                    Wait::TimedOut => return Err("the attach didn't complete".into()),
                }
                // If Tethys exits without detaching, the editor keeps running.
                engine
                    .client
                    .SetProcessOptions(DEBUG_PROCESS_DETACH_ON_EXIT)
                    .map_err(failed("DbgEng"))?;
                engine.add_module_folders_to_symbol_path();
                Ok(engine)
            }
        }

        /// Serves commands until the session ends.
        pub fn run(
            mut self,
            commands: &Receiver<Command>,
            events: &dyn Fn(DebugEvent),
        ) -> DebugEnd {
            events(DebugEvent::Attached);
            match self.serve(commands, events) {
                Ok(end) => end,
                Err(e) => {
                    self.end_session(true);
                    DebugEnd::Failed(e)
                }
            }
        }

        fn serve(
            &mut self,
            commands: &Receiver<Command>,
            events: &dyn Fn(DebugEvent),
        ) -> Result<DebugEnd, String> {
            // Starts at the attach break, which the user doesn't see.
            let mut stopped = true;
            // Whether the user sees the current stop (else it's resumed once
            // the queued commands are handled).
            let mut shown = false;
            // A command that arrived while running, to handle once stopped.
            let mut pending: Option<Command> = None;
            let mut announced_running = false;
            loop {
                if stopped {
                    let command = match pending.take() {
                        Some(command) => command,
                        None if shown => commands.recv().unwrap_or(Command::Detach),
                        None => match commands.try_recv() {
                            Ok(command) => command,
                            Err(TryRecvError::Disconnected) => Command::Detach,
                            Err(TryRecvError::Empty) => {
                                self.go()?;
                                stopped = false;
                                if !announced_running {
                                    announced_running = true;
                                    events(DebugEvent::Running);
                                }
                                continue;
                            }
                        },
                    };
                    match command {
                        Command::SetBreakpoints(breakpoints) => {
                            self.wanted = breakpoints;
                            events(DebugEvent::Breakpoints(self.apply_breakpoints()));
                        }
                        Command::Resume => {
                            if shown {
                                shown = false;
                                announced_running = false;
                            }
                        }
                        Command::Pause => {
                            if !shown {
                                shown = true;
                                events(self.paused());
                            }
                        }
                        Command::Detach => return Ok(self.end_session(true)),
                    }
                    continue;
                }

                // Running: a command needs the program stopped first.
                let command = match commands.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Disconnected) => Some(Command::Detach),
                    Err(TryRecvError::Empty) => None,
                };
                if let Some(command) = command {
                    self.break_in()?;
                    pending = Some(command);
                }
                let timeout = if pending.is_some() { INFINITE } else { POLL_MS };
                match self.wait(timeout)? {
                    Wait::TimedOut => {}
                    Wait::Exited => return Ok(self.end_session(false)),
                    Wait::Event => {
                        stopped = true;
                        match self.inspect_event()? {
                            Seen::Ignore => {}
                            Seen::Exited => return Ok(self.end_session(false)),
                            Seen::Stop(event) => {
                                shown = true;
                                events(event);
                            }
                        }
                    }
                }
            }
        }

        fn wait(&self, timeout: u32) -> Result<Wait, String> {
            // SAFETY: DbgEng calls on objects this thread owns. The raw vtable
            // call is the same one `WaitForEvent` makes, minus folding S_FALSE
            // into Ok.
            unsafe {
                let waited = (Interface::vtable(&self.control).WaitForEvent)(
                    Interface::as_raw(&self.control),
                    DEBUG_WAIT_DEFAULT,
                    timeout,
                );
                // Timed out, with the program still running. The execution
                // status isn't reliable here: it can say BREAK.
                if waited == S_FALSE {
                    return Ok(Wait::TimedOut);
                }
                if self.control.GetExecutionStatus() == Ok(DEBUG_STATUS_NO_DEBUGGEE) {
                    return Ok(Wait::Exited);
                }
                waited.ok().map_err(failed("waiting for the program"))?;
                Ok(Wait::Event)
            }
        }

        /// Makes the OS start a thread in the program that hits a breakpoint;
        /// the next wait returns with it ([`BREAK_IN_THREAD`]).
        fn break_in(&self) -> Result<(), String> {
            // SAFETY: `process` is a live handle with all access.
            unsafe { DebugBreakProcess(self.process) }.map_err(failed("can't break in"))
        }

        fn go(&self) -> Result<(), String> {
            // SAFETY: DbgEng call on an object this thread owns.
            unsafe { self.control.SetExecutionStatus(DEBUG_STATUS_GO) }
                .map_err(failed("can't continue"))
        }

        /// Classifies the event the engine just stopped on.
        fn inspect_event(&self) -> Result<Seen, String> {
            let mut kind = 0;
            let (mut process, mut thread) = (0, 0);
            // The largest extra information we read; a breakpoint's id is its first u32.
            let mut info = DEBUG_LAST_EVENT_INFO_EXCEPTION::default();
            let mut description = vec![0u8; 1024];
            // SAFETY: `info` and `description` outlive the call, and the size
            // passed is `info`'s.
            unsafe {
                self.control
                    .GetLastEventInformation(
                        &mut kind,
                        &mut process,
                        &mut thread,
                        Some(&mut info as *mut _ as *mut c_void),
                        size_of::<DEBUG_LAST_EVENT_INFO_EXCEPTION>() as u32,
                        None,
                        Some(&mut description),
                        None,
                    )
                    .map_err(failed("DbgEng"))?;
            }
            let reason = match kind {
                DEBUG_EVENT_BREAKPOINT => {
                    // SAFETY: for breakpoints the extra information is a
                    // DEBUG_LAST_EVENT_INFO_BREAKPOINT, which starts with its id.
                    let id = unsafe { *(&info as *const _ as *const u32) };
                    if !self.applied.contains(&id) {
                        return Ok(Seen::Ignore);
                    }
                    StopReason::Breakpoint(BreakpointId(id))
                }
                DEBUG_EVENT_EXCEPTION => {
                    let code = info.ExceptionRecord.ExceptionCode.0;
                    if code == STATUS_BREAKPOINT || code == STATUS_WX86_BREAKPOINT {
                        StopReason::DebugBreak
                    } else {
                        StopReason::Exception(c_text(&description))
                    }
                }
                DEBUG_EVENT_EXIT_PROCESS => return Ok(Seen::Exited),
                _ => return Ok(Seen::Ignore),
            };
            let frames = self.stack();
            if reason == StopReason::DebugBreak
                && frames
                    .iter()
                    .take(4)
                    .any(|f| f.function.contains(BREAK_IN_THREAD))
            {
                return Ok(Seen::Ignore);
            }
            Ok(Seen::Stop(DebugEvent::Stopped {
                reason,
                thread: self.current_thread(),
                frames,
            }))
        }

        /// A pause stops in the OS's break-in thread, so show the main (game) thread.
        fn paused(&self) -> DebugEvent {
            let (mut id, mut system_id) = (0, 0);
            // SAFETY: DbgEng calls on objects this thread owns; the outputs
            // are single u32s and the count is 1.
            unsafe {
                if self
                    .system
                    .GetThreadIdsByIndex(0, 1, Some(&mut id), Some(&mut system_id))
                    .is_ok()
                {
                    let _ = self.system.SetCurrentThreadId(id);
                }
            }
            DebugEvent::Stopped {
                reason: StopReason::Pause,
                thread: self.current_thread(),
                frames: self.stack(),
            }
        }

        fn current_thread(&self) -> u32 {
            // SAFETY: DbgEng call on an object this thread owns.
            unsafe { self.system.GetCurrentThreadSystemId() }.unwrap_or(0)
        }

        /// The current thread's stack, innermost first.
        fn stack(&self) -> Vec<StackFrame> {
            let mut frames = vec![DEBUG_STACK_FRAME::default(); MAX_FRAMES];
            let mut filled = 0;
            // SAFETY: `frames` outlives the call.
            let walked = unsafe {
                self.control
                    .GetStackTrace(0, 0, 0, &mut frames, Some(&mut filled))
            };
            if walked.is_err() {
                return Vec::new();
            }
            frames.truncate(filled as usize);
            frames
                .iter()
                .enumerate()
                .map(|(i, frame)| {
                    let pc = frame.InstructionOffset;
                    // Outer frames hold return addresses; the call is just before.
                    let line_pc = if i == 0 { pc } else { pc.saturating_sub(1) };
                    let (file, line) = match self.line_at(line_pc) {
                        Some((file, line)) => (Some(file), Some(line)),
                        None => (None, None),
                    };
                    StackFrame {
                        function: self.name_at(pc).unwrap_or_else(|| format!("0x{pc:016x}")),
                        file,
                        line,
                    }
                })
                .collect()
        }

        fn name_at(&self, offset: u64) -> Option<String> {
            let mut name = vec![0u8; 1024];
            let mut displacement = 0;
            // SAFETY: the buffers outlive the call.
            unsafe {
                self.symbols
                    .GetNameByOffset(offset, Some(&mut name), None, Some(&mut displacement))
                    .ok()?;
            }
            let name = c_text(&name);
            Some(if displacement > 0 {
                format!("{name}+0x{displacement:x}")
            } else {
                name
            })
        }

        fn line_at(&self, offset: u64) -> Option<(PathBuf, u32)> {
            let mut line = 0;
            let mut file = vec![0u8; 1024];
            // SAFETY: the buffers outlive the call.
            unsafe {
                self.symbols
                    .GetLineByOffset(offset, Some(&mut line), Some(&mut file), None, None)
                    .ok()?;
            }
            Some((PathBuf::from(c_text(&file)), line))
        }

        /// Loaded modules as (image file stem, DbgEng module name), e.g.
        /// ("UnrealEditor-Game", "UnrealEditor_Game").
        fn modules(&self) -> Vec<(PathBuf, String)> {
            let (mut loaded, mut unloaded) = (0, 0);
            // SAFETY: DbgEng call on an object this thread owns.
            if unsafe { self.symbols.GetNumberModules(&mut loaded, &mut unloaded) }.is_err() {
                return Vec::new();
            }
            (0..loaded)
                .filter_map(|i| {
                    let image = self.module_name(DEBUG_MODNAME_IMAGE, i)?;
                    let module = self.module_name(DEBUG_MODNAME_MODULE, i)?;
                    Some((PathBuf::from(image), module))
                })
                .collect()
        }

        fn module_name(&self, which: u32, index: u32) -> Option<String> {
            let mut name = vec![0u8; 1024];
            // SAFETY: the buffer outlives the call.
            unsafe {
                self.symbols
                    .GetModuleNameString(which, index, 0, Some(&mut name), None)
                    .ok()?;
            }
            Some(c_text(&name))
        }

        /// UBT writes each PDB next to its DLL, but Launcher engines record
        /// build-machine PDB paths, so search every loaded module's folder.
        fn add_module_folders_to_symbol_path(&self) {
            let mut folders: Vec<String> = Vec::new();
            for (image, _) in self.modules() {
                let Some(folder) = image.parent().map(|f| f.display().to_string()) else {
                    continue;
                };
                if !folder.is_empty() && !folders.iter().any(|f| f.eq_ignore_ascii_case(&folder)) {
                    folders.push(folder);
                }
            }
            if let Ok(path) = CString::new(folders.join(";")) {
                // SAFETY: `path` outlives the call.
                let _ = unsafe {
                    self.symbols
                        .AppendSymbolPath(PCSTR::from_raw(path.as_ptr().cast()))
                };
            }
        }

        /// The engine's breakpoint `id`. The engine owns breakpoint objects and
        /// doesn't count references to them, so releasing one frees it: the
        /// wrapper must never be dropped.
        fn breakpoint(&self, id: u32) -> Option<ManuallyDrop<IDebugBreakpoint>> {
            // SAFETY: DbgEng call on an object this thread owns.
            unsafe { self.control.GetBreakpointById(id) }
                .ok()
                .map(ManuallyDrop::new)
        }

        /// Replaces the engine's breakpoints with `wanted`. Must be stopped.
        fn apply_breakpoints(&mut self) -> Vec<(BreakpointId, BreakpointState)> {
            for id in std::mem::take(&mut self.applied) {
                if let Some(bp) = self.breakpoint(id) {
                    // SAFETY: DbgEng call on an object this thread owns. `bp`
                    // is freed by this and never released.
                    let _ = unsafe { self.control.RemoveBreakpoint(&*bp) };
                }
            }
            let modules: Vec<(String, String)> = self
                .modules()
                .into_iter()
                .filter_map(|(image, name)| {
                    Some((image.file_stem()?.to_string_lossy().into_owned(), name))
                })
                .collect();
            let wanted = self.wanted.clone();
            wanted
                .iter()
                .map(|b| {
                    let state = self.set_breakpoint(b, &modules);
                    if !matches!(state, BreakpointState::Failed(_)) {
                        self.applied.push(b.id.0);
                    }
                    (b.id, state)
                })
                .collect()
        }

        fn set_breakpoint(
            &self,
            b: &SourceBreakpoint,
            modules: &[(String, String)],
        ) -> BreakpointState {
            let Some(module) = &b.module else {
                return BreakpointState::Failed(
                    "not in an Unreal module (no .Build.cs above it)".into(),
                );
            };
            let Some(file) = b.file.file_name().map(|f| f.to_string_lossy()) else {
                return BreakpointState::Failed("not a file".into());
            };
            // DbgEng's name for the loaded DLL, or the name it will have.
            let name = modules
                .iter()
                .find(|(stem, _)| debug::is_module_image(module, stem))
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| format!("UnrealEditor_{module}"));
            let not_found =
                || BreakpointState::Failed(format!("no code for {file}:{} in {name}", b.line));
            // `bu` (unresolved) binds again whenever the module loads.
            let Ok(command) = CString::new(format!("bu{} `{name}!{file}:{}`", b.id.0, b.line))
            else {
                return not_found();
            };
            // SAFETY: `command` outlives the call; the rest are DbgEng calls
            // on objects this thread owns.
            unsafe {
                let set = self.control.Execute(
                    DEBUG_OUTCTL_IGNORE,
                    PCSTR::from_raw(command.as_ptr().cast()),
                    DEBUG_EXECUTE_NOT_LOGGED | DEBUG_EXECUTE_NO_REPEAT,
                );
                let flags = self.breakpoint(b.id.0).map(|bp| bp.GetFlags());
                match (set, flags) {
                    (_, Some(Ok(flags))) if flags & DEBUG_BREAKPOINT_DEFERRED != 0 => {
                        BreakpointState::Pending
                    }
                    (Ok(()), Some(Ok(_))) => BreakpointState::Bound,
                    _ => not_found(),
                }
            }
        }

        /// Ends the session. `detach` leaves the program running; otherwise
        /// it has already exited. Must be stopped (or exited).
        fn end_session(&mut self, detach: bool) -> DebugEnd {
            // SAFETY: DbgEng calls on objects this thread owns.
            unsafe {
                if detach {
                    let _ = self.client.DetachProcesses();
                }
                let _ = self.client.EndSession(DEBUG_END_PASSIVE);
            }
            if detach {
                DebugEnd::Detached
            } else {
                DebugEnd::Exited
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lists_processes_including_this_one() {
            let me = std::process::id();
            let processes = processes().unwrap();
            assert!(processes.iter().any(|p| p.pid == me && !p.exe.is_empty()));
        }

        #[test]
        fn reads_c_strings() {
            assert_eq!(c_text(b"abc\0junk"), "abc");
            assert_eq!(c_text(b"full"), "full");
        }

        /// Attaching to a real process, breaking in and detaching leaves it
        /// running. Starts and kills a `ping` of its own.
        #[test]
        fn attaches_pauses_and_detaches() {
            use std::sync::{Arc, Mutex};
            use std::time::{Duration, Instant};

            use tethys_core::ports::Debugger;

            let mut child = std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = {
                let seen = seen.clone();
                Arc::new(move |e: DebugEvent| seen.lock().unwrap().push(e))
            };
            let session = super::super::DbgEng.attach(child.id(), sink).unwrap();
            let wait_for = |pred: &dyn Fn(&DebugEvent) -> bool| {
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(20) {
                    if seen.lock().unwrap().iter().any(pred) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!("timed out; saw {:?}", seen.lock().unwrap());
            };
            wait_for(&|e| *e == DebugEvent::Running);
            session.pause();
            wait_for(
                &|e| matches!(e, DebugEvent::Stopped { reason: StopReason::Pause, frames, .. } if !frames.is_empty()),
            );
            session.resume();
            session.detach();
            wait_for(&|e| *e == DebugEvent::Ended(DebugEnd::Detached));
            // Still alive after the detach.
            assert!(child.try_wait().unwrap().is_none());
            child.kill().unwrap();
        }

        /// Against a running Unreal Editor: a breakpoint on code that runs
        /// every frame binds and is hit, a pause shows the game thread, and
        /// detaching leaves the editor running.
        ///
        /// `TETHYS_DEBUG_UPROJECT=<.uproject> TETHYS_DEBUG_BREAKPOINT=<file>:<line>
        /// cargo test -p tethys-adapters live_editor -- --ignored --nocapture`
        #[test]
        #[ignore]
        fn live_editor() {
            use std::path::Path;
            use std::sync::{Arc, Mutex};
            use std::time::{Duration, Instant};

            use tethys_core::ports::Debugger;

            let (Some(uproject), Some(breakpoint)) = (
                std::env::var_os("TETHYS_DEBUG_UPROJECT"),
                std::env::var("TETHYS_DEBUG_BREAKPOINT").ok(),
            ) else {
                return;
            };
            let (file, line) = breakpoint.rsplit_once(':').unwrap();
            let file = PathBuf::from(file);
            let module = debug::module_for_source(&file, Path::is_file);
            eprintln!("breakpoint in module {module:?}");

            let running = processes().unwrap();
            let editor = debug::find_editor(&running, Path::new(&uproject)).unwrap();
            eprintln!("attaching to {} ({})", editor.exe, editor.pid);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = {
                let seen = seen.clone();
                Arc::new(move |e: DebugEvent| seen.lock().unwrap().push(e))
            };
            let started = Instant::now();
            let session = super::super::DbgEng.attach(editor.pid, sink).unwrap();
            let wait_for = |what: &str, pred: &dyn Fn(&DebugEvent) -> bool| -> DebugEvent {
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(120) {
                    if let Some(e) = seen.lock().unwrap().iter().find(|e| pred(e)) {
                        eprintln!("{what} after {:?}", start.elapsed());
                        return e.clone();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!(
                    "timed out waiting for {what}; saw {:?}",
                    seen.lock().unwrap()
                );
            };
            let print = |event: &DebugEvent| {
                if let DebugEvent::Stopped { reason, frames, .. } = event {
                    eprintln!("  stopped: {reason:?}");
                    for f in frames.iter().take(8) {
                        eprintln!("    {} {:?}:{:?}", f.function, f.file, f.line);
                    }
                }
            };
            wait_for("running", &|e| *e == DebugEvent::Running);
            eprintln!("attached in {:?}", started.elapsed());

            session.set_breakpoints(vec![SourceBreakpoint {
                id: BreakpointId(1),
                file: file.clone(),
                line: line.parse().unwrap(),
                module,
            }]);
            let states = wait_for("breakpoint states", &|e| {
                matches!(e, DebugEvent::Breakpoints(_))
            });
            eprintln!("  {states:?}");
            let hit = wait_for("breakpoint hit", &|e| {
                matches!(
                    e,
                    DebugEvent::Stopped {
                        reason: StopReason::Breakpoint(_),
                        ..
                    }
                )
            });
            print(&hit);

            seen.lock().unwrap().clear();
            session.set_breakpoints(Vec::new());
            session.resume();
            wait_for("running again", &|e| *e == DebugEvent::Running);
            session.pause();
            let paused = wait_for("pause", &|e| {
                matches!(
                    e,
                    DebugEvent::Stopped {
                        reason: StopReason::Pause,
                        ..
                    }
                )
            });
            print(&paused);
            session.detach();
            wait_for("detach", &|e| *e == DebugEvent::Ended(DebugEnd::Detached));
            std::thread::sleep(Duration::from_secs(2));
            let still_running = processes().unwrap().iter().any(|p| p.pid == editor.pid);
            assert!(still_running, "the editor died after detaching");
        }
    }
}
