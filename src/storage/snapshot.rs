//! Snapshot = colored PNG (what you see) + 16-bit PNG of the raw counts + JSON metadata.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::thermal::calibration::Calibration;
use crate::thermal::frame::ThermalFrame;
use crate::thermal::measure::Spot;
use crate::thermal::shapes::{Shape, ShapeKind};
use crate::thermal::upscale::{UpscaleSettings, FACTOR};
use crate::webcam::blend::Overlay;
use crate::webcam::Picture;

#[derive(Debug, Clone, Serialize)]
pub struct SpotReading {
    pub id: u32,
    pub x: usize,
    pub y: usize,
    pub value: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShapeReading {
    pub label: String,
    pub kind: String,
    pub a: (usize, usize),
    pub b: (usize, usize),
    pub min: f32,
    pub max: f32,
    pub mean: f32,
}

/// Difference between two measurements ("S1 − S2").
#[derive(Debug, Clone, Serialize)]
pub struct DeltaReading {
    pub label: String,
    pub value: f32,
}

/// The difference image a snapshot's color picture shows.
#[derive(Debug, Clone, Serialize)]
pub struct DifferenceInfo {
    /// Local time of day the reference frame was taken.
    pub reference_local: String,
    /// The colors run from −span to +span.
    pub span: f32,
    /// Largest drop and largest rise in the image.
    pub min: f32,
    pub max: f32,
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
    pub shapes: Vec<ShapeReading>,
    pub deltas: Vec<DeltaReading>,
    /// Strength of the temporal noise filter the frame went through (`null` when off). The
    /// 16-bit PNG holds the filtered counts.
    pub noise_filter: Option<u8>,
    /// Set when the color image is a difference image (change since a reference frame) and
    /// `range` is ± its span; `null` for a temperature image. The 16-bit PNG always holds
    /// temperatures.
    pub difference: Option<DifferenceInfo>,
    pub device: Option<String>,
    pub lepton_part: Option<String>,
    pub fpa_c: Option<f32>,
    pub shutter_mode: Option<String>,
    /// Correction applied to every temperature above; `null` when none. The 16-bit PNG holds the
    /// camera's uncorrected counts.
    pub calibration: Option<Calibration>,
    /// Webcam overlay in use; its picture is saved as `_webcam.png`.
    pub webcam: Option<WebcamInfo>,
    /// Display upscaling in use; the upscaled color image is saved as `_x4.png`.
    pub upscale: Option<UpscaleSettings>,
    /// Upscayl model making `_ai_x4.png` after the snapshot (written a moment later).
    pub ai_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebcamInfo {
    pub device: String,
    pub width: usize,
    pub height: usize,
    pub overlay: Overlay,
}

pub struct SnapshotFiles {
    pub color_png: PathBuf,
    pub raw_png: PathBuf,
    pub meta_json: PathBuf,
    pub webcam_png: Option<PathBuf>,
    pub upscaled_png: Option<PathBuf>,
    pub temps_csv: Option<PathBuf>,
}

/// Write the files into `dir` with a timestamped base name: three, plus the upscaled color image
/// (FACTOR times the frame size) and the webcam picture when given, and with `temps` the
/// temperature of every pixel as a CSV table.
pub fn save(
    dir: &Path,
    frame: &ThermalFrame,
    rgba: &[u8],
    upscaled: Option<&[u8]>,
    webcam: Option<&Picture>,
    temps: bool,
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
        webcam_png: webcam.map(|_| dir.join(format!("{base}_webcam.png"))),
        upscaled_png: upscaled.map(|_| dir.join(format!("{base}_x4.png"))),
        temps_csv: temps.then(|| dir.join(format!("{base}_temps.csv"))),
    };
    let (w, h) = (frame.width as u32, frame.height as u32);

    let color = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("color buffer size mismatch"))?;
    color.save(&files.color_png)?;

    let raw = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(w, h, frame.raw.clone())
        .ok_or_else(|| anyhow::anyhow!("raw buffer size mismatch"))?;
    raw.save(&files.raw_png)?;

    if let (Some(up), Some(path)) = (upscaled, &files.upscaled_png) {
        let f = FACTOR as u32;
        let img = image::RgbaImage::from_raw(w * f, h * f, up.to_vec())
            .ok_or_else(|| anyhow::anyhow!("upscaled buffer size mismatch"))?;
        img.save(path)?;
    }

