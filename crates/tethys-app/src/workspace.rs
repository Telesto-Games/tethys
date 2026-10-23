//! One window: a project summary plus its agent sessions as tabs.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::assets::IconName;
use gpui_kit::base::{Disableable, GlobalState};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::dock::{
    DockArea, DockEvent, DockPlacement, InsertTarget, NodeId, PaneRef, Panel, PanelHandle, PanelId,
    PanelStyle,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{AppMenuBar, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
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
use tethys_adapters::udp_messagebus::runner::BusTestRunner;
use tethys_adapters::update_github::GitHubReleases;
use tethys_core::automation::{TestEnd, TestEvent};
use tethys_core::debug::{self as dbg, Breakpoints, DebugEnd, DebugEvent, StackFrame, StopReason};
use tethys_core::diff::FileDiff;
use tethys_core::ports::{
    AgentSession, ConfigStore, DebugEventSink, DebugSession, Debugger, EventSink, LiveCoding,
    SelfInstaller, SessionEvent, SourceControl, TestEventSink, TestRunner, TestSession,
    UpdateSource,
};
use tethys_core::unreal::{
    BuildAction, BuildConfig, BuildState, Configuration, Freshness, LaunchMode,
};
use tethys_core::usecases::{DebugStart, RecentProject, ScmSummary, TestTarget};
use tethys_core::{AgentProfile, AssociationKind, Project, SessionId, unreal, usecases};

use crate::build_placeholder::BuildPlaceholder;
use crate::debug_panel::{
    BreakpointRow, DebugPanel, DebugPanelEvent, DebugStatus, DebugView, FrameRow,
};
use crate::diff_panel::DiffPanel;
use crate::editor_panel::{self, EditorEvent, EditorPanel};
use crate::file_tree::{FileTreeEvent, FileTreePanel, file_name};
use crate::history_panel::HistoryPanel;
use crate::session_panel::{SessionKind, SessionPanel};
use crate::settings_ui;
use crate::tab_skin::{self, NewSessionAt};
use crate::tests_panel::{TestsPanel, TestsPanelEvent};
use crate::theme;
use crate::themes_ui;
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
        OpenFolder,
        NewSession,
        NewTerminal,
        CloseSession,
        NextSession,
        PrevSession,
        BuildProject,
        RebuildProject,
        LiveCode,
        LaunchEditor,
        ShowProjects,
        OpenSettings,
        OpenThemes,
        Exit,
        About,
        CheckForUpdates,
        AttachOrContinue,
        AttachToEditor,
        LaunchWithDebugger,
        PauseDebugger,
        DetachDebugger,
        ShowTests,
        ConnectTestsToEditor,
        StartTestEditor
    ]
);

