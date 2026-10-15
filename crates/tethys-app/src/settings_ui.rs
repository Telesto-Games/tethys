//! The Settings dialog: app-wide preferences, saved in `state.toml`.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use tethys_core::ports::ConfigStore;
use tethys_core::unreal;

use crate::tab_skin::{self, TabLayout};
use crate::workspace::Services;

/// The extra editor arguments to launch with, as saved.
pub fn editor_args(cx: &App) -> Vec<String> {
    let config = &cx.global::<Services>().config;
    args_or_default(config.editor_args(), unreal::DEFAULT_EDITOR_ARGS)
}

/// The extra arguments for the headless test editor, as saved.
pub fn test_editor_args(cx: &App) -> Vec<String> {
    let config = &cx.global::<Services>().config;
    args_or_default(config.test_editor_args(), unreal::DEFAULT_TEST_EDITOR_ARGS)
}

/// The saved argument line split into arguments, or `default`'s if the
/// config couldn't be read. A saved empty line means no arguments.
fn args_or_default<E>(saved: Result<String, E>, default: &str) -> Vec<String> {
    let line = saved.unwrap_or_else(|_| default.to_string());
    unreal::split_args(&line)
}

/// Opens the Settings dialog in `window`.
pub fn open(window: &mut Window, cx: &mut App) {
    let config = &cx.global::<Services>().config;
    let saved = config
        .editor_args()
        .unwrap_or_else(|_| unreal::DEFAULT_EDITOR_ARGS.to_string());
    let saved_test = config
        .test_editor_args()
        .unwrap_or_else(|_| unreal::DEFAULT_TEST_EDITOR_ARGS.to_string());
    let args = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("No extra arguments")
            .default_value(saved)
    });
    let test_args = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("No extra arguments")
            .default_value(saved_test)
    });
    let vertical = Rc::new(Cell::new(cx.global::<TabLayout>().vertical));

    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let section = |title: &'static str| {
            div()
                .text_xs()
                .text_color(cx.theme().primary.opacity(0.8))
                .child(title)
        };
        let reset = args.clone();
        let reset_test = test_args.clone();
        let save = args.clone();
        let save_test = test_args.clone();
        let toggle = vertical.clone();
        let save_vertical = vertical.clone();
        dialog.title("Settings").w(px(560.)).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(section("LAUNCHING THE EDITOR"))
                .child(div().text_sm().child("Extra command-line arguments"))
                .child(Input::new(&args))
                .child(div().text_xs().text_color(muted).whitespace_normal().child(
                    "Added after the project whenever Tethys starts the Unreal Editor: \
                             Launch (as the editor, a game or a server), and Debug when no \
                             editor is running. Separate arguments with spaces and quote one \
                             that contains spaces.",
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(format!(
                            "The default, {}, starts the Unreal MCP server (the \
                             ModelContextProtocol plugin) with the editor. The editor ignores \
                             it if the plugin isn't enabled.",
                            unreal::DEFAULT_EDITOR_ARGS
                        )),
                )
                .child(div().pt_2().child(section("RUNNING TESTS")))
                .child(div().text_sm().child("Test editor arguments"))
                .child(Input::new(&test_args))
                .child(div().text_xs().text_color(muted).whitespace_normal().child(format!(
                    "Added when Tests starts a headless test editor (UnrealEditor-Cmd). The \
                     default, {}, skips rendering; remove it for tests that render. Add \
                     anything your project needs to start headless, e.g. -DisablePlugins=….",
                    unreal::DEFAULT_TEST_EDITOR_ARGS
                )))
                .child(div().pt_2().child(section("TABS")))
                .child(
                    Checkbox::new("vertical-tabs")
                        .label("Vertical tabs")
                        .checked(vertical.get())
                        .on_click(move |checked, window, _| {
                            toggle.set(*checked);
                            window.refresh();
                        }),
                )
                .child(div().text_xs().text_color(muted).whitespace_normal().child(
                    "List sessions and open files down the left of the main area instead of \n                     along the top. Side panes keep their tabs along the top.",
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .pt_2()
                        .child(
                            Button::new("reset-editor-args")
                                .ghost()
                                .small()
                                .label("Reset to defaults")
                                .on_click(move |_, window, cx| {
                                    reset.update(cx, |input, cx| {
                                        input.set_value(unreal::DEFAULT_EDITOR_ARGS, window, cx)
                                    });
                                    reset_test.update(cx, |input, cx| {
                                        input.set_value(
                                            unreal::DEFAULT_TEST_EDITOR_ARGS,
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                        .child(
                            Button::new("cancel-settings")
                                .small()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save-settings")
                                .primary()
                                .small()
                                .label("Save")
                                .on_click(move |_, window, cx| {
                                    let line = save.read(cx).value().to_string();
                                    let test_line = save_test.read(cx).value().to_string();
                                    let vertical = save_vertical.get();
                                    let config = &cx.global::<Services>().config;
                                    match config
                                        .set_editor_args(&line)
                                        .and_then(|()| config.set_test_editor_args(&test_line))
                                        .and_then(|()| config.set_vertical_tabs(vertical))
                                    {
                                        Ok(()) => {
                                            tab_skin::set_vertical(vertical, cx);
                                            window.close_dialog(cx)
                                        }
                                        Err(e) => {
                                            eprintln!("tethys: can't save settings: {e}")
                                        }
                                    }
                                }),
                        ),
                ),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::args_or_default;

    #[test]
    fn saved_args_are_split_and_errors_fall_back() {
        let ok: Result<String, ()> = Ok("-log \"-ini:Engine:[Core]:x=1\"".into());
        assert_eq!(
            args_or_default(ok, "-default"),
            ["-log", "-ini:Engine:[Core]:x=1"]
        );
        let empty: Result<String, ()> = Ok(String::new());
        assert!(args_or_default(empty, "-default").is_empty());
        let failed: Result<String, ()> = Err(());
        assert_eq!(args_or_default(failed, "-a -b"), ["-a", "-b"]);
    }
}
