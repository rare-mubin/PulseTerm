//! The window's own caption, on Windows.
//!
//! SDL gives a window the system frame by default, and on Windows that is a
//! titlebar *above* the setup screen's bar -- two captions, which the design
//! refuses in PulseTerm itself. So there the window is borderless and its own
//! bar is the caption, through the platform's own route: `SDL_SetWindowHitTest`,
//! which SDL answers from `WM_NCHITTEST`. Windows then runs the real caption
//! move loop -- Aero Snap, Snap Layouts, double-click to maximise -- and its own
//! edge resizing; nothing about moving or sizing is imitated here. What the
//! frame took with it, minimise / maximise / close, is drawn in the bar
//! (`buttons`).
//!
//! On Linux the compositor decides about decorations and this is all a no-op.

use std::sync::Mutex;

use sdl3_sys::everything::*;

use crate::ui::Ui;

/// Whether the window draws its own caption: Windows only.
pub const OWN_CAPTION: bool = cfg!(windows);

/// How far in from the right edge the pointer reveals the sidebar's arrow.
/// Elsewhere the last few pixels do; on Windows those are the resize border
/// (`EDGE`), which Windows keeps for itself -- the app never sees the pointer
/// there -- so the zone is the strip just inside it instead.
pub fn reveal_zone() -> (f32, f32) {
    if OWN_CAPTION {
        (EDGE + 10.0, EDGE)
    } else {
        (4.0, 0.0)
    }
}

/// How far in from a free edge a press resizes rather than reaching the
/// content. The design's seams are 7px for the same reason: a hairline is too
/// thin to hit.
pub const EDGE: f32 = 7.0;

type Rect = (f32, f32, f32, f32);

/// What the hit test needs from the frame just drawn, in window points. Set
/// every frame by whatever drew the bar; the callback runs on the same thread,
/// so the lock is never contended.
struct Layout {
    /// The caption -- the bar, where a press moves the window. `None` while
    /// nothing should: the stream picture belongs to the host.
    caption: Option<Rect>,
    /// The buttons inside it, which stay ordinary clicks.
    buttons: Rect,
}

static LAYOUT: Mutex<Layout> = Mutex::new(Layout { caption: None, buttons: (0.0, 0.0, 0.0, 0.0) });

/// The flag the window is created with on top of SDL's own.
pub fn window_flags() -> SDL_WindowFlags {
    if OWN_CAPTION {
        SDL_WINDOW_BORDERLESS
    } else {
        SDL_WindowFlags(0)
    }
}

/// Hands the window's caption and edges to the hit test. Call once, after the
/// window exists.
pub unsafe fn install(window: *mut SDL_Window) {
    if OWN_CAPTION {
        SDL_SetWindowHitTest(window, Some(hit_test), std::ptr::null_mut());
    }
}

/// Where this frame's caption is, and the buttons inside it.
pub fn set_caption(caption: Option<Rect>, buttons: Rect) {
    if let Ok(mut layout) = LAYOUT.lock() {
        layout.caption = caption;
        layout.buttons = buttons;
    }
}