/// The menu bar: File, Edit, Build, Debug, Test and Help. Rendered in-window
/// by `AppMenuBar`.
pub fn install_menus(cx: &mut App) {
    let menus = vec![
        Menu::new("File")
            .items([
                MenuItem::action("Open Project…", OpenProject),
                MenuItem::action("Open Folder…", OpenFolder),
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
        Menu::new("Edit")
            .items([MenuItem::action("Themes…", OpenThemes)])
            .owned(),
        Menu::new("Build")
            .items([
                MenuItem::action("Build", BuildProject),
                MenuItem::action("Rebuild", RebuildProject),
                MenuItem::action("Live Coding (Ctrl+Alt+F11)", LiveCode),
                MenuItem::action("Launch", LaunchEditor),
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
        Menu::new("Test")
            .items([
                MenuItem::action("Show Tests", ShowTests),
                MenuItem::separator(),
                MenuItem::action("Connect to This Project's Editor", ConnectTestsToEditor),
                MenuItem::action("Start a Test Editor (Headless)", StartTestEditor),
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
    pub tests: Arc<dyn TestRunner>,
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
            tests: Arc::new(BusTestRunner),
            next_session: AtomicU64::new(1),
        }
    }

    fn next_session_id(&self) -> SessionId {
        SessionId(self.next_session.fetch_add(1, Ordering::Relaxed))
    }
}

impl Global for Services {}

/// Opens a Tethys window, optionally loading a `.uproject` or folder into it.
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
    /// The main area group the next session goes in, when started from that
    /// group's new session button; otherwise the first group.
    session_group: Option<NodeId>,
    /// What Build builds; saved when changed.
    build: BuildConfig,
    /// The editor configuration for Launch, Debug and the test editor: the
    /// last Editor build configuration chosen.
    configuration: Configuration,
    /// What Launch starts; saved when changed.
    launch_mode: LaunchMode,
    /// When each configuration was last built, against the source; `None`
    /// until the first scan, and while a scan follows a finished build.
    build_state: Option<BuildState>,
    /// Numbers the scans, so a slow one can't overwrite a newer one.
    build_scan: u64,
    /// The build that is running, and what follows it.
    build_run: Option<BuildRun>,
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
    histories: Vec<Entity<HistoryPanel>>,
    /// Submits the open name prompt on Enter.
    name_prompt: Option<Subscription>,
    /// The source control managing the project folder, if any.
    scm: Option<Arc<dyn SourceControl>>,
    subscriptions: Vec<Subscription>,
    debug: Debug,
    tests: Tests,
}

/// The automation-test side of a window.
#[derive(Default)]
struct Tests {
    session: Option<Box<dyn TestSession>>,
    panel: Option<Entity<TestsPanel>>,
    /// Tags events, so a late event from an earlier session is ignored.
    generation: u64,
    events: Option<UnboundedSender<(u64, TestEvent)>>,
}

/// A running build: which configuration, and what to do once it succeeds.
struct BuildRun {
    id: SessionId,
    config: BuildConfig,
    then: Option<AfterBuild>,
}

/// What asked for a build because its configuration was out of date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterBuild {
    Launch,
    Debug(DebugStart),
}

impl AfterBuild {
    fn label(self) -> &'static str {
        match self {
            AfterBuild::Launch => "Launch",
            AfterBuild::Debug(_) => "Debug",
        }
    }
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

/// What the window must do after [`Debug::apply`] took an event.
#[derive(Debug, PartialEq)]
enum DebugEffect {
    /// The event was from an earlier session; nothing changed.
    Ignored,
    None,
    /// Show this frame of the stop in the editor.
    OpenFrame(usize),
    /// Stopped with no source to show: bring up the Debug pane.
    ShowPanel,
    /// Tell the user the debugger failed.
    Error(String),
}

impl Debug {
    /// Takes an event from the session tagged `generation`, updating the
    /// debug state. `engine` and `exists` map frame sources to this machine.
    fn apply(
        &mut self,
        generation: u64,
        event: DebugEvent,
        engine: Option<&Path>,
        exists: impl Fn(&Path) -> bool,
    ) -> DebugEffect {
        if generation != self.generation {
            return DebugEffect::Ignored;
        }
        match event {
            DebugEvent::Attached => DebugEffect::None,
            DebugEvent::Running => {
                self.status = DebugStatus::Running;
                self.frames.clear();
                self.frame = None;
                DebugEffect::None
            }
            DebugEvent::Stopped { reason, frames, .. } => {
                self.status = DebugStatus::Stopped(describe_stop(&reason, &self.breakpoints));
                self.frames = frame_rows(frames, engine, exists);
                // Show the innermost frame we have the source for.
                match innermost_with_source(&self.frames) {
                    Some(i) => DebugEffect::OpenFrame(i),
                    None => DebugEffect::ShowPanel,
                }
            }
            DebugEvent::Breakpoints(states) => {
                self.breakpoints.set_states(&states);
                DebugEffect::None
            }
            DebugEvent::Ended(end) => {
                self.session = None;
                self.status = DebugStatus::Detached;
                self.frames.clear();
                self.frame = None;
                self.breakpoints.reset_states();
                self.process = None;
                match end {
                    DebugEnd::Detached => DebugEffect::None,
                    DebugEnd::Exited => {
                        self.process = Some("The editor exited.".into());
                        DebugEffect::None
                    }
                    DebugEnd::Failed(e) => DebugEffect::Error(format!("The debugger stopped: {e}")),
                }
            }
        }
    }
}

/// Why the program stopped, naming the breakpoint when it was one of ours.
fn describe_stop(reason: &StopReason, breakpoints: &Breakpoints) -> String {
    let StopReason::Breakpoint(id) = reason else {
        return reason.describe();
    };
    match breakpoints.all().iter().find(|b| b.id == *id) {
        Some(b) => {
            let name = b.file.file_name().unwrap_or_default().to_string_lossy();
            format!("Breakpoint at {name}:{}", b.line)
        }
        None => reason.describe(),
    }
}

/// Stack frames for display, with sources mapped to this machine: a frame
/// gets a source only if its file (or its engine-relative twin under
/// `engine`) exists.
fn frame_rows(
    frames: Vec<StackFrame>,
    engine: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Vec<FrameRow> {
    frames
        .into_iter()
        .map(|frame| {
            let located = frame.file.zip(frame.line);
            let location = located.as_ref().map(|(file, line)| {
                let name = file.file_name().unwrap_or_default().to_string_lossy();
                format!("{name}:{line}")
            });
            let source = located.and_then(|(file, line)| {
                let file = match engine {
                    Some(root) => dbg::local_source(&file, root, &exists),
                    None => file,
                };
                exists(&file).then_some((file, line))
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

/// The innermost frame with source on this machine: the one to show first.
fn innermost_with_source(frames: &[FrameRow]) -> Option<usize> {
    frames.iter().position(|f| f.source.is_some())
}

/// The line to mark as the stop in the editor of `path`: the shown frame's,
/// if that frame's source is this file (paths compared without case).
fn stop_line(frames: &[FrameRow], frame: Option<usize>, path: &Path) -> Option<u32> {
    frame
        .and_then(|i| frames.get(i)?.source.as_ref())
        .filter(|(file, _)| file.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        .map(|(_, line)| *line)
}

impl Workspace {
    fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let events = cx.spawn_in(window, async move |this, cx| {
            while let Some((id, event)) = rx.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.on_session_event(id, event, window, cx)
                    })
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

        let (tests_tx, mut tests_rx) = futures::channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while let Some((generation, event)) = tests_rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_test_event(generation, event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let new_session: tab_skin::NewSessionButton = {
            let this = cx.entity().downgrade();
            Rc::new(move |at, _, _| new_session_button(this.clone(), at))
        };
        let (dock, skin) = tab_skin::dock_area("sessions", new_session, window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);
        skin.set_close_button_visible(true, cx);
        let dock_events =
            cx.subscribe_in(&dock, window, |this, _, event: &DockEvent, window, cx| {
                if let DockEvent::LayoutChanged = event {
                    this.prune_closed_panels(cx);
                    this.collapse_empty_docks(window, cx);
                    this.ensure_build_pane(window, cx);
                }
            });

        let services = cx.global::<Services>();
        let recent = usecases::recent_projects(&services.config, &services.locator);
        let (profiles, problems) = usecases::agent_profiles(&services.config);
        let configuration = services.config.build_configuration().unwrap_or_default();
        let build = services
            .config
            .build_config()
            .unwrap_or(BuildConfig::editor(configuration));
        let launch_mode = services.config.launch_mode().unwrap_or_default();

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
                // Disconnecting stops a test editor Tethys started.
                this.tests.session = None;
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
            session_group: None,
            build,
            configuration,
            launch_mode,
            build_state: None,
            build_scan: 0,
            build_run: None,
            events: tx,
            focus,
            _events: events,
            _dock_events: dock_events,
            menu_bar: AppMenuBar::new(cx),
            build_placeholder: None,
            file_tree: None,
            editors: Vec::new(),
            diffs: Vec::new(),
            histories: Vec::new(),
            name_prompt: None,
            scm: None,
            subscriptions: Vec::new(),
            debug: Debug {
                events: Some(debug_tx),
                ..Debug::default()
            },
            tests: Tests {
                events: Some(tests_tx),
                ..Tests::default()
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
                self.refresh_build_state(cx);
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

    fn prompt_open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Open folder")
            .pick_folder();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(folder) = dialog.await {
                let path = folder.path().to_path_buf();
                let _ = this.update_in(cx, |this, window, cx| this.open(path, window, cx));
            }
        })
        .detach();
    }

    /// The open project, if it's an Unreal one: builds, the editor and the
    /// debugger need it.
    fn unreal_project(&self) -> Option<&Project> {
        self.project.as_ref().filter(|p| p.is_unreal())
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
        let shell = agent_terminal::default_shell();
        self.start_tab(
            "Terminal".into(),
            SessionKind::Terminal,
            window,
            cx,
            move |host, id, project, sink| {
                usecases::start_terminal(&[host], id, shell, project, sink)
                    .map_err(|e| e.to_string())
            },
        );
    }

    fn build(&mut self, _: &BuildProject, window: &mut Window, cx: &mut Context<Self>) {
        self.start_build(BuildAction::Build, self.build, None, window, cx);
    }

    fn rebuild(&mut self, _: &RebuildProject, window: &mut Window, cx: &mut Context<Self>) {
        self.start_build(BuildAction::Rebuild, self.build, None, window, cx);
    }

    /// Builds `config` in the build pane; `then` runs when it succeeds.
    fn start_build(
        &mut self,
        action: BuildAction,
        config: BuildConfig,
        then: Option<AfterBuild>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.unreal_project() else {
            return;
        };
        // One build at a time: focus the running one instead. A launch
        // waits for it when it's building the same configuration.
        if let Some(i) = self.panels.iter().position(|p| {
            let p = p.read(cx);
            p.kind() == SessionKind::Build && p.is_running()
        }) {
            match (&mut self.build_run, then) {
                (Some(run), Some(then)) if run.config == config => run.then = Some(then),
                (_, Some(then)) => {
                    self.error = Some(format!(
                        "A build is already running. {} again when it finishes.",
                        then.label()
                    ));
                }
                (_, None) => {}
            }
            self.activate(i, window, cx);
            return;
        }
        let target = unreal::build_target(project, config)
            .unwrap_or_else(|_| config.target.as_str().to_string());
        let label = format!(
            "{} {target} {}",
            action.as_str(),
            config.configuration.as_str()
        );
        let id = self.start_tab(
            label,
            SessionKind::Build,
            window,
            cx,
            |host, id, project, sink| {
                usecases::start_build(&[host], id, project, config, action, sink)
                    .map_err(|e| e.to_string())
            },
        );
        if let Some(id) = id {
            self.build_run = Some(BuildRun { id, config, then });
        }
    }

    /// A build whose tab was closed is over as far as the toolbar and any
    /// launch waiting on it are concerned.
    fn forget_closed_build(&mut self, cx: &App) {
        if let Some(run) = &self.build_run
            && !self.panels.iter().any(|p| p.read(cx).id() == run.id)
        {
            self.build_run = None;
        }
    }

    /// Whether `config` must be built before it's run: its last build is
    /// older than the source, or there is none. Unknown counts as current,
    /// so a launch isn't held up by a scan.
    fn needs_build(&self, config: BuildConfig) -> bool {
        self.build_state
            .as_ref()
            .and_then(|state| state.freshness(config))
            .is_some_and(|f| !f.is_current())
    }

    /// Rescans the source and receipts in the background.
    fn refresh_build_state(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.unreal_project().cloned() else {
            return;
        };
        self.build_scan += 1;
        let scan = self.build_scan;
        let scanned = cx
            .background_executor()
            .spawn(async move { BuildState::scan(&project) });
        cx.spawn(async move |this, cx| {
            let state = scanned.await;
            let _ = this.update(cx, |this, cx| {
                if this.build_scan == scan {
                    this.build_state = Some(state);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The build finished: rescan, then do what waited for it.
    fn on_build_finished(
        &mut self,
        run: BuildRun,
        code: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Unknown until the scan lands, so what follows doesn't build again.
        self.build_state = None;
        self.refresh_build_state(cx);
        let Some(then) = run.then else {
            return;
        };
        if code != 0 {
            self.error = Some(format!("The build failed, so {} didn't run.", then.label()));
            cx.notify();
            return;
        }
        match then {
            AfterBuild::Launch => self.launch_editor(&LaunchEditor, window, cx),
            AfterBuild::Debug(how) => self.start_debugging(how, window, cx),
        }
    }

    /// Asks the running editor to Live Code the changed C++.
    fn live_code(&mut self, _: &LiveCode, window: &mut Window, cx: &mut Context<Self>) {
        if self.unreal_project().is_none() {
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

    /// Chooses what Build builds. An Editor configuration also becomes the
    /// one Launch, Debug and the test editor use.
    fn set_build_config(&mut self, config: BuildConfig, cx: &mut Context<Self>) {
        self.build = config;
        let config_store = &cx.global::<Services>().config;
        let mut saved = config_store.set_build_config(config);
        if config.is_editor() {
            self.configuration = config.configuration;
            saved = saved.and_then(|()| config_store.set_build_configuration(config.configuration));
        }
        if let Err(e) = saved {
            self.error = Some(format!("can't save build configuration: {e}"));
        }
        cx.notify();
    }

    /// Launches in `mode`, and makes it what Launch starts from now on.
    fn launch_in(&mut self, mode: LaunchMode, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_mode = mode;
        if let Err(e) = cx.global::<Services>().config.set_launch_mode(mode) {
            self.error = Some(format!("can't save launch mode: {e}"));
        }
        self.launch_editor(&LaunchEditor, window, cx);
    }

    fn launch_editor(&mut self, _: &LaunchEditor, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.unreal_project() else {
            return;
        };
        if !self.build.is_editor() {
            self.error = Some(format!(
                "Launch runs Editor configurations. {} is build-only for now: running it needs \
                 its content cooked. Pick an Editor configuration to launch.",
                self.build.label()
            ));
            cx.notify();
            return;
        }
        let editor = BuildConfig::editor(self.configuration);
        if self.needs_build(editor) {
            self.start_build(
                BuildAction::Build,
                editor,
                Some(AfterBuild::Launch),
                window,
                cx,
            );
            return;
        }
        let args = settings_ui::editor_args(cx);
        let launched = usecases::launch_editor(
            project,
            self.configuration,
            self.launch_mode,
            &args,
            &DetachedLauncher,
        );
        if let Err(e) = launched {
            self.error = Some(e.to_string());
        }
        cx.notify();
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
    ) -> Option<SessionId> {
        let group = self.session_group.take();
        let project = self.project.as_ref()?;
        let services = cx.global::<Services>();
        let host = services.host.clone();
        let id = services.next_session_id();
        let tx = self.events.clone();
        let sink: EventSink = Arc::new(move |id, event| {
            let _ = tx.unbounded_send((id, event));
        });

        let started = match start(&host, id, project, sink) {
            Ok(session) => {
                let Some(handle) = host.take_handle(id) else {
                    self.error = Some("terminal host lost the session".into());
                    return None;
                };
                let panel = cx.new(|cx| SessionPanel::new(id, kind, label, session, handle, cx));
                // Agents and terminals go in the main area; builds go in the
                // build pane on the right, or wherever its tab was dragged.
                let home = match kind {
                    SessionKind::Build => self.build_tab_home(cx),
                    SessionKind::Agent | SessionKind::Terminal => None,
                };
                let placement = match (kind, home) {
                    (_, Some((placement, ..))) => placement,
                    (SessionKind::Agent | SessionKind::Terminal, None) => DockPlacement::Center,
                    (SessionKind::Build, None) => DockPlacement::Right,
                };
                self.add_dock_panel(&panel, placement, Some(BUILD_PANE_WIDTH), window, cx);
                if let Some((_, node, ix)) = home {
                    // Takes the old tab's place before that tab goes, so its
                    // group survives.
                    let id = PanelId::from(panel.entity_id());
                    let target = InsertTarget::Tabs {
                        node,
                        ix: Some(ix),
                        activate: true,
                    };
                    self.dock
                        .update(cx, |dock, cx| dock.move_panel(id, target, window, cx));
                }
                if kind == SessionKind::Build {
                    self.close_finished_builds(window, cx);
                }
                // The dock adds to the first group; move it to the one asked for.
                if let Some(node) =
                    group.filter(|_| home.is_none() && placement == DockPlacement::Center)
                {
                    let id = PanelId::from(panel.entity_id());
                    let target = InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    };
                    self.dock
                        .update(cx, |dock, cx| dock.move_panel(id, target, window, cx));
                }
                self.panels.push(panel);
                if kind == SessionKind::Build
                    && let Some(placeholder) = self.build_placeholder.take()
                {
                    self.dock
                        .update(cx, |dock, cx| dock.remove_panel(placeholder, window, cx));
                }
                self.activate(self.panels.len() - 1, window, cx);
                Some(id)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        };
        cx.notify();
        started
    }

    /// Where the build tab, or the placeholder standing in for it, sits: its
    /// region, tab group and index. A new build replaces it there.
    fn build_tab_home(&self, cx: &App) -> Option<(DockPlacement, NodeId, usize)> {
        let id = self
            .panels
            .iter()
            .find(|p| p.read(cx).kind() == SessionKind::Build)
            .map(|p| p.entity_id())
            .or_else(|| self.build_placeholder.as_ref().map(|p| p.entity_id()))?;
        let id = PanelId::from(id);
        let dock = self.dock.read(cx);
        [
            DockPlacement::Right,
            DockPlacement::Bottom,
            DockPlacement::Left,
            DockPlacement::Center,
        ]
        .into_iter()
        .find_map(|placement| {
            let tree = dock.layout(placement)?;
            let node = tree.find_panel_node(id)?;
            let ix = match tree.find_node(node)?.kind() {
                PaneRef::Tabs { panels, .. } => panels.iter().position(|held| *held == id)?,
                PaneRef::Split { .. } => return None,
            };
            Some((placement, node, ix))
        })
    }

    /// Keeps the build pane on the right: when it has no build tab, it shows
    /// the placeholder. Idempotent.
    fn ensure_build_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_build = self
            .panels
            .iter()
            .any(|p| p.read(cx).kind() == SessionKind::Build);
        if self.unreal_project().is_none() || has_build || self.build_placeholder.is_some() {
            return;
        }
        // The placeholder doesn't reopen a collapsed pane: nothing happened
        // that the user needs to see.
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

    /// Adds a panel to a side or bottom dock and makes sure it can be seen:
    /// a dock collapses once its last tab is dragged away, and adding to it
    /// doesn't open it again by itself.
    fn add_dock_panel<P: Panel>(
        &mut self,
        panel: &Entity<P>,
        placement: DockPlacement,
        size: Option<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                Arc::new(PanelHandle::new(panel.clone())),
                placement,
                size,
                window,
                cx,
            );
            if placement != DockPlacement::Center && !dock.is_dock_open(placement) {
                dock.toggle_dock(placement, window, cx);
            }
        });
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
        let events = cx.subscribe_in(
            &tree,
            window,
            |this, _, event: &FileTreeEvent, window, cx| {
                this.on_file_tree_event(event.clone(), window, cx)
            },
        );
        self.subscriptions.push(events);
        self.add_dock_panel(
            &tree,
            DockPlacement::Left,
            Some(FILES_PANE_WIDTH),
            window,
            cx,
        );
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

    fn on_file_tree_event(
        &mut self,
        event: FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open(path) => self.open_file(path, window, cx),
            FileTreeEvent::Diff(path) => self.show_diff(path, None, window, cx),
            FileTreeEvent::NewFile(dir) => self.prompt_for_name(
                "New File",
                "Create",
                "",
                window,
                cx,
                move |name| usecases::create_file(&dir, name),
                |this, path, window, cx| {
                    this.reveal_in_files(&path, cx);
                    this.open_file(path, window, cx);
                },
            ),
            FileTreeEvent::NewFolder(dir) => self.prompt_for_name(
                "New Folder",
                "Create",
                "",
                window,
                cx,
                move |name| usecases::create_folder(&dir, name),
                |this, path, _, cx| this.reveal_in_files(&path, cx),
            ),
            FileTreeEvent::Rename(path) => {
                let name = file_name(&path);
                let scm = self.scm.clone();
                let from = path.clone();
                self.prompt_for_name(
                    "Rename",
                    "Rename",
                    &name,
                    window,
                    cx,
                    move |name| usecases::rename_path(scm.as_deref(), &path, name),
                    move |this, to, window, cx| this.moved(&from, &to, window, cx),
                )
            }
            FileTreeEvent::Delete(path) => self.confirm_delete(path, window, cx),
            FileTreeEvent::History(path) => self.show_history(path, window, cx),
        }
    }

    /// Asks for a name in a dialog. On OK or Enter, `work` gets the name on
    /// a background thread (it may run source control, which can take a
    /// while on a big folder). An error it returns is shown in the dialog,
    /// which stays open to try again; otherwise `done` gets its result and
    /// the dialog closes.
    #[allow(clippy::too_many_arguments)]
    fn prompt_for_name<T: Send + 'static>(
        &mut self,
        title: &'static str,
        ok: &'static str,
        initial: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl Fn(&str) -> Result<T, String> + Send + Sync + 'static,
        done: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.to_string()));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            // Typing replaces the name.
            input.select_all(window, cx);
        });
        let error: Rc<RefCell<Option<String>>> = Rc::default();
        // Set while `work` runs, so a second Enter doesn't start it again.
        let busy: Rc<Cell<bool>> = Rc::default();
        let work = Arc::new(work);
        let done = Rc::new(done);
        let this = cx.entity().downgrade();
        let submit: Rc<WindowAction> = Rc::new({
            let (input, error, busy) = (input.clone(), error.clone(), busy.clone());
            move |window, cx| {
                if busy.get() {
                    return;
                }
                let Some(this) = this.upgrade() else {
                    return;
                };
                let name = input.read(cx).value().to_string();
                busy.set(true);
                let (work, done, error, busy) =
                    (work.clone(), done.clone(), error.clone(), busy.clone());
                this.update(cx, |_, cx| {
                    let result = cx.background_executor().spawn(async move { work(&name) });
                    cx.spawn_in(window, async move |this, cx| {
                        let result = result.await;
                        busy.set(false);
                        let _ = this.update_in(cx, |this, window, cx| match result {
                            Ok(value) => {
                                done(this, value, window, cx);
                                window.close_dialog(cx);
                            }
                            Err(e) => {
                                *error.borrow_mut() = Some(e);
                                window.refresh();
                            }
                        });
                    })
                    .detach();
                });
            }
        });
        self.name_prompt = Some(cx.subscribe_in(&input, window, {
            let submit = submit.clone();
            move |_, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    submit(window, cx);
                }
            }
        }));

        window.open_dialog(cx, move |dialog, _, cx| {
            let submit = submit.clone();
            dialog
                .title(title)
                .w(px(420.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(Input::new(&input))
                        .children(error.borrow().clone().map(|e| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .whitespace_normal()
                                .child(e)
                        })),
                )
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("name-cancel")
                                .small()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("name-ok")
                                .primary()
                                .small()
                                .label(ok)
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
    }

    /// Shows a new or renamed path in the Files panel, and refreshes status.
    fn reveal_in_files(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.reveal(path, cx));
        }
        self.refresh_status(cx);
    }

    /// Follows a rename: editors of anything moved now edit it at its new
    /// path. Diffs and histories of it are closed, being of the old path.
    fn moved(&mut self, from: &Path, to: &Path, window: &mut Window, cx: &mut Context<Self>) {
        for editor in &self.editors {
            let old = editor.read(cx).path().to_path_buf();
            if let Some(new) = remap_path(&old, from, to) {
                editor.update(cx, |editor, cx| editor.set_path(new, cx));
            }
        }
        self.close_views_under(from, false, window, cx);
        // Tab titles are drawn by the dock.
        self.dock.update(cx, |_, cx| cx.notify());
        self.reveal_in_files(to, cx);
    }

    /// Closes the diffs and histories of `path` or anything under it, and
    /// its editors too when `editors`.
    fn close_views_under(
        &mut self,
        path: &Path,
        editors: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        fn remove<P: Panel>(
            dock: &Entity<DockArea>,
            panels: &[Entity<P>],
            path_of: impl Fn(&P) -> &Path,
            path: &Path,
            window: &mut Window,
            cx: &mut App,
        ) {
            let doomed: Vec<_> = panels
                .iter()
                .filter(|p| path_of(p.read(cx)).starts_with(path))
                .cloned()
                .collect();
            for panel in doomed {
                dock.update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
            }
        }
        remove(&self.dock, &self.diffs, DiffPanel::path, path, window, cx);
        remove(
            &self.dock,
            &self.histories,
            HistoryPanel::path,
            path,
            window,
            cx,
        );
        if editors {
            remove(
                &self.dock,
                &self.editors,
                EditorPanel::path,
                path,
                window,
                cx,
            );
        }
    }

    /// Asks before deleting `path` for good.
    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (title, description) = delete_prompt(
            &file_name(&path),
            path.is_dir(),
            self.scm.as_ref().map(|scm| scm.name()),
        );
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, path) = (this.clone(), path.clone());
            alert
                .confirm()
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger),
                )
                .on_ok(move |_, window, cx| {
                    let path = path.clone();
                    let _ = this.update(cx, |this, cx| this.delete(path, window, cx));
                    true
                })
        });
    }

    /// Deletes `path` in the background, then closes its tabs.
    fn delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let scm = self.scm.clone();
        let target = path.clone();
        let deleted = cx
            .background_executor()
            .spawn(async move { usecases::delete_path(scm.as_deref(), &target) });
        cx.spawn_in(window, async move |this, cx| {
            let deleted = deleted.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match deleted {
                    // The tabs go only once the file has: a failed delete
                    // keeps an editor's unsaved edits.
                    Ok(()) => this.close_views_under(&path, true, window, cx),
                    Err(e) => this.error = Some(e),
                }
                this.refresh_files(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens `path`'s history, or brings its open history up to date.
    fn show_history(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scm) = self.scm.clone() else {
            self.error = Some(format!(
                "{} isn't under source control, so it has no history.",
                path.display()
            ));
            cx.notify();
            return;
        };
        if let Some(panel) = self
            .histories
            .iter()
            .find(|h| h.read(cx).path() == path)
            .cloned()
        {
            panel.update(cx, |panel, cx| panel.reload(cx));
            self.focus_panel(&panel, window, cx);
            return;
        }
        let panel = cx.new(|cx| HistoryPanel::new(path, scm, cx));
        self.add_center_panel(&panel, window, cx);
        self.histories.push(panel);
    }

    /// Re-reads the file tree from disk and reloads source-control status.
    fn refresh_files(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.refresh(cx));
        }
        self.refresh_status(cx);
        self.refresh_build_state(cx);
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
                EditorEvent::Saved => {
                    this.refresh_status(cx);
                    this.refresh_build_state(cx);
                }
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

    /// Collapses docks whose last tab was dragged away, so they leave a strip
    /// rather than a blank column. The dock stays, and a tab dropped on its
    /// strip opens it again.
    fn collapse_empty_docks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for placement in [
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ] {
            let empty = {
                let dock = self.dock.read(cx);
                dock.is_dock_open(placement) && dock.is_empty(placement, cx)
            };
            if empty {
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
            }
        }
    }

    /// Drops panels the dock no longer holds (closed from their tab).
    fn prune_closed_panels(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let held = |id: EntityId| dock.panel(PanelId::from(id)).is_some();
        let before = (self.panels.len(), self.editors.len(), self.diffs.len());
        self.panels.retain(|p| held(p.entity_id()));
        self.editors.retain(|p| held(p.entity_id()));
        self.diffs.retain(|p| held(p.entity_id()));
        self.histories.retain(|p| held(p.entity_id()));
        self.forget_closed_build(cx);
        if self
            .debug
            .panel
            .as_ref()
            .is_some_and(|p| !held(p.entity_id()))
        {
            self.debug.panel = None;
        }
        // The panel holds the results, so closing it ends the session too.
        if self
            .tests
            .panel
            .as_ref()
            .is_some_and(|p| !held(p.entity_id()))
        {
            self.tests.panel = None;
            self.tests.session = None;
            self.tests.generation += 1;
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
        let focused = self.focused_panel(window, cx);
        let Some(index) = close_target(focused, self.panels.len()) else {
            return;
        };
        let panel = self.panels.remove(index);
        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        self.forget_closed_build(cx);
        match after_close(index, self.panels.len()) {
            Some(next) => self.activate(next, window, cx),
            None => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    fn next_session(&mut self, _: &NextSession, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused_panel(window, cx);
        if let Some(next) = next_index(focused, self.panels.len()) {
            self.activate(next, window, cx);
        }
    }

    fn prev_session(&mut self, _: &PrevSession, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused_panel(window, cx);
        if let Some(prev) = prev_index(focused, self.panels.len()) {
            self.activate(prev, window, cx);
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

    fn on_session_event(
        &mut self,
        id: SessionId,
        event: SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The build's tab may be closed already; what waited for it still runs.
        if let SessionEvent::Exited(code) = event
            && self.build_run.as_ref().is_some_and(|run| run.id == id)
            && let Some(run) = self.build_run.take()
        {
            self.on_build_finished(run, code, window, cx);
        }
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

    /// F5 with the editor out of date: attach if it's running, otherwise
    /// build and launch it under the debugger.
    fn debug_after_finding_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.unreal_project().cloned() else {
            return;
        };
        self.debug.status = DebugStatus::Attaching;
        self.refresh_debug(cx);
        let debugger = cx.global::<Services>().debugger.clone();
        let found = cx
            .background_executor()
            .spawn(async move { usecases::editor_is_running(debugger.as_ref(), &project) });
        cx.spawn_in(window, async move |this, cx| {
            let found = found.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.debug.status = DebugStatus::Detached;
                match found {
                    Ok(true) => this.start_debugging(DebugStart::Attach, window, cx),
                    Ok(false) => this.start_build(
                        BuildAction::Build,
                        BuildConfig::editor(this.configuration),
                        Some(AfterBuild::Debug(DebugStart::Launch)),
                        window,
                        cx,
                    ),
                    Err(e) => this.error = Some(format!("Can't debug: {e}")),
                }
                this.refresh_debug(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Attaches to this project's running editor, or launches it under the
    /// debugger in the chosen configuration.
    fn start_debugging(&mut self, how: DebugStart, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(project), Some(tx)) = (self.unreal_project().cloned(), self.debug.events.clone())
        else {
            return;
        };
        if self.debug.status != DebugStatus::Detached {
            return;
        }
        let editor = BuildConfig::editor(self.configuration);
        if how != DebugStart::Attach && self.needs_build(editor) {
            match how {
                DebugStart::Launch => self.start_build(
                    BuildAction::Build,
                    editor,
                    Some(AfterBuild::Debug(DebugStart::Launch)),
                    window,
                    cx,
                ),
                // The running editor is attached to as it is; without one,
                // build, then launch.
                DebugStart::Auto => self.debug_after_finding_editor(window, cx),
                DebugStart::Attach => unreachable!(),
            }
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
        let engine = self.project.as_ref().and_then(|p| p.engine.clone().ok());
        let effect = self
            .debug
            .apply(generation, event, engine.as_deref(), |p| p.is_file());
        match effect {
            DebugEffect::Ignored => return,
            DebugEffect::None => {}
            DebugEffect::OpenFrame(i) => {
                window.activate_window();
                self.open_frame(i, window, cx);
            }
            DebugEffect::ShowPanel => {
                window.activate_window();
                self.show_debug_panel(window, cx);
            }
            DebugEffect::Error(e) => self.error = Some(e),
        }
        self.refresh_debug(cx);
        cx.notify();
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
        self.add_dock_panel(
            &panel,
            DockPlacement::Bottom,
            Some(DEBUG_PANE_HEIGHT),
            window,
            cx,
        );
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
        let stop = stop_line(&self.debug.frames, self.debug.frame, &path);
        editor.update(cx, |editor, cx| editor.set_debug_marks(&lines, stop, cx));
    }

    fn show_tests(&mut self, _: &ShowTests, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.show_tests_panel(window, cx) {
            self.focus_panel(&panel, window, cx);
        }
    }

    fn connect_tests_to_editor(
        &mut self,
        _: &ConnectTestsToEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect_tests(TestTarget::OpenEditor, window, cx);
    }

    fn start_test_editor(
        &mut self,
        _: &StartTestEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect_tests(TestTarget::TestEditor, window, cx);
    }

    /// Adds the Tests pane at the bottom, unless it's already there.
    fn show_tests_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TestsPanel>> {
        if let Some(panel) = &self.tests.panel {
            return Some(panel.clone());
        }
        let root = self.unreal_project()?.root().to_path_buf();
        let panel = cx.new(|cx| TestsPanel::new(root, window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event, window, cx| match event {
            TestsPanelEvent::Connect(target) => this.connect_tests(*target, window, cx),
            TestsPanelEvent::Disconnect => this.disconnect_tests(cx),
            TestsPanelEvent::Run(tests) => {
                if let Some(session) = &this.tests.session {
                    session.run(tests.clone());
                }
            }
            TestsPanelEvent::Stop => {
                if let Some(session) = &this.tests.session {
                    session.stop();
                }
            }
            TestsPanelEvent::Refresh => {
                if let Some(session) = &this.tests.session {
                    session.refresh();
                }
            }
            TestsPanelEvent::OpenSource(file, line) => {
                this.open_source(file.clone(), *line, window, cx)
            }
        });
        self.subscriptions.push(events);
        self.add_dock_panel(
            &panel,
            DockPlacement::Bottom,
            Some(DEBUG_PANE_HEIGHT),
            window,
            cx,
        );
        self.tests.panel = Some(panel.clone());
        Some(panel)
    }

    /// Connects the Tests pane to this project's editor, or to a headless
    /// test editor it starts.
    fn connect_tests(&mut self, target: TestTarget, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(project), Some(tx)) = (self.unreal_project().cloned(), self.tests.events.clone())
        else {
            return;
        };
        let Some(panel) = self.show_tests_panel(window, cx) else {
            return;
        };
        if self.tests.session.is_some() || !panel.read(cx).is_disconnected() {
            return;
        }
        self.tests.generation += 1;
        let generation = self.tests.generation;
        let sink: TestEventSink = Arc::new(move |event| {
            let _ = tx.unbounded_send((generation, event));
        });
        let status = match target {
            TestTarget::OpenEditor => "Looking for this project's editor…",
            TestTarget::TestEditor => "Starting the test editor…",
        };
        panel.update(cx, |panel, cx| panel.set_connecting(status.into(), cx));

        let services = cx.global::<Services>();
        let runner = services.tests.clone();
        let debugger = services.debugger.clone();
        let configuration = self.configuration;
        let args = settings_ui::test_editor_args(cx);
        // Finding editors listens on the bus for a few seconds.
        let start = cx.background_executor().spawn(async move {
            usecases::connect_tests(
                runner.as_ref(),
                || debugger.processes(),
                &project,
                configuration,
                &args,
                target,
                sink,
            )
            .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let started = start.await;
            let _ = this.update(cx, |this, cx| {
                // Cancelled while connecting: dropping the session disconnects it.
                if this.tests.generation != generation {
                    return;
                }
                match started {
                    Ok((_, session)) => this.tests.session = Some(session),
                    Err(e) => {
                        if let Some(panel) = &this.tests.panel {
                            panel.update(cx, |panel, cx| panel.set_failed(e, cx));
                        }
                    }
                }
            });
        })
        .detach();
    }

    fn disconnect_tests(&mut self, cx: &mut Context<Self>) {
        self.tests.generation += 1;
        self.tests.session = None;
        if let Some(panel) = &self.tests.panel {
            panel.update(cx, |panel, cx| {
                panel.apply(TestEvent::Ended(TestEnd::Disconnected), cx)
            });
        }
    }

    fn on_test_event(&mut self, generation: u64, event: TestEvent, cx: &mut Context<Self>) {
        if generation != self.tests.generation {
            return;
        }
        if let TestEvent::Ended(_) = event {
            self.tests.session = None;
        }
        if let Some(panel) = &self.tests.panel {
            panel.update(cx, |panel, cx| panel.apply(event, cx));
        }
    }

    /// Launch, in the mode last chosen, joined to a menu of the other modes.
    /// Only Editor configurations launch; the others are build-only for now.
    fn render_launch_button(&self, cx: &mut Context<Self>) -> Div {
        let disabled = !self.build.is_editor();
        let tooltip = if disabled {
            format!(
                "{} is build-only: running it needs its content cooked. Pick an Editor \
                 configuration to launch.",
                self.build.label()
            )
        } else {
            format!(
                "{} in {} (Ctrl+Shift+E)",
                self.launch_mode.action_label(),
                BuildConfig::editor(self.configuration).label()
            )
        };
        // Inside the border, so both halves fill the toolbar height together.
        let inner_height = TOOLBAR_HEIGHT - px(2.);
        let launch = Button::new("launch-editor")
            .small()
            .h(inner_height)
            .ghost()
            .icon(Icon::new(IconName::Play))
            .label(self.launch_mode.action_label())
            .tooltip(tooltip)
            .disabled(disabled)
            .on_click(
                cx.listener(|this, _, window, cx| this.launch_editor(&LaunchEditor, window, cx)),
            );
        let current = self.launch_mode;
        let modes = Button::new("launch-mode")
            .small()
            .h(inner_height)
            .ghost()
            .icon(Icon::new(IconName::ChevronDown))
            .tooltip("Launch the editor, the game, a server, or a server and a client")
            .disabled(disabled)
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu, _, _| {
                    LaunchMode::ALL.into_iter().fold(menu, |menu, mode| {
                        let this = this.clone();
                        menu.item(
                            PopupMenuItem::new(mode.as_str())
                                .checked(mode == current)
                                .on_click(move |_, window, cx| {
                                    let _ = this
                                        .update(cx, |this, cx| this.launch_in(mode, window, cx));
                                }),
                        )
                    })
                }
            });
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .flex_none()
            .h(TOOLBAR_HEIGHT)
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(launch)
            .child(div().w_px().h_full().bg(theme.border))
            .child(modes)
    }

    /// Debug when detached, Detach when attached: an icon for each, named by
    /// its tooltip.
    fn render_debug_button(&self, cx: &mut Context<Self>) -> Button {
        let button = Button::new("debug").small().size(TOOLBAR_HEIGHT).outline();
        match self.debug.status {
            DebugStatus::Detached => button
                .icon(Icon::new(IconName::Bug))
                .tooltip(format!(
                    "Debug: attach to this project's editor, or launch {} under the debugger (F5)",
                    BuildConfig::editor(self.configuration).label()
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_debugging(DebugStart::Auto, window, cx)
                })),
            DebugStatus::Attaching => button
                .icon(Icon::new(IconName::LoaderCircle))
                .tooltip("Starting the debugger…")
                .disabled(true),
            DebugStatus::Running | DebugStatus::Stopped(_) => button
                .icon(Icon::new(IconName::BugOff))
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
            let (name, detail, is_error) = recent_entry_text(entry);
            let detail_color = if is_error { danger } else { muted };
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
                            .child("Open a .uproject, or any folder, to start a Claude Code session in it. You can also drop one onto this window."),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("open")
                                    .primary()
                                    .label("Open .uproject…  (Ctrl+Shift+O)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.prompt_open(&OpenProject, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("open-folder")
                                    .outline()
                                    .label("Open folder…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.prompt_open_folder(&OpenFolder, window, cx)
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
        let launch_editor = self.render_launch_button(cx);
        let theme = cx.theme();
        let unreal = project.is_unreal();
        let engine_ok = !unreal || project.engine.is_ok();
        let (engine_chip, details) = if unreal {
            let engine = match &project.engine {
                Ok(path) => path.display().to_string(),
                Err(e) => e.clone(),
            };
            let enabled_plugins = project.plugins.iter().filter(|(_, on)| *on).count();
            (
                association_label(project),
                format!(
                    "{} · engine {} · {} module(s), {} plugin(s)",
                    project.root().display(),
                    engine,
                    project.modules.len(),
                    enabled_plugins
                ),
            )
        } else {
            ("Folder".to_string(), project.root().display().to_string())
        };

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
                    .child(details),
            );

        // Every header control is TOOLBAR_HEIGHT tall with small-size labels.
        let configuration = Button::new("build-config")
            .small()
            .h(TOOLBAR_HEIGHT)
            .outline()
            .label(self.build.label())
            .dropdown_caret(true)
            .tooltip(format!(
                "What Build builds. Launch, Debug and the test editor use the last Editor \
                 configuration ({}).",
                BuildConfig::editor(self.configuration).label()
            ))
            .dropdown_menu({
                let this = cx.entity().downgrade();
                let configs = unreal::build_configs(project);
                let installed = project.installed_engine;
                let current = self.build;
                move |menu, _, _| build_config_menu(menu, &configs, current, installed, &this)
            });

        // Icon only, square at the toolbar height; the tooltips name them.
        // Build and Rebuild carry a dot saying whether the chosen
        // configuration is up to date.
        let freshness = self
            .build_state
            .as_ref()
            .and_then(|state| state.freshness(self.build));
        let building = self.build_run.is_some();
        let status = build_status(freshness.as_ref(), building);
        // While building, the hammer taps and the rebuild arrows turn.
        let build_icon = |icon: IconName, ease: fn(f32) -> f32| {
            move |button: Button| {
                if building {
                    button.icon(Spinner::new().icon(icon).ease(ease))
                } else {
                    button.icon(Icon::new(icon))
                }
            }
        };
        let build = Button::new("build")
            .small()
            .size(TOOLBAR_HEIGHT)
            .outline()
            .map(build_icon(IconName::Hammer, hammer_tap))
            .tooltip(format!(
                "Build {} (Ctrl+Shift+B)\n{status}",
                self.build.label()
            ))
            .on_click(cx.listener(|this, _, window, cx| this.build(&BuildProject, window, cx)));
        let build = with_status_dot(build, freshness.as_ref(), building, cx);
        let rebuild = Button::new("rebuild")
            .small()
            .size(TOOLBAR_HEIGHT)
            .outline()
            .map(build_icon(IconName::RefreshCw, |t| t))
            .tooltip(format!(
                "Rebuild {}: clean, then build everything\n{status}",
                self.build.label()
            ))
            .on_click(cx.listener(|this, _, window, cx| this.rebuild(&RebuildProject, window, cx)));
        let rebuild = with_status_dot(rebuild, freshness.as_ref(), building, cx);
        let live_coding = Button::new("live-coding")
            .small()
            .size(TOOLBAR_HEIGHT)
            .outline()
            .icon(Icon::new(IconName::Zap))
            .tooltip("Live Coding: recompile C++ into the running editor (Ctrl+Alt+F11)")
            .on_click(cx.listener(|this, _, window, cx| this.live_code(&LiveCode, window, cx)));

        let tests = Button::new("tests")
            .small()
            .size(TOOLBAR_HEIGHT)
            .outline()
            .icon(Icon::new(IconName::FlaskConical))
            .tooltip("Tests: list and run this project's automation tests")
            .on_click(cx.listener(|this, _, window, cx| this.show_tests(&ShowTests, window, cx)));

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
            // Builds, the editor and the debugger need a .uproject.
            .when(unreal, |header| {
                header
                    .child(configuration)
                    .child(build)
                    .child(rebuild)
                    .child(launch_editor)
                    .child(debug_button)
                    .child(live_coding)
                    .child(tests)
            });

        // For a .uproject the build pane is always in the dock, even with no sessions.
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

/// The second line of Build's and Rebuild's tooltips: how the chosen
/// configuration's last build stands against the source.
fn build_status(freshness: Option<&Freshness>, building: bool) -> String {
    if building {
        return "Building…".into();
    }
    match freshness {
        None => "Not checked yet".into(),
        Some(Freshness::Current) => "Up to date: built after the last source change".into(),
        Some(Freshness::NeverBuilt) => "Never built".into(),
        Some(Freshness::Stale { changed }) => {
            let name = changed
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| changed.display().to_string());
            format!("Out of date: {name} changed after the last build")
        }
    }
}

/// The Build button's spinner easing, as a fraction of a turn: the hammer
/// lifts a tenth of a turn and strikes back down, once per cycle. Counted
/// back from a whole turn, since gpui only takes fractions from 0 to 1.
fn hammer_tap(t: f32) -> f32 {
    1. - 0.1 * (std::f32::consts::PI * t).sin().powi(2)
}

/// The dot in a build button's corner: green when the configuration is up
/// to date, amber when it needs building; none while that isn't known or a
/// build is running.
fn with_status_dot(button: Button, freshness: Option<&Freshness>, building: bool, cx: &App) -> Div {
    let color = match freshness {
        _ if building => None,
        None => None,
        Some(Freshness::Current) => Some(crate::tests_panel::passed_color()),
        Some(Freshness::NeverBuilt | Freshness::Stale { .. }) => {
            Some(crate::tests_panel::warning_color())
        }
    };
    let theme = cx.theme();
    div()
        .relative()
        .flex_none()
        .child(button)
        .when_some(color, |this, color| {
            this.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(2.))
                    .size(px(7.))
                    .rounded_full()
                    .bg(color)
                    .border_1()
                    .border_color(theme.title_bar),
            )
        })
}

/// The button that starts an agent or a terminal at `at` in the main area:
/// a + at the end of a tab bar, a row in the side tab list, or a labelled
/// button in an empty main area. The session goes in that group.
fn new_session_button(this: WeakEntity<Workspace>, at: NewSessionAt) -> AnyElement {
    let button = Button::new("new-session")
        .icon(Icon::new(IconName::Plus))
        .tab_stop(false)
        .tooltip(
            "New session: pick an agent or a plain terminal (Ctrl+Shift+T starts the first agent)",
        );
    let button = match at {
        NewSessionAt::TabBar(_) => button.xsmall().ghost(),
        NewSessionAt::SideList(_) => button.small().ghost().w_full().label("New session"),
        NewSessionAt::EmptyArea => button.primary().label("New session"),
    };
    let group = at.group();
    button
        .dropdown_menu(move |menu, _, cx| new_session_menu(menu, this.clone(), group, cx))
        .into_any_element()
}

/// The agent profiles, then a plain terminal; the session goes in `group`.
fn new_session_menu(
    menu: PopupMenu,
    this: WeakEntity<Workspace>,
    group: Option<NodeId>,
    cx: &App,
) -> PopupMenu {
    let names: Vec<String> = this
        .upgrade()
        .map(|workspace| {
            let profiles = &workspace.read(cx).profiles;
            profiles.iter().map(|p| p.name.clone()).collect()
        })
        .unwrap_or_default();
    let menu = names.into_iter().enumerate().fold(menu, |menu, (i, name)| {
        let this = this.clone();
        menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
            let _ = this.update(cx, |this, cx| {
                this.session_group = group;
                this.start_agent(i, window, cx)
            });
        }))
    });
    menu.separator()
        .item(
            PopupMenuItem::new("Terminal (Ctrl+Shift+`)").on_click(move |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.session_group = group;
                    this.new_terminal(&NewTerminal, window, cx)
                });
            }),
        )
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
            .on_action(cx.listener(Self::prompt_open_folder))
            .on_action(cx.listener(Self::new_session))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::close_session))
            .on_action(cx.listener(Self::next_session))
            .on_action(cx.listener(Self::prev_session))
            .on_action(cx.listener(Self::build))
            .on_action(cx.listener(Self::rebuild))
            .on_action(cx.listener(Self::live_code))
            .on_action(cx.listener(Self::launch_editor))
            .on_action(cx.listener(Self::show_projects))
            .on_action(cx.listener(Self::about))
            .on_action(cx.listener(|_, _: &OpenSettings, window, cx| settings_ui::open(window, cx)))
            .on_action(cx.listener(|_, _: &OpenThemes, window, cx| themes_ui::open(window, cx)))
            .on_action(cx.listener(Self::check_for_updates))
            .on_action(cx.listener(Self::attach_or_continue))
            .on_action(cx.listener(Self::attach_to_editor))
            .on_action(cx.listener(Self::launch_with_debugger))
            .on_action(cx.listener(Self::pause_debugger))
            .on_action(cx.listener(Self::detach_debugger))
            .on_action(cx.listener(Self::show_tests))
            .on_action(cx.listener(Self::connect_tests_to_editor))
            .on_action(cx.listener(Self::start_test_editor))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = droppable(paths.paths(), |p| p.is_dir()) {
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

/// Something done in a window, outside any one view.
type WindowAction = dyn Fn(&mut Window, &mut App);

fn is_uproject(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("uproject"))
}

/// The first dropped path Tethys can open: a `.uproject` or a folder.
fn droppable(paths: &[PathBuf], is_dir: impl Fn(&Path) -> bool) -> Option<&PathBuf> {
    paths.iter().find(|p| is_uproject(p) || is_dir(p))
}

/// Where `old` is after `from` was renamed to `to`, if it was under it.
fn remap_path(old: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = old.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

/// Which session Ctrl+Shift+W closes: the focused one, else the last.
fn close_target(focused: Option<usize>, count: usize) -> Option<usize> {
    focused.or(count.checked_sub(1))
}

/// Which session to show after closing the one at `closed`, or `None` when
/// none are left.
fn after_close(closed: usize, remaining: usize) -> Option<usize> {
    remaining.checked_sub(1).map(|last| closed.min(last))
}

/// The session after the focused one, wrapping; the first when none is focused.
fn next_index(focused: Option<usize>, count: usize) -> Option<usize> {
    (count > 0).then(|| focused.map_or(0, |i| i + 1) % count)
}

/// The session before the focused one, wrapping; the last when none is focused.
fn prev_index(focused: Option<usize>, count: usize) -> Option<usize> {
    (count > 0).then(|| focused.map_or(count - 1, |i| i + count - 1) % count)
}

/// The title and body of the "delete for good" prompt.
fn delete_prompt(name: &str, is_dir: bool, scm: Option<&str>) -> (String, String) {
    let what = if is_dir {
        "This folder and everything in it"
    } else {
        "This file"
    };
    let tracked = match scm {
        Some(scm) => {
            format!(" If it's under {scm}, the deletion is marked there, ready to commit.")
        }
        None => String::new(),
    };
    (
        format!("Delete {name}?"),
        format!(
            "{what} will be deleted permanently, including changes not yet saved or \
             committed.{tracked}"
        ),
    )
}

/// A recent project's name and detail line, and whether the detail is an
/// error (an engine that can't be found, or a project that won't open).
fn recent_entry_text(entry: &RecentProject) -> (String, String, bool) {
    let name = match &entry.project {
        Ok(project) => project.name().to_string(),
        Err(_) => entry
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let (detail, is_error) = match &entry.project {
        Ok(project) if !project.is_unreal() => ("Folder".to_string(), false),
        Ok(project) => match &project.engine {
            Ok(_) => (format!("Engine: {}", association_label(project)), false),
            Err(e) => (
                format!("Engine: {} — {e}", association_label(project)),
                true,
            ),
        },
        Err(e) => (e.clone(), true),
    };
    (name, detail, is_error)
}

/// The build configuration menu: the configurations in `configs` under a
/// heading per kind of target, `current` checked. With an installed engine,
/// those only a source-built engine can build are greyed and say so.
fn build_config_menu(
    menu: PopupMenu,
    configs: &[BuildConfig],
    current: BuildConfig,
    installed_engine: bool,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    let mut menu = menu;
    let mut kind = None;
    for &config in configs {
        if kind != Some(config.target) {
            if kind.is_some() {
                menu = menu.separator();
            }
            menu = menu.label(config.target.as_str());
            kind = Some(config.target);
        }
        let unavailable = installed_engine && config.needs_source_engine();
        let label = if unavailable {
            format!("{}  (needs a source-built engine)", config.label())
        } else {
            config.label()
        };
        let workspace = workspace.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .checked(config == current)
                .disabled(unavailable)
                .on_click(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| this.set_build_config(config, cx));
                }),
        );
    }
    menu
}

