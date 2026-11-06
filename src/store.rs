use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use gpui::{App, BorrowAppContext as _, Global};
use serde::{Deserialize, Serialize};

use crate::ssh::{AuthMethod, ConnectParams};

pub(crate) fn data_dir() -> PathBuf {
    let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("PulseTerm");
    let _ = fs::create_dir_all(&p);
    p
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    fs::read_to_string(data_dir().join(name))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn write_json<T: Serialize>(name: &str, value: &T) {
    if let Ok(s) = serde_json::to_string_pretty(value) {
        let _ = fs::write(data_dir().join(name), s);
    }
}

/// How a saved profile authenticates.
///
/// **No secret is ever stored.** A password profile records only that it *is* a
/// password profile, so the connect dialog can come up with the right mode
/// selected and the password field focused; a key profile records the path to
/// the key file, which is a filename rather than a credential. Passphrases are
/// not stored either.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum SavedAuth {
    #[serde(rename = "password")]
    Password,
    #[serde(rename = "key")]
    PrivateKey { path: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SavedAuth,
    /// Unix seconds of the last successful connect, so the list can put the one
    /// you actually use at the top.
    #[serde(default)]
    pub last_used: u64,
}

impl Profile {
    pub fn label(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.username, self.host)
        } else {
            format!("{}@{}:{}", self.username, self.host, self.port)
        }
    }

    /// Two profiles are the same connection if they point at the same account on
    /// the same endpoint; the auth method is a detail of how you get in, not
    /// which machine you're getting into.
    fn same_target(&self, other: &Self) -> bool {
        self.host == other.host && self.port == other.port && self.username == other.username
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Profiles {
    pub items: Vec<Profile>,
}

impl Global for Profiles {}

impl Profiles {
    /// The host connected to most recently, as `user@host[:port]` -- what the
    /// home screen's SSH entry says it would reach.
    pub fn latest(cx: &gpui::App) -> Option<String> {
        let profile = cx
            .try_global::<Profiles>()?
            .items
            .iter()
            .max_by_key(|profile| profile.last_used)?;
        Some(if profile.port == 22 {
            format!("{}@{}", profile.username, profile.host)
        } else {
            format!("{}@{}:{}", profile.username, profile.host, profile.port)
        })
    }

    pub fn load() -> Self {
        let mut this: Self = read_json("profiles.json");
        this.sort();
        this
    }

    fn sort(&mut self) {
        self.items.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    }

    /// Records a successful connection, replacing any existing profile for the
    /// same target so repeated connects don't pile up duplicates.
    pub fn remember(&mut self, params: &ConnectParams) {
        let auth = match &params.auth {
            AuthMethod::Password(_) => SavedAuth::Password,
            AuthMethod::PrivateKey { path, .. } => SavedAuth::PrivateKey { path: path.clone() },
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let profile = Profile {
            id: now,
            host: params.host.clone(),
            port: params.port,
            username: params.username.clone(),
            auth,
            last_used: now,
        };
        self.items.retain(|p| !p.same_target(&profile));
        self.items.insert(0, profile);
        self.sort();
        self.save();
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|p| p.id != id);
        self.save();
    }

    pub fn save(&self) {
        write_json("profiles.json", self);
    }
}

/// Everything on the Settings page, persisted to `settings.json`.
///
/// `#[serde(default)]` on the struct is what lets a file written by an older
/// build -- or hand-edited down to a couple of keys -- still load: any missing
/// field takes its value from `Default` instead of failing the whole file.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Id of the preset theme -- see `theme::presets`.
    pub theme: String,
    /// Colour overrides on top of the preset: token name -> `#rrggbb`. Kept
    /// separate from the preset rather than baked into a copy of it, so
    /// switching preset (or resetting) is just clearing this map.
    pub colors: BTreeMap<String, String>,
    /// Font family; empty means the first installed of a list of good ones.
    pub font_family: String,
    pub font_size: f32,
    /// Row height as a percentage of the font size.
    pub line_height: f32,
    pub font_weight: u16,
    pub ligatures: bool,
    /// Window opacity, 30-100 percent.
    pub opacity: f32,
    pub blur: bool,
    /// Outline the focused pane and text field in the focus-ring colour.
    pub focus_ring: bool,
    pub scrollback: usize,
    /// `block`, `bar` or `underline`.
    pub cursor_shape: String,
    pub copy_on_select: bool,
    pub show_hidden: bool,
    pub explorer_on_connect: bool,
    /// Command a *local* terminal tab runs. Empty means "the best one
    /// installed" (`local::default_shell`), which is also the fallback when
    /// what is named here has since been uninstalled -- a stale path would open
    /// a tab that dies on arrival.
    pub local_shell: String,
    /// Folder the plugin catalogue is read from. Empty means the default
    /// beside the other stores; a path that does not exist falls back to it
    /// too, because a typo should not quietly switch every plugin off.
    pub plugins_dir: String,
    /// What the window opens on: `home`, `resume` or `local`. Validated on
    /// load -- an unknown value would otherwise leave the app with no startup
    /// behaviour at all.
    pub startup: String,
    /// Which edge the one navigation bar sits on: `left`, `right`, `top` or
    /// `bottom`. There is exactly one bar and it carries the window controls,
    /// so this is also where the caption ends up -- see `ui::nav_bar`.
    pub nav_edge: String,
    pub editor_line_numbers: bool,
    pub editor_soft_wrap: bool,
    pub editor_tab_size: usize,
    /// Whether anything moves. Off is a real setting, not a degraded mode:
    /// every animated value simply lands on its target, which is what the
    /// app did before `ui::motion` existed.
    pub animations: bool,
    /// How long a move takes, in milliseconds. One number for the whole app
    /// -- a strip that scrolls at one speed and a panel that fades at
    /// another reads as two apps.
    pub animation_ms: f32,
    /// `ease`, `linear` or `spring`. See `ui::motion::Curve`.
    pub animation_curve: String,
    /// What a new column takes on the strip: `third`, `half`, `two_thirds` or
    /// `full`. Settings -> Interface, as the design places it.
    pub default_width: String,
    /// The widest any column may be drawn while there is more than one, as a
    /// percentage of the strip. Below 100 the neighbours always peek in at the
    /// edge, so there is a seam to grab and somewhere to scroll to.
    pub max_column_width: f32,
    /// The navigation bar: `show`, `hide` or `auto` (slides in while the
    /// pointer is at its edge). Hidden, the window has no caption of its own
    /// -- see `ui::nav_bar` for what keeps it movable.
    pub nav_mode: String,
    /// Whether the bar carries minimise / maximise / close. Off, the window
    /// still closes through the OS (Alt-F4, the taskbar) and the close guard.
    pub window_buttons: bool,
    /// Where each window's header sits: `top` or `bottom`.
    pub header_position: String,
    /// The window headers: `show`, `hide` or `auto` (shown while the pointer
    /// is over that window's header edge, or the window has the focus).
    pub header_mode: String,
    /// Windows: "Open with PulseTerm" in File Explorer's right-click menu,
    /// for folders and files, registered for this user only (`shell_menu`).
    /// Mirrors what the registry says rather than being trusted blindly: the
    /// Settings page re-reads it.
    pub explorer_menu: bool,
}

