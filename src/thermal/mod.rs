//! Device-independent thermal image processing (pure functions, unit-tested without hardware).

pub mod alarm;
pub mod calibration;
pub mod frame;
pub mod histogram;
pub mod history;
pub mod measure;
pub mod palette;
pub mod range;
pub mod shapes;

pub use frame::{FrameStats, ThermalFrame};
