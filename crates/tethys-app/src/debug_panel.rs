//! The debugger pane: status, controls, the stopped thread's stack and the
//! breakpoint list. It only draws what the workspace hands it and reports
//! clicks back; the workspace owns the debug session.

use std::path::{Path, PathBuf};

use gpui_kit::base::Disableable;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::debug::{BreakpointId, BreakpointState};

/// Where the debugger is, as the panel shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DebugStatus {
    #[default]
    Detached,
    Attaching,
    Running,
    Stopped(String),
}

/// A stack frame, ready to draw.
#[derive(Debug, Clone)]
pub struct FrameRow {
    pub function: String,
    /// `File.cpp:42`, if known.
    pub location: Option<String>,
    /// Where to open it, if the file is on this machine.
    pub source: Option<(PathBuf, u32)>,
    pub current: bool,
}

#[derive(Debug, Clone)]
pub struct BreakpointRow {
    pub id: BreakpointId,
    pub file: PathBuf,
    pub line: u32,
    pub state: BreakpointState,
}

#[derive(Debug, Clone, Default)]
pub struct DebugView {
    pub status: DebugStatus,
    /// e.g. `UnrealEditor.exe (pid 1234)`.
    pub process: Option<String>,
    pub frames: Vec<FrameRow>,
    pub breakpoints: Vec<BreakpointRow>,
}

pub enum DebugPanelEvent {
    Attach,
    Detach,
    Continue,
    Pause,
    /// Open this source at this 1-based line, and show it as the current frame.
    OpenFrame(usize),
    OpenBreakpoint(PathBuf, u32),
    RemoveBreakpoint(BreakpointId),
}

/// How a piece of status text is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Muted,
    Normal,
    /// The accent: something to look at.
    Primary,
    Danger,
}

/// The toolbar for a status: its text and which controls are live.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolbarState {
    text: String,
    tone: Tone,
    continue_enabled: bool,
    pause_enabled: bool,
    /// Detach replaces Debug while attached.
    show_detach: bool,
    attach_enabled: bool,
}

fn toolbar_state(status: &DebugStatus) -> ToolbarState {
    let (text, tone) = match status {
        DebugStatus::Detached => ("Not attached".to_string(), Tone::Muted),
        DebugStatus::Attaching => ("Starting the debugger…".to_string(), Tone::Muted),
        DebugStatus::Running => ("Running".to_string(), Tone::Normal),
        DebugStatus::Stopped(reason) => (format!("Stopped: {reason}"), Tone::Primary),
    };
    let stopped = matches!(status, DebugStatus::Stopped(_));
    ToolbarState {
        text,
        tone,
        continue_enabled: stopped,
        pause_enabled: *status == DebugStatus::Running,
        show_detach: stopped || *status == DebugStatus::Running,
        attach_enabled: *status != DebugStatus::Attaching,
    }
}

/// A breakpoint's state as the list shows it.
fn breakpoint_state_label(state: &BreakpointState) -> (String, Tone) {
    match state {
        BreakpointState::Unset => ("not attached".to_string(), Tone::Muted),
        BreakpointState::Bound => ("set".to_string(), Tone::Normal),
        BreakpointState::Pending => ("waiting for its module".to_string(), Tone::Muted),
        BreakpointState::Failed(e) => (e.clone(), Tone::Danger),
    }
}

/// `File.cpp:42`: a file's name and a 1-based line.
pub(crate) fn file_line_label(file: &Path, line: u32) -> String {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{name}:{line}")
}

pub struct DebugPanel {
    view: DebugView,
    focus: FocusHandle,
}

impl DebugPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            view: DebugView::default(),
            focus: cx.focus_handle(),
        }
    }

    pub fn set_view(&mut self, view: DebugView, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    fn render_frames(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.secondary_hover;
        let accent = theme.primary;
        let rows = self.view.frames.iter().enumerate().map(|(i, frame)| {
            let openable = frame.source.is_some();
            div()
                .id(("frame", i))
                .flex()
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded_sm()
                .when(frame.current, |d| d.bg(accent.opacity(0.15)))
                .when(openable, |d| d.cursor_pointer().hover(|s| s.bg(hover)))
                .when(!openable, |d| d.text_color(muted))
                .child(
                    div()
                        .flex_none()
                        .w(px(24.))
                        .text_color(muted)
                        .child(i.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(frame.function.clone()),
                )
                .children(
                    frame
                        .location
                        .clone()
                        .map(|l| div().flex_none().text_color(muted).child(l)),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(DebugPanelEvent::OpenFrame(i));
                }))
        });
        section("Call stack", cx).child(
            div()
                .id("frames")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .text_xs()
                .children(rows)
                .when(self.view.frames.is_empty(), |d| {
                    d.px_2().text_color(muted).child(match self.view.status {
                        DebugStatus::Running => {
                            "Running. Pause, or wait for a breakpoint or crash."
                        }
                        _ => "Shown when the editor stops.",
                    })
                }),
        )
    }

    fn render_breakpoints(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let hover = theme.secondary_hover;
        let tone_color = |tone: Tone| match tone {
            Tone::Muted => muted,
            Tone::Normal => theme.foreground,
            Tone::Primary => theme.primary,
            Tone::Danger => danger,
        };
        let rows = self.view.breakpoints.iter().map(|b| {
            let (state, tone) = breakpoint_state_label(&b.state);
            let color = tone_color(tone);
            let label = file_line_label(&b.file, b.line);
            let (file, line, id) = (b.file.clone(), b.line, b.id);
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded_sm()
                .hover(|s| s.bg(hover))
                .child(div().flex_none().size(px(8.)).rounded_full().bg(danger))
                .child(
                    div()
                        .id(("bp", id.0 as usize))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .gap_2()
                        .py_0p5()
                        .cursor_pointer()
                        .child(div().flex_none().child(label))
                        .child(div().min_w_0().truncate().text_color(color).child(state))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DebugPanelEvent::OpenBreakpoint(file.clone(), line));
                        })),
                )
                .child(
                    Button::new(("remove-bp", id.0 as usize))
                        .ghost()
                        .xsmall()
                        .label("Remove")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DebugPanelEvent::RemoveBreakpoint(id));
                        })),
                )
        });
        section("Breakpoints", cx).child(
            div()
                .id("breakpoints")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .text_xs()
                .children(rows)
                .when(self.view.breakpoints.is_empty(), |d| {
                    d.px_2()
                        .text_color(muted)
                        .child("None. Press F9 in an editor to set one on the cursor's line.")
                }),
        )
    }
}

