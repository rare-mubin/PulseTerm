//! Keyboard: SDL scancodes to the Windows virtual-key codes the GameStream
//! protocol carries (Sunshine maps them back to the host's own keys).
//!
//! Scancodes, not keycodes: a scancode is the physical key, so the host's
//! layout decides what it types -- the same as a keyboard plugged into it.
//! SDL's scancodes are USB HID usage ids, which is why they are spelled as
//! numbers here.

pub fn virtual_key(scancode: i32) -> Option<i16> {
    let vk: i16 = match scancode {
        4..=29 => 0x41 + (scancode - 4) as i16,  // A..Z
        30..=38 => 0x31 + (scancode - 30) as i16, // 1..9
        39 => 0x30,                               // 0
        40 => 0x0D,                               // Return
        41 => 0x1B,                               // Escape
        42 => 0x08,                               // Backspace
        43 => 0x09,                               // Tab
        44 => 0x20,                               // Space
        45 => 0xBD,                               // -
        46 => 0xBB,                               // =
        47 => 0xDB,                               // [
        48 => 0xDD,                               // ]
        49 | 50 => 0xDC,                          // \ (and the ISO key beside Return)
        51 => 0xBA,                               // ;
        52 => 0xDE,                               // '
        53 => 0xC0,                               // `
        54 => 0xBC,                               // ,
        55 => 0xBE,                               // .
        56 => 0xBF,                               // /
        57 => 0x14,                               // Caps Lock
        58..=69 => 0x70 + (scancode - 58) as i16, // F1..F12
        70 => 0x2C,                               // Print Screen
        71 => 0x91,                               // Scroll Lock
        72 => 0x13,                               // Pause
        73 => 0x2D,                               // Insert
        74 => 0x24,                               // Home
        75 => 0x21,                               // Page Up
        76 => 0x2E,                               // Delete
        77 => 0x23,                               // End
        78 => 0x22,                               // Page Down
        79 => 0x27,                               // Right
        80 => 0x25,                               // Left
        81 => 0x28,                               // Down
        82 => 0x26,                               // Up
        83 => 0x90,                               // Num Lock
        84 => 0x6F,                               // keypad /
        85 => 0x6A,                               // keypad *
        86 => 0x6D,                               // keypad -
        87 => 0x6B,                               // keypad +
        88 => 0x0D,                               // keypad Enter
        89..=97 => 0x61 + (scancode - 89) as i16, // keypad 1..9
        98 => 0x60,                               // keypad 0
        99 => 0x6E,                               // keypad .
        100 => 0xE2,                              // ISO \ left of Z
        101 => 0x5D,                              // Menu
        104..=115 => 0x7C + (scancode - 104) as i16, // F13..F24
        224 => 0xA2,                              // left Ctrl
        225 => 0xA0,                              // left Shift
        226 => 0xA4,                              // left Alt
        227 => 0x5B,                              // left Super
        228 => 0xA3,                              // right Ctrl
        229 => 0xA1,                              // right Shift
        230 => 0xA5,                              // right Alt
        231 => 0x5C,                              // right Super
        _ => return None,
    };
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_modifiers() {
        assert_eq!(virtual_key(4), Some(0x41)); // A
        assert_eq!(virtual_key(29), Some(0x5A)); // Z
        assert_eq!(virtual_key(30), Some(0x31)); // 1
        assert_eq!(virtual_key(39), Some(0x30)); // 0
        assert_eq!(virtual_key(69), Some(0x7B)); // F12
        assert_eq!(virtual_key(225), Some(0xA0)); // left Shift
        assert_eq!(virtual_key(98), Some(0x60)); // keypad 0
        assert_eq!(virtual_key(0), None);
    }
}
