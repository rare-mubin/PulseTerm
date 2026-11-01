use alacritty_terminal::index::{Column, Point as AlacPoint};
use alacritty_terminal::term::TermMode;
use gpui::{Modifiers, MouseButton};

/// What a mouse gesture should do, once the remote program's mode has had its
/// say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseIntent {
    /// Forward it to the remote program: something over there is listening.
    Report,
    /// Handle it locally — selection, or scrolling our own scrollback.
    Local,
}

/// Decides who gets a mouse event.
///
/// A TUI that has turned mouse tracking on owns the mouse: without this, clicking
/// a button in htop or a pane in tmux only ever painted a local text selection.
/// Holding **shift** overrides and takes it back, which is the long-standing
/// convention for selecting text out of a mouse-grabbing program.
pub fn intent(mode: TermMode, modifiers: &Modifiers) -> MouseIntent {
    if mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift {
        MouseIntent::Report
    } else {
        MouseIntent::Local
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    /// Cursor moved with a button held.
    Drag,
    ScrollUp,
    ScrollDown,
}

/// Encodes a mouse event in whichever protocol the program asked for.
///
/// Returns `None` when the event isn't reportable in the current mode — motion
/// without `MOUSE_DRAG`/`MOUSE_MOTION`, or a release under the legacy protocol
/// on a wheel "button", which has no release.
pub fn encode(
    kind: MouseEventKind,
    button: MouseButton,
    point: AlacPoint<usize>,
    modifiers: &Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    // Motion is only reported when the program asked for it: `MOUSE_DRAG` wants
    // moves while a button is down, `MOUSE_MOTION` wants every move.
    if kind == MouseEventKind::Drag
        && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    {
        return None;
    }

    let mut code = match kind {
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        _ => match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return None,
        },
    };

    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }
    if kind == MouseEventKind::Drag {
        code += 32;
    }

    // Both protocols are 1-based, and the coordinates are viewport rows and
    // columns -- not buffer points, which is what selection uses.
    let column = point.column.0 + 1;
    let line = point.line + 1;

    if mode.contains(TermMode::SGR_MOUSE) {
        // SGR (1006): no coordinate ceiling, and press and release are
        // distinguishable by the final byte, which is why it's preferred.
        let final_byte = if kind == MouseEventKind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{column};{line}{final_byte}").into_bytes());
    }

    // Legacy X10/normal (1000): one byte per field, offset by 32. A release is
    // reported as button 3 rather than as a distinct event, and wheel events
    // have no release at all.
    if kind == MouseEventKind::Release {
        if matches!(kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            return None;
        }
        code = 3 + (code & !3);
    }

    // Beyond 223 the byte would overflow, and there is no way to say so in this
    // protocol; dropping the event beats reporting a wrong cell.
    if column > 223 || line > 223 {
        return None;
    }

    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code as u8,
        32 + column as u8,
        32 + line as u8,
    ])
}

/// Scrolling inside a full-screen program that has *not* asked for mouse
/// reporting: with `ALTERNATE_SCROLL` set on the alternate screen, a wheel tick
/// is conventionally translated into arrow keys, so `less` and `man` scroll the
/// way you expect instead of moving a scrollback that isn't there.
pub fn alternate_scroll(mode: TermMode, lines: i32) -> Option<Vec<u8>> {
    if !mode.contains(TermMode::ALT_SCREEN) || !mode.contains(TermMode::ALTERNATE_SCROLL) {
        return None;
    }
    let sequence: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
        if lines > 0 { b"\x1bOA" } else { b"\x1bOB" }
    } else if lines > 0 {
        b"\x1b[A"
    } else {
        b"\x1b[B"
    };
    Some(sequence.repeat(lines.unsigned_abs() as usize))
}

/// Viewport coordinates for the protocol, from a cell the caller already
/// resolved. Kept here so the 1-based conversion lives next to the encoder that
/// depends on it.
pub fn viewport_point(row: usize, col: usize) -> AlacPoint<usize> {
    AlacPoint::new(row, Column(col))
}
