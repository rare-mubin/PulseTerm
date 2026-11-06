use alacritty_terminal::index::Point as AlacPoint;
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    fill, point, px, size, App, Bounds, Element, ElementId, Entity, FontId, GlobalElementId,
    GlyphId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, ShapedLine,
    SharedString, StrikethroughStyle, Style, TextRun, UnderlineStyle, Window,
};

use super::emulator::{Emulator, TermSize};
use super::palette;
use super::view::TerminalView;
use crate::store::Settings;
use crate::theme::Theme;

/// Paints the terminal grid.
///
/// A hand-written `Element` rather than a tree of divs for the obvious reason:
/// an 80x24 grid is ~2000 cells, and a div per cell would rebuild that many
/// taffy nodes every frame. Here each frame walks the grid once and emits one
/// quad per run of same-coloured background and, for the text, either one
/// glyph per cell from `GLYPHS` (the normal path) or one shaped line per row
/// (ligatures on, which need a row's characters shaped together).
pub struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

pub struct LayoutState {
    cell: CellMetrics,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    rows: Vec<PositionedRow>,
    /// The glyph path's output: every visible glyph, placed.
    glyphs: Vec<GlyphPaint>,
    decorations: Vec<Decoration>,
    font_size: Pixels,
    cursor: Option<CursorLayout>,
}

/// One glyph to paint, already at its pixel position (baseline included).
struct GlyphPaint {
    origin: gpui::Point<Pixels>,
    font_id: FontId,
    glyph: GlyphId,
    color: Hsla,
    emoji: bool,
}

/// An underline or strikethrough across one cell.
struct Decoration {
    origin: gpui::Point<Pixels>,
    width: Pixels,
    color: Hsla,
    strike: bool,
}

// Each distinct character (with any combining marks, in bold or italic) shaped
// once and remembered: a terminal is a grid of a monospace font, so a glyph's
// shape never depends on its neighbours unless ligatures are on.
//
// Shaping whole rows every frame was the cost. A TUI that redraws braille or
// colour across the screen (measured with `lite-eq`: 56 rows, 1,300 colour
// runs, 6,400 characters a frame) spent ~15 ms a frame in the shaper -- about
// 2.4 µs a character, font fallback included -- and drew at 30 fps where
// Alacritty, which caches glyphs, is smooth. Shaping goes through gpui's own
// path, so fallback fonts for braille, emoji and CJK resolve as before.
//
// Shared by every terminal (one font for all of them); it empties itself when
// the font, size or weight changes, and if it ever grows past `LIMIT`.
thread_local! {
    static GLYPHS: RefCell<GlyphCache> = RefCell::new(GlyphCache::default());
}

#[derive(Default)]
struct GlyphCache {
    font: Option<(SharedString, u32, u32)>,
    ascent: Pixels,
    descent: Pixels,
    map: HashMap<GlyphKey, Rc<[CachedGlyph]>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GlyphKey {
    c: char,
    marks: Option<Box<[char]>>,
    bold: bool,
    italic: bool,
}

#[derive(Clone, Copy)]
struct CachedGlyph {
    font_id: FontId,
    glyph: GlyphId,
    x: Pixels,
    emoji: bool,
}

impl GlyphCache {
    const LIMIT: usize = 10_000;

