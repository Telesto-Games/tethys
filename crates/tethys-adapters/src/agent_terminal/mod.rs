//! `AgentHost` that runs an agent's own TUI in a PTY, parsed by alacritty_terminal.
//!
//! The core only sees the session lifecycle. The app's terminal view gets a
//! [`TerminalHandle`] from [`TerminalHost::take_handle`] to draw the grid and
//! send input.

mod command;
mod job;
pub mod palette;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use tethys_core::ports::{
    AgentHost, AgentSession, EventSink, PortResult, SessionEvent, SessionStatus,
};
use tethys_core::{AdapterKind, AgentProfile, Project, SessionId};

/// alacritty_terminal, re-exported so views don't need their own dependency.
pub use alacritty_terminal as backend;

pub use command::{CommandLine, resolve as resolve_command};

/// The shell for a plain terminal: PowerShell 7 (`pwsh`) when it's on PATH,
/// otherwise Windows PowerShell, which every Windows has.
pub fn default_shell() -> &'static str {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    if command::find_executable("pwsh", &path, &pathext).is_some() {
        "pwsh"
    } else {
        "powershell"
    }
}

/// Variables a parent Claude Code session leaves in Tethys's environment
/// when Tethys is launched from one (e.g. `cargo run` in its Bash tool).
/// Sessions inherit the environment, so these would turn their colours off
/// (`NO_COLOR`) and make `claude` think it's nested. `NO_COLOR` only counts
/// when it came from Claude Code; a user who set it themselves keeps it.
pub fn leaked_agent_vars(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let keys: Vec<String> = keys.into_iter().collect();
    let from_claude = keys.iter().any(|k| k == "CLAUDECODE");
    keys.into_iter()
        .filter(|k| {
            k == "CLAUDECODE"
                || k == "AI_AGENT"
                || k.starts_with("CLAUDE_CODE_")
                || matches!(
                    k.as_str(),
                    "CLAUDE_PID" | "CLAUDE_EFFORT" | "CLAUDE_JOB_DIR"
                )
                || (from_claude && k == "NO_COLOR")
        })
        .collect()
}

/// The terminal model a view draws.
pub type TerminalModel = Term<Listener>;

/// Events for the view (not the core): redraw, or copy to clipboard.
#[derive(Debug, Clone)]
pub enum ViewEvent {
    Wakeup,
    Clipboard(String),
}

type ViewCallback = Box<dyn Fn(ViewEvent) + Send + Sync>;

const INITIAL_SIZE: WindowSize = WindowSize {
    num_lines: 24,
    num_cols: 80,
    cell_width: 8,
    cell_height: 16,
};

/// State shared between the listener (PTY thread), session and handle.
struct Shared {
    id: SessionId,
    sink: EventSink,
    sender: OnceLock<EventLoopSender>,
    view: Mutex<Option<ViewCallback>>,
    size: Mutex<WindowSize>,
    status: Mutex<SessionStatus>,
}

impl Shared {
    fn to_view(&self, event: ViewEvent) {
        if let Some(view) = self.view.lock().unwrap().as_ref() {
            view(event);
        }
    }

    fn write(&self, bytes: Cow<'static, [u8]>) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(bytes));
        }
    }
}

/// Receives events from alacritty's PTY thread.
#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let s = &self.0;
        match event {
            Event::Wakeup => s.to_view(ViewEvent::Wakeup),
            Event::Title(title) => (s.sink)(s.id, SessionEvent::TitleChanged(title)),
            Event::ResetTitle => (s.sink)(s.id, SessionEvent::TitleChanged(String::new())),
            Event::Bell => (s.sink)(s.id, SessionEvent::Bell),
            Event::PtyWrite(text) => s.write(Cow::Owned(text.into_bytes())),
            // Called while the terminal is locked, so answer from defaults.
            Event::ColorRequest(index, format) => s.write(Cow::Owned(
                format(palette::default_color(index)).into_bytes(),
            )),
            Event::TextAreaSizeRequest(format) => {
                let size = *s.size.lock().unwrap();
                s.write(Cow::Owned(format(size).into_bytes()));
            }
            Event::ClipboardStore(_, text) => s.to_view(ViewEvent::Clipboard(text)),
            Event::ChildExit(status) => {
                let code = status.code().unwrap_or(-1);
                *s.status.lock().unwrap() = SessionStatus::Exited(code);
                (s.sink)(s.id, SessionEvent::Exited(code));
                s.to_view(ViewEvent::Wakeup);
            }
            // Reading the clipboard from a program is a security hole; ignore.
            Event::ClipboardLoad(..)
            | Event::MouseCursorDirty
            | Event::CursorBlinkingChange
            | Event::Exit => {}
        }
    }
}

