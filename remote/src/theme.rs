//! PulseTerm's design tokens, as handed over on the command line.
//!
//! The helper links none of PulseTerm's code (it is GPL; see REMOTE_DESKTOP.md),
//! so PulseTerm passes its *resolved* theme -- palette, user overrides, font
//! -- as JSON in `--theme`, and this window draws with exactly those. Started
//! without one (`--local`, or by hand), it falls back to Ink, the design's own
//! dark palette.

use gpui::{px, rgb, Global, Hsla, Pixels};
use serde::Deserialize;

#[derive(Clone)]
pub struct Theme {
    pub font: String,
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub bg: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub border: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub fg_subtle: Hsla,
    pub accent: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
}

impl Global for Theme {}

#[derive(Deserialize)]
#[serde(default)]
struct Handoff {
    font: String,
    font_size: f32,
    line_height: f32,
    bg: String,
    surface: String,
    raised: String,
    border: String,
    fg: String,
    fg_muted: String,
    fg_subtle: String,
    accent: String,
    success: String,
    warning: String,
    danger: String,
}

impl Default for Handoff {
    // Ink, token for token (PulseTerm's `theme::presets`).
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        Self {
            // Empty: `resolve_font` picks the first installed family PulseTerm
            // itself prefers. gpui does not resolve a generic "monospace".
            font: String::new(),
            font_size: 13.,
            line_height: 18.,
            bg: s("#121212"),
            surface: s("#191919"),
            raised: s("#202020"),
            border: s("#2b2b2b"),
            fg: s("#edeae3"),
            fg_muted: s("#9a958b"),
            fg_subtle: s("#5e5a52"),
            accent: s("#ff2e57"),
            success: s("#8fb573"),
            warning: s("#d1a74f"),
            danger: s("#ff2e57"),
        }
    }
}

fn colour(hex: &str, fallback: &str) -> Hsla {
    let parse = |h: &str| u32::from_str_radix(h.trim_start_matches('#'), 16).ok();
    rgb(parse(hex).or_else(|| parse(fallback)).unwrap_or(0)).into()
}

impl Theme {
    pub fn from_handoff(json: Option<&str>) -> Self {
        let h: Handoff = json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        let d = Handoff::default();
        Self {
            font: h.font,
            font_size: px(h.font_size.clamp(8., 40.)),
            line_height: px(h.line_height.clamp(10., 60.)),
            bg: colour(&h.bg, &d.bg),
            surface: colour(&h.surface, &d.surface),
            raised: colour(&h.raised, &d.raised),
            border: colour(&h.border, &d.border),
            fg: colour(&h.fg, &d.fg),
            fg_muted: colour(&h.fg_muted, &d.fg_muted),
            fg_subtle: colour(&h.fg_subtle, &d.fg_subtle),
            accent: colour(&h.accent, &d.accent),
            success: colour(&h.success, &d.success),
            warning: colour(&h.warning, &d.warning),
            danger: colour(&h.danger, &d.danger),
        }
    }

    /// Settles the font on an installed family: the handed-over one if it is
    /// here, otherwise the first of the families PulseTerm's `theme.rs`
    /// prefers on this platform. A family gpui cannot find silently becomes a
    /// proportional one, which breaks the whole grid look.
    pub fn resolve_font(&mut self, installed: &[String]) {
        if installed.iter().any(|i| *i == self.font) {
            return;
        }
        let preferred: &[&str] = if cfg!(windows) {
            &["Cascadia Mono", "Cascadia Code", "JetBrains Mono", "Fira Code", "Consolas", "Courier New"]
        } else {
            &["JetBrains Mono", "Fira Code", "Cascadia Mono", "Adwaita Mono", "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono"]
        };
        self.font = preferred
            .iter()
            .find(|name| installed.iter().any(|i| i == *name))
            .unwrap_or(&preferred[0])
            .to_string();
    }

    // The design's measurements, derived from the type exactly as PulseTerm's
    // `ui/kit.rs` derives them, so the two windows share a proportion.
    pub fn bar(&self) -> Pixels {
        self.line_height + px(26.)
    }
    pub fn row(&self) -> Pixels {
        self.line_height + px(10.)
    }
    pub fn micro(&self) -> Pixels {
        (self.font_size * 0.72).max(px(8.))
    }
    pub fn head(&self) -> Pixels {
        self.font_size * (16. / 12.)
    }
}

/// Tracked-out label text: gpui has no letter spacing, so the design's 0.18em
/// is spelled into the string. Display only.
pub fn label(text: &str) -> String {
    let upper = text.to_uppercase();
    let mut out = String::with_capacity(upper.len() * 2);
    for (ix, ch) in upper.chars().enumerate() {
        if ix > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_or_broken_handoff_still_gives_a_whole_theme() {
        let t = Theme::from_handoff(Some(r##"{"accent":"#00ff00","bg":"nonsense"}"##));
        assert_eq!(t.accent, Hsla::from(rgb(0x00ff00)));
        assert_eq!(t.bg, Hsla::from(rgb(0x121212)), "bad colour falls back to Ink");
        let t = Theme::from_handoff(Some("not json"));
        assert_eq!(t.fg, Hsla::from(rgb(0xedeae3)));
    }

    #[test]
    fn labels_are_tracked_capitals() {
        assert_eq!(label("ready"), "R E A D Y");
    }
}