    /// Empties the cache when the font changed, and keeps the base font's
    /// ascent and descent for placing every glyph on one baseline.
    fn sync(&mut self, theme: &Theme, window: &mut Window) {
        let sig = (theme.font.clone(), f32::from(theme.font_size).to_bits(), theme.font_weight.0.to_bits());
        if self.font.as_ref() == Some(&sig) && self.map.len() < Self::LIMIT {
            return;
        }
        self.map.clear();
        let run = TextRun {
            len: 1,
            font: theme.text_font(),
            color: theme.fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = window.text_system().layout_line("M", theme.font_size, &[run], None);
        self.ascent = layout.ascent;
        self.descent = layout.descent;
        self.font = Some(sig);
    }

    fn glyphs(&mut self, key: GlyphKey, font: gpui::Font, theme: &Theme, window: &mut Window) -> Rc<[CachedGlyph]> {
        if let Some(found) = self.map.get(&key) {
            return found.clone();
        }
        let mut text = String::new();
        text.push(key.c);
        if let Some(marks) = &key.marks {
            text.extend(marks.iter());
        }
        let run = TextRun {
            len: text.len(),
            font,
            color: theme.fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = window.text_system().layout_line(&text, theme.font_size, &[run], None);
        let shaped: Rc<[CachedGlyph]> = layout
            .runs
            .iter()
            .flat_map(|run| {
                run.glyphs.iter().map(move |g| CachedGlyph {
                    font_id: run.font_id,
                    glyph: g.id,
                    x: g.position.x,
                    emoji: g.is_emoji,
                })
            })
            .collect();
        self.map.insert(key, shaped.clone());
        shaped
    }
}

struct PositionedRow {
    line: ShapedLine,
    origin: gpui::Point<Pixels>,
}

struct CursorLayout {
    bounds: Bounds<Pixels>,
    color: Hsla,
    shape: CursorShape,
    /// Redrawn over a filled block cursor in the background colour, so the
    /// character under it reads as reverse video instead of disappearing.
    glyph: Option<ShapedLine>,
}

#[derive(Clone, Copy)]
pub struct CellMetrics {
    pub width: Pixels,
    pub height: Pixels,
}

impl CellMetrics {
    /// Measures the font once per frame. `shape_line` is backed by gpui's line
    /// layout cache, so repeating this every frame is a hash lookup rather than
    /// a reshape.
    pub fn measure(window: &mut Window, theme: &Theme) -> Self {
        let run = TextRun {
            len: 1,
            font: theme.text_font(),
            color: theme.fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line("M".into(), theme.font_size, &[run], None);
        Self {
            width: shaped.width.max(px(1.)),
            height: theme.line_height.max(px(1.)),
        }
    }

    pub fn grid_size(&self, bounds: Bounds<Pixels>) -> TermSize {
        let cols = (f32::from(bounds.size.width) / f32::from(self.width)).floor() as usize;
        let rows = (f32::from(bounds.size.height) / f32::from(self.height)).floor() as usize;
        TermSize::new(cols, rows)
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = LayoutState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = gpui::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> LayoutState {
        let _span = super::view::trace::Span::new(0);
        let cell = CellMetrics::measure(window, cx.global::<Theme>());
        let grid = cell.grid_size(bounds);
        let (scrollback, cursor) = {
            let settings = cx.global::<Settings>();
            (settings.scrollback, cursor_shape(&settings.cursor_shape))
        };

        // Reconcile the emulator (and the remote pty) with the space we were
        // actually given, before reading any content out of it. Doing this here
        // rather than after layout is what stops a full-screen TUI painting one
        // garbled frame at the old size every time a pane is resized. Settings
        // are reconciled the same way, so a change on the Settings page reaches
        // every open terminal without anything walking the pane tree.
        self.view.update(cx, |view, cx| {
            view.configure(scrollback, cursor);
            view.set_layout(bounds, grid, cell.width, cell.height, cx);
        });

        let theme = cx.global::<Theme>();
        let view = self.view.read(cx);
        let emulator = view.emulator();
        let content = emulator.term().renderable_content();
        let colors = content.colors;

        let display_offset = content.display_offset as i32;
        let selection = content.selection;

        let mut backgrounds = Vec::new();
        let mut rows = Vec::new();
        let mut row = RowBuilder::new(theme, cell, bounds.origin);
        let mut glyphs = Vec::new();
        let mut decorations = Vec::new();
        if row.glyph_mode {
            GLYPHS.with(|cache| cache.borrow_mut().sync(theme, window));
        }
        let mut current_row: Option<i32> = None;

        for indexed in content.display_iter {
            // `display_iter` yields *buffer* lines, which go negative once you
            // scroll back; the viewport row is what we actually paint at. Miss
            // this and scrollback renders above the top of the pane.
            let viewport_row = indexed.point.line.0 + display_offset;
            if current_row != Some(viewport_row) {
                row.flush(window, &mut backgrounds, &mut rows, &mut glyphs, &mut decorations);
                row.start(viewport_row);
                current_row = Some(viewport_row);
            }
            row.push(indexed.point, indexed.cell, colors, selection.as_ref());
        }
        row.flush(window, &mut backgrounds, &mut rows, &mut glyphs, &mut decorations);

        let cursor = layout_cursor(
            &content.cursor,
            emulator,
            view.is_focused_for_cursor(),
            theme,
            colors,
            cell,
            bounds,
            window,
        );

        LayoutState {
            cell,
            backgrounds,
            rows,
            glyphs,
            decorations,
            font_size: theme.font_size,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        layout: &mut LayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let _span = super::view::trace::Span::new(1);
        let line_height = layout.cell.height;

        window.paint_layer(bounds, |window| {
            for (rect, color) in &layout.backgrounds {
                window.paint_quad(fill(*rect, *color));
            }

            // Cursor under the text: a hollow or beam cursor must not cover the
            // glyph, and the filled block paints its own inverted glyph after.
            if let Some(cursor) = &layout.cursor {
                paint_cursor(cursor, line_height, window);
            }

            for row in &layout.rows {
                // A paint failure here means one row's glyphs could not be
                // rasterised; skipping it keeps the rest of the frame intact.
                let _ = row.line.paint(row.origin, line_height, window, cx);
            }

            // The glyph path: each glyph straight from the cache to the GPU,
            // skipped when it lies outside what is visible (a column scrolled
            // off the strip still lays out, but need not paint).
            let visible = window.content_mask().bounds;
            let margin = layout.cell.height * 2.;
            for g in &layout.glyphs {
                if g.origin.y < visible.top() - margin
                    || g.origin.y > visible.bottom() + margin
                    || g.origin.x < visible.left() - margin
                    || g.origin.x > visible.right() + margin
                {
                    continue;
                }
                let _ = if g.emoji {
                    window.paint_emoji(g.origin, g.font_id, g.glyph, layout.font_size)
                } else {
                    window.paint_glyph(g.origin, g.font_id, g.glyph, layout.font_size, g.color)
                };
            }
            for d in &layout.decorations {
                if d.strike {
                    window.paint_strikethrough(
                        d.origin,
                        d.width,
                        &StrikethroughStyle { thickness: px(1.), color: Some(d.color) },
                    );
                } else {
                    window.paint_underline(
                        d.origin,
                        d.width,
                        &UnderlineStyle { thickness: px(1.), color: Some(d.color), wavy: false },
                    );
                }
            }

            if let Some(cursor) = &layout.cursor {
                if let Some(glyph) = &cursor.glyph {
                    let _ = glyph.paint(cursor.bounds.origin, line_height, window, cx);
                }
            }
        });
    }
}

fn paint_cursor(cursor: &CursorLayout, line_height: Pixels, window: &mut Window) {
    let bounds = cursor.bounds;
    let rect = match cursor.shape {
        CursorShape::Block | CursorShape::HollowBlock => bounds,
        CursorShape::Underline => Bounds {
            origin: point(bounds.origin.x, bounds.origin.y + line_height - px(2.)),
            size: size(bounds.size.width, px(2.)),
        },
        CursorShape::Beam => Bounds {
            origin: bounds.origin,
            size: size(px(2.), bounds.size.height),
        },
        CursorShape::Hidden => return,
    };

    if matches!(cursor.shape, CursorShape::HollowBlock) {
        // Four hairlines rather than a fill: an unfocused terminal should show
        // where the cursor is without implying it will receive keystrokes.
        let t = px(1.);
        for edge in [
            Bounds {
                origin: rect.origin,
                size: size(rect.size.width, t),
            },
            Bounds {
                origin: point(rect.origin.x, rect.origin.y + rect.size.height - t),
                size: size(rect.size.width, t),
            },
            Bounds {
                origin: rect.origin,
                size: size(t, rect.size.height),
            },
            Bounds {
                origin: point(rect.origin.x + rect.size.width - t, rect.origin.y),
                size: size(t, rect.size.height),
            },
        ] {
            window.paint_quad(fill(edge, cursor.color));
        }
    } else {
        window.paint_quad(fill(rect, cursor.color));
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_cursor(
    cursor: &alacritty_terminal::term::RenderableCursor,
    emulator: &Emulator,
    focused: bool,
    theme: &Theme,
    colors: &Colors,
    cell: CellMetrics,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> Option<CursorLayout> {
    if matches!(cursor.shape, CursorShape::Hidden) {
        return None;
    }

    // The cursor sits at an absolute grid point; scrolling back through history
    // moves the viewport but not the cursor, so it has to be offset by the
    // display offset and culled once it scrolls out of view.
    let row = cursor.point.line.0 + emulator.display_offset() as i32;
    if row < 0 || row >= emulator.size().rows as i32 {
        return None;
    }

    let origin = point(
        bounds.origin.x + cell.width * cursor.point.column.0 as f32,
        bounds.origin.y + cell.height * row as f32,
    );
    let cursor_bounds = Bounds {
        origin,
        size: size(cell.width, cell.height),
    };

    let color = colors[NamedColor::Cursor as usize]
        .map(|c| gpui::rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into())
        .unwrap_or(theme.cursor);

    let shape = if focused {
        cursor.shape
    } else {
        CursorShape::HollowBlock
    };

    // Under a filled block, redraw the character in the background colour so it
    // stays readable.
    let glyph = if matches!(shape, CursorShape::Block) {
        let c = emulator.term().grid()[cursor.point].c;
        (c != ' ' && c != '\0').then(|| {
            let run = TextRun {
                len: c.len_utf8(),
                font: theme.text_font(),
                // The palette's bg, not the theme's: on a translucent window
                // the latter has alpha, and the glyph would fade into the block.
                color: theme.palette.bg,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(c.to_string().into(), theme.font_size, &[run], None)
        })
    } else {
        None
    };

    Some(CursorLayout {
        bounds: cursor_bounds,
        color,
        shape,
        glyph,
    })
}

/// The `cursor_shape` setting as alacritty's shape.
fn cursor_shape(name: &str) -> CursorShape {
    match name {
        "bar" => CursorShape::Beam,
        "underline" => CursorShape::Underline,
        _ => CursorShape::Block,
    }
}

/// Whether a buffer point falls inside a selection range. Block selections are
/// a rectangle; normal ones run start-to-end in reading order, so only the first
/// and last rows are partial.
fn selected(range: &SelectionRange, point: AlacPoint) -> bool {
    if range.is_block {
        point.line >= range.start.line
            && point.line <= range.end.line
            && point.column >= range.start.column
            && point.column <= range.end.column
    } else {
        (point.line > range.start.line
            || (point.line == range.start.line && point.column >= range.start.column))
            && (point.line < range.end.line
                || (point.line == range.end.line && point.column <= range.end.column))
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

/// Accumulates one grid row into a single shaped line plus a handful of
/// background quads. Neighbouring cells that share a style extend the previous
/// `TextRun` instead of starting a new one, so a typical row of plain output
/// costs one run rather than eighty.
struct RowBuilder<'a> {
    theme: &'a Theme,
    cell: CellMetrics,
    origin: gpui::Point<Pixels>,
    line: i32,

    /// Ligatures off (the default): cells are painted glyph by glyph from the
    /// cache, and `cells` collects them instead of `text`/`runs`.
    glyph_mode: bool,
    cells: Vec<(usize, char, Option<Box<[char]>>, CellStyle)>,

    text: String,
    runs: Vec<TextRun>,
    open_style: Option<CellStyle>,

    open_bg: Option<Hsla>,
    bg_start: usize,
    bg_end: usize,
    backgrounds: Vec<(usize, usize, Hsla)>,
}

impl<'a> RowBuilder<'a> {
    fn new(theme: &'a Theme, cell: CellMetrics, origin: gpui::Point<Pixels>) -> Self {
        Self {
            theme,
            cell,
            origin,
            line: 0,
            glyph_mode: !theme.ligatures,
            cells: Vec::new(),
            text: String::new(),
            runs: Vec::new(),
            open_style: None,
            open_bg: None,
            bg_start: 0,
            bg_end: 0,
            backgrounds: Vec::new(),
        }
    }

    fn start(&mut self, line: i32) {
        self.line = line;
        self.text.clear();
        self.runs.clear();
        self.open_style = None;
        self.open_bg = None;
        self.backgrounds.clear();
    }

    fn push(
        &mut self,
        point: AlacPoint,
        cell: &Cell,
        colors: &Colors,
        selection: Option<&SelectionRange>,
    ) {
        let col = point.column.0;
        let flags = cell.flags;

        // The trailing half of a double-width character carries no glyph of its
        // own. The wide glyph itself advances two cells in any sane monospace
        // font, so emitting the spacer as well would shift the row right.
        if flags.contains(Flags::WIDE_CHAR_SPACER) || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            return;
        }

        let mut fg = palette::resolve(cell.fg, self.theme, colors);
        let mut bg = palette::resolve(cell.bg, self.theme, colors);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        // Selection is compared in buffer coordinates, which is what both the
        // range and `point` are in -- the viewport conversion above is only for
        // deciding where to paint.
        if selection.is_some_and(|range| selected(range, point)) {
            bg = self.theme.selection;
        }

        // Only emit a quad where the cell differs from the window ground. On a
        // mostly-empty screen that skips nearly every cell, keeping the quad
        // count proportional to real content rather than to grid area.
        self.push_background(col, (bg != self.theme.bg).then_some(bg));

        let style = CellStyle {
            fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strikeout: flags.contains(Flags::STRIKEOUT),
        };

        let c = if cell.c == '\0' { ' ' } else { cell.c };
        if self.glyph_mode {
            // A blank draws nothing unless a line runs under it.
            if c != ' ' || style.underline || style.strikeout {
                self.cells.push((col, c, cell.zerowidth().map(Box::from), style));
            }
            return;
        }
        let len = c.len_utf8();
        self.text.push(c);

        match (self.open_style, self.runs.last_mut()) {
            (Some(open), Some(run)) if open == style => run.len += len,
            _ => {
                self.runs.push(self.text_run(style, len));
                self.open_style = Some(style);
            }
        }
    }

    fn text_run(&self, style: CellStyle, len: usize) -> TextRun {
        let mut font = self.theme.text_font();
        if style.bold {
            font = font.bold();
        }
        if style.italic {
            font = font.italic();
        }
        TextRun {
            len,
            font,
            color: style.fg,
            background_color: None,
            underline: style.underline.then(|| UnderlineStyle {
                thickness: px(1.),
                color: Some(style.fg),
                wavy: false,
            }),
            strikethrough: style.strikeout.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(style.fg),
            }),
        }
    }

    fn push_background(&mut self, col: usize, bg: Option<Hsla>) {
        match (self.open_bg, bg) {
            (Some(open), Some(next)) if open == next && self.bg_end + 1 == col => {
                self.bg_end = col;
            }
            (_, Some(next)) => {
                self.commit_background();
                self.open_bg = Some(next);
                self.bg_start = col;
                self.bg_end = col;
            }
            (_, None) => self.commit_background(),
        }
    }

    fn commit_background(&mut self) {
        if let Some(color) = self.open_bg.take() {
            self.backgrounds.push((self.bg_start, self.bg_end, color));
        }
    }

    /// The glyph path for one row: each collected cell's glyphs from the
    /// cache, at its column, on the base font's baseline; underline and
    /// strikethrough where gpui's own line painter puts them.
    fn emit_glyphs(
        &mut self,
        y: Pixels,
        window: &mut Window,
        glyphs: &mut Vec<GlyphPaint>,
        decorations: &mut Vec<Decoration>,
    ) {
        let theme = self.theme;
        GLYPHS.with(|cache| {
            let mut cache = cache.borrow_mut();
            let (ascent, descent) = (cache.ascent, cache.descent);
            let baseline = (self.cell.height - ascent - descent) / 2. + ascent;
            let cells = std::mem::take(&mut self.cells);
            for (col, c, marks, style) in cells {
                let x = self.origin.x + self.cell.width * col as f32;
                if c != ' ' {
                    let font = self.text_run(style, 1).font;
                    let key = GlyphKey { c, marks, bold: style.bold, italic: style.italic };
                    for g in cache.glyphs(key, font, theme, window).iter() {
                        glyphs.push(GlyphPaint {
                            origin: point(x + g.x, y + baseline),
                            font_id: g.font_id,
                            glyph: g.glyph,
                            color: style.fg,
                            emoji: g.emoji,
                        });
                    }
                }
                if style.underline {
                    decorations.push(Decoration {
                        origin: point(x, y + baseline + descent * 0.618),
                        width: self.cell.width,
                        color: style.fg,
                        strike: false,
                    });
                }
                if style.strikeout {
                    decorations.push(Decoration {
                        origin: point(x, y + (ascent * 0.5 + baseline) * 0.5),
                        width: self.cell.width,
                        color: style.fg,
                        strike: true,
                    });
                }
            }
        });
        self.text.clear();
        self.runs.clear();
        self.open_style = None;
    }

    fn trim_trailing_blanks(&mut self) {
        let keep = self.text.trim_end_matches(' ').len();
        if keep == self.text.len() {
            return;
        }
        let mut at = 0;
        for run in &self.runs {
            if at + run.len > keep && (run.underline.is_some() || run.strikethrough.is_some()) {
                return;
            }
            at += run.len;
        }
        self.text.truncate(keep);
        let mut at = 0;
        self.runs.retain_mut(|run| {
            if at >= keep {
                return false;
            }
            run.len = run.len.min(keep - at);
            at += run.len;
            true
        });
    }

    fn flush(
        &mut self,
        window: &mut Window,
        backgrounds: &mut Vec<(Bounds<Pixels>, Hsla)>,
        rows: &mut Vec<PositionedRow>,
        glyphs: &mut Vec<GlyphPaint>,
        decorations: &mut Vec<Decoration>,
    ) {
        if self.text.is_empty()
            && self.cells.is_empty()
            && self.backgrounds.is_empty()
            && self.open_bg.is_none()
        {
            return;
        }
        self.commit_background();

        let y = self.origin.y + self.cell.height * self.line as f32;

        for (start, end, color) in self.backgrounds.drain(..) {
            backgrounds.push((
                Bounds {
                    origin: point(self.origin.x + self.cell.width * start as f32, y),
                    size: size(self.cell.width * (end - start + 1) as f32, self.cell.height),
                },
                color,
            ));
        }

        if self.glyph_mode {
            self.emit_glyphs(y, window, glyphs, decorations);
            return;
        }

        // Trailing blanks are not shaped. A row is built cell by cell across
        // the whole width, so `1234567` arrived as seven characters and ~190
        // spaces, and shaping those was nearly all of a frame: measured at
        // ~30 ms for 50 rows of fresh text during `seq 1 2000000`. Blanks draw
        // nothing (backgrounds are their own quads), so they are cut -- unless
        // an underline or strikethrough runs under them, which does show.
        self.trim_trailing_blanks();

        if !self.text.is_empty() {
            let shaped = window.text_system().shape_line(
                self.text.clone().into(),
                self.theme.font_size,
                &self.runs,
                None,
            );
            rows.push(PositionedRow {
                line: shaped,
                origin: point(self.origin.x, y),
            });
        }

        self.text.clear();
        self.runs.clear();
        self.open_style = None;
    }
}
