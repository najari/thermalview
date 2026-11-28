//! Webcam overlay: how the webcam picture is lined up with the thermal image and mixed with it.
//! Pure code (no Windows calls), unit-tested. The screen (egui) and the MP4 writer use the same
//! placement, so both show the same picture.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// How the two pictures are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// The thermal image, semi-transparent, over the webcam picture.
    #[default]
    Blend,
    /// The webcam picture's edges drawn in white over the thermal image.
    Edges,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Blend => "겹치기",
            Mode::Edges => "윤곽",
        }
    }
}

/// Where the webcam picture sits on the thermal image. The two cameras look from slightly
/// different places, so this only lines up exactly at one distance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alignment {
    /// Webcam picture width / thermal image width. The webcam's aspect ratio is kept.
    pub scale: f32,
    /// Shift of the picture's center, as a fraction of the thermal image width.
    pub dx: f32,
    /// Shift of the picture's center, as a fraction of the thermal image height.
    pub dy: f32,
}

impl Default for Alignment {
    fn default() -> Self {
        Self {
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }
}

impl Alignment {
    pub const SCALE: RangeInclusive<f32> = 0.5..=3.0;
    pub const SHIFT: RangeInclusive<f32> = -0.5..=0.5;

    /// The webcam picture's rectangle `[x0, y0, x1, y1]` in thermal pixel units (the thermal
    /// image spans `0..width`, `0..height`), for a `thermal` image and a `webcam` picture size.
    pub fn placement(&self, thermal: (usize, usize), webcam: (usize, usize)) -> [f32; 4] {
        let (tw, th) = (thermal.0 as f32, thermal.1 as f32);
        let scale = self.scale.clamp(*Self::SCALE.start(), *Self::SCALE.end());
        let w = tw * scale;
        let h = if webcam.0 == 0 {
            0.0
        } else {
            w * webcam.1 as f32 / webcam.0 as f32
        };
        let shift = |v: f32| v.clamp(*Self::SHIFT.start(), *Self::SHIFT.end());
        let cx = tw / 2.0 + shift(self.dx) * tw;
        let cy = th / 2.0 + shift(self.dy) * th;
        [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0]
    }
}

/// Overlay settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub mode: Mode,
    /// Blend: opacity of the thermal image (0 = webcam only, 1 = thermal only).
    pub thermal_opacity: f32,
    /// Edges: opacity of the edge lines.
    pub edge_strength: f32,
    pub align: Alignment,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            mode: Mode::Blend,
            thermal_opacity: 0.5,
            edge_strength: 0.8,
            align: Alignment::default(),
        }
    }
}

impl Overlay {
    /// The opacity the current mode uses, 0..=1.
    pub fn strength(&self) -> f32 {
        match self.mode {
            Mode::Blend => self.thermal_opacity,
            Mode::Edges => self.edge_strength,
        }
        .clamp(0.0, 1.0)
    }
}

/// Edge response below this (|Gx| + |Gy| of a Sobel filter on 0..255 luma) is sensor noise.
const EDGE_LOW: i32 = 64;
/// Edge response from which a line is drawn at full strength. A hard black/white step gives 1020.
const EDGE_HIGH: i32 = 320;

/// One webcam picture with its edge map.
#[derive(Debug)]
pub struct Picture {
    /// Increases with every picture from the same camera.
    pub seq: u64,
    pub width: usize,
    pub height: usize,
    /// RGBA, alpha 255.
    pub rgba: Vec<u8>,
    /// Edge strength per pixel, 0..=255.
    pub edges: Vec<u8>,
}

impl Picture {
    pub fn new(seq: u64, width: usize, height: usize, rgba: Vec<u8>) -> Self {
        let edges = edges(width, height, &rgba);
        Self {
            seq,
            width,
            height,
            rgba,
            edges,
        }
    }

    /// Bilinear sample at (x, y) in picture pixels (pixel centers at +0.5). `None` outside.
    pub fn color_at(&self, x: f32, y: f32) -> Option<[f32; 3]> {
        let mut out = [0.0; 3];
        self.bilinear(x, y, |i, w| {
            for (k, o) in out.iter_mut().enumerate() {
                *o += self.rgba[i * 4 + k] as f32 * w;
            }
        })?;
        Some(out)
    }

