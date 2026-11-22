//! Camera access: libuvc wrapper, Lepton commands, and the background camera worker.

pub mod lepton;
pub mod uvc;
pub mod worker;

pub use worker::{Camera, CameraStatus, Command, ConnState};

/// PureThermal USB IDs.
pub const VID: u16 = 0x1E4E;
pub const PID: u16 = 0x0100;
/// Lepton 3.x frame size.
pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 120;
