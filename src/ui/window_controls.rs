//! Caption behaviour that gpui can't do for us on Windows.
//!
//! `Window::start_window_move` is implemented for X11 and Wayland only — the
//! Windows backend never overrides it, so it resolves to the empty default in
//! `platform.rs` and a title-bar drag does nothing. `Window::zoom_window` maps to
//! gpui's `zoom()`, which unconditionally calls `ShowWindowAsync(SW_MAXIMIZE)`
//! and has no restore path, so the button goes dead once maximised.
//!
//! Both are fixed by going through **`WM_SYSCOMMAND`**, which is the OS's own
//! entry point for these. gpui's `handle_system_command` intercepts only
//! `SC_KEYMENU` and returns `None` for everything else, so `SC_MOVE`,
//! `SC_MAXIMIZE` and `SC_RESTORE` all reach `DefWindowProcW` untouched.
//!
//! `SC_MOVE | HTCAPTION` starts the OS's real caption move loop, which is what
//! brings **genuine** Aero Snap with it: the system snap zones, the system
//! preview overlay, Snap Assist, Snap Layouts and Aero Shake. None of that can
//! be had any other way — an app-side imitation gets the geometry but not the
//! shell integration.
//!
//! Note the message that does *not* work here: posting `WM_NCLBUTTONDOWN` with
//! `HTCAPTION`, the usual custom-titlebar trick, is intercepted by gpui in
//! `handle_nc_mouse_down_msg`. It re-injects the message as a synthetic *client*
//! `MouseDownEvent` at the same position — which gpui's own click counter scores
//! as click #2 — and returns `Some(0)` so `DefWindowProc` never runs. That made
//! one physical press arrive twice, the second indistinguishable from a
//! double-click, and no move loop ever started.

use gpui::Window;

/// Handles a left press on the caption: hands the drag to the OS, or toggles
/// maximise/restore on a double-click.
pub fn caption_press(window: &Window) {
    platform::caption_press(window);
}

/// Maximises, or restores if already maximised.
pub fn toggle_maximize(window: &Window) {
    platform::toggle_maximize(window);
}

#[cfg(target_os = "windows")]
mod platform {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, ReleaseCapture};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, IsZoomed, PostMessageW, HTCAPTION, SC_MAXIMIZE, SC_MOVE,
        SC_RESTORE, SM_CXDOUBLECLK, SM_CYDOUBLECLK, WM_SYSCOMMAND,
    };

    thread_local! {
        /// When and where the last caption press landed, for our own
        /// double-click detection. Only touched from the UI thread.
        static LAST_PRESS: Cell<Option<(Instant, POINT)>> = const { Cell::new(None) };
    }

    pub fn caption_press(window: &Window) {
        if is_double_click() {
            toggle_maximize(window);
            return;
        }

        let Some(hwnd) = hwnd(window) else { return };
        let cursor = cursor_pos().unwrap_or_default();
        unsafe {
            // gpui took the capture on mouse-down. The OS will not start its
            // move loop until we give it back.
            let _ = ReleaseCapture();
            // Posted, not sent: the move loop is modal and does not return until
            // the drag ends, which would freeze rendering for its duration.
            // `HTCAPTION` in the low bits selects mouse-driven move rather than
            // the keyboard variant.
            let _ = PostMessageW(
                Some(hwnd),
                WM_SYSCOMMAND,
                WPARAM((SC_MOVE | HTCAPTION) as usize),
                lparam_point(cursor),
            );
        }
    }

    pub fn toggle_maximize(window: &Window) {
        LAST_PRESS.with(|cell| cell.set(None));
        let Some(hwnd) = hwnd(window) else { return };
        unsafe {
            let command = if IsZoomed(hwnd).as_bool() {
                SC_RESTORE
            } else {
                SC_MAXIMIZE
            };
            // Posted for the same reason `ShowWindowAsync` was needed before:
            // dispatching this synchronously from inside a gpui input callback
            // re-enters `WM_SIZE`, gpui misses the resize, and the frame
            // maximises while the renderer keeps drawing at the old size.
            let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(command as usize), LPARAM(0));
        }
    }

    /// gpui's `click_count` cannot be trusted for this: the OS move loop
    /// consumes the mouse-up, so gpui never sees the button released and its
    /// counter never resets — the next single press would arrive claiming
    /// `click_count: 2`. This applies Windows' own rule instead, and consumes
    /// the stored press on a match so a third click starts a fresh pair.
    fn is_double_click() -> bool {
        let Some(point) = cursor_pos() else {
            return false;
        };

        let now = Instant::now();
        let interval = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);
        let (dx, dy) = unsafe {
            (
                GetSystemMetrics(SM_CXDOUBLECLK),
                GetSystemMetrics(SM_CYDOUBLECLK),
            )
        };

        let previous = LAST_PRESS.with(|cell| cell.get());
        let doubled = previous.is_some_and(|(at, from)| {
            now.duration_since(at) <= interval
                && (point.x - from.x).abs() <= dx
                && (point.y - from.y).abs() <= dy
        });

        LAST_PRESS.with(|cell| cell.set((!doubled).then_some((now, point))));
        doubled
    }

    fn lparam_point(point: POINT) -> LPARAM {
        LPARAM(((point.y as isize) << 16) | (point.x as isize & 0xFFFF))
    }

    fn cursor_pos() -> Option<POINT> {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.ok().map(|_| point)
    }

    fn hwnd(window: &Window) -> Option<HWND> {
        // Fully qualified: gpui has an inherent `Window::window_handle` that
        // returns its own `AnyWindowHandle` and would shadow the trait method.
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
            _ => None,
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use gpui::Window;

    pub fn caption_press(window: &Window) {
        window.start_window_move();
    }

    pub fn toggle_maximize(window: &Window) {
        window.zoom_window();
    }
}
