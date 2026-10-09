//! The dock's look: gpui-component's skin, except that tab groups in the main
//! area can list their tabs down the left side instead of along the top.
//!
//! The dock lays a group out as a column (tab bar, then content), so the side
//! list is drawn over the group's left edge and the content is moved over by
//! its width. Docks (Files, build, debug) keep their tabs along the top.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::ResizeHandleContext;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaRenderer, DockContext, DockPlacement, DockSkin, DragPanel,
    DropIndicator, NodeId, PaneNode, PaneRef, PanelHandle, PanelState, TabGroupContext,
    TabGroupRenderer,
};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::ports::ConfigStore;

use crate::workspace::Services;

/// Width of the side tab list.
const SIDE_TABS_WIDTH: Pixels = px(200.);
/// Height of one tab in the side list.
const SIDE_TAB_HEIGHT: Pixels = px(30.);

/// Whether the main area's tabs run down the side.
pub struct TabLayout {
    pub vertical: bool,
}

impl Global for TabLayout {}

/// Loads the saved tab layout.
pub fn init(cx: &mut App) {
    let vertical = cx
        .global::<Services>()
        .config
        .vertical_tabs()
        .unwrap_or(false);
    cx.set_global(TabLayout { vertical });
}

/// Switches every window's tab layout.
pub fn set_vertical(vertical: bool, cx: &mut App) {
    cx.set_global(TabLayout { vertical });
    cx.refresh_windows();
}

/// A [`DockArea`] wearing this skin, and the gpui-component skin inside it
/// that its settings are changed through.
pub fn dock_area(
    id: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<DockArea>, Rc<DockSkin>) {
    let mut skin = None;
    let area = cx.new(|cx| {
        let inner = DockSkin::new(cx);
        skin = Some(inner.clone());
        let this = Rc::new(Skin {
            inner,
            area: cx.weak_entity(),
        });
        DockArea::new(id, None, window, cx).with_renderer(this)
    });
    (area, skin.expect("the area's constructor ran"))
}

struct Skin {
    inner: Rc<DockSkin>,
    area: WeakEntity<DockArea>,
}

impl DockAreaRenderer for Skin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(window, cx)
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.split_frame(node, axis, window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.center_frame(window, cx)
    }

    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_split_handle(handle, window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_dock(dock, content, window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.inner.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(Tabs {
            inner: self.inner.tab_group_renderer(),
            area: self.area.clone(),
            scroll: ScrollHandle::new(),
            last_active_ix: Cell::new(None),
        })
    }
}

/// One group's tabs: along the top as gpui-component draws them, or down the
/// side.
struct Tabs {
    inner: Rc<dyn TabGroupRenderer>,
    area: WeakEntity<DockArea>,
    scroll: ScrollHandle,
    /// The tab the last frame showed, so a newly shown one is scrolled to.
    last_active_ix: Cell<Option<usize>>,
}

impl Tabs {
    /// Whether this group lists its tabs down the side: only in the main
    /// area, and not while collapsed (a collapsed group is just its tab strip).
    fn vertical(&self, group: &TabGroupContext, cx: &App) -> bool {
        cx.global::<TabLayout>().vertical
            && !group.is_collapsed()
            && self.area.upgrade().is_some_and(|area| {
                area.read(cx)
                    .layout(DockPlacement::Center)
                    .is_some_and(|tree| tree.find_node(group.node()).is_some())
            })
    }

    /// The button that collapses or expands the dock at `placement`, if this
    /// group is the one that carries it (the same groups gpui-component picks).
    fn dock_toggle(
        &self,
        placement: DockPlacement,
        group: &TabGroupContext,
        cx: &App,
    ) -> Option<Button> {
        if group.is_zoomed() {
            return None;
        }
        let handle = self.area.upgrade()?;
        let area = handle.read(cx);
        if !area.is_dock_collapsible(placement) {
            return None;
        }
        let root = area.layout(DockPlacement::Center)?.root();
        let designated = match placement {
            DockPlacement::Left => left_top_group(root),
            DockPlacement::Right => right_top_group(root),
            _ => None,
        };
        if designated != Some(group.node()) {
            return None;
        }
        let open = area.is_dock_open(placement);
        let icon = match (placement, open) {
            (DockPlacement::Left, true) => IconName::PanelLeft,
            (DockPlacement::Left, false) => IconName::PanelLeftOpen,
            (_, true) => IconName::PanelRight,
            (_, false) => IconName::PanelRightOpen,
        };
        Some(
            Button::new(SharedString::from(format!("side-tabs-dock:{placement:?}")))
                .icon(icon)
                .xsmall()
                .ghost()
                .tab_stop(false)
                .tooltip(if open { "Collapse" } else { "Expand" })
                .on_click(move |_, window, cx| {
                    handle.update(cx, |area, cx| area.toggle_dock(placement, window, cx));
                }),
        )
    }

