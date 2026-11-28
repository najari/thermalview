//! Webcam capture with the Windows Media Foundation Source Reader: list video capture devices and
//! read frames converted to RGB32.
//!
//! One of the few files with `unsafe` code (docs/DEVELOPMENT.md §7). A [`Reader`] and the COM
//! objects in it are created and used on one thread: the webcam thread.

use anyhow::Context as _;
use windows::core::{Interface as _, GUID, HSTRING, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample,
    IMFSourceReader, MFCreateAttributes, MFCreateDeviceSource, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video,
    MFVideoFormat_RGB32, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::CoTaskMemFree;

use super::Device;
use crate::video::mf::Runtime;

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
/// Preferred picture width: enough detail for a 160×120 thermal image, cheap to convert.
const WANT_WIDTH: u32 = 640;

/// Video capture devices, in the order Windows lists them.
pub fn enumerate() -> anyhow::Result<Vec<Device>> {
    let _runtime = Runtime::start()?;
    let mut out = Vec::new();
    // SAFETY: `list` receives a CoTaskMem array of `n` interface pointers. Each element is moved
    // out exactly once with ptr::read (so it is released once, when dropped), then the array
    // itself is freed with CoTaskMemFree. `_runtime` outlives every object made here.
    unsafe {
        let attrs = attributes(1)?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFEnumDeviceSources(&attrs, &mut list, &mut n).context("listing cameras failed")?;
        if !list.is_null() {
            for i in 0..n as usize {
                let Some(device) = std::ptr::read(list.add(i)) else {
                    continue;
                };
                let link = string(
                    &device,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                );
                if let Ok(link) = link {
                    let name = string(&device, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                        .unwrap_or_else(|_| link.clone());
                    out.push(Device { name, link });
                }
            }
            CoTaskMemFree(Some(list as *const _));
        }
    }
    Ok(out)
}

/// An attribute store with room for `n` values.
fn attributes(n: u32) -> anyhow::Result<IMFAttributes> {
    let mut attrs: Option<IMFAttributes> = None;
    // SAFETY: `attrs` is a valid out-pointer for the duration of the call.
    unsafe { MFCreateAttributes(&mut attrs, n)? };
    attrs.context("MFCreateAttributes returned nothing")
}

/// A string attribute of `device`.
fn string(device: &IMFActivate, key: &GUID) -> windows::core::Result<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: on success `p` points to a NUL-terminated CoTaskMem string, which is copied and
    // then freed exactly once.
    unsafe {
        device.GetAllocatedString(key, &mut p, &mut len)?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(s)
    }
}

/// A device source that is shut down when dropped. Only releasing it can leave the camera in
/// use until the program ends, so this also covers every error path of [`Reader::open`].
struct Source(IMFMediaSource);

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: plain COM call on the source this owns; shutting down twice is harmless.
        unsafe {
            let _ = self.0.Shutdown();
        }
    }
}

/// An open webcam delivering RGB32 frames.
pub struct Reader {
    // Before `_source`: the reader is released first, then the camera is shut down.
    reader: Option<IMFSourceReader>,
    _source: Source,
    width: usize,
    height: usize,
    /// Bytes per row; negative when rows are stored bottom-up. Only used when the buffer has
    /// no 2-D interface.
    stride: i32,
    /// The buffer layout has been logged (once, for orientation problems).
    logged: bool,
    // Last: dropped after the COM objects above.
    _runtime: Runtime,
}

impl Reader {
    /// Open the camera with this symbolic link and start it.
    pub fn open(link: &str) -> anyhow::Result<Self> {
        let runtime = Runtime::start()?;
        let link = HSTRING::from(link);
        // SAFETY: plain COM calls on objects created here; `link` outlives the call reading it.
        let (source, reader) = unsafe {
            let attrs = attributes(2)?;
            attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;
            attrs.SetString(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                &link,
            )?;
            let source = Source(MFCreateDeviceSource(&attrs).context("cannot open the camera")?);
            let options = attributes(1)?;
            // Lets the reader convert MJPG / YUY2 / NV12 to RGB32.
            options.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source.0, &options)
                .context("cannot read from the camera")?;
            if let Some(native) = best_native_type(&reader) {
                // Not fatal: the reader then keeps the camera's default format.
                if let Err(e) = reader.SetCurrentMediaType(STREAM, None, &native) {
                    log::warn!("webcam: format not accepted: {e}");
                }
            }
            let rgb = MFCreateMediaType()?;
            rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader
                .SetCurrentMediaType(STREAM, None, &rgb)
                .context("the camera picture cannot be converted to RGB")?;
            (source, reader)
        };
        let mut r = Self {
            reader: Some(reader),
            _source: source,
            width: 0,
            height: 0,
            stride: 0,
            logged: false,
            _runtime: runtime,
        };
        r.refresh_format()?;
        Ok(r)
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn reader(&self) -> anyhow::Result<&IMFSourceReader> {
        self.reader.as_ref().context("camera closed")
    }

