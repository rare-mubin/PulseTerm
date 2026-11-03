//! A small immediate-mode UI on SDL3's renderer, in PulseTerm's design
//! language: flat grounds, one-pixel hairlines, no radius, no shadow,
//! monospace text, tracked capital labels, segmented choices.
//!
//! Immediate mode because the window is one screen redrawn on change: each
//! frame calls the widgets in order, and a widget answers "was I clicked" from
//! the frame's pointer state. Layout is in points; drawing multiplies by the
//! window's pixel density, so text and hairlines are crisp on HiDPI screens.

pub mod text;

use sdl3_sys::everything::*;

use crate::theme::{label, Rgb, Theme};
use text::Fonts;

pub struct Ui {
    pub r: *mut SDL_Renderer,
    pub fonts: Fonts,
    pub t: Theme,
    /// Pixels per point.
    pub scale: f32,
    /// Window size in points.
    pub width: f32,
    pub height: f32,
    /// Pointer position in points.
    pub mouse: (f32, f32),
    /// The left button went up this frame (a click lands on release).
    pub click: bool,
    /// Wheel movement this frame, in lines (positive = up).
    pub wheel: f32,
}

impl Ui {
    fn p(&self, v: f32) -> f32 {
        v * self.scale
    }

    pub fn fill(&self, x: f32, y: f32, w: f32, h: f32, c: Rgb, a: u8) {
        unsafe {
            SDL_SetRenderDrawColor(self.r, c.0, c.1, c.2, a);
            let rect = SDL_FRect { x: self.p(x), y: self.p(y), w: self.p(w), h: self.p(h) };
            SDL_RenderFillRect(self.r, &rect);
        }
    }

    /// A one-pixel outline, whatever the density -- the design's hairline.
    pub fn outline(&self, x: f32, y: f32, w: f32, h: f32, c: Rgb) {
        self.hline(x, y, w, c);
        self.hline(x, y + h - 1.0 / self.scale, w, c);
        self.vline(x, y, h, c);
        self.vline(x + w - 1.0 / self.scale, y, h, c);
    }

    pub fn hline(&self, x: f32, y: f32, w: f32, c: Rgb) {
        unsafe {
            SDL_SetRenderDrawColor(self.r, c.0, c.1, c.2, 255);
            let rect = SDL_FRect { x: self.p(x), y: self.p(y).floor(), w: self.p(w), h: 1.0 };
            SDL_RenderFillRect(self.r, &rect);
        }
    }

    pub fn vline(&self, x: f32, y: f32, h: f32, c: Rgb) {
        unsafe {
            SDL_SetRenderDrawColor(self.r, c.0, c.1, c.2, 255);
            let rect = SDL_FRect { x: self.p(x).floor(), y: self.p(y), w: 1.0, h: self.p(h) };
            SDL_RenderFillRect(self.r, &rect);
        }
    }

    /// The height of one line of text at `size`.
    pub fn line(&self, size: f32) -> f32 {
        if (size - self.t.font_size).abs() < 0.01 {
            self.t.line_height
        } else {
            size * 1.4
        }
    }

