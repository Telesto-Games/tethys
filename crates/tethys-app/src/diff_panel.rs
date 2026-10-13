//! A unified diff of one file against source control.

use std::path::{Path, PathBuf};

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::*;
use tethys_core::diff::{FileDiff, LineKind};

use crate::editor_panel::expand_tabs;
use crate::file_tree::file_name;

const ROW_HEIGHT: f32 = 20.;
const GUTTER: f32 = 48.;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Hunk {
        old_start: usize,
        new_start: usize,
    },
    Line {
        kind: LineKind,
        old_line: Option<usize>,
        new_line: Option<usize>,
        text: SharedString,
    },
}

pub struct DiffPanel {
    path: PathBuf,
    /// "against BASE (Subversion)" or similar.
    against: String,
    rows: Vec<Row>,
    added: usize,
    removed: usize,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl DiffPanel {
    pub fn new(path: PathBuf, against: String, diff: FileDiff, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            against,
            rows: Vec::new(),
            added: 0,
            removed: 0,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        };
        this.set_diff(diff, cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Shows a diff of something else, e.g. another commit.
    pub fn show(&mut self, against: String, diff: FileDiff, cx: &mut Context<Self>) {
        self.against = against;
        self.set_diff(diff, cx);
    }

    pub fn set_diff(&mut self, diff: FileDiff, cx: &mut Context<Self>) {
        (self.rows, self.added, self.removed) = diff_rows(diff);
        cx.notify();
    }

    fn file_name(&self) -> String {
        file_name(&self.path)
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let added_bg = Hsla::from(rgb(0x98c379)).opacity(0.14);
        let removed_bg = Hsla::from(rgb(0xe06c75)).opacity(0.14);
        let added_fg: Hsla = rgb(0xb5dd96).into();
        let removed_fg: Hsla = rgb(0xf0a0a6).into();
        let mono = theme.mono_font_family.clone();
        let number = |n: Option<usize>| {
            div()
                .w(px(GUTTER))
                .flex_none()
                .pr_2()
                .text_right()
                .text_color(muted.opacity(0.7))
                .child(n.map(|n| n.to_string()).unwrap_or_default())
        };

        range
            .filter_map(|ix| self.rows.get(ix))
            .map(|row| match row {
                Row::Hunk {
                    old_start,
                    new_start,
                } => div()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .px_3()
                    .bg(theme.primary.opacity(0.08))
                    .text_color(theme.primary.opacity(0.85))
                    .child(hunk_header(*old_start, *new_start))
                    .into_any_element(),
                Row::Line {
                    kind,
                    old_line,
                    new_line,
                    text,
                } => {
                    let (bg, fg, sign) = match kind {
                        LineKind::Added => (Some(added_bg), added_fg, "+"),
                        LineKind::Removed => (Some(removed_bg), removed_fg, "-"),
                        LineKind::Context => (None, theme.foreground.opacity(0.8), " "),
                    };
                    let mut line = div()
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .whitespace_nowrap()
                        .child(number(*old_line))
                        .child(number(*new_line))
                        .child(div().w(px(16.)).flex_none().text_color(fg).child(sign))
                        .child(div().text_color(fg).child(text.clone()));
                    if let Some(bg) = bg {
                        line = line.bg(bg);
                    }
                    line.into_any_element()
                }
            })
            .map(|row| {
                div()
                    .font_family(mono.clone())
                    .text_sm()
                    .child(row)
                    .into_any_element()
            })
            .collect()
    }
}

/// A hunk as a header row followed by its lines, with the counts of lines
/// added and removed. Tabs expand to tab stops, as the editor shows them.
fn diff_rows(diff: FileDiff) -> (Vec<Row>, usize, usize) {
    let rows = diff
        .hunks
        .into_iter()
        .flat_map(|h| {
            std::iter::once(Row::Hunk {
                old_start: h.old_start,
                new_start: h.new_start,
            })
            .chain(h.lines.into_iter().map(|l| Row::Line {
                kind: l.kind,
                old_line: l.old_line,
                new_line: l.new_line,
                text: expand_tabs(&l.text).into(),
            }))
        })
        .collect();
    (rows, diff.added, diff.removed)
}

/// The unified-diff style hunk line.
fn hunk_header(old_start: usize, new_start: usize) -> String {
    format!("@@ -{old_start} +{new_start} @@")
}

impl EventEmitter<PanelEvent> for DiffPanel {}

impl gpui_kit::component::dock::BasePanel for DiffPanel {
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn panel_name(&self) -> &'static str {
        "TethysDiff"
    }
}

impl Panel for DiffPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(format!("Δ {}", self.file_name()).into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        format!("Δ {}", self.file_name())
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for DiffPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(format!(
                "{}  {}",
                self.path.display(),
                self.against
            )))
            .child(
                div()
                    .text_color(rgb(0x98c379))
                    .child(format!("+{}", self.added)),
            )
            .child(
                div()
                    .text_color(rgb(0xe06c75))
                    .child(format!("-{}", self.removed)),
            );

        let body = if self.rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("No changes")
                .into_any_element()
        } else {
            uniform_list(
                "diff-rows",
                self.rows.len(),
                cx.processor(Self::render_rows),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        };

        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .p_1p5()
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .bg(theme.title_bar)
                    .child(header)
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use tethys_core::diff::{DiffLine, FileDiff, Hunk, LineKind};

    use super::{Row, diff_rows, hunk_header};

    fn line(kind: LineKind, old: Option<usize>, new: Option<usize>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: text.into(),
        }
    }

    #[test]
    fn rows_are_a_header_per_hunk_then_its_lines() {
        let diff = FileDiff {
            hunks: vec![
                Hunk {
                    old_start: 1,
                    new_start: 1,
                    lines: vec![
                        line(LineKind::Context, Some(1), Some(1), "a"),
                        line(LineKind::Removed, Some(2), None, "b"),
                        line(LineKind::Added, None, Some(2), "c"),
                    ],
                },
                Hunk {
                    old_start: 10,
                    new_start: 11,
                    lines: vec![line(LineKind::Added, None, Some(11), "d")],
                },
            ],
            added: 2,
            removed: 1,
        };
        let (rows, added, removed) = diff_rows(diff);
        assert_eq!((added, removed), (2, 1));
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[0],
            Row::Hunk {
                old_start: 1,
                new_start: 1
            }
        );
        assert_eq!(
            rows[2],
            Row::Line {
                kind: LineKind::Removed,
                old_line: Some(2),
                new_line: None,
                text: "b".into(),
            }
        );
        assert_eq!(
            rows[4],
            Row::Hunk {
                old_start: 10,
                new_start: 11
            }
        );
    }

    #[test]
    fn tabs_expand_to_tab_stops_like_the_editor() {
        let diff = FileDiff {
            hunks: vec![Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec![line(LineKind::Context, Some(1), Some(1), "\tx\ty")],
            }],
            added: 0,
            removed: 0,
        };
        let (rows, _, _) = diff_rows(diff);
        let Row::Line { text, .. } = &rows[1] else {
            panic!("a line row");
        };
        assert_eq!(text.as_ref(), "    x   y");
    }

    #[test]
    fn an_empty_diff_has_no_rows() {
        let (rows, added, removed) = diff_rows(FileDiff::default());
        assert!(rows.is_empty());
        assert_eq!((added, removed), (0, 0));
    }

    #[test]
    fn hunk_header_is_unified_diff_style() {
        assert_eq!(hunk_header(3, 7), "@@ -3 +7 @@");
    }
}
