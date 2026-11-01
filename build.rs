//! Turns `assets/logo.svg` into the exe's icon.
//!
//! The SVG is rasterised at every size Windows asks an icon for, packed into an
//! `.ico`, and linked in as icon resource **1** -- the id gpui's Windows backend
//! loads for the window class (`LoadImageW(module, MAKEINTRESOURCE(1))`), which
//! is what the title bar, taskbar and Alt-Tab show. Explorer shows the same
//! resource as the file's own icon. The title bar logo inside the app is drawn
//! from a PNG this script also writes (`ui::icon::logo`), so there is one source.

use std::path::PathBuf;
use std::{env, fs};

use resvg::{tiny_skia, usvg};

/// 16/20/24 are small icons at 100-150% display scaling, 32/40/48 the taskbar
/// and Alt-Tab, 64 and 256 Explorer's larger views.
const SIZES: &[u32] = &[16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.svg");
    println!("cargo:rerun-if-changed=build.rs");

    let svg = fs::read("assets/logo.svg").expect("read assets/logo.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())
        .expect("parse assets/logo.svg");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    // The in-app logo, as a small raster. Handed the SVG, gpui's `img()` would
    // render it at the file's own size x2 on every launch -- a 2000x2000
    // texture for this 1000-unit logo, filters and all, for a spot under 20px.
    // 128px stays sharp to 64px on a 2x display. Written on every platform:
    // `ui::icon` includes it.
    fs::write(out.join("logo.png"), render_png(&tree, 128)).expect("write logo.png");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| (size, render_png(&tree, size)))
        .collect();

    let ico = out.join("pulseterm.ico");
    fs::write(&ico, ico_bytes(&images)).expect("write icon");

    // windres reads the quoted path C-style, so backslashes would be escapes.
    let rc = out.join("pulseterm.rc");
    let ico_path = ico.display().to_string().replace('\\', "/");
    fs::write(&rc, format!("1 ICON \"{ico_path}\"\n")).expect("write resource script");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .expect("link the icon resource");
}

fn render_png(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().expect("encode png")
}

/// An `.ico` is a 6-byte header, a 16-byte directory entry per image, then the
/// images. Windows Vista onwards accepts PNG data for every entry, so there is
/// no BMP-and-mask encoding to do.
fn ico_bytes(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        // A width/height byte of 0 means 256.
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // width, height, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}
