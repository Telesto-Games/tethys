//! One window: a project summary plus its agent sessions as tabs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    DockArea, DockEvent, DockPlacement, DockSkin, PanelHandle, PanelId, PanelStyle,
};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_adapters::agent_terminal::TerminalHost;
use tethys_adapters::config_toml::TomlConfigStore;
use tethys_adapters::engine_registry::RegistryEngineLocator;
use tethys_adapters::process_launcher::DetachedLauncher;
use tethys_core::ports::{AgentSession, ConfigStore, EventSink, SessionEvent};
use tethys_core::unreal::Configuration;
use tethys_core::usecases::RecentProject;
use tethys_core::{AgentProfile, AssociationKind, Project, SessionId, unreal, usecases};

use crate::session_panel::{SessionKind, SessionPanel};

gpui_kit::actions!(
    tethys,
    [
        OpenProject,
        NewSession,
        CloseSession,
        NextSession,
        PrevSession,
        BuildProject,
        LaunchEditor,
        ShowProjects
    ]
);

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-shift-p", ShowProjects, None),
        KeyBinding::new("ctrl-shift-b", BuildProject, None),
        KeyBinding::new("ctrl-shift-e", LaunchEditor, None),
        KeyBinding::new("ctrl-shift-o", OpenProject, None),
        KeyBinding::new("ctrl-shift-t", NewSession, None),
        KeyBinding::new("ctrl-shift-w", CloseSession, None),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PrevSession, None),
    ]
}

/// The adapters, created once by the composition root.
pub struct Services {
    pub host: TerminalHost,
    pub config: TomlConfigStore,
    pub locator: RegistryEngineLocator,
    next_session: AtomicU64,
}

impl Services {
    pub fn new() -> Self {
        Self {
            host: TerminalHost::new(),
            config: TomlConfigStore::user_default(),
            locator: RegistryEngineLocator,
            next_session: AtomicU64::new(1),
        }
    }

    fn next_session_id(&self) -> SessionId {
        SessionId(self.next_session.fetch_add(1, Ordering::Relaxed))
    }
}

impl Global for Services {}

/// Opens a Tethys window, optionally loading a `.uproject` into it.
pub fn open_window(path: Option<PathBuf>, cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Tethys".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(size(px(1280.), px(840.)), cx)),
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(path, window, cx))
    });
    if let Err(e) = opened {
        eprintln!("tethys: failed to open window: {e}");
    }
}

pub struct Workspace {
    project: Option<Project>,
    error: Option<String>,
    recent: Vec<RecentProject>,
    profiles: Vec<AgentProfile>,
    /// Session panels in the order they were opened. The dock decides where
    /// they are shown; panels closed from the dock are pruned on layout change.
    panels: Vec<Entity<SessionPanel>>,
    dock: Entity<DockArea>,
    sessions_started: usize,
    /// Configuration for builds and editor launches; saved when changed.
    configuration: Configuration,
    events: UnboundedSender<(SessionId, SessionEvent)>,
    focus: FocusHandle,
    _events: Task<()>,
    _dock_events: Subscription,
}

