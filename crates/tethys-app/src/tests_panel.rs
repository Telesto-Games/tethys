//! The Tests pane: an editor's automation tests as a tree, run state and
//! results, and the selected test's log. It keeps the test catalog and its own
//! view state (filter, expanded groups, selection); the workspace owns the
//! connection and forwards its events here.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::automation::{
    AutomationTest, EntryKind, TestCatalog, TestCounts, TestEnd, TestEntry, TestEvent, TestRow,
    TestState,
};
use tethys_core::usecases::TestTarget;

const ROW_HEIGHT: f32 = 22.;
const INDENT: f32 = 14.;
/// Log lines shown for one test; a chatty test can log thousands.
const MAX_INFO_LINES: usize = 500;
/// The gap between cells of the results grid.
const CELL_GAP: f32 = 1.;
/// The results grid's starting height; the split under it is draggable.
const GRID_HEIGHT: f32 = 120.;

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

/// The pane's state apart from GPUI handles, so it can be driven and read
/// without a window.
struct TestsState {
    project_root: PathBuf,
    catalog: TestCatalog,
    connection: Connection,
    /// Tests in the current run, and how many have finished.
    run: Option<(usize, usize)>,
    /// The filter, trimmed and lowercased.
    filter_text: String,
    project_only: bool,
    expanded: HashSet<String>,
    selected: Option<String>,
    rows: Vec<TestRow>,
}

impl TestsState {
    fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            catalog: TestCatalog::default(),
            connection: Connection::Disconnected(None),
            run: None,
            filter_text: String::new(),
            project_only: true,
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
        }
    }

    fn set_filter(&mut self, text: &str) {
        self.filter_text = text.trim().to_lowercase();
        self.rebuild();
    }

    fn set_project_only(&mut self, project_only: bool) {
        self.project_only = project_only;
        self.rebuild();
    }

    fn apply(&mut self, event: TestEvent) {
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
    }

    /// Marks `tests` as queued and starts a run of them, or returns false
    /// when there's nothing to run or a run is already going.
    fn start_run(&mut self, tests: &[AutomationTest]) -> bool {
        if tests.is_empty() || self.run.is_some() {
            return false;
        }
        self.catalog.queue(tests);
        self.run = Some((tests.len(), 0));
        self.rebuild();
        true
    }

    fn keep(&self) -> impl Fn(&AutomationTest) -> bool + '_ {
        move |t: &AutomationTest| {
            keep_test(t, self.project_only, &self.project_root, &self.filter_text)
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

    /// Selects row `ix`, and opens or closes it if it's a group.
    fn click_row(&mut self, ix: usize) {
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
    }

    /// One entry per test shown, in path order: what the results grid draws.
    fn cells(&self) -> Vec<(String, TestState)> {
        let keep = self.keep();
        self.catalog
            .all()
            .iter()
            .filter(|t| keep(t))
            .filter_map(|t| {
                self.catalog
                    .get(&t.path)
                    .map(|(_, s, _)| (t.path.clone(), s))
            })
            .collect()
    }

    /// Selects the test behind grid cell `ix`, and opens its groups so the
    /// tree shows it.
    fn click_cell(&mut self, ix: usize) {
        let Some((path, _)) = self.cells().into_iter().nth(ix) else {
            return;
        };
        let mut prefix = String::new();
        for part in path.split('.') {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(part);
            if prefix != path {
                self.expanded.insert(prefix.clone());
            }
        }
        self.selected = Some(path);
        self.rebuild();
    }

    /// Totals for a group: the tests under it that are shown.
    fn group_counts(&self, key: &str) -> TestCounts {
        self.catalog
            .counts(|t| in_group(&t.path, key) && self.keep()(t))
    }

    /// The text under the toolbar, and whether it's an error.
    fn status_line(&self) -> (String, bool) {
        status_line(&self.connection, self.run, self.catalog.counts(self.keep()))
    }

    /// Why the list is empty, if it is and that needs explaining.
    fn empty_note(&self) -> Option<&'static str> {
        empty_note(
            self.catalog.is_empty(),
            self.rows.is_empty(),
            self.project_only,
            self.filter_text.is_empty(),
        )
    }
}