/// What a view needs: the grid, input and resize.
#[derive(Clone)]
pub struct TerminalHandle {
    term: Arc<FairMutex<TerminalModel>>,
    shared: Arc<Shared>,
}

impl TerminalHandle {
    pub fn term(&self) -> &Arc<FairMutex<TerminalModel>> {
        &self.term
    }

    /// Sends bytes to the agent, as if typed.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        // Typing jumps back to the live screen.
        self.term.lock().scroll_display(Scroll::Bottom);
        self.shared.write(bytes.into());
    }

    /// Resizes the grid and the PTY. Cheap to call when nothing changed.
    pub fn resize(&self, cols: u16, lines: u16, cell_width: u16, cell_height: u16) {
        let size = WindowSize {
            num_lines: lines.max(1),
            num_cols: cols.max(2),
            cell_width,
            cell_height,
        };
        {
            let mut current = self.shared.size.lock().unwrap();
            if current.num_cols == size.num_cols && current.num_lines == size.num_lines {
                return;
            }
            *current = size;
        }
        self.term.lock().resize(TermSize::new(
            size.num_cols as usize,
            size.num_lines as usize,
        ));
        if let Some(sender) = self.shared.sender.get() {
            let _ = sender.send(Msg::Resize(size));
        }
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn status(&self) -> SessionStatus {
        *self.shared.status.lock().unwrap()
    }

    /// Sets the view callback, replacing any previous one. Called from the PTY thread.
    pub fn on_view_event(&self, callback: impl Fn(ViewEvent) + Send + Sync + 'static) {
        *self.shared.view.lock().unwrap() = Some(Box::new(callback));
    }
}

/// Runs agents in PTYs.
#[derive(Clone, Default)]
pub struct TerminalHost {
    handles: Arc<Mutex<HashMap<SessionId, TerminalHandle>>>,
    stopping: Arc<Stopping>,
}

impl TerminalHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// The view's handle for a session this host started. Returns it once.
    pub fn take_handle(&self, id: SessionId) -> Option<TerminalHandle> {
        self.handles.lock().unwrap().remove(&id)
    }

    /// Blocks until stopped sessions have finished exiting, or `timeout`.
    /// Call before quitting so agents aren't killed mid-cleanup.
    pub fn wait_for_stopped(&self, timeout: Duration) {
        self.stopping.wait(timeout);
    }
}

impl AgentHost for TerminalHost {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Terminal
    }

    fn start(
        &self,
        id: SessionId,
        profile: &AgentProfile,
        project: &Project,
        events: EventSink,
    ) -> PortResult<Box<dyn AgentSession>> {
        let line = command::resolve(&profile.command, &profile.args)?;
        let options = tty::Options {
            shell: Some(tty::Shell::new(line.program, line.args)),
            working_directory: Some(project.root().to_path_buf()),
            drain_on_exit: true,
            env: HashMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ]),
            #[cfg(windows)]
            escape_args: line.escape_args,
        };

        let shared = Arc::new(Shared {
            id,
            sink: events.clone(),
            sender: OnceLock::new(),
            view: Mutex::new(None),
            size: Mutex::new(INITIAL_SIZE),
            status: Mutex::new(SessionStatus::Starting),
        });
        let listener = Listener(shared.clone());

        let pty = tty::new(&options, INITIAL_SIZE, id.0)?;

        #[cfg(windows)]
        let job = {
            let job = job::Job::new()?;
            job.assign(pty.child_watcher().raw_handle() as _)?;
            job
        };

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(
                INITIAL_SIZE.num_cols as usize,
                INITIAL_SIZE.num_lines as usize,
            ),
            listener.clone(),
        )));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        let _ = shared.sender.set(sender.clone());
        event_loop.spawn();

        *shared.status.lock().unwrap() = SessionStatus::Running;
        events(id, SessionEvent::Started);

        self.handles.lock().unwrap().insert(
            id,
            TerminalHandle {
                term,
                shared: shared.clone(),
            },
        );
        Ok(Box::new(TerminalSession {
            shared,
            sender,
            stopping: self.stopping.clone(),
            #[cfg(windows)]
            job: Some(job),
        }))
    }
}

