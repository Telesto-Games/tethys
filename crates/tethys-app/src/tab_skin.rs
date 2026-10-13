//! The dock's look: gpui-component's skin, except that tab groups in the main
//! area can list their tabs down the left side instead of along the top.
//!
//! The dock lays a group out as a column (tab bar, then content), so the side
//! list is drawn over the group's left edge and the content is moved over by
//! its width. Docks (Files, build, debug) keep their tabs along the top.
//!
//! Every tab can be dragged, including the last one in its area: the dock
//! would hold that one in place so no dock is left empty, but the workspace
//! collapses an emptied dock instead. An empty region takes a dropped tab.
//!
//! Each dock's collapse button ends its own top tab bar, and a collapsed side
//! dock leaves a strip that opens it again: the side docks get the pattern
//! gpui-component gives the bottom dock, whose collapsed tab strip stays.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::{Placement, ResizeHandleContext};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaRenderer, DockContext, DockEvent, DockPlacement, DockSkin,
    DragPanel, DropIndicator, InsertTarget, NodeId, PaneNode, PaneRef, PanelHandle, PanelState,
    TabGroupContext, TabGroupRenderer,
};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::ports::ConfigStore;

use crate::session_panel::SESSION_PANEL_NAME;
use crate::workspace::Services;

/// Width of the side tab list.
const SIDE_TABS_WIDTH: Pixels = px(200.);
/// Height of one tab in the side list.
const SIDE_TAB_HEIGHT: Pixels = px(30.);
/// Width of the strip a collapsed side dock leaves.
const COLLAPSED_STRIP_WIDTH: Pixels = px(28.);

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
        // Kept up to date from outside rendering: the skin's hooks run while
        // the area is drawing itself, when it cannot be read.
        let snapshot = Rc::new(Cell::new(Snapshot::default()));
        cx.observe_self({
            let snapshot = snapshot.clone();
            move |area, cx| snapshot.set(Snapshot::of(area, cx))
        })
        .detach();
        cx.subscribe_self({
            let snapshot = snapshot.clone();
            move |area, _: &DockEvent, cx| snapshot.set(Snapshot::of(area, cx))
        })
        .detach();
        let this = Rc::new(Skin {
            inner,
            area: cx.weak_entity(),
            snapshot: snapshot.clone(),
        });
        let area = DockArea::new(id, None, window, cx).with_renderer(this);
        snapshot.set(Snapshot::of(&area, cx));
        area
    });
    let skin = skin.expect("the area's constructor ran");
    // Each dock carries its own collapse button (see `Tabs::dock_toggle`),
    // not gpui-component's, which puts the side docks' on the main area.
    // Set once the area exists: the setting notifies it.
    skin.set_toggle_button_visible(false, cx);
    (area, skin)
}

/// The main area, then each dock.
const PLACEMENTS: [DockPlacement; 4] = [
    DockPlacement::Center,
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
];

/// What the area-level hooks need to know about the area.
#[derive(Clone, Copy, Default)]
struct Snapshot {
    /// Per [`PLACEMENTS`], the root of a region that shows nothing.
    empty_roots: [Option<NodeId>; 4],
    left_collapsed: bool,
    right_collapsed: bool,
}

impl Snapshot {
    fn of(area: &DockArea, cx: &App) -> Self {
        let collapsed = |placement| area.has_dock(placement) && !area.is_dock_open(placement);
        Self {
            empty_roots: PLACEMENTS.map(|placement| {
                area.is_empty(placement, cx)
                    .then(|| area.layout(placement))
                    .flatten()
                    .map(|tree| tree.root().id())
            }),
            left_collapsed: collapsed(DockPlacement::Left),
            right_collapsed: collapsed(DockPlacement::Right),
        }
    }

    /// The region `node` is the root of, if that region shows nothing.
    fn empty_region(&self, node: NodeId) -> Option<DockPlacement> {
        PLACEMENTS
            .into_iter()
            .zip(self.empty_roots)
            .find_map(|(placement, root)| (root == Some(node)).then_some(placement))
    }
}

