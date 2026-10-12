//! The session and automation messages (`SessionServiceMessages.h`, `AutomationWorkerMessages.h`)
//! as CBOR bodies: builders for what a controller sends, parsers for what comes back.

use super::cbor::{Value, obj};
use super::wire::Guid;

pub const SESSION_PING: &str = "/Script/SessionMessages.SessionServicePing";
pub const SESSION_PONG: &str = "/Script/SessionMessages.SessionServicePong";
pub const FIND_WORKERS: &str = "/Script/AutomationMessages.AutomationWorkerFindWorkers";
pub const FIND_WORKERS_RESPONSE: &str =
    "/Script/AutomationMessages.AutomationWorkerFindWorkersResponse";
pub const REQUEST_TESTS: &str = "/Script/AutomationMessages.AutomationWorkerRequestTests";
pub const REQUEST_TESTS_REPLY: &str =
    "/Script/AutomationMessages.AutomationWorkerRequestTestsReplyComplete";
pub const START_TEST_SESSION: &str = "/Script/AutomationMessages.AutomationWorkerStartTestSession";
pub const STOP_TEST_SESSION: &str = "/Script/AutomationMessages.AutomationWorkerStopTestSession";
pub const RUN_TESTS: &str = "/Script/AutomationMessages.AutomationWorkerRunTests";
pub const RUN_TESTS_REPLY: &str = "/Script/AutomationMessages.AutomationWorkerRunTestsReply";
pub const STOP_TESTS: &str = "/Script/AutomationMessages.AutomationWorkerStopTests";
pub const WORKER_PING: &str = "/Script/AutomationMessages.AutomationWorkerPing";
pub const WORKER_OFFLINE: &str = "/Script/AutomationMessages.AutomationWorkerWorkerOffline";

/// `EAutomationTestFlags` Smoke | Engine | Product | Perf: what Session Frontend asks for.
pub const DEFAULT_TEST_FLAGS: u32 = 0x0100_0000 | 0x0200_0000 | 0x0400_0000 | 0x0800_0000;

/// `FindWorkers` only gets an answer when `Changelist` is this.
const WORKER_CHANGELIST: u32 = 10000;

pub fn session_ping(user_name: &str) -> Value {
    obj([("UserName", user_name.into())])
}

pub fn find_workers(session_id: Guid) -> Value {
    obj([
        ("Changelist", WORKER_CHANGELIST.into()),
        ("GameName", "".into()),
        ("ProcessName", "".into()),
        ("SessionId", session_id.into()),
    ])
}

pub fn request_tests(flags: u32) -> Value {
    obj([
        ("DeveloperDirectoryIncluded", false.into()),
        ("RequestedTestFlags", flags.into()),
    ])
}

/// An empty message body, for the messages that carry nothing but their type.
pub fn empty() -> Value {
    Value::Map(Vec::new())
}

pub fn run_test(execution_count: u32, test: &TestInfo) -> Value {
    obj([
        ("ExecutionCount", execution_count.into()),
        ("RoleIndex", 0u32.into()),
        ("TestName", test.test_name.as_str().into()),
        ("BeautifiedTestName", test.display_name.as_str().into()),
        ("FullTestPath", test.full_path.as_str().into()),
        ("bSendAnalytics", false.into()),
        ("bPruneLogsOnSuccess", false.into()),
    ])
}

/// An editor (or game) instance on the bus, from `SessionServicePong`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub session_id: Guid,
    pub instance_id: Guid,
    pub session_name: String,
    pub session_owner: String,
    pub instance_name: String,
    pub platform: String,
    pub device: String,
    pub build_date: String,
    pub standalone: bool,
    pub authorized: bool,
}

impl SessionInfo {
    pub fn from_pong(body: &Value) -> SessionInfo {
        SessionInfo {
            session_id: body.guid("SessionId"),
            instance_id: body.guid("InstanceId"),
            session_name: body.str("SessionName").into(),
            session_owner: body.str("SessionOwner").into(),
            instance_name: body.str("InstanceName").into(),
            platform: body.str("PlatformName").into(),
            device: body.str("DeviceName").into(),
            build_date: body.str("BuildDate").into(),
            standalone: body.bool("Standalone"),
            authorized: body.bool("Authorized"),
        }
    }
}

/// One test, from `RequestTestsReplyComplete`.
#[derive(Debug, Clone, PartialEq)]
pub struct TestInfo {
    pub display_name: String,
    /// Dotted path, e.g. `Subliminal.Inventory.Accounting`.
    pub full_path: String,
    /// What `RunTests` needs: the test's class name plus any parameter.
    pub test_name: String,
    pub source_file: String,
    pub source_line: u32,
    pub flags: u32,
    pub participants: u32,
    pub tags: String,
}

impl TestInfo {
    pub fn list_from_reply(body: &Value) -> Vec<TestInfo> {
        body.array("Tests")
            .iter()
            .map(|t| TestInfo {
                display_name: t.str("DisplayName").into(),
                full_path: t.str("FullTestPath").into(),
                test_name: t.str("TestName").into(),
                source_file: t.str("SourceFile").into(),
                source_line: t.int("SourceFileLine") as u32,
                flags: t.int("TestFlags") as u32,
                participants: t.int("NumParticipantsRequired") as u32,
                tags: t.str("TestTags").into(),
            })
            .collect()
    }
}

/// One event a test logged (`FAutomationExecutionEntry`).
#[derive(Debug, Clone, PartialEq)]
pub struct TestEntry {
    /// `EAutomationEventType`: Info, Warning or Error.
    pub kind: String,
    pub message: String,
    pub file: String,
    pub line: u32,
}

/// The outcome of one test, from `RunTestsReply`.
#[derive(Debug, Clone, PartialEq)]
pub struct TestResult {
    pub test_name: String,
    pub execution_count: u32,
    /// `EAutomationState`: Success, Fail, Skipped, NotRun, InProcess.
    pub state: String,
    pub duration_secs: f64,
    pub errors: u32,
    pub warnings: u32,
    pub entries: Vec<TestEntry>,
}

impl TestResult {
    pub fn from_reply(body: &Value) -> TestResult {
        TestResult {
            test_name: body.str("TestName").into(),
            execution_count: body.int("ExecutionCount") as u32,
            state: body.str("State").into(),
            duration_secs: body.float("Duration"),
            errors: body.int("ErrorTotal") as u32,
            warnings: body.int("WarningTotal") as u32,
            entries: body
                .array("Entries")
                .iter()
                .map(|e| {
                    let event = e.get("Event").unwrap_or(e);
                    TestEntry {
                        kind: event.str("Type").into(),
                        message: event.str("Message").into(),
                        file: e.str("Filename").into(),
                        line: e.int("LineNumber") as u32,
                    }
                })
                .collect(),
        }
    }
}
