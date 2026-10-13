//! The Themes dialog (Edit → Themes…): pick the theme in use, and make, edit
//! or delete your own. A theme is a backdrop, two text colours, an accent and
//! a font (`ThemeSpec`); the built-in Telesto theme can be copied but not
//! changed. Themes live in `state.toml`.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::color_picker::{ColorPickerState, ColorSelect};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::ports::ConfigStore;
use tethys_core::theme::{self, ThemeSpec};

use crate::theme::{self as app_theme, CurrentTheme};
use crate::workspace::Services;

/// Opens the Themes dialog in `window`.
pub fn open(window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| ThemesEditor::new(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title("Themes").w(px(720.)).child(editor.clone())
    });
}

/// The dialog's state: the user's themes, which is selected, and the fields
/// editing it.
struct ThemesEditor {
    /// The user's themes; the built-in one is not among them.
    themes: Vec<ThemeSpec>,
    /// Index into `themes`, or `None` for the built-in theme.
    selected: Option<usize>,
    name: Entity<InputState>,
    font_family: Entity<InputState>,
    font_size: Entity<InputState>,
    background: Entity<ColorPickerState>,
    foreground: Entity<ColorPickerState>,
    muted_foreground: Entity<ColorPickerState>,
    accent: Entity<ColorPickerState>,
    /// The "Terminals follow this theme" checkbox.
    terminal: bool,
    error: Option<String>,
}

impl ThemesEditor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let themes = cx.global::<Services>().config.themes().unwrap_or_default();
        let current = &cx.global::<CurrentTheme>().spec;
        let selected = themes.iter().position(|t| t.name == current.name);
        let spec = selected.map_or_else(ThemeSpec::builtin, |i| themes[i].clone());

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Theme name")
                .default_value(spec.name.clone())
        });
        let font_family = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(ThemeSpec::builtin().font_family)
                .default_value(spec.font_family.clone())
        });
        let font_size = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("16")
                .default_value(size_text(spec.font_size))
        });
        let builtin = ThemeSpec::builtin();
        let mut picker = |text: &str, fallback: &str, cx: &mut Context<Self>| {
            let colour = app_theme::colour(text, fallback);
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(colour))
        };
        let background = picker(&spec.background, &builtin.background, cx);
        let foreground = picker(&spec.foreground, &builtin.foreground, cx);
        let muted_foreground = picker(&spec.muted_foreground, &builtin.muted_foreground, cx);
        let accent = picker(&spec.accent, &builtin.accent, cx);

        Self {
            themes,
            selected,
            name,
            font_family,
            font_size,
            background,
            foreground,
            muted_foreground,
            accent,
            terminal: spec.terminal,
            error: None,
        }
    }

    /// The theme the fields show, before any edits: built-in or a user one.
    fn selected_spec(&self) -> ThemeSpec {
        self.selected
            .map_or_else(ThemeSpec::builtin, |i| self.themes[i].clone())
    }

    /// The selected theme with the fields' current values.
    fn edited_spec(&self, cx: &App) -> ThemeSpec {
        let base = self.selected_spec();
        if self.selected.is_none() {
            return base;
        }
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let colour = |picker: &Entity<ColorPickerState>, fallback: &str| {
            picker
                .read(cx)
                .value()
                .map(hex_of)
                .unwrap_or_else(|| fallback.to_string())
        };
        let name = text(&self.name);
        let font_family = text(&self.font_family);
        ThemeSpec {
            name: if name.is_empty() {
                base.name.clone()
            } else {
                name
            },
            background: colour(&self.background, &base.background),
            foreground: colour(&self.foreground, &base.foreground),
            muted_foreground: colour(&self.muted_foreground, &base.muted_foreground),
            accent: colour(&self.accent, &base.accent),
            font_family: if font_family.is_empty() {
                base.font_family.clone()
            } else {
                font_family
            },
            font_size: text(&self.font_size)
                .parse::<f32>()
                .ok()
                .filter(|size| size.is_finite())
                .map_or(base.font_size, app_theme::font_size_of),
            terminal: self.terminal,
        }
    }

    /// Writes the fields back into the selected user theme.
    fn commit(&mut self, cx: &App) {
        if let Some(i) = self.selected {
            self.themes[i] = self.edited_spec(cx);
        }
    }

    /// Shows `spec` in the fields.
    fn load(&mut self, spec: &ThemeSpec, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal = spec.terminal;
        self.name.update(cx, |input, cx| {
            input.set_value(spec.name.clone(), window, cx)
        });
        self.font_family.update(cx, |input, cx| {
            input.set_value(spec.font_family.clone(), window, cx)
        });
        self.font_size.update(cx, |input, cx| {
            input.set_value(size_text(spec.font_size), window, cx)
        });
        let builtin = ThemeSpec::builtin();
        for (picker, text, fallback) in [
            (&self.background, &spec.background, &builtin.background),
            (&self.foreground, &spec.foreground, &builtin.foreground),
            (
                &self.muted_foreground,
                &spec.muted_foreground,
                &builtin.muted_foreground,
            ),
            (&self.accent, &spec.accent, &builtin.accent),
        ] {
            let colour = app_theme::colour(text, fallback);
            picker.update(cx, |picker, cx| picker.set_value(colour, window, cx));
        }
    }

    fn select(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
        self.selected = index;
        self.error = None;
        let spec = self.selected_spec();
        self.load(&spec, window, cx);
        cx.notify();
    }

    /// Adds a copy of the selected theme and selects it.
    fn new_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
        let source = self.selected_spec();
        let taken: Vec<String> = self.themes.iter().map(|t| t.name.clone()).collect();
        let copy = ThemeSpec {
            name: source.copy_name(&taken),
            ..source
        };
        self.themes.push(copy);
        self.select(Some(self.themes.len() - 1), window, cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.selected else { return };
        self.themes.remove(i);
        self.selected = None;
        let next = if self.themes.is_empty() {
            None
        } else {
            Some(i.min(self.themes.len() - 1))
        };
        self.select(next, window, cx);
    }

    /// Saves every theme, makes the selected one the theme in use, applies it
    /// and closes.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
        let spec = self.selected_spec();
        // Themes are found by name, so every name must be unique, and none
        // may take the built-in one's.
        if let Some(taken) = self.themes.iter().enumerate().find_map(|(i, t)| {
            let earlier = self.themes[..i].iter().any(|other| other.name == t.name);
            (t.is_builtin() || earlier).then_some(t.name.clone())
        }) {
            self.error = Some(format!("Another theme is already called {taken}."));
            cx.notify();
            return;
        }
        let active = self.selected.map(|_| spec.name.as_str());
        let config = &cx.global::<Services>().config;
        match config.set_themes(&self.themes, active) {
            Ok(()) => {
                app_theme::apply(&spec, cx);
                window.close_dialog(cx);
            }
            Err(e) => {
                self.error = Some(format!("Can't save themes: {e}"));
                cx.notify();
            }
        }
    }

    fn theme_row(
        &self,
        index: Option<usize>,
        label: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected == index;
        let in_use = cx.global::<CurrentTheme>().spec.name == label;
        div()
            .id(("theme-row", index.map_or(0, |i| i + 1)))
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .text_sm()
            .when(selected, |d| {
                d.bg(cx.theme().accent)
                    .text_color(cx.theme().accent_foreground)
            })
            .when(!selected, |d| d.hover(|d| d.bg(cx.theme().muted)))
            .on_click(cx.listener(move |this, _, window, cx| this.select(index, window, cx)))
            .child(div().overflow_hidden().text_ellipsis().child(label))
            .when(in_use, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("in use"),
                )
            })
            // Lets view tests find and click the row; nothing outside tests.
            .test_support()
    }

    fn colour_field(
        &self,
        label: &'static str,
        picker: &Entity<ColorPickerState>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().w(px(100.)).flex_shrink_0().child(label))
            .child(div().flex_1().child(ColorSelect::new(picker).small()))
    }
}