/// Moves a dropped panel into a region: into its first tab group, or a new
/// one when it has none. A collapsed dock opens to show it.
fn drop_into(
    area: &WeakEntity<DockArea>,
    placement: DockPlacement,
    drag: &DragPanel,
    window: &mut Window,
    cx: &mut App,
) {
    let _ = area.update(cx, |area, cx| {
        let Some(root) = area.layout(placement).map(|tree| tree.root()) else {
            return;
        };
        let target = match first_group(root) {
            Some(node) => InsertTarget::Tabs {
                node,
                ix: None,
                activate: true,
            },
            None => InsertTarget::Split {
                node: root.id(),
                placement: Placement::Right,
                size: None,
            },
        };
        area.move_panel(drag.panel(), target, window, cx);
        if placement != DockPlacement::Center && !area.is_dock_open(placement) {
            area.toggle_dock(placement, window, cx);
        }
    });
}

fn first_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.iter().find_map(first_group),
    }
}

struct Skin {
    inner: Rc<DockSkin>,
    area: WeakEntity<DockArea>,
    snapshot: Rc<Cell<Snapshot>>,
}

impl Skin {
    /// The strip a collapsed side dock leaves at its edge of the main area;
    /// clicking it opens the dock again, as a collapsed bottom dock's tab
    /// strip does, and a tab dropped on it moves into the dock.
    fn collapsed_strip(&self, placement: DockPlacement, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let drop = theme.tokens.drop_target;
        let drop_area = self.area.clone();
        let (icon, tooltip_id) = match placement {
            DockPlacement::Left => (IconName::PanelLeftOpen, "collapsed-dock-left"),
            _ => (IconName::PanelRightOpen, "collapsed-dock-right"),
        };
        let area = self.area.clone();
        div()
            .id(tooltip_id)
            .absolute()
            .top_0()
            .bottom_0()
            .w(COLLAPSED_STRIP_WIDTH)
            .flex()
            .flex_col()
            .items_center()
            .pt_2()
            .bg(theme.tab_bar)
            .border_color(theme.border)
            .map(|this| match placement {
                DockPlacement::Left => this.left_0().border_r_1(),
                _ => this.right_0().border_l_1(),
            })
            .text_color(theme.tab_foreground)
            .hover(|this| this.text_color(theme.tab_active_foreground))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Expand").build(window, cx))
            .on_click(move |_, window, cx| {
                let _ = area.update(cx, |area, cx| area.toggle_dock(placement, window, cx));
            })
            .drag_over::<DragPanel>(move |this, _, _, _| this.bg(drop))
            .on_drop(move |drag: &DragPanel, window, cx| {
                drop_into(&drop_area, placement, drag, window, cx)
            })
            .child(Icon::new(icon).small())
    }
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
        let frame = self.inner.split_frame(node, axis, window, cx);
        // An empty region (the main area, or a dock whose tabs were all
        // dragged away) has no tab group to drop on, so its root takes the
        // drop and starts one.
        let Some(placement) = self.snapshot.get().empty_region(node) else {
            return frame;
        };
        let area = self.area.clone();
        let drop = cx.theme().tokens.drop_target;
        frame
            .drag_over::<DragPanel>(move |this, _, _, _| this.bg(drop))
            .on_drop(move |drag: &DragPanel, window, cx| {
                drop_into(&area, placement, drag, window, cx)
            })
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let snapshot = self.snapshot.get();
        let left = snapshot
            .left_collapsed
            .then(|| self.collapsed_strip(DockPlacement::Left, cx));
        let right = snapshot
            .right_collapsed
            .then(|| self.collapsed_strip(DockPlacement::Right, cx));
        self.inner
            .center_frame(window, cx)
            .relative()
            .when(left.is_some(), |this| this.pl(COLLAPSED_STRIP_WIDTH))
            .when(right.is_some(), |this| this.pr(COLLAPSED_STRIP_WIDTH))
            .children(left)
            .children(right)
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