fn inside((x, y, w, h): Rect, px: f32, py: f32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

unsafe extern "C" fn hit_test(
    window: *mut SDL_Window,
    area: *const SDL_Point,
    _: *mut std::ffi::c_void,
) -> SDL_HitTestResult {
    let (x, y) = ((*area).x as f32, (*area).y as f32);
    let flags = SDL_GetWindowFlags(window);
    // A maximised or full-screen window's edges are against the screen; a
    // resize handle there would be a strip of dead pixels.
    if flags & (SDL_WINDOW_MAXIMIZED | SDL_WINDOW_FULLSCREEN) == 0 {
        let (mut w, mut h) = (0, 0);
        SDL_GetWindowSize(window, &mut w, &mut h);
        let (w, h) = (w as f32, h as f32);
        let (left, right) = (x < EDGE, x >= w - EDGE);
        let (top, bottom) = (y < EDGE, y >= h - EDGE);
        match (top, bottom, left, right) {
            (true, _, true, _) => return SDL_HITTEST_RESIZE_TOPLEFT,
            (true, _, _, true) => return SDL_HITTEST_RESIZE_TOPRIGHT,
            (_, true, true, _) => return SDL_HITTEST_RESIZE_BOTTOMLEFT,
            (_, true, _, true) => return SDL_HITTEST_RESIZE_BOTTOMRIGHT,
            (true, ..) => return SDL_HITTEST_RESIZE_TOP,
            (_, true, ..) => return SDL_HITTEST_RESIZE_BOTTOM,
            (_, _, true, _) => return SDL_HITTEST_RESIZE_LEFT,
            (_, _, _, true) => return SDL_HITTEST_RESIZE_RIGHT,
            _ => {}
        }
    }
    let Ok(layout) = LAYOUT.lock() else {
        return SDL_HITTEST_NORMAL;
    };
    match layout.caption {
        Some(_) if inside(layout.buttons, x, y) => SDL_HITTEST_NORMAL,
        Some(caption) if inside(caption, x, y) => SDL_HITTEST_DRAGGABLE,
        _ => SDL_HITTEST_NORMAL,
    }
}

/// Draws minimise, maximise (or restore) and close at the right end of a bar
/// `bar` tall ending at `right`, acts on a click, and returns where they start
/// so the bar's other contents can stop short of them. Square cells the bar's
/// full height, like PulseTerm's own, so they are easy to hit.
pub fn buttons(ui: &mut Ui, window: *mut SDL_Window, right: f32, bar: f32) -> f32 {
    if !OWN_CAPTION {
        return right;
    }
    let t = ui.t.clone();
    let cell = bar;
    let left = right - cell * 3.0;
    let maximized = unsafe { SDL_GetWindowFlags(window) } & SDL_WINDOW_MAXIMIZED != 0;
    let glyph = 10.0;
    for i in 0..3 {
        let x = left + cell * i as f32;
        let hovered = ui.hover(x, 0.0, cell, bar);
        if hovered {
            // Close goes red, as everywhere; the other two only lift.
            let ground = if i == 2 { t.danger } else { t.raised };
            ui.fill(x, 0.0, cell, bar, ground, 255);
        }
        let ink = if hovered { t.fg } else { t.fg_muted };
        let (cx, cy) = (x + cell / 2.0, bar / 2.0);
        let (gx, gy) = (cx - glyph / 2.0, cy - glyph / 2.0);
        match i {
            0 => ui.hline(gx, cy, glyph, ink),
            1 if maximized => {
                // Restore: two offset squares, the one behind showing its corner.
                ui.outline(gx, gy + 2.0, glyph - 2.0, glyph - 2.0, ink);
                ui.hline(gx + 2.0, gy, glyph - 2.0, ink);
                ui.vline(gx + glyph - 1.0, gy, glyph - 2.0, ink);
            }
            1 => ui.outline(gx, gy, glyph, glyph, ink),
            _ => line_x(ui, gx, gy, glyph, ink),
        }
        if ui.clicked(x, 0.0, cell, bar) {
            unsafe {
                match i {
                    0 => {
                        SDL_MinimizeWindow(window);
                    }
                    1 if maximized => {
                        SDL_RestoreWindow(window);
                    }
                    1 => {
                        SDL_MaximizeWindow(window);
                    }
                    _ => {
                        // The same way out as the system's own close: the
                        // event loop ends the stream and lets go of the pipe.
                        let mut quit: SDL_Event = std::mem::zeroed();
                        quit.r#type = SDL_EVENT_QUIT.0;
                        SDL_PushEvent(&mut quit);
                    }
                }
            }
        }
    }
    left
}

/// The close glyph: two hairline diagonals.
fn line_x(ui: &Ui, x: f32, y: f32, size: f32, c: crate::theme::Rgb) {
    let s = ui.scale;
    unsafe {
        SDL_SetRenderDrawColor(ui.r, c.0, c.1, c.2, 255);
        SDL_RenderLine(ui.r, x * s, y * s, (x + size) * s, (y + size) * s);
        SDL_RenderLine(ui.r, (x + size) * s, y * s, x * s, (y + size) * s);
    }
}
