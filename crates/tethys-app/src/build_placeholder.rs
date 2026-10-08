//! Holds the build pane open before the first build, and after the last build tab closes.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::*;

pub struct BuildPlaceholder {
    focus: FocusHandle,
}

impl BuildPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl gpui_kit::component::dock::BasePanel for BuildPlaceholder {
    fn panel_name(&self) -> &'static str {
        "TethysBuildPlaceholder"
    }

    /// The build pane is always there; only build tabs close.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for BuildPlaceholder {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Build".into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Build"
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }
}

impl EventEmitter<PanelEvent> for BuildPlaceholder {}

impl Focusable for BuildPlaceholder {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BuildPlaceholder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .p_1p5()
            .child(
                div()
                    .size_full()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_color(theme.muted_foreground)
                    .child("No build yet")
                    .child(
                        div()
                            .text_xs()
                            .child("Build (Ctrl+Shift+B) shows its log here."),
                    ),
            )
    }
}
