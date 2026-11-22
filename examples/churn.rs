//! Stress the shutdown path the app relies on: open, stream, stop at a varying moment, close.
//! Stops land on idle gaps and mid-frame bursts alike. Run it under a debugger or page heap to
//! catch use-after-free in the USB teardown.
//! Usage: cargo run --release --example churn [cycles]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thermalview::camera::{uvc, HEIGHT, PID, VID, WIDTH};

/// One open/stream/close cycle. Returns the frames received and whether teardown stayed clean.
fn cycle(ctx: &uvc::Context, hold_ms: u64) -> anyhow::Result<(u64, bool)> {
    let mut h = ctx.find_device(VID, PID)?.open()?;
    let frames = Arc::new(AtomicU64::new(0));
    let counter = frames.clone();
    h.start_stream(uvc::FORMAT_Y16, WIDTH as u32, HEIGHT as u32, move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
    })?;
    std::thread::sleep(Duration::from_millis(hold_ms));
    h.stop_stream();
    let clean = !h.has_stuck_transfers();
    Ok((frames.load(Ordering::Relaxed), clean))
}

fn main() -> anyhow::Result<()> {
    let cycles: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let ctx = uvc::Context::new()?;
    let t0 = Instant::now();
    let (mut total, mut failed, mut stuck) = (0u64, 0u32, 0u32);

    for i in 0..cycles {
        // Holds sweep 0..~1.5 s so some stops come before the first frame and some after.
        let hold = (i * 137) % 1500;
        match cycle(&ctx, hold) {
            Ok((n, clean)) => {
                total += n;
                if !clean {
                    stuck += 1;
                    println!("cycle {i} (hold {hold} ms): transfers stuck");
                    break; // the device stays held until this process exits
                }
            }
            Err(e) => {
                failed += 1;
                println!("cycle {i} (hold {hold} ms): {e}");
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("{cycles} cycles: {total} frames, {failed} failed, {stuck} stuck");
    anyhow::ensure!(stuck == 0, "USB transfers got stuck");

    // The device must still stream normally afterwards.
    let (after, _) = cycle(&ctx, 5000)?;
    println!("final 5 s stream: {after} frames");
    println!("done in {:.0} s", t0.elapsed().as_secs_f64());
    anyhow::ensure!(after >= 30, "device did not stream normally after churn");
    anyhow::ensure!(failed == 0, "{failed} cycles failed");
    Ok(())
}
