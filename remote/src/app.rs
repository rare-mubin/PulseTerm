//! PulseTerm Remote: one window for everything.
//!
//! Before a stream it is the setup screen -- the host, its checks, the stream
//! settings, Connect. While streaming the picture fills the window and a
//! translucent arrow on the right edge opens a sidebar with the stream's
//! readings, the controls and the same settings. One window, one toolkit:
//! SDL3, because the stream needs it (pointer lock, video), and the setup
//! screen is drawn in PulseTerm's design language by `ui`.
//!
//! Work that waits -- probing the host, starting Sunshine, pairing, the launch
//! request -- runs on threads and comes back as a `Msg` on a channel; an SDL
//! event wakes the loop so it is handled at once.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use sdl3_sys::everything::*;

use crate::bridge::{Bridge, Event};
use crate::config::{self, StreamConfig};
use crate::pairing::{self, Endpoints, Identity};
use crate::probe::{self, Facts, State};
use crate::stream::grab::{self as keygrab, Grab, GrabError};
use crate::stream::{self, Session, Stream};
use crate::sunshine::{self, Instance, Tuning};
use crate::theme::Rgb;
use crate::ui::Ui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorState {
    Normal,
    Hidden,
    Dot,
}

/// A small dot cursor -- the theme's ink with a ground-coloured ring, so it
/// shows on any picture -- sized for the screen's density, hot spot centred.
fn dot_cursor(t: &crate::theme::Theme, scale: f32) -> *mut SDL_Cursor {
    let size = (11.0 * scale).round().max(7.0) as i32;
    let c = (size - 1) as f32 / 2.0;
    let (inner, outer) = (2.6 * scale, 4.2 * scale);
    unsafe {
        let surface = SDL_CreateSurface(size, size, SDL_PIXELFORMAT_ARGB8888);
        if surface.is_null() {
            return std::ptr::null_mut();
        }
        let s = &*surface;
        for y in 0..size {
            let row = (s.pixels as *mut u8).add((y * s.pitch) as usize) as *mut u32;
            for x in 0..size {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                let (rgb, a) = if d <= inner {
                    (t.fg, 255u32)
                } else if d <= outer {
                    (t.bg, 230)
                } else {
                    (t.bg, 0)
                };
                *row.add(x as usize) = (a << 24) | ((rgb.0 as u32) << 16) | ((rgb.1 as u32) << 8) | rgb.2 as u32;
            }
        }
        let cursor = SDL_CreateColorCursor(surface, size / 2, size / 2);
        SDL_DestroySurface(surface);
        cursor
    }
}

/// Paired for this launch: the identity the host now trusts, its pinned
/// certificate, and the local ends of the SSH forwards to its HTTP ports.
#[derive(Clone)]
pub struct Paired {
    pub identity: Identity,
    pub server_cert: openssl::x509::X509,
    pub at: Endpoints,
}

enum Probe {
    Checking,
    Done(Facts),
    Failed(String),
}

/// PulseTerm's own Sunshine instance on the host.
enum Sun {
    Stopped,
    Starting,
    Pairing(Instance),
    Paired(Instance, Box<Paired>),
    Failed(String),
}

impl Sun {
    fn instance(&self) -> Option<Instance> {
        match self {
            Sun::Pairing(i) | Sun::Paired(i, _) => Some(*i),
            _ => None,
        }
    }
}

enum Msg {
    Probed(Result<Facts, String>),
    Started(Result<(Instance, Vec<sunshine::Display>), String>),
    Paired(Instance, Result<Paired, String>),
    Launched(Result<Session, String>),
    Stopped,
}

pub struct App {
    pub ui: Ui,
    window: *mut SDL_Window,
    host: String,
    address: String,
    bridge: Bridge,
    events: Option<futures::channel::mpsc::UnboundedReceiver<Event>>,
    probe: Probe,
    config: StreamConfig,
    sun: Sun,
    notice: Option<String>,
    stream: Option<Stream>,
    /// Asked for, not yet answered: Connect is pressed and a launch is out.
    launching: bool,
    sidebar: bool,
    captured: bool,
    /// The compositor is asked to pass its shortcuts through (the fallback
    /// when the keyboards cannot be claimed outright).
    keyboard_grabbed: bool,
    /// The keyboards claimed outright: this computer sees no key at all.
    grab: Option<Grab>,
    /// Claiming them was refused (no permission) at this moment; tried again
    /// every few seconds, so running the grant takes effect without a restart.
    grab_denied: Option<Instant>,
    /// The loop's pulse, read by the grab's thread: two seconds without one
    /// and it lets go by itself.
    heartbeat: std::sync::Arc<std::sync::atomic::AtomicU64>,
    epoch: Instant,
    hotkey_event: u32,
    /// Since when the pointer has rested on the window's right edge: the
    /// arrow appears after a moment there, so it does not cover a strip of
    /// the host's screen the rest of the time.
    edge_since: Option<Instant>,
    /// The audio line in the sidebar, refreshed once a second.
    audio_reading: (i32, i32, u64, u64, f32),
    audio_tick: Instant,
    /// Vsync as last set: on normally, off in low-latency mode.
    vsync: bool,
    /// What this computer's cursor is doing over the window right now.
    cursor_state: CursorState,
    /// The small dot "both" uses over the picture, made once.
    dot: *mut SDL_Cursor,
    fullscreen: bool,
    scroll: f32,
    side_scroll: f32,
    settings_scroll: f32,
    /// The host-side settings the running Sunshine was started with; Connect
    /// restarts it when they have changed since.
    running_tuning: Option<Tuning>,
    /// The host's displays, as its Sunshine listed them when it started.
    displays: Vec<sunshine::Display>,
    /// Stopping in order to start again with new host settings.
    restart: bool,
    /// The last picture of a stream being replaced, shown until the new one
    /// draws -- what makes a settings change look like a hiccup, not a
    /// disconnect.
    frozen: Option<stream::Frozen>,
    /// A stream setting changed mid-stream: rejoin with it at this moment
    /// (a short pause, so clicking through options rejoins once).
    apply_at: Option<Instant>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    wake: u32,
}

impl App {
    pub fn new(
        ui: Ui,
        window: *mut SDL_Window,
        host: String,
        address: String,
        bridge: Bridge,
        events: Option<futures::channel::mpsc::UnboundedReceiver<Event>>,
    ) -> Self {
        let (tx, rx) = channel();
        let wake = unsafe { SDL_RegisterEvents(1) };
        stream::WAKE.store(wake, std::sync::atomic::Ordering::Relaxed);
        stream::CONNECTION.store(unsafe { SDL_RegisterEvents(1) }, std::sync::atomic::Ordering::Relaxed);
        let config = config::load(&host);
        let mut app = Self {
            ui,
            window,
            host,
            address,
            bridge,
            events,
            probe: Probe::Checking,
            config,
            sun: Sun::Stopped,
            notice: None,
            stream: None,
            launching: false,
            sidebar: false,
            captured: false,
            keyboard_grabbed: false,
            grab: None,
            grab_denied: None,
            heartbeat: Default::default(),
            epoch: Instant::now(),
            hotkey_event: unsafe { SDL_RegisterEvents(1) },
            edge_since: None,
            audio_reading: (0, 0, 0, 0, 0.0),
            audio_tick: Instant::now(),
            vsync: true,
            cursor_state: CursorState::Normal,
            dot: std::ptr::null_mut(),
            fullscreen: false,
            scroll: 0.0,
            side_scroll: 0.0,
            settings_scroll: 0.0,
            running_tuning: None,
            displays: Vec::new(),
            restart: false,
            frozen: None,
            apply_at: None,
            tx,
            rx,
            wake,
        };
        app.recheck();
        app
    }

