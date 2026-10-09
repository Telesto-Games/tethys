//! The debugger pane: status, controls, the stopped thread's stack and the
//! breakpoint list. It only draws what the workspace hands it and reports
//! clicks back; the workspace owns the debug session.

use std::path::PathBuf;

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
        let rows = self.view.breakpoints.iter().map(|b| {
            let (state, color) = match &b.state {
                BreakpointState::Unset => ("not attached".to_string(), muted),
                BreakpointState::Bound => ("set".to_string(), theme.foreground),
                BreakpointState::Pending => ("waiting for its module".to_string(), muted),
                BreakpointState::Failed(e) => (e.clone(), danger),
            };
            let name = b
                .file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
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
                        .child(div().flex_none().child(format!("{name}:{line}")))
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
        let status = &self.view.status;
        let (text, color) = match status {
            DebugStatus::Detached => ("Not attached".to_string(), theme.muted_foreground),
            DebugStatus::Attaching => {
                ("Starting the debugger…".to_string(), theme.muted_foreground)
            }
            DebugStatus::Running => ("Running".to_string(), theme.foreground),
            DebugStatus::Stopped(reason) => (format!("Stopped: {reason}"), theme.primary),
        };
        let attached = matches!(status, DebugStatus::Running | DebugStatus::Stopped(_));
        let stopped = matches!(status, DebugStatus::Stopped(_));

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
                    .child(div().truncate().text_color(color).child(text))
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
                    .disabled(!stopped)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Continue))),
            )
            .child(
                Button::new("pause")
                    .xsmall()
                    .outline()
                    .label("Pause")
                    .disabled(*status != DebugStatus::Running)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugPanelEvent::Pause))),
            )
            .child(if attached {
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
                    .disabled(*status == DebugStatus::Attaching)
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
