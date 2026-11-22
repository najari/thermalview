//! Record real camera frames to a `.tvseq`, read them back and verify they are bit-identical.
//! Reports the compression ratio and encode/decode time. The file is deleted afterwards.
//! Usage: cargo run --release --example seqcheck [seconds]

use std::time::{Duration, Instant};

use thermalview::camera::lepton::ShutterMode;
use thermalview::camera::Camera;
use thermalview::storage::sequence::{encode_frame, SeqHeader, SeqReader, SeqWriter};

fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let cam = Camera::start(ShutterMode::Manual, || {});

    let wait = Instant::now();
    let first = loop {
        if let Some(f) = cam.take_frame() {
            break f;
        }
        anyhow::ensure!(
            wait.elapsed() < Duration::from_secs(20),
            "no frame from camera"
        );
        std::thread::sleep(Duration::from_millis(5));
    };

    let dir = std::env::temp_dir().join(format!("thermalview_seqcheck_{}", std::process::id()));
    let header = SeqHeader {
        width: first.width,
        height: first.height,
        kelvin_per_count: first.kelvin_per_count,
        started_local: "seqcheck".into(),
        device: None,
    };
    let mut writer = SeqWriter::start(&dir, header)?;
    writer.push(&first);
    let mut frames = vec![first];

    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Some(f) = cam.take_frame() {
            writer.push(&f);
            frames.push(f);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(cam);
    let elapsed = t0.elapsed().as_secs_f64();
    let bytes_file = writer.bytes();
    let (path, written) = writer.finish()?;
    anyhow::ensure!(
        written == frames.len() as u64,
        "wrote {written} of {}",
        frames.len()
    );

    let t = Instant::now();
    for f in &frames {
        encode_frame(&f.raw, f.width)?;
    }
    let encode_ms = t.elapsed().as_secs_f64() * 1000.0 / frames.len() as f64;

    let mut reader = SeqReader::open(&path)?;
    anyhow::ensure!(reader.len() == frames.len(), "read {} frames", reader.len());
    let t = Instant::now();
    let mut mismatched = 0;
    for (i, f) in frames.iter().enumerate() {
        let (_, back) = reader.frame(i)?;
        if back.raw != f.raw || back.sequence != f.sequence {
            mismatched += 1;
        }
    }
    let decode_ms = t.elapsed().as_secs_f64() * 1000.0 / frames.len() as f64;

    let raw_bytes = (frames.len() * frames[0].raw.len() * 2) as f64;
    let on_disk = std::fs::metadata(&path)?.len();
    let per_hour_gb = on_disk as f64 / elapsed * 3600.0 / 1e9;
    println!("frames     : {} in {elapsed:.1} s", frames.len());
    println!("mismatched : {mismatched}");
    println!(
        "size       : {:.2} MB raw -> {:.2} MB on disk (ratio {:.2}:1, reported {:.2} MB)",
        raw_bytes / 1e6,
        on_disk as f64 / 1e6,
        raw_bytes / on_disk as f64,
        bytes_file as f64 / 1e6
    );
    println!("per hour   : {per_hour_gb:.2} GB");
    println!("per frame  : encode {encode_ms:.2} ms, read+decode {decode_ms:.2} ms");
    drop(reader);
    std::fs::remove_dir_all(&dir)?;
    anyhow::ensure!(mismatched == 0, "{mismatched} frames differ");
    Ok(())
}
