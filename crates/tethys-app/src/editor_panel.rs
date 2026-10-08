//! A basic code editor panel for one file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::base::Disableable;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::*;

gpui_kit::actions!(editor, [Save]);

/// Files larger than this aren't opened in the editor.
pub const MAX_EDIT_BYTES: u64 = 4 * 1024 * 1024;

pub enum EditorEvent {
    /// The file was written to disk.
    Saved,
    /// Show this file's changes; carries the current (possibly unsaved) text.
    Diff(PathBuf, String),
    /// The unsaved-changes marker changed; the tab title needs redrawing.
    DirtyChanged,
}

/// Width of a tab stop when showing tab characters.
pub const TAB_WIDTH: usize = 4;

/// Converts between a file's bytes-on-disk form and the editor's form, so
/// saving writes the file back exactly as it was apart from real edits.
///
/// The editor shows LF line endings, no BOM, and tabs expanded to spaces
/// (it can't draw tab stops). Unedited lines are written back verbatim; edited
/// lines in a tab-indented file get their leading indentation re-tabbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCodec {
    crlf: bool,
    bom: bool,
    /// The file indents with tabs.
    hard_tabs: bool,
    /// Editor line → the exact line on disk, for lines that differ.
    originals: HashMap<String, String>,
}

impl TextCodec {
    /// Splits loaded text into the editor's form and the codec to write it back.
    pub fn decode(raw: &str) -> (String, TextCodec) {
        let bom = raw.starts_with('\u{feff}');
        let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let crlf = text.contains("\r\n");
        let text = text.replace("\r\n", "\n");
        let mut codec = TextCodec {
            crlf,
            bom,
            hard_tabs: text.split('\n').any(|l| l.starts_with('\t')),
            originals: HashMap::new(),
        };
        let shown: Vec<String> = text
            .split('\n')
            .map(|line| {
                let expanded = expand_tabs(line);
                // Remember any line that wouldn't otherwise be written back as is.
                if codec.encode_line(&expanded) != line {
                    codec.originals.insert(expanded.clone(), line.to_string());
                }
                expanded
            })
            .collect();
        (shown.join("\n"), codec)
    }

    /// The file contents for the editor's `text`.
    pub fn encode(&self, text: &str) -> String {
        let lines: Vec<String> = text
            .replace("\r\n", "\n")
            .split('\n')
            .map(|line| self.encode_line(line))
            .collect();
        let newline = if self.crlf { "\r\n" } else { "\n" };
        let body = lines.join(newline);
        if self.bom {
            format!("\u{feff}{body}")
        } else {
            body
        }
    }

    /// After saving `text`, remembers how each line was written.
    pub fn remember(&mut self, text: &str) {
        for line in text.replace("\r\n", "\n").split('\n') {
            let written = self.encode_line(line);
            if written != line {
                self.originals.insert(line.to_string(), written);
            }
        }
    }

    fn encode_line(&self, line: &str) -> String {
        if let Some(original) = self.originals.get(line) {
            return original.clone();
        }
        if !self.hard_tabs {
            return line.to_string();
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        format!(
            "{}{}{}",
            "\t".repeat(indent / TAB_WIDTH),
            " ".repeat(indent % TAB_WIDTH),
            &line[indent..]
        )
    }
}

/// Replaces tabs with spaces up to the next tab stop.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let n = TAB_WIDTH - col % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

pub struct EditorPanel {
    path: PathBuf,
    state: Entity<EditorState>,
    codec: TextCodec,
    /// The text as last loaded or saved, in editor form.
    saved: String,
    dirty: bool,
    error: Option<String>,
    _changes: Subscription,
}

impl EditorPanel {
    /// Opens `path` with its already-read contents (see [`read_text`]).
    pub fn new(path: PathBuf, raw: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (text, codec) = TextCodec::decode(raw);
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language_for(&path))
                .line_number(true)
                .soft_wrap(false)
                .tab_size(TabSize {
                    tab_size: TAB_WIDTH,
                    hard_tabs: false,
                })
                .default_value(text.clone())
        });
        let changes = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let dirty = state.read(cx).value().as_ref() != this.saved;
                if dirty != this.dirty {
                    this.dirty = dirty;
                    cx.emit(EditorEvent::DirtyChanged);
                    cx.notify();
                }
            }
        });
        Self {
            path,
            state,
            codec,
            saved: text,
            dirty: false,
            error: None,
            _changes: changes,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.state.read(cx).value().to_string();
        match std::fs::write(&self.path, self.codec.encode(&text)) {
            Ok(()) => {
                self.codec.remember(&text);
                self.saved = text;
                self.dirty = false;
                self.error = None;
                cx.emit(EditorEvent::DirtyChanged);
                cx.emit(EditorEvent::Saved);
            }
            Err(e) => self.error = Some(format!("Can't save {}: {e}", self.path.display())),
        }
        cx.notify();
    }

    /// Throws away unsaved changes and reloads the file from disk.
    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match read_text(&self.path) {
            Ok(raw) => {
                let (text, codec) = TextCodec::decode(&raw);
                self.codec = codec;
                self.saved = text.clone();
                self.state
                    .update(cx, |state, cx| state.set_value(text, window, cx));
                self.dirty = false;
                self.error = None;
                cx.emit(EditorEvent::DirtyChanged);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn diff(&mut self, cx: &mut Context<Self>) {
        // Diff what would be written, not the editor's tab-expanded form.
        let text = self.codec.encode(&self.state.read(cx).value());
        cx.emit(EditorEvent::Diff(self.path.clone(), text));
    }
}

/// Reads a file as text, refusing binary and very large files.
pub fn read_text(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("Can't open {}: {e}", path.display()))?
        .len();
    if size > MAX_EDIT_BYTES {
        return Err(format!(
            "{} is too large to edit ({} MB)",
            path.display(),
            size / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Can't open {}: {e}", path.display()))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err(format!(
            "{} looks like a binary file (e.g. an asset), so it can't be edited here",
            path.display()
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The highlighter language for a file, by extension.
pub fn language_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "h" | "hpp" | "hh" | "inl" | "c" | "cc" | "cpp" | "cxx" | "ush" | "usf" => "cpp",
        "cs" => "csharp",
        "json" | "uproject" | "uplugin" => "json",
        "toml" | "ini" => "toml",
        "py" => "python",
        "md" => "markdown",
        "rs" => "rust",
        "yml" | "yaml" => "yaml",
        "sh" | "bat" | "cmd" | "ps1" => "bash",
        _ => "plaintext",
    }
}

impl EventEmitter<EditorEvent> for EditorPanel {}
impl EventEmitter<PanelEvent> for EditorPanel {}

impl gpui_kit::component::dock::BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "TethysEditor"
    }

    /// Unsaved edits can't be lost by closing the tab: save or revert first.
    fn closable(&self, _: &App) -> bool {
        !self.dirty
    }
}

