//! Raw bindings to the vendored libuvc (see `vendor/libuvc/PATCHES.md`).
//!
//! Hand-written instead of bindgen-generated so the build needs no libclang. Only the subset
//! ThermalView uses is declared; extend it from `vendor/libuvc/include/libuvc/libuvc.h`.
#![allow(non_camel_case_types, non_snake_case)]

use std::os::raw::{c_char, c_int, c_long, c_void};

// Pull in libusb1-sys so its static libusb gets linked.
pub use libusb1_sys;

pub type uvc_error_t = c_int;
pub const UVC_SUCCESS: uvc_error_t = 0;
pub const UVC_ERROR_IO: uvc_error_t = -1;
pub const UVC_ERROR_INVALID_PARAM: uvc_error_t = -2;
pub const UVC_ERROR_ACCESS: uvc_error_t = -3;
pub const UVC_ERROR_NO_DEVICE: uvc_error_t = -4;
pub const UVC_ERROR_NOT_FOUND: uvc_error_t = -5;
pub const UVC_ERROR_BUSY: uvc_error_t = -6;
pub const UVC_ERROR_TIMEOUT: uvc_error_t = -7;
pub const UVC_ERROR_PIPE: uvc_error_t = -9;
pub const UVC_ERROR_NOT_SUPPORTED: uvc_error_t = -12;
pub const UVC_ERROR_INVALID_MODE: uvc_error_t = -51;

/// `enum uvc_frame_format`
pub type uvc_frame_format = c_int;
pub const UVC_FRAME_FORMAT_ANY: uvc_frame_format = 0;
pub const UVC_FRAME_FORMAT_UYVY: uvc_frame_format = 4;
pub const UVC_FRAME_FORMAT_RGB: uvc_frame_format = 7;
pub const UVC_FRAME_FORMAT_GRAY8: uvc_frame_format = 11;
pub const UVC_FRAME_FORMAT_Y16: uvc_frame_format = 13;

/// `enum uvc_req_code`
pub type uvc_req_code = c_int;
pub const UVC_SET_CUR: uvc_req_code = 0x01;
pub const UVC_GET_CUR: uvc_req_code = 0x81;
pub const UVC_GET_MIN: uvc_req_code = 0x82;
pub const UVC_GET_MAX: uvc_req_code = 0x83;
pub const UVC_GET_LEN: uvc_req_code = 0x85;

#[repr(C)]
pub struct uvc_context_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct uvc_device_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct uvc_device_handle_t {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct timeval {
    pub tv_sec: c_long,
    pub tv_usec: c_long,
}

#[repr(C)]
pub struct uvc_device_descriptor_t {
    pub idVendor: u16,
    pub idProduct: u16,
    pub bcdUVC: u16,
    pub serialNumber: *const c_char,
    pub manufacturer: *const c_char,
    pub product: *const c_char,
}

#[repr(C)]
pub struct uvc_frame_t {
    pub data: *mut c_void,
    pub data_bytes: usize,
    pub width: u32,
    pub height: u32,
    pub frame_format: uvc_frame_format,
    pub step: usize,
    pub sequence: u32,
    pub capture_time: timeval,
    pub source: *mut uvc_device_handle_t,
    pub library_owns_data: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct uvc_stream_ctrl_t {
    pub bmHint: u16,
    pub bFormatIndex: u8,
    pub bFrameIndex: u8,
    pub dwFrameInterval: u32,
    pub wKeyFrameRate: u16,
    pub wPFrameRate: u16,
    pub wCompQuality: u16,
    pub wCompWindowSize: u16,
    pub wDelay: u16,
    pub dwMaxVideoFrameSize: u32,
    pub dwMaxPayloadTransferSize: u32,
    pub dwClockFrequency: u32,
    pub bmFramingInfo: u8,
    pub bPreferredVersion: u8,
    pub bMinVersion: u8,
    pub bMaxVersion: u8,
    pub bInterfaceNumber: u8,
}

pub type uvc_frame_callback_t =
    unsafe extern "C" fn(frame: *mut uvc_frame_t, user_ptr: *mut c_void);

extern "C" {
    pub fn uvc_init(ctx: *mut *mut uvc_context_t, usb_ctx: *mut c_void) -> uvc_error_t;
    pub fn uvc_exit(ctx: *mut uvc_context_t);

    pub fn uvc_find_device(
        ctx: *mut uvc_context_t,
        dev: *mut *mut uvc_device_t,
        vid: c_int,
        pid: c_int,
        sn: *const c_char,
    ) -> uvc_error_t;
    pub fn uvc_ref_device(dev: *mut uvc_device_t);
    pub fn uvc_unref_device(dev: *mut uvc_device_t);
    pub fn uvc_get_device_descriptor(
        dev: *mut uvc_device_t,
        desc: *mut *mut uvc_device_descriptor_t,
    ) -> uvc_error_t;
    pub fn uvc_free_device_descriptor(desc: *mut uvc_device_descriptor_t);

    pub fn uvc_open(dev: *mut uvc_device_t, devh: *mut *mut uvc_device_handle_t) -> uvc_error_t;
    pub fn uvc_close(devh: *mut uvc_device_handle_t);

    pub fn uvc_get_stream_ctrl_format_size(
        devh: *mut uvc_device_handle_t,
        ctrl: *mut uvc_stream_ctrl_t,
        format: uvc_frame_format,
        width: c_int,
        height: c_int,
        fps: c_int,
    ) -> uvc_error_t;
    pub fn uvc_start_streaming(
        devh: *mut uvc_device_handle_t,
        ctrl: *mut uvc_stream_ctrl_t,
        cb: Option<uvc_frame_callback_t>,
        user_ptr: *mut c_void,
        flags: u8,
    ) -> uvc_error_t;
    pub fn uvc_stop_streaming(devh: *mut uvc_device_handle_t);

    pub fn uvc_get_ctrl(
        devh: *mut uvc_device_handle_t,
        unit: u8,
        ctrl: u8,
        data: *mut c_void,
        len: c_int,
        req_code: uvc_req_code,
    ) -> c_int;
    pub fn uvc_set_ctrl(
        devh: *mut uvc_device_handle_t,
        unit: u8,
        ctrl: u8,
        data: *mut c_void,
        len: c_int,
    ) -> c_int;

    pub fn uvc_strerror(err: uvc_error_t) -> *const c_char;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_links_and_works() {
        // SAFETY: uvc_strerror returns a pointer to a static string for any code.
        let s = unsafe { std::ffi::CStr::from_ptr(uvc_strerror(UVC_ERROR_ACCESS)) };
        assert!(!s.to_bytes().is_empty());
    }

    #[test]
    fn init_and_exit() {
        let mut ctx = std::ptr::null_mut();
        // SAFETY: ctx is a valid out-pointer; a successful init is paired with exit.
        unsafe {
            assert_eq!(uvc_init(&mut ctx, std::ptr::null_mut()), UVC_SUCCESS);
            uvc_exit(ctx);
        }
    }
}
