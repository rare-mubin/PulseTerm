use std::borrow::Cow;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use gpui::{
    px, svg, AssetSource, Hsla, Image, ImageFormat, ImageSource, IntoElement, Pixels,
    SharedString, Styled,
};
use icons::common::icon_registry_getter::get_icon_elements;
use icons::common::{IconType, StaticSvgElement};

/// The icon set: Lucide, via the `icons` crate from rust-ui.com.
///
/// That crate targets Leptos and Dioxus, and both of those features are off —
/// they generate framework components gpui cannot use. What we take instead is
/// its always-compiled `common` module, which holds the raw path data as
/// `&'static str`. `Assets` renders that into SVG markup on demand, so the whole
/// ~1500-icon set is available without a single file on disk.
pub type Icon = IconType;

/// The app logo: a 128px PNG that `build.rs` rasterises from `assets/logo.svg`,
/// the same file it turns into the exe's icon, so there is one source. Not the
/// SVG itself -- `img()` renders an SVG at its own size x2 on every launch, a
/// 2000x2000 texture for this one. Compiled in, so the exe stays self-contained.
const LOGO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/logo.png"));

/// The logo as an image source for `img()`.
///
/// Handed over as in-memory bytes, never as an asset name: gpui treats a string
/// as a URL whenever it parses as an `http::Uri`, and a bare `logo.png` does --
/// as an authority, the way a host name would -- so `img("logo.png")` went to
/// the HTTP client, failed silently, and drew nothing. Built once; gpui caches
/// the decoded image by the bytes' hash.
pub fn logo() -> ImageSource {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    IMAGE
        .get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, LOGO.to_vec())))
        .clone()
        .into()
}

/// Renders an icon at `size`, tinted with `color`.
///
/// The generated SVG uses `currentColor`, so the tint is just `text_color` and an
/// icon inherits a container's hover colour like any other glyph.
pub fn icon(which: Icon, size: Pixels, color: Hsla) -> impl IntoElement {
    svg()
        .path(format!("icons/{}.svg", which.as_ref()))
        .size(size)
        .flex_none()
        .text_color(color)
}

/// Default icon size: a little under the line height, so an icon beside a label
/// reads as the same weight rather than towering over it.
pub fn small() -> Pixels {
    px(14.)
}

/// Serves icons by synthesising them from the registry's path data.
///
/// gpui's `svg()` resolves a *path* through the asset source, so the path is
/// used as the lookup key: `icons/<IconType name>.svg`. `IconType` derives
/// strum's `EnumString`, which is what turns that name back into the variant.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|name| name.strip_suffix(".svg"))
        else {
            return Ok(None);
        };
        let Some(icon) = resolve(name) else {
            return Ok(None);
        };
        let Some(elements) = get_icon_elements(icon) else {
            return Ok(None);
        };
        Ok(Some(Cow::Owned(render_svg(elements).into_bytes())))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        // Only ever asked for by directory-style lookups, which nothing here
        // does -- icons are always requested by exact name.
        Ok(Vec::new())
    }
}

/// Resolves an icon name in either spelling it arrives in.
///
/// Our own call sites ask for the registry's PascalCase names (`ChevronDown`).
/// gpui-component's editor asks for the very same Lucide icons in kebab-case
/// (`chevron-down`), through this same asset source -- so without accepting that
/// spelling too, its search bar renders every icon blank.
fn resolve(name: &str) -> Option<IconType> {
    if let Ok(icon) = IconType::from_str(name) {
        return Some(icon);
    }
    // The one name gpui-component spells differently from Lucide itself.
    if name == "close" {
        return Some(IconType::X);
    }
    let pascal: String = name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    IconType::from_str(&pascal).ok()
}

/// Wraps the registry's elements in Lucide's standard envelope: a 24x24 viewBox
/// with 2px round-capped strokes and no fill.
fn render_svg(elements: &[StaticSvgElement]) -> String {
    let mut svg = String::from(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
    );
    for element in elements {
        match element {
            StaticSvgElement::Path { d } => {
                svg.push_str(&format!(r#"<path d="{d}"/>"#));
            }
            StaticSvgElement::Circle { cx, cy, r } => {
                svg.push_str(&format!(r#"<circle cx="{cx}" cy="{cy}" r="{r}"/>"#));
            }
            StaticSvgElement::Rect {
                x,
                y,
                width,
                height,
                rx,
                ry,
            } => {
                svg.push_str(&format!(r#"<rect x="{x}" y="{y}" width="{width}" height="{height}""#));
                if let Some(rx) = rx {
                    svg.push_str(&format!(r#" rx="{rx}""#));
                }
                if let Some(ry) = ry {
                    svg.push_str(&format!(r#" ry="{ry}""#));
                }
                svg.push_str("/>");
            }
            StaticSvgElement::Ellipse { cx, cy, rx, ry } => {
                svg.push_str(&format!(
                    r#"<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}"/>"#
                ));
            }
            StaticSvgElement::Line { x1, y1, x2, y2 } => {
                svg.push_str(&format!(
                    r#"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}"/>"#
                ));
            }
            StaticSvgElement::Polyline { points } => {
                svg.push_str(&format!(r#"<polyline points="{points}"/>"#));
            }
            StaticSvgElement::Polygon { points } => {
                svg.push_str(&format!(r#"<polygon points="{points}"/>"#));
            }
        }
    }
    svg.push_str("</svg>");
    svg
}

/// Picks a file icon from a filename, the way an editor's explorer does.
pub fn for_filename(name: &str) -> Icon {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "rs" | "go" | "c" | "h" | "cpp" | "hpp" | "py" | "rb" | "java" | "kt" | "swift" | "cs"
        | "php" | "lua" | "pl" | "r" | "scala" | "hs" | "ml" | "zig" => IconType::FileCode,
        "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "vue" | "svelte" => IconType::FileCode,
        "html" | "htm" | "xml" | "css" | "scss" | "sass" | "less" => IconType::FileCode,
        "json" | "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" | "env" | "properties" => {
            IconType::FileCog
        }
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => IconType::FileTerminal,
        "md" | "markdown" | "rst" | "txt" | "log" => IconType::FileText,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "ico" | "tiff" => {
            IconType::FileImage
        }
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "flv" => IconType::FileVideo,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => IconType::FileAudio,
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" => IconType::FileArchive,
        "pdf" => IconType::FileType,
        "lock" => IconType::FileLock,
        "db" | "sqlite" | "sql" => IconType::Database,
        _ => IconType::File,
    }
}