impl Panel for EditorPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title_text().into())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title_text()
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EditorPanel {
    fn title_text(&self) -> String {
        if self.dirty {
            format!("{} ●", self.file_name())
        } else {
            self.file_name()
        }
    }
}

impl Focusable for EditorPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

impl Render for EditorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let toolbar = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.path.display().to_string()),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(theme.danger).child(e)),
            )
            .child(
                Button::new("diff")
                    .xsmall()
                    .ghost()
                    .label("Diff")
                    .tooltip("Changes against source control")
                    .on_click(cx.listener(|this, _, _, cx| this.diff(cx))),
            )
            .child(
                Button::new("revert")
                    .xsmall()
                    .ghost()
                    .label("Revert")
                    .tooltip("Discard unsaved changes")
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.revert(window, cx))),
            )
            .child(
                Button::new("save")
                    .xsmall()
                    .label("Save")
                    .tooltip("Save (Ctrl+S)")
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.save(&Save, window, cx))),
            );

        div()
            .size_full()
            .bg(theme.background)
            .p_1p5()
            .on_action(cx.listener(Self::save))
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
                    .child(toolbar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(Editor::new(&self.state).bordered(false).h_full()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{TextCodec, language_for};

    #[test]
    fn untouched_files_round_trip_exactly() {
        for raw in [
            "\u{feff}a\r\nb\r\n",
            "x\n",
            "no newline",
            "\tint32 A;\t\t// aligned\r\n\t\tif (x)\r\n    spaces kept\r\n",
            "mixed \t tab\n\n",
        ] {
            let (text, codec) = TextCodec::decode(raw);
            assert!(!text.contains('\t'), "{raw:?} shown as {text:?}");
            assert_eq!(codec.encode(&text), raw);
        }
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        let (text, _) = TextCodec::decode("\tab\tc\n");
        assert_eq!(text, "    ab  c\n");
    }

    #[test]
    fn edited_lines_are_retabbed_in_tab_indented_files() {
        let (text, mut codec) = TextCodec::decode("\tA;\r\n\tB;\r\n");
        let edited = text.replace("    B;", "    B2;") + "        C;";
        let written = codec.encode(&edited);
        assert_eq!(written, "\tA;\r\n\tB2;\r\n\t\tC;");

        codec.remember(&edited);
        assert_eq!(codec.encode(&edited), written);
    }

    #[test]
    fn space_indented_files_stay_spaces() {
        let (text, codec) = TextCodec::decode("    a\n");
        assert_eq!(codec.encode(&(text + "    b\n")), "    a\n    b\n");
    }

    #[test]
    fn picks_languages() {
        assert_eq!(language_for(Path::new("A.h")), "cpp");
        assert_eq!(language_for(Path::new("Game.Build.cs")), "csharp");
        assert_eq!(language_for(Path::new("Game.uproject")), "json");
        assert_eq!(language_for(Path::new("README")), "plaintext");
    }
}

#[cfg(test)]
mod round_trip {
    use super::{TextCodec, read_text};

    /// Run with `TETHYS_ROUNDTRIP_DIR=<folder> cargo test -- --ignored`: every
    /// text file under it must survive decode + encode byte for byte.
    #[test]
    #[ignore]
    fn real_files_round_trip() {
        let Some(dir) = std::env::var_os("TETHYS_ROUNDTRIP_DIR") else {
            return;
        };
        let mut stack = vec![std::path::PathBuf::from(dir)];
        let mut checked = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(raw) = read_text(&path) {
                    let (text, codec) = TextCodec::decode(&raw);
                    assert_eq!(codec.encode(&text), raw, "{}", path.display());
                    checked += 1;
                }
            }
        }
        eprintln!("round-tripped {checked} files");
    }
}
