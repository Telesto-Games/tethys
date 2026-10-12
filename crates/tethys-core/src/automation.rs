//! Unreal automation tests: what the `TestRunner` port talks about, which
//! running editor is this project's, and the bookkeeping for a test pass
//! (results, and the tree the Tests panel shows).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::debug::{self, FindEditorError, ProcessInfo};

/// An editor that can run tests, as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInstance {
    /// Identifies this editor process (UE's instance id).
    pub instance_id: String,
    /// The session it belongs to; its automation worker answers only to it.
    pub session_id: String,
    /// UE's instance name, `<Computer>-<pid>`.
    pub name: String,
    pub owner: String,
}

impl TestInstance {
    /// The editor's process id, from its instance name.
    pub fn pid(&self) -> Option<u32> {
        self.name.rsplit_once('-')?.1.parse().ok()
    }
}

/// One automation test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationTest {
    /// Dotted path, e.g. `Project.Inventory.Accounting`; unique.
    pub path: String,
    /// What the editor needs to run it (its class name plus any parameter).
    pub command: String,
    /// Where it's defined, if known.
    pub source: Option<(PathBuf, u32)>,
}

impl AutomationTest {
    /// The last segment of the path.
    pub fn name(&self) -> &str {
        self.path.rsplit('.').next().unwrap_or(&self.path)
    }

    /// Whether it's defined under `root` (the project's own tests, not the engine's).
    pub fn is_under(&self, root: &Path) -> bool {
        let Some((file, _)) = &self.source else {
            return false;
        };
        let normalize = |p: &Path| p.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
        let root = normalize(root);
        let root = root.trim_end_matches('\\');
        let file = normalize(file);
        file.len() > root.len() && file.starts_with(root) && file[root.len()..].starts_with('\\')
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestState {
    #[default]
    NotRun,
    Queued,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Info,
    Warning,
    Error,
}

/// Something a test logged: an assertion failure, a warning, a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    pub kind: EntryKind,
    pub message: String,
    pub source: Option<(PathBuf, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestResult {
    /// Passed, Failed or Skipped.
    pub state: TestState,
    pub duration_secs: f64,
    pub entries: Vec<TestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestEnd {
    /// Disconnected on request.
    Disconnected,
    Failed(String),
}

/// Pushed by a test session from its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum TestEvent {
    /// Progress while connecting, for the status line.
    Status(String),
    /// Connected to the editor's automation worker.
    Connected {
        name: String,
    },
    /// The editor's tests, after connecting and after each refresh.
    Tests(Vec<AutomationTest>),
    Started(String),
    Finished(String, TestResult),
    /// A run is over: every test ran, or it was stopped.
    RunEnded {
        stopped: bool,
    },
    /// The session is over; no more events follow.
    Ended(TestEnd),
}

/// This project's editor among `instances`: the one whose process has the
/// project open (see [`debug::find_editor`]). `processes` may be unavailable
/// (the debugger lists them, and refuses while it debugs); then the only
/// editor running is taken.
pub fn pick_instance<'a>(
    instances: &'a [TestInstance],
    processes: Option<&[ProcessInfo]>,
    uproject: &Path,
) -> Result<&'a TestInstance, FindEditorError> {
    let Some(processes) = processes else {
        return match instances {
            [] => Err(FindEditorError::NoneRunning),
            [only] => Ok(only),
            many => Err(FindEditorError::Ambiguous(
                many.len(),
                uproject
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )),
        };
    };
    let reachable: Vec<ProcessInfo> = processes
        .iter()
        .filter(|p| instances.iter().any(|i| i.pid() == Some(p.pid)))
        .cloned()
        .collect();
    let editor = debug::find_editor(&reachable, uproject)?;
    instances
        .iter()
        .find(|i| i.pid() == Some(editor.pid))
        .ok_or(FindEditorError::NoneRunning)
}

/// A row of the test tree: a group (a path prefix) or a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRow {
    /// The group's prefix or the test's path.
    pub key: String,
    pub label: String,
    pub depth: usize,
    pub is_group: bool,
    pub expanded: bool,
    /// A test's state, or a summary of a group's.
    pub state: TestState,
    /// Tests in a group (1 for a test).
    pub count: usize,
}