fn association_label(project: &Project) -> String {
    match project.association_kind() {
        AssociationKind::Native => "native (in-tree)".to_string(),
        AssociationKind::Launcher => format!("Launcher {}", project.engine_association),
        AssociationKind::SourceBuild => format!("source build {}", project.engine_association),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_status_names_the_changed_file() {
        assert_eq!(build_status(None, false), "Not checked yet");
        assert_eq!(build_status(Some(&Freshness::Current), true), "Building…");
        assert_eq!(
            build_status(Some(&Freshness::Current), false),
            "Up to date: built after the last source change"
        );
        assert_eq!(
            build_status(Some(&Freshness::NeverBuilt), false),
            "Never built"
        );
        let stale = Freshness::Stale {
            changed: PathBuf::from(r"D:\p\Source\Game\Pawn.cpp"),
        };
        assert_eq!(
            build_status(Some(&stale), false),
            "Out of date: Pawn.cpp changed after the last build"
        );
    }

    #[test]
    fn hammer_tap_stays_within_a_turn() {
        // `gpui::percentage` panics outside 0..=1.
        for i in 0..=100 {
            let turn = hammer_tap(i as f32 / 100.);
            assert!((0.0..=1.0).contains(&turn), "{turn} at step {i}");
        }
    }

    use std::path::{Path, PathBuf};

    use tethys_core::Project;
    use tethys_core::debug::{
        BreakpointId, BreakpointState, Breakpoints, DebugEnd, DebugEvent, StackFrame, StopReason,
    };
    use tethys_core::usecases::RecentProject;

    use super::{
        Debug, DebugEffect, DebugStatus, FrameRow, Freshness, after_close, association_label,
        build_status, close_target, delete_prompt, describe_stop, droppable, frame_rows,
        hammer_tap, innermost_with_source, is_uproject, key_bindings, next_index, prev_index,
        recent_entry_text, remap_path, stop_line,
    };

    fn unreal(association: &str) -> Project {
        let json = format!(r#"{{"EngineAssociation": "{association}", "Modules": []}}"#);
        Project::parse(r"D:\p\Game.uproject", &json).unwrap()
    }

    #[test]
    fn uproject_is_matched_by_extension_in_any_case() {
        assert!(is_uproject(Path::new(r"D:\p\Game.uproject")));
        assert!(is_uproject(Path::new(r"D:\p\Game.UPROJECT")));
        assert!(!is_uproject(Path::new(r"D:\p\Game.uplugin")));
        assert!(!is_uproject(Path::new(r"D:\p\uproject")));
    }

    #[test]
    fn association_labels_name_the_engine_kind() {
        assert_eq!(association_label(&unreal("5.6")), "Launcher 5.6");
        assert_eq!(
            association_label(&unreal("{1234-ABCD}")),
            "source build {1234-ABCD}"
        );
        assert_eq!(association_label(&unreal("")), "native (in-tree)");
    }

    #[test]
    fn every_key_binding_parses_and_no_keystroke_is_bound_twice() {
        // `KeyBinding::new` panics on a keystroke it can't parse, so building
        // the list is the parse check; then no two actions share a key.
        let bindings = key_bindings();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for binding in &bindings {
            let keystrokes: Vec<String> = binding
                .keystrokes()
                .iter()
                .map(|k| k.inner().unparse())
                .collect();
            assert!(!seen.contains(&keystrokes), "{keystrokes:?} is bound twice");
            seen.push(keystrokes);
        }
        assert!(bindings.len() >= 10);
    }

    #[test]
    fn dropping_takes_the_first_uproject_or_folder() {
        let paths = vec![
            PathBuf::from(r"D:\a\notes.txt"),
            PathBuf::from(r"D:\a\Game.uproject"),
            PathBuf::from(r"D:\b"),
        ];
        let is_dir = |p: &Path| p == Path::new(r"D:\b");
        assert_eq!(droppable(&paths, is_dir), Some(&paths[1]));
        assert_eq!(droppable(&paths[2..], is_dir), Some(&paths[2]));
        assert_eq!(droppable(&paths[..1], is_dir), None);
    }

    #[test]
    fn renames_remap_the_file_itself_and_anything_under_a_folder() {
        let from = Path::new(r"D:\p\Source\Old");
        let to = Path::new(r"D:\p\Source\New");
        assert_eq!(
            remap_path(Path::new(r"D:\p\Source\Old\A.cpp"), from, to),
            Some(PathBuf::from(r"D:\p\Source\New\A.cpp"))
        );
        assert_eq!(remap_path(from, from, to), Some(to.to_path_buf()));
        assert_eq!(
            remap_path(Path::new(r"D:\p\Source\Other.cpp"), from, to),
            None
        );
        // A sibling whose name merely starts with the old name is not under it.
        assert_eq!(
            remap_path(Path::new(r"D:\p\Source\Older\A.cpp"), from, to),
            None
        );
    }

    #[test]
    fn closing_takes_the_focused_session_or_the_last_one() {
        assert_eq!(close_target(Some(1), 3), Some(1));
        assert_eq!(close_target(None, 3), Some(2));
        assert_eq!(close_target(None, 0), None);
    }

    #[test]
    fn after_closing_the_same_slot_shows_unless_it_was_the_last() {
        assert_eq!(after_close(1, 2), Some(1));
        assert_eq!(after_close(2, 2), Some(1));
        assert_eq!(after_close(0, 0), None);
    }

    #[test]
    fn session_cycling_wraps_both_ways() {
        assert_eq!(next_index(Some(0), 3), Some(1));
        assert_eq!(next_index(Some(2), 3), Some(0));
        assert_eq!(next_index(None, 3), Some(0));
        assert_eq!(prev_index(Some(0), 3), Some(2));
        assert_eq!(prev_index(Some(2), 3), Some(1));
        assert_eq!(prev_index(None, 3), Some(2));
        assert_eq!(next_index(None, 0), None);
        assert_eq!(prev_index(Some(0), 0), None);
    }

    #[test]
    fn delete_prompt_says_what_goes_and_mentions_source_control() {
        let (title, body) = delete_prompt("A.cpp", false, Some("Subversion"));
        assert_eq!(title, "Delete A.cpp?");
        assert!(body.starts_with("This file will be deleted permanently"));
        assert!(body.contains("If it's under Subversion"));
        let (_, body) = delete_prompt("Source", true, None);
        assert!(body.starts_with("This folder and everything in it"));
        assert!(!body.contains("If it's under"));
    }

    #[test]
    fn recent_entries_describe_folders_engines_and_errors() {
        let folder = RecentProject {
            path: PathBuf::from(r"D:\tools\tethys"),
            project: Ok(Project::folder(r"D:\tools\tethys")),
        };
        assert_eq!(
            recent_entry_text(&folder),
            ("tethys".to_string(), "Folder".to_string(), false)
        );

        let mut found = unreal("5.6");
        found.engine = Ok(PathBuf::from(r"C:\UE_5.6"));
        let entry = RecentProject {
            path: found.path.clone(),
            project: Ok(found),
        };
        assert_eq!(
            recent_entry_text(&entry),
            (
                "Game".to_string(),
                "Engine: Launcher 5.6".to_string(),
                false
            )
        );

        let mut missing = unreal("5.9");
        missing.engine = Err("no such engine".into());
        let entry = RecentProject {
            path: missing.path.clone(),
            project: Ok(missing),
        };
        let (_, detail, is_error) = recent_entry_text(&entry);
        assert_eq!(detail, "Engine: Launcher 5.9 — no such engine");
        assert!(is_error);

        let broken = RecentProject {
            path: PathBuf::from(r"D:\gone\Old.uproject"),
            project: Err("not found".into()),
        };
        assert_eq!(
            recent_entry_text(&broken),
            ("Old".to_string(), "not found".to_string(), true)
        );
    }

    fn breakpoints_at(file: &str, lines: &[u32]) -> Breakpoints {
        let mut bps = Breakpoints::default();
        for line in lines {
            bps.toggle(Path::new(file), *line);
        }
        bps
    }

    #[test]
    fn stops_name_our_breakpoint_by_file_and_line() {
        let bps = breakpoints_at(r"D:\p\Source\A.cpp", &[10, 20]);
        let second = bps.all()[1].id;
        assert_eq!(
            describe_stop(&StopReason::Breakpoint(second), &bps),
            "Breakpoint at A.cpp:20"
        );
        // One we don't know (e.g. removed since) falls back to the plain word.
        assert_eq!(
            describe_stop(&StopReason::Breakpoint(BreakpointId(99)), &bps),
            "Breakpoint"
        );
        assert_eq!(describe_stop(&StopReason::Pause, &bps), "Paused");
    }

    fn frame(function: &str, file: Option<&str>, line: Option<u32>) -> StackFrame {
        StackFrame {
            function: function.into(),
            file: file.map(PathBuf::from),
            line,
        }
    }

    #[test]
    fn frame_rows_locate_sources_here_or_under_the_local_engine() {
        let local = Path::new(r"D:\p\Source\A.cpp");
        let engine_twin = Path::new(r"C:\UE\Engine\Source\B.cpp");
        let exists = |p: &Path| p == local || p == engine_twin;
        let frames = vec![
            frame("Game!Foo", Some(r"D:\p\Source\A.cpp"), Some(12)),
            frame("Core!Bar", Some(r"E:\build\Engine\Source\B.cpp"), Some(7)),
            frame("Core!Baz", Some(r"E:\build\Engine\Source\C.cpp"), Some(9)),
            frame("0x7ff0", None, None),
        ];
        let rows = frame_rows(frames, Some(Path::new(r"C:\UE")), exists);

        assert_eq!(rows[0].location.as_deref(), Some("A.cpp:12"));
        assert_eq!(rows[0].source, Some((local.to_path_buf(), 12)));
        // Built elsewhere, but the engine file exists under the local install.
        assert_eq!(rows[1].source, Some((engine_twin.to_path_buf(), 7)));
        // Known location, but nowhere on this machine.
        assert_eq!(rows[2].location.as_deref(), Some("C.cpp:9"));
        assert_eq!(rows[2].source, None);
        assert_eq!(rows[3].location, None);
        assert_eq!(rows[3].source, None);
        assert!(rows.iter().all(|r| !r.current));
    }

    #[test]
    fn without_an_engine_only_files_that_exist_as_named_get_sources() {
        let frames = vec![frame(
            "Core!Bar",
            Some(r"E:\build\Engine\Source\B.cpp"),
            Some(7),
        )];
        let rows = frame_rows(frames, None, |_| false);
        assert_eq!(rows[0].source, None);
    }

    fn row(source: Option<(&str, u32)>) -> FrameRow {
        FrameRow {
            function: String::new(),
            location: None,
            source: source.map(|(f, l)| (PathBuf::from(f), l)),
            current: false,
        }
    }

    #[test]
    fn the_first_frame_with_source_is_shown() {
        let rows = vec![
            row(None),
            row(Some((r"D:\p\A.cpp", 3))),
            row(Some((r"D:\p\B.cpp", 4))),
        ];
        assert_eq!(innermost_with_source(&rows), Some(1));
        assert_eq!(innermost_with_source(&[row(None)]), None);
    }

    #[test]
    fn the_stop_line_marks_only_the_shown_frames_file_ignoring_case() {
        let rows = vec![row(Some((r"D:\p\A.cpp", 3))), row(Some((r"D:\p\B.cpp", 4)))];
        assert_eq!(stop_line(&rows, Some(0), Path::new(r"d:\P\a.CPP")), Some(3));
        assert_eq!(stop_line(&rows, Some(0), Path::new(r"D:\p\B.cpp")), None);
        assert_eq!(stop_line(&rows, Some(1), Path::new(r"D:\p\B.cpp")), Some(4));
        assert_eq!(stop_line(&rows, None, Path::new(r"D:\p\A.cpp")), None);
        assert_eq!(stop_line(&rows, Some(5), Path::new(r"D:\p\A.cpp")), None);
    }

    fn stopped(reason: StopReason, frames: Vec<StackFrame>) -> DebugEvent {
        DebugEvent::Stopped {
            reason,
            thread: 1,
            frames,
        }
    }

    #[test]
    fn debug_events_from_an_earlier_session_are_ignored() {
        let mut debug = Debug {
            generation: 2,
            ..Debug::default()
        };
        let effect = debug.apply(1, DebugEvent::Running, None, |_| false);
        assert_eq!(effect, DebugEffect::Ignored);
        assert_eq!(debug.status, DebugStatus::Detached);
    }

    #[test]
    fn a_stop_with_source_opens_that_frame_and_without_shows_the_panel() {
        let mut debug = Debug::default();
        assert_eq!(
            debug.apply(0, DebugEvent::Running, None, |_| false),
            DebugEffect::None
        );
        assert_eq!(debug.status, DebugStatus::Running);

        let a = r"D:\p\A.cpp";
        let frames = vec![
            frame("0x1", None, None),
            frame("Game!Foo", Some(a), Some(5)),
        ];
        let effect = debug.apply(0, stopped(StopReason::Pause, frames), None, |p| {
            p == Path::new(a)
        });
        assert_eq!(effect, DebugEffect::OpenFrame(1));
        assert_eq!(debug.status, DebugStatus::Stopped("Paused".into()));
        assert_eq!(debug.frames.len(), 2);

        let frames = vec![frame("0x1", None, None)];
        let effect = debug.apply(0, stopped(StopReason::DebugBreak, frames), None, |_| false);
        assert_eq!(effect, DebugEffect::ShowPanel);

        // Running again clears the stack from the last stop.
        debug.frame = Some(0);
        debug.apply(0, DebugEvent::Running, None, |_| false);
        assert!(debug.frames.is_empty());
        assert_eq!(debug.frame, None);
    }

    #[test]
    fn a_stop_at_our_breakpoint_is_named_from_the_breakpoint_list() {
        let mut debug = Debug::default();
        debug.breakpoints.toggle(Path::new(r"D:\p\A.cpp"), 42);
        let id = debug.breakpoints.all()[0].id;
        debug.apply(0, stopped(StopReason::Breakpoint(id), vec![]), None, |_| {
            false
        });
        assert_eq!(
            debug.status,
            DebugStatus::Stopped("Breakpoint at A.cpp:42".into())
        );
    }

    #[test]
    fn breakpoint_states_are_recorded_and_forgotten_when_the_session_ends() {
        let mut debug = Debug::default();
        debug.breakpoints.toggle(Path::new(r"D:\p\A.cpp"), 1);
        let id = debug.breakpoints.all()[0].id;
        let effect = debug.apply(
            0,
            DebugEvent::Breakpoints(vec![(id, BreakpointState::Bound)]),
            None,
            |_| false,
        );
        assert_eq!(effect, DebugEffect::None);
        assert_eq!(debug.breakpoints.all()[0].state, BreakpointState::Bound);

        debug.status = DebugStatus::Running;
        debug.process = Some("UnrealEditor.exe (pid 1)".into());
        let effect = debug.apply(0, DebugEvent::Ended(DebugEnd::Detached), None, |_| false);
        assert_eq!(effect, DebugEffect::None);
        assert_eq!(debug.status, DebugStatus::Detached);
        assert_eq!(debug.breakpoints.all()[0].state, BreakpointState::Unset);
        assert_eq!(debug.process, None);
    }

    #[test]
    fn an_exit_leaves_a_note_and_a_failure_reports_an_error() {
        let mut debug = Debug::default();
        debug.apply(0, DebugEvent::Ended(DebugEnd::Exited), None, |_| false);
        assert_eq!(debug.process.as_deref(), Some("The editor exited."));

        let effect = debug.apply(
            0,
            DebugEvent::Ended(DebugEnd::Failed("lost the pipe".into())),
            None,
            |_| false,
        );
        assert_eq!(
            effect,
            DebugEffect::Error("The debugger stopped: lost the pipe".into())
        );
        assert_eq!(debug.process, None);
    }
}
