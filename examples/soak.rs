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
    let t0 = Instant::now();
    let total = Duration::from_secs(minutes * 60);
    let mut first_frames = None;
    let mut min_fps = f32::MAX;

    while t0.elapsed() < total {
        std::thread::sleep(Duration::from_secs(30));
        let _ = cam.take_frame();
        let s = cam.status();
        if s.frames > 0 && first_frames.is_none() {
            first_frames = Some((Instant::now(), s.frames));
        }
        if first_frames.is_some() {
            min_fps = min_fps.min(s.fps);
        }
        println!(
            "{:>5.1} min  state={:?} fps={:.2} frames={} stalls={} reapplied={} fpa={:.2}C err={}",
            t0.elapsed().as_secs_f32() / 60.0,
            s.state,
            s.fps,
            s.frames,
            s.stalls,
            s.reapplied,
            s.fpa_c.unwrap_or(f32::NAN),
            s.last_error.as_deref().unwrap_or("-")
        );
    }

    let s = cam.status();
    let avg = first_frames
        .map(|(t, n)| (s.frames - n) as f64 / t.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    println!(
        "RESULT: {minutes} min, frames={}, avg {:.2} fps, min sampled {:.2} fps, stalls={}, reapplied={}",
        s.frames, avg, min_fps, s.stalls, s.reapplied
    );
    let ok = avg >= 8.5 && s.stalls == 0;
    println!("{}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(if ok { 0 } else { 1 });
}
