//! Settings that survive restarts: `~/.config/simple-whipper/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::encode::Format;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Last selected drive, e.g. `/dev/sr0`.
    pub device: Option<String>,
    /// Where ripped files go.
    pub output_dir: PathBuf,
    /// Drive read offset in samples (stereo frames). Look yours up in the
    /// AccurateRip drive offset list; e.g. many drives are +6, +667, +48.
    pub read_offset: i32,
    pub format: Format,
}

impl Default for Config {
    fn default() -> Self {
        let base = dirs::audio_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            device: None,
            output_dir: base.join("CD Rips"),
            read_offset: 0,
            format: Format::default(),
        }
    }
}

impl Config {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("simple-whipper").join("config.toml"))
    }

    /// Load the config, falling back to defaults on any problem.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best-effort save; returns an error message for the status line.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_files() {
        let c = Config {
            device: Some("/dev/sr0".into()),
            output_dir: "/music".into(),
            read_offset: 6,
            format: Format::Wav,
        };
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.read_offset, 6);
        assert_eq!(back.format, Format::Wav);
        // Missing keys fall back to defaults.
        let partial: Config = toml::from_str("read_offset = -12").unwrap();
        assert_eq!(partial.read_offset, -12);
        assert_eq!(partial.format, Format::Flac);
    }
}
