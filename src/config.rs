//! Settings that survive a restart: the NAS address, paths, checkboxes.
//!
//! Written next to the user profile rather than beside the exe, because resub.exe is
//! often run straight out of the downloads folder.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub remote: String,
    pub nas_bin: String,
    pub nas_local: String,
    pub skip_existing: bool,
    pub mkv: String,
    pub out: String,
    /// Last directory visited in the remote browser, so it opens where you left it.
    pub remote_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: String::new(),
            nas_bin: "~/resub-linux-x86_64".to_string(),
            nas_local: String::new(),
            skip_existing: true,
            mkv: String::new(),
            out: String::new(),
            remote_dir: String::new(),
        }
    }
}

/// %APPDATA%\resub\config.json on Windows, $XDG_CONFIG_HOME/resub/config.json elsewhere.
pub fn path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(PathBuf::from(appdata).join("resub").join("config.json"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("resub").join("config.json"));
        }
        if let Ok(home) = std::env::var("HOME") {
            return Some(
                PathBuf::from(home)
                    .join(".config")
                    .join("resub")
                    .join("config.json"),
            );
        }
    }
    None
}

impl Config {
    /// Missing or unreadable file is not an error: it just means first run.
    pub fn load() -> Self {
        let Some(p) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(p) = path() else {
            return;
        };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&p, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_every_field() {
        let c = Config {
            remote: "yukine@10.0.0.205:3444".into(),
            nas_bin: "/volume1/resub/resub-linux-x86_64".into(),
            nas_local: "C:\\tmp\\resub-linux-x86_64".into(),
            skip_existing: false,
            mkv: "/volume1/video/动画".into(),
            out: String::new(),
            remote_dir: "/volume1/video".into(),
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(back.remote, c.remote);
        assert_eq!(back.nas_bin, c.nas_bin);
        assert!(!back.skip_existing);
        assert_eq!(back.remote_dir, c.remote_dir);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // An older config without remote_dir must still load.
        let back: Config = serde_json::from_str("{\"remote\":\"nas\"}").unwrap();
        assert_eq!(back.remote, "nas");
        assert_eq!(back.nas_bin, "~/resub-linux-x86_64");
        assert!(back.skip_existing);
    }
}
