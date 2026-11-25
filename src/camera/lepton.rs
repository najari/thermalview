//! FLIR Lepton CCI commands tunnelled through the PureThermal's UVC extension units.
//!
//! Mapping (same as GetThermal): unit = module of the command ID, control = ((id & 0xFF) >> 2) + 1.
//! Lepton enums and bools are 4 bytes on the wire; "run" commands send the control number as one byte.

use super::uvc::{Handle, UvcError};

/// Lepton command IDs (from the Lepton SDK headers).
pub mod cid {
    pub const SYS_CAM_UPTIME: u16 = 0x020C;
    pub const SYS_AUX_TEMPERATURE_K: u16 = 0x0210;
    pub const SYS_FPA_TEMPERATURE_K: u16 = 0x0214;
    pub const SYS_SHUTTER_POSITION: u16 = 0x0238;
    pub const SYS_FFC_SHUTTER_MODE_OBJ: u16 = 0x023C;
    pub const SYS_RUN_FFC: u16 = 0x0242;
    pub const SYS_FFC_STATUS: u16 = 0x0244;
    pub const RAD_FLUX_LINEAR_PARAMS: u16 = 0x4EBC;
    pub const RAD_TLINEAR_ENABLE: u16 = 0x4EC0;
    pub const RAD_TLINEAR_RESOLUTION: u16 = 0x4EC4;
    pub const OEM_FLIR_PART_NUMBER: u16 = 0x481C;
    pub const OEM_SOFTWARE_VERSION: u16 = 0x4820;
}

