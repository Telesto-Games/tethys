//! A lazily loaded file tree of the project folder, coloured by source-control status.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tethys_core::scm::{ChangeKind, Tracking, WorkingCopyStatus};
use tethys_core::usecases::ScmSummary;

/// Folders that are tool metadata, not project content.
const HIDDEN: &[&str] = &[".svn", ".git", ".vs", ".idea"];
const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileTreeEvent {
    /// Open a file in the editor.
    Open(PathBuf),
    /// Show a file's changes against source control.
    Diff(PathBuf),
    /// Ask for a name and create a file in this folder.
    NewFile(PathBuf),
    /// Ask for a name and create a folder in this folder.
    NewFolder(PathBuf),
    /// Ask for a new name for this file or folder.
    Rename(PathBuf),
    /// Confirm, then delete this file or folder.
    Delete(PathBuf),
    /// Show this file's commits.
    History(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Whether the footer shows its detail lines under the headline.
    scm_expanded: bool,
    scroll: UniformListScrollHandle,
    /// The row last right-clicked, which the context menu acts on; `None`
    /// for the space below the rows, which means the project folder.
    menu_target: Option<Entry>,
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
            scm_expanded: false,
            scroll: UniformListScrollHandle::new(),
            menu_target: None,
        };
        this.rebuild();
        this
    }

    /// Re-reads the tree from disk and shows `path`: its folders expanded,
    /// it selected and scrolled to.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.expanded.extend(ancestors_to_expand(path, &self.root));
        self.selected = Some(path.to_path_buf());
        self.refresh(cx);
        if let Some(ix) = self.rows.iter().position(|r| r.entry.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// Whether View History can work: the project is under source control.
    fn has_history(&self) -> bool {
        matches!(self.scm, Some(ScmSummary::Ready { .. }))
    }

    /// The right-click menu for [`Self::menu_target`].
    fn context_menu(
        this: &WeakEntity<Self>,
        menu: PopupMenu,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(tree) = this.upgrade() else {
            return menu;
        };
        let (target, root, has_history) = {
            let tree = tree.read(cx);
            (
                tree.menu_target.clone(),
                tree.root.clone(),
                tree.has_history(),
            )
        };
        let mut menu = menu;
        for item in menu_items(target.as_ref(), &root, has_history) {
            if item.separator_before {
                menu = menu.separator();
            }
            let tree = tree.downgrade();
            let event = Rc::new(item.event);
            menu = menu.item(
                PopupMenuItem::new(item.label)
                    .disabled(!item.enabled)
                    .on_click(move |_, _, cx| {
                        let event = event.clone();
                        let _ = tree.update(cx, |_, cx| cx.emit(event.as_ref().clone()));
                    }),
            );
        }
        menu
    }

    pub fn set_scm(&mut self, scm: ScmSummary, cx: &mut Context<Self>) {
        self.scm = Some(scm);
        cx.notify();
    }

    /// The source control footer: a headline of provider, branch and
    /// revision, which clicking expands to show the URL and last change.
    fn render_scm_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let line = || div().text_xs().truncate().text_color(muted).pl(px(18.));
        let (provider, ok, lines) = scm_footer(self.scm.as_ref());
        let dot = if ok { Hsla::from(rgb(0x98c379)) } else { muted };
        let expanded = self.scm_expanded;
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
                    .id("scm-footer-header")
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_sm()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scm_expanded = !this.scm_expanded;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_none()
                            .text_xs()
                            .text_color(muted)
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(dot))
                    .child(div().truncate().child(provider))
                    // Lets view tests find and click the header; nothing outside tests.
                    .test_support(),
            )
            .when(expanded, |d| {
                d.children(
                    lines
                        .into_iter()
                        .enumerate()
                        .map(|(ix, l)| line().id(("scm-footer-line", ix)).child(l).test_support()),
                )
            })
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
        self.rows = if self.changes_only {
            change_rows(&self.status, &self.root)
        } else {
            flatten(&self.root, &self.expanded, &mut self.children, &read_dir)
        };
    }

    fn set_changes_only(&mut self, changes_only: bool, cx: &mut Context<Self>) {
        self.changes_only = changes_only;
        self.rebuild();
        cx.notify();
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
                let RowStatus {
                    change,
                    untracked,
                    diffable,
                    letter,
                } = row_status(&self.status, path, row.entry.is_dir);
                let color = change.map(change_color).unwrap_or(if row.entry.is_dir {
                    fg
                } else {
                    muted.opacity(0.95)
                });
                let selected = self.selected.as_deref() == Some(path.as_path());
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
                                    .when(untracked.is_some(), |d| d.italic())
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
                    .when_some(letter, |d, kind| {
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
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.selected = Some(row.entry.path.clone());
                            this.menu_target = Some(row.entry.clone());
                            cx.notify();
                        }),
                    )
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
        ChangeKind::Ignored => Hsla::from(rgb(0x6b7280)).opacity(0.6),
    }
}

