//! Frame-gap check: stream for a while, timestamp every frame in the libuvc callback and report
//! the gaps between frames, with the time since the last Lepton status poll.
//! Usage: cargo run --release --example gaps [seconds] [poll_ms]
//! `poll_ms` > 0 sends the app's status poll (FPA temperature + shutter-mode object) that often;
//! 0 sends no control requests while streaming.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thermalview::camera::{lepton, uvc, HEIGHT, PID, VID, WIDTH};

/// A gap at least this long counts as a missing frame or more (normal spacing is ~114 ms).
const GAP: Duration = Duration::from_millis(200);

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let poll_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let ctx = uvc::Context::new()?;
    let dev = ctx.find_device(VID, PID)?;
    let mut h = dev.open()?;
    println!("FPA      : {:.2} C", lepton::fpa_temp_c(&h)?);

    // (arrival, good?, libuvc sequence) for every callback. libuvc hands the callback only the
    // newest assembled frame, so a sequence jump means frames were assembled but skipped here.
    let arrivals = Arc::new(Mutex::new(Vec::<(Instant, bool, u32)>::with_capacity(
        secs as usize * 10 + 100,
    )));
    {
        let arrivals = arrivals.clone();
        h.start_stream(uvc::FORMAT_Y16, WIDTH as u32, HEIGHT as u32, move |f| {
            let good = f.data.len() == WIDTH * HEIGHT * 2 && f.format == uvc::FORMAT_Y16;
            if let Ok(mut a) = arrivals.lock() {
                a.push((Instant::now(), good, f.sequence));
            }
        })?;
    }

    let t0 = Instant::now();
    let mut polls: Vec<(Instant, Duration)> = Vec::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        if poll_ms == 0 {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
        let tp = Instant::now();
        let _ = lepton::fpa_temp_c(&h);
        let _ = lepton::get_shutter_mode_obj(&h);
        polls.push((tp, tp.elapsed()));
    }
    h.stop_stream();
    drop(h);

    let a = arrivals.lock().unwrap().clone();
    let good: Vec<Instant> = a.iter().filter(|x| x.1).map(|x| x.0).collect();
    let good_seq: Vec<u32> = a.iter().filter(|x| x.1).map(|x| x.2).collect();
    let bad: Vec<Instant> = a.iter().filter(|x| !x.1).map(|x| x.0).collect();
    let skipped: u32 = a
        .windows(2)
        .map(|w| w[1].2.wrapping_sub(w[0].2).saturating_sub(1))
        .sum();
    println!("skipped  : {skipped} assembled frame(s) never reached the callback");
    let Some((&first, &last)) = good.first().zip(good.last()) else {
        anyhow::bail!("no frames");
    };
    let span = (last - first).as_secs_f64();
    println!(
        "frames   : {} good, {} bad in {span:.1} s -> {:.2} fps",
        good.len(),
        bad.len(),
        (good.len() - 1) as f64 / span
    );
    if !polls.is_empty() {
        let worst = polls.iter().map(|p| p.1).max().unwrap_or_default();
        println!(
            "polls    : {} (every {poll_ms} ms), slowest {:.1} ms",
            polls.len(),
            worst.as_secs_f64() * 1000.0
        );
    }

    let mut dts: Vec<f64> = good
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .collect();
    let gaps: Vec<(usize, f64)> = dts
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, d)| *d >= GAP.as_secs_f64() * 1000.0)
        .collect();
    dts.sort_by(f64::total_cmp);
    let pct = |p: f64| dts[((dts.len() - 1) as f64 * p) as usize];
    println!(
        "interval : median {:.1} ms, p90 {:.1}, p99 {:.1}, max {:.1}",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(1.0)
    );
    let lost: f64 = gaps.iter().map(|(_, d)| d / pct(0.5) - 1.0).sum();
    println!(
        "gaps     : {} of >= {} ms, about {lost:.0} frames missing",
        gaps.len(),
        GAP.as_millis()
    );
    for (i, d) in &gaps {
        // The gap runs from frame i to frame i+1.
        let start = good[*i];
        let since_poll = polls
            .iter()
            .rev()
            .find(|p| p.0 <= start + Duration::from_millis(*d as u64))
            .map(|p| {
                let ms = start.duration_since(p.0).as_secs_f64() * 1000.0;
                if p.0 > start {
                    format!(
                        "poll {:.0} ms into the gap",
                        (p.0 - start).as_secs_f64() * 1000.0
                    )
                } else {
                    format!("{ms:.0} ms after a poll")
                }
            })
            .unwrap_or_else(|| "no poll before".into());
        let bad_in = bad
            .iter()
            .filter(|b| **b > start && **b < good[i + 1])
            .count();
        let seq_jump = good_seq[i + 1].wrapping_sub(good_seq[*i]);
        println!(
            "  at {:7.2} s: {d:5.0} ms  ({since_poll}, seq +{seq_jump}{})",
            (start - first).as_secs_f64(),
            if bad_in > 0 {
                format!(", {bad_in} bad frame(s) inside")
            } else {
                String::new()
            }
        );
    }
    Ok(())
}
