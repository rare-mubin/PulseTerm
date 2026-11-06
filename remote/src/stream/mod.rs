//! The stream: moonlight-common-c's connection, drawn into the one window.
//!
//! * moonlight-common-c runs the connection on its own threads (RTSP,
//!   control, video, input); `LiStartConnection` blocks for the whole
//!   handshake, so it is called from a thread of its own and the window keeps
//!   painting meanwhile;
//! * video goes through `decoder` (FFmpeg, VA-API) into `decoder::LATEST`, and
//!   the window's thread uploads the newest picture and draws it letterboxed;
//! * keyboard and mouse go back to the host from the window's events.
//!
//! moonlight-common-c holds one connection per process, in globals, so this
//! module does too. Each `Stream` takes a generation number, and connection
//! events carry it, so a late event from a stream already stopped is ignored.

pub mod ffi;
pub mod grab;
mod audio;
mod decoder;
mod keys;
mod zerocopy;

use std::ffi::{CStr, CString};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use sdl3_sys::everything::*;
use serde::{Deserialize, Serialize};

use crate::ui::Ui;
use decoder::{Frame, Layout, Picture, DECODED, DECODE_US, HOST_FRAMES, HOST_US, LATEST, VIDEO_BYTES, ZERO_COPY};
use zerocopy::ZeroCopy;

pub use decoder::WAKE;

/// Everything a stream needs, from the launch request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    /// The host as dialled; video, audio and input go to it directly.
    pub address: String,
    pub app_version: String,
    pub gfe_version: String,
    pub rtsp_url: String,
    pub codec_support: i32,
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    /// Kilobits per second.
    pub bitrate: i32,
    /// `VIDEO_FORMAT_*` mask this client will decode.
    pub formats: i32,
    pub audio_configuration: i32,
    pub input_key: [u8; 16],
    pub input_key_id: i32,
    /// Play the host's sound here.
    pub audio: bool,
}

/// SDL event type for "the connection changed state"; `user.code` is 0 for
/// started or the error, `user.data1` the stream's generation.
pub static CONNECTION: AtomicU32 = AtomicU32::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static TERMINATED: AtomicI32 = AtomicI32::new(0);
static STAGE_FAILED: AtomicI32 = AtomicI32::new(0);

pub struct Stream {
    pub session: Session,
    generation: u64,
    pub started: bool,
    texture: *mut SDL_Texture,
    shape: (i32, i32, Layout),
    /// Zero-copy display, once set up on this window's renderer; `None` with
    /// `zero_copy_tried` means the copy path (`texture`) is in use.
    zero_copy: Option<ZeroCopy>,
    zero_copy_tried: bool,
    /// The picture's rectangle in window points, for mouse positions.
    video: (f32, f32, f32, f32),
    carry: (f32, f32),
    /// Motion gathered since the last flush: one update per pass of the
    /// window's loop, not one per event -- a 1000 Hz mouse or a touchpad
    /// otherwise sends hundreds of input packets a second, crowding the
    /// stream the host is sending back.
    pending_rel: (f32, f32),
    pending_pos: Option<(f32, f32)>,
    /// Wheel movement not yet sent, in 1/120ths of a detent: a touchpad
    /// scrolls in fractions, and rounding each event to a step would lose
    /// them.
    wheel_carry: (f32, f32),
    /// Buttons and keys the host has been told are down, so they can be
    /// released when this window can no longer see them come up -- focus
    /// lost, the sidebar opened, the stream replaced. A button released
    /// outside the window otherwise stays held on the host.
    buttons: u8,
    keys: std::collections::BTreeSet<i16>,
    /// Decoded and shown frames per second, measured every two seconds.
    pub fps: (f32, f32),
    /// Over the same two seconds, in milliseconds: the network's round trip,
    /// the host's processing per frame (0 when Sunshine does not say), and
    /// decoding here.
    pub latency: (f32, f32, f32),
    /// The picture's received bitrate over the same two seconds, Mbps.
    pub received_mbps: f32,
    video_bytes: u64,
    tally: (Instant, u64, u64),
    latency_tally: (u64, u64, u64),
    shown: u64,
}

