//! Draws an alacritty_terminal grid with GPUI and feeds it keyboard and mouse input.

use std::cell::Cell;
use std::rc::Rc;

use futures::StreamExt;
use gpui_kit::*;
use tethys_adapters::agent_terminal::backend::grid::Dimensions;
use tethys_adapters::agent_terminal::backend::index::{Column, Line, Point as GridPoint, Side};
use tethys_adapters::agent_terminal::backend::selection::{Selection, SelectionType};
use tethys_adapters::agent_terminal::backend::term::TermMode;
use tethys_adapters::agent_terminal::backend::term::cell::Flags;
use tethys_adapters::agent_terminal::backend::vte::ansi::{CursorShape, Rgb};
use tethys_adapters::agent_terminal::{TerminalHandle, ViewEvent, palette};

use crate::keys;
use crate::theme::TerminalSkin;

const LINE_HEIGHT: f32 = 1.3;
const PADDING: f32 = 6.;
const SELECTION: Rgb = Rgb {
    r: 0x55,
    g: 0x1b,
    b: 0x2d,
};

pub struct TerminalView {
    handle: TerminalHandle,
    focus: FocusHandle,
    /// Cell geometry from the last layout, for mapping mouse positions.
    metrics: Rc<Cell<Metrics>>,
    scroll_lines: f32,
    selecting: bool,
    _events: Task<()>,
}

