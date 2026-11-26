//! Thermal Master P3 check: open the camera, stream for a few seconds and report the device
//! information, frame rate, dropped frames and temperatures. No picture is saved.
//! Usage: cargo run --release --example p3 [seconds] [shutter]
//! With `shutter`, a shutter calibration is triggered halfway (the P3 has its own shutter).

use std::time::{Duration, Instant};

use thermalview::camera::p3;

fn celsius(raw: u16) -> f32 {
    raw as f32 * p3::KELVIN_PER_COUNT - 273.15
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let shutter = std::env::args().any(|a| a == "shutter");

    let ctx = rusb::Context::new()?;
    let t = Instant::now();
    let mut dev = p3::open(&ctx).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let info = dev.info()?;
    println!(
        "device   : {} fw {} sn {} (opened in {:?})",
        info.model,
        info.firmware,
        info.serial,
        t.elapsed()
    );

    let t = Instant::now();
    if !dev.start(&mut |d| {
        std::thread::sleep(d);
        true
    })? {
        anyhow::bail!("start interrupted");
    }
    dev.set_gain(p3::Gain::High)?;
    println!("started  : {:?}", t.elapsed());

    let mut asm = p3::Assembler::default();
    let mut buf = vec![0u8; p3::CHUNK];
    let mut frames = 0u64;
    let mut first: Option<Instant> = None;
    let mut last: Option<(Instant, Vec<u16>)> = None;
    let mut worst_gap = Duration::ZERO;
    let mut reads = 0u64;
    let mut empty = 0u64;
    let mut shutter_sent = false;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if shutter && !shutter_sent && t0.elapsed() >= Duration::from_secs(secs / 2) {
            dev.shutter()?;
            shutter_sent = true;
            println!("shutter  : sent at {:.1} s", t0.elapsed().as_secs_f64());
        }
        let n = dev.read(&mut buf)?;
        reads += 1;
        if n == 0 {
            empty += 1;
        }
        if let Some(raw) = asm.push(&buf[..n]) {
            let now = Instant::now();
            if let Some((prev, _)) = &last {
                worst_gap = worst_gap.max(now - *prev);
            }
            first.get_or_insert(now);
            frames += 1;
            last = Some((now, raw));
        }
    }
    drop(dev);

    let Some((end, raw)) = last else {
        anyhow::bail!("no frame in {secs} s ({reads} reads, {empty} empty)");
    };
    let span = (end - first.unwrap_or(end)).as_secs_f64();
    println!(
        "frames   : {frames} in {span:.1} s -> {:.2} fps, {} dropped, longest gap {:.0} ms",
        (frames.saturating_sub(1)) as f64 / span.max(1e-9),
        asm.dropped,
        worst_gap.as_secs_f64() * 1000.0
    );
    println!("reads    : {reads} ({empty} timed out)");
    let (mn, mx) = raw
        .iter()
        .fold((u16::MAX, 0), |(a, b), &v| (a.min(v), b.max(v)));
    let center = raw[(p3::HEIGHT / 2) * p3::WIDTH + p3::WIDTH / 2];
    let mean = raw.iter().map(|&v| v as f64).sum::<f64>() / raw.len() as f64;
    println!(
        "last     : min {:.1} °C, max {:.1} °C, mean {:.1} °C, center {:.1} °C",
        celsius(mn),
        celsius(mx),
        mean * p3::KELVIN_PER_COUNT as f64 - 273.15,
        celsius(center)
    );
    Ok(())
}
