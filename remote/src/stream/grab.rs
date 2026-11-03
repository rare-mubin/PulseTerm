//! Full keyboard takeover: while it is held, this computer sees no key at
//! all -- not niri's shortcuts, not its Mod for Alt+drag -- and the stream
//! window reads the keyboards itself and sends every key to the host.
//!
//! The keyboards are claimed below the compositor, with an exclusive evdev
//! grab (`EVIOCGRAB`) on each `/dev/input/eventN` that udev marks
//! `ID_INPUT_KEYBOARD`. The compositor's shortcut inhibit is not enough: niri
//! still sees the modifiers, and starts its own Mod+drag on them
//! (REMOTE_DESKTOP.md, "Full keyboard"). The mouse and touchpad are left
//! alone; only keys are taken.
//!
//! Safety, because a keyboard that never comes back is the worst thing this
//! can do:
//!
//! * the grab starts only when no key is down, or the compositor would see a
//!   press and never its release, and think a key stuck;
//! * the Ctrl+Alt+Shift hotkeys are still read here and never sent;
//! * the window's loop beats a heartbeat; two seconds without it (the UI
//!   hung) and the grab lets go by itself;
//! * the kernel ends a grab when its file closes, so a crash ends it too.
//!
//! It needs read access to the keyboard devices, which a desktop user does
//! not have by default: `GRANT` is the one-time udev rule that gives the
//! logged-in user keyboards (only), and without it the app falls back to the
//! compositor's shortcut inhibit.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use super::ffi;

/// The one-time permission: a udev rule giving the active user's session
/// access to keyboard devices -- keyboards only, not mice or other input.
pub const GRANT: &str = "echo 'SUBSYSTEM==\"input\", KERNEL==\"event*\", ENV{ID_INPUT_KEYBOARD}==\"1\", TAG+=\"uaccess\"' | sudo tee /etc/udev/rules.d/70-pulseterm-keyboards.rules && sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=input";

pub enum GrabError {
    /// No permission to open the keyboards; `GRANT` fixes it.
    NoAccess,
    /// A key is down; try again in a moment.
    KeyHeld,
    /// Nothing to grab, or not Linux.
    Unavailable,
}

/// Hotkeys read while grabbed, handed to the window as an SDL user event
/// whose `code` is one of these.
pub const HOTKEY_CAPTURE: i32 = 1;
pub const HOTKEY_SIDEBAR: i32 = 2;
pub const HOTKEY_QUIT: i32 = 3;
pub const HOTKEY_FULLSCREEN: i32 = 4;

