//! The Tests pane: an editor's automation tests as a tree, run state and
//! results, and the selected test's log. It keeps the test catalog and its own
//! view state (filter, expanded groups, selection); the workspace owns the
//! connection and forwards its events here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::automation::{
    AutomationTest, EntryKind, TestCatalog, TestEnd, TestEvent, TestRow, TestState,
};
use tethys_core::usecases::TestTarget;

const ROW_HEIGHT: f32 = 22.;
const INDENT: f32 = 14.;
/// Log lines shown for one test; a chatty test can log thousands.
const MAX_INFO_LINES: usize = 500;

fn passed_color() -> Hsla {
    rgb(0x98c379).into()
}
fn failed_color() -> Hsla {
    rgb(0xe06c75).into()
}
fn warning_color() -> Hsla {
    rgb(0xe5c07b).into()
}

pub enum TestsPanelEvent {
    Connect(TestTarget),
    Disconnect,
    Run(Vec<AutomationTest>),
    Stop,
    Refresh,
    OpenSource(PathBuf, u32),
}

enum Connection {
    /// With a note about how the last connection ended, if it failed.
    Disconnected(Option<String>),
    Connecting(String),
    Connected(String),
}

pub struct TestsPanel {
    focus: FocusHandle,
    project_root: PathBuf,
    catalog: TestCatalog,
    connection: Connection,
    /// Tests in the current run, and how many have finished.
    run: Option<(usize, usize)>,
    filter: Entity<InputState>,
    filter_text: String,
    project_only: bool,
    expanded: HashSet<String>,
    selected: Option<String>,
    rows: Vec<TestRow>,
    scroll: UniformListScrollHandle,
    _filter_events: Subscription,
}

