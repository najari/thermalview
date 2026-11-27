//! Thermal Master P3 gain study. Looks at one still scene in HIGH, then LOW, then HIGH again,
//! averages frames in each, and fits `LOW raw = a × HIGH raw + b` over all pixels, to find out
//! how LOW pixels relate to temperature. No picture is saved; with `out=<dir>` the three averaged
//! frames are written as raw f32 little-endian (256×192) for further study.
//! Usage: cargo run --release --example p3gain [settle_secs] [shutter] [out=<dir>]
//! With `shutter`, a shutter calibration is triggered after each gain switch.

use std::time::{Duration, Instant};

use thermalview::camera::p3;

const AVERAGE: usize = 50;
const NO_FRAME_LIMIT: Duration = Duration::from_secs(30);

fn celsius(raw: f64) -> f64 {
    raw / 64.0 - 273.15
}

struct Stream {
    dev: p3::Device,
    asm: p3::Assembler,
    buf: Vec<u8>,
}

impl Stream {
    /// Next frame, or an error after [`NO_FRAME_LIMIT`] without one.
    fn frame(&mut self) -> anyhow::Result<Vec<u16>> {
        let t = Instant::now();
        loop {
            let n = self.dev.read(&mut self.buf)?;
            if let Some(raw) = self.asm.push(&self.buf[..n]) {
                return Ok(raw);
            }
            if t.elapsed() > NO_FRAME_LIMIT {
                anyhow::bail!("no frame for {NO_FRAME_LIMIT:?} (camera may need a replug)");
            }
        }
    }

    /// Stream for `secs`, printing frames, drops and temperatures once a second.
    fn settle(&mut self, label: &str, secs: u64) -> anyhow::Result<()> {
        let t0 = Instant::now();
        let mut tick = Instant::now();
        let (mut frames, dropped0) = (0u32, self.asm.dropped);
        let mut last = Vec::new();
        while t0.elapsed() < Duration::from_secs(secs) {
            last = self.frame()?;
            frames += 1;
            if tick.elapsed() >= Duration::from_secs(1) {
                let (mean, mn, mx) = stats(&last);
                println!(
                    "{label:7} {:5.1} s: {frames:2} frames, {:3} dropped, mean {:7.2} °C, \
                     min {:7.2}, max {:7.2} (raw mean {mean:.0})",
                    t0.elapsed().as_secs_f64(),
                    self.asm.dropped - dropped0,
                    celsius(mean),
                    celsius(mn),
                    celsius(mx),
                );
                frames = 0;
                tick = Instant::now();
            }
        }
        let _ = last;
        Ok(())
    }

    fn average(&mut self) -> anyhow::Result<Vec<f64>> {
        self.average_n(AVERAGE)
    }

    fn average_n(&mut self, count: usize) -> anyhow::Result<Vec<f64>> {
        let mut acc = vec![0f64; p3::WIDTH * p3::HEIGHT];
        for _ in 0..count {
            for (a, &v) in acc.iter_mut().zip(&self.frame()?) {
                *a += v as f64;
            }
        }
        acc.iter_mut().for_each(|a| *a /= count as f64);
        Ok(acc)
    }
}

fn stats(raw: &[u16]) -> (f64, f64, f64) {
    let mean = raw.iter().map(|&v| v as f64).sum::<f64>() / raw.len() as f64;
    let (mn, mx) = raw
        .iter()
        .fold((u16::MAX, 0), |(a, b), &v| (a.min(v), b.max(v)));
    (mean, mn as f64, mx as f64)
}

/// Least squares `y = a x + b`: (a, b, r², residual RMS).
fn fit(x: &[f64], y: &[f64]) -> (f64, f64, f64, f64) {
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for (&a, &b) in x.iter().zip(y) {
        sxx += (a - mx) * (a - mx);
        sxy += (a - mx) * (b - my);
        syy += (b - my) * (b - my);
    }
    let a = sxy / sxx;
    let b = my - a * mx;
    let res = x
        .iter()
        .zip(y)
        .map(|(&u, &v)| (v - (a * u + b)).powi(2))
        .sum::<f64>();
    (a, b, 1.0 - res / syy, (res / n).sqrt())
}

fn spread(v: &[f64]) -> (f64, f64) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    (s[s.len() / 100], s[s.len() * 99 / 100])
}

