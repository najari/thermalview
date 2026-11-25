//! Remote control over TCP: one text command per line, one reply line per command.
//!
//! Replies start with `OK` or `ERR`. The command set and wire format are documented in
//! `docs/remote.md`. Parsing lives here (pure, unit-tested); the socket handling is in
//! [`server`], and the commands are carried out on the UI thread (`app/remote.rs`).

pub mod server;

use crate::thermal::palette::Palette;

/// Longest accepted command line, in bytes (without the line ending).
pub const MAX_LINE: usize = 256;
pub const DEFAULT_PORT: u16 = 7700;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeSet {
    Auto,
    Manual { lo: f32, hi: f32 },
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
    Emissivity(Option<f32>),
    Alarms,
    Snapshot,
    Csv(bool),
    Record(bool),
    /// MP4 recording of the view.
    Video(bool),
    /// Raw counts of the current frame, base64 of little-endian u16.
    Frame,
    /// Close the connection (answered by the connection thread).
    Quit,
}

pub const HELP: &str = "commands: STATUS | MEAS | TEMP x y | SPOT [LIST|ADD x y|DEL id|CLEAR] | \
PALETTE [name] | RANGE [AUTO|lo hi] | EMISSIVITY [e] | ALARMS | SNAPSHOT | CSV START|STOP | \
REC START|STOP | MP4 START|STOP | FRAME | QUIT";

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
        ["RANGE", lo, hi] => {
            let (lo, hi) = (number(lo)?, number(hi)?);
            if lo >= hi {
                return Err("range: lo must be below hi".into());
            }
            Request::Range(Some(RangeSet::Manual { lo, hi }))
        }
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
                "EMISSIVITY",
                "ALARMS",
                "SNAPSHOT",
                "CSV",
                "REC",
                "MP4",
                "FRAME",
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
        assert!(parse("FFC").unwrap_err().starts_with("unknown command"));
        assert!(parse("CSV").unwrap_err().starts_with("bad arguments"));
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
