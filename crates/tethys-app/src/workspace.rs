//! One window: a project summary plus its agent sessions as tabs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::assets::IconName;
use gpui_kit::base::{Disableable, GlobalState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    DockArea, DockEvent, DockPlacement, DockSkin, Panel, PanelHandle, PanelId, PanelStyle,
};
use gpui_kit::component::menu::{AppMenuBar, DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::component::{Icon, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_adapters::agent_terminal::{self, TerminalHost};
use tethys_adapters::config_toml::TomlConfigStore;
use tethys_adapters::debug_dbgeng::DbgEng;
use tethys_adapters::engine_registry::RegistryEngineLocator;
use tethys_adapters::live_coding_hotkey::HotkeyLiveCoding;
use tethys_adapters::process_launcher::DetachedLauncher;
use tethys_adapters::scm_git::Git;
use tethys_adapters::scm_svn::Subversion;
use tethys_adapters::self_install::ExeReplacer;
use tethys_adapters::update_github::GitHubReleases;
use tethys_core::debug::{self as dbg, Breakpoints, DebugEnd, DebugEvent, StackFrame, StopReason};
use tethys_core::diff::FileDiff;
use tethys_core::ports::{
    AgentSession, ConfigStore, DebugEventSink, DebugSession, Debugger, EventSink, LiveCoding,
    SelfInstaller, SessionEvent, SourceControl, UpdateSource,
};
use tethys_core::unreal::Configuration;
use tethys_core::usecases::{DebugStart, RecentProject, ScmSummary};
use tethys_core::{AgentProfile, AssociationKind, Project, SessionId, unreal, usecases};

use crate::build_placeholder::BuildPlaceholder;
use crate::debug_panel::{
    BreakpointRow, DebugPanel, DebugPanelEvent, DebugStatus, DebugView, FrameRow,
};
use crate::diff_panel::DiffPanel;
use crate::editor_panel::{self, EditorEvent, EditorPanel};
use crate::file_tree::{FileTreeEvent, FileTreePanel};
use crate::session_panel::{SessionKind, SessionPanel};
use crate::settings_ui;
use crate::theme;
use crate::update_ui;

/// Initial width of the build pane on the right.
const BUILD_PANE_WIDTH: Pixels = px(560.);
/// Initial width of the Files pane on the left.
const FILES_PANE_WIDTH: Pixels = px(280.);
/// Initial height of the debug pane at the bottom.
const DEBUG_PANE_HEIGHT: Pixels = px(240.);
/// Height of every control in the project header, so they line up.
const TOOLBAR_HEIGHT: Pixels = px(28.);

gpui_kit::actions!(
    tethys,
    [
        OpenProject,
        NewSession,
        NewTerminal,
        CloseSession,
        NextSession,
        PrevSession,
        BuildProject,
        LiveCode,
        LaunchEditor,
        ShowProjects,
        OpenSettings,
        Exit,
        About,
        CheckForUpdates,
        AttachOrContinue,
        AttachToEditor,
        LaunchWithDebugger,
        PauseDebugger,
        DetachDebugger
    ]
);

/// The menu bar: File, Build and Help. Rendered in-window by `AppMenuBar`.
pub fn install_menus(cx: &mut App) {
    let menus = vec![
        Menu::new("File")
            .items([
                MenuItem::action("Open Project…", OpenProject),
                MenuItem::action("Projects…", ShowProjects),
                MenuItem::separator(),
                MenuItem::action("New Session", NewSession),
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("Close Session", CloseSession),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Exit", Exit),
            ])
            .owned(),
        Menu::new("Build")
            .items([
                MenuItem::action("Build", BuildProject),
                MenuItem::action("Live Coding (Ctrl+Alt+F11)", LiveCode),
                MenuItem::action("Launch Editor", LaunchEditor),
            ])
            .owned(),
        Menu::new("Debug")
            .items([
                MenuItem::action("Debug Editor / Continue", AttachOrContinue),
                MenuItem::action("Launch Editor with Debugger", LaunchWithDebugger),
                MenuItem::action("Attach to Running Editor", AttachToEditor),
                MenuItem::separator(),
                MenuItem::action("Pause", PauseDebugger),
                MenuItem::action("Detach", DetachDebugger),
                MenuItem::separator(),
                MenuItem::action("Toggle Breakpoint", editor_panel::ToggleBreakpoint),
            ])
            .owned(),
        Menu::new("Help")
            .items([
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("About Tethys", About),
            ])
            .owned(),
    ];
    GlobalState::global_mut(cx).set_app_menus(menus);
}

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-shift-p", ShowProjects, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-s", crate::editor_panel::Save, None),
        KeyBinding::new("f9", crate::editor_panel::ToggleBreakpoint, None),
        KeyBinding::new("f5", AttachOrContinue, None),
        KeyBinding::new("shift-f5", DetachDebugger, None),
        KeyBinding::new("ctrl-shift-b", BuildProject, None),
        KeyBinding::new("ctrl-shift-e", LaunchEditor, None),
        KeyBinding::new("ctrl-shift-o", OpenProject, None),
        KeyBinding::new("ctrl-shift-t", NewSession, None),
        KeyBinding::new("ctrl-shift-`", NewTerminal, None),
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
    /// Every source control Tethys knows; each project uses the one managing it.
    pub scms: Vec<Arc<dyn SourceControl>>,
    pub updates: Arc<dyn UpdateSource>,
    pub installer: Arc<dyn SelfInstaller>,
    pub debugger: Arc<dyn Debugger>,
    pub live_coding: Arc<dyn LiveCoding>,
    next_session: AtomicU64,
}