/// Totals for a set of tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TestCounts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Queued or running.
    pub pending: usize,
}

impl TestCounts {
    fn add(&mut self, state: TestState) {
        self.total += 1;
        match state {
            TestState::Passed => self.passed += 1,
            TestState::Failed => self.failed += 1,
            TestState::Skipped => self.skipped += 1,
            TestState::Queued | TestState::Running => self.pending += 1,
            TestState::NotRun => {}
        }
    }

    /// One state standing for all of them, for a group's icon.
    fn summary(&self, running: bool) -> TestState {
        if running {
            TestState::Running
        } else if self.failed > 0 {
            TestState::Failed
        } else if self.pending > 0 {
            TestState::Queued
        } else if self.passed > 0 && self.passed + self.skipped == self.total {
            TestState::Passed
        } else {
            TestState::NotRun
        }
    }
}

/// The tests an editor has, and what happened when they ran.
#[derive(Debug, Default)]
pub struct TestCatalog {
    tests: Vec<AutomationTest>,
    index: HashMap<String, usize>,
    states: Vec<TestState>,
    results: Vec<Option<TestResult>>,
}

impl TestCatalog {
    /// Replaces the tests, keeping results for tests that are still there.
    pub fn set_tests(&mut self, mut tests: Vec<AutomationTest>) {
        tests.sort_by(|a, b| a.path.cmp(&b.path));
        tests.dedup_by(|a, b| a.path == b.path);
        let old = std::mem::take(self);
        self.index = tests
            .iter()
            .enumerate()
            .map(|(i, t)| (t.path.clone(), i))
            .collect();
        self.states = vec![TestState::NotRun; tests.len()];
        self.results = vec![None; tests.len()];
        for (path, &i) in &self.index {
            if let Some(&j) = old.index.get(path) {
                self.results[i] = old.results[j].clone();
                self.states[i] = match old.states[j] {
                    TestState::Queued | TestState::Running => TestState::NotRun,
                    s => s,
                };
            }
        }
        self.tests = tests;
    }

    pub fn is_empty(&self) -> bool {
        self.tests.is_empty()
    }

    /// Every test, in path order.
    pub fn all(&self) -> &[AutomationTest] {
        &self.tests
    }

    pub fn get(&self, path: &str) -> Option<(&AutomationTest, TestState, Option<&TestResult>)> {
        let &i = self.index.get(path)?;
        Some((&self.tests[i], self.states[i], self.results[i].as_ref()))
    }

    /// Tests at or below `key` (a test's path or a group's prefix), in order.
    pub fn under(&self, key: &str) -> Vec<AutomationTest> {
        self.tests
            .iter()
            .filter(|t| in_group(&t.path, key))
            .cloned()
            .collect()
    }

    /// Every test that failed in its last run.
    pub fn failed(&self) -> Vec<AutomationTest> {
        self.tests
            .iter()
            .zip(&self.states)
            .filter(|(_, s)| **s == TestState::Failed)
            .map(|(t, _)| t.clone())
            .collect()
    }

    /// Marks `tests` as waiting to run.
    pub fn queue(&mut self, tests: &[AutomationTest]) {
        for t in tests {
            if let Some(&i) = self.index.get(&t.path) {
                self.states[i] = TestState::Queued;
            }
        }
    }

    pub fn started(&mut self, path: &str) {
        if let Some(&i) = self.index.get(path) {
            self.states[i] = TestState::Running;
        }
    }

    pub fn finished(&mut self, path: &str, result: TestResult) {
        if let Some(&i) = self.index.get(path) {
            self.states[i] = result.state;
            self.results[i] = Some(result);
        }
    }

    /// A run ended: tests still waiting go back to their last result.
    pub fn run_ended(&mut self) {
        for (state, result) in self.states.iter_mut().zip(&self.results) {
            if matches!(state, TestState::Queued | TestState::Running) {
                *state = result.as_ref().map_or(TestState::NotRun, |r| r.state);
            }
        }
    }

