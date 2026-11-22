//! Device-independent thermal image processing (pure functions, unit-tested without hardware).

pub mod frame;
pub mod measure;
pub mod palette;
pub mod range;

pub use frame::{FrameStats, ThermalFrame};
