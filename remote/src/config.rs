//! Stream settings, per host, in `remote.json` beside PulseTerm's own stores.
//!
//! Same rules as PulseTerm's `settings.json`: `#[serde(default)]` so an older
//! or trimmed file still loads, values clamped on load, and **no secret** --
//! pairing is redone every launch (REMOTE_DESKTOP.md, 2.4), so there is none to
//! keep.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    /// `window` (the stream window's own pixels; `native`, from older files,
    /// means the same), `720`, `1080`, `1440`, `2160`.
    pub resolution: String,
    pub fps: u32,
    /// Megabits per second; 0 picks one from resolution and fps.
    pub bitrate: u32,
    /// `auto`, `h264`, `hevc`, `av1`.
    pub codec: String,
    /// Derived from `sound` (kept for older files): whether sound comes here.
    pub audio: bool,
    /// Where the host's sound plays: `here` (this computer; the host goes
    /// quiet), `host` (stays on the host, nothing streamed), or `both`.
    pub sound: String,
    pub gamepads: bool,
    /// When the whole keyboard goes to the host -- this computer reacting to
    /// no key at all: `always` (the window focused and the pointer over it),
    /// `captured` (while the mouse is captured), or `off`.
    pub keyboard: String,
    /// `desktop`: the pointer moves freely and the host follows it.
    /// `game`: a click in the picture captures mouse (and keyboard, with the
    /// default keyboard setting) for relative motion.
    pub mouse: String,
    /// `host`: this computer's cursor hides over the picture, which already
    /// shows the host's. `both`: keep it (it moves without the round trip).
    pub pointer: String,
    /// `here`: scroll the host as this computer scrolls (its natural-scroll
    /// setting included); `reversed`: the other way round.
    pub scroll: String,
    /// `low`: frames shown without waiting for the display (may tear), a
    /// shorter audio buffer, and the host's encoder in its low-latency mode.
    pub latency: String,
    /// The host's software encoder speed (Sunshine's `sw_preset`):
    /// `ultrafast`, `superfast` (Sunshine's default), `veryfast`. Only a host
    /// without a hardware encoder uses it -- and there it is most of the CPU.
    pub sw_preset: String,
    /// Encoder threads on the host (Sunshine's `min_threads`, default 2).
    pub threads: u32,
    /// Error-correction data, percent of each frame (`fec_percentage`, 20).
    pub fec: u32,
    /// The host display to stream, by connector name (`HDMI-A-1`); empty
    /// lets Sunshine pick.
    pub display: String,
    // No "keep Sunshine running": PulseTerm's instance is started with `-1`
    // and forgets its pairings on exit, so one left running would be paired
    // with nothing. It lives exactly as long as this window (sunshine.rs).
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            resolution: "window".into(),
            fps: 60,
            bitrate: 0,
            codec: "auto".into(),
            audio: true,
            // Empty until `clamp`: a file from before this setting has no
            // `sound`, and its `audio` switch must decide, not this default.
            sound: String::new(),
            gamepads: true,
            keyboard: "always".into(),
            mouse: "desktop".into(),
            pointer: "host".into(),
            scroll: "here".into(),
            latency: "normal".into(),
            sw_preset: "superfast".into(),
            threads: 2,
            fec: 20,
            display: String::new(),
        }
    }
}

impl StreamConfig {
    fn clamp(&mut self) {
        // A file from before `sound` has only the on/off switch: on meant
        // here, off meant the host kept its sound.
        if !["here", "host", "both"].contains(&self.sound.as_str()) {
            self.sound = if self.audio { "here" } else { "host" }.into();
        }
        self.audio = self.sound != "host";
        if ![30, 60, 90, 120].contains(&self.fps) {
            self.fps = 60;
        }
        self.bitrate = self.bitrate.min(500);
        if !["desktop", "game"].contains(&self.mouse.as_str()) {
            self.mouse = "desktop".into();
        }
        if !["normal", "low"].contains(&self.latency.as_str()) {
            self.latency = "normal".into();
        }
        if !["here", "reversed"].contains(&self.scroll.as_str()) {
            self.scroll = "here".into();
        }
        if !["host", "both"].contains(&self.pointer.as_str()) {
            self.pointer = "host".into();
        }
        if !["captured", "always", "off"].contains(&self.keyboard.as_str()) {
            self.keyboard = "always".into();
        }
        if !["ultrafast", "superfast", "veryfast"].contains(&self.sw_preset.as_str()) {
            self.sw_preset = "superfast".into();
        }
        if ![1, 2, 4, 8].contains(&self.threads) {
            self.threads = 2;
        }
        if !self.display.is_empty() && !crate::sunshine::safe_display(&self.display) {
            self.display.clear();
        }
        if ![5, 10, 20].contains(&self.fec) {
            self.fec = 20;
        }
        if !["window", "native", "720", "1080", "1440", "2160"].contains(&self.resolution.as_str()) {
            self.resolution = "window".into();
        }
        if !["auto", "h264", "hevc", "av1"].contains(&self.codec.as_str()) {
            self.codec = "auto".into();
        }
    }
}

/// The file: one `StreamConfig` per host label (`user@host:port`).
#[derive(Default, Serialize, Deserialize)]
struct File {
    hosts: BTreeMap<String, StreamConfig>,
}

fn path() -> Option<PathBuf> {
    // PulseTerm's data directory, so the helper's settings sit with the app's.
    dirs::data_dir().map(|dir| dir.join("PulseTerm").join("remote.json"))
}

fn read() -> File {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn load(host: &str) -> StreamConfig {
    let mut config = read().hosts.get(host).cloned().unwrap_or_default();
    config.clamp();
    config
}

pub fn save(host: &str, config: &StreamConfig) {
    let Some(path) = path() else { return };
    let mut file = read();
    file.hosts.insert(host.to_string(), config.clone());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&file) {
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("pulseterm-remote: could not save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_audio_switch_becomes_where_sound_plays() {
        let mut off: StreamConfig = serde_json::from_str(r#"{"audio":false}"#).unwrap();
        off.clamp();
        assert_eq!(off.sound, "host");
        let mut on: StreamConfig = serde_json::from_str(r#"{"audio":true}"#).unwrap();
        on.clamp();
        assert_eq!(on.sound, "here");
        let mut both: StreamConfig = serde_json::from_str(r#"{"sound":"both","audio":false}"#).unwrap();
        both.clamp();
        assert!(both.audio, "sound decides; audio follows it");
    }

    #[test]
    fn host_cpu_settings_are_clamped_to_their_choices() {
        let mut c: StreamConfig = serde_json::from_str(r#"{"sw_preset":"placebo","threads":3,"fec":50}"#).unwrap();
        c.clamp();
        assert_eq!((c.sw_preset.as_str(), c.threads, c.fec), ("superfast", 2, 20));
    }

    #[test]
    fn a_trimmed_or_wrong_file_still_loads_sanely() {
        let mut c: StreamConfig = serde_json::from_str(r#"{"fps":75,"codec":"vp9"}"#).unwrap();
        c.clamp();
        assert_eq!(c.fps, 60);
        assert_eq!(c.codec, "auto");
        assert!(c.audio, "missing fields take their defaults");
    }
}
