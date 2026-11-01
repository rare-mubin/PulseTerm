use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Translates a keystroke into the bytes a pty expects.
///
/// Returns `None` for anything the terminal shouldn't swallow -- app shortcuts
/// and modifier-only presses -- so the caller can let the event bubble up to
/// the workspace keybindings instead.
pub fn encode(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let mods = &keystroke.modifiers;
    let key = keystroke.key.as_str();

    // Cursor and keypad keys switch between CSI (`ESC [`) and SS3 (`ESC O`)
    // introducers depending on whether the remote program has put the terminal
    // in application-cursor mode. Getting this wrong is why arrow keys "work in
    // bash but not in vim" in a hand-rolled terminal.
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi = |final_byte: char| -> Option<Vec<u8>> {
        let intro = if app_cursor { 'O' } else { '[' };
        Some(format!("\x1b{intro}{final_byte}").into_bytes())
    };
    // Modified cursor keys always use CSI with a parameter, never SS3.
    let modified = |final_byte: char, param: u8| -> Option<Vec<u8>> {
        Some(format!("\x1b[1;{param}{final_byte}").into_bytes())
    };

    let modifier_param = modifier_param(mods);

    match key {
        "up" | "down" | "right" | "left" => {
            let final_byte = match key {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                _ => 'D',
            };
            match modifier_param {
                Some(param) => modified(final_byte, param),
                None => csi(final_byte),
            }
        }
        "home" => match modifier_param {
            Some(param) => modified('H', param),
            None => csi('H'),
        },
        "end" => match modifier_param {
            Some(param) => modified('F', param),
            None => csi('F'),
        },
        "insert" => Some(b"\x1b[2~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),

        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),

        "enter" => Some(b"\r".to_vec()),
        "tab" => {
            if mods.shift {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(b"\t".to_vec())
            }
        }
        "escape" => Some(b"\x1b".to_vec()),
        "space" if mods.control => Some(vec![0]),
        "space" => Some(b" ".to_vec()),
        "backspace" => {
            // DEL, not BS: that is what `stty` reports as the erase character
            // on essentially every Linux host, and sending 0x08 instead makes
            // readline print `^H` rather than deleting.
            if mods.alt {
                Some(b"\x1b\x7f".to_vec())
            } else {
                Some(b"\x7f".to_vec())
            }
        }

        _ => encode_char(keystroke, mods),
    }
}

fn encode_char(keystroke: &Keystroke, mods: &gpui::Modifiers) -> Option<Vec<u8>> {
    // Anything with a platform-command modifier belongs to the app, not the
    // remote shell.
    if mods.platform {
        return None;
    }

    // A terminal has no distinct encoding for ctrl+shift+<letter> -- it would
    // send the same byte as plain ctrl+<letter> -- so that whole space is free
    // for app shortcuts. Declining to encode it here is what lets ctrl-shift-t
    // open a tab instead of sending ^T to the shell.
    if mods.control && mods.shift {
        return None;
    }

    if mods.control {
        let c = keystroke.key.chars().next()?;
        let byte = control_byte(c)?;
        return Some(if mods.alt {
            vec![0x1b, byte]
        } else {
            vec![byte]
        });
    }

    // `key_char` is the character the layout actually produces -- the one that
    // accounts for dead keys, AltGr, and non-Latin layouts. Falling back to
    // `key` would send the ASCII-equivalent key cap instead of what was typed.
    let text = keystroke.key_char.as_deref().or_else(|| {
        (keystroke.key.chars().count() == 1).then_some(keystroke.key.as_str())
    })?;

    if text.is_empty() {
        return None;
    }

    // Alt-prefixed keys are sent as ESC then the character, which is how a
    // terminal expresses Meta.
    if mods.alt {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(text.as_bytes());
        Some(bytes)
    } else {
        Some(text.as_bytes().to_vec())
    }
}

/// Ctrl-A..Ctrl-Z plus the handful of control codes on punctuation keys.
fn control_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' => Some(0),
        '[' => Some(27),
        '\\' => Some(28),
        ']' => Some(29),
        '^' => Some(30),
        '_' => Some(31),
        '?' => Some(127),
        _ => None,
    }
}

/// xterm's modifier encoding: 1 + shift(1) + alt(2) + ctrl(4). `None` means no
/// modifiers, which selects the unparameterised sequence.
fn modifier_param(mods: &gpui::Modifiers) -> Option<u8> {
    let mut param = 1;
    if mods.shift {
        param += 1;
    }
    if mods.alt {
        param += 2;
    }
    if mods.control {
        param += 4;
    }
    (param > 1).then_some(param)
}