impl TestsPanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tests"));
        let filter_events = cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.filter_text = input.read(cx).value().trim().to_lowercase();
                this.rebuild();
                cx.notify();
            }
        });
        Self {
            focus: cx.focus_handle(),
            project_root,
            catalog: TestCatalog::default(),
            connection: Connection::Disconnected(None),
            run: None,
            filter,
            filter_text: String::new(),
            project_only: true,
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            _filter_events: filter_events,
        }
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self.connection, Connection::Disconnected(_))
    }

    /// Connecting has started; `status` says how.
    pub fn set_connecting(&mut self, status: String, cx: &mut Context<Self>) {
        self.connection = Connection::Connecting(status);
        cx.notify();
    }

    /// Connecting failed before a session existed.
    pub fn set_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.connection = Connection::Disconnected(Some(error));
        self.run = None;
        cx.notify();
    }

    pub fn apply(&mut self, event: TestEvent, cx: &mut Context<Self>) {
        match event {
            TestEvent::Status(status) => {
                if let Connection::Connecting(s) = &mut self.connection
                    && !status.is_empty()
                {
                    *s = status;
                }
            }
            TestEvent::Connected { name } => self.connection = Connection::Connected(name),
            TestEvent::Tests(tests) => {
                self.catalog.set_tests(tests);
                // Open the project's own groups, which is usually all there is to see.
                if self.expanded.is_empty() {
                    let root = self.project_root.clone();
                    let mut groups = HashSet::new();
                    for row in self
                        .catalog
                        .rows(|t| t.is_under(&root), &HashSet::new(), true)
                    {
                        if row.is_group {
                            groups.insert(row.key);
                        }
                    }
                    self.expanded = groups;
                }
            }
            TestEvent::Started(path) => self.catalog.started(&path),
            TestEvent::Finished(path, result) => {
                self.catalog.finished(&path, result);
                if let Some((_, done)) = &mut self.run {
                    *done += 1;
                }
            }
            TestEvent::RunEnded { .. } => {
                self.catalog.run_ended();
                self.run = None;
            }
            TestEvent::Ended(end) => {
                self.catalog.run_ended();
                self.run = None;
                self.connection = Connection::Disconnected(match end {
                    TestEnd::Disconnected => None,
                    TestEnd::Failed(e) => Some(e),
                });
            }
        }
        self.rebuild();
        cx.notify();
    }

    /// Marks `tests` as queued; the workspace sends them to the editor.
    fn start_run(&mut self, tests: Vec<AutomationTest>, cx: &mut Context<Self>) {
        if tests.is_empty() || self.run.is_some() {
            return;
        }
        self.catalog.queue(&tests);
        self.run = Some((tests.len(), 0));
        self.rebuild();
        cx.emit(TestsPanelEvent::Run(tests));
        cx.notify();
    }

    fn keep(&self) -> impl Fn(&AutomationTest) -> bool + '_ {
        move |t: &AutomationTest| {
            (!self.project_only || t.is_under(&self.project_root))
                && (self.filter_text.is_empty()
                    || t.path.to_lowercase().contains(&self.filter_text))
        }
    }

    fn rebuild(&mut self) {
        let rows = self
            .catalog
            .rows(self.keep(), &self.expanded, !self.filter_text.is_empty());
        self.rows = rows;
    }

    /// What Run runs: the selected test or group, else everything shown.
    fn run_targets(&self) -> Vec<AutomationTest> {
        let keep = self.keep();
        match &self.selected {
            Some(key) => self
                .catalog
                .under(key)
                .into_iter()
                .filter(|t| keep(t))
                .collect(),
            None => self
                .catalog
                .all()
                .iter()
                .filter(|t| keep(t))
                .cloned()
                .collect(),
        }
    }

    fn click_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        if row.is_group {
            if !self.expanded.remove(&row.key) {
                self.expanded.insert(row.key.clone());
            }
            self.rebuild();
        }
        self.selected = Some(row.key);
        cx.notify();
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, hover, selected_bg) = (
            theme.muted_foreground,
            theme.secondary_hover,
            theme.primary.opacity(0.18),
        );
        let primary = theme.primary;
        range
            .filter_map(|ix| self.rows.get(ix).cloned().map(|row| (ix, row)))
            .map(|(ix, row)| {
                let selected = self.selected.as_deref() == Some(row.key.as_str());
                let (glyph, color) = state_glyph(row.state, muted, primary);
                let trailing = if row.is_group {
                    Some(row.count.to_string())
                } else {
                    self.catalog
                        .get(&row.key)
                        .and_then(|(_, _, r)| r)
                        .map(|r| format_duration(r.duration_secs))
                };
                div()
                    .id(ix)
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(px(8. + row.depth as f32 * INDENT))
                    .pr_2()
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(selected_bg))
                    .hover(|s| s.bg(hover))
                    .child(div().w(px(12.)).text_xs().text_color(muted).child(
                        match (row.is_group, row.expanded) {
                            (true, true) => "▾",
                            (true, false) => "▸",
                            _ => "",
                        },
                    ))
                    .child(div().w(px(14.)).flex_none().text_color(color).child(glyph))
                    .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
                    .children(
                        trailing.map(|t| div().flex_none().text_xs().text_color(muted).child(t)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.click_row(ix, cx)))
                    .into_any_element()
            })
            .collect()
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.secondary_hover;
        let body = div()
            .id("test-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .flex()
            .flex_col()
            .gap_1()
            .text_xs();
        let Some(key) = &self.selected else {
            return body.text_color(muted).child(
                "Select a test or group to see its results. Run runs the selection, or every \
                 test shown when nothing is selected.",
            );
        };
        let Some((test, state, result)) = self.catalog.get(key) else {
            // A group.
            let counts = self.catalog.counts(|t| {
                (t.path == *key || t.path.starts_with(&format!("{key}."))) && self.keep()(t)
            });
            return body.child(div().text_sm().child(key.clone())).child(
                div().text_color(muted).child(format!(
                    "{} tests · {} passed · {} failed · {} skipped",
                    counts.total, counts.passed, counts.failed, counts.skipped
                )),
            );
        };

        let (glyph, color) = state_glyph(state, muted, theme.primary);
        let mut body = body
            .child(div().text_sm().whitespace_normal().child(test.path.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(color)
                            .child(format!("{glyph} {}", state_label(state))),
                    )
                    .children(result.map(|r| {
                        div()
                            .text_color(muted)
                            .child(format_duration(r.duration_secs))
                    })),
            );
        if let Some((file, line)) = test.source.clone() {
            body = body.child(source_link("test-source", &file, line, muted, hover, cx));
        }
        let Some(result) = result else {
            return body;
        };
        // Problems first, then the log.
        let problems = result.entries.iter().filter(|e| e.kind != EntryKind::Info);
        let log = result
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Info)
            .take(MAX_INFO_LINES);
        for (i, entry) in problems.chain(log).enumerate() {
            let color = match entry.kind {
                EntryKind::Error => failed_color(),
                EntryKind::Warning => warning_color(),
                EntryKind::Info => muted,
            };
            let mut row = div().id(("entry", i)).flex().flex_col().py_0p5().child(
                div()
                    .whitespace_normal()
                    .text_color(color)
                    .child(entry.message.clone()),
            );
            if let Some((file, line)) = entry.source.clone() {
                row = row.child(source_link(
                    ("entry-source", i),
                    &file,
                    line,
                    muted,
                    hover,
                    cx,
                ));
            }
            body = body.child(row);
        }
        body
    }

    fn status_line(&self) -> (String, bool) {
        let counts = self.catalog.counts(self.keep());
        match &self.connection {
            Connection::Disconnected(None) => (
                "Not connected. Connect to this project's editor, or start a headless test editor."
                    .into(),
                false,
            ),
            Connection::Disconnected(Some(e)) => (e.clone(), true),
            Connection::Connecting(status) => (status.clone(), false),
            Connection::Connected(name) => {
                let progress = match self.run {
                    Some((total, done)) => format!("Running {done}/{total} · "),
                    None => String::new(),
                };
                let mut text = format!("{progress}{name} · {} tests", counts.total);
                if counts.passed + counts.failed + counts.skipped > 0 {
                    text += &format!(" · {} passed · {} failed", counts.passed, counts.failed);
                    if counts.skipped > 0 {
                        text += &format!(" · {} skipped", counts.skipped);
                    }
                }
                (text, false)
            }
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (status, is_error) = self.status_line();
        let status_color = if is_error {
            theme.danger
        } else {
            theme.foreground
        };
        let connected = matches!(self.connection, Connection::Connected(_));
        let running = self.run.is_some();
        let targets = if connected {
            self.run_targets().len()
        } else {
            0
        };
        let failed = self.catalog.failed();

        // The status gets its own line: the pane is often narrow, and the
        // buttons would squeeze it to nothing.
        let status = div()
            .px_2()
            .pb_1()
            .truncate()
            .text_xs()
            .text_color(status_color)
            .child(status);
        let mut bar = div().flex().items_center().gap_1().px_2().py_1();
        match &self.connection {
            Connection::Disconnected(_) => {
                let this = cx.entity().downgrade();
                bar = bar.child(
                    Button::new("connect-tests")
                        .xsmall()
                        .primary()
                        .label("Connect")
                        .dropdown_menu(move |menu, _, _| {
                            let open = this.clone();
                            let launch = this.clone();
                            menu.item(PopupMenuItem::new("This project's editor").on_click(
                                move |_, _, cx| {
                                    let _ = open.update(cx, |_, cx| {
                                        cx.emit(TestsPanelEvent::Connect(TestTarget::OpenEditor))
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Start a test editor (headless)").on_click(
                                    move |_, _, cx| {
                                        let _ = launch.update(cx, |_, cx| {
                                            cx.emit(TestsPanelEvent::Connect(
                                                TestTarget::TestEditor,
                                            ))
                                        });
                                    },
                                ),
                            )
                        }),
                );
            }
            Connection::Connecting(_) => {
                bar = bar.child(
                    Button::new("cancel-tests")
                        .xsmall()
                        .outline()
                        .label("Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(TestsPanelEvent::Disconnect))),
                );
            }
            Connection::Connected(_) => {
                bar = bar
                    .when(!running, |bar| {
                        bar.child(
                            Button::new("run-tests")
                                .xsmall()
                                .primary()
                                .label(format!("Run ({targets})"))
                                .tooltip("Run the selected test or group, or every test shown")
                                .disabled(targets == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let tests = this.run_targets();
                                    this.start_run(tests, cx);
                                })),
                        )
                        .child(
                            Button::new("run-failed")
                                .xsmall()
                                .outline()
                                .label(format!("Run failed ({})", failed.len()))
                                .disabled(failed.is_empty())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let tests = this.catalog.failed();
                                    this.start_run(tests, cx);
                                })),
                        )
                        .child(
                            Button::new("refresh-tests")
                                .xsmall()
                                .outline()
                                .label("Refresh")
                                .tooltip("List the tests again, e.g. after Live Coding")
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(TestsPanelEvent::Refresh)),
                                ),
                        )
                    })
                    .when(running, |bar| {
                        bar.child(
                            Button::new("stop-tests")
                                .xsmall()
                                .outline()
                                .label("Stop")
                                .tooltip("Stop after the current test")
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(TestsPanelEvent::Stop)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("disconnect-tests")
                            .xsmall()
                            .ghost()
                            .label("Disconnect")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(TestsPanelEvent::Disconnect)),
                            ),
                    );
            }
        }
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(theme.border)
            .child(bar)
            .child(status)
    }
}

