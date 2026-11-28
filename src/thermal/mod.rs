//! Device-independent thermal image processing (pure functions, unit-tested without hardware).

pub mod alarm;
pub mod calibration;
pub mod delta;
pub mod filter;
pub mod frame;
pub mod histogram;
pub mod history;
pub mod isotherm;
pub mod measure;
pub mod palette;
pub mod range;
pub mod rotate;
pub mod shapes;
pub mod upscale;

pub use frame::{FrameStats, ThermalFrame};
