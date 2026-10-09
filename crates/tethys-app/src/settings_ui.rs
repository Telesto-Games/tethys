//! The Settings dialog: app-wide preferences, saved in `state.toml`.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;
use tethys_core::ports::ConfigStore;
use tethys_core::unreal;

use crate::workspace::Services;

/// The extra editor arguments to launch with, as saved.
pub fn editor_args(cx: &App) -> Vec<String> {
    let config = &cx.global::<Services>().config;
    let line = config
        .editor_args()
        .unwrap_or_else(|_| unreal::DEFAULT_EDITOR_ARGS.to_string());
    unreal::split_args(&line)
}

/// Opens the Settings dialog in `window`.
pub fn open(window: &mut Window, cx: &mut App) {
    let saved = cx
        .global::<Services>()
        .config
        .editor_args()
        .unwrap_or_else(|_| unreal::DEFAULT_EDITOR_ARGS.to_string());
    let args = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("No extra arguments")
            .default_value(saved)
    });

    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let section = |title: &'static str| {
            div()
                .text_xs()
                .text_color(cx.theme().primary.opacity(0.8))
                .child(title)
        };
        let reset = args.clone();
        let save = args.clone();
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
                             Launch editor, and Debug when no editor is running. Separate \
                             arguments with spaces and quote one that contains spaces.",
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
                                .label("Reset to default")
                                .on_click(move |_, window, cx| {
                                    reset.update(cx, |input, cx| {
                                        input.set_value(unreal::DEFAULT_EDITOR_ARGS, window, cx)
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
                                    let config = &cx.global::<Services>().config;
                                    match config.set_editor_args(&line) {
                                        Ok(()) => window.close_dialog(cx),
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