    fn refresh_format(&mut self) -> anyhow::Result<()> {
        // SAFETY: plain COM calls on the reader created in `open`.
        let (size, stride) = unsafe {
            let t = self.reader()?.GetCurrentMediaType(STREAM)?;
            (
                t.GetUINT64(&MF_MT_FRAME_SIZE)?,
                t.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok(),
            )
        };
        self.width = (size >> 32) as usize;
        self.height = (size & 0xffff_ffff) as usize;
        anyhow::ensure!(
            self.width > 0 && self.height > 0,
            "the camera reports no picture size"
        );
        self.stride = stride.map_or(self.width as i32 * 4, |s| s as i32);
        Ok(())
    }

    /// Wait for the next frame and return it as (width, height, RGBA). `None` when the camera
    /// sent a gap instead of a picture.
    pub fn read(&mut self) -> anyhow::Result<Option<(usize, usize, Vec<u8>)>> {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: the out-pointers are valid for the call.
        unsafe {
            self.reader()?
                .ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))
                .context("reading from the camera failed")?;
        }
        let has = |f: i32| flags & f as u32 != 0;
        anyhow::ensure!(
            !has(MF_SOURCE_READERF_ERROR.0),
            "the camera stopped (stream error)"
        );
        anyhow::ensure!(
            !has(MF_SOURCE_READERF_ENDOFSTREAM.0),
            "the camera stream ended"
        );
        if has(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) {
            self.refresh_format()?;
        }
        let Some(sample) = sample else {
            return Ok(None);
        };
        let (w, h) = (self.width, self.height);
        let row_bytes = w * 4;
        let mut rgba = vec![0u8; w * h * 4];
        let mut put_row = |y: usize, src: &[u8]| {
            let dst = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
            for (d, s) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0)
            {
                // RGB32 is stored B, G, R, unused.
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        };
        // SAFETY: the buffer is locked while read and unlocked before it is released. With a 2-D
        // lock, row y starts at scan0 + y × pitch for `height` rows of at least |pitch| bytes,
        // and the buffer's own size (without row padding) is checked to hold `height` rows of
        // this picture; otherwise the contiguous length is checked against |stride| × height
        // first. Only `row_bytes` (≤ |pitch|) bytes of each row are read.
        unsafe {
            let buf = sample.ConvertToContiguousBuffer()?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                let mut scan0: *mut u8 = std::ptr::null_mut();
                let mut pitch = 0i32;
                b2.Lock2D(&mut scan0, &mut pitch)?;
                if !self.logged {
                    self.logged = true;
                    log::info!("webcam: 2-D buffer, pitch {pitch}");
                }
                // What the picture needs without padding; a buffer for another (smaller) size
                // must not be read as this one.
                let holds = b2.GetContiguousLength().unwrap_or(0) as usize;
                if scan0.is_null()
                    || (pitch.unsigned_abs() as usize) < row_bytes
                    || holds < row_bytes * h
                {
                    let _ = b2.Unlock2D();
                    anyhow::bail!("unexpected camera buffer layout");
                }
                for y in 0..h {
                    let row = scan0.offset(y as isize * pitch as isize);
                    put_row(y, std::slice::from_raw_parts(row, row_bytes));
                }
                b2.Unlock2D()?;
            } else {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buf.Lock(&mut data, None, Some(&mut len))?;
                if !self.logged {
                    self.logged = true;
                    log::info!("webcam: contiguous buffer, stride {}", self.stride);
                }
                let pitch = self.stride.unsigned_abs() as usize;
                if data.is_null() || pitch < row_bytes || (len as usize) < pitch * h {
                    let _ = buf.Unlock();
                    anyhow::bail!("camera buffer too small");
                }
                for y in 0..h {
                    let stored = if self.stride < 0 { h - 1 - y } else { y };
                    let row = data.add(stored * pitch);
                    put_row(y, std::slice::from_raw_parts(row, row_bytes));
                }
                buf.Unlock()?;
            }
        }
        Ok(Some((w, h, rgba)))
    }
}

/// Of the camera's own formats, the one whose width is closest to [`WANT_WIDTH`] (wider wins a
/// tie), then the highest frame rate.
fn best_native_type(reader: &IMFSourceReader) -> Option<IMFMediaType> {
    let mut best: Option<((u32, bool, u64), IMFMediaType)> = None;
    for i in 0..500 {
        // SAFETY: plain COM calls; the loop ends at the first error (MF_E_NO_MORE_TYPES).
        let Ok(t) = (unsafe { reader.GetNativeMediaType(STREAM, i) }) else {
            break;
        };
        // SAFETY: plain COM calls on a media type the reader returned.
        let (size, rate) = unsafe {
            (
                t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0),
                t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0),
            )
        };
        let width = (size >> 32) as u32;
        if width == 0 {
            continue;
        }
        let (num, den) = (rate >> 32, (rate & 0xffff_ffff).max(1));
        let millifps = num * 1000 / den;
        // Smaller is better.
        let key = (
            width.abs_diff(WANT_WIDTH),
            width < WANT_WIDTH,
            u64::MAX - millifps,
        );
        if best.as_ref().is_none_or(|(k, _)| key < *k) {
            best = Some((key, t));
        }
    }
    best.map(|(_, t)| t)
}
