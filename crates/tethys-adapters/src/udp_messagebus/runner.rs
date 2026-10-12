//! `TestRunner` over the UDP message bus: finds editors the way Session
//! Frontend does and drives their automation workers.
//!
//! Each session has its own [`Node`] and a thread that owns it, polling the
//! socket and taking commands from the session handle through a channel. The
//! thread goes through: find the editor's node (by session ping, or by the
//! Ping a launched editor sends to our static endpoint), find its automation
//! worker (`FindWorkers`, repeated until the worker answers, which it does
//! once the asset registry has loaded), list the tests, then run them one at
//! a time on request.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tethys_core::automation::{
    AutomationTest, EntryKind, TestEnd, TestEntry, TestEvent, TestInstance, TestResult, TestState,
};
use tethys_core::ports::{PortResult, TestEventSink, TestRunner, TestSession};
use tethys_core::unreal::CommandSpec;

use super::automation::{self as msgs, SessionInfo, TestInfo};
use super::cbor::Value;
use super::message::Message;
use super::node::{Event, Node};
use super::wire::Guid;

/// How long `instances` listens for editors answering the session ping.
const DISCOVERY_TIME: Duration = Duration::from_millis(2500);
const SESSION_PING_INTERVAL: Duration = Duration::from_millis(500);
/// How long a running editor gets to answer before connecting gives up.
const LOCATE_TIMEOUT: Duration = Duration::from_secs(15);
const FIND_WORKERS_INTERVAL: Duration = Duration::from_secs(2);
/// A test list of thousands of tests is megabytes; ask again if it's lost.
const REQUEST_TESTS_RETRY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Default)]
pub struct BusTestRunner;

impl TestRunner for BusTestRunner {
    fn instances(&self) -> PortResult<Vec<TestInstance>> {
        let mut node = Node::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0), true)?;
        let me = Guid::new_random();
        let ping = Message::new(
            msgs::SESSION_PING,
            me,
            vec![],
            msgs::session_ping(&user_name()),
        );
        let mut found: Vec<TestInstance> = Vec::new();
        let deadline = Instant::now() + DISCOVERY_TIME;
        let mut next_ping = Instant::now();
        while Instant::now() < deadline {
            if Instant::now() >= next_ping {
                node.publish(&ping);
                next_ping = Instant::now() + SESSION_PING_INTERVAL;
            }
            for event in node.poll(Duration::from_millis(50)) {
                let Event::Message { message, .. } = event else {
                    continue;
                };
                if message.type_path != msgs::SESSION_PONG {
                    continue;
                }
                let info = SessionInfo::from_pong(&message.body);
                // Editors only: a game can have a worker too, but not this project's tests.
                if info.platform.ends_with("Editor")
                    && !found
                        .iter()
                        .any(|i| i.instance_id == info.instance_id.to_string())
                {
                    found.push(instance(&info));
                }
            }
        }
        Ok(found)
    }

    fn connect(
        &self,
        instance: &TestInstance,
        events: TestEventSink,
    ) -> PortResult<Box<dyn TestSession>> {
        let node = Node::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0), true)?;
        let target = Target::Running {
            instance_id: instance.instance_id.clone(),
            name: instance.name.clone(),
        };
        Ok(Box::new(BusTestSession::start(node, target, events)))
    }

    fn launch(
        &self,
        command: &CommandSpec,
        cwd: &Path,
        events: TestEventSink,
    ) -> PortResult<Box<dyn TestSession>> {
        if !command.program.is_file() {
            return Err(format!("{} doesn't exist", command.program.display()).into());
        }
        // Loopback only: the editor finds us through its static endpoint.
        let node = Node::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), false)?;
        let port = node.local_addr()?.port();
        let session_id = Guid::new_random();
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args)
            .arg(format!("-SessionId={session_id}"))
            .arg("-SessionName=TethysTests")
            .arg(format!("-UDPMESSAGING_TRANSPORT_STATIC=127.0.0.1:{port}"))
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn()?;
        let editor = LaunchedEditor::new(child);
        let target = Target::Launched { editor, session_id };
        Ok(Box::new(BusTestSession::start(node, target, events)))
    }
}