    if let (Some(pic), Some(path)) = (webcam, &files.webcam_png) {
        let img = image::RgbaImage::from_raw(pic.width as u32, pic.height as u32, pic.rgba.clone())
            .ok_or_else(|| anyhow::anyhow!("webcam buffer size mismatch"))?;
        image::DynamicImage::ImageRgba8(img).to_rgb8().save(path)?;
    }

    if let Some(path) = &files.temps_csv {
        std::fs::write(path, temps_table(frame))?;
    }

    std::fs::write(&files.meta_json, serde_json::to_string_pretty(meta)?)?;
    Ok(files)
}

/// The display value (corrected °C, or counts) of every pixel: one line per image row, comma
/// separated, two decimals.
pub fn temps_table(frame: &ThermalFrame) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(frame.raw.len() * 6);
    for row in frame.raw.chunks(frame.width.max(1)) {
        for (i, &v) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "{:.2}", frame.value(v));
        }
        out.push_str("\r\n");
    }
    out
}

/// File-name endings of the files of one snapshot, after the common base name.
const SUFFIXES: [&str; 7] = [
    "_ai_x4_in",
    "_ai_x4",
    "_raw16",
    "_webcam",
    "_temps",
    "_report",
    "_x4",
];

/// A saved snapshot read back.
#[derive(Debug)]
pub struct Loaded {
    /// The frame as saved: after rotation and noise filter, before temperature correction.
    pub frame: ThermalFrame,
    /// Base name of the snapshot's files.
    pub name: String,
    /// The measurements placed when it was taken (those that had a value then).
    pub marks: SavedMarks,
}

/// Measurements recorded in a snapshot's JSON, with their original numbers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedMarks {
    pub spots: Vec<Spot>,
    pub shapes: Vec<Shape>,
}

impl SavedMarks {
    pub fn is_empty(&self) -> bool {
        self.spots.is_empty() && self.shapes.is_empty()
    }
}

/// The part of the JSON needed to read the 16-bit image as temperatures, and to show the
/// measurements again.
#[derive(serde::Deserialize)]
struct SavedMeta {
    kelvin_per_count: Option<f32>,
    #[serde(default)]
    captured_local: String,
    #[serde(default)]
    spots: Vec<SavedSpot>,
    #[serde(default)]
    shapes: Vec<SavedShape>,
}

#[derive(serde::Deserialize)]
struct SavedSpot {
    id: u32,
    x: usize,
    y: usize,
}

#[derive(serde::Deserialize)]
struct SavedShape {
    label: String,
    kind: String,
    a: (usize, usize),
    b: (usize, usize),
}

impl SavedShape {
    /// "B2" + "Box" → Box 2. `None` for a kind this version does not know.
    fn shape(&self) -> Option<Shape> {
        let kind = match self.kind.as_str() {
            "Box" => ShapeKind::Box,
            "Line" => ShapeKind::Line,
            "Ellipse" => ShapeKind::Ellipse,
            _ => return None,
        };
        let id = self
            .label
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .unwrap_or(0);
        Some(Shape {
            id,
            kind,
            a: self.a,
            b: self.b,
        })
    }
}

/// Read a snapshot back from any of its files (color PNG, 16-bit PNG, JSON…): the frame comes
/// from `<base>_raw16.png` and its scale from `<base>.json`.
pub fn load(path: &Path) -> anyhow::Result<Loaded> {
    use anyhow::Context as _;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("파일 이름을 읽을 수 없습니다"))?;
    let name = SUFFIXES
        .iter()
        .find_map(|s| stem.strip_suffix(s))
        .unwrap_or(stem)
        .to_owned();
    let dir = path.parent().unwrap_or(Path::new("."));
    let raw_png = dir.join(format!("{name}_raw16.png"));
    let meta_json = dir.join(format!("{name}.json"));

    let text = std::fs::read_to_string(&meta_json)
        .with_context(|| format!("{name}.json이 없어 온도 단위를 알 수 없습니다"))?;
    let meta: SavedMeta = serde_json::from_str(&text)
        .with_context(|| format!("{name}.json은 스냅샷 정보가 아닙니다"))?;
    let image::DynamicImage::ImageLuma16(img) = image::open(&raw_png)
        .with_context(|| format!("온도 원본 {name}_raw16.png를 열 수 없습니다"))?
    else {
        anyhow::bail!("{name}_raw16.png는 16비트 온도 원본이 아닙니다");
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        anyhow::bail!("{name}_raw16.png에 화소가 없습니다");
    }
    let scale = meta.kelvin_per_count.filter(|k| k.is_finite() && *k > 0.0);
    let mut frame = ThermalFrame::new(w, h, img.into_raw(), scale);
    // Unknown time: the same mark as a played-back frame.
    frame.captured = chrono::DateTime::parse_from_rfc3339(&meta.captured_local)
        .map_or(std::time::SystemTime::UNIX_EPOCH, Into::into);
    let marks = SavedMarks {
        spots: meta
            .spots
            .iter()
            .map(|s| Spot {
                id: s.id,
                x: s.x,
                y: s.y,
            })
            .collect(),
        shapes: meta.shapes.iter().filter_map(SavedShape::shape).collect(),
    };
    Ok(Loaded { frame, name, marks })
}

