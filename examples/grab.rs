//! Phase-1 check: open the PureThermal, stream Y16 for a few seconds and report the frame rate.
//! Usage: cargo run --release --example grab [seconds] [poll_ms]
//! With `poll_ms` > 0 the Lepton status is read every poll_ms while streaming (like the app does).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thermalview::camera::{lepton, uvc, HEIGHT, PID, VID, WIDTH};

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    // Optional: poll Lepton status every N ms while streaming (0 = no control traffic).
    let poll_ms: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let ctx = uvc::Context::new()?;
    let dev = ctx.find_device(VID, PID)?;
    let info = dev.info()?;
    println!(
        "device   : {} / {} (fw {})",
        info.manufacturer, info.product, info.serial
    );
    let mut h = dev.open()?;

    println!(
        "part     : {}",
        lepton::part_number(&h).unwrap_or_else(|e| e.to_string())
    );
    println!(
        "lepton sw: {}",
        lepton::software_version(&h).unwrap_or_else(|e| e.to_string())
    );
    println!("FPA      : {:.2} C", lepton::fpa_temp_c(&h)?);
    println!("TLinear  : {:?} K/count", lepton::tlinear_scale(&h)?);
    let obj = lepton::get_shutter_mode_obj(&h)?;
    println!(
        "shutter  : mode={} since FFC {:.1}s",
        obj.mode(),
        obj.elapsed_since_ffc_ms() as f32 / 1000.0
    );

    let frames = Arc::new(AtomicU64::new(0));
    let bad = Arc::new(AtomicU64::new(0));
    let sample = Arc::new(std::sync::Mutex::new(None::<(u16, u16, u32)>));
    {
        let (frames, bad, sample) = (frames.clone(), bad.clone(), sample.clone());
        h.start_stream(uvc::FORMAT_Y16, WIDTH as u32, HEIGHT as u32, move |f| {
            if f.data.len() != WIDTH * HEIGHT * 2 {
                bad.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let px = thermalview::thermal::frame::decode_le_u16(f.data);
            let (mn, mx) = px
                .iter()
                .fold((u16::MAX, 0), |(a, b), &v| (a.min(v), b.max(v)));
            if let Ok(mut s) = sample.lock() {
                *s = Some((mn, mx, f.sequence));
            }
            frames.fetch_add(1, Ordering::Relaxed);
        })?;
    }

    // Measure the steady-state rate from the first frame on (stream start-up takes ~0.5 s).
    let t_start = Instant::now();
    while frames.load(Ordering::Relaxed) == 0 && t_start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let first_latency = t_start.elapsed();
    let n0 = frames.load(Ordering::Relaxed);
    let t0 = Instant::now();
    if poll_ms == 0 {
        std::thread::sleep(Duration::from_secs(secs));
    } else {
        // Same status reads the camera worker does, to measure their effect on the stream.
        let mut polls = 0u32;
        let mut worst = Duration::ZERO;
        while t0.elapsed() < Duration::from_secs(secs) {
            std::thread::sleep(Duration::from_millis(poll_ms));
            let tp = Instant::now();
            let _ = lepton::fpa_temp_c(&h);
            let _ = lepton::get_shutter_mode_obj(&h);
            let _ = lepton::aux_temp_c(&h);
            let _ = lepton::ffc_status(&h);
            worst = worst.max(tp.elapsed());
            polls += 1;
        }
        println!(
            "polling  : {polls} polls every {poll_ms} ms, slowest {:.1} ms",
            worst.as_secs_f64() * 1000.0
        );
    }
    let n = frames.load(Ordering::Relaxed) - n0;
    let dt = t0.elapsed().as_secs_f64();
    h.stop_stream();

    println!(
        "first    : {:.0} ms after start",
        first_latency.as_secs_f64() * 1000.0
    );
    println!(
        "frames   : {n} good, {} bad in {dt:.2}s -> {:.2} fps",
        bad.load(Ordering::Relaxed),
        n as f64 / dt
    );
    if let Some((mn, mx, seq)) = *sample.lock().unwrap() {
        println!(
            "last     : seq={seq} min={mn} ({:.2} C) max={mx} ({:.2} C)",
            mn as f32 / 100.0 - 273.15,
            mx as f32 / 100.0 - 273.15
        );
    }
    Ok(())
}