impl Stream {
    pub fn start(session: Session) -> Self {
        set_muted(!session.audio);
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        TERMINATED.store(0, Ordering::Relaxed);
        STAGE_FAILED.store(0, Ordering::Relaxed);
        ZERO_COPY.store(false, Ordering::Relaxed);
        *LATEST.lock().unwrap() = None;
        let connecting = session.clone();
        std::thread::spawn(move || {
            let result = unsafe { connect(&connecting) };
            post(generation, if result == 0 { 0 } else { result.min(-1) });
        });
        Self {
            session,
            generation,
            started: false,
            texture: null_mut(),
            shape: (0, 0, Layout::Nv12),
            zero_copy: None,
            zero_copy_tried: false,
            video: (0.0, 0.0, 0.0, 0.0),
            carry: (0.0, 0.0),
            pending_rel: (0.0, 0.0),
            pending_pos: None,
            wheel_carry: (0.0, 0.0),
            buttons: 0,
            keys: Default::default(),
            fps: (0.0, 0.0),
            latency: (0.0, 0.0, 0.0),
            received_mbps: 0.0,
            video_bytes: VIDEO_BYTES.load(Ordering::Relaxed),
            tally: (Instant::now(), DECODED.load(Ordering::Relaxed), 0),
            latency_tally: (DECODE_US.load(Ordering::Relaxed), HOST_US.load(Ordering::Relaxed), HOST_FRAMES.load(Ordering::Relaxed)),
            shown: 0,
        }
    }

    /// Whether a picture has been drawn yet.
    pub fn has_picture(&self) -> bool {
        self.picture().is_some()
    }

    /// The texture on screen and its size, from whichever path drew it.
    fn picture(&self) -> Option<(*mut SDL_Texture, (i32, i32))> {
        if let Some(t) = self.zero_copy.as_ref().and_then(|z| z.texture()) {
            return Some(t);
        }
        (!self.texture.is_null()).then_some((self.texture, (self.shape.0, self.shape.1)))
    }

    /// Hands over the last picture, to stay on screen while the stream is
    /// replaced -- a settings change rejoins the host, and a black window in
    /// between would read as a disconnect.
    pub fn freeze(&mut self) -> Option<Frozen> {
        if let Some((texture, (w, h))) = self.zero_copy.as_ref().and_then(|z| z.texture()) {
            // The importer owns the texture and the frame under it; it goes
            // along with the picture.
            let keep = self.zero_copy.take();
            return Some(Frozen { texture, size: (w as f32, h as f32), _zero_copy: keep });
        }
        if self.texture.is_null() {
            return None;
        }
        let texture = std::mem::replace(&mut self.texture, null_mut());
        Some(Frozen { texture, size: (self.shape.0 as f32, self.shape.1 as f32), _zero_copy: None })
    }

    /// A connection event. Returns why the stream ended, if it did.
    pub fn on_connection(&mut self, event: &SDL_UserEvent) -> Option<String> {
        if event.data1 as u64 != self.generation {
            return None;
        }
        if event.code == 0 {
            self.started = true;
            return None;
        }
        Some(end_reason(event.code))
    }