pub struct Grab {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Grab {
    /// Claims every keyboard. `hotkey_event` is the SDL user event the
    /// Ctrl+Alt+Shift hotkeys are posted as; `heartbeat` the window loop's
    /// last pass, in milliseconds since `epoch`.
    #[cfg(target_os = "linux")]
    pub fn start(hotkey_event: u32, heartbeat: Arc<AtomicU64>, epoch: std::time::Instant) -> Result<Self, GrabError> {
        let devices = imp::keyboards();
        if devices.is_empty() {
            return Err(GrabError::Unavailable);
        }
        let mut fds = Vec::new();
        for path in &devices {
            match imp::open(path) {
                Ok(fd) => fds.push(fd),
                Err(e) => {
                    for fd in fds {
                        imp::close(fd, false);
                    }
                    return Err(e);
                }
            }
        }
        if fds.iter().any(|&fd| imp::any_key_down(fd)) {
            for fd in fds {
                imp::close(fd, false);
            }
            return Err(GrabError::KeyHeld);
        }
        for &fd in &fds {
            if !imp::grab(fd, true) {
                for fd in fds {
                    imp::close(fd, true);
                }
                return Err(GrabError::Unavailable);
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (stop2, done2) = (stop.clone(), done.clone());
        let thread = std::thread::Builder::new()
            .name("remote-keyboard".into())
            .spawn(move || {
                imp::run(&fds, &stop2, hotkey_event, &heartbeat, epoch);
                for fd in fds {
                    imp::close(fd, true);
                }
                done2.store(true, Ordering::Relaxed);
            })
            .map_err(|_| GrabError::Unavailable)?;
        Ok(Self { stop, done, thread: Some(thread) })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn start(_: u32, _: Arc<AtomicU64>, _: std::time::Instant) -> Result<Self, GrabError> {
        Err(GrabError::Unavailable)
    }

    /// The grab let go by itself (the heartbeat stopped).
    pub fn ended(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

impl Drop for Grab {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// evdev key codes (linux/input-event-codes.h) to the Windows virtual keys
/// the protocol carries -- the same targets as `keys::virtual_key`, from the
/// kernel's numbering instead of SDL's.
pub fn evdev_vk(code: u16) -> Option<i16> {
    const ROW_Q: &[u8] = b"QWERTYUIOP";
    const ROW_A: &[u8] = b"ASDFGHJKL";
    const ROW_Z: &[u8] = b"ZXCVBNM";
    let vk: i16 = match code {
        1 => 0x1B,
        2..=10 => 0x31 + (code - 2) as i16, // 1..9
        11 => 0x30,
        12 => 0xBD,
        13 => 0xBB,
        14 => 0x08,
        15 => 0x09,
        16..=25 => ROW_Q[(code - 16) as usize] as i16,
        26 => 0xDB,
        27 => 0xDD,
        28 => 0x0D,
        29 => 0xA2,
        30..=38 => ROW_A[(code - 30) as usize] as i16,
        39 => 0xBA,
        40 => 0xDE,
        41 => 0xC0,
        42 => 0xA0,
        43 => 0xDC,
        44..=50 => ROW_Z[(code - 44) as usize] as i16,
        51 => 0xBC,
        52 => 0xBE,
        53 => 0xBF,
        54 => 0xA1,
        55 => 0x6A,
        56 => 0xA4,
        57 => 0x20,
        58 => 0x14,
        59..=68 => 0x70 + (code - 59) as i16, // F1..F10
        69 => 0x90,
        70 => 0x91,
        71 => 0x67,
        72 => 0x68,
        73 => 0x69,
        74 => 0x6D,
        75 => 0x64,
        76 => 0x65,
        77 => 0x66,
        78 => 0x6B,
        79 => 0x61,
        80 => 0x62,
        81 => 0x63,
        82 => 0x60,
        83 => 0x6E,
        86 => 0xE2,
        87 => 0x7A,
        88 => 0x7B,
        96 => 0x0D,
        97 => 0xA3,
        98 => 0x6F,
        99 => 0x2C,
        100 => 0xA5,
        102 => 0x24,
        103 => 0x26,
        104 => 0x21,
        105 => 0x25,
        106 => 0x27,
        107 => 0x23,
        108 => 0x28,
        109 => 0x22,
        110 => 0x2D,
        111 => 0x2E,
        119 => 0x13,
        125 => 0x5B,
        126 => 0x5C,
        127 => 0x5D,
        183..=194 => 0x7C + (code - 183) as i16, // F13..F24
        _ => return None,
    };
    Some(vk)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::BTreeSet;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use sdl3_sys::everything::*;

    use super::{evdev_vk, ffi, GrabError};

    // _IOW('E', 0x90, int) and _IOC(_IOC_READ, 'E', 0x18, 96).
    const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
    const EVIOCGKEY_96: libc::c_ulong = 0x8060_4518;
    const EV_KEY: u16 = 1;

    /// Keyboard devices, as udev labels them (`ID_INPUT_KEYBOARD=1` in its
    /// database) -- read from /run/udev, so no libudev is needed.
    pub fn keyboards() -> Vec<String> {
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir("/sys/class/input") else { return out };
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("event") {
                continue;
            }
            let Ok(dev) = std::fs::read_to_string(entry.path().join("dev")) else { continue };
            let db = std::fs::read_to_string(format!("/run/udev/data/c{}", dev.trim())).unwrap_or_default();
            if db.lines().any(|l| l == "E:ID_INPUT_KEYBOARD=1") {
                out.push(format!("/dev/input/{name}"));
            }
        }
        out.sort();
        out
    }

    pub fn open(path: &str) -> Result<i32, GrabError> {
        let c = CString::new(path).map_err(|_| GrabError::Unavailable)?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            let err = std::io::Error::last_os_error().raw_os_error();
            return Err(if err == Some(libc::EACCES) || err == Some(libc::EPERM) {
                GrabError::NoAccess
            } else {
                GrabError::Unavailable
            });
        }
        Ok(fd)
    }

    pub fn any_key_down(fd: i32) -> bool {
        let mut state = [0u8; 96];
        let r = unsafe { libc::ioctl(fd, EVIOCGKEY_96, state.as_mut_ptr()) };
        r >= 0 && state.iter().any(|&b| b != 0)
    }

    pub fn grab(fd: i32, on: bool) -> bool {
        unsafe { libc::ioctl(fd, EVIOCGRAB, if on { 1 as libc::c_int } else { 0 }) == 0 }
    }

    pub fn close(fd: i32, grabbed: bool) {
        if grabbed {
            grab(fd, false);
        }
        unsafe { libc::close(fd) };
    }

    #[repr(C)]
    struct InputEvent {
        time: libc::timeval,
        kind: u16,
        code: u16,
        value: i32,
    }

    pub fn run(fds: &[i32], stop: &AtomicBool, hotkey_event: u32, heartbeat: &AtomicU64, epoch: std::time::Instant) {
        let mut pollfds: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        let mut pressed: BTreeSet<i16> = BTreeSet::new();
        let mut swallowed: BTreeSet<u16> = BTreeSet::new();
        let (mut ctrl, mut shift, mut alt, mut meta) = (0u8, 0u8, 0u8, 0u8);
        while !stop.load(Ordering::Relaxed) {
            // The window's loop has not passed in two seconds: it hung. Let go.
            let now = epoch.elapsed().as_millis() as u64;
            if now.saturating_sub(heartbeat.load(Ordering::Relaxed)) > 2000 {
                eprintln!("pulseterm-remote: the window stopped responding -- keyboard released");
                break;
            }
            let ready = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 100) };
            if ready <= 0 {
                continue;
            }
            for p in &mut pollfds {
                if p.revents & libc::POLLIN == 0 {
                    continue;
                }
                loop {
                    let mut ev: InputEvent = unsafe { std::mem::zeroed() };
                    let n = unsafe { libc::read(p.fd, (&mut ev as *mut InputEvent).cast(), std::mem::size_of::<InputEvent>()) };
                    if n != std::mem::size_of::<InputEvent>() as isize {
                        break;
                    }
                    if ev.kind != EV_KEY || ev.value == 2 {
                        continue; // only presses and releases; the host repeats keys itself
                    }
                    let down = ev.value == 1;
                    let bit = |m: &mut u8, b: u8| if down { *m |= b } else { *m &= !b };
                    match ev.code {
                        29 => bit(&mut ctrl, 1),
                        97 => bit(&mut ctrl, 2),
                        42 => bit(&mut shift, 1),
                        54 => bit(&mut shift, 2),
                        56 => bit(&mut alt, 1),
                        100 => bit(&mut alt, 2),
                        125 => bit(&mut meta, 1),
                        126 => bit(&mut meta, 2),
                        _ => {}
                    }
                    // Ctrl+Alt+Shift+Z/S/Q/X stay ours, even now.
                    if down && ctrl != 0 && alt != 0 && shift != 0 {
                        let hotkey = match ev.code {
                            44 => super::HOTKEY_CAPTURE,
                            31 => super::HOTKEY_SIDEBAR,
                            16 => super::HOTKEY_QUIT,
                            45 => super::HOTKEY_FULLSCREEN,
                            _ => 0,
                        };
                        if hotkey != 0 {
                            swallowed.insert(ev.code);
                            unsafe {
                                let mut e: SDL_Event = std::mem::zeroed();
                                e.r#type = hotkey_event;
                                e.user.code = hotkey;
                                SDL_PushEvent(&mut e);
                            }
                            continue;
                        }
                    }
                    if !down && swallowed.remove(&ev.code) {
                        continue;
                    }
                    let Some(vk) = evdev_vk(ev.code) else { continue };
                    if !down && !pressed.remove(&vk) {
                        continue;
                    }
                    if down {
                        pressed.insert(vk);
                    }
                    let mods = (if shift != 0 { ffi::MODIFIER_SHIFT } else { 0 })
                        | (if ctrl != 0 { ffi::MODIFIER_CTRL } else { 0 })
                        | (if alt != 0 { ffi::MODIFIER_ALT } else { 0 })
                        | (if meta != 0 { ffi::MODIFIER_META } else { 0 });
                    let action = if down { ffi::KEY_ACTION_DOWN } else { ffi::KEY_ACTION_UP };
                    unsafe { ffi::LiSendKeyboardEvent(vk, action as _, mods as _) };
                }
            }
        }
        // Nothing stays held on the host when the grab ends.
        for vk in pressed {
            unsafe { ffi::LiSendKeyboardEvent(vk, ffi::KEY_ACTION_UP as _, 0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_codes_map_like_the_sdl_ones() {
        assert_eq!(evdev_vk(16), Some(b'Q' as i16));
        assert_eq!(evdev_vk(30), Some(b'A' as i16));
        assert_eq!(evdev_vk(50), Some(b'M' as i16));
        assert_eq!(evdev_vk(11), Some(0x30)); // 0
        assert_eq!(evdev_vk(56), Some(0xA4)); // left Alt
        assert_eq!(evdev_vk(125), Some(0x5B)); // left Super
        assert_eq!(evdev_vk(88), Some(0x7B)); // F12
        assert_eq!(evdev_vk(0), None);
    }

    #[test]
    fn the_grant_is_keyboards_only() {
        assert!(GRANT.contains("ID_INPUT_KEYBOARD") && GRANT.contains("uaccess"));
        assert!(!GRANT.contains("ID_INPUT_MOUSE"));
    }
}
