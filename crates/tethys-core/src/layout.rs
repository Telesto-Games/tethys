//! Panel layouts: where the tool panes sit and how big the docks are, saved
//! by name. A layout doesn't hold sessions or files: applying one places the
//! tool panes it names and leaves every other tab in the region it's in.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest a layout name may be, so it fits a menu.
const MAX_NAME_LEN: usize = 40;

/// A pane a layout places. Everything else in a window (sessions, editors,
/// diffs, histories) is content, which layouts leave where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPane {
    Files,
    /// The build log, or the placeholder standing in for it.
    Build,
    Tests,
    Debug,
}

impl ToolPane {
    pub const ALL: [ToolPane; 4] = [
        ToolPane::Files,
        ToolPane::Build,
        ToolPane::Tests,
        ToolPane::Debug,
    ];
}

/// A dock beside or below the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 3] = [Side::Left, Side::Right, Side::Bottom];
}

/// A tab in a window: a tool pane, or content `C` a layout doesn't place.
#[derive(Debug, Clone, PartialEq)]
pub enum Tab<C> {
    Tool(ToolPane),
    Content(C),
}

/// A region's contents: tab groups, split side by side or stacked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub enum Pane<T> {
    /// Children side by side, or stacked when `vertical`.
    Split {
        #[serde(default)]
        vertical: bool,
        children: Vec<Slot<T>>,
    },
    /// Tabs shown one at a time; `active` is the one shown.
    Tabs {
        tabs: Vec<T>,
        #[serde(default)]
        active: usize,
    },
}

/// One child of a split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Slot<T> {
    /// Pixels along the split; 0 takes what's left.
    #[serde(default)]
    pub size: f32,
    pub pane: Pane<T>,
}

/// A dock: its size across, whether it's open, and what's in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Dock<T> {
    pub side: Side,
    pub size: f32,
    pub open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<Pane<T>>,
}

/// A window's panels as they stand: the main area and the docks.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement<C> {
    pub center: Option<Pane<Tab<C>>>,
    pub docks: Vec<Dock<Tab<C>>>,
}

impl<T> Pane<T> {
    /// A tab group of `tabs`, or nothing when there are none.
    pub fn tabs(tabs: Vec<T>) -> Option<Self> {
        (!tabs.is_empty()).then_some(Pane::Tabs { tabs, active: 0 })
    }

    /// Every tab, in order: depth first, left to right.
    pub fn iter(&self) -> Box<dyn Iterator<Item = &T> + '_> {
        match self {
            Pane::Split { children, .. } => {
                Box::new(children.iter().flat_map(|slot| slot.pane.iter()))
            }
            Pane::Tabs { tabs, .. } => Box::new(tabs.iter()),
        }
    }

    /// Every tab, taken out in the order of [`Self::iter`].
    pub fn into_tabs(self) -> Vec<T> {
        match self {
            Pane::Split { children, .. } => children
                .into_iter()
                .flat_map(|slot| slot.pane.into_tabs())
                .collect(),
            Pane::Tabs { tabs, .. } => tabs,
        }
    }

    /// Maps each tab, dropping those `f` returns `None` for, then the groups
    /// and splits left empty. A split left with one child becomes that
    /// child. The shown tab stays shown if it's kept; otherwise its nearest
    /// kept neighbour is.
    pub fn filter_map<U>(self, f: &mut impl FnMut(T) -> Option<U>) -> Option<Pane<U>> {
        match self {
            Pane::Split { vertical, children } => {
                let mut children: Vec<Slot<U>> = children
                    .into_iter()
                    .filter_map(|slot| {
                        let pane = slot.pane.filter_map(f)?;
                        Some(Slot {
                            size: slot.size,
                            pane,
                        })
                    })
                    .collect();
                match children.len() {
                    0 => None,
                    1 => children.pop().map(|slot| slot.pane),
                    _ => Some(Pane::Split { vertical, children }),
                }
            }
            Pane::Tabs { tabs, active } => {
                let mut kept = Vec::new();
                let mut shown = 0;
                for (i, tab) in tabs.into_iter().enumerate() {
                    if let Some(tab) = f(tab) {
                        if i <= active {
                            shown = kept.len();
                        }
                        kept.push(tab);
                    }
                }
                (!kept.is_empty()).then_some(Pane::Tabs {
                    tabs: kept,
                    active: shown,
                })
            }
        }
    }

    /// Adds `tabs` to the end of the first tab group, keeping the shown tab.
    fn push_tabs(&mut self, more: Vec<T>) {
        match self {
            Pane::Tabs { tabs, .. } => tabs.extend(more),
            Pane::Split { children, .. } => match children.first_mut() {
                Some(slot) => slot.pane.push_tabs(more),
                None => children.extend(Pane::tabs(more).map(|pane| Slot { size: 0., pane })),
            },
        }
    }
}