impl Render for ThemesEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let heading = cx.theme().primary.opacity(0.8);
        let section = move |title: &'static str| div().text_xs().text_color(heading).child(title);
        let custom = self.selected.is_some();

        let mut list = div().flex().flex_col().gap_px().child(self.theme_row(
            None,
            ThemeSpec::BUILTIN_NAME.to_string(),
            cx,
        ));
        for i in 0..self.themes.len() {
            list = list.child(self.theme_row(Some(i), self.themes[i].name.clone(), cx));
        }

        let fields = if custom {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().text_sm().w(px(100.)).child("Name"))
                        .child(div().flex_1().child(Input::new(&self.name))),
                )
                .child(div().pt_1().child(section("COLOURS")))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(self.colour_field("Background", &self.background))
                        .child(self.colour_field("Text", &self.foreground)),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(self.colour_field("Quiet text", &self.muted_foreground))
                        .child(self.colour_field("Accent", &self.accent)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child("Headers, buttons and hairlines are derived from these four."),
                )
                .child(div().pt_1().child(section("TYPE")))
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .flex_1()
                                .child(div().text_sm().child("Font family"))
                                .child(Input::new(&self.font_family)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .w(px(100.))
                                .child(div().text_sm().child("Font size"))
                                .child(Input::new(&self.font_size)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(format!(
                            "Any installed font; the UI scales with the size ({}–{} px).",
                            ThemeSpec::MIN_FONT_SIZE,
                            ThemeSpec::MAX_FONT_SIZE
                        )),
                )
                .child(div().pt_1().child(section("TERMINALS")))
                .child(
                    Checkbox::new("terminal-follows")
                        .label("Terminals follow this theme")
                        .checked(self.terminal)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.terminal = *checked;
                            cx.notify();
                        })),
                )
                .child(div().text_xs().text_color(muted).whitespace_normal().child(
                    "Sessions and terminals take these colours and font, two pixels smaller.",
                ))
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .child("Tethys's own look, after the Telesto brand."),
                )
                .child(div().text_xs().text_color(muted).whitespace_normal().child(
                    "The built-in theme can't be edited. New theme makes a copy of it \
                     that can be.",
                ))
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_4()
                    .items_start()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .w(px(200.))
                            .flex_shrink_0()
                            .child(section("THEMES"))
                            .child(list)
                            .child(
                                Button::new("new-theme")
                                    .ghost()
                                    .small()
                                    .label("New theme")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_theme(window, cx)
                                    })),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(fields)),
            )
            .children(self.error.clone().map(|e| {
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child(e)
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .pt_2()
                    .when(custom, |d| {
                        d.child(
                            Button::new("delete-theme")
                                .ghost()
                                .small()
                                .label("Delete")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.delete(window, cx)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel-themes")
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-themes")
                            .primary()
                            .small()
                            .label("Save and use")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

/// A GPUI colour as `#rrggbb`, alpha dropped.
fn hex_of(colour: Hsla) -> String {
    theme::hex(app_theme::packed(colour))
}

/// A font size for its input: `14` rather than `14.0`.
fn size_text(size: f32) -> String {
    if size.fract() == 0. {
        format!("{}", size as u32)
    } else {
        format!("{size}")
    }
}

#[cfg(test)]
mod tests {
    use super::{hex_of, size_text};
    use crate::theme::colour;

    #[test]
    fn colours_round_trip_through_pickers() {
        for text in ["#0b001f", "#f24b38", "#ffffff", "#000000", "#c8b8d8"] {
            assert_eq!(hex_of(colour(text, "#000000")), text);
        }
    }

    #[test]
    fn sizes_show_without_a_trailing_zero() {
        assert_eq!(size_text(16.), "16");
        assert_eq!(size_text(13.5), "13.5");
    }
}

/// The editor rendered in a headless window, driven by clicks and typing.
#[cfg(test)]
mod view_tests {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{App, AppContext, Entity, Hsla, TestAppContext, WindowHandle, px, size};
    use tethys_core::ports::ConfigStore;
    use tethys_core::theme::ThemeSpec;

    use super::ThemesEditor;
    use crate::test_support::{self, open_window, temp_config};
    use crate::theme::CurrentTheme;
    use crate::workspace::Services;

    type Fixture = (WindowHandle<gpui_kit::base::Root>, Entity<ThemesEditor>);

    fn open(name: &str, cx: &mut TestAppContext) -> Fixture {
        let config = temp_config(name);
        cx.update(|cx| test_support::init(config, cx));
        open_window(cx, size(px(900.), px(900.)), |window, cx| {
            cx.new(|cx| ThemesEditor::new(window, cx))
        })
    }

    fn state<R>(
        (window, editor): &Fixture,
        cx: &mut TestAppContext,
        read: impl FnOnce(&ThemesEditor, &App) -> R,
    ) -> R {
        cx.update_window((*window).into(), |_, _, cx| read(editor.read(cx), cx))
            .unwrap()
    }

    fn set_name(fixture: &Fixture, name: &str, cx: &mut TestAppContext) {
        let (window, editor) = fixture;
        cx.update_window((*window).into(), |_, window, cx| {
            let input = editor.read(cx).name.clone();
            input.update(cx, |input, cx| input.set_value(name, window, cx));
        })
        .unwrap();
    }

    fn click(fixture: &Fixture, id: &'static str, cx: &mut TestAppContext) {
        cx.update_window(fixture.0.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
    }

    fn click_row(fixture: &Fixture, row: usize, cx: &mut TestAppContext) {
        cx.update_window(fixture.0.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("theme-row", row), cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn starts_on_the_builtin_theme_with_no_fields(cx: &mut TestAppContext) {
        let fixture = open("start", cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), None);
        assert!(state(&fixture, cx, |e, _| e.themes.is_empty()));
        cx.update_window(fixture.0.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("delete-theme").is_none());
            assert!(window.find("new-theme").visible());
            assert!(window.find("save-themes").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn new_theme_copies_the_selection_and_is_editable(cx: &mut TestAppContext) {
        let fixture = open("new", cx);
        click(&fixture, "new-theme", cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), Some(0));
        let copy = state(&fixture, cx, |e, _| e.themes[0].clone());
        assert_eq!(copy.name, "Telesto copy");
        assert_eq!(copy.accent, ThemeSpec::builtin().accent);
        click(&fixture, "new-theme", cx);
        assert_eq!(
            state(&fixture, cx, |e, _| e.themes[1].name.clone()),
            "Telesto copy copy"
        );
        cx.update_window(fixture.0.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("delete-theme").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn edits_survive_switching_rows(cx: &mut TestAppContext) {
        let fixture = open("switch", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "Night", cx);
        // Back to the built-in row (row 0), then to the copy (row 1).
        click_row(&fixture, 0, cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), None);
        assert_eq!(
            state(&fixture, cx, |e, _| e.themes[0].name.clone()),
            "Night"
        );
        click_row(&fixture, 1, cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), Some(0));
        assert_eq!(
            state(&fixture, cx, |e, cx| e.name.read(cx).value().to_string()),
            "Night"
        );
    }

    #[gpui_kit::test]
    fn save_refuses_duplicate_and_builtin_names(cx: &mut TestAppContext) {
        let fixture = open("names", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "Telesto", cx);
        click(&fixture, "save-themes", cx);
        let error = state(&fixture, cx, |e, _| e.error.clone());
        assert_eq!(
            error.as_deref(),
            Some("Another theme is already called Telesto.")
        );
        assert!(state(&fixture, cx, |_, cx| cx
            .global::<Services>()
            .config
            .themes()
            .unwrap()
            .is_empty()));

        // Two user themes with the same name, neither of them selected.
        set_name(&fixture, "Dusk", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "Dusk", cx);
        click_row(&fixture, 0, cx);
        click(&fixture, "save-themes", cx);
        assert_eq!(
            state(&fixture, cx, |e, _| e.error.clone()).as_deref(),
            Some("Another theme is already called Dusk.")
        );
    }

    #[gpui_kit::test]
    fn save_persists_every_theme_and_applies_the_selected_one(cx: &mut TestAppContext) {
        let fixture = open("save", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "Night", cx);
        cx.update_window(fixture.0.into(), |_, window, cx| {
            let accent = fixture.1.read(cx).accent.clone();
            accent.update(cx, |picker, cx| {
                picker.set_value(Hsla::from(gpui_kit::rgb(0x00ff00)), window, cx)
            });
            fixture.1.update(cx, |editor, _| editor.terminal = true);
        })
        .unwrap();
        click(&fixture, "save-themes", cx);
        assert_eq!(state(&fixture, cx, |e, _| e.error.clone()), None);

        let (themes, active, current) = state(&fixture, cx, |_, cx| {
            let config = &cx.global::<Services>().config;
            (
                config.themes().unwrap(),
                config.active_theme().unwrap(),
                cx.global::<CurrentTheme>().spec.clone(),
            )
        });
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name, "Night");
        assert_eq!(themes[0].accent, "#00ff00");
        assert!(themes[0].terminal);
        assert_eq!(active.as_deref(), Some("Night"));
        assert_eq!(current, themes[0]);
    }

    #[gpui_kit::test]
    fn delete_removes_the_selection_and_falls_back_to_a_neighbour(cx: &mut TestAppContext) {
        let fixture = open("delete", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "A", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "B", cx);
        click(&fixture, "new-theme", cx);
        set_name(&fixture, "C", cx);
        // Select B (row 2) and delete it: C moves into its place and is selected.
        click_row(&fixture, 2, cx);
        click(&fixture, "delete-theme", cx);
        let names = state(&fixture, cx, |e, _| {
            e.themes.iter().map(|t| t.name.clone()).collect::<Vec<_>>()
        });
        assert_eq!(names, ["A", "C"]);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), Some(1));
        click(&fixture, "delete-theme", cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), Some(0));
        click(&fixture, "delete-theme", cx);
        assert_eq!(state(&fixture, cx, |e, _| e.selected), None);
        assert!(state(&fixture, cx, |e, _| e.themes.is_empty()));
    }

    #[gpui_kit::test]
    fn reopening_selects_the_theme_in_use(cx: &mut TestAppContext) {
        let config = temp_config("reopen");
        let mine = ThemeSpec {
            name: "Mine".into(),
            ..ThemeSpec::builtin()
        };
        config
            .set_themes(std::slice::from_ref(&mine), Some("Mine"))
            .unwrap();
        cx.update(|cx| test_support::init(config, cx));
        let fixture = open_window(cx, size(px(900.), px(900.)), |window, cx| {
            cx.new(|cx| ThemesEditor::new(window, cx))
        });
        assert_eq!(state(&fixture, cx, |e, _| e.selected), Some(0));
        assert_eq!(
            state(&fixture, cx, |e, cx| e.name.read(cx).value().to_string()),
            "Mine"
        );
    }
}