#[derive(Clone, Copy)]
struct Metrics {
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

impl TerminalView {
    pub fn new(handle: TerminalHandle, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        handle.on_view_event(move |event| {
            let _ = tx.unbounded_send(event);
        });
        // Coalesce bursts of PTY output into one redraw.
        let events = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut clipboard = None;
                let mut next = Some(first);
                while let Some(event) = next {
                    if let ViewEvent::Clipboard(text) = event {
                        clipboard = Some(text);
                    }
                    next = rx.try_recv().ok();
                }
                let updated = this.update(cx, |_, cx| {
                    if let Some(text) = clipboard {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        Self {
            handle,
            focus: cx.focus_handle(),
            metrics: Rc::new(Cell::new(Metrics {
                origin: Point::default(),
                cell_width: px(8.),
                line_height: px(cx.global::<TerminalSkin>().font_size * LINE_HEIGHT),
            })),
            scroll_lines: 0.,
            selecting: false,
            _events: events,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        let key = keystroke.key.as_str();

        // The clipboard is only read for a paste key.
        let clipboard = (m.control && !m.alt && key == "v")
            .then(|| cx.read_from_clipboard().and_then(|c| c.text()))
            .flatten();
        match clipboard_action(m, key, self.has_selection(), clipboard.is_some()) {
            KeyAction::Copy => {
                self.copy(cx);
                cx.stop_propagation();
                return;
            }
            KeyAction::Paste => {
                self.paste(&clipboard.expect("a paste has text"));
                cx.stop_propagation();
                return;
            }
            KeyAction::Swallow => return,
            KeyAction::PassThrough => {}
        }

        let app_cursor = self
            .handle
            .term()
            .lock()
            .mode()
            .contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::to_bytes(keystroke, app_cursor) {
            self.handle.term().lock().selection = None;
            self.handle.write(bytes);
            cx.stop_propagation();
        }
    }

    fn has_selection(&self) -> bool {
        self.handle.term().lock().selection.is_some()
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let mut term = self.handle.term().lock();
        if let Some(text) = term.selection_to_string().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        term.selection = None;
        cx.notify();
    }

    fn paste(&mut self, text: &str) {
        let bracketed = self
            .handle
            .term()
            .lock()
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        self.handle.write(keys::paste_bytes(text, bracketed));
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let term = self.handle.term().lock();
        grid_point_at(
            position,
            self.metrics.get(),
            term.columns(),
            term.screen_lines(),
            term.grid().display_offset(),
        )
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (point, side) = self.grid_point(event.position);
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.handle.term().lock().selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(event.position);
        if let Some(selection) = self.handle.term().lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    /// Right-click pastes, like Windows Terminal.
    fn right_click(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.paste(&text);
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.metrics.get().line_height;
        let delta = event.delta.pixel_delta(line_height).y / line_height;
        let Some(lines) = take_lines(&mut self.scroll_lines, delta) else {
            return;
        };
        let mode = *self.handle.term().lock().mode();
        let (point, _) = self.grid_point(event.position);
        match scroll_output(mode, lines, point) {
            ScrollOut::Write(bytes) => self.handle.write(bytes),
            ScrollOut::History(lines) => self.handle.scroll(lines),
        }
        cx.notify();
    }
}

/// What a key does before it reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    /// Copy the selection.
    Copy,
    /// Paste the clipboard's text.
    Paste,
    /// Do nothing, and don't send the key either.
    Swallow,
    /// Send the key to the terminal.
    PassThrough,
}

/// Ctrl+C copies when there's a selection (Ctrl+Shift+C always tries).
/// Ctrl+V pastes text; without text on the clipboard it falls through as
/// ^V, which Claude Code uses to paste images, unless Shift is held.
fn clipboard_action(
    m: Modifiers,
    key: &str,
    has_selection: bool,
    clipboard_text: bool,
) -> KeyAction {
    if !m.control || m.alt {
        return KeyAction::PassThrough;
    }
    match key {
        "c" if m.shift || has_selection => KeyAction::Copy,
        "v" if clipboard_text => KeyAction::Paste,
        "v" if m.shift => KeyAction::Swallow,
        _ => KeyAction::PassThrough,
    }
}

/// The cell under a window position, clamped to the grid, and which half of
/// it. `display_offset` is how far the view is scrolled back into history.
fn grid_point_at(
    position: Point<Pixels>,
    m: Metrics,
    cols: usize,
    screen_lines: usize,
    display_offset: usize,
) -> (GridPoint, Side) {
    let x = ((position.x - m.origin.x) / m.cell_width).max(0.);
    let y = ((position.y - m.origin.y) / m.line_height).max(0.);
    let col = (x as usize).min(cols.saturating_sub(1));
    let row = (y as usize).min(screen_lines.saturating_sub(1));
    let line = Line(row as i32 - display_offset as i32);
    let side = if x.fract() < 0.5 {
        Side::Left
    } else {
        Side::Right
    };
    (GridPoint::new(line, Column(col)), side)
}

/// Adds a wheel movement in lines to `acc` and takes the whole lines out of
/// it, positive for up. `None` while less than a line has built up.
fn take_lines(acc: &mut f32, delta: f32) -> Option<i32> {
    *acc += delta;
    let lines = acc.trunc();
    if lines == 0. {
        return None;
    }
    *acc -= lines;
    Some(lines as i32)
}

/// What a wheel movement of `lines` becomes.
#[derive(Debug, PartialEq, Eq)]
enum ScrollOut {
    /// Bytes for the program: wheel reports or arrow keys.
    Write(Vec<u8>),
    /// Scroll the view through history by this many lines.
    History(i32),
}

/// Full-screen TUIs (opencode) ask for mouse reports, so get wheel events at
/// `point`; the alternate screen has no history, so programs there expect
/// arrow keys; otherwise the view scrolls through history.
fn scroll_output(mode: TermMode, lines: i32, point: GridPoint) -> ScrollOut {
    let up = lines > 0;
    let count = lines.unsigned_abs() as usize;
    if mode.intersects(TermMode::MOUSE_MODE) {
        let report = wheel_report(
            up,
            point.column.0 + 1,
            point.line.0.max(0) as usize + 1,
            mode.contains(TermMode::SGR_MOUSE),
        );
        ScrollOut::Write(report.repeat(count))
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        let arrow: &[u8] = match (up, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        ScrollOut::Write(arrow.repeat(count))
    } else {
        ScrollOut::History(lines)
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let handle = self.handle.clone();
        let metrics = self.metrics.clone();
        let skin = cx.global::<TerminalSkin>().clone();
        let background = hsla_from(skin.colors.background);
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .p(px(PADDING))
            .bg(background)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_click))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(
                canvas(
                    move |bounds, window, _| {
                        layout(&handle, bounds, focused, &metrics, &skin, window)
                    },
                    move |_, frame, window, cx| frame.paint(window, cx),
                )
                .size_full(),
            )
    }
}

pub fn hsla_from(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// Everything needed to paint one frame, computed in prepaint.
struct Frame {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    /// Block element characters, drawn as rectangles so they tile seamlessly.
    blocks: Vec<(Bounds<Pixels>, Hsla)>,
    text: Vec<(Point<Pixels>, ShapedLine)>,
    cursor_outline: Option<Bounds<Pixels>>,
    cursor_color: Hsla,
    line_height: Pixels,
}

impl Frame {
    fn paint(self, window: &mut Window, cx: &mut App) {
        for (bounds, color) in self.backgrounds {
            window.paint_quad(fill(bounds, color));
        }
        for (bounds, color) in self.blocks {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in self.text {
            let _ = line.paint(origin, self.line_height, TextAlign::Left, None, window, cx);
        }
        if let Some(bounds) = self.cursor_outline {
            window.paint_quad(outline(bounds, self.cursor_color, BorderStyle::Solid));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strikeout: bool,
}

/// A horizontal run of same-styled cells.
struct Run {
    row: usize,
    col: usize,
    cells: usize,
    text: String,
    style: Style,
    /// A wide character gets its own run so the next cell stays aligned.
    wide: bool,
}

fn layout(
    handle: &TerminalHandle,
    bounds: Bounds<Pixels>,
    focused: bool,
    metrics: &Rc<Cell<Metrics>>,
    skin: &TerminalSkin,
    window: &mut Window,
) -> Frame {
    let font_size = px(skin.font_size);
    let base_font = font(skin.font_family.clone());
    let text_system = window.text_system().clone();
    let font_id = text_system.resolve_font(&base_font);
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(px(8.));
    let line_height = px((skin.font_size * LINE_HEIGHT).round());
    metrics.set(Metrics {
        origin: bounds.origin,
        cell_width,
        line_height,
    });

    let (cols, lines) = grid_size(bounds.size, cell_width, line_height);
    handle.resize(
        cols,
        lines,
        f32::from(cell_width) as u16,
        f32::from(line_height) as u16,
    );

    let term = handle.term().lock();
    let content = term.renderable_content();
    let display_offset = content.display_offset as i32;
    let colors = content.colors;
    let cursor = content.cursor;
    let show_cursor = cursor.shape != CursorShape::Hidden;

    let mut runs: Vec<Run> = Vec::new();
    let mut blocks = Vec::new();
    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let row = (indexed.point.line.0 + display_offset) as usize;
        let col = indexed.point.column.0;

        let fg = palette::resolve(cell.fg, colors, &skin.colors);
        let bg = palette::resolve(cell.bg, colors, &skin.colors);
        let selected = content.selection.is_some_and(|s| s.contains(indexed.point));
        let at_cursor = focused && show_cursor && indexed.point == cursor.point;
        let style = cell_style(fg, bg, cell.flags, selected, at_cursor, skin.colors.cursor);
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let run = push_cell(&mut runs, row, col, style, wide);
        let fg = style.fg;
        if let Some((rects, alpha)) = block_rects(cell.c) {
            let cell_origin = point(
                bounds.origin.x + cell_width * col as f32,
                bounds.origin.y + line_height * row as f32,
            );
            let color = hsla_from(fg).opacity(alpha);
            for &(x, y, w, h) in rects {
                blocks.push((
                    Bounds::new(
                        point(
                            cell_origin.x + cell_width * x,
                            cell_origin.y + line_height * y,
                        ),
                        size(cell_width * w, line_height * h),
                    ),
                    color,
                ));
            }
            run.text.push(' ');
            continue;
        }
        run.text.push(cell.c);
        if let Some(extra) = cell.zerowidth() {
            run.text.extend(extra);
        }
    }

    let cursor_outline = (!focused && show_cursor).then(|| {
        let row = (cursor.point.line.0 + display_offset) as f32;
        Bounds::new(
            point(
                bounds.origin.x + cell_width * cursor.point.column.0 as f32,
                bounds.origin.y + line_height * row,
            ),
            size(cell_width, line_height),
        )
    });
    drop(term);

    let mut frame = Frame {
        backgrounds: Vec::new(),
        blocks,
        text: Vec::new(),
        cursor_outline,
        cursor_color: hsla_from(skin.colors.cursor),
        line_height,
    };
    for run in runs {
        let origin = point(
            bounds.origin.x + cell_width * run.col as f32,
            bounds.origin.y + line_height * run.row as f32,
        );
        if run.style.bg != skin.colors.background {
            frame.backgrounds.push((
                Bounds::new(origin, size(cell_width * run.cells as f32, line_height)),
                hsla_from(run.style.bg),
            ));
        }
        let decorated = run.style.underline || run.style.undercurl || run.style.strikeout;
        if !decorated && run.text.trim().is_empty() {
            continue;
        }
        let fg = hsla_from(run.style.fg);
        let mut run_font = base_font.clone();
        if run.style.bold {
            run_font.weight = FontWeight::BOLD;
        }
        if run.style.italic {
            run_font.style = FontStyle::Italic;
        }
        let text_run = TextRun {
            len: run.text.len(),
            font: run_font,
            color: fg,
            background_color: None,
            underline: (run.style.underline || run.style.undercurl).then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(fg),
                wavy: run.style.undercurl,
            }),
            strikethrough: run.style.strikeout.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(fg),
            }),
        };
        let shaped =
            text_system.shape_line(run.text.into(), font_size, &[text_run], Some(cell_width));
        frame.text.push((origin, shaped));
    }
    frame
}

/// The grid that fits `size`: at least 2 columns and 1 line.
fn grid_size(size: Size<Pixels>, cell_width: Pixels, line_height: Pixels) -> (u16, u16) {
    let cols = (size.width / cell_width).floor().max(2.) as u16;
    let lines = (size.height / line_height).floor().max(1.) as u16;
    (cols, lines)
}

/// A cell's colours and decorations: dim scales the text to two thirds,
/// inverse swaps the colours, a selection takes the selection background, the
/// focused cursor inverts onto the cursor colour, and hidden text matches its
/// background. Undercurl is drawn as its own underline kind.
fn cell_style(
    fg: Rgb,
    bg: Rgb,
    flags: Flags,
    selected: bool,
    at_cursor: bool,
    cursor: Rgb,
) -> Style {
    let (mut fg, mut bg) = (fg, bg);
    if flags.contains(Flags::DIM) {
        fg = Rgb {
            r: (fg.r as u16 * 2 / 3) as u8,
            g: (fg.g as u16 * 2 / 3) as u8,
            b: (fg.b as u16 * 2 / 3) as u8,
        };
    }
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if selected {
        bg = SELECTION;
    }
    if at_cursor {
        fg = bg;
        bg = cursor;
    }
    if flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    Style {
        fg,
        bg,
        bold: flags.contains(Flags::BOLD),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::ALL_UNDERLINES - Flags::UNDERCURL),
        undercurl: flags.contains(Flags::UNDERCURL),
        strikeout: flags.contains(Flags::STRIKEOUT),
    }
}

/// Adds a cell to the last run when it continues it (same row, next column,
/// same style, and neither is wide), otherwise starts a run. A wide cell
/// counts for two columns. Returns the run the cell went into.
fn push_cell(runs: &mut Vec<Run>, row: usize, col: usize, style: Style, wide: bool) -> &mut Run {
    let extend = runs.last().is_some_and(|r| {
        r.row == row && r.col + r.cells == col && r.style == style && !r.wide && !wide
    });
    if !extend {
        runs.push(Run {
            row,
            col,
            cells: 0,
            text: String::new(),
            style,
            wide,
        });
    }
    let run = runs.last_mut().expect("just pushed");
    run.cells += if wide { 2 } else { 1 };
    run
}

type Rect = (f32, f32, f32, f32);

const UL: Rect = (0., 0., 0.5, 0.5);
const UR: Rect = (0.5, 0., 0.5, 0.5);
const LL: Rect = (0., 0.5, 0.5, 0.5);
const LR: Rect = (0.5, 0.5, 0.5, 0.5);
const FULL: Rect = (0., 0., 1., 1.);

/// Unicode block elements (U+2580..U+259F) as fractions of a cell, plus alpha.
fn block_rects(c: char) -> Option<(&'static [Rect], f32)> {
    const LOWER: [[Rect; 1]; 7] = [
        [(0., 7. / 8., 1., 1. / 8.)],
        [(0., 6. / 8., 1., 2. / 8.)],
        [(0., 5. / 8., 1., 3. / 8.)],
        [(0., 4. / 8., 1., 4. / 8.)],
        [(0., 3. / 8., 1., 5. / 8.)],
        [(0., 2. / 8., 1., 6. / 8.)],
        [(0., 1. / 8., 1., 7. / 8.)],
    ];
    const LEFT: [[Rect; 1]; 7] = [
        [(0., 0., 7. / 8., 1.)],
        [(0., 0., 6. / 8., 1.)],
        [(0., 0., 5. / 8., 1.)],
        [(0., 0., 4. / 8., 1.)],
        [(0., 0., 3. / 8., 1.)],
        [(0., 0., 2. / 8., 1.)],
        [(0., 0., 1. / 8., 1.)],
    ];
    let solid = |rects: &'static [Rect]| Some((rects, 1.));
    match c {
        '\u{2580}' => solid(&[(0., 0., 1., 0.5)]),
        '\u{2581}'..='\u{2587}' => solid(&LOWER[c as usize - 0x2581]),
        '\u{2588}' => solid(&[FULL]),
        '\u{2589}'..='\u{258F}' => solid(&LEFT[c as usize - 0x2589]),
        '\u{2590}' => solid(&[(0.5, 0., 0.5, 1.)]),
        '\u{2591}' => Some((&[FULL], 0.25)),
        '\u{2592}' => Some((&[FULL], 0.5)),
        '\u{2593}' => Some((&[FULL], 0.75)),
        '\u{2594}' => solid(&[(0., 0., 1., 1. / 8.)]),
        '\u{2595}' => solid(&[(7. / 8., 0., 1. / 8., 1.)]),
        '\u{2596}' => solid(&[LL]),
        '\u{2597}' => solid(&[LR]),
        '\u{2598}' => solid(&[UL]),
        '\u{2599}' => solid(&[UL, LL, LR]),
        '\u{259A}' => solid(&[UL, LR]),
        '\u{259B}' => solid(&[UL, UR, LL]),
        '\u{259C}' => solid(&[UL, UR, LR]),
        '\u{259D}' => solid(&[UR]),
        '\u{259E}' => solid(&[UR, LL]),
        '\u{259F}' => solid(&[UR, LL, LR]),
        _ => None,
    }
}

/// An xterm mouse wheel report at 1-based `col`/`row`: SGR (`ESC [ < 64 ; c ; r M`)
/// when the program enabled it, otherwise the legacy `ESC [ M` byte encoding.
fn wheel_report(up: bool, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    if sgr {
        return format!("\x1b[<{button};{col};{row}M").into_bytes();
    }
    // Legacy encoding offsets by 32 and can't go past 223.
    let enc = |v: usize| (32 + v.min(223)) as u8;
    vec![0x1b, b'[', b'M', enc(button), enc(col), enc(row)]
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Modifiers, Pixels, Point, Size, point, px, size};
    use tethys_adapters::agent_terminal::backend::index::{Column, Line, Point as GridPoint, Side};
    use tethys_adapters::agent_terminal::backend::term::TermMode;
    use tethys_adapters::agent_terminal::backend::term::cell::Flags;
    use tethys_adapters::agent_terminal::backend::vte::ansi::Rgb;