/// `pane` with `tabs` added to its first tab group, or a group of its own.
fn append<T>(pane: Option<Pane<T>>, tabs: Vec<T>) -> Option<Pane<T>> {
    match pane {
        Some(mut pane) => {
            if !tabs.is_empty() {
                pane.push_tabs(tabs);
            }
            Some(pane)
        }
        None => Pane::tabs(tabs),
    }
}

/// A saved layout: the docks' sizes and where the tool panes go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelLayout {
    pub name: String,
    /// Tool panes kept in the main area, among the sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub center: Vec<ToolPane>,
    #[serde(default)]
    pub docks: Vec<Dock<ToolPane>>,
}

impl PanelLayout {
    /// The layout of `window`: its docks and its tool panes, not its content.
    pub fn capture<C: Clone>(name: impl Into<String>, window: &Arrangement<C>) -> Self {
        let tool = |tab: &Tab<C>| match tab {
            Tab::Tool(tool) => Some(*tool),
            Tab::Content(_) => None,
        };
        Self {
            name: name.into(),
            center: window
                .center
                .iter()
                .flat_map(|pane| pane.iter())
                .filter_map(tool)
                .collect(),
            docks: window
                .docks
                .iter()
                .map(|dock| Dock {
                    side: dock.side,
                    size: dock.size,
                    open: dock.open,
                    pane: dock
                        .pane
                        .clone()
                        .and_then(|pane| pane.filter_map(&mut |tab| tool(&tab))),
                })
                .collect(),
        }
    }

    /// Every tool pane the layout places.
    pub fn tools(&self) -> impl Iterator<Item = ToolPane> + '_ {
        let docked = self
            .docks
            .iter()
            .flat_map(|dock| dock.pane.iter().flat_map(|pane| pane.iter()));
        self.center.iter().chain(docked).copied()
    }

    fn places(&self, tool: ToolPane) -> bool {
        self.tools().any(|t| t == tool)
    }

    /// `window` rearranged to this layout. Tool panes the layout names go
    /// where it says, if the window has them; the docks take its sizes and
    /// open state. Everything else stays in its region, joining that
    /// region's first tab group, so nothing leaves the window.
    pub fn apply<C>(&self, window: Arrangement<C>) -> Arrangement<C> {
        let has: Vec<ToolPane> = {
            let center = window.center.iter().flat_map(|pane| pane.iter());
            let docked = window
                .docks
                .iter()
                .flat_map(|dock| dock.pane.iter().flat_map(|pane| pane.iter()));
            center
                .chain(docked)
                .filter_map(|tab| match tab {
                    Tab::Tool(tool) => Some(*tool),
                    Tab::Content(_) => None,
                })
                .collect()
        };
        let present = |tool: &ToolPane| has.contains(tool);
        // Whatever the layout doesn't place.
        let mut unplaced = |tab: Tab<C>| match tab {
            Tab::Tool(tool) if self.places(tool) => None,
            tab => Some(tab),
        };

        let center = window
            .center
            .and_then(|pane| pane.filter_map(&mut unplaced));
        let center = append(
            center,
            self.center
                .iter()
                .filter(|t| present(t))
                .map(|t| Tab::Tool(*t))
                .collect(),
        );

        let mut docks = Vec::new();
        let mut current = window.docks;
        for side in Side::ALL {
            let saved = self.docks.iter().find(|dock| dock.side == side);
            let now = current
                .iter()
                .position(|dock| dock.side == side)
                .map(|i| current.remove(i));
            let (size, open) = match (saved, &now) {
                (Some(dock), _) => (dock.size, dock.open),
                (None, Some(dock)) => (dock.size, dock.open),
                (None, None) => continue,
            };
            let placed = saved
                .and_then(|dock| dock.pane.clone())
                .and_then(|pane| pane.filter_map(&mut |t| present(&t).then_some(Tab::Tool(t))));
            let staying = now
                .and_then(|dock| dock.pane)
                .map(|pane| {
                    pane.into_tabs()
                        .into_iter()
                        .filter_map(&mut unplaced)
                        .collect()
                })
                .unwrap_or_default();
            docks.push(Dock {
                side,
                size,
                open,
                pane: append(placed, staying),
            });
        }
        Arrangement { center, docks }
    }
}

