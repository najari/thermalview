//! The picture recorded to MP4: the colored image enlarged, with the screen's markers and labels,
//! a color bar and a caption burned in. Pure code (no Windows calls), unit-tested.

use std::sync::Arc;

use ab_glyph::{point, Font as _, FontVec, PxScale, ScaleFont as _};

use crate::webcam::blend::{self, Overlay};
use crate::webcam::Picture;

/// Each thermal pixel becomes a SCALE×SCALE block (nearest neighbour, as on screen).
pub const SCALE: usize = 4;
/// Room right of the image for the color bar and its labels.
const BAR_AREA: usize = 96;
const FONT_PX: f32 = 15.0;

pub type Rgb = [u8; 3];
const BLACK: Rgb = [0, 0, 0];
pub const WHITE: Rgb = [255, 255, 255];
pub const MAX_COLOR: Rgb = [255, 60, 60];
pub const MIN_COLOR: Rgb = [80, 160, 255];
pub const SHAPE_COLOR: Rgb = [120, 255, 120];

/// A marker drawn over the image, in thermal pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    /// Hottest / coldest pixel.
    Cross {
        at: (usize, usize),
        text: String,
        color: Rgb,
    },
    Spot {
        at: (usize, usize),
        text: String,
    },
    Box {
        a: (usize, usize),
        b: (usize, usize),
        text: String,
    },
    Line {
        a: (usize, usize),
        b: (usize, usize),
        text: String,
    },
    /// Hottest pixel of a Box.
    Peak {
        at: (usize, usize),
    },
}

/// The isotherm band, marked beside the color bar. An open end is infinite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandMark {
    pub lo: f32,
    pub hi: f32,
    pub color: Rgb,
}

/// Everything needed to draw one video frame.
#[derive(Debug, Clone)]
pub struct Scene {
    pub width: usize,
    pub height: usize,
    /// The colored image, width × height RGBA.
    pub rgba: Vec<u8>,
    /// Palette; the top of the color bar is the last entry.
    pub lut: Vec<Rgb>,
    /// Temperatures at the bottom and top of the color bar.
    pub range: (f32, f32),
    /// Isotherm in use; its pixels are already painted in `rgba`.
    pub band: Option<BandMark>,
    pub unit: &'static str,
    /// Top-left text (time, calibration).
    pub caption: String,
    pub marks: Vec<Mark>,
    /// Webcam picture mixed in as on screen.
    pub webcam: Option<WebcamLayer>,
    /// The upscaled image (SCALE·width × SCALE·height RGBA) when the view shows one; drawn
    /// instead of enlarging `rgba`.
    pub upscaled: Option<Vec<u8>>,
}

// The upscaled view image fills the video's image area exactly.
const _: () = assert!(SCALE == crate::thermal::upscale::FACTOR);

#[derive(Debug, Clone)]
pub struct WebcamLayer {
    pub picture: Arc<Picture>,
    pub overlay: Overlay,
}

/// Video frame size for a thermal image. H.264 works in 16×16 blocks.
pub fn canvas_size(width: usize, height: usize) -> (usize, usize) {
    (
        (width * SCALE + BAR_AREA).next_multiple_of(16),
        (height * SCALE).next_multiple_of(16),
    )
}