    /// Draws the newest picture letterboxed into `area` (window points): the
    /// whole window, or what the docked sidebar leaves of it. Returns whether
    /// a new picture arrived.
    pub fn draw(&mut self, ui: &Ui, area: (f32, f32, f32, f32)) -> bool {
        unsafe {
            // Zero-copy is VA-API surfaces mapped to DRM PRIME and imported
            // through EGL -- a Linux path end to end. On Windows the D3D11
            // frames are downloaded instead, and there is nothing to try.
            if !self.zero_copy_tried && cfg!(not(windows)) {
                self.zero_copy_tried = true;
                match ZeroCopy::new(ui.r) {
                    Ok(z) => {
                        eprintln!("pulseterm-remote: zero-copy display on");
                        self.zero_copy = Some(z);
                        ZERO_COPY.store(true, Ordering::Relaxed);
                    }
                    Err(why) => eprintln!("pulseterm-remote: zero-copy display off ({why}); copying frames"),
                }
            }
        }
        let frame = LATEST.lock().unwrap().take();
        let fresh = frame.is_some();
        unsafe {
            match frame {
                Some(Frame::Prime(prime)) => {
                    let shown = self.zero_copy.as_mut().map(|z| z.show(ui.r, prime));
                    if let Some(Err(why)) = shown {
                        // Off for the rest of this stream: the decoder copies
                        // from its next frame on.
                        eprintln!("pulseterm-remote: zero-copy display failed ({why}); copying frames");
                        ZERO_COPY.store(false, Ordering::Relaxed);
                        self.zero_copy = None;
                    }
                }
                Some(Frame::Copied(p)) => {
                    if self.texture.is_null() || self.shape != (p.width, p.height, p.layout) {
                        if !self.texture.is_null() {
                            SDL_DestroyTexture(self.texture);
                        }
                        let format = match p.layout {
                            Layout::Nv12 => SDL_PIXELFORMAT_NV12,
                            Layout::I420 => SDL_PIXELFORMAT_IYUV,
                        };
                        self.texture = video_texture(ui.r, format, p.width, p.height);
                        self.shape = (p.width, p.height, p.layout);
                    }
                    upload(self.texture, &p);
                    // Frames come by copy from here on; drop a stale import.
                    self.zero_copy = None;
                }
                None => {}
            }
            if let Some((texture, (tw, th))) = self.picture() {
                let (tw, th) = (tw as f32, th as f32);
                let (ax, ay, aw, ah) = area;
                let scale = (aw / tw).min(ah / th);
                let (w, h) = (tw * scale, th * scale);
                // `video` is in window points, so pointer positions map
                // through it however the area is offset.
                self.video = (ax + (aw - w) / 2.0, ay + (ah - h) / 2.0, w, h);
                let s = ui.scale;
                let dst = SDL_FRect { x: self.video.0 * s, y: self.video.1 * s, w: w * s, h: h * s };
                SDL_RenderTexture(ui.r, texture, null(), &dst);
            }
        }
        if fresh {
            self.shown += 1;
        }
        let elapsed = self.tally.0.elapsed().as_secs_f32();
        if elapsed >= 2.0 {
            let decoded = DECODED.load(Ordering::Relaxed);
            self.fps = ((decoded - self.tally.1) as f32 / elapsed, (self.shown - self.tally.2) as f32 / elapsed);
            let (dec_us, host_us, host_n) =
                (DECODE_US.load(Ordering::Relaxed), HOST_US.load(Ordering::Relaxed), HOST_FRAMES.load(Ordering::Relaxed));
            let frames = (decoded - self.tally.1).max(1) as f32;
            let decode_ms = (dec_us - self.latency_tally.0) as f32 / frames / 1000.0;
            let host_frames = (host_n - self.latency_tally.2) as f32;
            let host_ms = if host_frames > 0.0 { (host_us - self.latency_tally.1) as f32 / host_frames / 1000.0 } else { 0.0 };
            let mut rtt = 0u32;
            let mut variance = 0u32;
            let network_ms = if self.started && unsafe { ffi::LiGetEstimatedRttInfo(&mut rtt, &mut variance) } { rtt as f32 } else { 0.0 };
            self.latency = (network_ms, host_ms, decode_ms);
            let bytes = VIDEO_BYTES.load(Ordering::Relaxed);
            self.received_mbps = (bytes - self.video_bytes) as f32 * 8.0 / elapsed / 1_000_000.0;
            self.video_bytes = bytes;
            if std::env::var_os("PULSETERM_REMOTE_STATS").is_some() {
                // Opt-in diagnostics: one line per two seconds on stderr.
                let (q, target, gaps, drops, late) = audio_readings();
                eprintln!(
                    "pulseterm-remote: stats rtt {network_ms:.0}±{variance} ms · host {host_ms:.1} · decode {decode_ms:.1} · {:.0}/{:.0} fps · {:.1} Mbps · audio {q}/{target} gaps {gaps} drops {drops} late {late}",
                    self.fps.0, self.fps.1, self.received_mbps
                );
            }
            self.latency_tally = (dec_us, host_us, host_n);
            self.tally = (Instant::now(), decoded, self.shown);
        }
        fresh
    }

