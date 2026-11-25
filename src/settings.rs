//! Persistent user settings (`%APPDATA%\thermalview\config.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::camera::lepton::ShutterMode;
use crate::thermal::alarm::AlarmConfig;
use crate::thermal::calibration::Calibration;
use crate::thermal::measure::Spots;
use crate::thermal::palette::Palette;
use crate::thermal::range::RangeMode;
use crate::thermal::shapes::Shapes;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Shutter-less module: MANUAL by default so the camera never runs FFC on an open lens.
    pub shutter_mode: ShutterMode,
    pub palette: Palette,
    pub range: RangeMode,
    pub spots: Spots,
    /// Box and Line measurements.
    pub shapes: Shapes,
    pub alarms: AlarmConfig,
    /// Emissivity, reflected temperature and reference correction.
    pub calibration: Calibration,
    pub show_extremes: bool,
    pub snapshot_dir: Option<PathBuf>,
    pub remote: RemoteSettings,
}

/// TCP remote control. Off by default; loopback only unless `allow_lan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSettings {
    pub enabled: bool,
    pub port: u16,
    pub allow_lan: bool,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: crate::remote::DEFAULT_PORT,
            allow_lan: false,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shutter_mode: ShutterMode::Manual,
            palette: Palette::default(),
            range: RangeMode::default(),
            spots: Spots::default(),
            shapes: Shapes::default(),
            alarms: AlarmConfig::default(),
            calibration: Calibration::default(),
            show_extremes: true,
            snapshot_dir: None,
            remote: RemoteSettings::default(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("thermalview")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Settings {
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable settings: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let tmp = config_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, config_path())?;
        Ok(())
    }

    pub fn snapshot_dir(&self) -> PathBuf {
        self.snapshot_dir.clone().unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Pictures")
                .join("ThermalView")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"palette":"Rainbow"}"#).unwrap();
        assert_eq!(s.palette, Palette::Rainbow);
        assert_eq!(s.shutter_mode, ShutterMode::Manual);
        assert!(s.show_extremes);
        assert!(!s.remote.enabled, "remote control must be opt-in");
        assert!(!s.remote.allow_lan);
    }

    #[test]
    fn roundtrip() {
        let mut s = Settings {
            range: RangeMode::Manual { lo: 20.0, hi: 35.0 },
            ..Default::default()
        };
        s.spots.add(3, 4);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.range, s.range);
        assert_eq!(back.spots.items, s.spots.items);
    }
}
