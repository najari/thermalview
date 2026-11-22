//! Safe wrapper over the vendored libuvc. All libuvc `unsafe` in the app lives in this file.

use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::Arc;

use libuvc_sys as sys;

/// libuvc error code with its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UvcError(pub i32);

impl UvcError {
    pub fn is_access(self) -> bool {
        self.0 == sys::UVC_ERROR_ACCESS || self.0 == sys::UVC_ERROR_BUSY
    }
    pub fn is_not_found(self) -> bool {
        self.0 == sys::UVC_ERROR_NOT_FOUND || self.0 == sys::UVC_ERROR_NO_DEVICE
    }
}

impl std::fmt::Display for UvcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: uvc_strerror returns a pointer to a static NUL-terminated string for any code.
        let msg = unsafe { CStr::from_ptr(sys::uvc_strerror(self.0)) };
        write!(f, "{} ({})", msg.to_string_lossy(), self.0)
    }
}

impl std::error::Error for UvcError {}

fn check(code: sys::uvc_error_t) -> Result<(), UvcError> {
    if code == sys::UVC_SUCCESS {
        Ok(())
    } else {
        Err(UvcError(code))
    }
}

struct ContextInner(*mut sys::uvc_context_t);

// SAFETY: a uvc_context_t is only used to find devices and is torn down once, from Drop, after
// every Device/Handle holding an Arc to it is gone. libusb contexts are thread-safe.
unsafe impl Send for ContextInner {}
unsafe impl Sync for ContextInner {}

impl Drop for ContextInner {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful uvc_init and nothing else references it.
        unsafe { sys::uvc_exit(self.0) }
    }
}

/// A libuvc context (owns a libusb context and its event thread).
#[derive(Clone)]
pub struct Context(Arc<ContextInner>);

impl Context {
    pub fn new() -> Result<Self, UvcError> {
        let mut ctx = std::ptr::null_mut();
        // SAFETY: ctx is a valid out-pointer; a null usb_ctx makes libuvc create its own.
        check(unsafe { sys::uvc_init(&mut ctx, std::ptr::null_mut()) })?;
        Ok(Self(Arc::new(ContextInner(ctx))))
    }

    /// Find the first device with the given VID/PID.
    pub fn find_device(&self, vid: u16, pid: u16) -> Result<Device, UvcError> {
        let mut dev = std::ptr::null_mut();
        // SAFETY: the context is alive; dev is a valid out-pointer. On success we own one reference.
        check(unsafe {
            sys::uvc_find_device(
                self.0 .0,
                &mut dev,
                vid as i32,
                pid as i32,
                std::ptr::null(),
            )
        })?;
        Ok(Device {
            ctx: self.clone(),
            dev,
        })
    }
}

/// USB descriptor strings, read before opening (WinUSB allows only one open).
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    /// PureThermal firmware reports its version here (e.g. "v1.3.1").
    pub serial: String,
}

/// A found (not yet opened) device.
pub struct Device {
    ctx: Context,
    dev: *mut sys::uvc_device_t,
}

// SAFETY: a uvc_device_t is a refcounted libusb device; it may be used and released from any thread.
unsafe impl Send for Device {}

impl Device {
    pub fn info(&self) -> Result<DeviceInfo, UvcError> {
        let mut desc = std::ptr::null_mut();
        // SAFETY: dev holds a reference; desc is a valid out-pointer freed below.
        check(unsafe { sys::uvc_get_device_descriptor(self.dev, &mut desc) })?;
        // SAFETY: desc is non-null after success; its string fields are NUL-terminated or null.
        let info = unsafe {
            let d = &*desc;
            let s = |p: *const c_char| {
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            DeviceInfo {
                vid: d.idVendor,
                pid: d.idProduct,
                manufacturer: s(d.manufacturer),
                product: s(d.product),
                serial: s(d.serialNumber),
            }
        };
        // SAFETY: desc came from uvc_get_device_descriptor and is freed exactly once.
        unsafe { sys::uvc_free_device_descriptor(desc) };
        Ok(info)
    }

    /// Open the device. Fails with an access error if another program holds it.
    pub fn open(self) -> Result<Handle, UvcError> {
        let mut devh = std::ptr::null_mut();
        // SAFETY: dev holds a reference; devh is a valid out-pointer.
        check(unsafe { sys::uvc_open(self.dev, &mut devh) })?;
        Ok(Handle {
            _dev: self,
            devh,
            callback: None,
        })
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: releases the one reference this Device owns; the context outlives it (Arc).
        unsafe { sys::uvc_unref_device(self.dev) };
        let _ = &self.ctx;
    }
}

/// One assembled video frame, borrowed for the duration of the callback.
pub struct RawFrame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub format: i32,
    pub sequence: u32,
}

