//! Thermal Master P3: vendor USB protocol (not UVC), through `rusb`.
//!
//! Protocol from the public reverse-engineering notes cited in docs/DEVELOPMENT.md, stage 9.
//! The camera worker drives a [`Device`]; [`Assembler`] turns bulk reads into frames and has no
//! USB dependency, so it is tested without the camera.

use std::time::Duration;

use rusb::UsbContext;
use serde::{Deserialize, Serialize};

use crate::thermal::frame::decode_le_u16;

pub const VID: u16 = 0x3474;
pub const PID: u16 = 0x45A2;
pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 192;
/// Temperature pixels are in 1/64 K.
pub const KELVIN_PER_COUNT: f32 = 1.0 / 64.0;

/// Start and end markers.
const MARKER: usize = 12;
/// IR image rows, 2 metadata rows, temperature rows.
const ROWS: usize = 2 * HEIGHT + 2;
const THERMAL_ROW: usize = HEIGHT + 2;
/// Start marker + pixels: everything before the end marker.
const BODY: usize = MARKER + 2 * ROWS * WIDTH;

/// Bulk read size. Reading a whole frame at once is reported to wedge the device on Windows.
pub const CHUNK: usize = 16 * 1024;
const EP_IN: u8 = 0x81;
const CTRL_TIMEOUT: Duration = Duration::from_secs(1);
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Measurement range of the sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Gain {
    /// −20 to 150 °C, more sensitive.
    #[default]
    High,
    /// 0 to 550 °C.
    Low,
}

impl Gain {
    pub fn label(self) -> &'static str {
        match self {
            Gain::High => "고감도 (−20~150 °C)",
            Gain::Low => "저감도 (0~550 °C)",
        }
    }
}

/// CRC16-CCITT, polynomial 0x1021, initial value 0.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// An 18-byte command: kind (big-endian), parameter, register, response length, CRC.
fn command(kind: u16, param: u16, register: u16, resp_len: u16) -> [u8; 18] {
    let mut c = [0u8; 18];
    c[0..2].copy_from_slice(&kind.to_be_bytes());
    c[2..4].copy_from_slice(&param.to_le_bytes());
    c[4..6].copy_from_slice(&register.to_le_bytes());
    c[12..14].copy_from_slice(&resp_len.to_le_bytes());
    let crc = crc16(&c[..16]);
    c[16..18].copy_from_slice(&crc.to_le_bytes());
    c
}

fn read_register(register: u16, len: u16) -> [u8; 18] {
    command(0x0101, 0x0081, register, len)
}

fn start_stream() -> [u8; 18] {
    command(0x012F, 0x0081, 0, 1)
}

fn set_gain(g: Gain) -> [u8; 18] {
    command(0x012F, 0x0041, if g == Gain::High { 1 } else { 0 }, 0)
}

fn shutter() -> [u8; 18] {
    command(0x0136, 0x0043, 0, 0)
}

fn is_start(c: &[u8]) -> bool {
    c.len() >= 2 && c[0] == MARKER as u8 && matches!(c[1], 0x8C | 0x8D)
}

fn is_end(c: &[u8]) -> bool {
    c.len() == MARKER && c[0] == MARKER as u8 && matches!(c[1], 0x8E | 0x8F)
}

/// Frame counter, the same in the start and end markers of one frame.
fn counter(marker: &[u8]) -> u32 {
    u32::from_le_bytes([marker[2], marker[3], marker[4], marker[5]])
}

/// Builds frames from bulk reads. A frame is a start marker plus pixels (several reads), then a
/// 12-byte read holding the end marker. Anything else is dropped until the next start marker.
#[derive(Default)]
pub struct Assembler {
    buf: Vec<u8>,
    /// Frames thrown away: wrong size, counter mismatch, or no end marker.
    pub dropped: u64,
}