/// How long an agent gets to exit after its console closes, before the
/// whole process tree is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Counts sessions that are still shutting down, so quitting can wait for them.
#[derive(Default)]
struct Stopping {
    count: Mutex<usize>,
    done: Condvar,
}

impl Stopping {
    fn begin(&self) {
        *self.count.lock().unwrap() += 1;
    }

    fn end(&self) {
        *self.count.lock().unwrap() -= 1;
        self.done.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let count = self.count.lock().unwrap();
        let _ = self.done.wait_timeout_while(count, timeout, |n| *n > 0);
    }
}

pub struct TerminalSession {
    shared: Arc<Shared>,
    sender: EventLoopSender,
    stopping: Arc<Stopping>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl AgentSession for TerminalSession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn status(&self) -> SessionStatus {
        *self.shared.status.lock().unwrap()
    }

    /// Closes the console, which sends the agent CTRL_CLOSE_EVENT so it can
    /// clean up. Anything left in the job after [`STOP_GRACE`] is killed.
    fn stop(&mut self) -> PortResult<()> {
        let _ = self.sender.send(Msg::Shutdown);
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            let stopping = self.stopping.clone();
            stopping.begin();
            std::thread::spawn(move || {
                let deadline = Instant::now() + STOP_GRACE;
                while job.active_processes() > 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                job.terminate();
                drop(job);
                stopping.end();
            });
        }
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn strips_vars_leaked_by_a_parent_claude() {
        let keys = |ks: &[&str]| ks.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(
            leaked_agent_vars(keys(&[
                "PATH",
                "CLAUDECODE",
                "NO_COLOR",
                "CLAUDE_CODE_SESSION_ID"
            ])),
            ["CLAUDECODE", "NO_COLOR", "CLAUDE_CODE_SESSION_ID"]
        );
        // NO_COLOR the user set themselves stays.
        assert!(leaked_agent_vars(keys(&["PATH", "NO_COLOR"])).is_empty());
    }

    /// Runs `cmd /c echo` in a real ConPTY and waits for its output and exit.
    #[test]
    fn runs_a_command_in_a_pty() {
        let dir = std::env::temp_dir();
        let project = Project::parse(dir.join("T.uproject"), "{}").unwrap();
        let profile = AgentProfile {
            id: "cmd".into(),
            name: "cmd".into(),
            adapter: AdapterKind::Terminal,
            command: "cmd".into(),
            args: vec!["/c".into(), "echo tethys-ok".into()],
        };
        let (tx, rx) = mpsc::channel();
        let sink: EventSink = Arc::new(move |_, e| {
            let _ = tx.send(e);
        });
        let host = TerminalHost::new();
        let _session = host.start(SessionId(1), &profile, &project, sink).unwrap();
        let handle = host.take_handle(SessionId(1)).unwrap();

        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(SessionEvent::Started)
        );
        let exited = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("no exit") {
                SessionEvent::Exited(code) => break code,
                _ => continue,
            }
        };
        assert_eq!(exited, 0);

        let term = handle.term().lock();
        let text: String = term.grid().display_iter().map(|cell| cell.c).collect();
        assert!(text.contains("tethys-ok"), "screen was: {text:?}");
    }
}

#[cfg(all(test, windows))]
mod stop_tests {
    use super::*;

    /// An interactive `cmd` exits when its console closes, well within the grace period.
    #[test]
    fn stop_closes_console_and_waits() {
        let project = Project::parse(std::env::temp_dir().join("T.uproject"), "{}").unwrap();
        let profile = AgentProfile {
            id: "cmd".into(),
            name: "cmd".into(),
            adapter: AdapterKind::Terminal,
            command: "cmd".into(),
            args: vec![],
        };
        let host = TerminalHost::new();
        let sink: EventSink = Arc::new(|_, _| {});
        let session = host.start(SessionId(2), &profile, &project, sink).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let started = Instant::now();
        drop(session);
        host.wait_for_stopped(Duration::from_secs(10));
        assert!(
            started.elapsed() < STOP_GRACE,
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(*host.stopping.count.lock().unwrap(), 0);
    }
}