fn state_glyph(state: TestState, muted: Hsla, running: Hsla) -> (&'static str, Hsla) {
    match state {
        TestState::NotRun => ("", muted),
        TestState::Queued => ("○", muted),
        TestState::Running => ("●", running),
        TestState::Passed => ("✓", passed_color()),
        TestState::Failed => ("✗", failed_color()),
        TestState::Skipped => ("–", muted),
    }
}

fn state_label(state: TestState) -> &'static str {
    match state {
        TestState::NotRun => "Not run",
        TestState::Queued => "Queued",
        TestState::Running => "Running",
        TestState::Passed => "Passed",
        TestState::Failed => "Failed",
        TestState::Skipped => "Skipped",
    }
}

fn format_duration(secs: f64) -> String {
    if secs < 1.0 {
        format!("{:.0} ms", secs * 1000.0)
    } else {
        format!("{secs:.2} s")
    }
}

/// `File.cpp:42`, opening the file at that line when clicked.
fn source_link(
    id: impl Into<ElementId>,
    file: &Path,
    line: u32,
    muted: Hsla,
    hover: Hsla,
    cx: &mut Context<TestsPanel>,
) -> impl IntoElement {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let file = file.to_path_buf();
    div()
        .id(id)
        .flex_none()
        .px_1()
        .rounded_sm()
        .text_color(muted)
        .cursor_pointer()
        .hover(|s| s.bg(hover))
        .child(format!("{name}:{line}"))
        .on_click(
            cx.listener(move |_, _, _, cx| {
                cx.emit(TestsPanelEvent::OpenSource(file.clone(), line))
            }),
        )
}

