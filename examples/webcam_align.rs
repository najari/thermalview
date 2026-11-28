//! Runs the automatic webcam alignment on a snapshot pair and writes the result as a picture.
//! Usage: cargo run --release --example webcam_align <snapshot>_raw16.png <snapshot>_webcam.png <out.png>
//! The output is the webcam picture with the thermal image (gray, hot = bright) blended over
//! it at the found place, at 4× the thermal size.

use std::time::Instant;

use thermalview::webcam::align;
use thermalview::webcam::blend::{self, Alignment, Overlay, Picture};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4 || args.len() == 7,
        "usage: webcam_align <raw16.png> <webcam.png> <out.png> [scale dx dy to compare]"
    );
    let raw = image::open(&args[1])?.into_luma16();
    let (tw, th) = (raw.width() as usize, raw.height() as usize);
    let thermal: Vec<f32> = raw.pixels().map(|p| p.0[0] as f32).collect();
    let cam = image::open(&args[2])?.into_rgba8();
    let (pw, ph) = (cam.width() as usize, cam.height() as usize);
    let rgba = cam.into_raw();

    let t = Instant::now();
    let found = align::find(&thermal, tw, th, &rgba, pw, ph);
    println!("search took {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let Some(f) = found else {
        anyhow::bail!("nothing to compare");
    };
    println!(
        "scale {:.3}, dx {:+.3}, dy {:+.3}, score {:.3} ({})",
        f.align.scale,
        f.align.dx,
        f.align.dy,
        f.score,
        if f.trusted() {
            "trusted"
        } else {
            "not trusted"
        }
    );
    if args.len() == 7 {
        let a = Alignment {
            scale: args[4].parse()?,
            dx: args[5].parse()?,
            dy: args[6].parse()?,
        };
        let s = align::score_at(&thermal, tw, th, &rgba, pw, ph, &a);
        println!("given {a:?}: score {s:?}");
    }

    let (lo, hi) = thermal
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let pic = Picture::new(0, pw, ph, rgba);
    let overlay = Overlay {
        align: f.align,
        ..Default::default()
    };
    let place = f.align.placement((tw, th), (pw, ph));
    const ZOOM: usize = 4;
    let mut out = image::RgbImage::new((tw * ZOOM) as u32, (th * ZOOM) as u32);
    for (x, y, px) in out.enumerate_pixels_mut() {
        let (tx, ty) = (
            (x as f32 + 0.5) / ZOOM as f32,
            (y as f32 + 0.5) / ZOOM as f32,
        );
        let v = thermal[(ty as usize).min(th - 1) * tw + (tx as usize).min(tw - 1)];
        let g = ((v - lo) / (hi - lo).max(1.0) * 255.0) as u8;
        let s = blend::sample(&pic, place, tx, ty, overlay.mode);
        px.0 = blend::mix(&overlay, [g, g / 2, 0], s);
    }
    out.save(&args[3])?;
    println!("written {}", args[3]);
    Ok(())
}
