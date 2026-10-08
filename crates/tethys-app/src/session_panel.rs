//! A dockable panel for one terminal session: a Claude Code agent or a build.
//!
//! The panel owns the session, so closing its tab (or dropping it) stops the
//! process tree.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::*;
use tethys_adapters::agent_terminal::TerminalHandle;
use tethys_core::SessionId;
use tethys_core::ports::AgentSession;

use crate::terminal_view::TerminalView;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Agent,
    Build,
}

pub struct SessionPanel {
    id: SessionId,
    kind: SessionKind,
    /// Fallback label, e.g. "Claude Code 2" or "Build GameEditor Development".
    label: String,
    /// Title set by the agent through escape codes. Ignored for builds.
    title: String,
    exit_code: Option<i32>,
    /// Dropping this stops the process and its tree. `None` once removed.
    session: Option<Box<dyn AgentSession>>,
    terminal: Entity<TerminalView>,
}

impl SessionPanel {
    pub fn new(
        id: SessionId,
        kind: SessionKind,
        label: String,
        session: Box<dyn AgentSession>,
        handle: TerminalHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            id,
            kind,
            label,
            title: String::new(),
            exit_code: None,
            session: Some(session),
            terminal: cx.new(|cx| TerminalView::new(handle, cx)),
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn kind(&self) -> SessionKind {
        self.kind
    }

    pub fn is_running(&self) -> bool {
        self.session.is_some() && self.exit_code.is_none()
    }

    pub fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        self.title = title;
        cx.notify();
    }

    pub fn set_exited(&mut self, code: i32, cx: &mut Context<Self>) {
        self.exit_code = Some(code);
        cx.notify();
    }

    /// Stops the process tree. Idempotent.
    pub fn stop(&mut self) {
        self.session.take();
    }

    pub fn label(&self) -> String {
        if self.kind == SessionKind::Build {
            return match self.exit_code {
                None => format!("{} …", self.label),
                Some(0) => format!("{} ✓", self.label),
                Some(code) => format!("{} ✗ ({code})", self.label),
            };
        }
        let name = if self.title.trim().is_empty() {
            &self.label
        } else {
            &self.title
        };
        match self.exit_code {
            Some(code) => format!("{name} (exited {code})"),
            None => name.clone(),
        }
    }
}

impl gpui_kit::component::dock::BasePanel for SessionPanel {
    fn panel_name(&self) -> &'static str {
        "TethysSession"
    }

    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.stop();
    }
}

impl Panel for SessionPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.label().into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.label()
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }

    /// The terminal draws its own padding.
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for SessionPanel {}

impl Focusable for SessionPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle().clone()
    }
}

impl Render for SessionPanel {
    /// The terminal as a rounded, bordered card on the backdrop. The focused
    /// pane's border takes the accent color.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self
            .terminal
            .read(cx)
            .focus_handle()
            .contains_focused(window, cx);
        let border = if focused {
            theme.primary.opacity(0.8)
        } else {
            theme.border
        };
        div().size_full().bg(theme.background).p_1p5().child(
            div()
                .size_full()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(border)
                .overflow_hidden()
                .child(self.terminal.clone()),
        )
    }
}