    /// Totals over the tests `keep` lets through.
    pub fn counts(&self, keep: impl Fn(&AutomationTest) -> bool) -> TestCounts {
        let mut counts = TestCounts::default();
        for (t, s) in self.tests.iter().zip(&self.states) {
            if keep(t) {
                counts.add(*s);
            }
        }
        counts
    }

    /// The visible rows of the tree of tests that `keep` lets through: groups
    /// are shown collapsed unless their prefix is in `expanded`, or `expand_all`.
    pub fn rows(
        &self,
        keep: impl Fn(&AutomationTest) -> bool,
        expanded: &HashSet<String>,
        expand_all: bool,
    ) -> Vec<TestRow> {
        let shown: Vec<usize> = (0..self.tests.len())
            .filter(|&i| keep(&self.tests[i]))
            .collect();

        // Totals per group, so a group row can summarise what's inside.
        let mut groups: HashMap<&str, (TestCounts, bool)> = HashMap::new();
        for &i in &shown {
            let path = self.tests[i].path.as_str();
            for (end, _) in path.match_indices('.') {
                let entry = groups.entry(&path[..end]).or_default();
                entry.0.add(self.states[i]);
                entry.1 |= self.states[i] == TestState::Running;
            }
        }
        let open = |prefix: &str| expand_all || expanded.contains(prefix);

        let mut rows = Vec::new();
        let mut previous: Vec<&str> = Vec::new();
        for &i in &shown {
            let path = self.tests[i].path.as_str();
            let segments: Vec<&str> = path.split('.').collect();
            let groups_here = &segments[..segments.len() - 1];
            let common = previous
                .iter()
                .zip(groups_here)
                .take_while(|(a, b)| a == b)
                .count();
            for depth in common..groups_here.len() {
                let prefix = &path[..prefix_len(&segments, depth)];
                if !(0..depth).all(|d| open(&path[..prefix_len(&segments, d)])) {
                    break;
                }
                let (counts, running) = groups.get(prefix).copied().unwrap_or_default();
                rows.push(TestRow {
                    key: prefix.to_string(),
                    label: segments[depth].to_string(),
                    depth,
                    is_group: true,
                    expanded: open(prefix),
                    state: counts.summary(running),
                    count: counts.total,
                });
            }
            previous = groups_here.to_vec();
            if (0..groups_here.len()).all(|d| open(&path[..prefix_len(&segments, d)])) {
                rows.push(TestRow {
                    key: path.to_string(),
                    label: segments[segments.len() - 1].to_string(),
                    depth: groups_here.len(),
                    is_group: false,
                    expanded: false,
                    state: self.states[i],
                    count: 1,
                });
            }
        }
        rows
    }
}

/// The length of the prefix made of `segments[..=depth]` joined by dots.
fn prefix_len(segments: &[&str], depth: usize) -> usize {
    segments[..=depth].iter().map(|s| s.len()).sum::<usize>() + depth
}

