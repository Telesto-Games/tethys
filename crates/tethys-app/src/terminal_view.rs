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

const FONT_FAMILY: &str = "Cascadia Mono";
const FONT_SIZE: f32 = 14.;
const LINE_HEIGHT: f32 = 1.3;
const PADDING: f32 = 6.;
const SELECTION: Rgb = Rgb {
    r: 0x3e,
    g: 0x44,
    b: 0x51,
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
                line_height: px(FONT_SIZE * LINE_HEIGHT),
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

        // Ctrl+C copies when there's a selection; Ctrl+Shift+C always tries.
        if m.control && !m.alt && key == "c" && (m.shift || self.has_selection()) {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        // Ctrl+V pastes text. Without text on the clipboard it falls through as
        // ^V, which Claude Code uses to paste images.
        if m.control && !m.alt && key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                self.paste(&text);
                cx.stop_propagation();
                return;
            }
            if m.shift {
                return;
            }
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
        let m = self.metrics.get();
        let x = ((position.x - m.origin.x) / m.cell_width).max(0.);
        let y = ((position.y - m.origin.y) / m.line_height).max(0.);
        let term = self.handle.term().lock();
        let col = (x as usize).min(term.columns().saturating_sub(1));
        let row = (y as usize).min(term.screen_lines().saturating_sub(1));
        let line = Line(row as i32 - term.grid().display_offset() as i32);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        (GridPoint::new(line, Column(col)), side)
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
        self.scroll_lines += event.delta.pixel_delta(line_height).y / line_height;
        let lines = self.scroll_lines.trunc();
        if lines != 0. {
            self.scroll_lines -= lines;
            self.handle.scroll(lines as i32);
            cx.notify();
        }
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
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .p(px(PADDING))
            .bg(hsla_from(palette::BACKGROUND))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_click))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(
                canvas(
                    move |bounds, window, _| layout(&handle, bounds, focused, &metrics, window),
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
            window.paint_quad(outline(
                bounds,
                hsla_from(palette::CURSOR),
                BorderStyle::Solid,
            ));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
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
    window: &mut Window,
) -> Frame {
    let font_size = px(FONT_SIZE);
    let base_font = font(FONT_FAMILY);
    let text_system = window.text_system().clone();
    let font_id = text_system.resolve_font(&base_font);
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(px(8.));
    let line_height = px((FONT_SIZE * LINE_HEIGHT).round());
    metrics.set(Metrics {
        origin: bounds.origin,
        cell_width,
        line_height,
    });

    let cols = (bounds.size.width / cell_width).floor().max(2.) as u16;
    let lines = (bounds.size.height / line_height).floor().max(1.) as u16;
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

        let mut fg = palette::resolve(cell.fg, colors);
        let mut bg = palette::resolve(cell.bg, colors);
        if cell.flags.contains(Flags::DIM) {
            fg = Rgb {
                r: (fg.r as u16 * 2 / 3) as u8,
                g: (fg.g as u16 * 2 / 3) as u8,
                b: (fg.b as u16 * 2 / 3) as u8,
            };
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if content.selection.is_some_and(|s| s.contains(indexed.point)) {
            bg = SELECTION;
        }
        if focused && show_cursor && indexed.point == cursor.point {
            fg = bg;
            bg = palette::CURSOR;
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        let style = Style {
            fg,
            bg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell
                .flags
                .intersects(Flags::ALL_UNDERLINES - Flags::UNDERCURL),
            undercurl: cell.flags.contains(Flags::UNDERCURL),
            strikeout: cell.flags.contains(Flags::STRIKEOUT),
        };
        let wide = cell.flags.contains(Flags::WIDE_CHAR);

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
        line_height,
    };
    for run in runs {
        let origin = point(
            bounds.origin.x + cell_width * run.col as f32,
            bounds.origin.y + line_height * run.row as f32,
        );
        if run.style.bg != palette::BACKGROUND {
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