    /// Runs `work` on a thread and wakes the loop with its message.
    fn task(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, wake) = (self.tx.clone(), self.wake);
        std::thread::spawn(move || {
            let _ = tx.send(work());
            unsafe {
                let mut e: SDL_Event = std::mem::zeroed();
                e.r#type = wake;
                SDL_PushEvent(&mut e);
            }
        });
    }

    // --- actions -------------------------------------------------------------

    fn recheck(&mut self) {
        self.probe = Probe::Checking;
        self.notice = None;
        let bridge = self.bridge.clone();
        self.task(move || {
            Msg::Probed(block_on(bridge.exec(probe::SCRIPT)).map(|out| Facts::parse(&out.stdout)))
        });
    }

    fn connect(&mut self) {
        if self.stream.is_some() || self.launching {
            return;
        }
        let changed = self.host_pending();
        match self.sun {
            // Host settings changed since Sunshine started: restart it (and
            // pair again) before streaming, or they would not apply.
            Sun::Paired(..) if changed => {
                self.restart = true;
                self.stop_sunshine();
                self.notice = Some("Restarting Sunshine with the new host settings…".into());
            }
            Sun::Paired(..) => self.launch(),
            Sun::Stopped | Sun::Failed(_) => self.start_sunshine(),
            Sun::Starting | Sun::Pairing(_) => {}
        }
    }

    fn start_sunshine(&mut self) {
        let Probe::Done(facts) = &self.probe else { return };
        let (port, capture) = (sunshine::base_port(facts), probe::capture(facts));
        let tuning = Tuning::from(&self.config);
        self.running_tuning = Some(tuning.clone());
        self.sun = Sun::Starting;
        self.notice = None;
        let bridge = self.bridge.clone();
        self.task(move || Msg::Started(block_on(sunshine::start(&bridge, port, capture, &tuning))));
    }

    fn pair(&mut self, instance: Instance) {
        self.sun = Sun::Pairing(instance);
        let bridge = self.bridge.clone();
        self.task(move || Msg::Paired(instance, pair(&bridge, instance)));
    }

    fn launch(&mut self) {
        let (Sun::Paired(_, paired), Probe::Done(facts)) = (&self.sun, &self.probe) else { return };
        let plan = StreamPlan::new(&self.config, facts, self.window_pixels());
        self.notice = Some(format!("Starting the desktop at {}x{} {} fps…", plan.width, plan.height, plan.fps));
        self.launching = true;
        let (paired, address) = ((**paired).clone(), self.address.clone());
        let (audio, on_host) = (self.config.sound != "host", self.config.sound != "here");
        self.task(move || Msg::Launched(launch(&paired, &plan, address, audio, on_host)));
    }

    /// Host CPU settings changed since the running Sunshine started.
    fn host_pending(&self) -> bool {
        let wanted = Tuning::from(&self.config);
        self.running_tuning.as_ref().is_some_and(|t| t.restart_needed(&wanted))
    }

    /// A stream setting (resolution, frame rate, codec, bitrate) changed. Mid-
    /// stream, it is applied by rejoining the host shortly after -- the
    /// protocol fixes them per session, so no client changes them in place.
    fn stream_setting_changed(&mut self) {
        if self.stream.is_some() || self.frozen.is_some() {
            self.apply_at = Some(Instant::now() + Duration::from_millis(700));
        }
    }

    /// Rejoins the host with the current settings, keeping the last picture
    /// up meanwhile. Still paired, same Sunshine: about a second.
    fn rejoin(&mut self) {
        if !matches!(self.sun, Sun::Paired(..)) || self.launching {
            return;
        }
        if let Some(mut stream) = self.stream.take() {
            if let Some(f) = stream.freeze() {
                self.frozen = Some(f);
            }
        }
        self.launch();
        self.notice = Some("applying settings…".into());
    }

    /// Applies host CPU settings mid-stream: Sunshine restarts with them,
    /// pairs again and the stream resumes, the last picture up meanwhile.
    fn apply_host(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            if let Some(f) = stream.freeze() {
                self.frozen = Some(f);
            }
        }
        self.restart = true;
        let notice = "restarting sunshine with the new host settings…";
        self.stop_sunshine();
        self.notice = Some(notice.into());
    }

    /// Ends the stream; the window goes back to the setup screen, still
    /// paired, so Connect opens it again at once.
    fn disconnect(&mut self, why: Option<String>) {
        self.stream = None;
        self.frozen = None;
        self.apply_at = None;
        self.sidebar = false;
        self.set_captured(false);
        self.notice = why.or_else(|| Some("Disconnected. Connect opens the desktop again.".into()));
    }

    fn stop_sunshine(&mut self) {
        if self.stream.is_some() {
            self.disconnect(None);
        }
        let Some(instance) = self.sun.instance() else { return };
        self.sun = Sun::Stopped;
        self.running_tuning = None;
        self.notice = Some("Sunshine stopped.".into());
        let bridge = self.bridge.clone();
        self.task(move || {
            block_on(sunshine::stop(&bridge, instance));
            Msg::Stopped
        });
    }

    /// Low latency's parts that live here, applied at once: no vsync wait
    /// (a frame shows the moment it is decoded, at the price of tearing),
    /// and the shorter audio queue.
    fn sync_latency(&mut self) {
        let low = self.config.latency == "low";
        stream::set_low_latency(low);
        if self.vsync == low {
            self.vsync = !low;
            unsafe { SDL_SetRenderVSync(self.ui.r, if low { 0 } else { 1 }) };
        }
    }

    /// This computer's cursor over the picture. The protocol carries no
    /// cursor shape -- the host's cursor, hand, text beam, spinner and all, is
    /// drawn into the video -- so this one cannot follow it. "host's" hides it
    /// there; "both" turns it into a small dot, which marks the spot without
    /// the round trip and does not pretend to be a shape the host is not
    /// showing.
    fn sync_cursor(&mut self) {
        let over = !self.captured
            && !self.sidebar
            && !self.over_tab()
            && self.stream.as_ref().is_some_and(|s| s.started && s.over_picture(self.ui.mouse.0, self.ui.mouse.1));
        let want = match (over, self.config.pointer.as_str()) {
            (false, _) => CursorState::Normal,
            (true, "both") => CursorState::Dot,
            (true, _) => CursorState::Hidden,
        };
        if want == self.cursor_state {
            return;
        }
        self.cursor_state = want;
        unsafe {
            match want {
                CursorState::Hidden => {
                    SDL_HideCursor();
                }
                CursorState::Dot => {
                    if self.dot.is_null() {
                        self.dot = dot_cursor(&self.ui.t, self.ui.scale);
                    }
                    SDL_SetCursor(if self.dot.is_null() { SDL_GetDefaultCursor() } else { self.dot });
                    SDL_ShowCursor();
                }
                CursorState::Normal => {
                    SDL_SetCursor(SDL_GetDefaultCursor());
                    SDL_ShowCursor();
                }
            }
        }
    }

    /// Opens or shuts the sidebar. Opening it takes the mouse and keyboard
    /// back from the host, and lets go of anything held there.
    fn set_sidebar(&mut self, open: bool) {
        self.sidebar = open;
        if open {
            self.set_captured(false);
            if let Some(s) = &mut self.stream {
                s.release_all();
            }
        }
        self.edge_since = None;
    }

    fn set_captured(&mut self, on: bool) {
        self.captured = on;
        unsafe { SDL_SetWindowRelativeMouseMode(self.window, on) };
        self.sync_keyboard();
    }

    /// Grabs or releases the whole keyboard, following the setting. While
    /// grabbed, SDL asks the compositor to stop acting on its own shortcuts
    /// for this window (Wayland's keyboard-shortcuts-inhibit; a keyboard grab
    /// on X11), so Super, Alt+Tab and the rest reach the host instead of this
    /// computer. Never while the sidebar is open -- it is a local thing -- and
    /// the Ctrl+Alt+Shift hotkeys are always ours, so there is always a way
    /// back (niri's own Mod+Escape is never inhibited either).
    fn sync_keyboard(&mut self) {
        let streaming = self.stream.as_ref().is_some_and(|s| s.started);
        // Straight from SDL each pass: focus and the pointer change under us
        // (focus-follows-mouse moves focus the moment the pointer leaves).
        let flags = unsafe { SDL_GetWindowFlags(self.window) };
        let focused = flags & SDL_WINDOW_INPUT_FOCUS != 0;
        let pointer_inside = flags & SDL_WINDOW_MOUSE_FOCUS != 0;
        let mode = match self.config.keyboard.as_str() {
            "always" => true,
            "captured" => self.captured,
            _ => false,
        };
        let want = streaming && !self.sidebar && mode && focused && (pointer_inside || self.captured);

        // The keyboards themselves, when this computer may claim them.
        if self.grab.as_ref().is_some_and(|g| !want || g.ended()) {
            self.grab = None;
        }
        let retry = self.grab_denied.is_none_or(|t| t.elapsed() >= Duration::from_secs(3));
        // Linux only: on Windows SDL's keyboard grab below is already a
        // low-level keyboard hook that takes every key a program may take,
        // with no permission to ask for -- so there is no evdev grab to try,
        // nothing is ever denied, and the udev notice never shows.
        if cfg!(target_os = "linux") && want && self.grab.is_none() && retry {
            match Grab::start(self.hotkey_event, self.heartbeat.clone(), self.epoch) {
                Ok(g) => {
                    self.grab = Some(g);
                    self.grab_denied = None;
                }
                Err(GrabError::NoAccess) | Err(GrabError::Unavailable) => self.grab_denied = Some(Instant::now()),
                Err(GrabError::KeyHeld) => {} // next pass, once it is let go
            }
        }

        // Otherwise SDL's own keyboard grab. On Linux that is the
        // compositor's shortcut inhibit: most keys reach the host, but niri
        // still sees its Mod (Alt+drag). On Windows it is the whole answer.
        let inhibit = want && self.grab.is_none();
        if inhibit != self.keyboard_grabbed {
            self.keyboard_grabbed = inhibit;
            unsafe { SDL_SetWindowKeyboardGrab(self.window, inhibit) };
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        unsafe { SDL_SetWindowFullscreen(self.window, self.fullscreen) };
    }

    fn set(&mut self, change: impl FnOnce(&mut StreamConfig)) {
        change(&mut self.config);
        config::save(&self.host, &self.config);
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Probed(r) => {
                self.probe = match r {
                    Ok(f) => Probe::Done(f),
                    Err(e) => Probe::Failed(e),
                }
            }
            Msg::Started(Ok((instance, displays))) => {
                self.displays = displays;
                self.pair(instance);
            }
            Msg::Started(Err(e)) => {
                self.frozen = None;
                self.sun = Sun::Failed(e);
            }
            Msg::Paired(instance, Ok(paired)) => {
                // Straight on to the desktop: Connect means "show me the
                // desktop", not "pair and wait".
                self.sun = Sun::Paired(instance, Box::new(paired));
                self.launch();
            }
            Msg::Paired(instance, Err(e)) => {
                // Stop it so the next try starts clean (`-0` reads the PIN once).
                self.frozen = None;
                self.sun = Sun::Failed(format!("pairing failed: {e}"));
                let bridge = self.bridge.clone();
                self.task(move || {
                    block_on(sunshine::stop(&bridge, instance));
                    Msg::Stopped
                });
            }
            Msg::Launched(r) => {
                self.launching = false;
                match r {
                    Ok(session) => {
                        // A rejoin keeps the sidebar where it was.
                        if self.frozen.is_none() {
                            self.sidebar = false;
                            self.side_scroll = 0.0;
                        }
                        self.notice = None;
                        self.stream = Some(Stream::start(session));
                    }
                    Err(e) => {
                        self.frozen = None;
                        self.notice = Some(format!("Could not start the stream: {e}"));
                    }
                }
            }
            Msg::Stopped => {
                if std::mem::take(&mut self.restart) {
                    self.start_sunshine();
                }
            }
        }
    }

    // --- the loop ------------------------------------------------------------

    pub fn run(&mut self) {
        let mut dirty = true;
        let mut drawn = Instant::now();
        'run: loop {
            let mut event: SDL_Event = unsafe { std::mem::zeroed() };
            let timeout = if self.stream.is_some() || self.frozen.is_some() || self.apply_at.is_some() { 16 } else { 250 };
            let mut pending = Vec::new();
            if unsafe { SDL_WaitEventTimeout(&mut event, timeout) } {
                pending.push(event);
                let mut more: SDL_Event = unsafe { std::mem::zeroed() };
                while unsafe { SDL_PollEvent(&mut more) } {
                    pending.push(more);
                }
            }
            self.ui.click = false;
            self.ui.wheel = 0.0;
            for ev in &pending {
                // While streaming, mouse motion over the picture redraws
                // nothing: with vsync every draw waits for a screen refresh,
                // and drawing on each motion spent refreshes on unchanged
                // pictures while the next video frame overwrote the last --
                // 42-55 of 60 frames shown while the mouse moved. The edge
                // arrow is caught by the redraw at least every 100 ms.
                let motion = unsafe { ev.r#type } == SDL_EVENT_MOUSE_MOTION.0;
                if !(motion && self.stream.is_some() && !self.sidebar) {
                    dirty = true;
                }
                if !self.event(ev) {
                    break 'run;
                }
            }
            // The pass's mouse motion, as one update.
            if let Some(s) = &mut self.stream {
                s.flush_motion();
            }
            while let Ok(msg) = self.rx.try_recv() {
                self.handle(msg);
                dirty = true;
            }
            if let Some(events) = &mut self.events {
                while let Ok(e) = events.try_recv() {
                    match e {
                        Event::Raise => unsafe {
                            SDL_RaiseWindow(self.window);
                        },
                        Event::Closed => break 'run,
                    }
                }
            }
            // Cheap, and the inputs (stream started, sidebar, capture, the
            // setting) change from many places: settle it once a pass.
            self.heartbeat.store(self.epoch.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
            self.sync_keyboard();
            self.sync_cursor();
            self.sync_latency();
            if self.apply_at.is_some_and(|at| Instant::now() >= at) {
                self.apply_at = None;
                self.rejoin();
            }
            let due = drawn.elapsed() >= Duration::from_millis(100);
            let streaming = self.stream.is_some() && (crate::stream::frame_waiting() || due);
            if streaming || self.frozen.is_some() || dirty {
                self.frame();
                dirty = false;
                drawn = Instant::now();
            }
        }
        // Leaving: end the stream; PulseTerm closing our pipe closes the
        // spawned channel, and the host's wrapper stops Sunshine.
        self.stream = None;
    }

    /// One event; false to quit.
    fn event(&mut self, ev: &SDL_Event) -> bool {
        let kind = unsafe { ev.r#type };
        let ui_scale = self.ui.scale;
        unsafe {
            if kind == SDL_EVENT_QUIT.0 || kind == SDL_EVENT_WINDOW_CLOSE_REQUESTED.0 {
                return false;
            }
            // The Ctrl+Alt+Shift hotkeys, read by the keyboard grab's thread.
            if kind == self.hotkey_event {
                match ev.user.code {
                    keygrab::HOTKEY_QUIT => return false,
                    keygrab::HOTKEY_CAPTURE => self.set_captured(!self.captured),
                    keygrab::HOTKEY_SIDEBAR => self.set_sidebar(!self.sidebar),
                    keygrab::HOTKEY_FULLSCREEN => self.toggle_fullscreen(),
                    _ => {}
                }
                return true;
            }
            if kind == stream::CONNECTION.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(stream) = &mut self.stream {
                    if let Some(why) = stream.on_connection(&ev.user) {
                        self.disconnect(Some(why));
                    }
                }
                return true;
            }
            let streaming = self.stream.as_ref().map(|s| s.started).unwrap_or(false);
            // The host gets the pointer when the sidebar is shut and the
            // pointer is not on the arrow -- always, while captured.
            let to_host = streaming && (self.captured || (!self.sidebar && !self.over_tab()));
            if kind == SDL_EVENT_MOUSE_MOTION.0 {
                let m = ev.motion;
                self.ui.mouse = (m.x, m.y);
                // Resting on the right edge reveals the arrow; leaving both
                // the edge and the arrow hides it again.
                if !self.captured && !self.sidebar {
                    let at_edge = m.x >= self.ui.width - 4.0;
                    if at_edge {
                        self.edge_since.get_or_insert_with(Instant::now);
                    } else if !self.over_tab() {
                        self.edge_since = None;
                    }
                }
                let to_host = streaming && (self.captured || (!self.sidebar && !self.over_tab()));
                if to_host {
                    if let Some(s) = &mut self.stream {
                        s.motion(&m, self.captured);
                    }
                }
            } else if kind == SDL_EVENT_MOUSE_BUTTON_DOWN.0 || kind == SDL_EVENT_MOUSE_BUTTON_UP.0 {
                let b = ev.button;
                let over_picture = self.stream.as_ref().is_some_and(|s| s.over_picture(b.x, b.y));
                if to_host && b.down && !self.captured && self.config.mouse == "game" && over_picture {
                    // Game mouse: the first click takes the pointer (and, by
                    // default, the keyboard) rather than reaching the host.
                    self.set_captured(true);
                } else if to_host {
                    if let Some(s) = &mut self.stream {
                        s.button(&b);
                    }
                } else if !b.down && b.button == 1 {
                    self.ui.click = true;
                }
            } else if kind == SDL_EVENT_MOUSE_WHEEL.0 {
                let w = ev.wheel;
                if to_host {
                    let reversed = self.config.scroll == "reversed";
                    if let Some(s) = &mut self.stream {
                        s.wheel(&w, reversed);
                    }
                } else {
                    self.ui.wheel += w.y;
                }
            } else if kind == SDL_EVENT_WINDOW_FOCUS_LOST.0 {
                // Whatever is held now will come up where this window cannot
                // see it: let go of it on the host.
                if let Some(s) = &mut self.stream {
                    s.release_all();
                }
            } else if kind == SDL_EVENT_KEY_DOWN.0 || kind == SDL_EVENT_KEY_UP.0 {
                let key = ev.key;
                let m = key.r#mod.0;
                let chord = m & SDL_KMOD_CTRL.0 != 0 && m & SDL_KMOD_ALT.0 != 0 && m & SDL_KMOD_SHIFT.0 != 0;
                if key.down && chord {
                    match key.scancode.0 {
                        20 => return false, // Q
                        29 if self.stream.is_some() => {
                            self.set_captured(!self.captured); // Z
                            return true;
                        }
                        27 => {
                            self.toggle_fullscreen(); // X
                            return true;
                        }
                        22 if self.stream.is_some() => {
                            self.set_sidebar(!self.sidebar); // S: the arrow, from the keyboard
                            return true;
                        }
                        _ => {}
                    }
                }
                // Escape shuts the sidebar; with it open no key reaches the host.
                if self.sidebar {
                    if key.down && key.scancode.0 == 41 {
                        self.set_sidebar(false);
                    }
                } else if let Some(s) = &mut self.stream {
                    s.key(&key);
                }
            } else if kind == SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED.0 || kind == SDL_EVENT_WINDOW_RESIZED.0 {
                self.measure();
                self.follow_window();
            }
        }
        let _ = ui_scale;
        true
    }

    /// The window's size in real pixels (HiDPI included).
    fn window_pixels(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        unsafe { SDL_GetWindowSizeInPixels(self.window, &mut w, &mut h) };
        (w.max(1) as u32, h.max(1) as u32)
    }

    /// Resolution "window": a resized (or fullscreened) window rejoins the
    /// host at its new size once the resizing settles -- each resize event
    /// pushes the rejoin back (`stream_setting_changed`), so a drag rejoins
    /// once, at the end.
    fn follow_window(&mut self) {
        if !matches!(self.config.resolution.as_str(), "window" | "native") {
            return;
        }
        let Some(s) = &self.stream else { return };
        let (w, h) = window_resolution(self.window_pixels());
        if (s.session.width, s.session.height) != (w as i32, h as i32) {
            self.stream_setting_changed();
        }
    }

    /// Window size and pixel density, read from SDL.
    pub fn measure(&mut self) {
        unsafe {
            let (mut w, mut h, mut pw) = (0, 0, 0);
            SDL_GetWindowSize(self.window, &mut w, &mut h);
            let mut ph = 0;
            SDL_GetWindowSizeInPixels(self.window, &mut pw, &mut ph);
            self.ui.width = w.max(1) as f32;
            self.ui.height = h.max(1) as f32;
            self.ui.scale = (pw as f32 / w.max(1) as f32).max(1.0);
        }
    }

    fn frame(&mut self) {
        let t = self.ui.t.clone();
        // No caption unless a bar below claims one this frame: with the
        // sidebar shut, the stream picture is the host's, every pixel of it.
        crate::chrome::set_caption(None, (0.0, 0.0, 0.0, 0.0));
        unsafe {
            SDL_SetRenderDrawBlendMode(self.ui.r, SDL_BLENDMODE_BLEND);
            SDL_SetRenderDrawColor(self.ui.r, t.bg.0, t.bg.1, t.bg.2, 255);
            SDL_RenderClear(self.ui.r);
        }
        if self.stream.is_some() || self.frozen.is_some() {
            self.draw_stream();
        } else {
            self.draw_setup();
        }
        unsafe { SDL_RenderPresent(self.ui.r) };
    }

    // --- the stream screen ---------------------------------------------------

    const TAB: (f32, f32) = (22.0, 72.0);
    const SIDEBAR: f32 = 380.0;

    fn tab_rect(&self) -> (f32, f32, f32, f32) {
        let x = if self.sidebar { self.ui.width - Self::SIDEBAR - Self::TAB.0 } else { self.ui.width - Self::TAB.0 };
        (x, (self.ui.height - Self::TAB.1) / 2.0, Self::TAB.0, Self::TAB.1)
    }

    /// The arrow shows while the sidebar is open, and after the pointer has
    /// rested a moment on the right edge -- not otherwise, so the host gets the
    /// whole picture.
    fn tab_visible(&self) -> bool {
        self.sidebar || self.edge_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(300))
    }

    fn over_tab(&self) -> bool {
        let (x, y, w, h) = self.tab_rect();
        self.tab_visible() && self.ui.hover(x, y, w, h)
    }

    fn draw_stream(&mut self) {
        let t = self.ui.t.clone();
        // The old picture holds the window until the new stream draws one.
        let mut new_picture = false;
        let mut started = false;
        if let Some(stream) = self.stream.as_mut() {
            // Draws nothing until its first picture; the frozen one shows then.
            stream.draw(&self.ui);
            new_picture = stream.has_picture();
            started = stream.started;
        }
        if new_picture {
            self.frozen = None;
        }
        if let Some(f) = &self.frozen {
            f.draw(&self.ui);
            // What is happening, small, over the held picture.
            let msg = self.notice.clone().unwrap_or_else(|| "applying settings…".into());
            let w = self.ui.micro_width(&msg) + 28.0;
            let x = (self.ui.width - w) / 2.0;
            self.ui.fill(x, 16.0, w, t.row(), t.surface, 230);
            self.ui.outline(x, 16.0, w, t.row(), t.border);
            self.ui.micro(x + 14.0, 16.0 + (t.row() - self.ui.line(t.micro())) / 2.0, &msg, t.warning);
        } else if !started {
            let msg = "connecting…";
            let w = self.ui.measure(msg, t.font_size, false);
            self.ui.text((self.ui.width - w) / 2.0, self.ui.height / 2.0 - 10.0, msg, t.font_size, false, t.fg_muted);
        }
        if self.stream.is_none() {
            return; // between streams: nothing to steer yet
        }
        if self.captured || !self.tab_visible() {
            return; // nothing over the host's picture
        }
        // The arrow: translucent, on the right edge; it opens the sidebar.
        let (x, y, w, h) = self.tab_rect();
        let hover = self.ui.hover(x, y, w, h);
        self.ui.fill(x, y, w, h, t.surface, if hover { 230 } else { 120 });
        self.ui.vline(x, y, h, t.border);
        self.ui.hline(x, y, w, t.border);
        self.ui.hline(x, y + h - 1.0, w, t.border);
        let glyph = if self.sidebar { "›" } else { "‹" };
        let gw = self.ui.measure(glyph, t.head(), true);
        let colour = if hover { t.fg } else { t.fg_muted };
        self.ui.text(x + (w - gw) / 2.0, y + (h - self.ui.line(t.head())) / 2.0, glyph, t.head(), true, colour);
        if self.ui.clicked(x, y, w, h) {
            self.set_sidebar(!self.sidebar);
            self.ui.click = false;
        }
        if self.sidebar {
            self.draw_sidebar();
        }
    }

    fn draw_sidebar(&mut self) {
        let t = self.ui.t.clone();
        let (sw, wh) = (Self::SIDEBAR, self.ui.height);
        let x0 = self.ui.width - sw;
        // A click outside the sidebar and its arrow shuts it.
        if self.ui.click && !self.ui.hover(x0, 0.0, sw, wh) && !self.over_tab() {
            self.set_sidebar(false);
            self.ui.click = false;
            return;
        }
        self.ui.fill(x0, 0.0, sw, wh, t.surface, 240);
        self.ui.vline(x0, 0.0, wh, t.border);

        // Header, like the setup screen's bar.
        let bar = t.bar();
        let ty = (bar - self.ui.line(t.micro())) / 2.0;
        let mut x = x0 + 16.0;
        x += self.ui.micro(x, ty, "remote", t.fg_muted) + 12.0;
        let started = self.stream.as_ref().map(|s| s.started).unwrap_or(false);
        let (word, tint) = if started { ("streaming", t.accent) } else { ("connecting", t.warning) };
        self.ui.micro(x, ty, word, tint);
        // While streaming this header is the only bar on screen, so on Windows
        // it is the caption and carries the window's buttons.
        let buttons = crate::chrome::buttons(&mut self.ui, self.window, x0 + sw, bar);
        crate::chrome::set_caption(Some((x0, 0.0, sw, bar)), (buttons, 0.0, x0 + sw - buttons, bar));
        self.ui.hline(x0, bar, sw, t.border);

        let pad = 20.0;
        let inner = sw - pad * 2.0;
        if self.ui.hover(x0, bar, sw, wh - bar) {
            self.side_scroll = (self.side_scroll - self.ui.wheel * 40.0).max(0.0);
        }
        self.ui.clip(Some((x0, bar + 1.0, sw, wh - bar - 1.0)));
        let mut y = bar + 18.0 - self.side_scroll;
        let start_y = y;

        // The stream.
        let host = self.host.clone();
        self.ui.heading(x0 + pad, y, inner, "Stream", None);
        y += self.ui.line(t.head()) + 10.0;
        self.ui.text(x0 + pad, y, &host, t.font_size, false, t.fg);
        y += t.line_height + 4.0;
        if let Some(s) = &self.stream {
            let ses = &s.session;
            let codec = if ses.formats & stream::ffi::VIDEO_FORMAT_MASK_H265 as i32 != 0 { "HEVC" } else { "H.264" };
            // The setting is a cap, and 20% of it is set aside for error
            // correction (moonlight-common-c sends 80% as the video's limit);
            // the encoder spends only what the picture needs under that.
            let cap = ses.bitrate as f32 * 0.8 / 1000.0;
            let asked = format!(
                "{}x{} · {} fps · {} Mbps ({cap:.0} for the picture) · {codec} or H.264",
                ses.width,
                ses.height,
                ses.fps,
                ses.bitrate / 1000
            );
            y += self.ui.paragraph(x0 + pad, y, inner, &asked, t.fg_muted);
            let (decoded, shown) = s.fps;
            let reading = format!("{decoded:.0} fps · {shown:.0} shown · {:.1} Mbps in", s.received_mbps);
            y += 4.0;
            self.ui.micro(x0 + pad, y, &reading, if decoded > 0.0 { t.success } else { t.fg_subtle });
            y += self.ui.line(t.micro()) + 6.0;
            // Where the delay is: the network's round trip, the host's own
            // work per frame, and decoding here.
            let (net, host, decode) = s.latency;
            let host = if host > 0.0 { format!("{host:.1} ms") } else { "--".into() };
            let lat = format!("control {net:.0} ms · host {host} · decode {decode:.1} ms");
            self.ui.text(x0 + pad, y, &lat, t.micro(), false, t.fg_muted);
            y += self.ui.line(t.micro()) + 6.0;
            // Audio: what the queue is doing, and the longest wait between two
            // packets (one should come every 5 ms) since the last reading.
            if self.audio_tick.elapsed() >= Duration::from_secs(1) {
                self.audio_reading = stream::audio_readings();
                self.audio_tick = Instant::now();
            }
            let (q, cushion, gaps, drops, late) = self.audio_reading;
            let aud = format!("audio {q}/{cushion} ms · gaps {gaps} · drops {drops} · late {late:.0} ms");
            self.ui.text(x0 + pad, y, &aud, t.micro(), false, if late > 40.0 || gaps > 0 { t.warning } else { t.fg_muted });
            y += self.ui.line(t.micro()) + 18.0;
        }
        self.ui.hline(x0 + pad, y, inner, t.border);
        y += 18.0;

        // Controls.
        self.ui.heading(x0 + pad, y, inner, "Controls", None);
        y += self.ui.line(t.head()) + 10.0;
        let rows: [(&str, &str, u8); 4] = [
            (
                match (self.captured, self.config.keyboard == "captured") {
                    (true, true) => "release mouse + keyboard",
                    (false, true) => "capture mouse + keyboard",
                    (true, false) => "release mouse",
                    (false, false) => "capture mouse",
                },
                "Ctrl+Alt+Shift+Z",
                0,
            ),
            (if self.fullscreen { "leave fullscreen" } else { "fullscreen" }, "Ctrl+Alt+Shift+X", 1),
            ("disconnect", "", 2),
            ("stop sunshine", "", 3),
        ];
        for (text, keys, action) in rows {
            if self.ui.row_button(x0 + pad, y, inner, text, keys) {
                match action {
                    0 => self.set_captured(!self.captured),
                    1 => self.toggle_fullscreen(),
                    2 => {
                        self.ui.clip(None);
                        self.disconnect(None);
                        return;
                    }
                    _ => {
                        self.ui.clip(None);
                        self.stop_sunshine();
                        return;
                    }
                }
            }
            y += t.row() + 8.0;
        }
        y += 10.0;
        self.ui.hline(x0 + pad, y, inner, t.border);
        y += 18.0;

        // Settings: the same as the setup screen's, applied as they change.
        self.ui.heading(x0 + pad, y, inner, "Settings", None);
        y += self.ui.line(t.head()) + 6.0;
        let note = "Audio switches at once; picture settings rejoin the host in a second or so.";
        y += self.ui.paragraph(x0 + pad, y, inner, note, t.fg_subtle) + 10.0;
        y = self.settings(x0 + pad, y);
        y += 10.0;
        self.ui.clip(None);
        // `y` and `start_y` are both already shifted by the scroll, so their
        // difference is the content's height as it is. Adding the scroll on
        // top made every scroll grow the limit -- an endless sidebar.
        let content = y - start_y;
        self.side_scroll = self.side_scroll.min((content - (wh - bar) + 30.0).max(0.0));
    }

    /// The stream settings as rows of segmented choices; returns the y after.
    fn settings(&mut self, x: f32, mut y: f32) -> f32 {
        let t = self.ui.t.clone();
        let c = self.config.clone();
        let row = |app: &mut Self, label: &str, y: &mut f32, options: &[(&'static str, &str)], current: &str| {
            app.ui.text(x, *y, label, t.font_size, false, t.fg_muted);
            *y += t.line_height + 6.0;
            let picked = app.ui.segmented(x, *y, options, current);
            *y += t.row() + 14.0;
            picked
        };
        if let Some(v) = row(self, "Resolution", &mut y, &[("window", "window"), ("720", "720"), ("1080", "1080"), ("1440", "1440"), ("2160", "4k")], &c.resolution) {
            self.set(|c| c.resolution = v.into());
            self.stream_setting_changed();
        }
        if let Some(v) = row(self, "Frame rate", &mut y, &[("30", "30"), ("60", "60"), ("90", "90"), ("120", "120")], &c.fps.to_string()) {
            self.set(|c| c.fps = v.parse().unwrap_or(60));
            self.stream_setting_changed();
        }
        if let Some(v) = row(self, "Codec", &mut y, &[("auto", "auto"), ("h264", "h.264"), ("hevc", "hevc"), ("av1", "av1")], &c.codec) {
            self.set(|c| c.codec = v.into());
            self.stream_setting_changed();
        }
        if let Some(v) = row(self, "Bitrate, Mbps", &mut y, &[("0", "auto"), ("20", "20"), ("50", "50"), ("100", "100"), ("150", "150")], &c.bitrate.to_string()) {
            self.set(|c| c.bitrate = v.parse().unwrap_or(0));
            self.stream_setting_changed();
        }
        if let Some(v) = row(self, "Audio", &mut y, &[("here", "this pc"), ("host", "host"), ("both", "both")], &c.sound) {
            self.set(|c| {
                c.sound = v.into();
                c.audio = v != "host";
            });
            // Here, at once; whether the host keeps playing is part of the
            // launch, so the stream rejoins for that half.
            stream::set_muted(v == "host");
            self.stream_setting_changed();
        }
        // No gamepads switch: gamepads are not planned (REMOTE_DESKTOP.md,
        // "Not planned"), and a switch that does nothing is worse than none.
        if let Some(v) = row(self, "Full keyboard", &mut y, &[("always", "over window"), ("captured", "with mouse"), ("off", "off")], &c.keyboard) {
            self.set(|c| c.keyboard = v.into());
            self.sync_keyboard();
        }
        let keys = "Over window: while this window is focused and the pointer is on it, every key goes to the host and this computer reacts to none. Move the pointer off and it is yours again. Ctrl+Alt+Shift+Z/S/Q stay here.";
        y += self.ui.paragraph(x, y - 8.0, 330.0, keys, t.fg_subtle) + 6.0;
        if self.grab_denied.is_some() && self.grab.is_none() {
            // Claiming the keyboards needs a one-time permission; until then
            // niri still sees its own Mod (Alt+drag).
            let need = "For this computer to ignore every key (niri's Alt+drag too), it needs access to the keyboards -- once, on this computer:";
            y += self.ui.paragraph(x, y, 330.0, need, t.warning) + 8.0;
            if self.ui.button(x, y, "copy the permission command", false, true).1 {
                if let Ok(cmd) = std::ffi::CString::new(keygrab::GRANT) {
                    unsafe { SDL_SetClipboardText(cmd.as_ptr()) };
                }
                self.notice = Some("Copied. Run it in a terminal on this computer.".into());
            }
            y += t.row() + 14.0;
        }
        if let Some(v) = row(self, "Mouse", &mut y, &[("desktop", "desktop"), ("game", "game")], &c.mouse) {
            self.set(|c| c.mouse = v.into());
        }
        if let Some(v) = row(self, "Pointer", &mut y, &[("host", "host's"), ("both", "both")], &c.pointer) {
            self.set(|c| c.pointer = v.into());
        }
        if let Some(v) = row(self, "Scroll", &mut y, &[("here", "as here"), ("reversed", "reversed")], &c.scroll) {
            self.set(|c| c.scroll = v.into());
        }
        if let Some(v) = row(self, "Latency", &mut y, &[("normal", "normal"), ("low", "low")], &c.latency) {
            self.set(|c| c.latency = v.into());
            self.sync_latency();
        }
        let lat = "Low: frames show the moment they are decoded (the picture may tear), a shorter audio buffer, and the host's encoder in its low-latency mode -- that part applies when Sunshine restarts.";
        y += self.ui.paragraph(x, y - 8.0, 330.0, lat, t.fg_subtle) + 6.0;
        let mouse = "Desktop: the pointer moves freely and the host follows. Game: a click in the picture captures it for relative motion. Pointer: the host's cursor (in the picture) shows hand, text and busy shapes; \"both\" adds a small dot here that moves without network delay.";
        y += self.ui.paragraph(x, y - 8.0, 330.0, mouse, t.fg_subtle) + 6.0;

        // What the host spends on the stream. Written into its Sunshine's
        // config at start; Connect restarts Sunshine when one changed.
        y += 6.0;
        self.ui.micro(x, y, "host cpu", t.fg_subtle);
        y += self.ui.line(t.micro()) + 6.0;
        let note = "Without a hardware encoder the host encodes on its CPU. 720p, 30 fps, H.264 and the fastest encoder save the most; audio off stops it capturing sound.";
        y += self.ui.paragraph(x, y, 330.0, note, t.fg_subtle) + 12.0;
        // Which display: offered once the host's Sunshine has listed them,
        // and only when there is a choice to make.
        if self.displays.len() > 1 {
            const SLOTS: [&str; 8] = ["0", "1", "2", "3", "4", "5", "6", "7"];
            let mut options: Vec<(&'static str, &str)> = vec![("auto", "auto")];
            for (i, d) in self.displays.iter().take(SLOTS.len()).enumerate() {
                options.push((SLOTS[i], d.name.as_str()));
            }
            let current = self.displays.iter().position(|d| d.name == c.display).map(|i| SLOTS[i]).unwrap_or("auto");
            let options: Vec<(&'static str, String)> = options.into_iter().map(|(v, t)| (v, t.to_string())).collect();
            let borrowed: Vec<(&'static str, &str)> = options.iter().map(|(v, t)| (*v, t.as_str())).collect();
            if let Some(v) = row(self, "Display", &mut y, &borrowed, current) {
                let name = v.parse::<usize>().ok().and_then(|i| self.displays.get(i)).map(|d| d.name.clone()).unwrap_or_default();
                self.set(|c| c.display = name);
            }
            let models: Vec<String> = self
                .displays
                .iter()
                .map(|d| if d.model.is_empty() { d.name.clone() } else { format!("{}: {}", d.name, d.model) })
                .collect();
            let note = format!("{}. Auto lets Sunshine pick.", models.join(" · "));
            y += self.ui.paragraph(x, y - 8.0, 330.0, &note, t.fg_subtle) + 6.0;
        }
        if let Some(v) = row(self, "Encoder speed", &mut y, &[("ultrafast", "fastest"), ("superfast", "fast"), ("veryfast", "better")], &c.sw_preset) {
            self.set(|c| c.sw_preset = v.into());
        }
        if let Some(v) = row(self, "Encoder threads", &mut y, &[("1", "1"), ("2", "2"), ("4", "4"), ("8", "8")], &c.threads.to_string()) {
            self.set(|c| c.threads = v.parse().unwrap_or(2));
        }
        if let Some(v) = row(self, "Error correction", &mut y, &[("5", "5%"), ("10", "10%"), ("20", "20%")], &c.fec.to_string()) {
            self.set(|c| c.fec = v.parse().unwrap_or(20));
        }
        let (_, lowest) = self.ui.button(x, y, "lowest cpu", false, true);
        // Mid-stream, host settings wait for this: Sunshine has to restart.
        // On a row of its own -- beside "lowest cpu" it ran off the sidebar.
        if self.stream.is_some() && self.host_pending() {
            y += t.row() + 10.0;
            if self.ui.button(x, y, "apply · restarts sunshine", true, true).1 {
                self.apply_host();
            }
        }
        if lowest {
            self.set(|c| {
                c.resolution = "720".into();
                c.fps = 30;
                c.codec = "h264".into();
                c.bitrate = 0;
                c.audio = false;
                c.sound = "host".into();
                c.sw_preset = "ultrafast".into();
                c.threads = 2;
                c.fec = 10;
            });
            self.notice = Some("Set for the lowest host CPU: 720p, 30 fps, H.264, fastest encoder, no audio.".into());
            stream::set_muted(true);
            self.stream_setting_changed();
        }
        y + t.row() + 14.0
    }

    // --- the setup screen ----------------------------------------------------

    fn draw_setup(&mut self) {
        let t = self.ui.t.clone();
        let (w, h) = (self.ui.width, self.ui.height);
        let bar = t.bar();
        let checks = match &self.probe {
            Probe::Done(f) => probe::checks(f),
            _ => Vec::new(),
        };
        let ready = matches!(self.probe, Probe::Done(_)) && probe::ready(&checks);
        let (word, tint) = match &self.probe {
            Probe::Checking => ("checking", t.warning),
            Probe::Failed(_) => ("offline", t.danger),
            Probe::Done(_) if ready => ("ready", t.success),
            Probe::Done(_) => ("not ready", t.danger),
        };

        // The bar.
        self.ui.fill(0.0, 0.0, w, bar, t.surface, 255);
        self.ui.hline(0.0, bar - 1.0, w, t.border);
        let ty = (bar - self.ui.line(t.micro())) / 2.0;
        let mut x = 16.0;
        x += self.ui.micro(x, ty, "remote", t.fg_muted) + 16.0;
        self.ui.vline(x, bar * 0.2, bar * 0.6, t.border);
        x += 16.0;
        let host = self.host.clone();
        x += self.ui.text(x, (bar - t.line_height) / 2.0, &host, t.font_size, false, t.fg) + 16.0;
        self.ui.micro(x, ty, word, tint);
        // The bar is the window's caption on Windows, with its buttons at the
        // right end (`chrome`); elsewhere `buttons` takes no room.
        let buttons = crate::chrome::buttons(&mut self.ui, self.window, w, bar);
        crate::chrome::set_caption(Some((0.0, 0.0, w, bar)), (buttons, 0.0, w - buttons, bar));
        // An address is not a label: small, but not spaced out in capitals.
        let address = self.address.clone();
        let aw = self.ui.measure(&address, t.micro(), false);
        self.ui.text(buttons - 16.0 - aw, ty, &address, t.micro(), false, t.fg_subtle);

        // The footer.
        let footer = t.bar();
        let fy = h - footer;
        self.ui.fill(0.0, fy, w, footer, t.bg, 255);
        self.ui.hline(0.0, fy, w, t.border);
        let by = fy + (footer - t.row()) / 2.0;
        let cw = self.ui.micro_width("connect") + 28.0;
        let rw = self.ui.micro_width("recheck") + 28.0;
        let busy = self.launching || matches!(self.sun, Sun::Starting | Sun::Pairing(_));
        let (_, connect) = self.ui.button(w - 20.0 - cw, by, "connect", true, ready && !busy);
        let (_, recheck) = self.ui.button(w - 20.0 - cw - 12.0 - rw, by, "recheck", false, !busy);
        let notice = self.notice.clone().unwrap_or_else(|| match &self.sun {
            Sun::Starting => "Starting Sunshine on the host…".into(),
            Sun::Pairing(_) => "Pairing over SSH -- nothing to type.".into(),
            Sun::Failed(e) => e.clone(),
            _ => String::new(),
        });
        let room = self.ui.columns(w - 60.0 - cw - rw, t.font_size);
        let shown: String = notice.chars().take(room).collect();
        self.ui.text(20.0, fy + (footer - t.line_height) / 2.0, &shown, t.font_size, false, t.fg_muted);

        // The body: host and checks on the left (scrolls), settings right.
        let right = 400.0f32.min(w * 0.45);
        let left = w - right;
        let (top, body) = (bar, fy - bar);
        self.ui.vline(left, top, body, t.border);
        self.settings_panel(left, top, right, body);

        if self.ui.hover(0.0, top, left, body) {
            self.scroll = (self.scroll - self.ui.wheel * 40.0).max(0.0);
        }
        self.ui.clip(Some((0.0, top, left, body)));
        let content = self.host_panel(0.0, top - self.scroll, left, &checks);
        self.ui.clip(None);
        self.scroll = self.scroll.min((content - body + 20.0).max(0.0));

        if connect {
            self.connect();
        }
        if recheck {
            self.recheck();
        }
    }

    fn settings_panel(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let t = self.ui.t.clone();
        if self.ui.hover(x, y, w, h) {
            self.settings_scroll = (self.settings_scroll - self.ui.wheel * 40.0).max(0.0);
        }
        self.ui.clip(Some((x, y, w, h)));
        let pad = 20.0;
        let top = y + 18.0 - self.settings_scroll;
        self.ui.heading(x + pad, top, w - pad * 2.0, "Stream", None);
        let end = self.settings(x + pad, top + self.ui.line(t.head()) + 14.0);
        self.ui.clip(None);
        let content = end - top + 18.0;
        self.settings_scroll = self.settings_scroll.min((content - h).max(0.0));
    }

    /// The host's facts, the checks and PulseTerm's Sunshine; returns the
    /// content's height.
    fn host_panel(&mut self, x: f32, y0: f32, w: f32, checks: &[probe::Check]) -> f32 {
        let t = self.ui.t.clone();
        let pad = 20.0;
        let inner = w - pad * 2.0;
        let mut y = y0 + 18.0;
        self.ui.heading(x + pad, y, inner, "Host", None);
        y += self.ui.line(t.head()) + 12.0;
        let facts = match &self.probe {
            Probe::Checking => {
                self.ui.text(x + pad, y, "asking the host over SSH…", t.font_size, false, t.fg_muted);
                return y - y0 + 40.0;
            }
            Probe::Failed(e) => {
                let e = format!("could not ask the host: {e}");
                return y - y0 + self.ui.paragraph(x + pad, y, inner, &e, t.danger) + 20.0;
            }
            Probe::Done(f) => f.clone(),
        };
        let gpu = match (&facts.nvidia, facts.gpus.is_empty()) {
            (Some(n), _) => n.clone(),
            (None, false) => facts.gpus.join(" · "),
            (None, true) => "unknown".into(),
        };
        for (key, value) in [("system", facts.distro.as_str()), ("name", facts.hostname.as_str()), ("gpu", gpu.as_str())] {
            let ky = y + (t.line_height - self.ui.line(t.micro())) / 2.0;
            self.ui.micro(x + pad, ky, key, t.fg_subtle);
            let value = if value.is_empty() { "unknown" } else { value };
            y += self.ui.paragraph(x + pad + 80.0, y, inner - 80.0, value, t.fg) + 8.0;
        }
        y += 8.0;
        self.ui.hline(x + pad, y, inner, t.border);
        y += 18.0;

        let count = checks.len().to_string();
        self.ui.heading(x + pad, y, inner, "Checks", Some(&count));
        y += self.ui.line(t.head()) + 12.0;
        let fix_w = 230.0f32.min(inner * 0.45);
        for check in checks {
            let tint = match check.state {
                State::Ok => t.success,
                State::Warn => t.warning,
                State::Fail => t.danger,
            };
            self.ui.text(x + pad, y, "■", t.font_size, false, tint);
            let text_w = if check.fix.is_some() { inner - 22.0 - fix_w - 12.0 } else { inner - 22.0 };
            self.ui.text(x + pad + 22.0, y, check.label, t.font_size, false, t.fg);
            let dh = self.ui.paragraph(x + pad + 22.0, y + t.line_height + 2.0, text_w, &check.detail, t.fg_muted);
            if let Some(fix) = &check.fix {
                // Click to copy: the fix is run on the host, in a terminal
                // PulseTerm already has open there.
                let (bx, bw, bh) = (x + pad + inner - fix_w, fix_w, t.row());
                let hover = self.ui.hover(bx, y, bw, bh);
                self.ui.outline(bx, y, bw, bh, if hover { t.accent } else { t.border });
                let cols = self.ui.columns(bw - 16.0, t.micro());
                let shown: String = format!("$ {fix}").chars().take(cols).collect();
                self.ui.text(bx + 8.0, y + (bh - self.ui.line(t.micro())) / 2.0, &shown, t.micro(), false, t.fg_muted);
                if self.ui.clicked(bx, y, bw, bh) {
                    if let Ok(c) = std::ffi::CString::new(fix.as_str()) {
                        unsafe { SDL_SetClipboardText(c.as_ptr()) };
                    }
                    self.notice = Some("Copied. Run it in a terminal on the host.".into());
                }
            }
            y += t.line_height + 2.0 + dh + 14.0;
        }
        y += 4.0;
        self.ui.hline(x + pad, y, inner, t.border);
        y += 18.0;

        // PulseTerm's own Sunshine.
        self.ui.heading(x + pad, y, inner, "Sunshine", None);
        y += self.ui.line(t.head()) + 12.0;
        let (word, tint, detail): (&str, Rgb, String) = match &self.sun {
            Sun::Stopped => ("stopped", t.fg_subtle, "starts with connect, stops when this window closes".into()),
            Sun::Starting => ("starting", t.warning, "the first time on a Wayland host it may ask to allow screen sharing -- accept it on the host's screen".into()),
            Sun::Pairing(_) => ("pairing", t.warning, "pairing over SSH -- nothing to type".into()),
            Sun::Paired(i, _) => ("paired", t.success, format!("base port {}, paired for this launch only -- nothing saved", i.port)),
            Sun::Failed(e) => ("failed", t.danger, e.clone()),
        };
        self.ui.text(x + pad, y, "■", t.font_size, false, tint);
        let lw = self.ui.text(x + pad + 22.0, y, "PulseTerm's instance", t.font_size, false, t.fg);
        let my = y + (t.line_height - self.ui.line(t.micro())) / 2.0;
        self.ui.micro(x + pad + 22.0 + lw + 12.0, my, word, tint);
        let action = match &self.sun {
            Sun::Paired(..) => Some(("stop", true)),
            Sun::Stopped | Sun::Failed(_) => Some(("start", false)),
            _ => None,
        };
        let mut bw = 0.0;
        if let Some((text, stop)) = action {
            bw = self.ui.micro_width(text) + 28.0;
            let enabled = stop || matches!(self.probe, Probe::Done(_));
            let (_, clicked) = self.ui.button(x + pad + inner - bw, y, text, false, enabled);
            if clicked {
                if stop {
                    self.stop_sunshine();
                } else {
                    self.start_sunshine();
                }
            }
        }
        let dh = self.ui.paragraph(x + pad + 22.0, y + t.line_height + 2.0, inner - 22.0 - bw - 12.0, &detail, t.fg_muted);
        y += t.line_height + 2.0 + dh + 24.0;
        y - y0
    }
}

/// What to ask the host for, from the settings and what the host can encode.
struct StreamPlan {
    width: u32,
    height: u32,
    fps: u32,
    /// Kilobits per second.
    bitrate: u32,
    formats: i32,
}

impl StreamPlan {
    fn new(config: &StreamConfig, facts: &Facts, window: (u32, u32)) -> Self {
        use crate::stream::ffi;
        let (width, height) = match config.resolution.as_str() {
            "720" => (1280, 720),
            "1080" => (1920, 1080),
            "1440" => (2560, 1440),
            "2160" => (3840, 2160),
            // "window" (and the older "native"): the window's own pixels, so
            // the picture is drawn 1:1 -- sharp, and no encoding of detail
            // that scaling would throw away.
            _ => window_resolution(window),
        };
        let fps = config.fps;
        let bitrate = if config.bitrate > 0 {
            config.bitrate * 1000
        } else {
            // ~20 Mbps for 1080p60, scaled by pixels and frames.
            let scale = (width * height) as f64 / (1920.0 * 1080.0) * fps as f64 / 60.0;
            ((20_000.0 * scale) as u32).clamp(5_000, 150_000)
        };
        let host = facts.codecs();
        let h264 = ffi::VIDEO_FORMAT_H264 as i32;
        let hevc = ffi::VIDEO_FORMAT_H265 as i32;
        let av1 = ffi::VIDEO_FORMAT_AV1_MAIN8 as i32;
        let formats = match config.codec.as_str() {
            "h264" => h264,
            "hevc" => hevc | h264,
            "av1" => av1 | h264,
            // Auto: what the host can encode in hardware; H.264 is always in
            // the mask as the floor every Sunshine can fall back to.
            _ => h264 | if host.contains(&"HEVC") { hevc } else { 0 } | if host.contains(&"AV1") { av1 } else { 0 },
        };
        Self { width, height, fps, bitrate, formats }
    }
}

/// The stream size for a window of `pixels`: capped at 4K (keeping its
/// shape), at least 640x360, and a multiple of 8 each way -- encoders work in
/// blocks, and NVENC refuses odd sizes.
fn window_resolution((w, h): (u32, u32)) -> (u32, u32) {
    let scale = (3840.0 / w as f64).min(2160.0 / h as f64).min(1.0);
    let fit = |v: u32, least: u32| (((v as f64 * scale) as u32) / 8 * 8).max(least);
    (fit(w, 640), fit(h, 360))
}

/// Pairs with a freshly started instance: forwards Sunshine's two pairing
/// ports over SSH, runs the handshake, and writes the PIN to Sunshine's stdin
/// a moment after the first request is on the wire. Blocking.
fn pair(bridge: &Bridge, instance: Instance) -> Result<Paired, String> {
    let at = Endpoints {
        http: block_on(bridge.forward(instance.port))?,
        https: block_on(bridge.forward(instance.port - 5))?,
    };
    let identity = Identity::new()?;
    let pin = pairing::new_pin()?;
    let (writer, line) = (bridge.clone(), format!("{pin}\n"));
    let server_cert = pairing::pair(&identity, &at, &pin, move || {
        // The PIN only counts once Sunshine holds the first request open; a
        // beat after it is sent, it is. Written from a thread of its own: this
        // one is about to wait for the answer.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = block_on(writer.write(instance.handle, &line));
        });
    })?;
    if !pairing::is_paired(&identity, &at, &server_cert)? {
        return Err("Sunshine does not list us as paired".into());
    }
    Ok(Paired { identity, server_cert, at })
}

/// The launch request: the host's apps, then `/launch` -- or `/resume`, since
/// leaving a stream leaves its app running on the host, as every Moonlight
/// client does. Blocking (OpenSSL).
fn launch(paired: &Paired, plan: &StreamPlan, address: String, audio: bool, on_host: bool) -> Result<Session, String> {
    use crate::stream::ffi;
    let info = pairing::server_info(&paired.identity, &paired.at, &paired.server_cert)?;
    let apps = pairing::app_list(&paired.identity, &paired.at, &paired.server_cert)?;
    let app = apps
        .iter()
        .find(|(_, title)| title == "Desktop")
        .or_else(|| apps.first())
        .ok_or("the host offers no apps")?;
    let resume = info.current_game.trim() != "0";
    let key = pairing::InputKey::new()?;
    // Stereo, as moonlight-common-c's MAKE_AUDIO_CONFIGURATION(2, 0x3).
    let audio_configuration = (0x3 << 16) | (2 << 8) | 0xCA;
    let surround_info = (0x3 << 16) | 2;
    let extra = unsafe { std::ffi::CStr::from_ptr(ffi::LiGetLaunchUrlQueryParameters()) }.to_string_lossy().into_owned();
    let rtsp_url = pairing::launch(
        &paired.identity,
        &paired.at,
        &paired.server_cert,
        &app.0,
        (plan.width, plan.height, plan.fps),
        &key,
        surround_info,
        &extra,
        resume,
        on_host,
    )?;
    Ok(Session {
        address,
        app_version: info.app_version,
        gfe_version: info.gfe_version,
        rtsp_url,
        codec_support: info.codec_support,
        width: plan.width as i32,
        height: plan.height as i32,
        fps: plan.fps as i32,
        bitrate: plan.bitrate as i32,
        formats: plan.formats,
        audio_configuration,
        input_key: key.key,
        input_key_id: key.key_id,
        audio,
    })
}

#[cfg(test)]
mod tests {
    use super::window_resolution;

    #[test]
    fn the_window_resolution_is_its_pixels_in_encoder_blocks() {
        assert_eq!(window_resolution((1920, 1080)), (1920, 1080));
        assert_eq!(window_resolution((943, 1028)), (936, 1024), "rounded down to multiples of 8");
        assert_eq!(window_resolution((5120, 2880)), (3840, 2160), "capped at 4K, shape kept");
        assert_eq!(window_resolution((300, 200)), (640, 360), "never below 640x360");
    }
}