    /// The button that collapses or expands the dock this group is in. Every
    /// group in a dock carries it, so a split dock can be collapsed from any.
    fn dock_toggle(&self, group: &TabGroupContext, cx: &App) -> Option<Button> {
        if group.is_zoomed() {
            return None;
        }
        let handle = self.area.upgrade()?;
        let area = handle.read(cx);
        let placement = [
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ]
        .into_iter()
        .find(|placement| {
            area.layout(*placement)
                .is_some_and(|tree| tree.find_node(group.node()).is_some())
        })?;
        if !area.is_dock_collapsible(placement) {
            return None;
        }
        let open = area.is_dock_open(placement);
        let (icon, tooltip) = dock_toggle_icon(placement, open);
        Some(
            Button::new(SharedString::from(format!("dock-toggle:{placement:?}")))
                .icon(icon)
                .xsmall()
                .ghost()
                .tab_stop(false)
                .tooltip(tooltip)
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

        let rows = side_rows(group.panels().iter().map(|panel| {
            (
                panel.visible(cx),
                panel.panel_name(cx) == SESSION_PANEL_NAME,
            )
        }));
        let active_ix = group.active_ix();
        let shown = group.active_panel().map(|panel| panel.panel_id(cx));
        let last = self.last_active_ix.replace(Some(active_ix));
        if let Some(pos) = scroll_target(last, active_ix, &rows) {
            self.scroll.scroll_to_item(pos);
        }
        let droppable = group.is_droppable();

        let tabs = rows.into_iter().map(|row| {
            let Some(ix) = row else {
                return div()
                    .flex_none()
                    .h_px()
                    .mx_2()
                    .my_1()
                    .bg(border)
                    .into_any_element();
            };
            let panel = group.panels()[ix].clone();
            let id = panel.panel_id(cx);
            let selected = Some(id) == shown;
            let name = tab_name(&panel, cx);
            let drag = tab_drag(group, ix, cx);
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
                .into_any_element()
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
                    .child(end)
                    .context_menu(tab_menu),
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
            return self.render_side(group, cx);
        }
        let bar = self.inner.render_tab_bar(group, window, cx);
        // gpui-component only drags tabs the dock calls draggable, which an
        // area's last panel is not, so that one is dragged by its tab bar. A
        // tab stops the mouse press from reaching anything around it, so the
        // drag is started by a layer over the bar. The layer does not block
        // the mouse, so the tab and its buttons still get their clicks.
        let lone = (!group.is_draggable())
            .then(|| tab_drag(group, group.active_ix(), cx))
            .flatten();
        let drag_layer = match (lone, group.active_panel()) {
            (Some(drag), Some(panel)) => {
                let name = tab_name(panel, cx);
                Some(div().id("lone-tab-drag").absolute().inset_0().on_drag(
                    drag,
                    move |drag: &DragPanel, offset, _, cx| {
                        cx.stop_propagation();
                        drag.set_drag_offset(offset);
                        drag.set_preview_size(size(SIDE_TABS_WIDTH, SIDE_TAB_HEIGHT));
                        cx.new(|_| DragPreview { name: name.clone() })
                    },
                ))
            }
            _ => None,
        };
        let toggle = self.dock_toggle(group, cx).map(|button| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .px_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().tab_bar)
                .child(button)
        });
        div()
            .id("tab-bar")
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .child(bar)
                    .children(drag_layer),
            )
            .children(toggle)
            .context_menu(tab_menu)
            .into_any_element()
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

/// The menu a right-click on the tabs opens.
fn tab_menu(menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let vertical = cx.global::<TabLayout>().vertical;
    menu.item(
        PopupMenuItem::new("Horizontal Tabs")
            .checked(!vertical)
            .on_click(|_, _, cx| choose_vertical(false, cx)),
    )
    .item(
        PopupMenuItem::new("Vertical Tabs")
            .checked(vertical)
            .on_click(|_, _, cx| choose_vertical(true, cx)),
    )
}

/// Switches the tab layout and saves it, as the Settings dialog does.
fn choose_vertical(vertical: bool, cx: &mut App) {
    if let Err(e) = cx.global::<Services>().config.set_vertical_tabs(vertical) {
        eprintln!("tethys: can't save the tab layout: {e}");
    }
    set_vertical(vertical, cx);
}

/// The drag payload for the tab at `ix`: any tab in a group that is not
/// locked or zoomed, even the last one in its area.
fn tab_drag(group: &TabGroupContext, ix: usize, cx: &App) -> Option<DragPanel> {
    (!group.is_locked())
        .then(|| group.drag_panel(ix, cx))
        .flatten()
}

fn tab_name(panel: &Arc<dyn BasePanelView>, cx: &App) -> SharedString {
    PanelHandle::of(panel)
        .and_then(|handle| handle.tab_name(cx))
        .unwrap_or_else(|| panel.panel_name(cx).into())
}

/// The side list's rows for a group's panels, given each panel's
/// `(visible, is_session)`: sessions first, then files, each in tab order,
/// with a divider (`None`) between the two when both are present. Hidden
/// panels get no row.
fn side_rows(panels: impl Iterator<Item = (bool, bool)>) -> Vec<Option<usize>> {
    let (mut sessions, mut files) = (Vec::new(), Vec::new());
    for (ix, (visible, is_session)) in panels.enumerate() {
        if !visible {
            continue;
        }
        if is_session {
            sessions.push(ix);
        } else {
            files.push(ix);
        }
    }
    let divided = !sessions.is_empty() && !files.is_empty();
    sessions
        .into_iter()
        .map(Some)
        .chain(divided.then_some(None))
        .chain(files.into_iter().map(Some))
        .collect()
}

/// The row to scroll into view when the active tab changed: its position in
/// `rows`, if it has one. `last` is the previously active tab.
fn scroll_target(last: Option<usize>, active: usize, rows: &[Option<usize>]) -> Option<usize> {
    if last == Some(active) {
        return None;
    }
    rows.iter().position(|row| *row == Some(active))
}

/// A dock's collapse button: the icon for its side and state, and the tooltip.
fn dock_toggle_icon(placement: DockPlacement, open: bool) -> (IconName, &'static str) {
    let icon = match (placement, open) {
        (DockPlacement::Left, true) => IconName::PanelLeft,
        (DockPlacement::Left, false) => IconName::PanelLeftOpen,
        (DockPlacement::Right, true) => IconName::PanelRight,
        (DockPlacement::Right, false) => IconName::PanelRightOpen,
        (_, true) => IconName::PanelBottom,
        (_, false) => IconName::PanelBottomOpen,
    };
    (icon, if open { "Collapse" } else { "Expand" })
}

/// What follows the pointer while a tab is dragged by this skin.
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

#[cfg(test)]
mod tests {
    use gpui_kit::component::IconName;
    use gpui_kit::component::dock::DockPlacement;

