//! Remote control over TCP: one text command per line, one reply line per command.
//!
//! Replies start with `OK` or `ERR`. The command set and wire format are documented in
//! `docs/remote.md`. Parsing lives here (pure, unit-tested); the socket handling is in
//! [`server`], and the commands are carried out on the UI thread (`app/remote.rs`).

pub mod server;

use crate::thermal::filter::Temporal;
use crate::thermal::palette::Palette;
use crate::thermal::rotate::Rotation;

/// Longest accepted command line, in bytes (without the line ending).
pub const MAX_LINE: usize = 256;
pub const DEFAULT_PORT: u16 = 7700;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeSet {
    Auto,
    /// Automatic without the coldest and hottest pixels.
    Robust,
    Manual {
        lo: f32,
        hi: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IsoSet {
    Off,
    Above(f32),
    Below(f32),
    Between(f32, f32),
}

/// Difference image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffSet {
    /// Take the current frame as the reference and show the change since then.
    On,
    Off,
    /// Color span, ± this many display units; `None` follows the image.
    Span(Option<f32>),
}

/// Magnification of the image view, optionally centered on a pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomSet {
    pub factor: f32,
    pub center: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request {
    Help,
    Status,
    /// Frame statistics, center, spots and Box/Line readings of the current frame.
    Measure,
    /// 3×3 mean at a pixel.
    Temp {
        x: usize,
        y: usize,
    },
    SpotList,
    SpotAdd {
        x: usize,
        y: usize,
    },
    SpotDel(u32),
    SpotClear,
    Palette(Option<Palette>),
    Range(Option<RangeSet>),
    /// Clockwise turn of the live image.
    Rotate(Option<Rotation>),
    Emissivity(Option<f32>),
    Alarms,
    Snapshot,
    Csv(bool),
    Record(bool),
    /// MP4 recording of the view.
    Video(bool),
    /// Raw counts of the current frame, base64 of little-endian u16.
    Frame,
    /// Line up the webcam picture with the thermal image automatically.
    Align,
    /// Hold the current image (incoming frames are dropped) or go back to live video.
    Freeze(Option<bool>),
    Isotherm(Option<IsoSet>),
    Diff(Option<DiffSet>),
    /// Strength of the temporal noise filter (1 = off).
    Filter(Option<u8>),
    Zoom(Option<ZoomSet>),
    /// Save a picture of the whole window as it is drawn.
    Screenshot,
    /// Close the connection (answered by the connection thread).
    Quit,
}

pub const HELP: &str = "commands: STATUS | MEAS | TEMP x y | SPOT [LIST|ADD x y|DEL id|CLEAR] | \
PALETTE [name] | RANGE [AUTO|ROBUST|lo hi] | ISOTHERM [OFF|ABOVE t|BELOW t|BETWEEN lo hi] | DIFF [ON|OFF|SPAN AUTO|SPAN x] | \
FILTER [1|2|4|8|16] | ROTATE [0|90|180|270] | EMISSIVITY [e] | ALARMS | FREEZE [ON|OFF] | ZOOM [factor [x y]] | \
SNAPSHOT | CSV START|STOP | REC START|STOP | MP4 START|STOP | FRAME | ALIGN | SCREENSHOT | QUIT";

/// Parse one command line (case-insensitive, words separated by spaces).
pub fn parse(line: &str) -> Result<Request, String> {
    let words: Vec<String> = line
        .split_whitespace()
        .map(str::to_ascii_uppercase)
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    let req = match w.as_slice() {
        ["HELP" | "?"] => Request::Help,
        ["STATUS"] => Request::Status,
        ["MEAS"] => Request::Measure,
        ["TEMP", x, y] => Request::Temp {
            x: pixel(x)?,
            y: pixel(y)?,
        },
        ["SPOT"] | ["SPOT", "LIST"] => Request::SpotList,
        ["SPOT", "ADD", x, y] => Request::SpotAdd {
            x: pixel(x)?,
            y: pixel(y)?,
        },
        ["SPOT", "DEL", id] => Request::SpotDel(spot_id(id)?),
        ["SPOT", "CLEAR"] => Request::SpotClear,
        ["PALETTE"] => Request::Palette(None),
        ["PALETTE", name] => Request::Palette(Some(palette(name)?)),
        ["RANGE"] => Request::Range(None),
        ["RANGE", "AUTO"] => Request::Range(Some(RangeSet::Auto)),
        ["RANGE", "ROBUST"] => Request::Range(Some(RangeSet::Robust)),
        ["ISOTHERM"] => Request::Isotherm(None),
        ["ISOTHERM", "OFF"] => Request::Isotherm(Some(IsoSet::Off)),
        ["ISOTHERM", "ABOVE", t] => Request::Isotherm(Some(IsoSet::Above(number(t)?))),
        ["ISOTHERM", "BELOW", t] => Request::Isotherm(Some(IsoSet::Below(number(t)?))),
        ["ISOTHERM", "BETWEEN", lo, hi] => {
            let (lo, hi) = (number(lo)?, number(hi)?);
            if lo >= hi {
                return Err("isotherm: lo must be below hi".into());
            }
            Request::Isotherm(Some(IsoSet::Between(lo, hi)))
        }
        ["DIFF"] => Request::Diff(None),
        ["DIFF", "ON"] => Request::Diff(Some(DiffSet::On)),
        ["DIFF", "OFF"] => Request::Diff(Some(DiffSet::Off)),
        ["DIFF", "SPAN", "AUTO"] => Request::Diff(Some(DiffSet::Span(None))),
        ["DIFF", "SPAN", s] => Request::Diff(Some(DiffSet::Span(Some(
            number(s)
                .ok()
                .filter(|s| (0.1..=1000.0).contains(s))
                .ok_or_else(|| format!("span {s}: use AUTO or 0.1 to 1000"))?,
        )))),
        ["FILTER"] => Request::Filter(None),
        ["FILTER", "OFF"] => Request::Filter(Some(1)),
        ["FILTER", n] => Request::Filter(Some(
            n.parse()
                .ok()
                .filter(|n| Temporal::STRENGTHS.contains(n))
                .ok_or_else(|| format!("filter {n}: use 1 (off), 2, 4, 8 or 16"))?,
        )),
        ["FREEZE"] => Request::Freeze(None),
        ["FREEZE", "ON"] => Request::Freeze(Some(true)),
        ["FREEZE", "OFF"] => Request::Freeze(Some(false)),
        ["ZOOM"] => Request::Zoom(None),
        ["ZOOM", z] => Request::Zoom(Some(ZoomSet {
            factor: zoom(z)?,
            center: None,
        })),
        ["ZOOM", z, x, y] => Request::Zoom(Some(ZoomSet {
            factor: zoom(z)?,
            center: Some((pixel(x)?, pixel(y)?)),
        })),
        ["RANGE", lo, hi] => {
            let (lo, hi) = (number(lo)?, number(hi)?);
            if lo >= hi {
                return Err("range: lo must be below hi".into());
            }
            Request::Range(Some(RangeSet::Manual { lo, hi }))
        }
        ["ROTATE"] => Request::Rotate(None),
        ["ROTATE", deg] => Request::Rotate(Some(rotation(deg)?)),
        ["EMISSIVITY"] => Request::Emissivity(None),
        ["EMISSIVITY", e] => Request::Emissivity(Some(number(e)?)),
        ["ALARMS"] => Request::Alarms,
        ["SNAPSHOT"] => Request::Snapshot,
        ["CSV", "START"] => Request::Csv(true),
        ["CSV", "STOP"] => Request::Csv(false),
        ["REC", "START"] => Request::Record(true),
        ["REC", "STOP"] => Request::Record(false),
        ["MP4", "START"] => Request::Video(true),
        ["MP4", "STOP"] => Request::Video(false),
        ["FRAME"] => Request::Frame,
        ["ALIGN"] => Request::Align,
        ["SCREENSHOT"] => Request::Screenshot,
        ["QUIT" | "EXIT"] => Request::Quit,
        [] => return Err("empty command".into()),
        [cmd, ..] => {
            let known = [
                "HELP",
                "STATUS",
                "MEAS",
                "TEMP",
                "SPOT",
                "PALETTE",
                "RANGE",
                "ISOTHERM",
                "DIFF",
                "FILTER",
                "FREEZE",
                "ZOOM",
                "ROTATE",
                "EMISSIVITY",
                "ALARMS",
                "SNAPSHOT",
                "CSV",
                "REC",
                "MP4",
                "FRAME",
                "ALIGN",
                "SCREENSHOT",
                "QUIT",
            ];
            return Err(if known.contains(cmd) {
                format!("bad arguments for {cmd}; send HELP")
            } else {
                format!("unknown command {cmd}; send HELP")
            });
        }
    };
    Ok(req)
}

/// Degrees clockwise as the remote protocol writes them.
pub fn rotation_degrees(r: Rotation) -> u16 {
    match r {
        Rotation::R0 => 0,
        Rotation::R90 => 90,
        Rotation::R180 => 180,
        Rotation::R270 => 270,
    }
}

fn rotation(s: &str) -> Result<Rotation, String> {
    Rotation::ALL
        .into_iter()
        .find(|r| s.parse() == Ok(rotation_degrees(*r)))
        .ok_or_else(|| format!("rotation {s}: use 0, 90, 180 or 270"))
}

/// Largest magnification of the image view.
pub const MAX_ZOOM: f32 = 8.0;

fn zoom(s: &str) -> Result<f32, String> {
    number(s)
        .ok()
        .filter(|z| (1.0..=MAX_ZOOM).contains(z))
        .ok_or_else(|| format!("zoom {s}: use 1 to {MAX_ZOOM}"))
}

fn pixel(s: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("{s} is not a pixel coordinate"))
}