impl Workspace {
    fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let events = cx.spawn(async move |this, cx| {
            while let Some((id, event)) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_session_event(id, event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let (dock, skin) = DockSkin::dock_area("sessions", None, window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);
        skin.set_close_button_visible(true, cx);
        let dock_events = cx.subscribe(&dock, |this, _, event: &DockEvent, cx| {
            if let DockEvent::LayoutChanged = event {
                this.prune_closed_panels(cx);
            }
        });

        let services = cx.global::<Services>();
        let recent = usecases::recent_projects(&services.config, &services.locator);
        let (profiles, problems) = usecases::agent_profiles(&services.config);
        let configuration = services.config.build_configuration().unwrap_or_default();

        // Stop sessions as the window closes, so quitting can wait for them.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                for panel in this.panels.drain(..) {
                    panel.update(cx, |panel, _| panel.stop());
                }
            });
            true
        });

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut this = Self {
            project: None,
            error: (!problems.is_empty()).then(|| problems.join("\n")),
            recent,
            profiles,
            panels: Vec::new(),
            dock,
            sessions_started: 0,
            configuration,
            events: tx,
            focus,
            _events: events,
            _dock_events: dock_events,
        };
        if let Some(path) = path {
            this.open(path, window, cx);
        }
        this
    }

    /// Opens a project here, or in a new window if this one already has one.
    fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.is_some() {
            cx.defer(move |cx| open_window(Some(path), cx));
            return;
        }
        let path = std::path::absolute(&path).unwrap_or(path);
        let services = cx.global::<Services>();
        match usecases::open_project(&path, &services.locator) {
            Ok(project) => {
                if let Err(e) = usecases::remember_project(&services.config, &path) {
                    eprintln!("tethys: can't save recent projects: {e}");
                }
                window.set_window_title(&format!("{} — Tethys", project.name()));
                self.project = Some(project);
                self.error = None;
                self.new_session(&NewSession, window, cx);
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.reload_recent(cx);
            }
        }
        cx.notify();
    }

    fn reload_recent(&mut self, cx: &mut Context<Self>) {
        let services = cx.global::<Services>();
        self.recent = usecases::recent_projects(&services.config, &services.locator);
    }

    fn forget(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Err(e) = usecases::forget_project(&cx.global::<Services>().config, path) {
            self.error = Some(format!("can't update recent projects: {e}"));
        }
        self.reload_recent(cx);
        cx.notify();
    }

    /// Opens a project browser in a new window.
    fn show_projects(&mut self, _: &ShowProjects, _: &mut Window, cx: &mut Context<Self>) {
        cx.defer(|cx| open_window(None, cx));
    }

    fn prompt_open(&mut self, _: &OpenProject, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Open Unreal project")
            .add_filter("Unreal project", &["uproject"])
            .pick_file();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(file) = dialog.await {
                let path = file.path().to_path_buf();
                let _ = this.update_in(cx, |this, window, cx| this.open(path, window, cx));
            }
        })
        .detach();
    }

    /// Starts the default (first) agent profile.
    fn new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        self.start_agent(0, window, cx);
    }

    /// Starts the agent profile at `index` in a new tab.
    fn start_agent(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.get(index).cloned() else {
            return;
        };
        self.sessions_started += 1;
        let label = format!("{} {}", profile.name, self.sessions_started);
        self.start_tab(
            label,
            SessionKind::Agent,
            window,
            cx,
            |host, id, project, sink| {
                usecases::start_session(&[host], id, &profile, project, sink)
                    .map_err(|e| e.to_string())
            },
        );
    }

    fn build(&mut self, _: &BuildProject, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        // One build at a time: focus the running one instead.
        if let Some(i) = self.panels.iter().position(|p| {
            let p = p.read(cx);
            p.kind() == SessionKind::Build && p.is_running()
        }) {
            self.activate(i, window, cx);
            return;
        }
        let configuration = self.configuration;
        let label = format!(
            "Build {} {}",
            unreal::editor_target(project),
            configuration.as_str()
        );
        self.start_tab(
            label,
            SessionKind::Build,
            window,
            cx,
            |host, id, project, sink| {
                usecases::start_build(&[host], id, project, configuration, sink)
                    .map_err(|e| e.to_string())
            },
        );
    }

    fn set_configuration(&mut self, configuration: Configuration, cx: &mut Context<Self>) {
        self.configuration = configuration;
        if let Err(e) = cx
            .global::<Services>()
            .config
            .set_build_configuration(configuration)
        {
            self.error = Some(format!("can't save build configuration: {e}"));
        }
        cx.notify();
    }

    fn launch_editor(&mut self, _: &LaunchEditor, _: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        if let Err(e) = usecases::launch_editor(project, self.configuration, &DetachedLauncher) {
            self.error = Some(e.to_string());
            cx.notify();
        }
    }

    /// Starts a terminal session with `start` and adds it to the dock.
    fn start_tab(
        &mut self,
        label: String,
        kind: SessionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
        start: impl FnOnce(
            &TerminalHost,
            SessionId,
            &Project,
            EventSink,
        ) -> Result<Box<dyn AgentSession>, String>,
    ) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let services = cx.global::<Services>();
        let host = services.host.clone();
        let id = services.next_session_id();
        let tx = self.events.clone();
        let sink: EventSink = Arc::new(move |id, event| {
            let _ = tx.unbounded_send((id, event));
        });

        match start(&host, id, project, sink) {
            Ok(session) => {
                let Some(handle) = host.take_handle(id) else {
                    self.error = Some("terminal host lost the session".into());
                    return;
                };
                let panel = cx.new(|cx| SessionPanel::new(id, kind, label, session, handle, cx));
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view(
                        Arc::new(PanelHandle::new(panel.clone())),
                        DockPlacement::Center,
                        None,
                        window,
                        cx,
                    )
                });
                self.panels.push(panel);
                self.activate(self.panels.len() - 1, window, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// Drops panels the dock no longer holds (closed from their tab).
    fn prune_closed_panels(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let before = self.panels.len();
        self.panels
            .retain(|p| dock.panel(PanelId::from(p.entity_id())).is_some());
        if self.panels.len() != before {
            cx.notify();
        }
    }

    /// The panel holding keyboard focus, if any.
    fn focused_panel(&self, window: &Window, cx: &App) -> Option<usize> {
        self.panels
            .iter()
            .position(|p| p.focus_handle(cx).contains_focused(window, cx))
    }

    fn close_session(&mut self, _: &CloseSession, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self
            .focused_panel(window, cx)
            .or(self.panels.len().checked_sub(1))
        else {
            return;
        };
        let panel = self.panels.remove(index);
        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        if self.panels.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.activate(index.min(self.panels.len() - 1), window, cx);
        }
        cx.notify();
    }

    fn next_session(&mut self, _: &NextSession, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panels.is_empty() {
            let next = self.focused_panel(window, cx).map_or(0, |i| i + 1);
            self.activate(next % self.panels.len(), window, cx);
        }
    }

    fn prev_session(&mut self, _: &PrevSession, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panels.is_empty() {
            let n = self.panels.len();
            let prev = self.focused_panel(window, cx).map_or(n - 1, |i| i + n - 1);
            self.activate(prev % n, window, cx);
        }
    }

    /// Shows a panel in its tab group and focuses it.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.panels.get(index).cloned() else {
            return;
        };
        self.dock.update(cx, |dock, cx| {
            dock.select_panel(PanelId::from(panel.entity_id()), window, cx)
        });
        let focus = panel.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn on_session_event(&mut self, id: SessionId, event: SessionEvent, cx: &mut Context<Self>) {
        let Some(panel) = self.panels.iter().find(|p| p.read(cx).id() == id).cloned() else {
            return;
        };
        match event {
            SessionEvent::TitleChanged(title) => panel.update(cx, |p, cx| p.set_title(title, cx)),
            SessionEvent::Exited(code) => panel.update(cx, |p, cx| p.set_exited(code, cx)),
            SessionEvent::Started | SessionEvent::Bell => return,
        }
        // Tab titles are drawn by the dock.
        self.dock.update(cx, |_, cx| cx.notify());
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let error = self.error.clone()?;
        Some(
            div()
                .id("error")
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .bg(cx.theme().danger.opacity(0.15))
                .text_color(cx.theme().danger)
                .text_sm()
                .child(div().flex_1().min_w_0().child(error))
                .child(
                    div().flex_none().child(
                        Button::new("dismiss-error")
                            .ghost()
                            .xsmall()
                            .label("Dismiss")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    ),
                ),
        )
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let hover = theme.secondary_hover;
        let recent = self.recent.iter().enumerate().map(|(i, entry)| {
            let open_path = entry.path.clone();
            let forget_path = entry.path.clone();
            let name = entry
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (detail, detail_color) = match &entry.project {
                Ok(project) => match &project.engine {
                    Ok(_) => (format!("Engine: {}", association_label(project)), muted),
                    Err(e) => (
                        format!("Engine: {} — {e}", association_label(project)),
                        danger,
                    ),
                },
                Err(e) => (e.clone(), danger),
            };
            let available = entry.project.is_ok();

            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .hover(|s| s.bg(hover))
                .child(
                    div()
                        .id(("recent", i))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .px_3()
                        .py_1p5()
                        .cursor_pointer()
                        .when(!available, |d| d.opacity(0.6))
                        .child(div().font_weight(FontWeight::MEDIUM).child(name))
                        .child(div().text_xs().text_color(detail_color).child(detail))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(entry.path.display().to_string()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open(open_path.clone(), window, cx)
                        })),
                )
                .child(
                    div().flex_none().pr_2().child(
                        Button::new(("forget", i))
                            .ghost()
                            .xsmall()
                            .label("Remove")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.forget(&forget_path, cx)),
                            ),
                    ),
                )
        });

        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .w(px(640.))
                    .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).child("Tethys"))
                    .child(
                        div()
                            .text_color(muted)
                            .child("Open a .uproject to start a Claude Code session in it. You can also drop one onto this window."),
                    )
                    .child(
                        div().flex().child(
                            Button::new("open")
                                .primary()
                                .label("Open .uproject…  (Ctrl+Shift+O)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.prompt_open(&OpenProject, window, cx)
                                })),
                        ),
                    )
                    .child(div().text_sm().text_color(muted).child("Recent projects"))
                    .child(if self.recent.is_empty() {
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("None yet. Projects you open appear here.")
                            .into_any_element()
                    } else {
                        div()
                            .id("recent-list")
                            .flex()
                            .flex_col()
                            .gap_1()
                            .max_h(px(480.))
                            .overflow_y_scroll()
                            .children(recent)
                            .into_any_element()
                    }),
            )
    }

    fn render_project(&self, project: &Project, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let association = association_label(project);
        let engine = match &project.engine {
            Ok(path) => div()
                .text_color(theme.muted_foreground)
                .child(format!("Engine: {association} → {}", path.display())),
            Err(e) => div()
                .text_color(theme.danger)
                .child(format!("Engine: {association} — {e}")),
        };
        let enabled_plugins = project.plugins.iter().filter(|(_, on)| *on).count();

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .items_baseline()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(project.name().to_string()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(project.root().display().to_string()),
                            ),
                    )
                    .child(engine)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} modules: {} · {} plugins enabled",
                                project.modules.len(),
                                project.modules.join(", "),
                                enabled_plugins
                            )),
                    ),
            )
            .child(
                Button::new("projects")
                    .small()
                    .ghost()
                    .label("Projects")
                    .tooltip("Projects (Ctrl+Shift+P)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_projects(&ShowProjects, window, cx)
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap_0p5()
                    .p_0p5()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .border_1()
                    .border_color(theme.border)
                    .children(Configuration::ALL.map(|configuration| {
                        let button = Button::new(configuration.as_str())
                            .small()
                            .label(configuration.as_str())
                            .tooltip("Configuration for Build and Launch editor")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_configuration(configuration, cx)
                            }));
                        if configuration == self.configuration {
                            button.primary()
                        } else {
                            button.ghost()
                        }
                    })),
            )
            .child(
                Button::new("build")
                    .small()
                    .label("Build")
                    .tooltip("Build (Ctrl+Shift+B)")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.build(&BuildProject, window, cx)),
                    ),
            )
            .child(
                Button::new("launch-editor")
                    .small()
                    .label("Launch editor")
                    .tooltip("Launch editor (Ctrl+Shift+E)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch_editor(&LaunchEditor, window, cx)
                    })),
            )
            .child(
                Button::new("new-session")
                    .small()
                    .label("New session ▾")
                    .tooltip("Pick an agent (Ctrl+Shift+T starts the first)")
                    .dropdown_menu({
                        let this = cx.entity().downgrade();
                        let names: Vec<String> =
                            self.profiles.iter().map(|p| p.name.clone()).collect();
                        move |menu, _, _| {
                            names.iter().enumerate().fold(menu, |menu, (i, name)| {
                                let this = this.clone();
                                menu.item(PopupMenuItem::new(name.clone()).on_click(
                                    move |_, window, cx| {
                                        let _ = this
                                            .update(cx, |this, cx| this.start_agent(i, window, cx));
                                    },
                                ))
                            })
                        }
                    }),
            )
            .when(!self.panels.is_empty(), |d| {
                d.child(
                    Button::new("close-session")
                        .small()
                        .ghost()
                        .label("Close session")
                        .tooltip("Close session (Ctrl+Shift+W)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_session(&CloseSession, window, cx)
                        })),
                )
            });

        let body = if self.panels.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("No sessions. Press Ctrl+Shift+T to start one.")
        } else {
            // Drag a tab to an edge of a pane to split, or onto another tab bar to move it.
            div().flex_1().min_h_0().child(self.dock.clone())
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.project.clone() {
            Some(project) => self.render_project(&project, cx).into_any_element(),
            None => self.render_welcome(cx).into_any_element(),
        };
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::prompt_open))
            .on_action(cx.listener(Self::new_session))
            .on_action(cx.listener(Self::close_session))
            .on_action(cx.listener(Self::next_session))
            .on_action(cx.listener(Self::prev_session))
            .on_action(cx.listener(Self::build))
            .on_action(cx.listener(Self::launch_editor))
            .on_action(cx.listener(Self::show_projects))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().iter().find(|p| is_uproject(p)) {
                    this.open(path.clone(), window, cx);
                }
            }))
            .children(self.render_error(cx))
            .child(content)
    }
}

fn is_uproject(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("uproject"))
}

fn association_label(project: &Project) -> String {
    match project.association_kind() {
        AssociationKind::Native => "native (in-tree)".to_string(),
        AssociationKind::Launcher => format!("Launcher {}", project.engine_association),
        AssociationKind::SourceBuild => format!("source build {}", project.engine_association),
    }
}