    use super::{
        FULL, KeyAction, Metrics, Run, SELECTION, ScrollOut, Style, block_rects, cell_style,
        clipboard_action, grid_point_at, grid_size, push_cell, scroll_output, take_lines,
        wheel_report,
    };

    #[test]
    fn wheel_reports() {
        assert_eq!(wheel_report(true, 3, 7, true), b"\x1b[<64;3;7M");
        assert_eq!(wheel_report(false, 1, 1, true), b"\x1b[<65;1;1M");
        assert_eq!(
            wheel_report(true, 1, 2, false),
            [0x1b, b'[', b'M', 96, 33, 34]
        );
        // Legacy: down is button 65, and coordinates stop at 223.
        assert_eq!(
            wheel_report(false, 300, 223, false),
            [0x1b, b'[', b'M', 97, 255, 255]
        );
    }

    #[test]
    fn block_rects_cover_the_right_part_of_the_cell() {
        let rect = |c: char| block_rects(c).unwrap().0;
        // Lower blocks grow upwards from one eighth to seven eighths.
        assert_eq!(rect('\u{2581}'), &[(0., 7. / 8., 1., 1. / 8.)]);
        assert_eq!(rect('\u{2584}'), &[(0., 0.5, 1., 0.5)]);
        assert_eq!(rect('\u{2587}'), &[(0., 1. / 8., 1., 7. / 8.)]);
        // Left blocks shrink from seven eighths to one eighth.
        assert_eq!(rect('\u{2589}'), &[(0., 0., 7. / 8., 1.)]);
        assert_eq!(rect('\u{258C}'), &[(0., 0., 0.5, 1.)]);
        assert_eq!(rect('\u{258F}'), &[(0., 0., 1. / 8., 1.)]);
        assert_eq!(block_rects('\u{2588}'), Some((&[FULL][..], 1.)));
        // Shades are the full cell at rising alpha.
        assert_eq!(block_rects('\u{2591}').unwrap().1, 0.25);
        assert_eq!(block_rects('\u{2592}').unwrap().1, 0.5);
        assert_eq!(block_rects('\u{2593}').unwrap().1, 0.75);
        for c in '\u{2580}'..='\u{259F}' {
            let (rects, alpha) = block_rects(c).unwrap_or_else(|| panic!("{c:?}"));
            assert!(!rects.is_empty());
            assert!((0. ..=1.).contains(&alpha));
            for &(x, y, w, h) in rects {
                assert!(x >= 0. && y >= 0. && x + w <= 1. && y + h <= 1., "{c:?}");
            }
        }
        assert_eq!(block_rects('\u{257F}'), None);
        assert_eq!(block_rects('\u{25A0}'), None);
        assert_eq!(block_rects('a'), None);
    }