    fn render_side(&self, group: &TabGroupContext, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        let (tab_bar, tab, tab_active, foreground, active_foreground, border, drag_border, drop) = (
            theme.tab_bar,
            theme.tab,
            theme.tab_active,
            theme.tab_foreground,
            theme.tab_active_foreground,
            theme.border,
            theme.drag_border,
            theme.tokens.drop_target,
        );

        let visible: Vec<usize> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect();
        let active_ix = group.active_ix();
        let shown = group.active_panel().map(|panel| panel.panel_id(cx));
        if self.last_active_ix.replace(Some(active_ix)) != Some(active_ix)
            && let Some(pos) = visible.iter().position(|ix| *ix == active_ix)
        {
            self.scroll.scroll_to_item(pos);
        }
        let droppable = group.is_droppable();

        let left = self.dock_toggle(DockPlacement::Left, group, cx);
        let right = self.dock_toggle(DockPlacement::Right, group, cx);
        let header = (left.is_some() || right.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .flex_none()
                .h(SIDE_TAB_HEIGHT)
                .px_2()
                .border_b_1()
                .border_color(border)
                .child(div().children(left))
                .child(div().children(right))
        });

        let tabs = visible.into_iter().map(|ix| {
            let panel = group.panels()[ix].clone();
            let id = panel.panel_id(cx);
            let selected = Some(id) == shown;
            let name: SharedString = PanelHandle::of(&panel)
                .and_then(|handle| handle.tab_name(cx))
                .unwrap_or_else(|| panel.panel_name(cx).into());
            let drag = group
                .is_draggable()
                .then(|| group.drag_panel(ix, cx))
                .flatten();
            let closable = group.is_panel_closable(id, cx);

            div()
                .id(("side-tab", ix))
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .h(SIDE_TAB_HEIGHT)
                .pl_3()
                .pr_1()
                .text_sm()
                .border_l_2()
                .map(|this| match selected {
                    true => this
                        .bg(tab_active)
                        .text_color(active_foreground)
                        .border_color(active_foreground),
                    false => this
                        .bg(tab)
                        .text_color(foreground)
                        .border_color(transparent_black())
                        .hover(|this| this.text_color(active_foreground.opacity(0.75))),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name.clone()),
                )
                .when(closable, |this| {
                    this.child(
                        Button::new(("side-tab-close", ix))
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .tab_stop(false)
                            .on_click({
                                let group = group.clone();
                                move |_, window, cx| {
                                    cx.stop_propagation();
                                    group.close(id, window, cx);
                                }
                            }),
                    )
                })
                .on_click({
                    let group = group.clone();
                    move |_, window, cx| group.select_tab(ix, window, cx)
                })
                .when_some(drag, |this, drag| {
                    this.on_drag(drag, move |drag: &DragPanel, offset, _, cx| {
                        cx.stop_propagation();
                        drag.set_drag_offset(offset);
                        drag.set_preview_size(size(SIDE_TABS_WIDTH, SIDE_TAB_HEIGHT));
                        cx.new(|_| DragPreview { name: name.clone() })
                    })
                })
                .when(droppable, |this| {
                    this.drag_over::<DragPanel>(move |this, _, _, _| {
                        this.border_t_2().border_color(drag_border)
                    })
                    .on_drop({
                        let group = group.clone();
                        move |drag: &DragPanel, window, cx| {
                            group.drop_panel(drag.clone(), Some(ix), true, window, cx);
                        }
                    })
                })
        });

        // Room below the last tab to drop a panel at the end.
        let tabs_count = group.panels().len();
        let end = div()
            .id("side-tabs-end")
            .flex_1()
            .min_h(SIDE_TAB_HEIGHT)
            .when(droppable, |this| {
                let group = group.clone();
                let node = group.node();
                this.drag_over::<DragPanel>(move |this, _, _, _| this.bg(drop))
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        let ix = (drag.source() == node).then(|| tabs_count - 1);
                        group.drop_panel(drag.clone(), ix, false, window, cx);
                    })
            });

        div()
            .absolute()
            .top_0()
            .left_0()
            .bottom_0()
            .w(SIDE_TABS_WIDTH)
            .flex()
            .flex_col()
            .bg(tab_bar)
            .border_r_1()
            .border_color(border)
            .children(header)
            .child(
                div()
                    .id("side-tabs")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(tabs)
                    .child(end),
            )
            .into_any_element()
    }
}

impl TabGroupRenderer for Tabs {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let vertical = self.vertical(group, cx);
        self.inner
            .frame(group, window, cx)
            .when(vertical, |this| this.relative())
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        let vertical = self.vertical(group, cx);
        self.inner
            .content_frame(group, window, cx)
            .when(vertical, |this| this.ml(SIDE_TABS_WIDTH))
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        if self.vertical(group, cx) {
            self.render_side(group, cx)
        } else {
            self.inner.render_tab_bar(group, window, cx)
        }
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_active_panel(panel, group, window, cx)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}

/// What follows the pointer while a side tab is dragged.
struct DragPreview {
    name: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_sm()
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().tab_active_foreground)
            .border_1()
            .border_color(cx.theme().drag_border)
            .child(self.name.clone())
    }
}

/// The left-most, top-most group, which carries the left dock's toggle.
fn left_top_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.first().and_then(left_top_group),
    }
}

/// The right-most, top-most group, which carries the right dock's toggle.
fn right_top_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { axis, children, .. } => match axis {
            Axis::Vertical => children.first(),
            Axis::Horizontal => children.last(),
        }
        .and_then(right_top_group),
    }
}
