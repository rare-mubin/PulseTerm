//! Text on an SDL renderer: fontdue rasterises each glyph once per size into
//! a small white texture, and drawing tints it. The theme's font is
//! monospace, so measuring a line is counting its characters.

use std::collections::HashMap;

use sdl3_sys::everything::*;

use crate::theme::Rgb;

struct Glyph {
    texture: *mut SDL_Texture,
    width: i32,
    height: i32,
    xmin: i32,
    ymin: i32,
    advance: f32,
}

pub struct Fonts {
    regular: fontdue::Font,
    bold: fontdue::Font,
    cache: HashMap<(char, u32, bool), Glyph>,
}

impl Fonts {
    pub fn load(regular: &std::path::Path, bold: &std::path::Path) -> Result<Self, String> {
        let read = |p: &std::path::Path| {
            let bytes = std::fs::read(p).map_err(|e| format!("cannot read font {}: {e}", p.display()))?;
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).map_err(|e| e.to_string())
        };
        Ok(Self { regular: read(regular)?, bold: read(bold)?, cache: HashMap::new() })
    }

    fn font(&self, bold: bool) -> &fontdue::Font {
        if bold {
            &self.bold
        } else {
            &self.regular
        }
    }

    /// Ascent of a line at `px`, for placing the baseline.
    pub fn ascent(&self, px: f32) -> f32 {
        self.regular.horizontal_line_metrics(px).map(|m| m.ascent).unwrap_or(px * 0.8)
    }

    pub fn measure(&self, text: &str, px: f32, bold: bool) -> f32 {
        let font = self.font(bold);
        text.chars().map(|c| font.metrics(c, px).advance_width).sum()
    }

    /// Draws `text` with its baseline at `y`; returns the advance in pixels.
    pub unsafe fn draw(&mut self, r: *mut SDL_Renderer, text: &str, x: f32, y: f32, px: f32, bold: bool, colour: Rgb, alpha: u8) -> f32 {
        let size = (px * 4.0).round() as u32; // quarter-pixel steps
        let mut pen = x;
        for ch in text.chars() {
            let key = (ch, size, bold);
            if !self.cache.contains_key(&key) {
                let glyph = self.rasterise(r, ch, size as f32 / 4.0, bold);
                self.cache.insert(key, glyph);
            }
            let g = &self.cache[&key];
            if !g.texture.is_null() {
                SDL_SetTextureColorMod(g.texture, colour.0, colour.1, colour.2);
                SDL_SetTextureAlphaMod(g.texture, alpha);
                let dst = SDL_FRect {
                    x: (pen + g.xmin as f32).round(),
                    y: (y - (g.ymin + g.height) as f32).round(),
                    w: g.width as f32,
                    h: g.height as f32,
                };
                SDL_RenderTexture(r, g.texture, std::ptr::null(), &dst);
            }
            pen += g.advance;
        }
        pen - x
    }

    unsafe fn rasterise(&self, r: *mut SDL_Renderer, ch: char, px: f32, bold: bool) -> Glyph {
        let (m, coverage) = self.font(bold).rasterize(ch, px);
        let mut texture = std::ptr::null_mut();
        if m.width > 0 && m.height > 0 {
            // White, with the coverage as alpha: one texture serves any colour.
            let mut rgba = Vec::with_capacity(coverage.len() * 4);
            for a in coverage {
                rgba.extend_from_slice(&[255, 255, 255, a]);
            }
            texture = SDL_CreateTexture(r, SDL_PIXELFORMAT_RGBA32, SDL_TEXTUREACCESS_STATIC, m.width as i32, m.height as i32);
            if !texture.is_null() {
                SDL_UpdateTexture(texture, std::ptr::null(), rgba.as_ptr().cast(), (m.width * 4) as i32);
                SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_BLEND);
            }
        }
        Glyph { texture, width: m.width as i32, height: m.height as i32, xmin: m.xmin, ymin: m.ymin, advance: m.advance_width }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for g in self.cache.values() {
            if !g.texture.is_null() {
                unsafe { SDL_DestroyTexture(g.texture) };
            }
        }
    }
}

/// Splits `text` into lines of at most `width` columns, at spaces where it
/// can. Monospace makes columns the measure.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        while word.chars().count() > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
        }
        let extra = if line.is_empty() { 0 } else { 1 };
        if line.chars().count() + extra + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_breaks_long_words() {
        assert_eq!(wrap("one two three four", 9), vec!["one two", "three", "four"]);
        assert_eq!(wrap("abcdefghijkl", 8), vec!["abcdefgh", "ijkl"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
    }
}
