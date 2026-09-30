//! Desktop preferences contain no archive policies or credentials.
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorMode {
    #[default]
    System,
    Light,
    Dark,
}
impl ColorMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}
impl Density {
    pub fn row_height(self) -> f32 {
        match self {
            Self::Comfortable => 44.,
            Self::Compact => 34.,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Geometry {
    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && (320. ..=16384.).contains(&self.width)
            && (300. ..=16384.).contains(&self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub color_mode: ColorMode,
    pub density: Density,
    pub font_size: f32,
    pub reduced_motion: bool,
    pub notifications: bool,
    pub sidebar_collapsed: bool,
    pub detail_visible: bool,
    pub detail_width: f32,
    pub queue_settings_open: bool,
    pub full_window: Option<Geometry>,
    pub quick_window: Option<Geometry>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            color_mode: ColorMode::System,
            density: Density::Comfortable,
            font_size: 14.,
            reduced_motion: false,
            notifications: true,
            sidebar_collapsed: false,
            detail_visible: true,
            detail_width: 340.,
            queue_settings_open: true,
            full_window: None,
            quick_window: None,
        }
    }
}
impl Preferences {
    pub fn path() -> Result<PathBuf, String> {
        // Keep explicit test/development configurations self-contained on every platform.
        if let Some(path) = std::env::var_os("SMARTZIP_CONFIG") {
            let path = PathBuf::from(path);
            return Ok(path.parent().unwrap_or(Path::new(".")).join("desktop.json"));
        }
        smartzip_platform::PlatformPaths::try_new()
            .map(|p| p.config_dir.join("desktop.json"))
            .map_err(|e| e.to_string())
    }
    pub fn normalize(&mut self) {
        self.font_size = if self.font_size.is_finite() {
            self.font_size.clamp(12., 18.)
        } else {
            14.
        };
        self.detail_width = if self.detail_width.is_finite() {
            self.detail_width.clamp(280., 520.)
        } else {
            340.
        };
        self.full_window = self.full_window.filter(|g| g.valid());
        self.quick_window = self.quick_window.filter(|g| g.valid());
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("无法读取外观偏好：{e}")),
        };
        let mut value: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("外观偏好格式错误：{e}"))?;
        value.normalize();
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_round_trip_and_bad_values_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("prefs.json");
        let mut p = Preferences {
            color_mode: ColorMode::Dark,
            font_size: 100.,
            ..Default::default()
        };
        p.full_window = Some(Geometry {
            x: 0.,
            y: 0.,
            width: 0.,
            height: 900.,
        });
        p.save(&path).unwrap();
        let loaded = Preferences::load(&path).unwrap();
        assert_eq!(loaded.color_mode, ColorMode::Dark);
        assert_eq!(loaded.font_size, 18.);
        assert!(loaded.full_window.is_none());
        std::fs::write(&path, "broken").unwrap();
        assert!(Preferences::load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    }
}