impl Services {
    pub fn new() -> Self {
        Self {
            host: TerminalHost::new(),
            config: TomlConfigStore::user_default(),
            locator: RegistryEngineLocator,
            scms: vec![Arc::new(Git::new()), Arc::new(Subversion::new())],
            updates: Arc::new(GitHubReleases::new(update_ui::REPO)),
            installer: Arc::new(ExeReplacer::new()),
            debugger: Arc::new(DbgEng),
            live_coding: Arc::new(HotkeyLiveCoding),
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
    menu_bar: Entity<AppMenuBar>,
    /// Shown in the build pane while it has no build tab.
    build_placeholder: Option<Entity<BuildPlaceholder>>,
    file_tree: Option<Entity<FileTreePanel>>,
    editors: Vec<Entity<EditorPanel>>,
    diffs: Vec<Entity<DiffPanel>>,
    /// The source control managing the project folder, if any.
    scm: Option<Arc<dyn SourceControl>>,
    subscriptions: Vec<Subscription>,
    debug: Debug,
}

/// The debugger side of a window.
#[derive(Default)]
struct Debug {
    session: Option<Box<dyn DebugSession>>,
    status: DebugStatus,
    /// What is attached, or a note about the last session.
    process: Option<String>,
    /// The stack of the last stop, with sources found on this machine.
    frames: Vec<FrameRow>,
    /// The frame shown in the editor.
    frame: Option<usize>,
    breakpoints: Breakpoints,
    panel: Option<Entity<DebugPanel>>,
    /// Tags events, so a late event from an earlier session is ignored.
    generation: u64,
    events: Option<futures::channel::mpsc::UnboundedSender<(u64, DebugEvent)>>,
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

        let (debug_tx, mut debug_rx) = futures::channel::mpsc::unbounded();
        cx.spawn_in(window, async move |this, cx| {
            while let Some((generation, event)) = debug_rx.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.on_debug_event(generation, event, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let (dock, skin) = DockSkin::dock_area("sessions", None, window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);
        skin.set_close_button_visible(true, cx);
        let dock_events =
            cx.subscribe_in(&dock, window, |this, _, event: &DockEvent, window, cx| {
                if let DockEvent::LayoutChanged = event {
                    this.prune_closed_panels(cx);
                    this.ensure_build_pane(window, cx);
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
                // Dropping the session detaches (and waits for it), so the
                // editor is never left with breakpoints in it.
                this.debug.session = None;
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
            menu_bar: AppMenuBar::new(cx),
            build_placeholder: None,
            file_tree: None,
            editors: Vec::new(),
            diffs: Vec::new(),
            scm: None,
            subscriptions: Vec::new(),
            debug: Debug {
                events: Some(debug_tx),
                ..Debug::default()
            },
        };
        if let Some(path) = path {
            this.open(path, window, cx);
        }
        // The first window checks for updates once, a little after startup.
        if update_ui::claim_startup_check() {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(update_ui::startup_delay())
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    update_ui::check(false, this.project_path(), window, cx)
                });
            })
            .detach();
        }
        this
    }

    fn project_path(&self) -> Option<PathBuf> {
        self.project.as_ref().map(|p| p.path.clone())
    }

    fn check_for_updates(
        &mut self,
        _: &CheckForUpdates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        update_ui::check(true, self.project_path(), window, cx);
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
                self.ensure_build_pane(window, cx);
                self.show_files(window, cx);
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

    fn about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            dialog.title("About Tethys").w(px(420.)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Tethys {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child("A lightweight, LLM-first IDE for Telesto Games' Unreal projects.")
                    .child(div().text_sm().text_color(muted).child(
                        "Runs Claude Code and opencode in native terminals, builds with UBT and launches the Unreal Editor.",
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Built with Rust, GPUI and alacritty_terminal."),
                    )
                    .child(div().text_xs().text_color(muted).child(
                        "© 2026 Gradient Ascent Ltd. MIT licensed; third-party licenses in THIRD-PARTY-NOTICES.md.",
                    )),
            )
        });
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

    /// Opens a plain shell in the project folder, with no agent.
    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.sessions_started += 1;
        let label = format!("Terminal {}", self.sessions_started);
        let shell = agent_terminal::default_shell();
        self.start_tab(
            label,
            SessionKind::Agent,
            window,
            cx,
            move |host, id, project, sink| {
                usecases::start_terminal(&[host], id, shell, project, sink)
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

    /// Asks the running editor to Live Code the changed C++.
    fn live_code(&mut self, _: &LiveCode, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.is_none() {
            return;
        }
        if let DebugStatus::Stopped(_) = self.debug.status {
            self.error =
                Some("The editor is stopped in the debugger. Continue it, then Live Code.".into());
            cx.notify();
            return;
        }
        // The shortcut Live Coding listens for also reaches this window; keep
        // it away from terminals, which would pass it to the agent.
        window.focus(&self.focus, cx);
        let live_coding = cx.global::<Services>().live_coding.clone();
        let compile = cx
            .background_executor()
            .spawn(async move { live_coding.compile().map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = compile.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Can't start Live Coding: {e}"));
                    cx.notify();
                });
            }
        })
        .detach();
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
        let args = settings_ui::editor_args(cx);
        if let Err(e) =
            usecases::launch_editor(project, self.configuration, &args, &DetachedLauncher)
        {
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
                // Agents go in the main area; builds go in the build pane on the right.
                let placement = match kind {
                    SessionKind::Agent => DockPlacement::Center,
                    SessionKind::Build => DockPlacement::Right,
                };
                if kind == SessionKind::Build {
                    self.close_finished_builds(window, cx);
                }
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view(
                        Arc::new(PanelHandle::new(panel.clone())),
                        placement,
                        Some(BUILD_PANE_WIDTH),
                        window,
                        cx,
                    )
                });
                self.panels.push(panel);
                if kind == SessionKind::Build
                    && let Some(placeholder) = self.build_placeholder.take()
                {
                    self.dock
                        .update(cx, |dock, cx| dock.remove_panel(placeholder, window, cx));
                }
                self.activate(self.panels.len() - 1, window, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// Keeps the build pane on the right: when it has no build tab, it shows
    /// the placeholder. Idempotent.
    fn ensure_build_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_build = self
            .panels
            .iter()
            .any(|p| p.read(cx).kind() == SessionKind::Build);
        if self.project.is_none() || has_build || self.build_placeholder.is_some() {
            return;
        }
        let placeholder = cx.new(BuildPlaceholder::new);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(PanelHandle::new(placeholder.clone())),
                DockPlacement::Right,
                Some(BUILD_PANE_WIDTH),
                window,
                cx,
            )
        });
        self.build_placeholder = Some(placeholder);
    }

    /// Removes build tabs whose build has finished, so the pane shows the latest log.
    fn close_finished_builds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (finished, kept): (Vec<_>, Vec<_>) = self.panels.drain(..).partition(|p| {
            let p = p.read(cx);
            p.kind() == SessionKind::Build && !p.is_running()
        });
        self.panels = kept;
        for panel in finished {
            self.dock
                .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        }
    }

    /// Adds the Files panel on the left and loads source-control status.
    fn show_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_ref() else {
            return;
        };
        let root = project.root().to_path_buf();
        self.scm = usecases::detect_source_control(&cx.global::<Services>().scms, &root);

        let tree = cx.new(|cx| FileTreePanel::new(root, cx));
        let events = cx.subscribe_in(&tree, window, |this, _, event, window, cx| match event {
            FileTreeEvent::Open(path) => this.open_file(path.clone(), window, cx),
            FileTreeEvent::Diff(path) => this.show_diff(path.clone(), None, window, cx),
        });
        self.subscriptions.push(events);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(PanelHandle::new(tree.clone())),
                DockPlacement::Left,
                Some(FILES_PANE_WIDTH),
                window,
                cx,
            )
        });
        self.file_tree = Some(tree);

        // Pick up changes made outside Tethys whenever the window comes back.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_files(cx);
            }
        });
        self.subscriptions.push(activation);
        self.refresh_status(cx);
    }

    /// Re-reads the file tree from disk and reloads source-control status.
    fn refresh_files(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.refresh(cx));
        }
        self.refresh_status(cx);
    }

    /// Loads working-copy status in the background and colours the tree with it.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let (Some(project), Some(tree)) = (self.project.as_ref(), self.file_tree.clone()) else {
            return;
        };
        let scm = self.scm.clone();
        let providers = cx
            .global::<Services>()
            .scms
            .iter()
            .map(|s| s.name())
            .collect();
        let root = project.root().to_path_buf();
        // Summary (footer) and file status (colours) together, off the UI thread.
        let loaded = cx.background_executor().spawn(async move {
            match scm {
                Some(scm) => (
                    usecases::scm_summary(scm.as_ref(), &root),
                    Some(usecases::working_copy_status(scm.as_ref(), &root)),
                ),
                None => (ScmSummary::NotUnderControl { providers }, None),
            }
        });
        cx.spawn(async move |this, cx| {
            let (summary, status) = loaded.await;
            let _ = this.update(cx, |this, cx| {
                tree.update(cx, |tree, cx| tree.set_scm(summary, cx));
                match status {
                    Some(Ok(status)) => {
                        tree.update(cx, |tree, cx| tree.set_status(Arc::new(status), cx))
                    }
                    Some(Err(e)) => {
                        this.error = Some(format!("Can't read source control status: {e}"));
                        cx.notify();
                    }
                    None => {}
                }
            });
        })
        .detach();
    }

    /// Opens a file in an editor tab, or focuses its existing tab.
    fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self
            .editors
            .iter()
            .find(|e| e.read(cx).path() == path)
            .cloned()
        {
            self.focus_panel(&editor, window, cx);
            return;
        }
        let raw = match editor_panel::read_text(&path) {
            Ok(raw) => raw,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let editor = cx.new(|cx| EditorPanel::new(path, &raw, window, cx));
        let events = cx.subscribe_in(&editor, window, |this, _, event, window, cx| {
            match event {
                EditorEvent::Saved => this.refresh_status(cx),
                EditorEvent::Diff(path, text) => {
                    this.show_diff(path.clone(), Some(text.clone()), window, cx)
                }
                EditorEvent::DirtyChanged => {}
                EditorEvent::ToggleBreakpoint(path, line) => {
                    this.toggle_breakpoint(path.clone(), *line, window, cx)
                }
            }
            // Tab titles (the unsaved marker) are drawn by the dock.
            this.dock.update(cx, |_, cx| cx.notify());
        });
        self.subscriptions.push(events);
        self.add_center_panel(&editor, window, cx);
        self.mark_editor(&editor, cx);
        self.editors.push(editor);
    }

    /// Shows `path`'s changes against source control. `text` is the current
    /// content when it differs from disk (an unsaved editor); otherwise the
    /// file is read.
    fn show_diff(
        &mut self,
        path: PathBuf,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scm) = self.scm.clone() else {
            self.error = Some(format!(
                "{} isn't under source control, so there's nothing to diff against.",
                path.display(),
            ));
            cx.notify();
            return;
        };
        let against = format!("against {} ({})", scm.base_label(), scm.name());
        let file = path.clone();
        let diff = cx.background_executor().spawn(async move {
            let text = match text {
                Some(text) => text,
                None => editor_panel::read_text(&file)?,
            };
            usecases::diff_against_base(scm.as_ref(), &file, &text).map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let diff = diff.await;
            let _ = this.update_in(cx, |this, window, cx| match diff {
                Ok(diff) => this.present_diff(path, against, diff, window, cx),
                Err(e) => {
                    this.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn present_diff(
        &mut self,
        path: PathBuf,
        against: String,
        diff: FileDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self
            .diffs
            .iter()
            .find(|d| d.read(cx).path() == path)
            .cloned()
        {
            panel.update(cx, |panel, cx| panel.set_diff(diff, cx));
            self.focus_panel(&panel, window, cx);
            return;
        }
        let panel = cx.new(|cx| DiffPanel::new(path, against, diff, cx));
        self.add_center_panel(&panel, window, cx);
        self.diffs.push(panel);
    }

    fn add_center_panel<P: Panel>(
        &mut self,
        panel: &Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(PanelHandle::new(panel.clone())),
                DockPlacement::Center,
                None,
                window,
                cx,
            )
        });
        self.focus_panel(panel, window, cx);
    }

    /// Selects a panel's tab and moves keyboard focus into it.
    fn focus_panel<P: Panel>(
        &mut self,
        panel: &Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.select_panel(PanelId::from(panel.entity_id()), window, cx)
        });
        let focus = panel.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Drops panels the dock no longer holds (closed from their tab).
    fn prune_closed_panels(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let held = |id: EntityId| dock.panel(PanelId::from(id)).is_some();
        let before = (self.panels.len(), self.editors.len(), self.diffs.len());
        self.panels.retain(|p| held(p.entity_id()));
        self.editors.retain(|p| held(p.entity_id()));
        self.diffs.retain(|p| held(p.entity_id()));
        if self
            .debug
            .panel
            .as_ref()
            .is_some_and(|p| !held(p.entity_id()))
        {
            self.debug.panel = None;
        }
        if (self.panels.len(), self.editors.len(), self.diffs.len()) != before {
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

    /// F5: start debugging (attach, or launch the editor), or continue after a stop.
    fn attach_or_continue(
        &mut self,
        _: &AttachOrContinue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.debug.status {
            DebugStatus::Detached => self.start_debugging(DebugStart::Auto, window, cx),
            DebugStatus::Stopped(_) => {
                if let Some(session) = &self.debug.session {
                    session.resume();
                }
            }
            DebugStatus::Attaching | DebugStatus::Running => {}
        }
    }

    fn attach_to_editor(
        &mut self,
        _: &AttachToEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_debugging(DebugStart::Attach, window, cx);
    }

    fn launch_with_debugger(
        &mut self,
        _: &LaunchWithDebugger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_debugging(DebugStart::Launch, window, cx);
    }

    fn pause_debugger(&mut self, _: &PauseDebugger, _: &mut Window, _: &mut Context<Self>) {
        if let Some(session) = &self.debug.session {
            session.pause();
        }
    }

    fn detach_debugger(&mut self, _: &DetachDebugger, _: &mut Window, _: &mut Context<Self>) {
        if let Some(session) = &self.debug.session {
            session.detach();
        }
    }

    /// Attaches to this project's running editor, or launches it under the
    /// debugger in the chosen configuration.
    fn start_debugging(&mut self, how: DebugStart, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(project), Some(tx)) = (self.project.clone(), self.debug.events.clone()) else {
            return;
        };
        if self.debug.status != DebugStatus::Detached {
            return;
        }
        self.debug.generation += 1;
        let generation = self.debug.generation;
        let sink: DebugEventSink = Arc::new(move |event| {
            let _ = tx.unbounded_send((generation, event));
        });
        self.debug.status = DebugStatus::Attaching;
        self.debug.process = None;
        self.show_debug_panel(window, cx);
        self.refresh_debug(cx);

        let debugger = cx.global::<Services>().debugger.clone();
        let configuration = self.configuration;
        let breakpoints = self.breakpoint_requests();
        let editor_args = settings_ui::editor_args(cx);
        // Listing processes and attaching take a few seconds on a big editor.
        let start = cx.background_executor().spawn(async move {
            usecases::debug_editor(
                debugger.as_ref(),
                &project,
                configuration,
                &editor_args,
                how,
                breakpoints,
                sink,
            )
            .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = start.await;
            let _ = this.update(cx, |this, cx| {
                match started {
                    Ok((debuggee, session)) => {
                        // Breakpoints changed while starting are sent now.
                        session.set_breakpoints(this.breakpoint_requests());
                        let process = &debuggee.process;
                        let how = if debuggee.launched {
                            "launched"
                        } else {
                            "attached"
                        };
                        this.debug.process =
                            Some(format!("{} (pid {}, {how})", process.exe, process.pid));
                        this.debug.session = Some(session);
                    }
                    Err(e) => {
                        this.debug.status = DebugStatus::Detached;
                        this.error = Some(format!("Can't debug: {e}"));
                    }
                }
                this.refresh_debug(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn on_debug_event(
        &mut self,
        generation: u64,
        event: DebugEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.debug.generation {
            return;
        }
        match event {
            DebugEvent::Attached => {}
            DebugEvent::Running => {
                self.debug.status = DebugStatus::Running;
                self.debug.frames.clear();
                self.debug.frame = None;
            }
            DebugEvent::Stopped { reason, frames, .. } => {
                self.debug.status = DebugStatus::Stopped(self.describe_stop(&reason));
                self.debug.frames = self.frame_rows(frames);
                // Show the innermost frame we have the source for.
                let shown = self.debug.frames.iter().position(|f| f.source.is_some());
                window.activate_window();
                match shown {
                    Some(i) => self.open_frame(i, window, cx),
                    None => self.show_debug_panel(window, cx),
                }
            }
            DebugEvent::Breakpoints(states) => self.debug.breakpoints.set_states(&states),
            DebugEvent::Ended(end) => {
                self.debug.session = None;
                self.debug.status = DebugStatus::Detached;
                self.debug.frames.clear();
                self.debug.frame = None;
                self.debug.breakpoints.reset_states();
                self.debug.process = match end {
                    DebugEnd::Detached => None,
                    DebugEnd::Exited => Some("The editor exited.".into()),
                    DebugEnd::Failed(e) => {
                        self.error = Some(format!("The debugger stopped: {e}"));
                        None
                    }
                };
            }
        }
        self.refresh_debug(cx);
        cx.notify();
    }

    fn describe_stop(&self, reason: &StopReason) -> String {
        let StopReason::Breakpoint(id) = reason else {
            return reason.describe();
        };
        match self.debug.breakpoints.all().iter().find(|b| b.id == *id) {
            Some(b) => {
                let name = b.file.file_name().unwrap_or_default().to_string_lossy();
                format!("Breakpoint at {name}:{}", b.line)
            }
            None => reason.describe(),
        }
    }

    /// Stack frames for display, with sources mapped to this machine.
    fn frame_rows(&self, frames: Vec<StackFrame>) -> Vec<FrameRow> {
        let engine = self.project.as_ref().and_then(|p| p.engine.clone().ok());
        frames
            .into_iter()
            .map(|frame| {
                let located = frame.file.zip(frame.line);
                let location = located.as_ref().map(|(file, line)| {
                    let name = file.file_name().unwrap_or_default().to_string_lossy();
                    format!("{name}:{line}")
                });
                let source = located.and_then(|(file, line)| {
                    let file = match &engine {
                        Some(root) => dbg::local_source(&file, root, |p| p.is_file()),
                        None => file,
                    };
                    file.is_file().then_some((file, line))
                });
                FrameRow {
                    function: frame.function,
                    location,
                    source,
                    current: false,
                }
            })
            .collect()
    }

    /// Shows frame `index` of the last stop in the editor.
    fn open_frame(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frame) = self.debug.frames.get(index) else {
            return;
        };
        self.debug.frame = Some(index);
        if let Some((file, line)) = frame.source.clone() {
            self.open_source(file, line, window, cx);
        }
        self.refresh_debug(cx);
    }

    /// Opens `file` in an editor at a 1-based line.
    fn open_source(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(file.clone(), window, cx);
        if let Some(editor) = self.editors.iter().find(|e| e.read(cx).path() == file) {
            editor.update(cx, |editor, cx| editor.go_to_line(line, window, cx));
        }
    }

    fn toggle_breakpoint(
        &mut self,
        file: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.debug.breakpoints.toggle(&file, line);
        if let Some(session) = &self.debug.session {
            session.set_breakpoints(self.breakpoint_requests());
        }
        self.show_debug_panel(window, cx);
        self.refresh_debug(cx);
    }

    fn breakpoint_requests(&self) -> Vec<dbg::SourceBreakpoint> {
        self.debug.breakpoints.requests(|p| p.is_file())
    }

    /// Adds the debug pane at the bottom, unless it's already there.
    fn show_debug_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.debug.panel.is_some() || self.project.is_none() {
            return;
        }
        let panel = cx.new(DebugPanel::new);
        let events = cx.subscribe_in(&panel, window, |this, _, event, window, cx| match event {
            DebugPanelEvent::Attach => this.start_debugging(DebugStart::Auto, window, cx),
            DebugPanelEvent::Detach => this.detach_debugger(&DetachDebugger, window, cx),
            DebugPanelEvent::Continue => this.attach_or_continue(&AttachOrContinue, window, cx),
            DebugPanelEvent::Pause => this.pause_debugger(&PauseDebugger, window, cx),
            DebugPanelEvent::OpenFrame(i) => this.open_frame(*i, window, cx),
            DebugPanelEvent::OpenBreakpoint(file, line) => {
                this.open_source(file.clone(), *line, window, cx)
            }
            DebugPanelEvent::RemoveBreakpoint(id) => {
                this.debug.breakpoints.remove(*id);
                if let Some(session) = &this.debug.session {
                    session.set_breakpoints(this.breakpoint_requests());
                }
                this.refresh_debug(cx);
            }
        });
        self.subscriptions.push(events);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(PanelHandle::new(panel.clone())),
                DockPlacement::Bottom,
                Some(DEBUG_PANE_HEIGHT),
                window,
                cx,
            )
        });
        self.debug.panel = Some(panel);
    }

    /// Redraws the debug pane and every editor's breakpoint and stop marks.
    fn refresh_debug(&mut self, cx: &mut Context<Self>) {
        let frames = self
            .debug
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| FrameRow {
                current: self.debug.frame == Some(i),
                ..f.clone()
            })
            .collect();
        let view = DebugView {
            status: self.debug.status.clone(),
            process: self.debug.process.clone(),
            frames,
            breakpoints: self
                .debug
                .breakpoints
                .all()
                .iter()
                .map(|b| BreakpointRow {
                    id: b.id,
                    file: b.file.clone(),
                    line: b.line,
                    state: b.state.clone(),
                })
                .collect(),
        };
        if let Some(panel) = &self.debug.panel {
            panel.update(cx, |panel, cx| panel.set_view(view, cx));
        }
        for editor in self.editors.clone() {
            self.mark_editor(&editor, cx);
        }
    }

    fn mark_editor(&self, editor: &Entity<EditorPanel>, cx: &mut Context<Self>) {
        let path = editor.read(cx).path().to_path_buf();
        let lines = self.debug.breakpoints.lines_in(&path);
        let stop = self
            .debug
            .frame
            .and_then(|i| self.debug.frames.get(i)?.source.clone())
            .filter(|(file, _)| file.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
            .map(|(_, line)| line);
        editor.update(cx, |editor, cx| editor.set_debug_marks(&lines, stop, cx));
    }

    /// Debug when detached, Detach when attached.
    fn render_debug_button(&self, cx: &mut Context<Self>) -> Button {
        let button = Button::new("debug")
            .small()
            .h(TOOLBAR_HEIGHT)
            .outline()
            .icon(Icon::new(IconName::Bug));
        match self.debug.status {
            DebugStatus::Detached => button
                .label("Debug")
                .tooltip("Attach to this project's editor, or launch it under the debugger (F5)")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_debugging(DebugStart::Auto, window, cx)
                })),
            DebugStatus::Attaching => button.label("Starting…").disabled(true),
            DebugStatus::Running | DebugStatus::Stopped(_) => button
                .label("Detach")
                .tooltip("Detach, leaving the editor running (Shift+F5)")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.detach_debugger(&DetachDebugger, window, cx)
                })),
        }
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.primary.opacity(0.6))
                            .child(theme::tracked("Telesto Games")),
                    )
                    .child(
                        div()
                            .text_3xl()
                            .text_color(theme.primary)
                            .child("TETHYS"),
                    )
                    .child(div().h(px(1.)).w_full().bg(theme.primary.opacity(0.2)))
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(theme::tracked("Recent projects")),
                    )
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
        let debug_button = self.render_debug_button(cx);
        let theme = cx.theme();
        let (engine_chip, engine_error) = match &project.engine {
            Ok(path) => (association_label(project), path.display().to_string()),
            Err(e) => (association_label(project), e.clone()),
        };
        let engine_ok = project.engine.is_ok();
        let enabled_plugins = project.plugins.iter().filter(|(_, on)| *on).count();
        let divider = || div().w_px().h(px(20.)).mx_1().bg(theme.border);

        let identity = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(project.name().to_string()),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .rounded(theme.radius)
                            .text_xs()
                            .bg(if engine_ok {
                                theme.primary.opacity(0.15)
                            } else {
                                theme.danger.opacity(0.15)
                            })
                            .text_color(if engine_ok {
                                theme.primary
                            } else {
                                theme.danger
                            })
                            .child(engine_chip),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if engine_ok {
                        theme.muted_foreground
                    } else {
                        theme.danger
                    })
                    .truncate()
                    .child(format!(
                        "{} · engine {} · {} module(s), {} plugin(s)",
                        project.root().display(),
                        engine_error,
                        project.modules.len(),
                        enabled_plugins
                    )),
            );

        // Every header control is TOOLBAR_HEIGHT tall with small-size labels.
        let configuration = div()
            .flex()
            .items_center()
            .flex_none()
            .h(TOOLBAR_HEIGHT)
            .gap_0p5()
            .p_0p5()
            .rounded(theme.radius)
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .children(Configuration::ALL.map(|configuration| {
                let button =
                    Button::new(configuration.as_str())
                        .small()
                        // Fill the pill: its height minus border and padding.
                        .h(TOOLBAR_HEIGHT - px(6.))
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
            }));

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2p5()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(identity)
            .child(configuration)
            .child(
                Button::new("build")
                    .small()
                    .h(TOOLBAR_HEIGHT)
                    .outline()
                    .icon(Icon::new(IconName::Hammer))
                    .label("Build")
                    .tooltip("Build (Ctrl+Shift+B)")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.build(&BuildProject, window, cx)),
                    ),
            )
            .child(
                Button::new("live-coding")
                    .small()
                    .h(TOOLBAR_HEIGHT)
                    .outline()
                    .icon(Icon::new(IconName::Zap))
                    .label("Live Coding")
                    .tooltip("Recompile C++ into the running editor (Ctrl+Alt+F11)")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.live_code(&LiveCode, window, cx)),
                    ),
            )
            .child(
                Button::new("launch-editor")
                    .small()
                    .h(TOOLBAR_HEIGHT)
                    .outline()
                    .icon(Icon::new(IconName::Play))
                    .label("Launch editor")
                    .tooltip("Launch editor (Ctrl+Shift+E)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.launch_editor(&LaunchEditor, window, cx)
                    })),
            )
            .child(debug_button)
            .child(divider())
            .child(
                Button::new("new-session")
                    .small()
                    .h(TOOLBAR_HEIGHT)
                    .primary()
                    .icon(Icon::new(IconName::Plus))
                    .label("New session")
                    .tooltip(
                        "Pick an agent or a plain terminal (Ctrl+Shift+T starts the first agent)",
                    )
                    .dropdown_menu({
                        let this = cx.entity().downgrade();
                        let names: Vec<String> =
                            self.profiles.iter().map(|p| p.name.clone()).collect();
                        move |menu, _, _| {
                            let menu = names.iter().enumerate().fold(menu, |menu, (i, name)| {
                                let this = this.clone();
                                menu.item(PopupMenuItem::new(name.clone()).on_click(
                                    move |_, window, cx| {
                                        let _ = this
                                            .update(cx, |this, cx| this.start_agent(i, window, cx));
                                    },
                                ))
                            });
                            let this = this.clone();
                            menu.separator().item(
                                PopupMenuItem::new("Terminal (Ctrl+Shift+`)").on_click(
                                    move |_, window, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            this.new_terminal(&NewTerminal, window, cx)
                                        });
                                    },
                                ),
                            )
                        }
                    }),
            );

        // Always shown: the build pane lives in the dock even with no sessions.
        // Drag a tab to an edge of a pane to split, or onto another tab bar to move it.
        let body = div().flex_1().min_h_0().child(self.dock.clone());

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
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::close_session))
            .on_action(cx.listener(Self::next_session))
            .on_action(cx.listener(Self::prev_session))
            .on_action(cx.listener(Self::build))
            .on_action(cx.listener(Self::live_code))
            .on_action(cx.listener(Self::launch_editor))
            .on_action(cx.listener(Self::show_projects))
            .on_action(cx.listener(Self::about))
            .on_action(cx.listener(|_, _: &OpenSettings, window, cx| settings_ui::open(window, cx)))
            .on_action(cx.listener(Self::check_for_updates))
            .on_action(cx.listener(Self::attach_or_continue))
            .on_action(cx.listener(Self::attach_to_editor))
            .on_action(cx.listener(Self::launch_with_debugger))
            .on_action(cx.listener(Self::pause_debugger))
            .on_action(cx.listener(Self::detach_debugger))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().iter().find(|p| is_uproject(p)) {
                    this.open(path.clone(), window, cx);
                }
            }))
            // The Telesto top hairline: one line of broadcast orange.
            .child(
                div()
                    .h(px(1.))
                    .w_full()
                    .flex_none()
                    .bg(cx.theme().primary.opacity(0.6)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_2()
                    .bg(cx.theme().title_bar)
                    .child(self.menu_bar.clone()),
            )
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
