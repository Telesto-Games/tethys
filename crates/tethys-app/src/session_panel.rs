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
    /// A plain shell, always labelled "Terminal".
    Terminal,
    Build,
}

pub struct SessionPanel {
    id: SessionId,
    kind: SessionKind,
    /// Fallback label, e.g. "Claude Code 2" or "Build GameEditor Development".
    label: String,
    /// Title set by the agent through escape codes. Ignored for builds and
    /// plain terminals, whose shell sets it to the current folder.
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
        session_label(self.kind, &self.label, &self.title, self.exit_code)
    }
}

/// A session's tab label. Builds show progress and their result; agents use
/// the title they set unless it's blank; terminals keep their label. A
/// finished session says so.
fn session_label(kind: SessionKind, label: &str, title: &str, exit_code: Option<i32>) -> String {
    if kind == SessionKind::Build {
        return match exit_code {
            None => format!("{label} …"),
            Some(0) => format!("{label} ✓"),
            Some(code) => format!("{label} ✗ ({code})"),
        };
    }
    let name = if kind == SessionKind::Terminal || title.trim().is_empty() {
        label
    } else {
        title
    };
    match exit_code {
        Some(code) => format!("{name} (exited {code})"),
        None => name.to_string(),
    }
}

/// `panel_name` of every session panel, so the dock can tell them from files.
pub const SESSION_PANEL_NAME: &str = "TethysSession";

impl gpui_kit::component::dock::BasePanel for SessionPanel {
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn panel_name(&self) -> &'static str {
        SESSION_PANEL_NAME
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

#[cfg(test)]
mod tests {
    use super::{SessionKind, session_label};

    #[test]
    fn build_labels_show_progress_and_result() {
        let build = |code| session_label(SessionKind::Build, "Build GameEditor", "ignored", code);
        assert_eq!(build(None), "Build GameEditor …");
        assert_eq!(build(Some(0)), "Build GameEditor ✓");
        assert_eq!(build(Some(6)), "Build GameEditor ✗ (6)");
    }

    #[test]
    fn agents_use_their_title_unless_blank() {
        let agent = |title, code| session_label(SessionKind::Agent, "Claude Code 2", title, code);
        assert_eq!(agent("", None), "Claude Code 2");
        assert_eq!(agent("  \t", None), "Claude Code 2");
        assert_eq!(agent("✳ fixing tests", None), "✳ fixing tests");
        assert_eq!(
            agent("✳ fixing tests", Some(0)),
            "✳ fixing tests (exited 0)"
        );
        assert_eq!(agent("", Some(1)), "Claude Code 2 (exited 1)");
    }

    #[test]
    fn terminals_ignore_the_shell_title() {
        assert_eq!(
            session_label(SessionKind::Terminal, "Terminal", "D:\\dev\\repos", None),
            "Terminal"
        );
        assert_eq!(
            session_label(SessionKind::Terminal, "Terminal", "D:\\dev", Some(-1)),
            "Terminal (exited -1)"
        );
    }
}