    /// Bilinear edge strength at (x, y), 0..=1. `None` outside.
    pub fn edge_at(&self, x: f32, y: f32) -> Option<f32> {
        let mut out = 0.0;
        self.bilinear(x, y, |i, w| out += self.edges[i] as f32 * w)?;
        Some(out / 255.0)
    }

    /// Calls `add(pixel index, weight)` for the four pixels around (x, y).
    fn bilinear(&self, x: f32, y: f32, mut add: impl FnMut(usize, f32)) -> Option<()> {
        let (w, h) = (self.width, self.height);
        let inside = x >= 0.0 && y >= 0.0 && x < w as f32 && y < h as f32;
        if !inside || self.rgba.len() != w * h * 4 || self.edges.len() != w * h {
            return None;
        }
        let (fx, fy) = ((x - 0.5).max(0.0), (y - 0.5).max(0.0));
        let (x0, y0) = ((fx as usize).min(w - 1), (fy as usize).min(h - 1));
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        add(y0 * w + x0, (1.0 - tx) * (1.0 - ty));
        add(y0 * w + x1, tx * (1.0 - ty));
        add(y1 * w + x0, (1.0 - tx) * ty);
        add(y1 * w + x1, tx * ty);
        Some(())
    }
}

/// Sobel edge map of an RGBA picture, 0..=255 per pixel (the border is 0).
pub fn edges(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    if width < 3 || height < 3 || rgba.len() != width * height * 4 {
        return out;
    }
    let luma: Vec<i32> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| (77 * p[0] as i32 + 150 * p[1] as i32 + 29 * p[2] as i32) >> 8)
        .collect();
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let p = |dx: usize, dy: usize| luma[(y + dy - 1) * width + (x + dx - 1)];
            let gx = (p(2, 0) + 2 * p(2, 1) + p(2, 2)) - (p(0, 0) + 2 * p(0, 1) + p(0, 2));
            let gy = (p(0, 2) + 2 * p(1, 2) + p(2, 2)) - (p(0, 0) + 2 * p(1, 0) + p(2, 0));
            let m = gx.abs() + gy.abs();
            out[y * width + x] =
                ((m - EDGE_LOW) * 255 / (EDGE_HIGH - EDGE_LOW)).clamp(0, 255) as u8;
        }
    }
    out
}

