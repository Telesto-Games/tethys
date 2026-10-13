//! A file's commits, and what the selected one changed in it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::diff::FileDiff;
use tethys_core::ports::SourceControl;
use tethys_core::scm::Commit;
use tethys_core::usecases;

use crate::diff_panel::DiffPanel;
use crate::file_tree::{file_name, revision_label};

const ROW_HEIGHT: f32 = 44.;
const LIST_WIDTH: f32 = 320.;

enum Commits {
    Loading,
    Failed(String),
    Loaded(Vec<Commit>),
}

pub struct HistoryPanel {
    path: PathBuf,
    scm: Arc<dyn SourceControl>,
    commits: Commits,
    selected: Option<usize>,
    /// Counts selections, so a diff that arrives after a later selection or
    /// a reload (which may select the same index of a new list) is dropped.
    selection: u64,
    /// What the selected commit changed; `None` until one is chosen.
    diff: Option<Result<Entity<DiffPanel>, String>>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl HistoryPanel {
    /// Shows `path`'s history, loading it in the background.
    pub fn new(path: PathBuf, scm: Arc<dyn SourceControl>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            scm,
            commits: Commits::Loading,
            selected: None,
            selection: 0,
            diff: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        };
        this.reload(cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the commits again, and shows the newest.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let (scm, path) = (self.scm.clone(), self.path.clone());
        let loaded = cx
            .background_executor()
            .spawn(async move { usecases::file_history(scm.as_ref(), &path) });
        cx.spawn(async move |this, cx| {
            let loaded = loaded.await;
            let _ = this.update(cx, |this, cx| {
                this.commits = match loaded {
                    Ok(commits) => Commits::Loaded(commits),
                    Err(e) => Commits::Failed(format!("Can't read the history: {e}")),
                };
                this.selected = None;
                this.diff = None;
                // Nothing in flight for the old list may land now.
                this.selection += 1;
                this.select(0, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows what commit `ix` changed in the file.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Commits::Loaded(commits) = &self.commits else {
            return;
        };
        let Some(commit) = commits.get(ix).cloned() else {
            return;
        };
        self.selected = Some(ix);
        self.selection += 1;
        let selection = self.selection;
        cx.notify();
        let (scm, path) = (self.scm.clone(), self.path.clone());
        let against = commit_meta(&commit);
        let diff = cx
            .background_executor()
            .spawn(async move { usecases::commit_diff(scm.as_ref(), &path, &commit) });
        cx.spawn(async move |this, cx| {
            let diff = diff.await;
            let _ = this.update(cx, |this, cx| {
                // A later click or reload won the race.
                if this.selection != selection {
                    return;
                }
                match diff {
                    Ok(diff) => this.show_diff(against, diff, cx),
                    Err(e) => this.diff = Some(Err(format!("Can't read this change: {e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_diff(&mut self, against: String, diff: FileDiff, cx: &mut Context<Self>) {
        match &self.diff {
            Some(Ok(view)) => view.update(cx, |view, cx| view.show(against, diff, cx)),
            _ => {
                let path = self.path.clone();
                self.diff = Some(Ok(cx.new(|cx| DiffPanel::new(path, against, diff, cx))));
            }
        }
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
        let Commits::Loaded(commits) = &self.commits else {
            return Vec::new();
        };
        let theme = cx.theme();
        let (muted, hover, selected_bg) = (
            theme.muted_foreground,
            theme.secondary_hover,
            theme.primary.opacity(0.18),
        );
        range
            .filter_map(|ix| commits.get(ix).map(|c| (ix, c)))
            .map(|(ix, commit)| {
                div()
                    .id(ix)
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(self.selected == Some(ix), |d| d.bg(selected_bg))
                    .hover(|s| s.bg(hover))
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(summary_line(&commit.message)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(muted)
                            .child(commit_meta(commit)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
                    .into_any_element()
            })
            .collect()
    }
}

/// `r712` for Subversion, whole; a git hash cut to its first eight characters.
fn short_revision(revision: &str) -> String {
    let label = revision_label(revision);
    if label == revision {
        // A git hash: `revision_label` left it alone.
        label.chars().take(8).collect()
    } else {
        label
    }
}

/// An ISO date's day: "2026-10-06".
fn day(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}

/// A commit's first line, or "(no message)".
fn summary_line(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    if first.is_empty() {
        "(no message)".to_string()
    } else {
        first.to_string()
    }
}

/// "r712 · alice · 2026-10-06": the line under a commit, and the diff's label.
fn commit_meta(commit: &Commit) -> String {
    format!(
        "{} · {} · {}",
        short_revision(&commit.revision),
        commit.author,
        day(&commit.date)
    )
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

impl gpui_kit::component::dock::BasePanel for HistoryPanel {
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn panel_name(&self) -> &'static str {
        "TethysHistory"
    }
}

impl Panel for HistoryPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(format!("History: {}", self.file_name()).into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        format!("History: {}", self.file_name())
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let note = |text: String| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(muted)
                .whitespace_normal()
                .child(text)
                .into_any_element()
        };

        let list = match &self.commits {
            Commits::Loading => note(format!("Reading {}'s history…", self.scm.name())),
            Commits::Failed(e) => note(e.clone()),
            Commits::Loaded(commits) if commits.is_empty() => {
                note("No commits yet: the file isn't committed.".into())
            }
            Commits::Loaded(commits) => uniform_list(
                "history-rows",
                commits.len(),
                cx.processor(Self::render_rows),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element(),
        };

        let diff = match &self.diff {
            Some(Ok(view)) => view.clone().into_any_element(),
            Some(Err(e)) => note(e.clone()),
            None => note(String::new()),
        };

        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(theme.background)
            .child(
                div()
                    .w(px(LIST_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .child(
                        div()
                            .px_3()
                            .py_1p5()
                            .border_b_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(self.path.display().to_string()),
                    )
                    .child(list),
            )
            .child(div().flex_1().min_w_0().flex().flex_col().child(diff))
    }
}

#[cfg(test)]
mod tests {
    use tethys_core::scm::Commit;

    use super::{commit_meta, day, short_revision, summary_line};

    #[test]
    fn svn_revisions_stay_whole_and_git_hashes_are_cut() {
        assert_eq!(short_revision("712"), "r712");
        assert_eq!(short_revision("12345678"), "r12345678");
        assert_eq!(
            short_revision("0123456789abcdef0123456789abcdef01234567"),
            "01234567"
        );
        assert_eq!(short_revision("abc"), "abc");
        assert_eq!(short_revision(""), "");
    }

    #[test]
    fn day_is_the_date_part() {
        assert_eq!(day("2026-10-06T12:34:56Z"), "2026-10-06");
        assert_eq!(day("2026-10-06"), "2026-10-06");
        assert_eq!(day("yesterday"), "yesterday");
        // Cutting inside a character falls back to the whole string.
        assert_eq!(day("ééééé"), "ééééé");
    }

    #[test]
    fn summary_is_the_first_line_or_a_placeholder() {
        assert_eq!(summary_line("Fix it\n\nDetails"), "Fix it");
        assert_eq!(summary_line("One line"), "One line");
        assert_eq!(summary_line(""), "(no message)");
        assert_eq!(summary_line("\nSecond"), "(no message)");
    }

    #[test]
    fn meta_joins_revision_author_and_day() {
        let commit = Commit {
            revision: "89abcdef01234567".into(),
            author: "alice".into(),
            date: "2026-10-06T09:00:00Z".into(),
            message: "m".into(),
            locator: "x".into(),
        };
        assert_eq!(commit_meta(&commit), "89abcdef · alice · 2026-10-06");
        let svn = Commit {
            revision: "12345678".into(),
            ..commit
        };
        assert_eq!(commit_meta(&svn), "r12345678 · alice · 2026-10-06");
    }
}
