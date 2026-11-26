//! Webcam check: list the cameras, open one for a few seconds and report size, frame rate and
//! picture statistics. No picture is saved or shown.
//! Usage: cargo run --release --example webcam [seconds] [device index]

use std::time::{Duration, Instant};

use thermalview::webcam::{list_devices, State, Webcam};

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let index: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let t = Instant::now();
    let devices = list_devices()?;
    println!("devices  : {} (listed in {:?})", devices.len(), t.elapsed());
    for (i, d) in devices.iter().enumerate() {
        println!("  [{i}] {}\n      {}", d.name, d.link);
    }
    let Some(dev) = devices.get(index) else {
        anyhow::bail!("no device #{index}");
    };

    let t = Instant::now();
    let cam = Webcam::open(&dev.link, || {});
    let mut first = None;
    let mut last_seq = 0;
    let mut frames = 0u64;
    while t.elapsed() < Duration::from_secs(secs) {
        match cam.state() {
            State::Failed(e) => anyhow::bail!("{e}"),
            State::Opening => {}
            State::Running { .. } => {
                if let Some(p) = cam.latest() {
                    if p.seq != last_seq {
                        frames += p.seq - last_seq;
                        last_seq = p.seq;
                        first.get_or_insert((t.elapsed(), p.seq));
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let state = cam.state();
    let picture = cam.latest();
    let t_stop = Instant::now();
    drop(cam);
    println!("state    : {state:?}");
    if let (Some((t0, seq0)), Some(p)) = (first, picture) {
        let fps = (p.seq - seq0) as f64 / (t.elapsed() - t0).as_secs_f64();
        let mean = |k: usize| {
            p.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| px[k] as f64)
                .sum::<f64>()
                / (p.width * p.height) as f64
        };
        let edge_px = p.edges.iter().filter(|e| **e > 128).count();
        println!("first    : {t0:?} after open");
        println!(
            "frames   : {frames} ({fps:.1} fps), {}×{}",
            p.width, p.height
        );
        let brightest = p
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[0].max(px[1]).max(px[2]))
            .max();
        println!(
            "mean RGB : {:.0} {:.0} {:.0}, brightest {}; strong edges {:.1}% of pixels",
            mean(0),
            mean(1),
            mean(2),
            brightest.unwrap_or(0),
            edge_px as f64 * 100.0 / p.edges.len() as f64
        );
    }
    println!("stopped  : {:?}", t_stop.elapsed());
    Ok(())
}