/// A layout name as entered, trimmed; an error says what's wrong with it.
pub fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name for the layout.".into());
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(format!(
            "Keep the name to {MAX_NAME_LEN} characters or fewer."
        ));
    }
    Ok(name)
}

/// The saved layouts, and which one each project starts with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layouts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<PanelLayout>,
    /// Applied when a project opens, unless the project has its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Projects that start with a layout other than the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectLayout>,
}

/// The layout one project starts with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLayout {
    pub project: PathBuf,
    pub layout: String,
}

impl Layouts {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&PanelLayout> {
        self.saved.iter().find(|layout| layout.name == name)
    }

    /// Saves `layout`, replacing the one of the same name in its place.
    pub fn save(&mut self, layout: PanelLayout) {
        match self.saved.iter_mut().find(|l| l.name == layout.name) {
            Some(existing) => *existing = layout,
            None => self.saved.push(layout),
        }
    }

    /// Deletes a layout; projects that started with it start with the default.
    pub fn delete(&mut self, name: &str) {
        self.saved.retain(|layout| layout.name != name);
        if self.default.as_deref() == Some(name) {
            self.default = None;
        }
        self.projects.retain(|p| p.layout != name);
    }

    /// Makes `name` the default; `None` starts projects with the built-in layout.
    pub fn set_default(&mut self, name: Option<&str>) {
        self.default = name.filter(|n| self.get(n).is_some()).map(str::to_string);
    }

    /// The layout `project` starts with, if it has its own.
    pub fn project_default(&self, project: &Path) -> Option<&str> {
        self.projects
            .iter()
            .find(|p| same_path(&p.project, project))
            .map(|p| p.layout.as_str())
    }

    /// Gives `project` a layout of its own; `None` returns it to the default.
    pub fn set_project_default(&mut self, project: &Path, name: Option<&str>) {
        self.projects.retain(|p| !same_path(&p.project, project));
        if let Some(name) = name.filter(|n| self.get(n).is_some()) {
            self.projects.push(ProjectLayout {
                project: project.to_path_buf(),
                layout: name.to_string(),
            });
        }
    }

    /// The layout `project` opens with: its own, else the default, else none.
    pub fn for_project(&self, project: &Path) -> Option<&PanelLayout> {
        self.project_default(project)
            .or(self.default.as_deref())
            .and_then(|name| self.get(name))
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    // Windows paths are case-insensitive.
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    use ToolPane::{Build, Debug, Files, Tests};

    fn tool(t: ToolPane) -> Tab<&'static str> {
        Tab::Tool(t)
    }

    fn content(c: &'static str) -> Tab<&'static str> {
        Tab::Content(c)
    }

    fn tabs<T>(tabs: Vec<T>) -> Pane<T> {
        Pane::Tabs { tabs, active: 0 }
    }

    fn dock<T>(side: Side, size: f32, open: bool, pane: Option<Pane<T>>) -> Dock<T> {
        Dock {
            side,
            size,
            open,
            pane,
        }
    }

    /// What a project window opens with: Files left, sessions in the middle,
    /// the build pane right.
    fn fresh_window() -> Arrangement<&'static str> {
        Arrangement {
            center: Some(tabs(vec![content("Claude 1")])),
            docks: vec![
                dock(Side::Left, 280., true, Some(tabs(vec![tool(Files)]))),
                dock(Side::Right, 560., true, Some(tabs(vec![tool(Build)]))),
            ],
        }
    }

    fn debugging() -> PanelLayout {
        PanelLayout {
            name: "Debugging".into(),
            center: vec![],
            docks: vec![
                dock(Side::Left, 300., false, Some(tabs(vec![Files]))),
                dock(Side::Right, 400., true, Some(tabs(vec![Build]))),
                dock(
                    Side::Bottom,
                    320.,
                    true,
                    Some(Pane::Split {
                        vertical: false,
                        children: vec![
                            Slot {
                                size: 600.,
                                pane: tabs(vec![Debug]),
                            },
                            Slot {
                                size: 0.,
                                pane: tabs(vec![Tests]),
                            },
                        ],
                    }),
                ),
            ],
        }
    }