/// Whether `path` is `key` or inside the group `key`.
fn in_group(path: &str, key: &str) -> bool {
    path == key || (path.starts_with(key) && path[key.len()..].starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(path: &str) -> AutomationTest {
        AutomationTest {
            path: path.into(),
            command: format!("F{}", path.replace('.', "")),
            source: None,
        }
    }

    fn catalog(paths: &[&str]) -> TestCatalog {
        let mut c = TestCatalog::default();
        c.set_tests(paths.iter().map(|p| test(p)).collect());
        c
    }

    fn result(state: TestState) -> TestResult {
        TestResult {
            state,
            duration_secs: 0.1,
            entries: vec![],
        }
    }

    fn labels(rows: &[TestRow]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.label))
            .collect()
    }

    #[test]
    fn rows_show_collapsed_groups_until_expanded() {
        let c = catalog(&["A.X.One", "A.X.Two", "A.Y", "B"]);
        let none = HashSet::new();
        assert_eq!(labels(&c.rows(|_| true, &none, false)), ["A", "B"]);
        let open: HashSet<String> = ["A".to_string(), "A.X".to_string()].into();
        assert_eq!(
            labels(&c.rows(|_| true, &open, false)),
            ["A", "  X", "    One", "    Two", "  Y", "B"]
        );
        let a = &c.rows(|_| true, &none, false)[0];
        assert!(a.is_group && a.count == 3 && !a.expanded);
    }

    #[test]
    fn rows_expand_everything_when_asked_and_respect_the_filter() {
        let c = catalog(&["A.X.One", "A.X.Two", "A.Y", "B"]);
        let rows = c.rows(|t| t.path.contains("Two"), &HashSet::new(), true);
        assert_eq!(labels(&rows), ["A", "  X", "    Two"]);
    }

    #[test]
    fn group_state_summarises_its_tests() {
        let mut c = catalog(&["A.One", "A.Two"]);
        let state = |c: &TestCatalog| c.rows(|_| true, &HashSet::new(), false)[0].state;
        assert_eq!(state(&c), TestState::NotRun);
        c.queue(&c.under("A"));
        assert_eq!(state(&c), TestState::Queued);
        c.started("A.One");
        assert_eq!(state(&c), TestState::Running);
        c.finished("A.One", result(TestState::Passed));
        c.finished("A.Two", result(TestState::Passed));
        assert_eq!(state(&c), TestState::Passed);
        c.finished("A.Two", result(TestState::Failed));
        assert_eq!(state(&c), TestState::Failed);
    }

    #[test]
    fn under_matches_whole_segments() {
        let c = catalog(&["A.B", "A.BC", "A.B.C"]);
        let paths: Vec<String> = c.under("A.B").into_iter().map(|t| t.path).collect();
        assert_eq!(paths, ["A.B", "A.B.C"]);
    }

    #[test]
    fn a_stopped_run_restores_previous_results_and_reload_keeps_them() {
        let mut c = catalog(&["A.One", "A.Two"]);
        c.finished("A.One", result(TestState::Failed));
        c.queue(&c.under("A"));
        c.started("A.One");
        c.run_ended();
        assert_eq!(c.get("A.One").unwrap().1, TestState::Failed);
        assert_eq!(c.get("A.Two").unwrap().1, TestState::NotRun);
        assert_eq!(c.failed().len(), 1);

        c.set_tests(vec![test("A.One"), test("A.Three")]);
        assert_eq!(c.get("A.One").unwrap().1, TestState::Failed);
        assert!(c.get("A.Two").is_none());
        let counts = c.counts(|_| true);
        assert_eq!((counts.total, counts.failed), (2, 1));
    }

    #[test]
    fn project_tests_are_those_defined_under_the_project() {
        let mut t = test("P.One");
        t.source = Some((r"D:\Dev\Game\Source\GameTests\One.cpp".into(), 10));
        assert!(t.is_under(Path::new("d:/dev/game")));
        assert!(t.is_under(Path::new(r"D:\Dev\Game\")));
        assert!(!t.is_under(Path::new(r"D:\Dev\Gam")));
        assert!(!test("P.Two").is_under(Path::new(r"D:\Dev\Game")));
    }

    fn instance(name: &str) -> TestInstance {
        TestInstance {
            instance_id: name.into(),
            session_id: "S".into(),
            name: name.into(),
            owner: "me".into(),
        }
    }

    fn process(pid: u32, details: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            exe: "UnrealEditor.exe".into(),
            details: details.into(),
        }
    }

    #[test]
    fn picks_the_editor_with_the_project_open() {
        let instances = [instance("PC-100"), instance("PC-200")];
        let processes = [
            process(100, r"UnrealEditor.exe D:\Other\Other.uproject"),
            process(200, r"UnrealEditor.exe D:\Game\Game.uproject"),
            process(300, r"UnrealEditor.exe D:\Game\Game.uproject"),
        ];
        let uproject = Path::new(r"D:\Game\Game.uproject");
        let picked = pick_instance(&instances, Some(&processes), uproject).unwrap();
        assert_eq!(picked.name, "PC-200");
        // Without a process list only a lone editor is taken.
        assert!(pick_instance(&instances, None, uproject).is_err());
        assert_eq!(
            pick_instance(&instances[..1], None, uproject).unwrap().name,
            "PC-100"
        );
        assert_eq!(
            pick_instance(&[], Some(&processes), uproject),
            Err(FindEditorError::NoneRunning)
        );
    }
}