/// Snapshots in `dir` that can be opened again, as (16-bit PNG, bytes, last change), newest
/// first.
pub fn list(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if !name.ends_with("_raw16.png") {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((path, meta.len(), meta.modified().ok()?))
        })
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.0.cmp(&a.0)));
    out
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
            shapes: vec![],
            deltas: vec![],
            noise_filter: None,
            difference: None,
            device: None,
            lepton_part: None,
            fpa_c: None,
            shutter_mode: None,
            calibration: None,
            webcam: None,
            upscale: None,
            ai_model: None,
        };
        let up = vec![200u8; 16 * 12 * 4];
        let files = save(&dir, &frame, &rgba, Some(&up), None, true, &meta).unwrap();
        assert!(files.webcam_png.is_none());
        let x4 = image::open(files.upscaled_png.as_ref().unwrap()).unwrap();
        assert_eq!((x4.width(), x4.height()), (16, 12));
        let back = image::open(&files.raw_png).unwrap().into_luma16();
        assert_eq!(back.into_raw(), raw);

        // One line per image row, in °C.
        let table = std::fs::read_to_string(files.temps_csv.as_ref().unwrap()).unwrap();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "16.85,17.22,17.59,17.96");
        assert!(lines[2].ends_with(",20.92"));

        // Opened again from any of its files: the same counts and scale.
        for path in [
            &files.color_png,
            &files.raw_png,
            &files.meta_json,
            files.upscaled_png.as_ref().unwrap(),
            files.temps_csv.as_ref().unwrap(),
        ] {
            let loaded = load(path).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            assert_eq!(loaded.frame.raw, raw);
            assert_eq!((loaded.frame.width, loaded.frame.height), (4, 3));
            assert_eq!(loaded.frame.kelvin_per_count, Some(0.01));
            assert!(loaded.name.starts_with("thermal_") && !loaded.name.contains("raw16"));
            // No time in this JSON: marked like a played-back frame.
            assert_eq!(loaded.frame.captured, std::time::SystemTime::UNIX_EPOCH);
        }
        assert_eq!(
            list(&dir).iter().map(|f| &f.0).collect::<Vec<_>>(),
            vec![&files.raw_png]
        );

        // The color picture alone is not a snapshot; neither is an 8-bit "raw16".
        std::fs::remove_file(&files.meta_json).unwrap();
        let e = load(&files.color_png).unwrap_err();
        assert!(format!("{e:#}").contains("온도 단위"), "{e:#}");
        std::fs::write(&files.meta_json, r#"{"kelvin_per_count":0.01}"#).unwrap();
        std::fs::copy(&files.color_png, &files.raw_png).unwrap();
        let e = load(&files.raw_png).unwrap_err();
        assert!(format!("{e:#}").contains("16비트"), "{e:#}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn captured_time_comes_back() {
        let dir = std::env::temp_dir().join(format!("thermalview_load_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(2, 1, vec![5, 6]);
        img.unwrap().save(dir.join("x_raw16.png")).unwrap();
        std::fs::write(
            dir.join("x.json"),
            r#"{"kelvin_per_count":null,"captured_local":"2026-11-28T10:00:00+09:00"}"#,
        )
        .unwrap();
        let loaded = load(&dir.join("x.png")).unwrap();
        assert_eq!(
            loaded.frame.kelvin_per_count, None,
            "raw counts stay counts"
        );
        let when = chrono::DateTime::<chrono::Utc>::from(loaded.frame.captured);
        assert_eq!(when.to_rfc3339(), "2026-11-28T01:00:00+00:00");
        std::fs::remove_dir_all(&dir).ok();
    }
}