#[derive(Debug, thiserror::Error)]
pub enum LeptonError {
    #[error("unknown Lepton module for command 0x{0:04X}")]
    UnknownModule(u16),
    #[error("USB control transfer failed: {0}")]
    Usb(#[from] UvcError),
    #[error("short transfer for command 0x{cmd:04X}: {got} of {want} bytes")]
    Short { cmd: u16, got: usize, want: usize },
}

pub type Result<T> = std::result::Result<T, LeptonError>;

/// UVC extension unit that carries a Lepton module's commands.
pub fn unit_for(cmd: u16) -> Option<u8> {
    match cmd & 0x3F00 {
        0x0100 => Some(3), // AGC
        0x0800 => Some(4), // OEM
        0x0E00 => Some(5), // RAD
        0x0200 => Some(6), // SYS
        0x0300 => Some(7), // VID
        _ => None,
    }
}

pub fn control_for(cmd: u16) -> u8 {
    (((cmd & 0x00FF) >> 2) + 1) as u8
}

pub fn get(h: &Handle, cmd: u16, buf: &mut [u8]) -> Result<()> {
    let unit = unit_for(cmd).ok_or(LeptonError::UnknownModule(cmd))?;
    let got = h.get_ctrl(unit, control_for(cmd), buf)?;
    if got != buf.len() {
        return Err(LeptonError::Short {
            cmd,
            got,
            want: buf.len(),
        });
    }
    Ok(())
}

pub fn set(h: &Handle, cmd: u16, data: &[u8]) -> Result<()> {
    let unit = unit_for(cmd).ok_or(LeptonError::UnknownModule(cmd))?;
    let got = h.set_ctrl(unit, control_for(cmd), data)?;
    if got != data.len() {
        return Err(LeptonError::Short {
            cmd,
            got,
            want: data.len(),
        });
    }
    Ok(())
}

pub fn run(h: &Handle, cmd: u16) -> Result<()> {
    let unit = unit_for(cmd).ok_or(LeptonError::UnknownModule(cmd))?;
    let control = control_for(cmd);
    let got = h.set_ctrl(unit, control, &[control])?;
    if got != 1 {
        return Err(LeptonError::Short { cmd, got, want: 1 });
    }
    Ok(())
}

pub fn get_u16(h: &Handle, cmd: u16) -> Result<u16> {
    let mut b = [0u8; 2];
    get(h, cmd, &mut b)?;
    Ok(u16::from_le_bytes(b))
}

pub fn get_u32(h: &Handle, cmd: u16) -> Result<u32> {
    let mut b = [0u8; 4];
    get(h, cmd, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub fn get_i32(h: &Handle, cmd: u16) -> Result<i32> {
    let mut b = [0u8; 4];
    get(h, cmd, &mut b)?;
    Ok(i32::from_le_bytes(b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ShutterMode {
    Manual = 0,
    Auto = 1,
    External = 2,
}

impl ShutterMode {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Manual),
            1 => Some(Self::Auto),
            2 => Some(Self::External),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "수동 (MANUAL)",
            Self::Auto => "자동 (AUTO)",
            Self::External => "외부 (EXTERNAL)",
        }
    }
}

/// LEP_SYS_FFC_SHUTTER_MODE_OBJ_T as it travels over USB (32 bytes, little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutterModeObj {
    pub raw: [u8; 32],
}

impl ShutterModeObj {
    pub fn mode(&self) -> i32 {
        i32::from_le_bytes(self.raw[0..4].try_into().unwrap_or_default())
    }
    pub fn set_mode(&mut self, m: ShutterMode) {
        self.raw[0..4].copy_from_slice(&(m as i32).to_le_bytes());
    }
    /// Milliseconds since the last FFC.
    pub fn elapsed_since_ffc_ms(&self) -> u32 {
        u32::from_le_bytes(self.raw[16..20].try_into().unwrap_or_default())
    }
    /// Auto-FFC period in milliseconds.
    pub fn ffc_period_ms(&self) -> u32 {
        u32::from_le_bytes(self.raw[20..24].try_into().unwrap_or_default())
    }
}

pub fn get_shutter_mode_obj(h: &Handle) -> Result<ShutterModeObj> {
    let mut raw = [0u8; 32];
    get(h, cid::SYS_FFC_SHUTTER_MODE_OBJ, &mut raw)?;
    Ok(ShutterModeObj { raw })
}

/// Read-modify-write: change only the shutter mode, keep the other fields as the camera has them.
pub fn set_shutter_mode(h: &Handle, mode: ShutterMode) -> Result<()> {
    let mut obj = get_shutter_mode_obj(h)?;
    obj.set_mode(mode);
    set(h, cid::SYS_FFC_SHUTTER_MODE_OBJ, &obj.raw)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfcStatus {
    WriteError,
    Error,
    Ready,
    Busy,
    Averaging,
    Unknown(i32),
}

impl FfcStatus {
    pub fn from_i32(v: i32) -> Self {
        match v {
            -2 => Self::WriteError,
            -1 => Self::Error,
            0 => Self::Ready,
            1 => Self::Busy,
            2 => Self::Averaging,
            other => Self::Unknown(other),
        }
    }
}

pub fn ffc_status(h: &Handle) -> Result<FfcStatus> {
    Ok(FfcStatus::from_i32(get_i32(h, cid::SYS_FFC_STATUS)?))
}

pub fn run_ffc(h: &Handle) -> Result<()> {
    run(h, cid::SYS_RUN_FFC)
}

/// Kelvin×100 → °C.
pub fn centi_kelvin_to_c(v: u16) -> f32 {
    v as f32 / 100.0 - 273.15
}

pub fn fpa_temp_c(h: &Handle) -> Result<f32> {
    Ok(centi_kelvin_to_c(get_u16(h, cid::SYS_FPA_TEMPERATURE_K)?))
}

pub fn aux_temp_c(h: &Handle) -> Result<f32> {
    Ok(centi_kelvin_to_c(get_u16(h, cid::SYS_AUX_TEMPERATURE_K)?))
}

/// Radiometric output scale: `Some(kelvin per count)` if TLinear is on, `None` if raw counts.
pub fn tlinear_scale(h: &Handle) -> Result<Option<f32>> {
    if get_i32(h, cid::RAD_TLINEAR_ENABLE)? == 0 {
        return Ok(None);
    }
    Ok(Some(match get_i32(h, cid::RAD_TLINEAR_RESOLUTION)? {
        0 => 0.1,
        _ => 0.01,
    }))
}

/// Scene emissivity the Lepton itself applies to TLinear (first field of
/// LEP_RAD_FLUX_LINEAR_PARAMS_T: 8 u16 values, emissivity scaled so 8192 = 1.0).
pub fn scene_emissivity(h: &Handle) -> Result<f32> {
    let mut b = [0u8; 16];
    get(h, cid::RAD_FLUX_LINEAR_PARAMS, &mut b)?;
    Ok(emissivity_from_params(&b))
}

fn emissivity_from_params(b: &[u8; 16]) -> f32 {
    u16::from_le_bytes([b[0], b[1]]) as f32 / 8192.0
}

pub fn part_number(h: &Handle) -> Result<String> {
    let mut b = [0u8; 32];
    get(h, cid::OEM_FLIR_PART_NUMBER, &mut b)?;
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    Ok(String::from_utf8_lossy(&b[..end]).trim().to_string())
}

/// Lepton software version as "gpp a.b.c / dsp a.b.c".
pub fn software_version(h: &Handle) -> Result<String> {
    let mut b = [0u8; 8];
    get(h, cid::OEM_SOFTWARE_VERSION, &mut b)?;
    Ok(format!("{}.{}.{}", b[0], b[1], b[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_mapping_matches_getthermal() {
        // SYS FFC shutter mode object: unit 6, control (0x3C >> 2) + 1 = 16
        assert_eq!(unit_for(cid::SYS_FFC_SHUTTER_MODE_OBJ), Some(6));
        assert_eq!(control_for(cid::SYS_FFC_SHUTTER_MODE_OBJ), 16);
        // run FFC: SYS 0x42 -> control 17
        assert_eq!(control_for(cid::SYS_RUN_FFC), 17);
        // RAD carries the OEM bit (0x4000) which the unit mapping ignores
        assert_eq!(unit_for(cid::RAD_TLINEAR_ENABLE), Some(5));
        assert_eq!(control_for(cid::RAD_TLINEAR_ENABLE), 49);
        assert_eq!(unit_for(cid::RAD_FLUX_LINEAR_PARAMS), Some(5));
        assert_eq!(control_for(cid::RAD_FLUX_LINEAR_PARAMS), 48);
        let mut p = [0u8; 16];
        p[0..2].copy_from_slice(&8192u16.to_le_bytes());
        assert_eq!(emissivity_from_params(&p), 1.0);
        assert_eq!(unit_for(cid::OEM_FLIR_PART_NUMBER), Some(4));
        assert_eq!(unit_for(0x1234), None);
    }

    #[test]
    fn shutter_obj_roundtrip() {
        let mut o = ShutterModeObj { raw: [0; 32] };
        o.raw[16..20].copy_from_slice(&1234u32.to_le_bytes());
        o.set_mode(ShutterMode::External);
        assert_eq!(o.mode(), 2);
        assert_eq!(o.elapsed_since_ffc_ms(), 1234);
    }
}