    /// Draws one line with its box's top at `y`; returns its width in points.
    pub fn text(&mut self, x: f32, y: f32, s: &str, size: f32, bold: bool, c: Rgb) -> f32 {
        self.text_alpha(x, y, s, size, bold, c, 255)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text_alpha(&mut self, x: f32, y: f32, s: &str, size: f32, bold: bool, c: Rgb, a: u8) -> f32 {
        let px = self.p(size);
        let ascent = self.fonts.ascent(px);
        let line = self.p(self.line(size));
        // Centre the glyphs' ascent-to-baseline band in the line box.
        let baseline = self.p(y) + (line - px) / 2.0 + ascent * 0.92;
        let w = unsafe { self.fonts.draw(self.r, s, self.p(x), baseline.round(), px, bold, c, a) };
        w / self.scale
    }

    pub fn measure(&self, s: &str, size: f32, bold: bool) -> f32 {
        self.fonts.measure(s, self.p(size), bold) / self.scale
    }

    /// Columns of `size` text that fit in `w` points (monospace).
    pub fn columns(&self, w: f32, size: f32) -> usize {
        let one = self.measure("M", size, false).max(1.0);
        (w / one).floor() as usize
    }

    /// A tracked capital label; returns its width.
    pub fn micro(&mut self, x: f32, y: f32, s: &str, c: Rgb) -> f32 {
        let size = self.t.micro();
        self.text(x, y, &label(s), size, false, c)
    }

    pub fn micro_width(&self, s: &str) -> f32 {
        self.measure(&label(s), self.t.micro(), false)
    }

    pub fn hover(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        let (mx, my) = self.mouse;
        mx >= x && my >= y && mx < x + w && my < y + h
    }

    pub fn clicked(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        self.click && self.hover(x, y, w, h)
    }

    /// Restricts drawing to a rectangle (points), or lifts the restriction.
    pub fn clip(&self, area: Option<(f32, f32, f32, f32)>) {
        unsafe {
            match area {
                Some((x, y, w, h)) => {
                    let rect = SDL_Rect {
                        x: self.p(x) as i32,
                        y: self.p(y) as i32,
                        w: self.p(w).ceil() as i32,
                        h: self.p(h).ceil() as i32,
                    };
                    SDL_SetRenderClipRect(self.r, &rect);
                }
                None => {
                    SDL_SetRenderClipRect(self.r, std::ptr::null());
                }
            }
        }
    }

    // --- widgets ----------------------------------------------------------

    /// A boxed, tracked button; returns (width, clicked). `strong` is the
    /// accent-filled one the design reserves for the primary action.
    pub fn button(&mut self, x: f32, y: f32, text: &str, strong: bool, enabled: bool) -> (f32, bool) {
        let t = self.t.clone();
        let w = self.micro_width(text) + 28.0;
        let h = t.row();
        let hover = enabled && self.hover(x, y, w, h);
        let (fg, bg, line) = match (strong, enabled) {
            (true, true) => (t.bg, Some(t.accent), t.accent),
            (_, false) => (t.fg_subtle, None, t.border),
            (false, true) => (t.fg_muted, None, if hover { t.accent } else { t.border }),
        };
        if let Some(bg) = bg {
            self.fill(x, y, w, h, bg, 255);
        } else if hover {
            self.fill(x, y, w, h, t.raised, 255);
        }
        self.outline(x, y, w, h, line);
        let ty = y + (h - self.line(t.micro())) / 2.0;
        self.micro(x + 14.0, ty, text, fg);
        (w, enabled && self.clicked(x, y, w, h))
    }

    /// A full-width row button: the action's name on the left, its shortcut
    /// (if any) on the right, inside the same box. Returns clicked.
    pub fn row_button(&mut self, x: f32, y: f32, w: f32, text: &str, hint: &str) -> bool {
        let t = self.t.clone();
        let h = t.row();
        let hover = self.hover(x, y, w, h);
        if hover {
            self.fill(x, y, w, h, t.raised, 255);
        }
        self.outline(x, y, w, h, if hover { t.accent } else { t.border });
        let ty = y + (h - self.line(t.micro())) / 2.0;
        self.micro(x + 14.0, ty, text, if hover { t.fg } else { t.fg_muted });
        if !hint.is_empty() {
            let hw = self.measure(hint, t.micro(), false);
            self.text(x + w - 14.0 - hw, ty, hint, t.micro(), false, t.fg_subtle);
        }
        self.clicked(x, y, w, h)
    }

    /// The design's segmented choice: boxes in a row, the chosen one filled.
    /// Each box gives up the edge it shares with the one before, so no edge
    /// is drawn twice. Returns the value clicked, if any.
    pub fn segmented(&mut self, x: f32, y: f32, options: &[(&'static str, &str)], current: &str) -> Option<&'static str> {
        let t = self.t.clone();
        let h = t.row();
        let mut cx = x;
        let mut picked = None;
        for (value, text) in options {
            let w = self.micro_width(text) + 20.0;
            let chosen = *value == current;
            let hover = self.hover(cx, y, w, h);
            if chosen {
                self.fill(cx, y, w, h, t.fg, 255);
            } else if hover {
                self.fill(cx, y, w, h, t.raised, 255);
            }
            self.outline(cx, y, w, h, t.border);
            let ty = y + (h - self.line(t.micro())) / 2.0;
            self.micro(cx + 10.0, ty, text, if chosen { t.bg } else { t.fg_muted });
            if self.clicked(cx, y, w, h) {
                picked = Some(*value);
            }
            cx += w - 1.0 / self.scale;
        }
        picked
    }

    /// A section heading with an optional count on the right.
    pub fn heading(&mut self, x: f32, y: f32, w: f32, s: &str, count: Option<&str>) {
        let t = self.t.clone();
        self.text(x, y, &s.to_uppercase(), t.head(), true, t.fg);
        if let Some(c) = count {
            let cw = self.micro_width(c);
            let ty = y + (self.line(t.head()) - self.line(t.micro())) / 2.0;
            self.micro(x + w - cw, ty, c, t.fg_subtle);
        }
    }

    /// Wrapped paragraph; returns the height used.
    pub fn paragraph(&mut self, x: f32, y: f32, w: f32, s: &str, c: Rgb) -> f32 {
        let size = self.t.font_size;
        let lines = text::wrap(s, self.columns(w, size));
        let lh = self.line(size);
        for (i, l) in lines.iter().enumerate() {
            self.text(x, y + i as f32 * lh, l, size, false, c);
        }
        lines.len() as f32 * lh
    }
}
