// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keys;
mod session_panel;
mod terminal_view;
mod theme;
mod workspace;

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

fn main() {
    // `tethys path\to\Game.uproject`
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    application().run(move |cx| {
        gpui_kit::init(cx);
        theme::apply(cx);
        cx.set_global(workspace::Services::new());
        cx.bind_keys(workspace::key_bindings());

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
