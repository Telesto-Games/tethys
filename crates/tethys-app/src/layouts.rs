//! Panel layouts in a window: the dock read as an [`Arrangement`], and an
//! arrangement put back without any panel leaving the dock on the way.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaState, DockPlacement, DockState, PanelId, PanelInfo,
    PanelState, register_panel,
};
use gpui_kit::*;
use tethys_core::layout::{Arrangement, Dock, Pane, Side, Slot, Tab, ToolPane};

/// The registry name a panel already in the window is written under, so a
/// load seats it again rather than building a new one.
const SEAT: &str = "tethys.seat";

/// The panels the running load seats again, by id.
struct Seats(HashMap<u64, Arc<dyn BasePanelView>>);

impl Global for Seats {}

pub fn init(cx: &mut App) {
    register_panel(cx, SEAT, |build, _, cx| {
        let id = match build.info() {
            PanelInfo::Panel(id) => id.as_u64(),
            _ => None,
        };
        // `write` names only panels it has a seat for, and sets the seats
        // for as long as the load runs.
        id.and_then(|id| cx.try_global::<Seats>()?.0.get(&id).cloned())
            .expect("a seated panel is in the window")
    });
}

fn placement(side: Side) -> DockPlacement {
    match side {
        Side::Left => DockPlacement::Left,
        Side::Right => DockPlacement::Right,
        Side::Bottom => DockPlacement::Bottom,
    }
}

