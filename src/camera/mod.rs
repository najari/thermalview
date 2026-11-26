//! Camera access: libuvc wrapper, Lepton commands, the Thermal Master P3, and the background
//! camera worker.

pub mod lepton;
pub mod p3;
pub mod uvc;
pub mod worker;

use serde::{Deserialize, Serialize};

pub use worker::{Camera, CameraStatus, Command, ConnState};

/// PureThermal USB IDs.
pub const VID: u16 = 0x1E4E;
pub const PID: u16 = 0x0100;
/// Lepton 3.x frame size.
pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 120;

/// A supported camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PureThermal board with a FLIR Lepton (UVC, libuvc).
    PureThermal,
    /// Thermal Master P3 (vendor protocol, rusb).
    P3,
}

/// Which camera to look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Source {
    /// Whichever is connected; PureThermal first.
    #[default]
    Auto,
    PureThermal,
    P3,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Auto, Source::PureThermal, Source::P3];

    pub fn label(self) -> &'static str {
        match self {
            Source::Auto => "자동",
            Source::PureThermal => "PureThermal (Lepton)",
            Source::P3 => "Thermal Master P3",
        }
    }

    pub fn allows(self, k: Kind) -> bool {
        match self {
            Source::Auto => true,
            Source::PureThermal => k == Kind::PureThermal,
            Source::P3 => k == Kind::P3,
        }
    }

    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or_default()
    }
}

/// What the worker applies on every connect and keeps applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub source: Source,
    pub shutter_mode: lepton::ShutterMode,
    pub gain: p3::Gain,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_round_trips_and_filters() {
        for s in Source::ALL {
            assert_eq!(Source::from_u8(s.to_u8()), s);
        }
        assert!(Source::Auto.allows(Kind::P3) && Source::Auto.allows(Kind::PureThermal));
        assert!(!Source::P3.allows(Kind::PureThermal));
        assert!(!Source::PureThermal.allows(Kind::P3));
    }
}