    fn mods(control: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            control,
            shift,
            alt,
            ..Modifiers::default()
        }
    }

    #[test]
    fn clipboard_keys() {
        let ctrl = mods(true, false, false);
        let ctrl_shift = mods(true, true, false);
        // Ctrl+C copies only with a selection; Ctrl+Shift+C always.
        assert_eq!(
            clipboard_action(ctrl, "c", false, false),
            KeyAction::PassThrough
        );
        assert_eq!(clipboard_action(ctrl, "c", true, false), KeyAction::Copy);
        assert_eq!(
            clipboard_action(ctrl_shift, "c", false, false),
            KeyAction::Copy
        );
        // Ctrl+V pastes text; without text it's ^V, unless Shift is held.
        assert_eq!(clipboard_action(ctrl, "v", false, true), KeyAction::Paste);
        assert_eq!(
            clipboard_action(ctrl, "v", false, false),
            KeyAction::PassThrough
        );
        assert_eq!(
            clipboard_action(ctrl_shift, "v", false, true),
            KeyAction::Paste
        );
        assert_eq!(
            clipboard_action(ctrl_shift, "v", false, false),
            KeyAction::Swallow
        );
        // Alt or no Ctrl: never ours.
        assert_eq!(
            clipboard_action(mods(true, true, true), "c", true, true),
            KeyAction::PassThrough
        );
        assert_eq!(
            clipboard_action(mods(false, true, false), "v", true, true),
            KeyAction::PassThrough
        );
        assert_eq!(
            clipboard_action(ctrl, "x", true, true),
            KeyAction::PassThrough
        );
    }