impl Assembler {
    /// Add one bulk read. Returns the temperature pixels (WIDTH × HEIGHT, 1/64 K) when a frame
    /// completes.
    pub fn push(&mut self, chunk: &[u8]) -> Option<Vec<u16>> {
        if is_end(chunk) {
            let ok = self.buf.len() == BODY && counter(&self.buf) == counter(chunk);
            let frame = ok.then(|| {
                let from = MARKER + THERMAL_ROW * WIDTH * 2;
                decode_le_u16(&self.buf[from..from + WIDTH * HEIGHT * 2])
            });
            if !ok && !self.buf.is_empty() {
                self.dropped += 1;
            }
            self.buf.clear();
            return frame;
        }
        if self.buf.is_empty() && !is_start(chunk) {
            return None; // out of step: wait for a start marker
        }
        if self.buf.len() + chunk.len() > BODY {
            // Too long, e.g. the odd frame the camera sends right after a shutter cycle.
            self.buf.clear();
            self.dropped += 1;
            return None;
        }
        self.buf.extend_from_slice(chunk);
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct Info {
    pub model: String,
    pub firmware: String,
    pub serial: String,
}

#[derive(Debug)]
pub enum OpenError {
    NotFound,
    /// Another program has the camera.
    Busy(String),
    Other(String),
}

/// An open P3. Dropping it stops streaming and releases the interfaces.
pub struct Device {
    h: rusb::DeviceHandle<rusb::Context>,
    streaming: bool,
}

/// Find and open the first P3.
pub fn open(ctx: &rusb::Context) -> Result<Device, OpenError> {
    let other = |what: &str, e: rusb::Error| OpenError::Other(format!("P3 {what}: {e}"));
    let devices = ctx.devices().map_err(|e| other("장치 목록", e))?;
    let dev = devices
        .iter()
        .find(|d| {
            d.device_descriptor()
                .is_ok_and(|x| x.vendor_id() == VID && x.product_id() == PID)
        })
        .ok_or(OpenError::NotFound)?;
    let h = dev.open().map_err(|e| match e {
        rusb::Error::Access | rusb::Error::Busy => OpenError::Busy(e.to_string()),
        rusb::Error::NoDevice | rusb::Error::NotFound => OpenError::NotFound,
        rusb::Error::NotSupported => {
            OpenError::Other("P3에 WinUSB 드라이버가 없습니다. Zadig로 WinUSB를 설치하세요.".into())
        }
        e => other("열기", e),
    })?;
    for i in [0, 1] {
        h.claim_interface(i).map_err(|e| match e {
            rusb::Error::Access | rusb::Error::Busy => OpenError::Busy(e.to_string()),
            e => other("인터페이스 점유", e),
        })?;
    }
    Ok(Device {
        h,
        streaming: false,
    })
}

impl Device {
    fn status(&self) -> rusb::Result<u8> {
        let mut b = [0u8; 1];
        self.h
            .read_control(0xC1, 0x22, 0, 0, &mut b, CTRL_TIMEOUT)?;
        Ok(b[0])
    }

    /// Send a command and read its acknowledgement.
    fn send(&self, cmd: &[u8; 18]) -> rusb::Result<()> {
        self.h.write_control(0x41, 0x20, 0, 0, cmd, CTRL_TIMEOUT)?;
        self.status()?;
        Ok(())
    }

    /// Send a command that answers with `len` bytes.
    fn query(&self, cmd: &[u8; 18], len: usize) -> rusb::Result<Vec<u8>> {
        self.send(cmd)?;
        let mut buf = vec![0u8; len];
        let n = self
            .h
            .read_control(0xC1, 0x21, 0, 0, &mut buf, CTRL_TIMEOUT)?;
        buf.truncate(n);
        self.status()?;
        Ok(buf)
    }

    fn text(&self, register: u16, len: u16) -> rusb::Result<String> {
        let b = self.query(&read_register(register, len), len as usize)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).trim().to_owned())
    }

    pub fn info(&self) -> rusb::Result<Info> {
        Ok(Info {
            model: self.text(0x01, 30)?,
            firmware: self.text(0x02, 12)?,
            serial: self.text(0x07, 64)?,
        })
    }

    /// The start sequence. `wait` sleeps and returns false if the caller wants to stop, in which
    /// case this returns Ok(false).
    pub fn start(&mut self, wait: &mut dyn FnMut(Duration) -> bool) -> rusb::Result<bool> {
        self.query(&start_stream(), 1)?;
        if !wait(Duration::from_secs(1)) {
            return Ok(false);
        }
        self.h.set_alternate_setting(1, 1)?;
        self.streaming = true;
        self.h.write_control(0x40, 0xEE, 0, 1, &[], CTRL_TIMEOUT)?;
        if !wait(Duration::from_secs(2)) {
            return Ok(false);
        }
        // Primes the pipe; its data and outcome do not matter.
        let mut buf = vec![0u8; CHUNK];
        let _ = self
            .h
            .read_bulk(EP_IN, &mut buf, Duration::from_millis(100));
        self.query(&start_stream(), 1)?;
        Ok(true)
    }

