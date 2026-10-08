//! A lazily loaded file tree of the project folder, coloured by source-control status.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::scm::{ChangeKind, WorkingCopyStatus};
use tethys_core::usecases::ScmSummary;

/// Folders that are tool metadata, not project content.
const HIDDEN: &[&str] = &[".svn", ".git", ".vs", ".idea"];
const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;

pub enum FileTreeEvent {
    /// Open a file in the editor.
    Open(PathBuf),
    /// Show a file's changes against source control.
    Diff(PathBuf),
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

#[derive(Clone)]
struct Row {
    entry: Entry,
    depth: usize,
    expanded: bool,
    /// Shown after the name: the folder, in the flat Changes view.
    detail: Option<SharedString>,
}

pub struct FileTreePanel {
    root: PathBuf,
    focus: FocusHandle,
    expanded: HashSet<PathBuf>,
    /// Directory listings, loaded when a folder is first expanded.
    children: HashMap<PathBuf, Vec<Entry>>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    status: Arc<WorkingCopyStatus>,
    /// Show only added and modified files, as a flat list.
    changes_only: bool,
    /// Source control details for the footer; `None` until loaded.
    scm: Option<ScmSummary>,
    scroll: UniformListScrollHandle,
}

impl FileTreePanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            focus: cx.focus_handle(),
            expanded: HashSet::new(),
            children: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            status: Arc::default(),
            changes_only: false,
            scm: None,
            scroll: UniformListScrollHandle::new(),
        };
        this.rebuild();
        this
    }

    pub fn set_scm(&mut self, scm: ScmSummary, cx: &mut Context<Self>) {
        self.scm = Some(scm);
        cx.notify();
    }

    /// The source control footer: provider, branch, revision and last change.
    fn render_scm_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let line = || div().text_xs().truncate().text_color(muted);
        let (provider, ok, lines): (String, bool, Vec<String>) = match &self.scm {
            None => ("Source control".into(), true, vec!["Checking…".into()]),
            Some(ScmSummary::NotUnderControl { providers }) => (
                "No source control".into(),
                false,
                vec![format!("Not a {} working copy", providers.join(" or "))],
            ),
            Some(ScmSummary::Failed { provider, error }) => {
                ((*provider).into(), false, vec![error.clone()])
            }
            Some(ScmSummary::Ready { provider, info }) => {
                let mut lines = vec![info.url.clone()];
                if let Some(last) = &info.last_change {
                    // ISO date to "2026-10-06".
                    let when = last.date.get(..10).unwrap_or(&last.date);
                    lines.push(format!(
                        "changed {} · {} · {when}",
                        revision_label(&last.revision),
                        last.author
                    ));
                }
                let mut title = (*provider).to_string();
                if let Some(branch) = &info.branch {
                    title.push_str(&format!("  {branch}"));
                }
                if let Some(rev) = &info.revision {
                    title.push_str(&format!("  {}", revision_label(rev)));
                }
                (title, true, lines)
            }
        };
        let dot = if ok { Hsla::from(rgb(0x98c379)) } else { muted };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5()
            .mt_1()
            .px_2()
            .pt_1p5()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_sm()
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
                    .child(div().truncate().child(provider)),
            )
            .children(lines.into_iter().map(|l| line().child(l)))
    }

    pub fn set_status(&mut self, status: Arc<WorkingCopyStatus>, cx: &mut Context<Self>) {
        self.status = status;
        if self.changes_only {
            self.rebuild();
        }
        cx.notify();
    }

    /// Re-reads every loaded folder from disk, keeping what's expanded.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.children.clear();
        self.rebuild();
        cx.notify();
    }

    fn rebuild(&mut self) {
        if self.changes_only {
            self.rows = self.change_rows();
            return;
        }
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.push_rows(&root, 0, &mut rows);
        self.rows = rows;
    }

    /// Added and modified files as a flat list: name, then folder relative to the root.
    fn change_rows(&self) -> Vec<Row> {
        self.status
            .local_edits()
            .map(|f| {
                let folder = f
                    .path
                    .parent()
                    .and_then(|p| p.strip_prefix(&self.root).ok())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                Row {
                    entry: Entry {
                        name: f
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                            .into(),
                        path: f.path.clone(),
                        is_dir: false,
                    },
                    depth: 0,
                    expanded: false,
                    detail: (!folder.is_empty()).then(|| folder.into()),
                }
            })
            .collect()
    }

    fn set_changes_only(&mut self, changes_only: bool, cx: &mut Context<Self>) {
        self.changes_only = changes_only;
        self.rebuild();
        cx.notify();
    }

    fn push_rows(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let entries = self
            .children
            .entry(dir.to_path_buf())
            .or_insert_with(|| read_dir(dir))
            .clone();
        for entry in entries {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row {
                entry,
                depth,
                expanded,
                detail: None,
            });
            if expanded {
                self.push_rows(&path, depth + 1, rows);
            }
        }
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        self.selected = Some(row.entry.path.clone());
        if row.entry.is_dir {
            if !self.expanded.remove(&row.entry.path) {
                self.expanded.insert(row.entry.path);
            }
            self.rebuild();
        } else {
            cx.emit(FileTreeEvent::Open(row.entry.path));
        }
        cx.notify();
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, fg, hover, selected_bg) = (
            theme.muted_foreground,
            theme.foreground,
            theme.secondary_hover,
            theme.primary.opacity(0.18),
        );
        range
            .filter_map(|ix| self.rows.get(ix).cloned().map(|row| (ix, row)))
            .map(|(ix, row)| {
                let path = &row.entry.path;
                let change = if row.entry.is_dir {
                    self.status
                        .contains_changes_under(path)
                        .then_some(ChangeKind::Modified)
                } else {
                    self.status.of(path)
                };
                let color = change.map(change_color).unwrap_or(if row.entry.is_dir {
                    fg
                } else {
                    muted.opacity(0.95)
                });
                let selected = self.selected.as_deref() == Some(path.as_path());
                let diffable = !row.entry.is_dir
                    && change.is_some_and(ChangeKind::has_base)
                    && !is_binary(path);
                let diff_path = path.clone();

                div()
                    .id(ix)
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(px(8. + row.depth as f32 * INDENT))
                    .pr_2()
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(selected_bg))
                    .hover(|s| s.bg(hover))
                    .child(div().w(px(12.)).text_xs().text_color(muted).child(
                        match (row.entry.is_dir, row.expanded) {
                            (true, true) => "▾",
                            (true, false) => "▸",
                            _ => "",
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .overflow_hidden()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(color)
                                    .child(row.entry.name.clone()),
                            )
                            .children(row.detail.clone().map(|folder| {
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted.opacity(0.7))
                                    .child(folder)
                            })),
                    )
                    .when(diffable, |d| {
                        d.child(
                            div()
                                .id(("diff", ix))
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .text_xs()
                                .text_color(muted)
                                .hover(|s| s.text_color(fg).bg(selected_bg))
                                .child("diff")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(FileTreeEvent::Diff(diff_path.clone()));
                                })),
                        )
                    })
                    .when_some(change.filter(|_| !row.entry.is_dir), |d, kind| {
                        d.child(
                            div()
                                .w(px(12.))
                                .flex_none()
                                .text_xs()
                                .text_color(change_color(kind))
                                .child(kind.letter().to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx)))
                    .into_any_element()
            })
            .collect()
    }
}