/// The last part of `path`, for display in a tab or a title.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// SVN revision numbers read as `r712`; git hashes as they are.
pub fn revision_label(rev: &str) -> String {
    if !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()) {
        format!("r{rev}")
    } else {
        rev.to_string()
    }
}

/// The folders between `root` and `path` (exclusive of both), which must be
/// expanded for `path` to show. Empty when `path` isn't under `root`.
fn ancestors_to_expand(path: &Path, root: &Path) -> Vec<PathBuf> {
    path.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(root) && *dir != root)
        .map(Path::to_path_buf)
        .collect()
}

/// Where New File / New Folder create: the folder clicked, beside the file
/// clicked, or the project folder.
fn new_item_folder(target: Option<&Entry>, root: &Path) -> PathBuf {
    match target {
        Some(entry) if entry.is_dir => entry.path.clone(),
        Some(entry) => entry.path.parent().unwrap_or(root).to_path_buf(),
        None => root.to_path_buf(),
    }
}

/// One entry of the right-click menu.
#[derive(Debug, PartialEq, Eq)]
struct MenuItem {
    label: &'static str,
    event: FileTreeEvent,
    enabled: bool,
    separator_before: bool,
}

/// The right-click menu for `target` (`None`: the space below the rows).
/// New items always; Rename and Delete for a target; History for a file,
/// greyed out unless the project is under source control.
fn menu_items(target: Option<&Entry>, root: &Path, has_history: bool) -> Vec<MenuItem> {
    let folder = new_item_folder(target, root);
    let item = |label, event, enabled, separator_before| MenuItem {
        label,
        event,
        enabled,
        separator_before,
    };
    let mut items = vec![
        item(
            "New File…",
            FileTreeEvent::NewFile(folder.clone()),
            true,
            false,
        ),
        item("New Folder…", FileTreeEvent::NewFolder(folder), true, false),
    ];
    let Some(entry) = target else {
        return items;
    };
    items.push(item(
        "Rename…",
        FileTreeEvent::Rename(entry.path.clone()),
        true,
        true,
    ));
    items.push(item(
        "Delete…",
        FileTreeEvent::Delete(entry.path.clone()),
        true,
        false,
    ));
    if !entry.is_dir {
        items.push(item(
            "View History",
            FileTreeEvent::History(entry.path.clone()),
            has_history,
            true,
        ));
    }
    items
}

/// The footer's title, whether things are fine (a green dot) and its lines.
fn scm_footer(scm: Option<&ScmSummary>) -> (String, bool, Vec<String>) {
    match scm {
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
            lines.push("Not tracked: ? untracked, I ignored (italic)".into());
            (title, true, lines)
        }
    }
}