    /// One bulk read into `buf` (CHUNK bytes). Ok(0) when nothing arrived in time.
    pub fn read(&self, buf: &mut [u8]) -> rusb::Result<usize> {
        match self.h.read_bulk(EP_IN, buf, READ_TIMEOUT) {
            Err(rusb::Error::Timeout) => Ok(0),
            r => r,
        }
    }

    pub fn set_gain(&self, g: Gain) -> rusb::Result<()> {
        self.send(&set_gain(g))
    }

    /// Close the shutter and recalibrate (the camera also does this by itself every ~90 s).
    pub fn shutter(&self) -> rusb::Result<()> {
        self.send(&shutter())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.streaming {
            if let Err(e) = self.h.set_alternate_setting(1, 0) {
                log::warn!("P3: stopping the stream failed: {e}");
            }
        }
        for i in [1, 0] {
            let _ = self.h.release_interface(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn commands_match_captured_bytes() {
        // Captured from the vendor tool (P3_PROTOCOL.md), CRC included.
        let cases = [
            (
                read_register(0x01, 30),
                "0101810001000000000000001e0000004f90",
            ),
            (
                read_register(0x07, 64),
                "01018100070000000000000040000000104c",
            ),
            (start_stream(), "012f81000000000000000000010000004930"),
            (set_gain(Gain::Low), "012f41000000000000000000000000003c3a"),
            (set_gain(Gain::High), "012f41000100000000000000000000004939"),
            (shutter(), "01364300000000000000000000000000cd0b"),
        ];
        for (cmd, want) in cases {
            assert_eq!(cmd.to_vec(), hex(want), "{want}");
        }
    }

    fn marker(sync: u8, count: u32) -> Vec<u8> {
        let mut m = vec![MARKER as u8, sync];
        m.extend_from_slice(&count.to_le_bytes());
        m.extend_from_slice(&[0; 6]);
        m
    }

    /// A frame as the camera sends it, split like 16 KiB bulk reads.
    fn frame_reads(count: u32, value: u16) -> Vec<Vec<u8>> {
        let mut body = marker(0x8C | (count & 1) as u8, count);
        for row in 0..ROWS {
            let v = if row >= THERMAL_ROW { value } else { 7 };
            for _ in 0..WIDTH {
                body.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut reads: Vec<Vec<u8>> = body.chunks(CHUNK).map(<[u8]>::to_vec).collect();
        reads.push(marker(0x8E | (count & 1) as u8, count));
        reads
    }

    fn feed(a: &mut Assembler, reads: &[Vec<u8>]) -> Vec<Vec<u16>> {
        reads.iter().filter_map(|r| a.push(r)).collect()
    }

    #[test]
    fn frames_are_built_from_bulk_reads() {
        let mut a = Assembler::default();
        let reads = frame_reads(5, 19_000);
        assert_eq!(reads.len(), 14, "12 full reads, the rest, the end marker");
        let frames = feed(&mut a, &reads);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), WIDTH * HEIGHT);
        assert!(
            frames[0].iter().all(|&v| v == 19_000),
            "temperature rows only"
        );
        assert_eq!(a.dropped, 0);
    }

    #[test]
    fn broken_frames_are_dropped_and_the_next_one_is_kept() {
        let mut a = Assembler::default();
        // Joining in the middle of a frame: ignored until the next start marker.
        let partial = frame_reads(1, 100);
        assert!(feed(&mut a, &partial[3..]).is_empty());
        assert_eq!(a.dropped, 0);

        // Counter mismatch between the markers.
        let mut bad = frame_reads(2, 100);
        *bad.last_mut().unwrap() = marker(0x8E, 99);
        assert!(feed(&mut a, &bad).is_empty());
        assert_eq!(a.dropped, 1);

        // A frame with extra data (as after a shutter cycle).
        let mut long = frame_reads(3, 100);
        long.insert(1, vec![0; 2048]);
        assert!(feed(&mut a, &long).is_empty());
        assert_eq!(a.dropped, 2);

        // A frame missing a read.
        let mut short = frame_reads(4, 100);
        short.remove(5);
        assert!(feed(&mut a, &short).is_empty());
        assert_eq!(a.dropped, 3);

        assert_eq!(feed(&mut a, &frame_reads(5, 200)).len(), 1);
    }

    #[test]
    fn temperature_conversion() {
        // 300 K in 1/64 K counts.
        let raw = (300.0 / KELVIN_PER_COUNT) as u16;
        assert_eq!(raw, 19_200);
        assert!((raw as f32 * KELVIN_PER_COUNT - 273.15 - 26.85).abs() < 1e-4);
    }
}
