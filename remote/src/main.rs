//! PulseTerm Remote -- the remote desktop helper PulseTerm launches.
//!
//! A separate program (GPL-3.0; see REMOTE_DESKTOP.md), built only on request
//! with `cargo build -p pulseterm-remote`. PulseTerm starts it with:
//!
//! ```text
//! pulseterm-remote --label user@host:22 --address host --theme '{...}'
//! ```
//!
//! and serves it over stdin/stdout (`bridge`). `--local` instead probes this
//! machine with no PulseTerm and no SSH -- for developing it.
//!
//! One window, one toolkit (SDL3): the setup screen before a stream, the
//! stream with a sidebar during one (`app`).

// A release PulseTerm has no console, and Windows gives a console program
// started from one a console window of its own -- so without this, every
// remote desktop opened from a release build came with a black window. The
// stdin/stdout pipe PulseTerm talks over does not need a console. Debug keeps
// one for the stderr log, as PulseTerm does.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod bridge;
mod chrome;
mod config;
mod pairing;
mod probe;
mod stream;
mod sunshine;
mod theme;
mod ui;

use std::ffi::CString;

use sdl3_sys::everything::*;

use bridge::Bridge;
use theme::Theme;
use ui::text::Fonts;
use ui::Ui;

struct Args {
    label: String,
    address: String,
    theme: Option<String>,
    local: bool,
}

fn args() -> Args {
    let mut out = Args { label: String::new(), address: String::new(), theme: None, local: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--label" => out.label = it.next().unwrap_or_default(),
            "--address" => out.address = it.next().unwrap_or_default(),
            "--theme" => out.theme = it.next(),
            "--local" => out.local = true,
            other => eprintln!("pulseterm-remote: ignoring unknown argument {other:?}"),
        }
    }
    if out.local && out.label.is_empty() {
        out.label = "this computer".into();
        out.address = "localhost".into();
    }
    out
}

fn main() {
    let args = args();
    if args.label.is_empty() {
        eprintln!(
            "pulseterm-remote is started by PulseTerm (the remote desktop icon on an SSH session).\n\
             To try it against this machine: pulseterm-remote --local"
        );
        std::process::exit(2);
    }
    let (bridge, events) = if args.local {
        (Bridge::local(), None)
    } else {
        let (bridge, events) = Bridge::pipe();
        (bridge, Some(events))
    };
    let theme = Theme::from_handoff(args.theme.as_deref());
    let (regular, bold) = theme.font_files();
    let fonts = match Fonts::load(&regular, &bold) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("pulseterm-remote: {e}");
            std::process::exit(1);
        }
    };

    unsafe {
        // Native Wayland first: left alone SDL3 picked X11 through XWayland,
        // and the pointer lock needs the real thing (REMOTE_DESKTOP.md).
        #[cfg(target_os = "linux")]
        SDL_SetHint(c"SDL_VIDEO_DRIVER".as_ptr(), c"wayland,x11".as_ptr());
        SDL_SetHint(c"SDL_APP_ID".as_ptr(), c"pulseterm-remote".as_ptr());
        // On Windows the keyboard grab is a low-level hook, and SDL lets
        // Alt+Tab through it by default; the full keyboard means the host gets
        // that too. Ctrl+Alt+Del and Win+L are Windows' own and no program
        // can take them -- the Ctrl+Alt+Shift hotkeys stay the way back.
        #[cfg(windows)]
        SDL_SetHint(c"SDL_ALLOW_ALT_TAB_WHILE_GRABBED".as_ptr(), c"0".as_ptr());
        if !SDL_Init(SDL_INIT_VIDEO) {
            eprintln!("pulseterm-remote: SDL could not start: {}", std::ffi::CStr::from_ptr(SDL_GetError()).to_string_lossy());
            std::process::exit(1);
        }
        let title = CString::new(format!("PulseTerm Remote — {}", args.label)).unwrap_or_default();
        let window = SDL_CreateWindow(
            title.as_ptr(),
            1100,
            760,
            SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY | chrome::window_flags(),
        );
        // Windows: the window's own bar is the caption (`chrome`).
        if !window.is_null() {
            chrome::install(window);
        }
        // OpenGL over EGL first: it is what zero-copy display imports decoded
        // frames into (stream/zerocopy.rs). Anything else still works, by
        // copying frames.
        SDL_SetHint(c"SDL_VIDEO_FORCE_EGL".as_ptr(), c"1".as_ptr());
        let mut renderer = SDL_CreateRenderer(window, c"opengl".as_ptr());
        if renderer.is_null() {
            renderer = SDL_CreateRenderer(window, std::ptr::null());
        }
        if window.is_null() || renderer.is_null() {
            eprintln!("pulseterm-remote: no window: {}", std::ffi::CStr::from_ptr(SDL_GetError()).to_string_lossy());
            std::process::exit(1);
        }
        SDL_SetRenderVSync(renderer, 1);
        let ui = Ui { r: renderer, fonts, t: theme, scale: 1.0, width: 1100.0, height: 760.0, mouse: (-1.0, -1.0), click: false, wheel: 0.0 };
        let mut app = app::App::new(ui, window, args.label, args.address, bridge, events);
        app.measure();
        app.run();
        drop(app);
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(window);
        SDL_Quit();
    }
}