/// Added and modified files as a flat list: name, then folder relative to the root.
fn change_rows(status: &WorkingCopyStatus, root: &Path) -> Vec<Row> {
    status
        .local_edits()
        .map(|f| {
            let folder = f
                .path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Row {
                entry: Entry {
                    name: file_name(&f.path).into(),
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

/// The tree under `root` as rows, expanded folders opened out. Listings come
/// from `children`, filled from `list` for folders seen for the first time.
fn flatten(
    root: &Path,
    expanded: &HashSet<PathBuf>,
    children: &mut HashMap<PathBuf, Vec<Entry>>,
    list: &dyn Fn(&Path) -> Vec<Entry>,
) -> Vec<Row> {
    fn push(
        dir: &Path,
        depth: usize,
        expanded: &HashSet<PathBuf>,
        children: &mut HashMap<PathBuf, Vec<Entry>>,
        list: &dyn Fn(&Path) -> Vec<Entry>,
        rows: &mut Vec<Row>,
    ) {
        let entries = children
            .entry(dir.to_path_buf())
            .or_insert_with(|| list(dir))
            .clone();
        for entry in entries {
            let open = entry.is_dir && expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row {
                entry,
                depth,
                expanded: open,
                detail: None,
            });
            if open {
                push(&path, depth + 1, expanded, children, list, rows);
            }
        }
    }
    let mut rows = Vec::new();
    push(root, 0, expanded, children, list, &mut rows);
    rows
}

/// How a row is drawn from the working copy status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowStatus {
    /// The change that colours the name: a file's own, or Modified for a
    /// folder with changes underneath.
    change: Option<ChangeKind>,
    /// Not tracked: itself, or inside a folder that isn't. Drawn in italic.
    untracked: Option<ChangeKind>,
    /// Whether a diff button makes sense: a changed text file with a base.
    diffable: bool,
    /// The letter after the name: a file's change, a folder's only for not
    /// being tracked.
    letter: Option<ChangeKind>,
}

fn row_status(status: &WorkingCopyStatus, path: &Path, is_dir: bool) -> RowStatus {
    let untracked = match status.tracking(path) {
        Tracking::Tracked => None,
        Tracking::Untracked => Some(ChangeKind::Unversioned),
        Tracking::Ignored => Some(ChangeKind::Ignored),
    };
    let change = if is_dir {
        untracked.or_else(|| {
            status
                .contains_changes_under(path)
                .then_some(ChangeKind::Modified)
        })
    } else {
        status.of(path).or(untracked)
    };
    RowStatus {
        change,
        untracked,
        diffable: !is_dir && change.is_some_and(ChangeKind::has_base) && !is_binary(path),
        letter: change.filter(|_| !is_dir).or(untracked),
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
        // One menu for the whole list: a right-click on a row names its
        // target on the way up, after this clears it on the way down.
        let this = cx.weak_entity();
        let body = div()
            .id("file-tree-body")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.menu_target = None))
            .child(body)
            .context_menu(move |menu, _, cx| Self::context_menu(&this, menu, cx));

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
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use gpui_kit::Hsla;
    use tethys_core::scm::{
        ChangeKind, FileStatus, LastChange, WorkingCopyInfo, WorkingCopyStatus,
    };
    use tethys_core::usecases::ScmSummary;

    use super::{
        Entry, FileTreeEvent, ancestors_to_expand, change_color, change_rows, file_name, flatten,
        is_binary, menu_items, new_item_folder, read_dir, revision_label, row_status, scm_footer,
    };

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tethys-tree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entry(path: &str, is_dir: bool) -> Entry {
        let path = PathBuf::from(path);
        Entry {
            name: file_name(&path).into(),
            path,
            is_dir,
        }
    }

    #[test]
    fn lists_folders_first_and_hides_tool_folders() {
        let dir = temp("list");
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

    #[test]
    fn hidden_folders_match_any_case_but_a_file_of_that_name_shows() {
        let dir = temp("hidden");
        std::fs::create_dir_all(dir.join(".SVN")).unwrap();
        std::fs::create_dir_all(dir.join(".Idea")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // A file called `.git` (a worktree pointer) is content, not metadata.
        // (Not beside a `.GIT` folder: NTFS treats those as the same name.)
        std::fs::write(dir.join(".git"), "gitdir: ../x").unwrap();
        let names: Vec<_> = read_dir(&dir).iter().map(|e| e.name.to_string()).collect();
        assert_eq!(names, ["src", ".git"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unreadable_folder_is_empty() {
        let dir = temp("missing");
        assert!(read_dir(&dir).is_empty());
    }

    #[test]
    fn revision_labels() {
        assert_eq!(revision_label("712"), "r712");
        assert_eq!(revision_label("12345678"), "r12345678");
        assert_eq!(revision_label("a1b2c3d"), "a1b2c3d");
        assert_eq!(revision_label(""), "");
    }

    #[test]
    fn binary_is_by_extension_in_any_case() {
        assert!(is_binary(Path::new("C:/p/Content/Ship.uasset")));
        assert!(is_binary(Path::new("C:/p/Content/Ship.UASSET")));
        assert!(is_binary(Path::new("tex.Png")));
        assert!(!is_binary(Path::new("Ship.cpp")));
        assert!(!is_binary(Path::new("README")));
        assert!(!is_binary(Path::new("uasset")));
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name(Path::new(r"C:\p\Source\A.cpp")), "A.cpp");
        assert_eq!(file_name(Path::new("C:/p/Source/A.cpp")), "A.cpp");
        // A trailing separator doesn't hide the last component.
        assert_eq!(file_name(Path::new("C:/p/Source/")), "Source");
        assert_eq!(file_name(Path::new("C:/")), "");
        assert_eq!(file_name(Path::new("..")), "");
    }

    #[test]
    fn change_colours_follow_the_kind() {
        assert_eq!(
            change_color(ChangeKind::Modified),
            change_color(ChangeKind::Replaced)
        );
        assert_eq!(
            change_color(ChangeKind::Deleted),
            change_color(ChangeKind::Conflicted)
        );
        assert_ne!(
            change_color(ChangeKind::Added),
            change_color(ChangeKind::Modified)
        );
        let plain: Hsla = change_color(ChangeKind::Unversioned);
        let ignored: Hsla = change_color(ChangeKind::Ignored);
        assert!(ignored.a < plain.a, "ignored is fainter");
    }

    #[test]
    fn ancestors_stop_at_the_root_and_outside_it() {
        let root = Path::new("C:/p");
        assert_eq!(
            ancestors_to_expand(Path::new("C:/p/Source/Ship/Hull.cpp"), root),
            [
                PathBuf::from("C:/p/Source/Ship"),
                PathBuf::from("C:/p/Source")
            ]
        );
        assert!(ancestors_to_expand(Path::new("C:/p/A.uproject"), root).is_empty());
        assert!(ancestors_to_expand(Path::new("D:/elsewhere/x/y.txt"), root).is_empty());
        assert!(ancestors_to_expand(root, root).is_empty());
    }

    #[test]
    fn new_items_go_in_the_folder_clicked_or_beside_the_file() {
        let root = Path::new("C:/p");
        assert_eq!(
            new_item_folder(Some(&entry("C:/p/Source", true)), root),
            PathBuf::from("C:/p/Source")
        );
        assert_eq!(
            new_item_folder(Some(&entry("C:/p/Source/A.cpp", false)), root),
            PathBuf::from("C:/p/Source")
        );
        assert_eq!(new_item_folder(None, root), PathBuf::from("C:/p"));
    }

    #[test]
    fn menu_depends_on_what_was_clicked() {
        let root = Path::new("C:/p");
        let labels = |items: &[super::MenuItem]| {
            items
                .iter()
                .map(|i| (i.label, i.enabled, i.separator_before))
                .collect::<Vec<_>>()
        };

        let below = menu_items(None, root, true);
        assert_eq!(
            labels(&below),
            [("New File…", true, false), ("New Folder…", true, false)]
        );
        assert_eq!(below[0].event, FileTreeEvent::NewFile(root.to_path_buf()));

        let folder = entry("C:/p/Source", true);
        let on_folder = menu_items(Some(&folder), root, true);
        assert_eq!(
            labels(&on_folder),
            [
                ("New File…", true, false),
                ("New Folder…", true, false),
                ("Rename…", true, true),
                ("Delete…", true, false),
            ]
        );
        assert_eq!(
            on_folder[2].event,
            FileTreeEvent::Rename(folder.path.clone())
        );

        let file = entry("C:/p/Source/A.cpp", false);
        let on_file = menu_items(Some(&file), root, false);
        assert_eq!(on_file.len(), 5);
        assert_eq!(
            (
                on_file[4].label,
                on_file[4].enabled,
                on_file[4].separator_before
            ),
            ("View History", false, true)
        );
        assert_eq!(on_file[4].event, FileTreeEvent::History(file.path.clone()));
        assert_eq!(
            on_file[0].event,
            FileTreeEvent::NewFile(PathBuf::from("C:/p/Source"))
        );
        assert!(menu_items(Some(&file), root, true)[4].enabled);
    }

    #[test]
    fn footer_text_per_summary() {
        assert_eq!(
            scm_footer(None),
            ("Source control".into(), true, vec!["Checking…".into()])
        );
        assert_eq!(
            scm_footer(Some(&ScmSummary::NotUnderControl {
                providers: vec!["Git", "Subversion"]
            })),
            (
                "No source control".into(),
                false,
                vec!["Not a Git or Subversion working copy".into()]
            )
        );
        assert_eq!(
            scm_footer(Some(&ScmSummary::Failed {
                provider: "Git",
                error: "boom".into()
            })),
            ("Git".into(), false, vec!["boom".into()])
        );
        let ready = ScmSummary::Ready {
            provider: "Subversion",
            info: WorkingCopyInfo {
                url: "https://svn/trunk".into(),
                branch: Some("trunk".into()),
                revision: Some("712".into()),
                last_change: Some(LastChange {
                    revision: "700".into(),
                    author: "alice".into(),
                    date: "2026-10-06T10:00:00.000000Z".into(),
                }),
            },
        };
        let (title, ok, lines) = scm_footer(Some(&ready));
        assert_eq!(title, "Subversion  trunk  r712");
        assert!(ok);
        assert_eq!(
            lines,
            [
                "https://svn/trunk",
                "changed r700 · alice · 2026-10-06",
                "Not tracked: ? untracked, I ignored (italic)",
            ]
        );
        let bare = ScmSummary::Ready {
            provider: "Git",
            info: WorkingCopyInfo::default(),
        };
        let (title, _, lines) = scm_footer(Some(&bare));
        assert_eq!(title, "Git");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn change_rows_list_local_edits_with_their_folder() {
        let root = Path::new("C:/p");
        let status = WorkingCopyStatus::new(vec![
            FileStatus {
                path: PathBuf::from("C:/p/Source/Ship/Hull.cpp"),
                kind: ChangeKind::Modified,
            },
            FileStatus {
                path: PathBuf::from("C:/p/A.uproject"),
                kind: ChangeKind::Added,
            },
            FileStatus {
                path: PathBuf::from("C:/p/junk.tmp"),
                kind: ChangeKind::Unversioned,
            },
        ]);
        let rows = change_rows(&status, root);
        let shown: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.entry.name.to_string(),
                    r.detail.as_ref().map(|d| d.to_string()),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("A.uproject".to_string(), None),
                (
                    "Hull.cpp".to_string(),
                    Some(Path::new("Source/Ship").display().to_string())
                ),
            ]
        );
        assert!(rows.iter().all(|r| !r.entry.is_dir && r.depth == 0));
    }

    #[test]
    fn flatten_opens_expanded_folders_and_caches_listings() {
        let root = PathBuf::from("C:/p");
        let listed = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            listed.borrow_mut().push(dir.to_path_buf());
            match dir.to_str().unwrap() {
                "C:/p" => vec![entry("C:/p/Source", true), entry("C:/p/A.uproject", false)],
                "C:/p/Source" => vec![
                    entry("C:/p/Source/Ship", true),
                    entry("C:/p/Source/a.cpp", false),
                ],
                "C:/p/Source/Ship" => vec![entry("C:/p/Source/Ship/Hull.cpp", false)],
                _ => Vec::new(),
            }
        };
        let mut children = HashMap::new();
        let expanded: HashSet<PathBuf> = [PathBuf::from("C:/p/Source")].into();

        let rows = flatten(&root, &expanded, &mut children, &list);
        let shown: Vec<_> = rows
            .iter()
            .map(|r| (r.entry.name.to_string(), r.depth, r.expanded))
            .collect();
        assert_eq!(
            shown,
            [
                ("Source".to_string(), 0, true),
                ("Ship".to_string(), 1, false),
                ("a.cpp".to_string(), 1, false),
                ("A.uproject".to_string(), 0, false),
            ]
        );
        // Only open folders are listed, and only once each.
        assert_eq!(listed.borrow().len(), 2);
        flatten(&root, &expanded, &mut children, &list);
        assert_eq!(listed.borrow().len(), 2, "listings come from the cache");

        // A file in the expanded set is never opened.
        let expanded: HashSet<PathBuf> = [PathBuf::from("C:/p/A.uproject")].into();
        let rows = flatten(&root, &expanded, &mut children, &list);
        assert!(rows.iter().all(|r| !r.expanded));
    }

    #[test]
    fn row_status_for_files_and_folders() {
        let status = WorkingCopyStatus::new(vec![
            FileStatus {
                path: PathBuf::from("C:/p/Source/Hull.cpp"),
                kind: ChangeKind::Modified,
            },
            FileStatus {
                path: PathBuf::from("C:/p/Content/Ship.uasset"),
                kind: ChangeKind::Modified,
            },
            FileStatus {
                path: PathBuf::from("C:/p/New.txt"),
                kind: ChangeKind::Added,
            },
            FileStatus {
                path: PathBuf::from("C:/p/Scratch"),
                kind: ChangeKind::Unversioned,
            },
            FileStatus {
                path: PathBuf::from("C:/p/Binaries"),
                kind: ChangeKind::Ignored,
            },
        ]);

        let modified = row_status(&status, Path::new("C:/p/Source/Hull.cpp"), false);
        assert_eq!(modified.change, Some(ChangeKind::Modified));
        assert_eq!(modified.untracked, None);
        assert!(modified.diffable);
        assert_eq!(modified.letter, Some(ChangeKind::Modified));

        let asset = row_status(&status, Path::new("C:/p/Content/Ship.uasset"), false);
        assert_eq!(asset.change, Some(ChangeKind::Modified));
        assert!(!asset.diffable, "binaries have no text diff");

        let added = row_status(&status, Path::new("C:/p/New.txt"), false);
        assert_eq!(added.change, Some(ChangeKind::Added));
        assert!(!added.diffable, "nothing to diff against");

        let clean = row_status(&status, Path::new("C:/p/Source/Other.cpp"), false);
        assert_eq!(clean.change, None);
        assert_eq!(clean.letter, None);
        assert!(!clean.diffable);

        // A folder with a change underneath is Modified but gets no letter.
        let source = row_status(&status, Path::new("C:/p/Source"), true);
        assert_eq!(source.change, Some(ChangeKind::Modified));
        assert_eq!(source.untracked, None);
        assert_eq!(source.letter, None);
        assert!(!source.diffable);

        // Inside an untracked folder: untracked, in italic, with its letter.
        let inside = row_status(&status, Path::new("C:/p/Scratch/notes.md"), false);
        assert_eq!(inside.untracked, Some(ChangeKind::Unversioned));
        assert_eq!(inside.change, Some(ChangeKind::Unversioned));
        assert_eq!(inside.letter, Some(ChangeKind::Unversioned));
        let scratch = row_status(&status, Path::new("C:/p/Scratch"), true);
        assert_eq!(scratch.letter, Some(ChangeKind::Unversioned));

        let ignored = row_status(&status, Path::new("C:/p/Binaries/x.dll"), false);
        assert_eq!(ignored.untracked, Some(ChangeKind::Ignored));
        assert!(!ignored.diffable);
    }
}

/// The panel rendered in a headless window: the source control footer
/// starts collapsed and its header click toggles the detail lines.
#[cfg(test)]
mod view_tests {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, TestAppContext, px, size};
    use tethys_core::scm::WorkingCopyInfo;
    use tethys_core::usecases::ScmSummary;

    use super::FileTreePanel;
    use crate::test_support::{self, open_window, temp_config};

    #[gpui_kit::test]
    fn scm_footer_starts_collapsed_and_toggles_on_click(cx: &mut TestAppContext) {
        let config = temp_config("scm-footer");
        cx.update(|cx| test_support::init(config, cx));
        let root = std::env::temp_dir();
        let (window, panel) = open_window(cx, size(px(300.), px(600.)), |_, cx| {
            cx.new(|cx| {
                let mut panel = FileTreePanel::new(root, cx);
                panel.set_scm(
                    ScmSummary::Ready {
                        provider: "Git",
                        info: WorkingCopyInfo::default(),
                    },
                    cx,
                );
                panel
            })
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("scm-footer-header").visible());
            assert!(window.try_find(("scm-footer-line", 0usize)).is_none());
            assert!(!panel.read(cx).scm_expanded);

            window.click("scm-footer-header", cx);
            window.render_frame(cx);
            assert!(panel.read(cx).scm_expanded);
            assert!(window.find(("scm-footer-line", 0usize)).visible());

            window.click("scm-footer-header", cx);
            window.render_frame(cx);
            assert!(!panel.read(cx).scm_expanded);
            assert!(window.try_find(("scm-footer-line", 0usize)).is_none());
        })
        .unwrap();
    }
}