/// `cycles=N`: HIGH, LOW, HIGH, LOW … HIGH with short steps, so slow drift and shutter jumps
/// cancel: each LOW frame is compared with the mean of the HIGH frames before and after it.
fn alternate(mut s: Stream, cycles: usize, settle: u64) -> anyhow::Result<()> {
    const STEP_SETTLE: u64 = 2;
    s.settle("start", settle)?;
    let mut highs = Vec::new();
    let mut lows = Vec::new();
    for i in 0..=cycles {
        s.dev.set_gain(p3::Gain::High)?;
        s.settle("HIGH", STEP_SETTLE)?;
        highs.push(s.average_n(25)?);
        if i == cycles {
            break;
        }
        s.dev.set_gain(p3::Gain::Low)?;
        s.settle("LOW", STEP_SETTLE)?;
        lows.push(s.average_n(25)?);
        println!("cycle {i}: done");
    }
    drop(s);

    let (mut x, mut y) = (Vec::new(), Vec::new());
    for (i, low) in lows.iter().enumerate() {
        let h: Vec<f64> = highs[i]
            .iter()
            .zip(&highs[i + 1])
            .map(|(a, b)| (a + b) / 2.0)
            .collect();
        let jump = highs[i + 1]
            .iter()
            .zip(&highs[i])
            .map(|(a, b)| a - b)
            .sum::<f64>()
            / h.len() as f64;
        let (a, b, r2, rms) = fit(&h, low);
        // The 20 hottest pixels in HIGH, read in both modes.
        let mut idx: Vec<usize> = (0..h.len()).collect();
        idx.sort_by(|&p, &q| h[q].total_cmp(&h[p]));
        let hot = &idx[..20];
        let mean_at = |v: &[f64]| hot.iter().map(|&k| v[k]).sum::<f64>() / hot.len() as f64;
        println!(
            "cycle {i}: LOW = {a:.4} × HIGH + {b:.0} (r² {r2:.4}, residual {rms:.1}); \
             HIGH moved {:.2} K across it; hottest 20 px: HIGH {:.1} °C, LOW {:.1} °C",
            jump / 64.0,
            celsius(mean_at(&h)),
            celsius(mean_at(low))
        );
        x.extend(h);
        y.extend_from_slice(low);
    }
    let (a, b, r2, rms) = fit(&x, &y);
    println!("all     : LOW = {a:.5} × HIGH + {b:.1} (r² {r2:.5}, residual {rms:.1} counts)");
    println!("HIGH °C   pixels   LOW raw   fit error");
    let lo = x.iter().cloned().fold(f64::MAX, f64::min);
    let hi = x.iter().cloned().fold(f64::MIN, f64::max);
    let mut t = (celsius(lo)).floor();
    while t <= celsius(hi) {
        let (r0, r1) = ((t + 273.15) * 64.0, (t + 274.15) * 64.0);
        let (mut n, mut sx, mut sy) = (0usize, 0.0, 0.0);
        for (&u, &v) in x.iter().zip(&y) {
            if u >= r0 && u < r1 {
                n += 1;
                sx += u;
                sy += v;
            }
        }
        if n >= 5 {
            let (mx, my) = (sx / n as f64, sy / n as f64);
            println!(
                "{:6.1}  {n:8}  {my:8.1}  {:+8.1}",
                celsius(mx),
                my - (a * mx + b)
            );
        }
        t += 1.0;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let settle: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let shutter = std::env::args().any(|a| a == "shutter");
    let out = std::env::args().find_map(|a| a.strip_prefix("out=").map(str::to_owned));

    let ctx = rusb::Context::new()?;
    let mut dev = p3::open(&ctx).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let info = dev.info()?;
    println!("device  : {} fw {}", info.model, info.firmware);
    if !dev.start(&mut |d| {
        std::thread::sleep(d);
        true
    })? {
        anyhow::bail!("start interrupted");
    }
    let mut s = Stream {
        dev,
        asm: p3::Assembler::default(),
        buf: vec![0u8; p3::CHUNK],
    };

    if let Some(n) = std::env::args().find_map(|a| a.strip_prefix("cycles=")?.parse().ok()) {
        return alternate(s, n, settle);
    }

    let mut result = Vec::new();
    for (label, gain) in [
        ("HIGH-1", None),
        ("LOW", Some(p3::Gain::Low)),
        ("HIGH-2", Some(p3::Gain::High)),
    ] {
        if let Some(g) = gain {
            s.dev.set_gain(g)?;
            println!("{label:7} gain command sent");
            if shutter {
                s.settle(label, 3)?;
                s.dev.shutter()?;
                println!("{label:7} shutter sent");
            }
        }
        s.settle(label, settle)?;
        let avg = s.average()?;
        let (p1, p99) = spread(&avg);
        println!(
            "{label:7} average of {AVERAGE}: 1st–99th percentile {:.2} … {:.2} °C (raw {p1:.0} … {p99:.0})",
            celsius(p1),
            celsius(p99)
        );
        result.push(avg);
    }
    drop(s);

    let (h1, low, h2) = (&result[0], &result[1], &result[2]);
    let drift = h2.iter().zip(h1).map(|(a, b)| a - b).sum::<f64>() / h1.len() as f64;
    let high: Vec<f64> = h1.iter().zip(h2).map(|(a, b)| (a + b) / 2.0).collect();
    let (a, b, r2, rms) = fit(&high, low);
    println!(
        "drift   : HIGH-2 − HIGH-1 = {:.3} K on average",
        drift / 64.0
    );
    println!("fit     : LOW = {a:.5} × HIGH + {b:.1}  (r² {r2:.5}, residual {rms:.1} counts)");
    if a.abs() > 1e-9 {
        println!(
            "inverse : HIGH = {:.5} × LOW + {:.1}; LOW raw for 0 K would be {b:.1}",
            1.0 / a,
            -b / a
        );
    }
    if let Some(dir) = out {
        for (name, v) in [("high1", h1), ("low", low), ("high2", h2)] {
            let bytes: Vec<u8> = v.iter().flat_map(|&x| (x as f32).to_le_bytes()).collect();
            std::fs::write(format!("{dir}/{name}.f32"), bytes)?;
        }
        println!("written : {dir}/{{high1,low,high2}}.f32");
    }
    Ok(())
}