/// An RGB image.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgb: vec![0; width * height * 3],
        }
    }

    pub fn get(&self, x: usize, y: usize) -> Rgb {
        let i = (y * self.width + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let inside = x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height;
        inside.then(|| (y as usize * self.width + x as usize) * 3)
    }

    fn put(&mut self, x: i32, y: i32, c: Rgb) {
        if let Some(i) = self.index(x, y) {
            self.rgb[i..i + 3].copy_from_slice(&c);
        }
    }

    /// Mix `c` over the pixel with opacity `a` (0..=1).
    fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if let Some(i) = self.index(x, y) {
            let a = a.clamp(0.0, 1.0);
            for k in 0..3 {
                let old = self.rgb[i + k] as f32;
                self.rgb[i + k] = (old + (c[k] as f32 - old) * a).round() as u8;
            }
        }
    }

    /// Fill `[x0, x1) × [y0, y1)`.
    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, c);
            }
        }
    }

    /// Darken `[x0, x1) × [y0, y1)` like a translucent black backdrop.
    fn shade(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: f32) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, BLACK, a);
            }
        }
    }

    /// Straight line drawn with a square brush `width` pixels wide.
    fn line(&mut self, a: (i32, i32), b: (i32, i32), width: i32, c: Rgb) {
        let lo = -(width / 2);
        let hi = lo + width;
        let (mut x, mut y) = a;
        let (dx, dy) = ((b.0 - a.0).abs(), -(b.1 - a.1).abs());
        let (sx, sy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
        let mut err = dx + dy;
        loop {
            self.fill(x + lo, y + lo, x + hi, y + hi, c);
            if (x, y) == b {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Circle outline of radius `r` (to the middle of the stroke) around pixel center `c`.
    fn ring(&mut self, c: (i32, i32), r: f32, width: f32, col: Rgb) {
        let reach = (r + width).ceil() as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                if (d - r).abs() <= width / 2.0 {
                    self.put(c.0 + dx, c.1 + dy, col);
                }
            }
        }
    }

    fn disc(&mut self, c: (i32, i32), r: f32, col: Rgb) {
        let reach = r.ceil() as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                if ((dx * dx + dy * dy) as f32).sqrt() <= r {
                    self.put(c.0 + dx, c.1 + dy, col);
                }
            }
        }
    }
}

/// Text drawing with a Windows font. Without a font, text is simply left out.
pub struct Text {
    font: Option<FontVec>,
}

impl Text {
    /// Malgun Gothic (as the UI; has Hangul and arrows), else Segoe UI or Arial.
    pub fn load() -> Self {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let fonts = std::path::Path::new(&windir).join("Fonts");
        let font = ["malgun.ttf", "segoeui.ttf", "arial.ttf"]
            .iter()
            .find_map(|name| {
                let bytes = std::fs::read(fonts.join(name)).ok()?;
                FontVec::try_from_vec(bytes).ok()
            });
        if font.is_none() {
            log::warn!("MP4: no font found in {}; labels left out", fonts.display());
        }
        Self { font }
    }

    pub fn none() -> Self {
        Self { font: None }
    }

    /// Width and height in pixels; (0, 0) without a font.
    fn size(&self, s: &str) -> (i32, i32) {
        let Some(font) = &self.font else {
            return (0, 0);
        };
        let f = font.as_scaled(PxScale::from(FONT_PX));
        let mut w = 0.0;
        let mut prev = None;
        for ch in s.chars() {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                w += f.kern(p, id);
            }
            w += f.h_advance(id);
            prev = Some(id);
        }
        (w.ceil() as i32, (f.ascent() - f.descent()).ceil() as i32)
    }

    /// Draw `s` with its top-left corner at (x, y).
    fn draw(&self, cv: &mut Canvas, x: i32, y: i32, s: &str, c: Rgb) {
        let Some(font) = &self.font else {
            return;
        };
        let scale = PxScale::from(FONT_PX);
        let f = font.as_scaled(scale);
        let baseline = y as f32 + f.ascent();
        let mut caret = x as f32;
        let mut prev = None;
        for ch in s.chars() {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                caret += f.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, point(caret, baseline));
            caret += f.h_advance(id);
            prev = Some(id);
            if let Some(g) = font.outline_glyph(glyph) {
                let bb = g.px_bounds();
                g.draw(|gx, gy, cov| {
                    cv.blend(
                        bb.min.x as i32 + gx as i32,
                        bb.min.y as i32 + gy as i32,
                        c,
                        cov,
                    )
                });
            }
        }
    }

    /// Text on a dark backdrop at `anchor + offset`, flipped to the other side of the anchor
    /// where it would leave `bounds` (x0, y0, x1, y1). Same placement as the screen.
    fn label(
        &self,
        cv: &mut Canvas,
        anchor: (i32, i32),
        offset: (i32, i32),
        bounds: (i32, i32, i32, i32),
        s: &str,
        c: Rgb,
    ) {
        let (w, h) = self.size(s);
        if w == 0 {
            return;
        }
        let mut x = anchor.0 + offset.0;
        let mut y = anchor.1 + offset.1;
        if x + w > bounds.2 {
            x = anchor.0 - offset.0.abs() - w;
        }
        if y < bounds.1 {
            y = anchor.1 + offset.1.abs();
        }
        if y + h > bounds.3 {
            y = anchor.1 - offset.1.abs() - h;
        }
        x = x.max(bounds.0);
        cv.shade(x - 2, y - 2, x + w + 2, y + h + 2, 160.0 / 255.0);
        self.draw(cv, x, y, s, c);
    }
}

