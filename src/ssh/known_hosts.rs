use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use russh::keys::{HashAlg, PublicKey};

/// Trust-on-first-use host key store, similar in spirit to `~/.ssh/known_hosts`
/// but scoped to this app. First connection to a host:port pins its key
/// fingerprint; later connections must match or are refused.
pub enum HostKeyCheck {
    New,
    Known,
    Mismatch,
}

fn store_path() -> PathBuf {
    let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("PulseTerm");
    let _ = fs::create_dir_all(&p);
    p.push("known_hosts.json");
    p
}

fn load() -> HashMap<String, String> {
    fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(map: &HashMap<String, String>) {
    if let Ok(s) = serde_json::to_string_pretty(map) {
        let _ = fs::write(store_path(), s);
    }
}

pub fn check_and_store(host_id: &str, key: &PublicKey) -> HostKeyCheck {
    let fp = key.fingerprint(HashAlg::Sha256).to_string();
    let mut map = load();
    match map.get(host_id) {
        Some(existing) if existing == &fp => HostKeyCheck::Known,
        Some(_) => HostKeyCheck::Mismatch,
        None => {
            map.insert(host_id.to_string(), fp);
            save(&map);
            HostKeyCheck::New
        }
    }
}
