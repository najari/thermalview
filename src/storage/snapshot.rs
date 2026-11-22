//! Snapshot = colored PNG (what you see) + 16-bit PNG of the raw counts + JSON metadata.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::thermal::frame::ThermalFrame;

#[derive(Debug, Clone, Serialize)]
pub struct SpotReading {
    pub id: u32,
    pub x: usize,
    pub y: usize,
    pub value: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMeta {
    pub app: &'static str,
    pub version: &'static str,
    pub captured_local: String,
    pub width: usize,
    pub height: usize,
    /// Value = raw × kelvin_per_count − 273.15 °C. `null` when TLinear is off (raw counts).
    pub kelvin_per_count: Option<f32>,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub palette: String,
    pub range: (f32, f32),
    pub spots: Vec<SpotReading>,
    pub device: Option<String>,
    pub lepton_part: Option<String>,
    pub fpa_c: Option<f32>,
    pub shutter_mode: Option<String>,
}

pub struct SnapshotFiles {
    pub color_png: PathBuf,
    pub raw_png: PathBuf,
    pub meta_json: PathBuf,
}

/// Write the three files into `dir` with a timestamped base name.
pub fn save(
    dir: &Path,
    frame: &ThermalFrame,
    rgba: &[u8],
    meta: &SnapshotMeta,
) -> anyhow::Result<SnapshotFiles> {
    std::fs::create_dir_all(dir)?;
    let base = chrono::Local::now()
        .format("thermal_%Y%m%d_%H%M%S_%3f")
        .to_string();
    let files = SnapshotFiles {
        color_png: dir.join(format!("{base}.png")),
        raw_png: dir.join(format!("{base}_raw16.png")),
        meta_json: dir.join(format!("{base}.json")),
    };
    let (w, h) = (frame.width as u32, frame.height as u32);

    let color = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("color buffer size mismatch"))?;
    color.save(&files.color_png)?;

    let raw = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(w, h, frame.raw.clone())
        .ok_or_else(|| anyhow::anyhow!("raw buffer size mismatch"))?;
    raw.save(&files.raw_png)?;

    std::fs::write(&files.meta_json, serde_json::to_string_pretty(meta)?)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_png_roundtrips_exact_counts() {
        let dir = std::env::temp_dir().join(format!("thermalview_test_{}", std::process::id()));
        let raw: Vec<u16> = (0..12).map(|i| 29000 + i * 37).collect();
        let frame = ThermalFrame::new(4, 3, raw.clone(), Some(0.01));
        let rgba = vec![128u8; 4 * 3 * 4];
        let meta = SnapshotMeta {
            app: "thermalview",
            version: "test",
            captured_local: String::new(),
            width: 4,
            height: 3,
            kelvin_per_count: Some(0.01),
            unit: "°C",
            min: 0.0,
            max: 0.0,
            mean: 0.0,
            palette: "Ironbow".into(),
            range: (0.0, 1.0),
            spots: vec![],
            device: None,
            lepton_part: None,
            fpa_c: None,
            shutter_mode: None,
        };
        let files = save(&dir, &frame, &rgba, &meta).unwrap();
        let back = image::open(&files.raw_png).unwrap().into_luma16();
        assert_eq!(back.into_raw(), raw);
        std::fs::remove_dir_all(&dir).ok();
    }
}
