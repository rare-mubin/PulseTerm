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
    /// `window` (match the stream window), `native`, `1080`, `1440`, `2160`.
    pub resolution: String,
    pub fps: u32,
    /// Megabits per second; 0 picks one from resolution and fps.
    pub bitrate: u32,
    /// `auto`, `h264`, `hevc`, `av1`.
    pub codec: String,
    pub audio: bool,
    pub gamepads: bool,
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
            gamepads: true,
        }
    }
}

impl StreamConfig {
    fn clamp(&mut self) {
        if ![30, 60, 90, 120].contains(&self.fps) {
            self.fps = 60;
        }
        self.bitrate = self.bitrate.min(500);
        if !["window", "native", "1080", "1440", "2160"].contains(&self.resolution.as_str()) {
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
    fn a_trimmed_or_wrong_file_still_loads_sanely() {
        let mut c: StreamConfig = serde_json::from_str(r#"{"fps":75,"codec":"vp9"}"#).unwrap();
        c.clamp();
        assert_eq!(c.fps, 60);
        assert_eq!(c.codec, "auto");
        assert!(c.audio, "missing fields take their defaults");
    }
}