/// A titled column of the panel.
fn section(title: &'static str, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .px_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
}

impl EventEmitter<DebugPanelEvent> for DebugPanel {}
impl EventEmitter<PanelEvent> for DebugPanel {}

impl gpui_kit::component::dock::BasePanel for DebugPanel {
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn panel_name(&self) -> &'static str {
        "TethysDebug"
    }
}

impl Panel for DebugPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Debug".into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Debug"
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }
}

impl Focusable for DebugPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DebugPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bar = toolbar_state(&self.view.status);
        let color = match bar.tone {
            Tone::Muted => theme.muted_foreground,
            Tone::Normal => theme.foreground,
            Tone::Primary => theme.primary,
            Tone::Danger => theme.danger,
        };

        let toolbar = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(div().truncate().text_color(color).child(bar.text.clone()))
                    .children(self.view.process.clone().map(|p| {
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(p)
                    })),
            )
            .child(
                Button::new("continue")
                    .xsmall()
                    .primary()
                    .label("Continue")
                    .tooltip("Continue (F5)")
                    .disabled(!bar.continue_enabled)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Continue))),
            )
            .child(
                Button::new("pause")
                    .xsmall()
                    .outline()
                    .label("Pause")
                    .disabled(!bar.pause_enabled)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Pause))),
            )
            .child(if bar.show_detach {
                Button::new("detach")
                    .xsmall()
                    .outline()
                    .label("Detach")
                    .tooltip("Detach, leaving the editor running (Shift+F5)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Detach)))
            } else {
                Button::new("attach")
                    .xsmall()
                    .outline()
                    .label("Debug")
                    .tooltip(
                        "Attach to this project's editor, or launch it under the debugger (F5)",
                    )
                    .disabled(!bar.attach_enabled)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Attach)))
            });

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
                    .child(toolbar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .gap_2()
                            .py_1()
                            .child(self.render_frames(cx))
                            .child(div().w_px().h_full().bg(theme.border))
                            .child(self.render_breakpoints(cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tethys_core::debug::BreakpointState;

    use super::{DebugStatus, Tone, breakpoint_state_label, file_line_label, toolbar_state};

    #[test]
    fn detached_offers_only_debug() {
        let bar = toolbar_state(&DebugStatus::Detached);
        assert_eq!(bar.text, "Not attached");
        assert_eq!(bar.tone, Tone::Muted);
        assert!(!bar.continue_enabled);
        assert!(!bar.pause_enabled);
        assert!(!bar.show_detach);
        assert!(bar.attach_enabled);
    }

    #[test]
    fn attaching_disables_debug_until_it_is_done() {
        let bar = toolbar_state(&DebugStatus::Attaching);
        assert_eq!(bar.text, "Starting the debugger…");
        assert!(!bar.attach_enabled);
        assert!(!bar.show_detach);
        assert!(!bar.continue_enabled && !bar.pause_enabled);
    }

    #[test]
    fn running_can_pause_or_detach() {
        let bar = toolbar_state(&DebugStatus::Running);
        assert_eq!((bar.text.as_str(), bar.tone), ("Running", Tone::Normal));
        assert!(bar.pause_enabled);
        assert!(!bar.continue_enabled);
        assert!(bar.show_detach);
    }

    #[test]
    fn stopped_can_continue_or_detach() {
        let bar = toolbar_state(&DebugStatus::Stopped("Breakpoint at A.cpp:3".into()));
        assert_eq!(bar.text, "Stopped: Breakpoint at A.cpp:3");
        assert_eq!(bar.tone, Tone::Primary);
        assert!(bar.continue_enabled);
        assert!(!bar.pause_enabled);
        assert!(bar.show_detach);
    }

    #[test]
    fn breakpoint_states_read_as_words_and_failures_as_their_error() {
        assert_eq!(
            breakpoint_state_label(&BreakpointState::Unset),
            ("not attached".into(), Tone::Muted)
        );
        assert_eq!(
            breakpoint_state_label(&BreakpointState::Bound),
            ("set".into(), Tone::Normal)
        );
        assert_eq!(
            breakpoint_state_label(&BreakpointState::Pending),
            ("waiting for its module".into(), Tone::Muted)
        );
        assert_eq!(
            breakpoint_state_label(&BreakpointState::Failed("no symbols".into())),
            ("no symbols".into(), Tone::Danger)
        );
    }

    #[test]
    fn file_line_labels_use_the_file_name_only() {
        assert_eq!(
            file_line_label(Path::new(r"D:\dev\Game\Source\A.cpp"), 42),
            "A.cpp:42"
        );
        assert_eq!(file_line_label(Path::new("B.h"), 1), "B.h:1");
        assert_eq!(file_line_label(Path::new(""), 7), ":7");
    }
}