    pub fn key(&mut self, key: &SDL_KeyboardEvent) {
        if !self.started || key.repeat {
            return;
        }
        if let Some(vk) = keys::virtual_key(key.scancode.0) {
            // An up the host never had a down for is dropped, so a key held
            // when the stream began cannot reach it half-pressed.
            if !key.down && !self.keys.remove(&vk) {
                return;
            }
            if key.down {
                self.keys.insert(vk);
            }
            let action = if key.down { ffi::KEY_ACTION_DOWN } else { ffi::KEY_ACTION_UP };
            unsafe { ffi::LiSendKeyboardEvent(vk, action as _, modifiers(key.r#mod) as _) };
        }
    }

    /// Releases on the host every button and key it believes are held.
    pub fn release_all(&mut self) {
        if !self.started {
            return;
        }
        unsafe {
            for b in 1..=5u8 {
                if self.buttons & (1 << b) != 0 {
                    ffi::LiSendMouseButtonEvent(ffi::BUTTON_ACTION_RELEASE as _, b as i32);
                }
            }
            for vk in std::mem::take(&mut self.keys) {
                ffi::LiSendKeyboardEvent(vk, ffi::KEY_ACTION_UP as _, 0);
            }
        }
        self.buttons = 0;
    }

    /// Whether a window point is over the picture (not the letterbox).
    pub fn over_picture(&self, x: f32, y: f32) -> bool {
        inside(self.video, x, y).is_some()
    }

    /// Pointer motion: relative while captured, else a position in the
    /// picture -- held at its edge when the pointer is in the letterbox, so a
    /// drag that overshoots keeps going and the host's screen edges are easy
    /// to reach.
    pub fn motion(&mut self, m: &SDL_MouseMotionEvent, captured: bool) {
        if !self.started {
            return;
        }
        // Gathered, sent by `flush_motion`: relative movement summed (none is
        // lost), a position replaced by the newest.
        if captured {
            self.pending_rel.0 += m.xrel;
            self.pending_rel.1 += m.yrel;
        } else {
            self.pending_pos = Some((m.x, m.y));
        }
    }

    /// Sends the motion gathered since the last flush. Called once per pass
    /// of the window's loop, and before any button or wheel event, so a
    /// click always lands where the pointer was.
    pub fn flush_motion(&mut self) {
        if !self.started {
            return;
        }
        unsafe {
            if self.pending_rel != (0.0, 0.0) {
                // Fractions are kept for next time; whole pixels go out.
                self.carry.0 += self.pending_rel.0;
                self.carry.1 += self.pending_rel.1;
                self.pending_rel = (0.0, 0.0);
                let (dx, dy) = (self.carry.0.trunc(), self.carry.1.trunc());
                self.carry.0 -= dx;
                self.carry.1 -= dy;
                if dx != 0.0 || dy != 0.0 {
                    ffi::LiSendMouseMoveEvent(dx as i16, dy as i16);
                }
            }
            if let Some((x, y)) = self.pending_pos.take() {
                if let Some((x, y)) = clamped(self.video, x, y) {
                    ffi::LiSendMousePositionEvent(x, y, self.video.2 as i16, self.video.3 as i16);
                }
            }
        }
    }

    pub fn button(&mut self, b: &SDL_MouseButtonEvent) {
        // SDL numbers buttons 1..5 (left, middle, right, back, forward)
        // exactly as the protocol does.
        if !self.started || !(1..=5).contains(&b.button) {
            return;
        }
        self.flush_motion();
        let bit = 1u8 << b.button;
        if !b.down && self.buttons & bit == 0 {
            return; // never pressed on the host (pressed on the sidebar, say)
        }
        if b.down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        let action = if b.down { ffi::BUTTON_ACTION_PRESS } else { ffi::BUTTON_ACTION_RELEASE };
        unsafe { ffi::LiSendMouseButtonEvent(action as _, b.button as i32) };
    }

    /// Wheel and touchpad scrolling. The values are sent as they arrive --
    /// already in the direction this computer's own setting gives them, so a
    /// "natural scroll" touchpad scrolls the host the way it scrolls here.
    /// (Undoing the flip, as this did for a while, reversed it on the host.)
    /// `reversed` turns it round on purpose, for a host that wants that.
    pub fn wheel(&mut self, w: &SDL_MouseWheelEvent, reversed: bool) {
        if !self.started {
            return;
        }
        self.flush_motion();
        let sign = if reversed { -1.0 } else { 1.0 };
        // 120 is one detent, the Windows convention the protocol uses;
        // fractions are carried to the next event.
        self.wheel_carry.0 += w.x * sign * 120.0;
        self.wheel_carry.1 += w.y * sign * 120.0;
        let (dx, dy) = (self.wheel_carry.0.trunc(), self.wheel_carry.1.trunc());
        self.wheel_carry.0 -= dx;
        self.wheel_carry.1 -= dy;
        unsafe {
            if dy != 0.0 {
                ffi::LiSendHighResScrollEvent(dy as i16);
            }
            if dx != 0.0 {
                ffi::LiSendHighResHScrollEvent(dx as i16);
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.release_all();
        // Interrupt a handshake still running, end a stream that started.
        // Either way later events from this generation are ignored.
        unsafe {
            if self.started {
                ffi::LiStopConnection();
            } else {
                ffi::LiInterruptConnection();
            }
            if !self.texture.is_null() {
                SDL_DestroyTexture(self.texture);
            }
        }
    }
}

/// Low-latency mode, the parts that live here: the audio queue's limit.
/// (Vsync is the window's: `App::sync_latency`.)
pub fn set_low_latency(low: bool) {
    audio::BASE_MS.store(if low { 10 } else { 20 }, Ordering::Relaxed);
}

/// Audio readings: (queued ms, gaps, drops, longest wait between packets in
/// ms -- reset by reading, so it shows the worst since the last look).
pub fn audio_readings() -> (i32, i32, u64, u64, f32) {
    (
        audio::QUEUED_MS.load(Ordering::Relaxed),
        audio::TARGET_MS.load(Ordering::Relaxed),
        audio::GAPS.load(Ordering::Relaxed),
        audio::DROPS.load(Ordering::Relaxed),
        audio::LATE_US.swap(0, Ordering::Relaxed) as f32 / 1000.0,
    )
}

/// Mutes or unmutes the stream's sound here, at once.
/// Whether a decoded frame is waiting to be drawn.
pub fn frame_waiting() -> bool {
    LATEST.lock().unwrap().is_some()
}

pub fn set_muted(muted: bool) {
    audio::MUTED.store(muted, Ordering::Relaxed);
}

/// The last picture of a stream being replaced.
pub struct Frozen {
    texture: *mut SDL_Texture,
    size: (f32, f32),
    /// Set when the picture is a zero-copy import, which owns `texture`.
    _zero_copy: Option<ZeroCopy>,
}

impl Frozen {
    pub fn draw(&self, ui: &Ui, area: (f32, f32, f32, f32)) {
        let (tw, th) = self.size;
        let (ax, ay, aw, ah) = area;
        let scale = (aw / tw).min(ah / th);
        let (w, h) = (tw * scale, th * scale);
        let s = ui.scale;
        let dst = SDL_FRect { x: (ax + (aw - w) / 2.0) * s, y: (ay + (ah - h) / 2.0) * s, w: w * s, h: h * s };
        unsafe { SDL_RenderTexture(ui.r, self.texture, null(), &dst) };
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        if self._zero_copy.is_none() {
            unsafe { SDL_DestroyTexture(self.texture) };
        }
    }
}

/// A streaming texture for copied frames, in the stream's colour space:
/// BT.709, limited range, as `connect` asks the host for. SDL's default for
/// YUV is BT.601, which shifted reds and greens slightly.
unsafe fn video_texture(r: *mut SDL_Renderer, format: SDL_PixelFormat, w: i32, h: i32) -> *mut SDL_Texture {
    let props = SDL_CreateProperties();
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, format.0 as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_STREAMING.0 as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, w as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, h as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, SDL_COLORSPACE_BT709_LIMITED.0 as i64);
    let texture = SDL_CreateTextureWithProperties(r, props);
    SDL_DestroyProperties(props);
    texture
}

fn post(generation: u64, code: i32) {
    unsafe {
        let mut e: SDL_Event = std::mem::zeroed();
        e.r#type = CONNECTION.load(Ordering::Relaxed);
        e.user.code = code;
        e.user.data1 = generation as usize as *mut std::ffi::c_void;
        SDL_PushEvent(&mut e);
    }
}

/// Starts the connection; returns 0 once streaming, else the error. Blocking.
unsafe fn connect(s: &Session) -> i32 {
    let address = CString::new(s.address.clone()).unwrap_or_default();
    let app_version = CString::new(s.app_version.clone()).unwrap_or_default();
    let gfe_version = CString::new(s.gfe_version.clone()).unwrap_or_default();
    let rtsp_url = CString::new(s.rtsp_url.clone()).unwrap_or_default();

    let mut server: ffi::SERVER_INFORMATION = std::mem::zeroed();
    ffi::LiInitializeServerInformation(&mut server);
    server.address = address.as_ptr();
    server.serverInfoAppVersion = app_version.as_ptr();
    server.serverInfoGfeVersion = gfe_version.as_ptr();
    server.rtspSessionUrl = rtsp_url.as_ptr();
    server.serverCodecModeSupport = s.codec_support;

    let mut config: ffi::STREAM_CONFIGURATION = std::mem::zeroed();
    ffi::LiInitializeStreamConfiguration(&mut config);
    config.width = s.width;
    config.height = s.height;
    config.fps = s.fps;
    config.bitrate = s.bitrate;
    config.packetSize = 1392;
    config.streamingRemotely = ffi::STREAM_CFG_AUTO as i32;
    config.audioConfiguration = s.audio_configuration;
    config.supportedVideoFormats = s.formats;
    config.colorSpace = ffi::COLORSPACE_REC_709 as i32;
    config.colorRange = ffi::COLOR_RANGE_LIMITED as i32;
    config.encryptionFlags = ffi::ENCFLG_ALL as i32;
    for (i, b) in s.input_key.iter().enumerate() {
        config.remoteInputAesKey[i] = *b as _;
    }
    // The IV is the key id, big-endian, then zeros -- as every client does.
    for (i, b) in s.input_key_id.to_be_bytes().iter().enumerate() {
        config.remoteInputAesIv[i] = *b as _;
    }

    let mut listener: ffi::CONNECTION_LISTENER_CALLBACKS = std::mem::zeroed();
    ffi::LiInitializeConnectionCallbacks(&mut listener);
    listener.stageFailed = Some(stage_failed);
    listener.connectionTerminated = Some(terminated);
    let mut video = decoder::callbacks();
    let mut audio = audio::callbacks();

    ffi::LiStartConnection(&mut server, &mut config, &mut listener, &mut video, &mut audio, null_mut(), 0, null_mut(), 0)
}

unsafe extern "C" fn stage_failed(stage: i32, error: i32) {
    STAGE_FAILED.store(stage, Ordering::Relaxed);
    let name = CStr::from_ptr(ffi::LiGetStageName(stage)).to_string_lossy();
    eprintln!("pulseterm-remote: {name} failed ({error})");
}

unsafe extern "C" fn terminated(error: i32) {
    TERMINATED.store(error, Ordering::Relaxed);
    post(GENERATION.load(Ordering::Relaxed), if error == 0 { -1 } else { error });
}

/// Why a stream ended, in words a user can act on.
fn end_reason(code: i32) -> String {
    let stage = STAGE_FAILED.load(Ordering::Relaxed);
    if stage != 0 {
        let name = unsafe { CStr::from_ptr(ffi::LiGetStageName(stage)).to_string_lossy().into_owned() };
        return format!("Could not connect: {name} failed. The host's Sunshine ports may be blocked.");
    }
    match TERMINATED.load(Ordering::Relaxed).min(code) {
        -1 | 0 => "The host ended the stream.".into(),
        -100 => "No video arrived -- a firewall on the host may be blocking UDP 47998-48000.".into(),
        -101 => "The stream stopped sending pictures.".into(),
        -102 => "The connection ended early -- the host may have run out of encoder or capture.".into(),
        -103 => "The host refused to stream protected content.".into(),
        other => format!("The connection ended ({other})."),
    }
}

/// The modifier bits the protocol carries beside each key.
fn modifiers(m: SDL_Keymod) -> u8 {
    let mut out = 0u8;
    if m.0 & SDL_KMOD_SHIFT.0 != 0 {
        out |= ffi::MODIFIER_SHIFT as u8;
    }
    if m.0 & SDL_KMOD_CTRL.0 != 0 {
        out |= ffi::MODIFIER_CTRL as u8;
    }
    if m.0 & SDL_KMOD_ALT.0 != 0 {
        out |= ffi::MODIFIER_ALT as u8;
    }
    if m.0 & SDL_KMOD_GUI.0 != 0 {
        out |= ffi::MODIFIER_META as u8;
    }
    out
}

/// A window point inside the picture, relative to the picture's corner.
fn inside(video: (f32, f32, f32, f32), x: f32, y: f32) -> Option<(i16, i16)> {
    let (vx, vy, vw, vh) = video;
    if vw <= 0.0 || x < vx || y < vy || x >= vx + vw || y >= vy + vh {
        return None;
    }
    Some(((x - vx) as i16, (y - vy) as i16))
}

/// A window point as a position in the picture, held at its edge when it is
/// outside. `None` until the picture has a size.
fn clamped(video: (f32, f32, f32, f32), x: f32, y: f32) -> Option<(i16, i16)> {
    let (vx, vy, vw, vh) = video;
    if vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    Some(((x - vx).clamp(0.0, vw - 1.0) as i16, (y - vy).clamp(0.0, vh - 1.0) as i16))
}

unsafe fn upload(texture: *mut SDL_Texture, p: &Picture) {
    match p.layout {
        Layout::Nv12 => {
            SDL_UpdateNVTexture(texture, null(), p.planes[0].0.as_ptr(), p.planes[0].1, p.planes[1].0.as_ptr(), p.planes[1].1);
        }
        Layout::I420 => {
            SDL_UpdateYUVTexture(
                texture,
                null(),
                p.planes[0].0.as_ptr(),
                p.planes[0].1,
                p.planes[1].0.as_ptr(),
                p.planes[1].1,
                p.planes[2].0.as_ptr(),
                p.planes[2].1,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_positions_are_relative_to_the_picture() {
        let video = (100.0, 50.0, 800.0, 450.0);
        assert_eq!(inside(video, 100.0, 50.0), Some((0, 0)));
        assert_eq!(inside(video, 899.0, 499.0), Some((799, 449)));
        assert_eq!(inside(video, 99.0, 60.0), None, "letterbox is not the picture");
        assert_eq!(inside(video, 900.0, 60.0), None);
    }

    #[test]
    fn positions_outside_the_picture_are_held_at_its_edge() {
        let video = (100.0, 50.0, 800.0, 450.0);
        assert_eq!(clamped(video, 10.0, 10.0), Some((0, 0)), "the letterbox's corner is the picture's");
        assert_eq!(clamped(video, 5000.0, 300.0), Some((799, 250)));
        assert_eq!(clamped((0.0, 0.0, 0.0, 0.0), 1.0, 1.0), None);
    }
}