/// Mix one output pixel. `thermal` is the thermal color there; `webcam` the picture sampled at
/// the same place (`None` where the picture does not reach).
pub fn mix(overlay: &Overlay, thermal: [u8; 3], webcam: Option<WebcamSample>) -> [u8; 3] {
    let a = overlay.strength();
    let mut out = [0u8; 3];
    for k in 0..3 {
        let t = thermal[k] as f32;
        let v = match (overlay.mode, webcam) {
            // Black where there is no webcam picture, as on screen.
            (Mode::Blend, None) => t * a,
            (Mode::Blend, Some(s)) => s.color[k] + (t - s.color[k]) * a,
            (Mode::Edges, None) => t,
            (Mode::Edges, Some(s)) => t + (255.0 - t) * s.edge * a,
        };
        out[k] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The webcam picture at one place.
#[derive(Debug, Clone, Copy)]
pub struct WebcamSample {
    pub color: [f32; 3],
    /// 0..=1
    pub edge: f32,
}

/// Sample `picture` where it covers point (tx, ty) of the thermal image (thermal pixel units),
/// given its `placement` from [`Alignment::placement`]. Only what `mode` mixes in is looked up
/// (the color for [`Mode::Blend`], the edge strength for [`Mode::Edges`]); the other is 0.
pub fn sample(
    picture: &Picture,
    placement: [f32; 4],
    tx: f32,
    ty: f32,
    mode: Mode,
) -> Option<WebcamSample> {
    let [x0, y0, x1, y1] = placement;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let u = (tx - x0) / (x1 - x0) * picture.width as f32;
    let v = (ty - y0) / (y1 - y0) * picture.height as f32;
    Some(match mode {
        Mode::Blend => WebcamSample {
            color: picture.color_at(u, v)?,
            edge: 0.0,
        },
        Mode::Edges => WebcamSample {
            color: [0.0; 3],
            edge: picture.edge_at(u, v)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, c: [u8; 3]) -> Picture {
        Picture::new(1, w, h, [c[0], c[1], c[2], 255].repeat(w * h))
    }

    #[test]
    fn default_placement_is_centered_and_keeps_the_aspect_ratio() {
        let a = Alignment::default();
        // 4:3 webcam on a 4:3 thermal image: exactly on top.
        assert_eq!(
            a.placement((160, 120), (640, 480)),
            [0.0, 0.0, 160.0, 120.0]
        );
        // 16:9 webcam: as wide, less tall, centered vertically.
        assert_eq!(
            a.placement((160, 120), (1280, 720)),
            [0.0, 15.0, 160.0, 105.0]
        );
        let moved = Alignment {
            scale: 2.0,
            dx: 0.25,
            dy: -0.5,
        };
        assert_eq!(
            moved.placement((160, 120), (640, 480)),
            [-40.0, -120.0, 280.0, 120.0]
        );
        // Out-of-range values (hand-edited settings) are clamped.
        let wild = Alignment {
            scale: 100.0,
            dx: 9.0,
            dy: 0.0,
        };
        // Scale 3 (480 × 360), center moved right by half the width (x 160).
        assert_eq!(
            wild.placement((160, 120), (640, 480)),
            [-80.0, -120.0, 400.0, 240.0]
        );
    }

    #[test]
    fn edges_follow_a_step_and_ignore_flat_areas() {
        let (w, h) = (8, 6);
        let mut rgba = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                let v = if x < 4 { 0 } else { 255 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let e = edges(w, h, &rgba);
        assert_eq!(e[2 * w + 1], 0, "flat dark area");
        assert_eq!(e[2 * w + 6], 0, "flat bright area");
        assert_eq!(e[2 * w + 3], 255, "next to the step");
        assert_eq!(e[2 * w + 4], 255);
        assert_eq!(e[0], 0, "border");
    }

    #[test]
    fn bilinear_sampling_mixes_neighbours() {
        let mut p = solid(2, 1, [0, 0, 0]);
        p.rgba[4..7].copy_from_slice(&[200, 100, 0]);
        assert_eq!(p.color_at(0.5, 0.5), Some([0.0, 0.0, 0.0]));
        assert_eq!(p.color_at(1.5, 0.5), Some([200.0, 100.0, 0.0]));
        assert_eq!(p.color_at(1.0, 0.5), Some([100.0, 50.0, 0.0]));
        assert_eq!(p.color_at(2.0, 0.5), None);
        assert_eq!(p.color_at(-0.1, 0.5), None);
    }

    #[test]
    fn mixing_matches_the_screen() {
        let pic = solid(64, 48, [0, 0, 200]);
        let place = Alignment::default().placement((160, 120), (64, 48));
        let inside = sample(&pic, place, 80.0, 60.0, Mode::Blend);
        let blend = Overlay::default();
        assert_eq!(mix(&blend, [200, 0, 0], inside), [100, 0, 100]);
        // Where the webcam picture does not reach: the thermal image over black.
        let half = Alignment {
            scale: 0.5,
            ..Default::default()
        };
        let place = half.placement((160, 120), (64, 48));
        let outside = sample(&pic, place, 5.0, 5.0, Mode::Blend);
        assert!(outside.is_none());
        assert_eq!(mix(&blend, [200, 0, 0], outside), [100, 0, 0]);

        let edges = Overlay {
            mode: Mode::Edges,
            edge_strength: 1.0,
            ..Default::default()
        };
        let line = Some(WebcamSample {
            color: [0.0; 3],
            edge: 1.0,
        });
        let flat = Some(WebcamSample {
            color: [0.0; 3],
            edge: 0.0,
        });
        assert_eq!(mix(&edges, [200, 0, 0], line), [255, 255, 255]);
        assert_eq!(mix(&edges, [200, 0, 0], flat), [200, 0, 0]);
    }
}
