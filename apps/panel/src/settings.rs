//! 面板的持久化设置：三档笔宽与压感曲线。
//!
//! 保存在系统配置目录（Linux: `$XDG_CONFIG_HOME` 或 `~/.config`，
//! Windows: `%APPDATA%`），避免每次启动重新调整。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PRESET_LABELS: [&str; 3] = ["细", "中", "粗"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub width_presets: [f64; 3],
    pub width_preset: usize,
    pub gamma: f64,
    pub min_ratio: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            width_presets: [1.0, 2.0, 4.0],
            width_preset: 1,
            gamma: 1.0,
            min_ratio: 0.25,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = path() else {
            log::warn!("找不到系统配置目录，设置将不会保存");
            return Self::default();
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(error) => {
                log::warn!("读取设置失败 {}: {error}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut settings) => {
                settings.sanitize();
                settings
            }
            Err(error) => {
                log::warn!("设置文件格式无效 {}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "找不到系统配置目录")
        })?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        fs::write(path, text)
    }

    pub fn width(&self) -> f64 {
        self.width_presets[self.width_preset.min(2)]
    }

    pub fn set_preset(&mut self, index: usize) {
        self.width_preset = index.min(2);
    }

    pub fn sanitize(&mut self) {
        for width in &mut self.width_presets {
            *width = if width.is_finite() {
                width.clamp(0.1, 20.0)
            } else {
                2.0
            };
        }
        self.width_preset = self.width_preset.min(2);
        self.gamma = if self.gamma.is_finite() {
            self.gamma.clamp(0.2, 4.0)
        } else {
            1.0
        };
        self.min_ratio = if self.min_ratio.is_finite() {
            self.min_ratio.clamp(0.0, 1.0)
        } else {
            0.25
        };
    }
}

pub fn path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("link-ink").join("settings.json"))
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link-ink/settings.json");
        let settings = Settings {
            width_presets: [0.8, 2.4, 6.0],
            width_preset: 2,
            gamma: 1.6,
            min_ratio: 0.4,
        };

        settings.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path);

        assert_eq!(loaded, settings);
    }

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Settings::load_from(&dir.path().join("missing.json"));
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn invalid_values_are_clamped() {
        let mut settings = Settings {
            width_presets: [-2.0, f64::NAN, 99.0],
            width_preset: 7,
            gamma: 0.0,
            min_ratio: 4.0,
        };
        settings.sanitize();

        assert_eq!(settings.width_presets, [0.1, 2.0, 20.0]);
        assert_eq!(settings.width_preset, 2);
        assert_eq!(settings.gamma, 0.2);
        assert_eq!(settings.min_ratio, 1.0);
    }

    #[test]
    fn corrupt_json_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
    }
}