impl Global for Settings {}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "ink".into(),
            colors: BTreeMap::new(),
            font_family: String::new(),
            font_size: 13.,
            // Terminal cells want a little more leading than prose so
            // box-drawing characters in TUI programs meet cleanly row to row;
            // 140% does that without looking airy.
            line_height: 140.,
            font_weight: 400,
            ligatures: false,
            opacity: 100.,
            blur: false,
            focus_ring: true,
            scrollback: 10_000,
            cursor_shape: "block".into(),
            copy_on_select: false,
            show_hidden: true,
            explorer_on_connect: false,
            local_shell: String::new(),
            plugins_dir: String::new(),
            startup: "home".into(),
            nav_edge: "left".into(),
            editor_line_numbers: true,
            editor_soft_wrap: false,
            editor_tab_size: 4,
            animations: true,
            // Long enough to be followed, short enough that holding the focus
            // key still walks the strip at the speed you press it. Much past
            // 200ms and the app starts feeling like it is thinking.
            animation_ms: 160.,
            animation_curve: "ease".into(),
            default_width: "half".into(),
            max_column_width: 90.,
            nav_mode: "show".into(),
            window_buttons: true,
            header_position: "top".into(),
            header_mode: "show".into(),
            explorer_menu: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut this: Self = read_json("settings.json");
        this.clamp();
        this
    }

    /// Keeps every value inside the range the app can actually render, whether
    /// it came from a control or from a hand-edited file.
    fn clamp(&mut self) {
        self.font_size = self.font_size.clamp(8., 32.);
        self.line_height = self.line_height.clamp(100., 200.);
        self.font_weight = self.font_weight.clamp(100, 900);
        self.opacity = self.opacity.clamp(30., 100.);
        self.scrollback = self.scrollback.min(200_000);
        self.editor_tab_size = self.editor_tab_size.clamp(1, 16);
        if !matches!(self.startup.as_str(), "home" | "resume" | "local") {
            self.startup = "home".into();
        }
        // An unknown edge would leave the window with no bar at all, and so no
        // way to move or close it.
        if !matches!(self.nav_edge.as_str(), "left" | "right" | "top" | "bottom") {
            self.nav_edge = "left".into();
        }
        // The floor is a frame or two rather than zero: a duration of nothing
        // is what the `animations` switch is for, and a 3ms glide is a jump
        // that also asks for frames.
        self.animation_ms = self.animation_ms.clamp(40., 600.);
        if !matches!(self.animation_curve.as_str(), "ease" | "linear" | "spring") {
            self.animation_curve = "ease".into();
        }
        self.max_column_width = self.max_column_width.clamp(50., 100.);
        if !matches!(self.default_width.as_str(), "third" | "half" | "two_thirds" | "full") {
            self.default_width = "half".into();
        }
        for mode in [&mut self.nav_mode, &mut self.header_mode] {
            if !matches!(mode.as_str(), "show" | "hide" | "auto") {
                *mode = "show".into();
            }
        }
        if !matches!(self.header_position.as_str(), "top" | "bottom") {
            self.header_position = "top".into();
        }
    }

    /// Cell height in pixels for the current font size and line height.
    pub fn line_height(&self) -> f32 {
        (self.font_size * self.line_height / 100.).round()
    }

    /// Changes settings, saves them, and re-applies the theme -- the one path
    /// every control on the Settings page goes through, so nothing can change a
    /// value without it also reaching the screen and the disk.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Self)) {
        cx.update_global::<Self, _>(|settings, _| {
            change(settings);
            settings.clamp();
            settings.save();
        });
        crate::theme::apply(cx);
    }

    pub fn save(&self) {
        write_json("settings.json", self);
    }
}
