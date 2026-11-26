//! Upscaling comparison: read a snapshot's 16-bit PNG and write one image with the same frame
//! four times: sensor pixels enlarged, then 4× upscaling without, with medium and with strong
//! sharpening. Also prints the time per frame of each.
//! Usage: cargo run --release --example upscale <file_raw16.png> [out.png]

use std::time::Instant;

use thermalview::thermal::palette::{self, Palette};
use thermalview::thermal::upscale::{Sharpen, Upscaler, FACTOR};

fn main() -> anyhow::Result<()> {
    let input = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: upscale <file_raw16.png> [out.png]"))?;
    let output = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "upscale_compare.png".into());
    let img = image::open(&input)?.into_luma16();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw = img.into_raw();
    // Color range: 1st to 99th percentile, as thermal-upscale does.
    let mut sorted = raw.clone();
    sorted.sort_unstable();
    let (lo, hi) = (sorted[sorted.len() / 100], sorted[sorted.len() * 99 / 100]);
    let lut = Palette::Ironbow.lut();

    let (ow, oh) = (w * FACTOR, h * FACTOR);
    let gap = 8;
    let mut sheet = image::RgbImage::new((ow * 4 + gap * 3) as u32, oh as u32);
    let mut put = |slot: usize, rgba: &[u8]| {
        let x0 = slot * (ow + gap);
        for y in 0..oh {
            for x in 0..ow {
                let s = (y * ow + x) * 4;
                sheet.put_pixel(
                    (x0 + x) as u32,
                    y as u32,
                    image::Rgb([rgba[s], rgba[s + 1], rgba[s + 2]]),
                );
            }
        }
    };

    // Sensor pixels, nearest-neighbour.
    let mut native = Vec::new();
    palette::colorize(&raw, lo, hi, &lut, &mut native);
    let mut nearest = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            let s = ((y / FACTOR) * w + x / FACTOR) * 4;
            nearest[(y * ow + x) * 4..][..4].copy_from_slice(&native[s..s + 4]);
        }
    }
    put(0, &nearest);

    for (slot, s) in Sharpen::ALL.into_iter().enumerate() {
        let mut u = Upscaler::new(s);
        let _ = u.upscale(&raw, w, h); // build the tables
        let t = Instant::now();
        let runs = 20;
        let mut field = Vec::new();
        for _ in 0..runs {
            field = u.upscale(&raw, w, h);
        }
        println!(
            "{:14}: {:.2} ms per frame",
            s.label(),
            t.elapsed().as_secs_f64() * 1000.0 / runs as f64
        );
        let mut rgba = Vec::new();
        palette::colorize_f32(&field, lo, hi, &lut, &mut rgba);
        put(slot + 1, &rgba);
    }
    sheet.save(&output)?;
    println!("{w}×{h} → {ow}×{oh}; wrote {output} (sensor pixels | off | medium | strong)");
    Ok(())
}