    #[test]
    fn capture_keeps_tool_panes_and_dock_sizes_but_not_content() {
        let mut window = fresh_window();
        window.docks[1].pane = Some(tabs(vec![tool(Build), content("Terminal")]));
        window.center = Some(tabs(vec![content("Claude 1"), tool(Tests)]));

        let layout = PanelLayout::capture("Mine", &window);

        assert_eq!(layout.name, "Mine");
        assert_eq!(layout.center, vec![Tests]);
        assert_eq!(
            layout.docks,
            vec![
                dock(Side::Left, 280., true, Some(tabs(vec![Files]))),
                dock(Side::Right, 560., true, Some(tabs(vec![Build]))),
            ]
        );
    }

    #[test]
    fn a_dock_holding_only_content_is_captured_empty() {
        let mut window = fresh_window();
        window.docks.push(dock(
            Side::Bottom,
            200.,
            true,
            Some(tabs(vec![content("Terminal")])),
        ));

        let layout = PanelLayout::capture("Mine", &window);

        assert_eq!(layout.docks[2], dock(Side::Bottom, 200., true, None));
    }

    #[test]
    fn applying_moves_tool_panes_and_resizes_docks() {
        let mut window = fresh_window();
        // Debug and Tests are open, side by side in the main area.
        window.center = Some(Pane::Split {
            vertical: false,
            children: vec![
                Slot {
                    size: 0.,
                    pane: tabs(vec![content("Claude 1")]),
                },
                Slot {
                    size: 300.,
                    pane: tabs(vec![tool(Debug), tool(Tests)]),
                },
            ],
        });

        let applied = debugging().apply(window);

        assert_eq!(applied.center, Some(tabs(vec![content("Claude 1")])));
        assert_eq!(
            applied.docks,
            debugging()
                .docks
                .into_iter()
                .map(|d| Dock {
                    side: d.side,
                    size: d.size,
                    open: d.open,
                    pane: d
                        .pane
                        .and_then(|p| p.filter_map(&mut |t| Some(Tab::Tool(t)))),
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tool_panes_the_window_doesnt_have_are_left_out() {
        // Neither Tests nor Debug is open.
        let applied = debugging().apply(fresh_window());

        let bottom = applied
            .docks
            .iter()
            .find(|d| d.side == Side::Bottom)
            .unwrap();
        assert_eq!(bottom.pane, None);
        assert_eq!((bottom.size, bottom.open), (320., true));
    }

    #[test]
    fn content_stays_in_its_dock() {
        let mut window = fresh_window();
        window.docks[0].pane = Some(tabs(vec![tool(Files), content("Terminal")]));

        let applied = debugging().apply(window);

        assert_eq!(
            applied.docks[0].pane,
            Some(tabs(vec![tool(Files), content("Terminal")]))
        );
    }

    #[test]
    fn tool_panes_the_layout_doesnt_name_stay_put() {
        let layout = PanelLayout {
            name: "Wide".into(),
            center: vec![],
            docks: vec![dock(Side::Left, 400., true, Some(tabs(vec![Files])))],
        };
        let mut window = fresh_window();
        window.docks.push(dock(
            Side::Bottom,
            240.,
            true,
            Some(tabs(vec![tool(Tests)])),
        ));

        let applied = layout.apply(window);

        let sides: Vec<_> = applied
            .docks
            .iter()
            .map(|d| (d.side, d.size, d.pane.clone()))
            .collect();
        assert_eq!(
            sides,
            vec![
                (Side::Left, 400., Some(tabs(vec![tool(Files)]))),
                (Side::Right, 560., Some(tabs(vec![tool(Build)]))),
                (Side::Bottom, 240., Some(tabs(vec![tool(Tests)]))),
            ]
        );
    }

    #[test]
    fn tool_panes_for_the_main_area_join_its_first_group() {
        let layout = PanelLayout {
            name: "Tests up front".into(),
            center: vec![Tests],
            docks: vec![],
        };
        let mut window = fresh_window();
        window.docks.push(dock(
            Side::Bottom,
            240.,
            true,
            Some(tabs(vec![tool(Tests)])),
        ));

        let applied = layout.apply(window);

        assert_eq!(
            applied.center,
            Some(tabs(vec![content("Claude 1"), tool(Tests)]))
        );
        assert_eq!(applied.docks[2].pane, None);
    }

    #[test]
    fn an_empty_main_area_gets_a_group_for_its_tool_panes() {
        let layout = PanelLayout {
            name: "Files in the middle".into(),
            center: vec![Files],
            docks: vec![],
        };
        let mut window = fresh_window();
        window.center = Some(Pane::Split {
            vertical: false,
            children: vec![],
        });

        let applied = layout.apply(window);

        assert_eq!(applied.center, Some(tabs(vec![tool(Files)])));
    }

    #[test]
    fn capture_then_apply_reproduces_the_tool_panes() {
        let mut window = fresh_window();
        window.docks.push(dock(
            Side::Bottom,
            300.,
            false,
            Some(tabs(vec![tool(Debug), tool(Tests)])),
        ));
        let layout = PanelLayout::capture("Mine", &window);

        // Shuffle everything about, then apply the layout again.
        let shuffled = Arrangement {
            center: Some(tabs(vec![content("Claude 1"), tool(Debug)])),
            docks: vec![
                dock(Side::Left, 100., false, Some(tabs(vec![tool(Build)]))),
                dock(Side::Right, 900., true, Some(tabs(vec![tool(Files)]))),
                dock(Side::Bottom, 50., true, Some(tabs(vec![tool(Tests)]))),
            ],
        };

        assert_eq!(layout.apply(shuffled), window);
    }

    #[test]
    fn filtering_keeps_the_shown_tab_or_its_neighbour() {
        let pane = Pane::Tabs {
            tabs: vec![1, 2, 3, 4],
            active: 2,
        };
        assert_eq!(
            pane.clone().filter_map(&mut |n| (n != 1).then_some(n)),
            Some(Pane::Tabs {
                tabs: vec![2, 3, 4],
                active: 1
            })
        );
        assert_eq!(
            pane.clone().filter_map(&mut |n| (n != 3).then_some(n)),
            Some(Pane::Tabs {
                tabs: vec![1, 2, 4],
                active: 1
            })
        );
        assert_eq!(
            pane.filter_map(&mut |n| (n == 4).then_some(n)),
            Some(Pane::Tabs {
                tabs: vec![4],
                active: 0
            })
        );
    }

    #[test]
    fn filtering_collapses_splits_left_with_one_child() {
        let pane = Pane::Split {
            vertical: true,
            children: vec![
                Slot {
                    size: 100.,
                    pane: tabs(vec![1]),
                },
                Slot {
                    size: 0.,
                    pane: tabs(vec![2]),
                },
            ],
        };
        assert_eq!(
            pane.filter_map(&mut |n| (n == 2).then_some(n)),
            Some(tabs(vec![2]))
        );
    }

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(check_name("  Debugging "), Ok("Debugging"));
        assert!(check_name("   ").is_err());
        assert!(check_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn saving_a_name_again_replaces_it_in_place() {
        let mut layouts = Layouts::default();
        layouts.save(debugging());
        layouts.save(PanelLayout {
            name: "Coding".into(),
            center: vec![],
            docks: vec![],
        });
        let changed = PanelLayout {
            center: vec![Tests],
            ..debugging()
        };
        layouts.save(changed.clone());

        let names: Vec<_> = layouts.saved.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Debugging", "Coding"]);
        assert_eq!(layouts.get("Debugging"), Some(&changed));
    }

    #[test]
    fn a_project_starts_with_its_own_layout_or_the_default() {
        let mut layouts = Layouts::default();
        layouts.save(debugging());
        let coding = PanelLayout {
            name: "Coding".into(),
            center: vec![],
            docks: vec![],
        };
        layouts.save(coding.clone());
        let game = Path::new(r"D:\Game\Game.uproject");
        let other = Path::new(r"D:\Other\Other.uproject");

        assert_eq!(layouts.for_project(game), None);
        layouts.set_default(Some("Coding"));
        layouts.set_project_default(game, Some("Debugging"));

        assert_eq!(layouts.for_project(game), Some(&debugging()));
        assert_eq!(
            layouts.for_project(Path::new(r"d:\game\GAME.uproject")),
            Some(&debugging())
        );
        assert_eq!(layouts.for_project(other), Some(&coding));

        layouts.set_project_default(game, None);
        assert_eq!(layouts.for_project(game), Some(&coding));
    }

    #[test]
    fn only_saved_layouts_become_defaults() {
        let mut layouts = Layouts::default();
        layouts.set_default(Some("Nope"));
        layouts.set_project_default(Path::new("Game.uproject"), Some("Nope"));
        assert!(layouts.is_empty());
    }

    #[test]
    fn deleting_a_layout_clears_the_defaults_naming_it() {
        let mut layouts = Layouts::default();
        layouts.save(debugging());
        layouts.set_default(Some("Debugging"));
        layouts.set_project_default(Path::new("Game.uproject"), Some("Debugging"));

        layouts.delete("Debugging");

        assert!(layouts.is_empty());
    }
}
