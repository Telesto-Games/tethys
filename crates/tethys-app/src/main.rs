// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod build_placeholder;
mod debug_panel;
mod diff_panel;
mod editor_panel;
mod file_tree;
mod keys;
mod session_panel;
mod settings_ui;
mod tab_skin;
mod terminal_view;
mod theme;
mod update_ui;
mod workspace;

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

fn main() {
    // `tethys path\to\Game.uproject` or `tethys path\to\folder`
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    // GPUI draws through DirectComposition with no redirection bitmap, which
    // screen capture tools like the Snipping Tool can't see. A plain swap chain
    // can be captured. Set GPUI_DISABLE_DIRECT_COMPOSITION=0 to opt back in.
    const NO_DCOMP: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";
    if std::env::var_os(NO_DCOMP).is_none() {
        // SAFETY: first thing in main, before any threads exist.
        unsafe { std::env::set_var(NO_DCOMP, "1") };
    }

    // Launched from inside a Claude Code session? Sessions shouldn't inherit
    // its NO_COLOR and nesting markers.
    let keys = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    for key in tethys_adapters::agent_terminal::leaked_agent_vars(keys) {
        // SAFETY: still before any threads exist.
        unsafe { std::env::remove_var(key) };
    }

    application().with_assets(assets::AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::apply(cx);
        workspace::install_menus(cx);
        cx.set_global(workspace::Services::new());
        tab_skin::init(cx);
        cx.bind_keys(workspace::key_bindings());

        // File > Exit: closing every window stops its sessions; the last close quits.
        cx.on_action(|_: &workspace::Exit, cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }
        });

        // Closing the last window quits, after giving agents a moment to exit cleanly.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.global::<workspace::Services>()
                    .host
                    .wait_for_stopped(Duration::from_secs(4));
                cx.quit();
            }
        })
        .detach();

        workspace::open_window(path, cx);
        cx.activate(true);
    });
}
