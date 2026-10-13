//! Tiny JSON config in the XDG config dir. Failure to read or write is never
//! fatal: the app simply falls back to defaults.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    pub age_confirmed: bool,
    pub discreet: bool,
    pub screen_share_safe: bool,
    pub inspector_open: bool,
    pub volume: Option<f32>,
}

pub fn path() -> PathBuf {
    jizzcraft_core::paths::config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        let mut c: Config = std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !path().exists() {
            c.inspector_open = true;
        }
        c
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, bytes);
        }
    }
}