/// The window's panels as they stand. `tool` says which are tool panes.
pub fn read(
    dock: &DockArea,
    tool: impl Fn(PanelId) -> Option<ToolPane>,
    cx: &App,
) -> Arrangement<PanelId> {
    // The dump has the sizes as drawn; the trees, in the same order, the ids.
    let state = dock.dump(cx);
    let tab = |id| tool(id).map_or(Tab::Content(id), Tab::Tool);
    let ids = |placement| {
        dock.layout(placement)
            .map(|tree| tree.panels().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
    };
    let docks = [
        (Side::Left, &state.left_dock),
        (Side::Right, &state.right_dock),
        (Side::Bottom, &state.bottom_dock),
    ];
    Arrangement {
        center: pane(&state.center, &mut ids(DockPlacement::Center), &tab),
        docks: docks
            .into_iter()
            .filter_map(|(side, dock)| {
                let dock = dock.as_ref()?;
                Some(Dock {
                    side,
                    size: dock.size().as_f32(),
                    open: dock.open(),
                    pane: pane(dock.panel(), &mut ids(placement(side)), &tab),
                })
            })
            .collect(),
    }
}

fn pane(
    state: &PanelState,
    ids: &mut impl Iterator<Item = PanelId>,
    tab: &impl Fn(PanelId) -> Tab<PanelId>,
) -> Option<Pane<Tab<PanelId>>> {
    match &state.info {
        PanelInfo::Stack { sizes, axis } => {
            let children: Vec<_> = state
                .children
                .iter()
                .enumerate()
                .filter_map(|(i, child)| {
                    Some(Slot {
                        size: sizes.get(i).map_or(0., |size| size.as_f32()),
                        pane: pane(child, ids, tab)?,
                    })
                })
                .collect();
            (!children.is_empty()).then_some(Pane::Split {
                vertical: *axis == 1,
                children,
            })
        }
        PanelInfo::Tabs { active_index } => {
            let tabs: Vec<_> = state
                .children
                .iter()
                .filter_map(|_| ids.next())
                .map(tab)
                .collect();
            (!tabs.is_empty()).then_some(Pane::Tabs {
                tabs,
                active: *active_index,
            })
        }
        PanelInfo::Panel(_) => ids.next().map(|id| Pane::Tabs {
            tabs: vec![tab(id)],
            active: 0,
        }),
    }
}

/// Puts `arrangement` into the dock in one load, seating its panels again
/// where it says. `tool` finds each tool pane's panel. A panel moving between
/// regions is never out of the dock, so nothing is closed or stopped.
pub fn write(
    dock: &mut DockArea,
    arrangement: Arrangement<PanelId>,
    tool: impl Fn(ToolPane) -> Option<PanelId>,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    let mut seats = HashMap::new();
    let mut seat = |tab: Tab<PanelId>| {
        let id = match tab {
            Tab::Tool(t) => tool(t)?,
            Tab::Content(id) => id,
        };
        let view = dock.panel(id)?.clone();
        seats.insert(id.as_u64(), view);
        Some(id)
    };
    let center = arrangement.center.and_then(|p| p.filter_map(&mut seat));
    let mut docks: HashMap<_, _> = arrangement
        .docks
        .into_iter()
        .map(|d| {
            let pane = d.pane.and_then(|p| p.filter_map(&mut seat));
            let side = d.side;
            let state = DockState::new(
                pane.map_or_else(empty, state_of),
                placement(side),
                px(d.size),
                d.open,
            );
            (side, state)
        })
        .collect();
    let state = DockAreaState {
        version: dock.version(),
        center: center.map_or_else(empty, state_of),
        left_dock: docks.remove(&Side::Left),
        right_dock: docks.remove(&Side::Right),
        bottom_dock: docks.remove(&Side::Bottom),
    };
    cx.set_global(Seats(seats));
    let loaded = dock.load(state, window, cx);
    cx.remove_global::<Seats>();
    if let Err(e) = loaded {
        eprintln!("tethys: can't apply the layout: {e}");
    }
}

/// A region with nothing in it.
fn empty() -> PanelState {
    PanelState {
        panel_name: String::new(),
        children: Vec::new(),
        info: PanelInfo::stack(Vec::new(), Axis::Horizontal),
    }
}

fn state_of(pane: Pane<PanelId>) -> PanelState {
    match pane {
        Pane::Split { vertical, children } => {
            let axis = if vertical {
                Axis::Vertical
            } else {
                Axis::Horizontal
            };
            let (sizes, children) = children
                .into_iter()
                .map(|slot| (px(slot.size), state_of(slot.pane)))
                .unzip();
            PanelState {
                panel_name: String::new(),
                children,
                info: PanelInfo::stack(sizes, axis),
            }
        }
        Pane::Tabs { tabs, active } => PanelState {
            panel_name: String::new(),
            children: tabs
                .into_iter()
                .map(|id| PanelState {
                    panel_name: SEAT.to_string(),
                    children: Vec::new(),
                    info: PanelInfo::panel(id.as_u64().into()),
                })
                .collect(),
            info: PanelInfo::tabs(active),
        },
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::dock::{
        BasePanel, DockArea, DockPlacement, Panel, PanelEvent, PanelId,
    };
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
        ParentElement, Render, SharedString, TestAppContext, Window, div, px, size,
    };
    use tethys_core::layout::{Dock, Pane, PanelLayout, Side, Slot, ToolPane};

    use crate::test_support::{self, open_window, temp_config};

    /// A tab that notes whether the dock let it go.
    struct Probe {
        name: &'static str,
        removed: bool,
        focus: FocusHandle,
    }

    impl BasePanel for Probe {
        fn panel_name(&self) -> &'static str {
            "Probe"
        }

        fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.removed = true;
        }
    }

    impl Panel for Probe {
        fn tab_name(&self, _: &App) -> Option<SharedString> {
            Some(self.name.into())
        }
    }

    impl EventEmitter<PanelEvent> for Probe {}

    impl Focusable for Probe {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus.clone()
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.name)
        }
    }

    fn probe(name: &'static str, cx: &mut App) -> Entity<Probe> {
        cx.new(|cx| Probe {
            name,
            removed: false,
            focus: cx.focus_handle(),
        })
    }

    fn id(panel: &Entity<Probe>) -> PanelId {
        PanelId::from(panel.entity_id())
    }

    fn tabs<T>(tabs: Vec<T>) -> Option<Pane<T>> {
        Pane::tabs(tabs)
    }

    #[gpui_kit::test]
    fn applying_a_layout_moves_panels_between_docks_without_closing_any(cx: &mut TestAppContext) {
        cx.update(|cx| {
            test_support::init(temp_config("layouts"), cx);
            super::init(cx);
        });
        let (window, dock) = open_window(cx, size(px(1200.), px(800.)), |window, cx| {
            cx.new(|cx| DockArea::new("layouts", None, window, cx))
        });
        let [files, build, tests, agent, terminal] = cx.update(|cx| {
            ["Files", "Build", "Tests", "Claude 1", "Terminal"].map(|name| probe(name, cx))
        });
        let tool = {
            let tools = [
                (id(&files), ToolPane::Files),
                (id(&build), ToolPane::Build),
                (id(&tests), ToolPane::Tests),
            ];
            move |panel| tools.iter().find(|(p, _)| *p == panel).map(|(_, t)| *t)
        };
        cx.update_window(window.into(), |_, window, cx| {
            dock.update(cx, |dock, cx| {
                let add = |dock: &mut DockArea,
                           panel: &Entity<Probe>,
                           at,
                           window: &mut Window,
                           cx: &mut Context<DockArea>| {
                    dock.add_panel(panel.clone(), at, Some(px(250.)), window, cx)
                };
                add(dock, &files, DockPlacement::Left, window, cx);
                add(dock, &terminal, DockPlacement::Left, window, cx);
                add(dock, &build, DockPlacement::Right, window, cx);
                add(dock, &agent, DockPlacement::Center, window, cx);
                add(dock, &tests, DockPlacement::Bottom, window, cx);
            });
            window.render_frame(cx);

            // Files to the bottom beside Tests, which shows; Build stays right.
            let layout = PanelLayout {
                name: "Bottom heavy".into(),
                center: vec![],
                docks: vec![
                    Dock {
                        side: Side::Left,
                        size: 200.,
                        open: false,
                        pane: None,
                    },
                    Dock {
                        side: Side::Bottom,
                        size: 300.,
                        open: true,
                        pane: Some(Pane::Split {
                            vertical: false,
                            children: vec![
                                Slot {
                                    size: 400.,
                                    pane: Pane::Tabs {
                                        tabs: vec![ToolPane::Files],
                                        active: 0,
                                    },
                                },
                                Slot {
                                    size: 0.,
                                    pane: Pane::Tabs {
                                        tabs: vec![ToolPane::Tests],
                                        active: 0,
                                    },
                                },
                            ],
                        }),
                    },
                ],
            };
            let arranged = layout.apply(super::read(dock.read(cx), tool, cx));
            let ids = [
                (ToolPane::Files, id(&files)),
                (ToolPane::Build, id(&build)),
                (ToolPane::Tests, id(&tests)),
            ];
            let seat = |t| ids.iter().find(|(tool, _)| *tool == t).map(|(_, id)| *id);
            dock.update(cx, |dock, cx| {
                super::write(dock, arranged, seat, window, cx)
            });
            window.render_frame(cx);

            for panel in [&files, &build, &tests, &agent, &terminal] {
                assert!(
                    !panel.read(cx).removed,
                    "{} was closed",
                    panel.read(cx).name
                );
            }
            let area = dock.read(cx);
            let panels = |at| {
                area.layout(at)
                    .map(|t| t.panels().collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            assert_eq!(panels(DockPlacement::Left), [id(&terminal)]);
            assert_eq!(panels(DockPlacement::Right), [id(&build)]);
            assert_eq!(panels(DockPlacement::Bottom), [id(&files), id(&tests)]);
            assert_eq!(panels(DockPlacement::Center), [id(&agent)]);
            assert!(!area.is_dock_open(DockPlacement::Left));
            assert_eq!(area.dock_size(DockPlacement::Bottom), Some(px(300.)));

            // And it reads back as the same layout.
            let captured = PanelLayout::capture("Bottom heavy", &super::read(area, tool, cx));
            let bottom = captured
                .docks
                .iter()
                .find(|d| d.side == Side::Bottom)
                .unwrap();
            assert_eq!(
                bottom
                    .pane
                    .as_ref()
                    .map(|p| p.iter().copied().collect::<Vec<_>>()),
                Some(vec![ToolPane::Files, ToolPane::Tests])
            );
            assert_eq!(
                captured
                    .docks
                    .iter()
                    .find(|d| d.side == Side::Right)
                    .unwrap()
                    .pane,
                tabs(vec![ToolPane::Build])
            );
        })
        .unwrap();
    }
}