pub struct TestsPanel {
    focus: FocusHandle,
    state: TestsState,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    /// Where the results grid was last drawn, so a click can find its cell.
    grid_bounds: Rc<Cell<Bounds<Pixels>>>,
    _filter_events: Subscription,
}

impl TestsPanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tests"));
        let filter_events = cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let text = input.read(cx).value().to_string();
                this.state.set_filter(&text);
                cx.notify();
            }
        });
        Self {
            focus: cx.focus_handle(),
            state: TestsState::new(project_root),
            filter,
            scroll: UniformListScrollHandle::new(),
            grid_bounds: Rc::new(Cell::new(Bounds::default())),
            _filter_events: filter_events,
        }
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self.state.connection, Connection::Disconnected(_))
    }

    /// Connecting has started; `status` says how.
    pub fn set_connecting(&mut self, status: String, cx: &mut Context<Self>) {
        self.state.connection = Connection::Connecting(status);
        cx.notify();
    }

    /// Connecting failed before a session existed.
    pub fn set_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.state.connection = Connection::Disconnected(Some(error));
        self.state.run = None;
        cx.notify();
    }

    pub fn apply(&mut self, event: TestEvent, cx: &mut Context<Self>) {
        self.state.apply(event);
        cx.notify();
    }

    /// Marks `tests` as queued; the workspace sends them to the editor.
    fn start_run(&mut self, tests: Vec<AutomationTest>, cx: &mut Context<Self>) {
        if self.state.start_run(&tests) {
            cx.emit(TestsPanelEvent::Run(tests));
            cx.notify();
        }
    }

    fn click_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.state.click_row(ix);
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
            .filter_map(|ix| self.state.rows.get(ix).cloned().map(|row| (ix, row)))
            .map(|(ix, row)| {
                let selected = self.state.selected.as_deref() == Some(row.key.as_str());
                let (glyph, color) = state_glyph(row.state, muted, primary);
                let trailing = if row.is_group {
                    Some(row.count.to_string())
                } else {
                    self.state
                        .catalog
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

    /// The results grid: one cell per test shown, coloured by its state, so a
    /// run's progress and failures can be seen at a glance.
    fn render_grid(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let palette = CellPalette {
            not_run: theme.secondary,
            queued: theme.muted_foreground.opacity(0.35),
            running: theme.primary,
            passed: passed_color(),
            failed: failed_color(),
            skipped: warning_color().opacity(0.6),
        };
        let cells = self.state.cells();
        let count = cells.len();
        let selected = self
            .state
            .selected
            .as_deref()
            .and_then(|key| cells.iter().position(|(path, _)| path == key));
        let colors: Vec<Hsla> = cells.iter().map(|(_, s)| palette.color(*s)).collect();
        let bounds_out = self.grid_bounds.clone();
        let outline = theme.foreground;
        div()
            .id("test-grid")
            .size_full()
            .p_1()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    let bounds = this.grid_bounds.get();
                    let count = this.state.cells().len();
                    if let Some(ix) = cell_at(count, bounds, event.position) {
                        this.state.click_cell(ix);
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        bounds_out.set(bounds);
                        bounds
                    },
                    move |_, bounds, window, _| {
                        for (ix, color) in colors.iter().enumerate() {
                            let Some(cell) = cell_bounds(count, bounds, ix) else {
                                break;
                            };
                            window.paint_quad(fill(cell, *color));
                            if selected == Some(ix) {
                                window.paint_quad(outline_quad(cell, outline));
                            }
                        }
                    },
                )
                .size_full(),
            )
            // Lets view tests find and click the grid; nothing outside tests.
            .test_support()
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
        let Some(key) = &self.state.selected else {
            return body.text_color(muted).child(
                "Select a test or group to see its results. Run runs the selection, or every \
                 test shown when nothing is selected.",
            );
        };
        let Some((test, state, result)) = self.state.catalog.get(key) else {
            // A group.
            let counts = self.state.group_counts(key);
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
        for (i, entry) in ordered_entries(&result.entries).enumerate() {
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

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (status, is_error) = self.state.status_line();
        let status_color = if is_error {
            theme.danger
        } else {
            theme.foreground
        };
        let connected = matches!(self.state.connection, Connection::Connected(_));
        let running = self.state.run.is_some();
        let targets = if connected {
            self.state.run_targets().len()
        } else {
            0
        };
        let failed = self.state.catalog.failed();

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
        match &self.state.connection {
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
                                    let tests = this.state.run_targets();
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
                                    let tests = this.state.catalog.failed();
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

/// The grid's colour for each state.
struct CellPalette {
    not_run: Hsla,
    queued: Hsla,
    running: Hsla,
    passed: Hsla,
    failed: Hsla,
    skipped: Hsla,
}

impl CellPalette {
    fn color(&self, state: TestState) -> Hsla {
        match state {
            TestState::NotRun => self.not_run,
            TestState::Queued => self.queued,
            TestState::Running => self.running,
            TestState::Passed => self.passed,
            TestState::Failed => self.failed,
            TestState::Skipped => self.skipped,
        }
    }
}

/// Columns and rows that split a `width` x `height` rectangle into `count`
/// cells as near to square as it can: no rows for nothing, and never more
/// columns than cells.
fn grid_dims(count: usize, width: f32, height: f32) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let aspect = if height > 0. && width > 0. {
        width / height
    } else {
        1.
    };
    let cols = ((count as f32 * aspect).sqrt().ceil() as usize).clamp(1, count);
    let rows = count.div_ceil(cols);
    (cols, rows)
}

/// Where cell `ix` of `count` is drawn inside `bounds`, cells reading left to
/// right then down, with [`CELL_GAP`] between them.
fn cell_bounds(count: usize, bounds: Bounds<Pixels>, ix: usize) -> Option<Bounds<Pixels>> {
    if ix >= count {
        return None;
    }
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (cols, rows) = grid_dims(count, w, h);
    let cell_w = (w - CELL_GAP * (cols - 1) as f32) / cols as f32;
    let cell_h = (h - CELL_GAP * (rows - 1) as f32) / rows as f32;
    if cell_w <= 0. || cell_h <= 0. {
        return None;
    }
    let (col, row) = (ix % cols, ix / cols);
    let x = f32::from(bounds.origin.x) + col as f32 * (cell_w + CELL_GAP);
    let y = f32::from(bounds.origin.y) + row as f32 * (cell_h + CELL_GAP);
    Some(Bounds::new(
        point(px(x), px(y)),
        size(px(cell_w), px(cell_h)),
    ))
}

/// The cell under `position`, if it's over one of the `count` cells in
/// `bounds` (gaps count as the cell to their left or above).
fn cell_at(count: usize, bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<usize> {
    if count == 0 || !bounds.contains(&position) {
        return None;
    }
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (cols, rows) = grid_dims(count, w, h);
    let fx = f32::from(position.x - bounds.origin.x) / w;
    let fy = f32::from(position.y - bounds.origin.y) / h;
    let col = ((fx * cols as f32) as usize).min(cols - 1);
    let row = ((fy * rows as f32) as usize).min(rows - 1);
    let ix = row * cols + col;
    (ix < count).then_some(ix)
}

/// A one-pixel border around `cell`, for the selected test.
fn outline_quad(cell: Bounds<Pixels>, color: Hsla) -> PaintQuad {
    fill(cell, transparent_black())
        .border_color(color)
        .border_widths(Edges::all(px(1.)))
}

/// `412 ms` under a second, else `1.25 s`; rounded first, so 999.6 ms is `1.00 s`.
fn format_duration(secs: f64) -> String {
    let ms = (secs * 1000.0).round();
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{secs:.2} s")
    }
}

/// Whether a test is shown: the project's own (unless `project_only` is off)
/// and matching `filter_lower` (a trimmed, lowercased filter, or empty).
fn keep_test(t: &AutomationTest, project_only: bool, root: &Path, filter_lower: &str) -> bool {
    (!project_only || t.is_under(root))
        && (filter_lower.is_empty() || t.path.to_lowercase().contains(filter_lower))
}

/// Whether a test `path` is the group `key` or inside it: `A.B` holds `A.B`
/// and `A.B.C`, not `A.BC`.
fn in_group(path: &str, key: &str) -> bool {
    path == key
        || path
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// The text under the toolbar for a connection, run and totals, and whether
/// it's an error.
fn status_line(
    connection: &Connection,
    run: Option<(usize, usize)>,
    counts: TestCounts,
) -> (String, bool) {
    match connection {
        Connection::Disconnected(None) => (
            "Not connected. Connect to this project's editor, or start a headless test editor."
                .into(),
            false,
        ),
        Connection::Disconnected(Some(e)) => (e.clone(), true),
        Connection::Connecting(status) => (status.clone(), false),
        Connection::Connected(name) => {
            let progress = match run {
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

/// A result's entries for display: problems first, then at most
/// `MAX_INFO_LINES` of the log.
fn ordered_entries(entries: &[TestEntry]) -> impl Iterator<Item = &TestEntry> {
    let problems = entries.iter().filter(|e| e.kind != EntryKind::Info);
    let log = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Info)
        .take(MAX_INFO_LINES);
    problems.chain(log)
}

/// Why the list is empty when the catalog isn't, if it is.
fn empty_note(
    catalog_empty: bool,
    rows_empty: bool,
    project_only: bool,
    filter_empty: bool,
) -> Option<&'static str> {
    if catalog_empty || !rows_empty {
        None
    } else if project_only && filter_empty {
        Some("No tests are defined in this project. Untick Project only to see the engine's.")
    } else {
        Some("No tests match.")
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
    fn zoomable(&self, _: &App) -> bool {
        false
    }

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
        let list = match self.state.empty_note() {
            Some(note) => div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(note)
                .into_any_element(),
            None => uniform_list(
                "tests",
                self.state.rows.len(),
                cx.processor(Self::render_rows),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element(),
        };
        let project_only = self.state.project_only;

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
                                                                this.state
                                                                    .set_project_only(*checked);
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
                                div().flex_1().min_w_0().child(
                                    v_resizable("test-results")
                                        .child(
                                            resizable_panel()
                                                .size(px(GRID_HEIGHT))
                                                .size_range(px(40.)..px(f32::MAX))
                                                .child(self.render_grid(cx)),
                                        )
                                        .child(
                                            resizable_panel().child(
                                                div()
                                                    .id("test-details-pane")
                                                    .size_full()
                                                    .flex()
                                                    .flex_col()
                                                    .child(self.render_details(cx))
                                                    .test_support(),
                                            ),
                                        ),
                                ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gpui_kit::{Hsla, rgb};
    use tethys_core::automation::{
        AutomationTest, EntryKind, TestCounts, TestEnd, TestEntry, TestEvent, TestResult, TestState,
    };

    use super::{
        Connection, MAX_INFO_LINES, TestsState, cell_at, cell_bounds, empty_note, format_duration,
        grid_dims, in_group, keep_test, ordered_entries, state_glyph, state_label, status_line,
    };
    use gpui_kit::{Bounds, Pixels, point, px, size};

    const ROOT: &str = r"D:\dev\Game";

    fn test(path: &str, file: Option<&str>) -> AutomationTest {
        AutomationTest {
            path: path.into(),
            command: path.into(),
            source: file.map(|f| (PathBuf::from(f), 1)),
        }
    }

    fn project_test(path: &str) -> AutomationTest {
        test(path, Some(r"D:\dev\Game\Source\Tests.cpp"))
    }

    fn engine_test(path: &str) -> AutomationTest {
        test(path, Some(r"C:\UE\Engine\Source\Tests.cpp"))
    }

    fn passed(secs: f64) -> TestResult {
        TestResult {
            state: TestState::Passed,
            duration_secs: secs,
            entries: Vec::new(),
        }
    }

    fn entry(kind: EntryKind, message: &str) -> TestEntry {
        TestEntry {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// A connected state with the project's tests listed.
    fn connected(tests: Vec<AutomationTest>) -> TestsState {
        let mut state = TestsState::new(PathBuf::from(ROOT));
        state.apply(TestEvent::Connected {
            name: "Editor".into(),
        });
        state.apply(TestEvent::Tests(tests));
        state
    }

    fn catalog() -> Vec<AutomationTest> {
        vec![
            project_test("Project.Inventory.Accounting"),
            project_test("Project.Inventory.Stacking"),
            project_test("Project.Hull.Livery"),
            engine_test("Engine.Core.Math"),
        ]
    }

    fn bounds(w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(w), px(h)))
    }

    #[test]
    fn grid_dims_fill_the_rectangle_with_near_square_cells() {
        assert_eq!(grid_dims(0, 100., 100.), (0, 0));
        assert_eq!(grid_dims(1, 100., 100.), (1, 1));
        assert_eq!(grid_dims(4, 100., 100.), (2, 2));
        assert_eq!(grid_dims(9, 300., 300.), (3, 3));
        // Twice as wide as high: twice the columns.
        assert_eq!(grid_dims(8, 200., 100.), (4, 2));
        // Never more columns than tests, and a row for the remainder.
        assert_eq!(grid_dims(3, 1000., 10.), (3, 1));
        assert_eq!(grid_dims(5, 100., 100.), (3, 2));
        assert_eq!(grid_dims(2, 0., 0.), (2, 1));
    }

    #[test]
    fn cells_tile_left_to_right_then_down_and_clicks_find_them() {
        // 201 wide: two 100-pixel cells and the gap between them.
        let b = bounds(201., 201.);
        assert_eq!(grid_dims(4, 201., 201.), (2, 2));
        let c0 = cell_bounds(4, b, 0).unwrap();
        let c1 = cell_bounds(4, b, 1).unwrap();
        let c2 = cell_bounds(4, b, 2).unwrap();
        assert_eq!(c0.origin, point(px(10.), px(20.)));
        assert_eq!(c0.size, size(px(100.), px(100.)));
        assert_eq!(c1.origin, point(px(111.), px(20.)));
        assert_eq!(c2.origin, point(px(10.), px(121.)));
        assert!(cell_bounds(4, b, 4).is_none());
        assert!(cell_bounds(0, b, 0).is_none());

        assert_eq!(cell_at(4, b, point(px(10.), px(20.))), Some(0));
        assert_eq!(cell_at(4, b, point(px(150.), px(30.))), Some(1));
        assert_eq!(cell_at(4, b, point(px(50.), px(150.))), Some(2));
        assert_eq!(cell_at(4, b, point(px(200.), px(220.))), Some(3));
        assert_eq!(cell_at(4, b, point(px(5.), px(30.))), None);
        assert_eq!(cell_at(0, b, point(px(50.), px(30.))), None);
        // Three cells in a 2x2 grid: the empty slot is nothing.
        assert_eq!(cell_at(3, b, point(px(200.), px(220.))), None);
    }

    #[test]
    fn grid_cells_follow_the_filter_and_the_run() {
        let mut state = connected(catalog());
        let shown: Vec<_> = state.cells().into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            shown,
            [
                "Project.Hull.Livery",
                "Project.Inventory.Accounting",
                "Project.Inventory.Stacking"
            ]
        );
        state.start_run(&[project_test("Project.Inventory.Stacking")]);
        state.apply(TestEvent::Started("Project.Inventory.Stacking".into()));
        assert_eq!(state.cells()[2].1, TestState::Running);
        state.apply(TestEvent::Finished(
            "Project.Inventory.Stacking".into(),
            passed(0.1),
        ));
        assert_eq!(state.cells()[2].1, TestState::Passed);
        assert_eq!(state.cells()[0].1, TestState::NotRun);
        state.set_project_only(false);
        assert_eq!(state.cells().len(), 4);
    }

    #[test]
    fn clicking_a_cell_selects_its_test_and_opens_its_groups() {
        let mut state = connected(catalog());
        state.expanded.clear();
        state.rebuild();
        state.click_cell(2);
        assert_eq!(
            state.selected.as_deref(),
            Some("Project.Inventory.Stacking")
        );
        assert!(state.expanded.contains("Project"));
        assert!(state.expanded.contains("Project.Inventory"));
        assert!(!state.expanded.contains("Project.Inventory.Stacking"));
        assert!(
            state
                .rows
                .iter()
                .any(|r| r.key == "Project.Inventory.Stacking")
        );
        state.click_cell(99);
        assert_eq!(
            state.selected.as_deref(),
            Some("Project.Inventory.Stacking")
        );
    }

    #[test]
    fn durations_round_before_choosing_the_unit() {
        assert_eq!(format_duration(0.412), "412 ms");
        assert_eq!(format_duration(0.0004), "0 ms");
        assert_eq!(format_duration(0.9996), "1.00 s");
        assert_eq!(format_duration(1.0), "1.00 s");
        assert_eq!(format_duration(12.345), "12.35 s");
    }

    #[test]
    fn states_have_a_label_and_a_glyph() {
        let muted: Hsla = rgb(0x111111).into();
        let running: Hsla = rgb(0x222222).into();
        assert_eq!(state_label(TestState::NotRun), "Not run");
        assert_eq!(state_label(TestState::Failed), "Failed");
        assert_eq!(state_glyph(TestState::NotRun, muted, running), ("", muted));
        assert_eq!(state_glyph(TestState::Queued, muted, running), ("○", muted));
        assert_eq!(
            state_glyph(TestState::Running, muted, running),
            ("●", running)
        );
        assert_eq!(state_glyph(TestState::Passed, muted, running).0, "✓");
        assert_eq!(state_glyph(TestState::Failed, muted, running).0, "✗");
        assert_eq!(
            state_glyph(TestState::Skipped, muted, running),
            ("–", muted)
        );
    }

    #[test]
    fn keep_respects_project_only_and_the_filter() {
        let root = Path::new(ROOT);
        let mine = project_test("Project.Hull.Livery");
        let theirs = engine_test("Engine.Core.Math");
        let nowhere = test("Unknown.Test", None);
        assert!(keep_test(&mine, true, root, ""));
        assert!(!keep_test(&theirs, true, root, ""));
        assert!(!keep_test(&nowhere, true, root, ""));
        assert!(keep_test(&theirs, false, root, ""));
        assert!(keep_test(&nowhere, false, root, ""));
        // The filter is already lowercased; paths match case-insensitively.
        assert!(keep_test(&mine, true, root, "hull"));
        assert!(keep_test(&theirs, false, root, "core.math"));
        assert!(!keep_test(&mine, true, root, "engine"));
        assert!(!keep_test(&theirs, true, root, "engine"));
    }

    #[test]
    fn groups_hold_their_prefix_and_not_lookalikes() {
        assert!(in_group("A.B", "A.B"));
        assert!(in_group("A.B.C", "A.B"));
        assert!(in_group("A.B.C.D", "A.B"));
        assert!(!in_group("A.BC", "A.B"));
        assert!(!in_group("A", "A.B"));
        assert!(!in_group("B.A.B", "A.B"));
    }

    #[test]
    fn status_line_follows_the_connection() {
        let none = TestCounts::default();
        let (text, error) = status_line(&Connection::Disconnected(None), None, none);
        assert!(text.starts_with("Not connected."));
        assert!(!error);
        assert_eq!(
            status_line(&Connection::Disconnected(Some("Lost".into())), None, none),
            ("Lost".to_string(), true)
        );
        assert_eq!(
            status_line(&Connection::Connecting("Finding…".into()), None, none),
            ("Finding…".to_string(), false)
        );
    }

    #[test]
    fn status_line_counts_only_what_has_happened() {
        let connected = Connection::Connected("Editor".into());
        let fresh = TestCounts {
            total: 4,
            ..TestCounts::default()
        };
        assert_eq!(
            status_line(&connected, None, fresh),
            ("Editor · 4 tests".to_string(), false)
        );
        let some = TestCounts {
            total: 4,
            passed: 2,
            failed: 1,
            skipped: 0,
            pending: 1,
        };
        assert_eq!(
            status_line(&connected, Some((4, 3)), some).0,
            "Running 3/4 · Editor · 4 tests · 2 passed · 1 failed"
        );
        let skipped = TestCounts { skipped: 1, ..some };
        assert_eq!(
            status_line(&connected, None, skipped).0,
            "Editor · 4 tests · 2 passed · 1 failed · 1 skipped"
        );
    }

    #[test]
    fn problems_come_first_and_the_log_is_capped() {
        let mut entries = vec![entry(EntryKind::Info, "first log")];
        entries.push(entry(EntryKind::Error, "boom"));
        entries.extend((0..MAX_INFO_LINES + 5).map(|i| entry(EntryKind::Info, &i.to_string())));
        entries.push(entry(EntryKind::Warning, "hmm"));
        let shown: Vec<&TestEntry> = ordered_entries(&entries).collect();
        assert_eq!(shown[0].message, "boom");
        assert_eq!(shown[1].message, "hmm");
        assert_eq!(shown[2].message, "first log");
        assert_eq!(shown.len(), 2 + MAX_INFO_LINES);
        assert!(shown.iter().skip(2).all(|e| e.kind == EntryKind::Info));
    }

    #[test]
    fn empty_notes_explain_why_nothing_shows() {
        assert_eq!(empty_note(true, true, true, true), None);
        assert_eq!(empty_note(false, false, true, true), None);
        assert!(
            empty_note(false, true, true, true)
                .unwrap()
                .starts_with("No tests are defined in this project.")
        );
        assert_eq!(
            empty_note(false, true, false, true),
            Some("No tests match.")
        );
        assert_eq!(
            empty_note(false, true, true, false),
            Some("No tests match.")
        );
    }

    #[test]
    fn listing_tests_opens_the_projects_groups_once() {
        let state = connected(catalog());
        let mut expanded: Vec<_> = state.expanded.iter().cloned().collect();
        expanded.sort();
        assert_eq!(expanded, ["Project", "Project.Hull", "Project.Inventory"]);
        let labels: Vec<_> = state.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(
            labels,
            [
                "Project",
                "Hull",
                "Livery",
                "Inventory",
                "Accounting",
                "Stacking"
            ]
        );

        // A refresh keeps whatever the user has opened or closed since.
        let mut state = state;
        state.expanded.clear();
        state.expanded.insert("Project".into());
        state.apply(TestEvent::Tests(catalog()));
        assert_eq!(state.expanded.len(), 1);
    }

    #[test]
    fn status_only_updates_while_connecting() {
        let mut state = TestsState::new(PathBuf::from(ROOT));
        state.apply(TestEvent::Status("ignored".into()));
        assert!(matches!(state.connection, Connection::Disconnected(None)));
        state.connection = Connection::Connecting("Finding".into());
        state.apply(TestEvent::Status(String::new()));
        assert!(matches!(&state.connection, Connection::Connecting(s) if s == "Finding"));
        state.apply(TestEvent::Status("Connecting".into()));
        assert!(matches!(&state.connection, Connection::Connecting(s) if s == "Connecting"));
    }

    #[test]
    fn a_run_counts_finishes_and_ends_with_the_run_or_the_session() {
        let mut state = connected(catalog());
        let targets = state.run_targets();
        assert_eq!(targets.len(), 3, "project tests only");
        assert!(state.start_run(&targets));
        assert_eq!(state.run, Some((3, 0)));
        assert!(!state.start_run(&targets), "one run at a time");
        assert!(!TestsState::new(PathBuf::from(ROOT)).start_run(&[]));

        state.apply(TestEvent::Started("Project.Hull.Livery".into()));
        state.apply(TestEvent::Finished(
            "Project.Hull.Livery".into(),
            passed(0.5),
        ));
        assert_eq!(state.run, Some((3, 1)));
        state.apply(TestEvent::RunEnded { stopped: true });
        assert_eq!(state.run, None);
        assert!(matches!(state.connection, Connection::Connected(_)));

        assert!(state.start_run(&targets));
        state.apply(TestEvent::Ended(TestEnd::Failed("editor quit".into())));
        assert_eq!(state.run, None);
        assert!(
            matches!(&state.connection, Connection::Disconnected(Some(e)) if e == "editor quit")
        );
        state.apply(TestEvent::Ended(TestEnd::Disconnected));
        assert!(matches!(state.connection, Connection::Disconnected(None)));
    }

    #[test]
    fn run_targets_follow_the_selection_and_the_filter() {
        let mut state = connected(catalog());
        state.selected = Some("Project.Inventory".into());
        let paths: Vec<_> = state.run_targets().into_iter().map(|t| t.path).collect();
        assert_eq!(
            paths,
            ["Project.Inventory.Accounting", "Project.Inventory.Stacking"]
        );
        state.set_filter("  STACK ");
        assert_eq!(state.filter_text, "stack");
        let paths: Vec<_> = state.run_targets().into_iter().map(|t| t.path).collect();
        assert_eq!(paths, ["Project.Inventory.Stacking"]);

        state.selected = None;
        state.set_filter("");
        state.set_project_only(false);
        assert_eq!(state.run_targets().len(), 4);
        assert_eq!(state.group_counts("Project.Inventory").total, 2);
        assert_eq!(state.group_counts("Engine").total, 1);
    }

    #[test]
    fn clicking_a_group_toggles_it_and_clicking_anything_selects_it() {
        let mut state = connected(catalog());
        // Row 1 is the open Hull group.
        state.click_row(1);
        assert_eq!(state.selected.as_deref(), Some("Project.Hull"));
        assert!(!state.expanded.contains("Project.Hull"));
        let labels: Vec<_> = state.rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(
            labels,
            ["Project", "Hull", "Inventory", "Accounting", "Stacking"]
        );
        state.click_row(1);
        assert!(state.expanded.contains("Project.Hull"));
        state.click_row(4);
        assert_eq!(
            state.selected.as_deref(),
            Some("Project.Inventory.Accounting")
        );
        assert_eq!(state.rows.len(), 6);
        state.click_row(99);
        assert_eq!(
            state.selected.as_deref(),
            Some("Project.Inventory.Accounting")
        );
    }

    #[test]
    fn the_list_explains_itself_when_filtered_to_nothing() {
        let mut state = connected(vec![engine_test("Engine.Core.Math")]);
        assert!(
            state
                .empty_note()
                .unwrap()
                .starts_with("No tests are defined")
        );
        state.set_project_only(false);
        assert_eq!(state.empty_note(), None);
        state.set_filter("nothing");
        assert_eq!(state.empty_note(), Some("No tests match."));
    }
}

#[cfg(test)]
mod view_tests {
    use std::path::PathBuf;

    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, TestAppContext, point, px, size};
    use tethys_core::automation::{AutomationTest, TestEvent, TestResult, TestState};

    use super::TestsPanel;
    use crate::test_support::{self, open_window, temp_config};

    fn test(path: &str) -> AutomationTest {
        AutomationTest {
            path: path.into(),
            command: path.into(),
            source: Some((PathBuf::from(r"D:\dev\Game\Source\Tests.cpp"), 1)),
        }
    }

    #[gpui_kit::test]
    fn the_grid_draws_above_the_details_and_a_cell_click_selects_its_test(cx: &mut TestAppContext) {
        let config = temp_config("tests-grid");
        cx.update(|cx| test_support::init(config, cx));
        let (window, panel) = open_window(cx, size(px(700.), px(500.)), |window, cx| {
            cx.new(|cx| TestsPanel::new(PathBuf::from(r"D:\dev\Game"), window, cx))
        });
        cx.update_window(window.into(), |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.apply(
                    TestEvent::Connected {
                        name: "Editor".into(),
                    },
                    cx,
                );
                panel.apply(
                    TestEvent::Tests(vec![
                        test("Project.A"),
                        test("Project.B"),
                        test("Project.C"),
                        test("Project.D"),
                    ]),
                    cx,
                );
                panel.apply(TestEvent::Started("Project.A".into()), cx);
                panel.apply(
                    TestEvent::Finished(
                        "Project.A".into(),
                        TestResult {
                            state: TestState::Failed,
                            duration_secs: 0.1,
                            entries: Vec::new(),
                        },
                    ),
                    cx,
                );
            });
            // The split settles its panel sizes after its first layout.
            window.render_frame(cx);
            window.render_frame(cx);
            let grid = window.find("test-grid");
            let details = window.find("test-details-pane");
            assert!(grid.visible() && details.visible());
            assert!(grid.bounds().bottom() <= details.bounds().top());
            assert_eq!(panel.read(cx).state.selected, None);

            // The last cell is at the bottom right of the grid.
            let grid_size = grid.bounds().size;
            window.click_at(
                "test-grid",
                point(grid_size.width - px(8.), grid_size.height - px(8.)),
                cx,
            );
            window.render_frame(cx);
            assert_eq!(panel.read(cx).state.selected.as_deref(), Some("Project.D"));

            window.click_at("test-grid", point(px(8.), px(8.)), cx);
            window.render_frame(cx);
            assert_eq!(panel.read(cx).state.selected.as_deref(), Some("Project.A"));
        })
        .unwrap();
    }
}