fn user_name() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

fn instance(info: &SessionInfo) -> TestInstance {
    TestInstance {
        instance_id: info.instance_id.to_string(),
        session_id: info.session_id.to_string(),
        name: info.instance_name.clone(),
        owner: info.session_owner.clone(),
    }
}

/// A test editor Tethys started. It's in a kill-on-close job, so it goes
/// down with Tethys even if Tethys dies; dropping this stops it.
struct LaunchedEditor {
    child: Child,
    #[cfg(windows)]
    _job: Option<crate::agent_terminal::job::Job>,
}

impl LaunchedEditor {
    fn new(child: Child) -> Self {
        #[cfg(windows)]
        let job = {
            use std::os::windows::io::AsRawHandle;
            crate::agent_terminal::job::Job::new()
                .and_then(|job| job.assign(child.as_raw_handle()).map(|()| job))
                .ok()
        };
        LaunchedEditor {
            child,
            #[cfg(windows)]
            _job: job,
        }
    }

    /// The exit code, once it has exited.
    fn exited(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

impl Drop for LaunchedEditor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Target {
    /// An editor the user started, known from `instances`.
    Running { instance_id: String, name: String },
    /// A test editor we started with this session id.
    Launched {
        editor: LaunchedEditor,
        session_id: Guid,
    },
}

enum Request {
    Refresh,
    Run(Vec<AutomationTest>),
    Stop,
    Shutdown,
}

struct BusTestSession {
    commands: Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl BusTestSession {
    fn start(node: Node, target: Target, events: TestEventSink) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tethys-tests".into())
            .spawn(move || Worker::new(node, target, events, rx).run())
            .expect("spawn test session thread");
        BusTestSession {
            commands: tx,
            thread: Some(thread),
        }
    }
}

impl TestSession for BusTestSession {
    fn refresh(&self) {
        let _ = self.commands.send(Request::Refresh);
    }
    fn run(&self, tests: Vec<AutomationTest>) {
        let _ = self.commands.send(Request::Run(tests));
    }
    fn stop(&self) {
        let _ = self.commands.send(Request::Stop);
    }
}

impl Drop for BusTestSession {
    fn drop(&mut self) {
        let _ = self.commands.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where the session is in connecting.
enum Phase {
    /// Looking for the editor's node.
    Locating {
        started: Instant,
        next_ping: Instant,
    },
    /// Asking the editor for its automation worker.
    FindingWorker {
        editor: Guid,
        session: Guid,
        next: Instant,
    },
    /// Waiting for the test list.
    Listing {
        editor: Guid,
        worker: Guid,
        asked: Instant,
    },
    Ready {
        editor: Guid,
        worker: Guid,
    },
}

struct Run {
    queue: VecDeque<AutomationTest>,
    /// The test running and the execution count it was sent with.
    current: Option<(u32, AutomationTest)>,
}

struct Worker {
    node: Node,
    /// Our endpoint address on the bus.
    me: Guid,
    target: Target,
    events: TestEventSink,
    commands: Receiver<Request>,
    phase: Phase,
    run: Option<Run>,
    execution_count: u32,
    /// Shown in `Connected`.
    name: String,
}

/// Why the session thread stopped.
enum Exit {
    Shutdown,
    Failed(String),
}

impl Worker {
    fn new(node: Node, target: Target, events: TestEventSink, commands: Receiver<Request>) -> Self {
        let name = match &target {
            Target::Running { name, .. } => name.clone(),
            Target::Launched { editor, .. } => format!("test editor (pid {})", editor.child.id()),
        };
        let now = Instant::now();
        Worker {
            node,
            me: Guid::new_random(),
            target,
            events,
            commands,
            phase: Phase::Locating {
                started: now,
                next_ping: now,
            },
            run: None,
            execution_count: 0,
            name,
        }
    }

    fn emit(&self, event: TestEvent) {
        (self.events)(event);
    }

    fn run(mut self) {
        self.emit(TestEvent::Status(match self.target {
            Target::Running { .. } => format!("Connecting to {}…", self.name),
            Target::Launched { .. } => "Starting the test editor…".into(),
        }));
        let end = match self.serve() {
            Exit::Shutdown => TestEnd::Disconnected,
            Exit::Failed(e) => TestEnd::Failed(e),
        };
        if self.run.take().is_some() {
            self.emit(TestEvent::RunEnded { stopped: true });
        }
        self.emit(TestEvent::Ended(end));
    }

    fn serve(&mut self) -> Exit {
        loop {
            loop {
                match self.commands.try_recv() {
                    Ok(Request::Shutdown) | Err(TryRecvError::Disconnected) => {
                        self.stop_run();
                        return Exit::Shutdown;
                    }
                    Ok(command) => self.command(command),
                    Err(TryRecvError::Empty) => break,
                }
            }
            if let Target::Launched { editor, .. } = &mut self.target
                && let Some(code) = editor.exited()
            {
                return Exit::Failed(format!("The test editor exited (code {code})."));
            }
            if let Some(exit) = self.tick() {
                return exit;
            }
            for event in self.node.poll(POLL) {
                if let Some(exit) = self.handle(event) {
                    return exit;
                }
            }
        }
    }

    fn send(&mut self, node: Guid, type_path: &str, to: Vec<Guid>, body: Value) {
        let message = Message::new(type_path, self.me, to, body);
        self.node.send(node, &message);
    }

    /// Timed work for the current phase.
    fn tick(&mut self) -> Option<Exit> {
        let now = Instant::now();
        match self.phase {
            Phase::Locating { started, next_ping } => {
                let Target::Running { .. } = self.target else {
                    // A launched editor finds us; nothing to send.
                    return None;
                };
                if now - started > LOCATE_TIMEOUT {
                    return Some(Exit::Failed(format!(
                        "{} didn't answer. Is UDP messaging blocked?",
                        self.name
                    )));
                }
                if now >= next_ping {
                    let ping = Message::new(
                        msgs::SESSION_PING,
                        self.me,
                        vec![],
                        msgs::session_ping(&user_name()),
                    );
                    self.node.publish(&ping);
                    self.phase = Phase::Locating {
                        started,
                        next_ping: now + SESSION_PING_INTERVAL,
                    };
                }
            }
            Phase::FindingWorker {
                editor,
                session,
                next,
            } if now >= next => {
                self.send(
                    editor,
                    msgs::FIND_WORKERS,
                    vec![],
                    msgs::find_workers(session),
                );
                self.phase = Phase::FindingWorker {
                    editor,
                    session,
                    next: now + FIND_WORKERS_INTERVAL,
                };
            }
            Phase::Listing {
                editor,
                worker,
                asked,
            } if now - asked > REQUEST_TESTS_RETRY => self.request_tests(editor, worker),
            _ => {}
        }
        None
    }

    fn found_editor(&mut self, editor: Guid, session: Guid) {
        self.emit(TestEvent::Status(
            "Waiting for the editor's automation worker (it answers once assets are scanned)…"
                .into(),
        ));
        self.phase = Phase::FindingWorker {
            editor,
            session,
            next: Instant::now(),
        };
    }

    fn request_tests(&mut self, editor: Guid, worker: Guid) {
        self.emit(TestEvent::Status("Listing tests…".into()));
        self.send(
            editor,
            msgs::REQUEST_TESTS,
            vec![worker],
            msgs::request_tests(msgs::DEFAULT_TEST_FLAGS),
        );
        self.phase = Phase::Listing {
            editor,
            worker,
            asked: Instant::now(),
        };
    }

    fn handle(&mut self, event: Event) -> Option<Exit> {
        let editor = match self.phase {
            Phase::Locating { .. } => None,
            Phase::FindingWorker { editor, .. }
            | Phase::Listing { editor, .. }
            | Phase::Ready { editor, .. } => Some(editor),
        };
        match event {
            Event::NodeFound { node, .. } => {
                // A launched editor's first segment is its Ping to our static endpoint.
                if let (Phase::Locating { .. }, Target::Launched { session_id, .. }) =
                    (&self.phase, &self.target)
                {
                    let session = *session_id;
                    self.found_editor(node, session);
                }
            }
            Event::NodeLost(node) if Some(node) == editor => {
                return Some(Exit::Failed(format!("Lost contact with {}.", self.name)));
            }
            Event::SendFailed { node, .. } if Some(node) == editor => {
                return Some(Exit::Failed(format!("{} stopped answering.", self.name)));
            }
            Event::Message { node, message } => return self.message(node, message),
            _ => {}
        }
        None
    }

    fn message(&mut self, node: Guid, message: Message) -> Option<Exit> {
        match (&self.phase, message.type_path.as_str()) {
            (Phase::Locating { .. }, msgs::SESSION_PONG) => {
                let info = SessionInfo::from_pong(&message.body);
                if let Target::Running { instance_id, .. } = &self.target
                    && info.instance_id.to_string() == *instance_id
                {
                    self.found_editor(node, info.session_id);
                }
            }
            (
                &Phase::FindingWorker {
                    editor, session, ..
                },
                msgs::FIND_WORKERS_RESPONSE,
            ) if node == editor && message.body.guid("SessionId") == session => {
                self.emit(TestEvent::Connected {
                    name: self.name.clone(),
                });
                self.request_tests(editor, message.sender);
            }
            (&Phase::Listing { editor, worker, .. }, msgs::REQUEST_TESTS_REPLY)
                if message.sender == worker =>
            {
                let tests = TestInfo::list_from_reply(&message.body)
                    .iter()
                    .map(test)
                    .collect();
                self.emit(TestEvent::Tests(tests));
                self.emit(TestEvent::Status(String::new()));
                self.phase = Phase::Ready { editor, worker };
            }
            (&Phase::Ready { worker, .. }, msgs::RUN_TESTS_REPLY) if message.sender == worker => {
                let reply = msgs::TestResult::from_reply(&message.body);
                let finished = self
                    .run
                    .as_mut()
                    .and_then(|run| run.current.take_if(|(n, _)| *n == reply.execution_count));
                if let Some((_, test)) = finished {
                    self.emit(TestEvent::Finished(test.path, result(&reply)));
                    self.next_test();
                }
            }
            (
                Phase::FindingWorker { .. } | Phase::Listing { .. } | Phase::Ready { .. },
                msgs::WORKER_OFFLINE,
            ) => {
                return Some(Exit::Failed(format!(
                    "{}'s automation worker went offline.",
                    self.name
                )));
            }
            _ => {}
        }
        None
    }

    fn command(&mut self, command: Request) {
        let Phase::Ready { editor, worker } = self.phase else {
            return;
        };
        match command {
            Request::Refresh if self.run.is_none() => self.request_tests(editor, worker),
            Request::Run(tests) if self.run.is_none() && !tests.is_empty() => {
                self.send(
                    editor,
                    msgs::START_TEST_SESSION,
                    vec![worker],
                    msgs::empty(),
                );
                self.run = Some(Run {
                    queue: tests.into(),
                    current: None,
                });
                self.next_test();
            }
            Request::Stop => self.stop_run(),
            _ => {}
        }
    }

    /// Sends the next queued test, or ends the run.
    fn next_test(&mut self) {
        let Phase::Ready { editor, worker } = self.phase else {
            return;
        };
        let Some(run) = self.run.as_mut() else {
            return;
        };
        match run.queue.pop_front() {
            Some(test) => {
                self.execution_count = self.execution_count.wrapping_add(1).max(1);
                let n = self.execution_count;
                run.current = Some((n, test.clone()));
                let body = msgs::run_test(
                    n,
                    &TestInfo {
                        display_name: test.name().to_string(),
                        full_path: test.path.clone(),
                        test_name: test.command.clone(),
                        source_file: String::new(),
                        source_line: 0,
                        flags: 0,
                        participants: 1,
                        tags: String::new(),
                    },
                );
                self.emit(TestEvent::Started(test.path));
                self.send(editor, msgs::RUN_TESTS, vec![worker], body);
            }
            None => {
                self.run = None;
                self.send(editor, msgs::STOP_TEST_SESSION, vec![worker], msgs::empty());
                self.emit(TestEvent::RunEnded { stopped: false });
            }
        }
    }

    /// Abandons the run. The worker finishes the test it's on (UE can't
    /// interrupt one); its result is ignored.
    fn stop_run(&mut self) {
        if self.run.take().is_none() {
            return;
        }
        if let Phase::Ready { editor, worker } = self.phase {
            self.send(editor, msgs::STOP_TESTS, vec![worker], msgs::empty());
            self.send(editor, msgs::STOP_TEST_SESSION, vec![worker], msgs::empty());
        }
        self.emit(TestEvent::RunEnded { stopped: true });
    }
}

fn source(file: &str, line: u32) -> Option<(PathBuf, u32)> {
    (!file.is_empty()).then(|| (PathBuf::from(file), line.max(1)))
}

fn test(info: &TestInfo) -> AutomationTest {
    AutomationTest {
        path: info.full_path.clone(),
        command: info.test_name.clone(),
        source: source(&info.source_file, info.source_line),
    }
}

fn result(reply: &msgs::TestResult) -> TestResult {
    TestResult {
        state: match reply.state.as_str() {
            "Success" => TestState::Passed,
            "Skipped" | "NotRun" => TestState::Skipped,
            _ => TestState::Failed,
        },
        duration_secs: reply.duration_secs,
        entries: reply
            .entries
            .iter()
            .map(|e| TestEntry {
                kind: match e.kind.as_str() {
                    "Error" => EntryKind::Error,
                    "Warning" => EntryKind::Warning,
                    _ => EntryKind::Info,
                },
                message: e.message.clone(),
                source: source(&e.file, e.line),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::udp_messagebus::cbor::obj;

    #[test]
    fn results_map_states_and_entry_sources() {
        let body = obj([
            ("TestName", "FTest".into()),
            ("ExecutionCount", 3u32.into()),
            ("State", "Fail".into()),
            ("Duration", Value::Float(0.25)),
            (
                "Entries",
                Value::Array(vec![
                    obj([
                        (
                            "Event",
                            obj([("Type", "Error".into()), ("Message", "Expected x".into())]),
                        ),
                        ("Filename", r"D:\Game\Test.cpp".into()),
                        ("LineNumber", 42u32.into()),
                    ]),
                    obj([
                        (
                            "Event",
                            obj([("Type", "Info".into()), ("Message", "log".into())]),
                        ),
                        ("Filename", "".into()),
                        ("LineNumber", 0u32.into()),
                    ]),
                ]),
            ),
        ]);
        let r = result(&msgs::TestResult::from_reply(&body));
        assert_eq!(r.state, TestState::Failed);
        assert_eq!(r.duration_secs, 0.25);
        assert_eq!(r.entries[0].kind, EntryKind::Error);
        assert_eq!(
            r.entries[0].source,
            Some((PathBuf::from(r"D:\Game\Test.cpp"), 42))
        );
        assert_eq!(r.entries[1].kind, EntryKind::Info);
        assert_eq!(r.entries[1].source, None);
    }

    #[test]
    fn instance_pid_comes_from_the_instance_name() {
        let info = SessionInfo {
            session_id: Guid([1, 2, 3, 4]),
            instance_id: Guid([5, 6, 7, 8]),
            session_name: String::new(),
            session_owner: "me".into(),
            instance_name: "EUROPA-20740".into(),
            platform: "WindowsEditor".into(),
            device: "EUROPA".into(),
            build_date: String::new(),
            standalone: true,
            authorized: true,
        };
        let i = instance(&info);
        assert_eq!(i.pid(), Some(20740));
        assert_eq!(i.session_id, Guid([1, 2, 3, 4]).to_string());
    }
}
