//! MVP gate: run the camera worker (same code as the app) for N minutes and log health every 30 s.
//! Usage: cargo run --release --example soak [minutes]   (default 60)
//! Exit code 0 if the average rate stayed >= 8.5 fps and no recovery was needed.

use std::time::{Duration, Instant};

use thermalview::camera::lepton::ShutterMode;
use thermalview::camera::Camera;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let minutes: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let cam = Camera::start(ShutterMode::Manual, || {});

    // Measure from the first frame so that stream start-up does not count against the rate.
    let wait = Instant::now();
    while cam.total_frames() == 0 && wait.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let first = cam.total_frames();
    let t0 = Instant::now();
    let total = Duration::from_secs(minutes * 60);
    let (mut last_n, mut last_t) = (first, t0);
    let mut min_window = f64::MAX;

    while t0.elapsed() < total {
        std::thread::sleep(Duration::from_secs(30).min(total.saturating_sub(t0.elapsed())));
        let _ = cam.take_frame();
        let n = cam.total_frames();
        let window = (n - last_n) as f64 / last_t.elapsed().as_secs_f64();
        (last_n, last_t) = (n, Instant::now());
        min_window = min_window.min(window);
        let s = cam.status();
        println!(
            "{:>5.1} min  state={:?} fps(30s)={window:.2} frames={n} stalls={} reapplied={} fpa={:.2}C err={}",
            t0.elapsed().as_secs_f32() / 60.0,
            s.state,
            s.stalls,
            s.reapplied,
            s.fpa_c.unwrap_or(f32::NAN),
            s.last_error.as_deref().unwrap_or("-")
        );
    }

    let s = cam.status();
    let n = cam.total_frames() - first;
    let avg = n as f64 / t0.elapsed().as_secs_f64();
    println!(
        "RESULT: {minutes} min, frames={n}, avg {avg:.2} fps, slowest 30 s window {min_window:.2} fps, stalls={}, reapplied={}",
        s.stalls, s.reapplied
    );
    let ok = avg >= 8.5 && s.stalls == 0;
    println!("{}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