fn spot_id(s: &str) -> Result<u32, String> {
    s.trim_start_matches('S')
        .parse()
        .map_err(|_| format!("{s} is not a spot id"))
}

fn number(s: &str) -> Result<f32, String> {
    s.parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{s} is not a number"))
}

/// Protocol name of a palette: its label without spaces, lower case.
pub fn palette_name(p: Palette) -> String {
    p.label().replace(' ', "").to_ascii_lowercase()
}

fn palette(s: &str) -> Result<Palette, String> {
    let want: String = s
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    Palette::ALL
        .into_iter()
        .find(|p| palette_name(*p) == want)
        .ok_or_else(|| {
            let names: Vec<String> = Palette::ALL.into_iter().map(palette_name).collect();
            format!("unknown palette; one of {}", names.join(", "))
        })
}

/// Reply line for a command result (without the line ending).
pub fn reply(r: &Result<String, String>) -> String {
    let clean = |s: &str| s.replace(['\r', '\n'], " ");
    match r {
        Ok(body) if body.is_empty() => "OK".into(),
        Ok(body) => format!("OK {}", clean(body)),
        Err(e) => format!("ERR {}", clean(e)),
    }
}

/// Standard base64 (RFC 4648, with padding).
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_case_insensitively() {
        assert_eq!(parse(" meas "), Ok(Request::Measure));
        assert_eq!(parse("temp 80 60"), Ok(Request::Temp { x: 80, y: 60 }));
        assert_eq!(parse("Spot Add 1 2"), Ok(Request::SpotAdd { x: 1, y: 2 }));
        assert_eq!(parse("SPOT DEL S3"), Ok(Request::SpotDel(3)));
        assert_eq!(parse("spot"), Ok(Request::SpotList));
        assert_eq!(
            parse("palette white-hot"),
            Ok(Request::Palette(Some(Palette::WhiteHot)))
        );
        assert_eq!(
            parse("RANGE 20 35.5"),
            Ok(Request::Range(Some(RangeSet::Manual {
                lo: 20.0,
                hi: 35.5
            })))
        );
        assert_eq!(
            parse("range auto"),
            Ok(Request::Range(Some(RangeSet::Auto)))
        );
        assert_eq!(parse("csv stop"), Ok(Request::Csv(false)));
        assert_eq!(parse("mp4 start"), Ok(Request::Video(true)));
        assert_eq!(
            parse("EMISSIVITY 0.95"),
            Ok(Request::Emissivity(Some(0.95)))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("").is_err());
        assert!(parse("TEMP -1 5").is_err());
        assert!(parse("RANGE 30 20").is_err());
        assert!(parse("RANGE nan 20").is_err());
        assert!(parse("PALETTE purple").unwrap_err().contains("ironbow"));
        assert_eq!(
            parse("rotate 270"),
            Ok(Request::Rotate(Some(Rotation::R270)))
        );
        assert_eq!(parse("ROTATE"), Ok(Request::Rotate(None)));
        assert!(parse("ROTATE 45").unwrap_err().contains("270"));
        assert!(parse("FFC").unwrap_err().starts_with("unknown command"));
        assert!(parse("CSV").unwrap_err().starts_with("bad arguments"));
    }

    #[test]
    fn view_commands() {
        assert_eq!(
            parse("range robust"),
            Ok(Request::Range(Some(RangeSet::Robust)))
        );
        assert_eq!(parse("ISOTHERM"), Ok(Request::Isotherm(None)));
        assert_eq!(
            parse("isotherm above 60"),
            Ok(Request::Isotherm(Some(IsoSet::Above(60.0))))
        );
        assert_eq!(
            parse("isotherm between 30 35.5"),
            Ok(Request::Isotherm(Some(IsoSet::Between(30.0, 35.5))))
        );
        assert!(parse("ISOTHERM BETWEEN 40 30").is_err());
        assert_eq!(
            parse("isotherm off"),
            Ok(Request::Isotherm(Some(IsoSet::Off)))
        );
        assert_eq!(parse("diff"), Ok(Request::Diff(None)));
        assert_eq!(parse("diff on"), Ok(Request::Diff(Some(DiffSet::On))));
        assert_eq!(
            parse("DIFF SPAN 2.5"),
            Ok(Request::Diff(Some(DiffSet::Span(Some(2.5)))))
        );
        assert_eq!(
            parse("diff span auto"),
            Ok(Request::Diff(Some(DiffSet::Span(None))))
        );
        assert!(parse("DIFF SPAN 0").unwrap_err().contains("AUTO"));
        assert_eq!(parse("filter 8"), Ok(Request::Filter(Some(8))));
        assert_eq!(parse("filter off"), Ok(Request::Filter(Some(1))));
        assert!(parse("FILTER 3").unwrap_err().contains("16"));
        assert_eq!(parse("freeze on"), Ok(Request::Freeze(Some(true))));
        assert_eq!(parse("FREEZE"), Ok(Request::Freeze(None)));
        assert!(parse("FREEZE NOW")
            .unwrap_err()
            .starts_with("bad arguments"));
        let zoom = |factor, center| Ok(Request::Zoom(Some(ZoomSet { factor, center })));
        assert_eq!(parse("zoom 2.5"), zoom(2.5, None));
        assert_eq!(parse("zoom 4 100 60"), zoom(4.0, Some((100, 60))));
        assert!(parse("ZOOM 0.5").is_err() && parse("ZOOM 9").is_err());
    }

    #[test]
    fn palette_names_roundtrip() {
        for p in Palette::ALL {
            assert_eq!(
                parse(&format!("PALETTE {}", palette_name(p))),
                Ok(Request::Palette(Some(p)))
            );
        }
    }

    #[test]
    fn reply_is_one_line() {
        assert_eq!(reply(&Ok(String::new())), "OK");
        assert_eq!(reply(&Ok("a\nb".into())), "OK a b");
        assert_eq!(reply(&Err("x".into())), "ERR x");
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }
}
