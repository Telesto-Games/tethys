//! The updater's UI: a quiet check after startup, Help → Check for Updates,
//! and the dialog that downloads, installs and restarts Tethys.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::ports::{ConfigStore, PortError};
use tethys_core::update::{self, Release, Version};

use crate::workspace::{Exit, Services};

/// The GitHub repository releases come from.
pub const REPO: &str = "Telesto-Games/tethys";
/// How long after startup the quiet check runs, so it doesn't slow startup.
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// The running version. `TETHYS_PRETEND_VERSION=0.0.1` overrides it, to try
/// an update against a real release.
pub fn current_version() -> Version {
    std::env::var("TETHYS_PRETEND_VERSION")
        .ok()
        .and_then(|v| Version::parse(&v))
        .unwrap_or_else(|| {
            Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a version")
        })
}

/// Whether the quiet startup check should run, once per process. Off in
/// debug builds (unless pretending to be an old version) and when
/// `TETHYS_NO_UPDATE_CHECK` is set.
pub fn claim_startup_check() -> bool {
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    let enabled = std::env::var_os("TETHYS_NO_UPDATE_CHECK").is_none()
        && (!cfg!(debug_assertions) || std::env::var_os("TETHYS_PRETEND_VERSION").is_some());
    enabled && !CLAIMED.swap(true, Ordering::Relaxed)
}

pub fn startup_delay() -> Duration {
    STARTUP_DELAY
}

/// Checks for a newer release. A `manual` check shows its progress and an
/// "up to date" result; a quiet one only opens a dialog when there's an
/// update to offer. `project` is reopened after updating.
pub fn check(manual: bool, project: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let flow = cx.new(|_| UpdateFlow {
        stage: Stage::Checking,
        project,
    });
    if manual {
        open_dialog(flow.clone(), window, cx);
    }
    let services = cx.global::<Services>();
    let source = services.updates.clone();
    let skipped = services.config.skipped_version().ok().flatten();
    let latest = cx
        .background_executor()
        .spawn(async move { source.latest() });
    window
        .spawn(cx, async move |cx| {
            let stage = match latest.await {
                Ok(Some(release))
                    if update::should_offer(current_version(), &release, skipped, manual) =>
                {
                    Stage::Available(release)
                }
                Ok(_) => Stage::UpToDate,
                Err(e) => Stage::Failed {
                    error: e.to_string(),
                    release: None,
                },
            };
            if !manual {
                match &stage {
                    Stage::Available(_) => {}
                    Stage::Failed { error, .. } => {
                        eprintln!("tethys: update check failed: {error}");
                        return;
                    }
                    _ => return,
                }
            }
            let _ = cx.update(|window, cx| {
                flow.update(cx, |flow, cx| {
                    flow.stage = stage;
                    cx.notify();
                });
                if !manual {
                    open_dialog(flow, window, cx);
                }
                window.refresh();
            });
        })
        .detach();
}

pub struct UpdateFlow {
    stage: Stage,
    project: Option<PathBuf>,
}

enum Stage {
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed {
        error: String,
        /// The release being installed, so the dialog can link to it.
        release: Option<Release>,
    },
}

fn open_dialog(flow: Entity<UpdateFlow>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let current = current_version();
        let body = div().flex().flex_col().gap_2();
        let mut buttons = div().flex().gap_2().justify_end();
        let body = match &flow.read(cx).stage {
            Stage::Checking => body.child("Checking for updates…"),
            Stage::UpToDate => body.child(format!("Tethys {current} is up to date.")),
            Stage::Available(release) => {
                let release_page = release.page_url.clone();
                let release = release.clone();
                buttons = buttons
                    .child(
                        Button::new("release-page")
                            .ghost()
                            .small()
                            .label("Release page")
                            .on_click(move |_, _, cx| cx.open_url(&release_page)),
                    )
                    .child(
                        Button::new("skip")
                            .small()
                            .label("Skip this version")
                            .on_click({
                                let version = release.version;
                                move |_, window, cx| {
                                    let config = &cx.global::<Services>().config;
                                    if let Err(e) = config.set_skipped_version(Some(version)) {
                                        eprintln!("tethys: can't save the skipped version: {e}");
                                    }
                                    window.close_dialog(cx);
                                }
                            }),
                    )
                    .child(
                        Button::new("later")
                            .small()
                            .label("Later")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("update")
                            .primary()
                            .small()
                            .label("Update and restart")
                            .on_click({
                                let flow = flow.clone();
                                move |_, window, cx| install(flow.clone(), window, cx)
                            }),
                    );
                body.child(format!(
                    "Tethys {} is available. You have {current}.",
                    release.version
                ))
                .child(div().text_sm().text_color(muted).child(
                    "Updating closes this window's agent sessions, then reopens the project.",
                ))
                .when(!release.notes.trim().is_empty(), |body| {
                    body.child(
                        div()
                            .id("release-notes")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .text_sm()
                            .whitespace_normal()
                            .child(release.notes.replace("\r\n", "\n")),
                    )
                })
            }
            Stage::Installing(release) => body.child(format!(
                "Downloading and installing Tethys {}…",
                release.version
            )),
            Stage::Failed { error, release } => {
                if let Some(release) = release {
                    let page = release.page_url.clone();
                    buttons = buttons.child(
                        Button::new("release-page")
                            .small()
                            .label("Download from the release page")
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    );
                }
                body.child("Couldn't update Tethys.")
                    .child(div().text_sm().text_color(muted).child(error.clone()))
            }
        };
        dialog
            .title("Tethys updates")
            .w(px(520.))
            .child(body)
            .footer(buttons)
    });
}

/// Downloads and installs the offered release, then restarts Tethys on the
/// same project.
fn install(flow: Entity<UpdateFlow>, window: &mut Window, cx: &mut App) {
    let (release, project) = {
        let flow = flow.read(cx);
        let Stage::Available(release) = &flow.stage else {
            return;
        };
        (release.clone(), flow.project.clone())
    };
    flow.update(cx, |flow, cx| {
        flow.stage = Stage::Installing(release.clone());
        cx.notify();
    });
    let services = cx.global::<Services>();
    let source = services.updates.clone();
    let installer = services.installer.clone();
    let installed = cx.background_executor().spawn({
        let release = release.clone();
        let installer = installer.clone();
        async move {
            let staging = installer.staging_path()?;
            source.download(&release, &staging)?;
            installer.install(&staging)?;
            Ok::<_, PortError>(())
        }
    });
    window
        .spawn(cx, async move |cx| {
            let args: Vec<String> = project
                .map(|p| p.to_string_lossy().into_owned())
                .into_iter()
                .collect();
            let result = installed.await.and_then(|()| installer.relaunch(&args));
            let _ = cx.update(|window, cx| match result {
                // The new Tethys is starting; close this one the usual way.
                Ok(()) => cx.dispatch_action(&Exit),
                Err(e) => {
                    flow.update(cx, |flow, cx| {
                        flow.stage = Stage::Failed {
                            error: e.to_string(),
                            release: Some(release),
                        };
                        cx.notify();
                    });
                    window.refresh();
                }
            });
        })
        .detach();
}