type FrameCallback = Box<dyn FnMut(&RawFrame<'_>) + Send + 'static>;

/// An open device. Dropping it stops streaming and closes the device.
pub struct Handle {
    // Field order matters for Drop: the explicit Drop impl closes devh first, then the Device
    // (and its context reference) is released.
    _dev: Device,
    devh: *mut sys::uvc_device_handle_t,
    callback: Option<Box<FrameCallback>>,
}

// SAFETY: libuvc device handles may be used from any thread as long as calls are serialized,
// which `&mut self` (or an outer Mutex) guarantees.
unsafe impl Send for Handle {}

unsafe extern "C" fn frame_trampoline(frame: *mut sys::uvc_frame_t, user: *mut c_void) {
    if frame.is_null() || user.is_null() {
        return;
    }
    // SAFETY: user is the Box<FrameCallback> registered in start_stream, kept alive until after
    // uvc_stop_streaming has joined this callback thread. frame is valid for this call only.
    let (cb, f) = unsafe { (&mut *(user as *mut FrameCallback), &*frame) };
    if f.data.is_null() {
        return;
    }
    // SAFETY: libuvc guarantees data points to data_bytes readable bytes during the callback.
    let data = unsafe { std::slice::from_raw_parts(f.data as *const u8, f.data_bytes) };
    let raw = RawFrame {
        data,
        width: f.width,
        height: f.height,
        format: f.frame_format,
        sequence: f.sequence,
    };
    // A panic must not unwind into C.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&raw)));
}

impl Handle {
    /// Negotiate `format` at `width`x`height` and start isochronous streaming. `on_frame` runs on
    /// libuvc's callback thread for every frame; keep it short.
    pub fn start_stream(
        &mut self,
        format: i32,
        width: u32,
        height: u32,
        on_frame: impl FnMut(&RawFrame<'_>) + Send + 'static,
    ) -> Result<(), UvcError> {
        if self.callback.is_some() {
            self.stop_stream();
        }
        let mut ctrl = sys::uvc_stream_ctrl_t::default();
        // SAFETY: devh is open; ctrl is a valid out-struct.
        check(unsafe {
            sys::uvc_get_stream_ctrl_format_size(
                self.devh,
                &mut ctrl,
                format,
                width as i32,
                height as i32,
                0,
            )
        })?;
        let mut cb: Box<FrameCallback> = Box::new(Box::new(on_frame));
        let user = &mut *cb as *mut FrameCallback as *mut c_void;
        // SAFETY: devh is open; user stays valid because the Box is stored in self.callback until
        // stop_stream, which calls uvc_stop_streaming (joins the callback thread) before dropping it.
        // On failure the handle is kept open (closing here crashed GetThermal); Drop cleans up.
        check(unsafe {
            sys::uvc_start_streaming(self.devh, &mut ctrl, Some(frame_trampoline), user, 0)
        })?;
        self.callback = Some(cb);
        Ok(())
    }

    pub fn is_streaming(&self) -> bool {
        self.callback.is_some()
    }

    pub fn stop_stream(&mut self) {
        if self.callback.is_some() {
            // SAFETY: devh is open; this joins libuvc's callback thread, so the callback box is
            // no longer referenced once it returns.
            unsafe { sys::uvc_stop_streaming(self.devh) };
            self.callback = None;
        }
    }

    /// True if stopping the stream gave up on USB transfers still held by the OS. The handle
    /// then stays open until the process exits, and the device usually needs a replug.
    pub fn has_stuck_transfers(&self) -> bool {
        // SAFETY: devh is open for the lifetime of self; this only reads a flag.
        unsafe { sys::uvc_has_stuck_transfers(self.devh) != 0 }
    }

    /// UVC GET_CUR on an extension-unit control. Returns the number of bytes read.
    pub fn get_ctrl(&self, unit: u8, control: u8, buf: &mut [u8]) -> Result<usize, UvcError> {
        // SAFETY: devh is open; buf is valid for buf.len() bytes of writes.
        let r = unsafe {
            sys::uvc_get_ctrl(
                self.devh,
                unit,
                control,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as i32,
                sys::UVC_GET_CUR,
            )
        };
        if r < 0 {
            Err(UvcError(r))
        } else {
            Ok(r as usize)
        }
    }

    /// UVC SET_CUR on an extension-unit control. Returns the number of bytes written.
    pub fn set_ctrl(&self, unit: u8, control: u8, data: &[u8]) -> Result<usize, UvcError> {
        let mut tmp = data.to_vec();
        // SAFETY: devh is open; tmp is valid for tmp.len() bytes (libuvc takes a non-const pointer
        // but only reads it for SET_CUR).
        let r = unsafe {
            sys::uvc_set_ctrl(
                self.devh,
                unit,
                control,
                tmp.as_mut_ptr() as *mut c_void,
                tmp.len() as i32,
            )
        };
        if r < 0 {
            Err(UvcError(r))
        } else {
            Ok(r as usize)
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop_stream();
        // SAFETY: devh came from a successful uvc_open and is closed exactly once.
        unsafe { sys::uvc_close(self.devh) };
    }
}

pub const FORMAT_Y16: i32 = sys::UVC_FRAME_FORMAT_Y16;