    fn metrics() -> Metrics {
        Metrics {
            origin: point(px(10.), px(20.)),
            cell_width: px(8.),
            line_height: px(16.),
        }
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    #[test]
    fn grid_points_map_pixels_to_cells_and_halves() {
        let m = metrics();
        // The origin is the top-left of cell (0, 0), on its left half.
        assert_eq!(
            grid_point_at(at(10., 20.), m, 80, 24, 0),
            (GridPoint::new(Line(0), Column(0)), Side::Left)
        );
        // 2.75 cells across, 1 line down: column 2, right half.
        assert_eq!(
            grid_point_at(at(32., 36.), m, 80, 24, 0),
            (GridPoint::new(Line(1), Column(2)), Side::Right)
        );
        // Left and above the origin clamp to the first cell.
        assert_eq!(
            grid_point_at(at(0., 0.), m, 80, 24, 0).0,
            GridPoint::new(Line(0), Column(0))
        );
        // Past the grid clamps to the last cell.
        assert_eq!(
            grid_point_at(at(10_000., 10_000.), m, 80, 24, 0).0,
            GridPoint::new(Line(23), Column(79))
        );
        // Scrolled back three lines, the top row is history line -3.
        assert_eq!(
            grid_point_at(at(10., 20.), m, 80, 24, 3).0,
            GridPoint::new(Line(-3), Column(0))
        );
        // An empty grid doesn't underflow.
        assert_eq!(
            grid_point_at(at(10., 20.), m, 0, 0, 0).0,
            GridPoint::new(Line(0), Column(0))
        );
    }

    #[test]
    fn wheel_lines_accumulate_fractions() {
        let mut acc = 0.;
        assert_eq!(take_lines(&mut acc, 0.4), None);
        assert_eq!(take_lines(&mut acc, 0.4), None);
        assert_eq!(take_lines(&mut acc, 0.4), Some(1));
        assert!((acc - 0.2).abs() < 1e-5);
        assert_eq!(take_lines(&mut acc, -2.7), Some(-2));
        assert!((acc + 0.5).abs() < 1e-5);
        assert_eq!(take_lines(&mut acc, 3.5), Some(3));
    }

    #[test]
    fn wheel_output_follows_the_terminal_mode() {
        let point = GridPoint::new(Line(4), Column(6));
        // Plain mode scrolls history.
        assert_eq!(
            scroll_output(TermMode::empty(), 2, point),
            ScrollOut::History(2)
        );
        assert_eq!(
            scroll_output(TermMode::ALT_SCREEN, -1, point),
            ScrollOut::History(-1)
        );
        // Mouse reporting: one report per line, 1-based coordinates.
        let mouse = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(
            scroll_output(mouse, 2, point),
            ScrollOut::Write(b"\x1b[<64;7;5M\x1b[<64;7;5M".to_vec())
        );
        // A history line reports as row 1.
        let above = GridPoint::new(Line(-2), Column(0));
        assert_eq!(
            scroll_output(mouse, -1, above),
            ScrollOut::Write(b"\x1b[<65;1;1M".to_vec())
        );
        // Alternate screen with alternate scroll: arrow keys, SS3 in app-cursor mode.
        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(
            scroll_output(alt, 1, point),
            ScrollOut::Write(b"\x1b[A".to_vec())
        );
        assert_eq!(
            scroll_output(alt, -2, point),
            ScrollOut::Write(b"\x1b[B\x1b[B".to_vec())
        );
        assert_eq!(
            scroll_output(alt | TermMode::APP_CURSOR, 1, point),
            ScrollOut::Write(b"\x1bOA".to_vec())
        );
        assert_eq!(
            scroll_output(alt | TermMode::APP_CURSOR, -1, point),
            ScrollOut::Write(b"\x1bOB".to_vec())
        );
        // Mouse mode wins over the alternate screen.
        assert!(matches!(
            scroll_output(alt | mouse, 1, point),
            ScrollOut::Write(bytes) if bytes.starts_with(b"\x1b[<")
        ));
    }

    const FG: Rgb = Rgb {
        r: 240,
        g: 120,
        b: 60,
    };
    const BG: Rgb = Rgb {
        r: 10,
        g: 20,
        b: 30,
    };
    const CURSOR: Rgb = Rgb { r: 1, g: 2, b: 3 };

    #[test]
    fn cell_styles() {
        let plain = cell_style(FG, BG, Flags::empty(), false, false, CURSOR);
        assert_eq!(
            plain,
            Style {
                fg: FG,
                bg: BG,
                bold: false,
                italic: false,
                underline: false,
                undercurl: false,
                strikeout: false,
            }
        );
        let dim = cell_style(FG, BG, Flags::DIM, false, false, CURSOR);
        assert_eq!(
            dim.fg,
            Rgb {
                r: 160,
                g: 80,
                b: 40
            }
        );
        let inverse = cell_style(FG, BG, Flags::INVERSE, false, false, CURSOR);
        assert_eq!((inverse.fg, inverse.bg), (BG, FG));
        let selected = cell_style(FG, BG, Flags::empty(), true, false, CURSOR);
        assert_eq!((selected.fg, selected.bg), (FG, SELECTION));
        // The cursor inverts whatever is under it onto the cursor colour.
        let cursor = cell_style(FG, BG, Flags::empty(), true, true, CURSOR);
        assert_eq!((cursor.fg, cursor.bg), (SELECTION, CURSOR));
        let hidden = cell_style(FG, BG, Flags::HIDDEN, false, false, CURSOR);
        assert_eq!(hidden.fg, BG);
        let decorated = cell_style(
            FG,
            BG,
            Flags::BOLD | Flags::ITALIC | Flags::UNDERLINE | Flags::STRIKEOUT,
            false,
            false,
            CURSOR,
        );
        assert!(decorated.bold && decorated.italic && decorated.underline && decorated.strikeout);
        assert!(!decorated.undercurl);
        let curly = cell_style(FG, BG, Flags::UNDERCURL, false, false, CURSOR);
        assert!(curly.undercurl && !curly.underline);
        let double = cell_style(FG, BG, Flags::DOUBLE_UNDERLINE, false, false, CURSOR);
        assert!(double.underline && !double.undercurl);
    }

    #[test]
    fn runs_merge_contiguous_same_styled_cells() {
        let style = cell_style(FG, BG, Flags::empty(), false, false, CURSOR);
        let bold = cell_style(FG, BG, Flags::BOLD, false, false, CURSOR);
        let mut runs: Vec<Run> = Vec::new();
        push_cell(&mut runs, 0, 0, style, false).text.push('a');
        push_cell(&mut runs, 0, 1, style, false).text.push('b');
        assert_eq!(runs.len(), 1);
        assert_eq!(
            (runs[0].col, runs[0].cells, runs[0].text.as_str()),
            (0, 2, "ab")
        );
        // A different style starts a run.
        push_cell(&mut runs, 0, 2, bold, false);
        assert_eq!(runs.len(), 2);
        // A gap starts a run.
        push_cell(&mut runs, 0, 5, bold, false);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[2].col, 5);
        // A new row starts a run.
        push_cell(&mut runs, 1, 6, bold, false);
        assert_eq!(runs.len(), 4);
        // A wide cell takes two columns and never merges on either side.
        push_cell(&mut runs, 1, 7, bold, true);
        assert_eq!(runs.len(), 5);
        assert_eq!(runs[4].cells, 2);
        push_cell(&mut runs, 1, 9, bold, false);
        assert_eq!(runs.len(), 6);
    }

    #[test]
    fn grid_sizes_fit_and_never_vanish() {
        let cell = px(8.);
        let line = px(16.);
        let area = |w: f32, h: f32| -> Size<Pixels> { size(px(w), px(h)) };
        assert_eq!(grid_size(area(800., 480.), cell, line), (100, 30));
        assert_eq!(grid_size(area(807., 479.), cell, line), (100, 29));
        assert_eq!(grid_size(area(0., 0.), cell, line), (2, 1));
    }
}