impl EventEmitter<TestsPanelEvent> for TestsPanel {}
impl EventEmitter<PanelEvent> for TestsPanel {}

impl gpui_kit::component::dock::BasePanel for TestsPanel {
    fn panel_name(&self) -> &'static str {
        "TethysTests"
    }
}

impl Panel for TestsPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Tests".into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Tests"
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }
}

impl Focusable for TestsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TestsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let empty_note = if self.catalog.is_empty() {
            None
        } else if self.rows.is_empty() {
            Some(if self.project_only && self.filter_text.is_empty() {
                "No tests are defined in this project. Untick Project only to see the engine's."
            } else {
                "No tests match."
            })
        } else {
            None
        };
        let list = match empty_note {
            Some(note) => div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(note)
                .into_any_element(),
            None => uniform_list("tests", self.rows.len(), cx.processor(Self::render_rows))
                .track_scroll(&self.scroll)
                .flex_1()
                .into_any_element(),
        };
        let project_only = self.project_only;

        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .p_1p5()
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .bg(theme.title_bar)
                    .child(self.render_toolbar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .gap_2()
                            .py_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .child(div().flex_1().min_w(px(120.)).child(
                                                Input::new(&self.filter).xsmall().cleanable(true),
                                            ))
                                            .child(
                                                div().flex_none().child(
                                                    Checkbox::new("project-only")
                                                        .label("Project only")
                                                        .checked(project_only)
                                                        .on_click(cx.listener(
                                                            |this, checked: &bool, _, cx| {
                                                                this.project_only = *checked;
                                                                this.rebuild();
                                                                cx.notify();
                                                            },
                                                        )),
                                                ),
                                            ),
                                    )
                                    .child(list),
                            )
                            .child(div().w_px().h_full().bg(theme.border))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(self.render_details(cx)),
                            ),
                    ),
            )
    }
}