    use super::{dock_toggle_icon, scroll_target, side_rows};

    #[test]
    fn side_rows_put_sessions_first_with_a_divider() {
        // (visible, is_session), in tab order.
        let panels = [
            (true, false),
            (true, true),
            (false, true),
            (true, false),
            (true, true),
        ];
        assert_eq!(
            side_rows(panels.into_iter()),
            [Some(1), Some(4), None, Some(0), Some(3)]
        );
    }

    #[test]
    fn side_rows_omit_the_divider_when_one_group_is_empty() {
        assert_eq!(
            side_rows([(true, true), (true, true)].into_iter()),
            [Some(0), Some(1)]
        );
        assert_eq!(
            side_rows([(true, false), (false, true)].into_iter()),
            [Some(0)]
        );
        assert!(side_rows(std::iter::empty()).is_empty());
        assert!(side_rows([(false, true), (false, false)].into_iter()).is_empty());
    }

    #[test]
    fn scrolls_to_the_active_row_only_when_it_changes() {
        let rows = [Some(2), None, Some(0), Some(1)];
        assert_eq!(scroll_target(None, 1, &rows), Some(3));
        assert_eq!(scroll_target(Some(0), 2, &rows), Some(0));
        assert_eq!(scroll_target(Some(1), 1, &rows), None);
        // A hidden active tab has no row.
        assert_eq!(scroll_target(None, 7, &rows), None);
    }

    #[test]
    fn dock_toggles_show_their_side_and_state() {
        let toggle = |placement, open| {
            let (icon, tooltip) = dock_toggle_icon(placement, open);
            (icon, tooltip)
        };
        assert!(matches!(
            toggle(DockPlacement::Left, true),
            (IconName::PanelLeft, "Collapse")
        ));
        assert!(matches!(
            toggle(DockPlacement::Left, false),
            (IconName::PanelLeftOpen, "Expand")
        ));
        assert!(matches!(
            toggle(DockPlacement::Right, true),
            (IconName::PanelRight, "Collapse")
        ));
        assert!(matches!(
            toggle(DockPlacement::Right, false),
            (IconName::PanelRightOpen, "Expand")
        ));
        assert!(matches!(
            toggle(DockPlacement::Bottom, true),
            (IconName::PanelBottom, "Collapse")
        ));
        assert!(matches!(
            toggle(DockPlacement::Bottom, false),
            (IconName::PanelBottomOpen, "Expand")
        ));
        assert!(matches!(
            toggle(DockPlacement::Center, false),
            (IconName::PanelBottomOpen, "Expand")
        ));
    }
}