pub fn change_color(kind: ChangeKind) -> Hsla {
    match kind {
        ChangeKind::Modified | ChangeKind::Replaced => rgb(0xe5c07b).into(),
        ChangeKind::Added => rgb(0x98c379).into(),
        ChangeKind::Deleted | ChangeKind::Missing | ChangeKind::Conflicted => rgb(0xe06c75).into(),
        ChangeKind::Unversioned => rgb(0x6b7280).into(),
    }
}

/// SVN revision numbers read as `r712`; git hashes as they are.
fn revision_label(rev: &str) -> String {
    if rev.bytes().all(|b| b.is_ascii_digit()) {
        format!("r{rev}")
    } else {
        rev.to_string()
    }
}

/// Folders first, then files, case-insensitively. Unreadable folders are empty.
fn read_dir(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().ok()?.is_dir();
            if is_dir && HIDDEN.iter().any(|h| name.eq_ignore_ascii_case(h)) {
                return None;
            }
            Some(Entry {
                path: e.path(),
                name: name.into(),
                is_dir,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

impl EventEmitter<FileTreeEvent> for FileTreePanel {}
impl EventEmitter<PanelEvent> for FileTreePanel {}

impl gpui_kit::component::dock::BasePanel for FileTreePanel {
    fn panel_name(&self) -> &'static str {
        "TethysFiles"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for FileTreePanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Files".into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Files"
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Focusable for FileTreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let edits = self.status.local_edits().count();
        let filter = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .pb_1()
            .mb_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .gap_0p5()
                    .p_0p5()
                    .rounded(theme.radius)
                    .bg(theme.background)
                    .children(
                        [
                            (false, "All".to_string()),
                            (true, format!("Changes ({edits})")),
                        ]
                        .map(|(changes_only, label)| {
                            let button = Button::new(if changes_only { "changes" } else { "all" })
                                .xsmall()
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_changes_only(changes_only, cx)
                                }));
                            if changes_only == self.changes_only {
                                button.primary()
                            } else {
                                button.ghost()
                            }
                        }),
                    ),
            );

        let body = if self.changes_only && self.rows.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No added or modified files")
                .into_any_element()
        } else {
            uniform_list(
                "file-tree",
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
                    .py_1()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .child(filter)
                    .child(body)
                    .child(self.render_scm_footer(cx)),
            )
    }
}

/// Unreal assets and other binaries, which have no text diff.
fn is_binary(path: &Path) -> bool {
    const BINARY: &[&str] = &[
        "uasset", "umap", "ubulk", "uexp", "upk", "png", "jpg", "jpeg", "tga", "bmp", "exr", "hdr",
        "wav", "ogg", "mp3", "fbx", "obj", "abc", "dll", "exe", "pdb", "lib", "zip",
    ];
    path.extension()
        .is_some_and(|e| BINARY.iter().any(|b| e.eq_ignore_ascii_case(b)))
}

#[cfg(test)]
mod tests {
    use super::read_dir;

    #[test]
    fn lists_folders_first_and_hides_tool_folders() {
        let dir = std::env::temp_dir().join(format!("tethys-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["Source", ".svn", "config"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["b.txt", "A.uproject"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let names: Vec<_> = read_dir(&dir).iter().map(|e| e.name.to_string()).collect();
        assert_eq!(names, ["config", "Source", "A.uproject", "b.txt"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
