//! Persistent user settings (`%APPDATA%\thermalview\config.json`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::camera::lepton::ShutterMode;
use crate::camera::Source;
use crate::thermal::alarm::AlarmConfig;
use crate::thermal::calibration::Calibration;
use crate::thermal::delta::Deltas;
use crate::thermal::filter::Temporal;
use crate::thermal::isotherm::Isotherm;
use crate::thermal::measure::Spots;
use crate::thermal::palette::Palette;
use crate::thermal::range::RangeMode;
use crate::thermal::rotate::Rotation;
use crate::thermal::shapes::Shapes;
use crate::thermal::upscale::UpscaleSettings;
use crate::webcam::blend::Overlay;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Which camera to look for.
    pub camera: Source,
    /// Shutter-less module: MANUAL by default so the camera never runs FFC on an open lens.
    pub shutter_mode: ShutterMode,
    pub palette: Palette,
    /// Use the palette hot end first.
    pub palette_invert: bool,
    pub range: RangeMode,
    /// Pixels inside a temperature band shown in one color.
    pub isotherm: Isotherm,
    /// Strength of the temporal noise filter (frames averaged); 1 = off.
    pub noise_filter: u8,
    /// Color range of the difference image, ± this many display units; 0 follows the image.
    pub diff_span: f32,
    pub spots: Spots,
    /// Box and Line measurements.
    pub shapes: Shapes,
    /// Differences between two measurements.
    pub deltas: Deltas,
    pub alarms: AlarmConfig,
    /// Emissivity, reflected temperature and reference correction.
    pub calibration: Calibration,
    pub show_extremes: bool,
    /// Clockwise turn applied to live frames.
    pub rotation: Rotation,
    /// 4× display upscaling (view, MP4, extra snapshot image).
    pub upscale: UpscaleSettings,
    pub snapshot_dir: Option<PathBuf>,
    /// Add the temperature of every pixel (`_temps.csv`) to each snapshot.
    pub snapshot_temps_csv: bool,
    /// Seconds between CSV rows; 0 logs every frame.
    pub csv_interval_s: f32,
    /// Seconds between the snapshots of an interval series.
    pub timelapse_s: u32,
    pub remote: RemoteSettings,
    pub webcam: WebcamSettings,
    pub layout: Layout,
}

/// What the window shows: the panels and which sections of the settings panel are unfolded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Settings panel on the left.
    pub panel: bool,
    /// Graph panel under the image.
    pub graphs: bool,
    pub camera: bool,
    /// Camera-specific controls (Lepton FFC, P3 shutter).
    pub device: bool,
    pub display: bool,
    pub webcam: bool,
    pub measure: bool,
    pub calibration: bool,
    pub alarms: bool,
    /// Save folder.
    pub storage: bool,
    pub remote: bool,
}

impl Default for Layout {
    /// Everyday sections open; the ones set up once (webcam, calibration, alarms, remote
    /// control) folded.
    fn default() -> Self {
        Self {
            panel: true,
            graphs: true,
            camera: true,
            device: true,
            display: true,
            webcam: false,
            measure: true,
            calibration: false,
            alarms: false,
            storage: false,
            remote: false,
        }
    }
}

/// Webcam overlay. Off by default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebcamSettings {
    pub enabled: bool,
    /// Windows device path of the chosen camera.
    pub device: Option<String>,
    /// Its name when it was chosen, shown while the device list loads.
    pub device_name: String,
    pub overlay: Overlay,
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
            camera: Source::default(),
            shutter_mode: ShutterMode::Manual,
            palette: Palette::default(),
            palette_invert: false,
            range: RangeMode::default(),
            isotherm: Isotherm::default(),
            noise_filter: 1,
            diff_span: 0.0,
            spots: Spots::default(),
            shapes: Shapes::default(),
            deltas: Deltas::default(),
            alarms: AlarmConfig::default(),
            calibration: Calibration::default(),
            show_extremes: true,
            rotation: Rotation::default(),
            upscale: UpscaleSettings::default(),
            snapshot_dir: None,
            snapshot_temps_csv: false,
            csv_interval_s: 0.0,
            timelapse_s: 10,
            remote: RemoteSettings::default(),
            webcam: WebcamSettings::default(),
            layout: Layout::default(),
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
    /// The saved settings, and a message for the user when they could not be used as saved.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(&config_path())
    }

    /// A file that cannot be read as settings is kept as `<name>.bad` instead of being
    /// overwritten by the next save: it holds the measurements, alarm rules and calibration.
    fn load_from(path: &std::path::Path) -> (Self, Option<String>) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return (Self::default(), None);
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut s) => {
                s.sanitize();
                let fixed = s.calibration.sanitize();
                let note = fixed.then(|| {
                    "설정 파일의 온도 보정 값이 올바르지 않아 일부를 기본값으로 바꿨습니다".into()
                });
                (s, note)
            }
            Err(e) => {
                log::warn!("unreadable settings ({}): {e}", path.display());
                let bad = path.with_extension("json.bad");
                let kept = std::fs::rename(path, &bad).is_ok();
                let note = if kept {
                    format!(
                        "설정 파일을 읽을 수 없어 기본 설정으로 시작합니다. 이전 파일은 {}에 \
                         남겨 두었습니다",
                        bad.display()
                    )
                } else {
                    "설정 파일을 읽을 수 없어 기본 설정으로 시작합니다".into()
                };
                (Self::default(), Some(note))
            }
        }
    }

    /// Bring hand-edited values back into what the window offers.
    fn sanitize(&mut self) {
        if !Temporal::STRENGTHS.contains(&self.noise_filter) {
            self.noise_filter = 1;
        }
        if !(self.csv_interval_s.is_finite() && (0.0..=3600.0).contains(&self.csv_interval_s)) {
            self.csv_interval_s = 0.0;
        }
        self.timelapse_s = self.timelapse_s.clamp(1, 3600);
        if !(self.diff_span.is_finite() && (0.0..=1000.0).contains(&self.diff_span)) {
            self.diff_span = 0.0;
        }
        let iso = &mut self.isotherm;
        if !(iso.lo.is_finite() && iso.hi.is_finite()) {
            *iso = Isotherm::default();
        }
        self.deltas.items.truncate(Deltas::MAX);
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
        assert!(!s.webcam.enabled, "the webcam must be opt-in");
    }

    #[test]
    fn unreadable_file_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("thermalview_cfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        assert!(Settings::load_from(&path).1.is_none(), "no file: defaults");

        std::fs::write(&path, r#"{"palette":"NoSuchPalette","spots":{}}"#).unwrap();
        let (s, note) = Settings::load_from(&path);
        assert_eq!(s.palette, Palette::default());
        assert!(note.unwrap().contains("config.json.bad"));
        assert!(!path.exists());
        assert!(dir.join("config.json.bad").exists());

        // Values the window does not offer fall back to the defaults.
        std::fs::write(
            &path,
            r#"{"noise_filter":3,"csv_interval_s":-5,"timelapse_s":0,"range":"Robust"}"#,
        )
        .unwrap();
        let (s, note) = Settings::load_from(&path);
        assert!(note.is_none());
        assert_eq!(
            (s.noise_filter, s.csv_interval_s, s.timelapse_s),
            (1, 0.0, 1)
        );
        assert_eq!(s.range, RangeMode::Robust);

        // Values that would break the temperature conversion are replaced, with a note.
        std::fs::write(&path, r#"{"calibration":{"emissivity":0.0}}"#).unwrap();
        let (s, note) = Settings::load_from(&path);
        assert!(s.calibration.emissivity > 0.0 && note.is_some());
        std::fs::remove_dir_all(&dir).ok();
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
