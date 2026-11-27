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
    let reset = std::env::args().any(|a| a == "reset");
    // "low"/"high": switch the gain halfway instead of a shutter cycle.
    let gain_mid = std::env::args().find_map(|a| match a.as_str() {
        "low" => Some(p3::Gain::Low),
        "high" => Some(p3::Gain::High),
        _ => None,
    });

    let ctx = rusb::Context::new()?;
    let t = Instant::now();
    let mut dev = p3::open(&ctx).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    println!(
        "log      : {}",
        dev.debug_log().unwrap_or_else(|e| e.to_string())
    );
    if reset {
        dev.reset()?;
        println!("reset    : done");
        std::thread::sleep(Duration::from_secs(2));
        dev = p3::open(&ctx).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    }
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
    if !std::env::args().any(|a| a == "nogain") {
        dev.set_gain(p3::Gain::High)?;
    }
    println!("started  : {:?}", t.elapsed());
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "log      : {}",
        dev.debug_log().unwrap_or_else(|e| e.to_string())
    );

    let mut asm = p3::Assembler::default();
    let mut buf = vec![0u8; p3::CHUNK];
    let mut frames = 0u64;
    let mut first: Option<Instant> = None;
    let mut last: Option<(Instant, Vec<u16>)> = None;
    let mut worst_gap = Duration::ZERO;
    let mut reads = 0u64;
    let mut empty = 0u64;
    let mut shutter_sent = false;
    let mut sizes: Vec<(usize, [u8; 2])> = Vec::new();
    let mut dump = std::env::args().find_map(|a| a.strip_prefix("dump=").map(str::to_owned));
    let mut transfer: Vec<u8> = Vec::new();
    let mut transfer_done = true;
    // `trace=<file>`: every read's size and first two bytes, one per line.
    let mut trace = std::env::args()
        .find_map(|a| a.strip_prefix("trace=").map(str::to_owned))
        .map(std::fs::File::create)
        .transpose()?;
    let mut first_drop: Option<Vec<(usize, [u8; 2])>> = None;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if (shutter || gain_mid.is_some())
            && !shutter_sent
            && t0.elapsed() >= Duration::from_secs(secs / 2)
        {
            match gain_mid {
                Some(g) => dev.set_gain(g)?,
                None => dev.shutter()?,
            }
            shutter_sent = true;
            println!(
                "halfway  : {:?} sent at {:.1} s",
                gain_mid,
                t0.elapsed().as_secs_f64()
            );
        }
        let n = dev.read(&mut buf)?;
        reads += 1;
        if n == 0 {
            empty += 1;
        }
        // Read sizes up to and including the first dropped frame, for diagnosing desync.
        if asm.dropped == 0 || sizes.len() < 60 {
            if sizes.len() >= 60 {
                sizes.remove(0);
            }
            sizes.push((n, buf.get(..2).map(|b| [b[0], b[1]]).unwrap_or_default()));
        }
        // Keep the bytes of the current USB transfer; save the first odd-sized one (dump=<file>).
        if n > 0 {
            if transfer_done {
                transfer.clear();
            }
            transfer.extend_from_slice(&buf[..n]);
            transfer_done = n < p3::CHUNK;
            if transfer_done && transfer.len() == 204_800 {
                if let Some(path) = dump.take() {
                    std::fs::write(&path, &transfer)?;
                    println!("dumped   : 204,800-byte transfer to {path}");
                }
            }
        }
        if let Some(f) = trace.as_mut() {
            use std::io::Write;
            let b = buf.get(..2).unwrap_or_default();
            writeln!(f, "{n} {:02x}{:02x}", b[0], b[1])?;
        }
        let before = asm.dropped;
        let got = asm.push(&buf[..n]);
        if asm.dropped > before && first_drop.is_none() {
            first_drop = Some(sizes.clone());
        }
        if let Some(raw) = got {
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
    if let Some(s) = &first_drop {
        let line: Vec<String> = s
            .iter()
            .map(|(n, b)| format!("{n}:{:02x}{:02x}", b[0], b[1]))
            .collect();
        println!(
            "1st drop : reads before it (size:first bytes) {}",
            line.join(" ")
        );
    }

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
    println!(
        "dropped  : size {}, counter {}, too long {}; {} reads skipped",
        asm.wrong_size, asm.mismatch, asm.too_long, asm.skipped
    );
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
