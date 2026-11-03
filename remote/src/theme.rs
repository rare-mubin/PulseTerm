//! PulseTerm's design tokens, as handed over on the command line.
//!
//! The helper links none of PulseTerm's code (it is GPL; see REMOTE_DESKTOP.md),
//! so PulseTerm passes its *resolved* theme -- palette, user overrides, font
//! -- as JSON in `--theme`, and this window draws with exactly those. Started
//! without one (`--local`, or by hand), it falls back to Ink, the design's own
//! dark palette.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone)]
pub struct Theme {
    pub font: String,
    pub font_size: f32,
    pub line_height: f32,
    pub bg: Rgb,
    pub surface: Rgb,
    pub raised: Rgb,
    pub border: Rgb,
    pub fg: Rgb,
    pub fg_muted: Rgb,
    pub fg_subtle: Rgb,
    pub accent: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
}

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

fn colour(hex: &str, fallback: &str) -> Rgb {
    let parse = |h: &str| {
        let h = h.trim_start_matches('#');
        (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok()).flatten()
    };
    let v = parse(hex).or_else(|| parse(fallback)).unwrap_or(0);
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Theme {
    pub fn from_handoff(json: Option<&str>) -> Self {
        let h: Handoff = json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
        let d = Handoff::default();
        Self {
            font: h.font,
            font_size: h.font_size.clamp(8., 40.),
            line_height: h.line_height.clamp(10., 60.),
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

    // The design's measurements, derived from the type exactly as PulseTerm's
    // `ui/kit.rs` derives them, so the two windows share a proportion.
    pub fn bar(&self) -> f32 {
        self.line_height + 26.
    }
    pub fn row(&self) -> f32 {
        self.line_height + 10.
    }
    pub fn micro(&self) -> f32 {
        (self.font_size * 0.72).max(8.)
    }
    pub fn head(&self) -> f32 {
        self.font_size * (16. / 12.)
    }

    /// The regular and bold files of the theme's font -- or, when it is not
    /// installed, of the first family PulseTerm itself prefers here. A
    /// proportional fallback would break the whole grid look.
    pub fn font_files(&self) -> (PathBuf, PathBuf) {
        let preferred: &[&str] = if cfg!(windows) {
            &["Cascadia Mono", "Cascadia Code", "JetBrains Mono", "Fira Code", "Consolas", "Courier New"]
        } else {
            &["JetBrains Mono", "Fira Code", "Cascadia Mono", "Adwaita Mono", "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono"]
        };
        let wanted = std::iter::once(self.font.as_str()).filter(|f| !f.is_empty()).chain(preferred.iter().copied());
        for family in wanted {
            if let Some(regular) = find_font(family, false) {
                let bold = find_font(family, true).unwrap_or_else(|| regular.clone());
                return (regular, bold);
            }
        }
        let last = find_font("monospace", false).unwrap_or_default();
        (last.clone(), last)
    }
}

/// A font family's file, if that exact family is installed.
#[cfg(not(windows))]
fn find_font(family: &str, bold: bool) -> Option<PathBuf> {
    let pattern = format!("{family}:{}", if bold { "weight=bold" } else { "style=Regular" });
    let out = std::process::Command::new("fc-match").args(["-f", "%{family}\n%{file}", &pattern]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (found, file) = text.split_once('\n')?;
    // fc-match always answers with *something*; only the family asked for counts.
    let matches = family == "monospace" || found.split(',').any(|f| f.trim().eq_ignore_ascii_case(family));
    (matches && !file.is_empty()).then(|| PathBuf::from(file.trim()))
}

#[cfg(windows)]
fn find_font(family: &str, bold: bool) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Fonts");
    let stem = family.replace(' ', "");
    let names = if bold { vec![format!("{stem}-Bold.ttf"), format!("{stem}bd.ttf")] } else { vec![format!("{stem}.ttf"), format!("{stem}-Regular.ttf")] };
    let names = if family == "Consolas" { vec![if bold { "consolab.ttf".into() } else { "consola.ttf".into() }] } else { names };
    names.into_iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Tracked-out label text: the design's 0.18em is spelled into the string.
/// Display only.
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
        assert_eq!(t.accent, Rgb(0, 255, 0));
        assert_eq!(t.bg, Rgb(0x12, 0x12, 0x12), "bad colour falls back to Ink");
        let t = Theme::from_handoff(Some("not json"));
        assert_eq!(t.fg, Rgb(0xed, 0xea, 0xe3));
    }

    #[test]
    fn labels_are_tracked_capitals() {
        assert_eq!(label("ready"), "R E A D Y");
    }
}
