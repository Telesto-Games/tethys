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
    version_override(std::env::var("TETHYS_PRETEND_VERSION").ok()).unwrap_or_else(|| {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is a version")
    })
}

/// The version `TETHYS_PRETEND_VERSION` asks for, if it names one.
fn version_override(pretend: Option<String>) -> Option<Version> {
    pretend.and_then(|v| Version::parse(&v))
}

/// Whether the quiet startup check should run, once per process. Off in
/// debug builds (unless pretending to be an old version) and when
/// `TETHYS_NO_UPDATE_CHECK` is set.
pub fn claim_startup_check() -> bool {
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    let enabled = startup_check_enabled(
        std::env::var_os("TETHYS_NO_UPDATE_CHECK").is_some(),
        std::env::var_os("TETHYS_PRETEND_VERSION").is_some(),
        cfg!(debug_assertions),
    );
    enabled && !CLAIMED.swap(true, Ordering::Relaxed)
}

/// See [`claim_startup_check`].
fn startup_check_enabled(no_check_set: bool, pretend_set: bool, debug_build: bool) -> bool {
    !no_check_set && (!debug_build || pretend_set)
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
            let stage = stage_for(latest.await, current_version(), skipped, manual);
            if !surfaces(manual, &stage) {
                if let Stage::Failed { error, .. } = &stage {
                    eprintln!("tethys: update check failed: {error}");
                }
                return;
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

/// What a check found: a release worth offering (see `update::should_offer`),
/// nothing newer, or a failure.
fn stage_for(
    latest: Result<Option<Release>, PortError>,
    current: Version,
    skipped: Option<Version>,
    manual: bool,
) -> Stage {
    match latest {
        Ok(Some(release)) if update::should_offer(current, &release, skipped, manual) => {
            Stage::Available(release)
        }
        Ok(_) => Stage::UpToDate,
        Err(e) => Stage::Failed {
            error: e.to_string(),
            release: None,
        },
    }
}

/// Whether a check's result opens the dialog: a manual check always shows
/// what it found; a quiet one only an update to offer.
fn surfaces(manual: bool, stage: &Stage) -> bool {
    manual || matches!(stage, Stage::Available(_))
}

pub struct UpdateFlow {
    stage: Stage,
    project: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use tethys_core::ports::PortError;
    use tethys_core::update::{Release, Version};

    use super::{Stage, stage_for, startup_check_enabled, surfaces, version_override};

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release(version: &str) -> Release {
        Release {
            version: v(version),
            notes: String::new(),
            page_url: String::new(),
            exe_url: String::new(),
            sha256: String::new(),
        }
    }

    #[test]
    fn a_newer_release_is_available_unless_skipped() {
        let current = v("0.4.0");
        assert_eq!(
            stage_for(Ok(Some(release("0.5.0"))), current, None, false),
            Stage::Available(release("0.5.0"))
        );
        assert_eq!(
            stage_for(Ok(Some(release("0.4.0"))), current, None, false),
            Stage::UpToDate
        );
        assert_eq!(stage_for(Ok(None), current, None, true), Stage::UpToDate);
        // A skipped version is up to date quietly, but offered when asked.
        assert_eq!(
            stage_for(Ok(Some(release("0.5.0"))), current, Some(v("0.5.0")), false),
            Stage::UpToDate
        );
        assert_eq!(
            stage_for(Ok(Some(release("0.5.0"))), current, Some(v("0.5.0")), true),
            Stage::Available(release("0.5.0"))
        );
        let failed: Result<Option<Release>, PortError> = Err("no network".into());
        assert_eq!(
            stage_for(failed, current, None, false),
            Stage::Failed {
                error: "no network".into(),
                release: None
            }
        );
    }

    #[test]
    fn quiet_checks_only_surface_an_update() {
        let failed = Stage::Failed {
            error: String::new(),
            release: None,
        };
        assert!(surfaces(false, &Stage::Available(release("1.0.0"))));
        assert!(!surfaces(false, &Stage::UpToDate));
        assert!(!surfaces(false, &failed));
        assert!(surfaces(true, &Stage::UpToDate));
        assert!(surfaces(true, &failed));
    }

    #[test]
    fn startup_check_rules() {
        // Release build: on unless opted out.
        assert!(startup_check_enabled(false, false, false));
        assert!(!startup_check_enabled(true, false, false));
        // Debug build: only when pretending to be an old version.
        assert!(!startup_check_enabled(false, false, true));
        assert!(startup_check_enabled(false, true, true));
        assert!(!startup_check_enabled(true, true, true));
    }

    #[test]
    fn pretend_version_must_parse() {
        assert_eq!(version_override(None), None);
        assert_eq!(version_override(Some("0.0.1".into())), Some(v("0.0.1")));
        assert_eq!(version_override(Some("latest".into())), None);
    }
}