fn cross(cv: &mut Canvas, c: (i32, i32), r: i32, color: Rgb) {
    for (width, col) in [(3, BLACK), (1, color)] {
        cv.line((c.0 - r, c.1), (c.0 + r, c.1), width, col);
        cv.line((c.0, c.1 - r), (c.0, c.1 + r), width, col);
    }
}

/// Draw one video frame.
pub fn compose(scene: &Scene, text: &Text) -> Canvas {
    let (cw, ch) = canvas_size(scene.width, scene.height);
    let mut cv = Canvas::new(cw, ch);
    let (iw, ih) = (scene.width * SCALE, scene.height * SCALE);
    if let Some(up) = scene.upscaled.as_ref().filter(|u| u.len() == iw * ih * 4) {
        for y in 0..ih {
            for x in 0..iw {
                let (s, d) = ((y * iw + x) * 4, (y * cw + x) * 3);
                cv.rgb[d..d + 3].copy_from_slice(&up[s..s + 3]);
            }
        }
    } else if scene.rgba.len() == scene.width * scene.height * 4 {
        for y in 0..ih {
            let row = (y / SCALE) * scene.width;
            for x in 0..iw {
                let s = (row + x / SCALE) * 4;
                let d = (y * cw + x) * 3;
                cv.rgb[d..d + 3].copy_from_slice(&scene.rgba[s..s + 3]);
            }
        }
    }
    if let Some(layer) = &scene.webcam {
        mix_webcam(&mut cv, scene, layer);
    }
    let bounds = (0, 0, iw as i32, ih as i32);
    let center = |(x, y): (usize, usize)| {
        (
            (x * SCALE + SCALE / 2) as i32,
            (y * SCALE + SCALE / 2) as i32,
        )
    };

    for m in &scene.marks {
        match m {
            Mark::Cross { at, text: s, color } => {
                let c = center(*at);
                cross(&mut cv, c, 7, *color);
                text.label(&mut cv, c, (9, -9), bounds, s, *color);
            }
            Mark::Spot { at, text: s } => {
                let c = center(*at);
                let r = (1.5 * SCALE as f32).max(3.0);
                cv.ring(c, r, 3.0, BLACK);
                cv.ring(c, r, 1.2, WHITE);
                text.label(&mut cv, c, (8, 6), bounds, s, WHITE);
            }
            Mark::Box { a, b, text: s } => {
                let x0 = (a.0.min(b.0) * SCALE) as i32;
                let y0 = (a.1.min(b.1) * SCALE) as i32;
                let x1 = ((a.0.max(b.0) + 1) * SCALE) as i32;
                let y1 = ((a.1.max(b.1) + 1) * SCALE) as i32;
                for (width, col) in [(3, BLACK), (1, SHAPE_COLOR)] {
                    cv.line((x0, y0), (x1, y0), width, col);
                    cv.line((x1, y0), (x1, y1), width, col);
                    cv.line((x1, y1), (x0, y1), width, col);
                    cv.line((x0, y1), (x0, y0), width, col);
                }
                text.label(&mut cv, (x0, y0), (2, -18), bounds, s, SHAPE_COLOR);
            }
            Mark::Line { a, b, text: s } => {
                let (pa, pb) = (center(*a), center(*b));
                cv.line(pa, pb, 3, BLACK);
                cv.line(pa, pb, 1, SHAPE_COLOR);
                for p in [pa, pb] {
                    cv.disc(p, 3.0, SHAPE_COLOR);
                }
                text.label(&mut cv, pa, (2, -18), bounds, s, SHAPE_COLOR);
            }
            Mark::Peak { at } => cross(&mut cv, center(*at), 4, SHAPE_COLOR),
        }
    }

    // Color bar: hottest at the top.
    let bx = iw as i32 + 12;
    let bar_w = 16;
    if !scene.lut.is_empty() && ih > 1 {
        let top = scene.lut.len() - 1;
        for y in 0..ih {
            let t = y as f32 / (ih - 1) as f32;
            let c = scene.lut[((1.0 - t) * top as f32).round() as usize];
            cv.fill(bx, y as i32, bx + bar_w, y as i32 + 1, c);
        }
    }
    let (lo, hi) = scene.range;
    // The isotherm band: a strip in its color along the part of the bar it covers.
    if let (Some(band), true) = (scene.band, hi > lo && ih > 1) {
        let y_of =
            |v: f32| (((hi - v) / (hi - lo)).clamp(0.0, 1.0) * (ih - 1) as f32).round() as i32;
        let (top, bottom) = (y_of(band.hi), y_of(band.lo));
        if bottom > top {
            cv.fill(bx - 7, top, bx - 2, bottom + 1, band.color);
        }
    }
    for k in 0..=4 {
        let t = k as f32 / 4.0;
        let v = hi - (hi - lo) * t;
        let y = (t * (ih - 1) as f32).round() as i32;
        cv.fill(bx + bar_w, y, bx + bar_w + 4, y + 1, WHITE);
        let s = format!("{v:.1}{}", scene.unit);
        let (_, h) = text.size(&s);
        let ty = (y - h / 2).clamp(0, ih as i32 - h);
        text.draw(&mut cv, bx + bar_w + 6, ty, &s, WHITE);
    }

    if !scene.caption.is_empty() {
        text.label(&mut cv, (0, 0), (6, 4), bounds, &scene.caption, WHITE);
    }
    cv
}

/// Mix the webcam picture into the enlarged thermal image, as the screen does.
fn mix_webcam(cv: &mut Canvas, scene: &Scene, layer: &WebcamLayer) {
    let pic = &layer.picture;
    let place = layer
        .overlay
        .align
        .placement((scene.width, scene.height), (pic.width, pic.height));
    for y in 0..scene.height * SCALE {
        let ty = (y as f32 + 0.5) / SCALE as f32;
        for x in 0..scene.width * SCALE {
            let tx = (x as f32 + 0.5) / SCALE as f32;
            let thermal = cv.get(x, y);
            let cam = blend::sample(pic, place, tx, ty, layer.overlay.mode);
            let i = (y * cv.width + x) * 3;
            cv.rgb[i..i + 3].copy_from_slice(&blend::mix(&layer.overlay, thermal, cam));
        }
    }
}

/// RGB → NV12 (BT.601, limited range): a full-size Y plane, then interleaved U/V at half size.
/// Width and height must be even.
pub fn to_nv12(cv: &Canvas, out: &mut Vec<u8>) {
    let (w, h) = (cv.width, cv.height);
    out.clear();
    out.resize(w * h * 3 / 2, 0);
    let (luma, chroma) = out.split_at_mut(w * h);
    for (i, px) in cv.rgb.as_chunks::<3>().0.iter().enumerate() {
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        luma[i] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let c = cv.get(x + dx, y + dy);
                r += c[0] as i32;
                g += c[1] as i32;
                b += c[2] as i32;
            }
            let (r, g, b) = ((r + 2) / 4, (g + 2) / 4, (b + 2) / 4);
            let i = (y / 2) * w + x;
            chroma[i] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            chroma[i + 1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Scene {
        let mut rgba = vec![0u8; 160 * 120 * 4];
        // Pixel (10, 20) is pure red.
        let i = (20 * 160 + 10) * 4;
        rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        Scene {
            width: 160,
            height: 120,
            rgba,
            lut: (0..=255u8).map(|v| [v, v, v]).collect(),
            range: (20.0, 40.0),
            band: None,
            unit: "°C",
            caption: "2026-11-25 10:00:00".into(),
            marks: vec![],
            webcam: None,
            upscaled: None,
        }
    }

    #[test]
    fn size_is_whole_macroblocks() {
        assert_eq!(canvas_size(160, 120), (736, 480));
    }

    #[test]
    fn image_is_enlarged_by_whole_blocks() {
        let cv = compose(&scene(), &Text::none());
        for (x, y) in [(40, 80), (43, 83)] {
            assert_eq!(cv.get(x, y), [255, 0, 0]);
        }
        assert_eq!(cv.get(44, 80), [0, 0, 0]);
        assert_eq!(cv.get(39, 80), [0, 0, 0]);
    }

    #[test]
    fn color_bar_runs_from_hot_at_the_top() {
        let cv = compose(&scene(), &Text::none());
        let x = 640 + 12 + 8;
        assert_eq!(cv.get(x, 0), [255, 255, 255]);
        assert_eq!(cv.get(x, 479), [0, 0, 0]);
    }

    #[test]
    fn isotherm_band_is_marked_beside_the_bar() {
        let mut s = scene();
        let color = [0, 235, 70];
        // 30 °C and above, on a 20–40 °C bar: the upper half.
        s.band = Some(BandMark {
            lo: 30.0,
            hi: f32::INFINITY,
            color,
        });
        let cv = compose(&s, &Text::none());
        let x = 640 + 12 - 4;
        assert_eq!(cv.get(x, 0), color);
        assert_eq!(cv.get(x, 230), color);
        assert_eq!(cv.get(x, 250), [0, 0, 0]);
        assert_eq!(cv.get(x, 479), [0, 0, 0]);
        // A band outside the bar's range marks nothing.
        s.band = Some(BandMark {
            lo: 50.0,
            hi: 60.0,
            color,
        });
        let cv = compose(&s, &Text::none());
        assert!((0..480).all(|y| cv.get(x, y) == [0, 0, 0]));
        // The hottest pixel of a box gets a small cross.
        s.marks = vec![Mark::Peak { at: (100, 60) }];
        let cv = compose(&s, &Text::none());
        assert_eq!(cv.get(402, 242), SHAPE_COLOR);
        assert_eq!(cv.get(402 + 4, 242), SHAPE_COLOR);
        assert_eq!(cv.get(402 + 8, 242), [0, 0, 0]);
    }

    #[test]
    fn markers_are_drawn_around_their_pixel() {
        let mut s = scene();
        s.marks = vec![
            Mark::Spot {
                at: (100, 60),
                text: String::new(),
            },
            Mark::Box {
                a: (10, 100),
                b: (20, 110),
                text: String::new(),
            },
        ];
        let cv = compose(&s, &Text::none());
        // Spot ring: radius 6 around the block center (402, 242).
        assert_eq!(cv.get(408, 242), WHITE);
        assert_eq!(cv.get(402, 242), [0, 0, 0], "center left clear");
        // Box outline on the outer edge of its pixels: x 40..84, y 400..444.
        assert_eq!(cv.get(60, 400), SHAPE_COLOR);
        assert_eq!(cv.get(84, 420), SHAPE_COLOR);
    }

    #[test]
    fn nv12_uses_bt601_limited_range() {
        let mut cv = Canvas::new(4, 2);
        for x in 0..2 {
            for y in 0..2 {
                cv.put(x, y, WHITE);
                cv.put(x + 2, y, [255, 0, 0]);
            }
        }
        let mut out = Vec::new();
        to_nv12(&cv, &mut out);
        assert_eq!(out.len(), 12);
        assert_eq!(&out[..4], &[235, 235, 82, 82]);
        // U, V of the white block, then of the red one.
        assert_eq!(&out[8..], &[128, 128, 90, 240]);
    }

    #[test]
    fn text_draws_when_a_font_exists() {
        let text = Text::load();
        if text.font.is_none() {
            return; // no Windows fonts on this machine
        }
        let mut s = scene();
        s.caption = "S1 33.5°C ↑".into();
        let plain = compose(&scene(), &Text::none());
        let with_text = compose(&s, &text);
        assert_ne!(plain.rgb, with_text.rgb);
    }
}
