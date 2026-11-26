//! H.264/MP4 encoding with the encoder built into Windows (Media Foundation sink writer).
//!
//! This file, `webcam/mf.rs` and the camera driver are the only places with `unsafe` code
//! (docs/DEVELOPMENT.md §7). Every object here is created and used on one thread: the MP4
//! writer thread.

use std::path::Path;

use anyhow::Context as _;
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_High, IMFAttributes, IMFByteStream, IMFMediaType, IMFSinkWriter,
    MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Video, MFNominalRange_16_235, MFShutdown, MFStartup,
    MFTranscodeContainerType_MPEG4, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT601, MFSTARTUP_FULL, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_TRANSCODE_CONTAINERTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// COM and Media Foundation started on the current thread, stopped again on drop. Keep it on
/// that thread and drop it after every Media Foundation object made there.
pub(crate) struct Runtime {
    com: bool,
}

impl Runtime {
    pub(crate) fn start() -> anyhow::Result<Self> {
        // SAFETY: no pointers are passed. A successful call (S_OK or S_FALSE) is balanced by
        // CoUninitialize in Drop; Runtime lives inside H264Writer, which stays on this thread.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        // SAFETY: no pointers; balanced by MFShutdown in Drop.
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            if com {
                // SAFETY: balances the successful CoInitializeEx above, on the same thread.
                unsafe { CoUninitialize() };
            }
            return Err(e).context(
                "Media Foundation is not available (Windows N editions need the Media Feature Pack)",
            );
        }
        Ok(Self { com })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: balances MFStartup / CoInitializeEx from `start`, on the same thread, after
        // every Media Foundation object (declared before `_runtime` in H264Writer) was released.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// Pack two u32 into one MF attribute value (`MFSetAttributeSize`/`MFSetAttributeRatio`).
fn pack(hi: u32, lo: u32) -> u64 {
    (hi as u64) << 32 | lo as u64
}

/// Writes NV12 frames to an H.264 MP4 file.
pub struct H264Writer {
    writer: IMFSinkWriter,
    stream: u32,
    frame_len: usize,
    // Last: dropped after the COM objects above.
    _runtime: Runtime,
}

impl H264Writer {
    /// Create `path` for `width`×`height` frames (both even). `fps` is the nominal rate for the
    /// file header; each frame carries its own time.
    pub fn create(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            width.is_multiple_of(2) && height.is_multiple_of(2),
            "frame size must be even"
        );
        let runtime = Runtime::start()?;
        let url = HSTRING::from(path.as_os_str());
        // SAFETY: plain COM calls on objects created here. Every GUID argument points to a
        // constant; `url` outlives the call that reads it.
        let (writer, stream) = unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.context("MFCreateAttributes returned nothing")?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let writer = MFCreateSinkWriterFromURL(&url, None::<&IMFByteStream>, &attrs)
                .context("cannot create the MP4 file")?;

            let set_common = |t: &IMFMediaType| -> windows::core::Result<()> {
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
                t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32)?;
                t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
            };

            let out = MFCreateMediaType()?;
            set_common(&out)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            let stream = writer.AddStream(&out).context("H.264 output rejected")?;

            let input = MFCreateMediaType()?;
            set_common(&input)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            writer
                .SetInputMediaType(stream, &input, None::<&IMFAttributes>)
                .context("the H.264 encoder does not accept NV12 input")?;
            writer.BeginWriting()?;
            (writer, stream)
        };
        Ok(Self {
            writer,
            stream,
            frame_len: width as usize * height as usize * 3 / 2,
            _runtime: runtime,
        })
    }

    /// Encode one NV12 frame shown at `time` for `duration` (both in 100 ns units).
    pub fn write(&mut self, nv12: &[u8], time: i64, duration: i64) -> anyhow::Result<()> {
        anyhow::ensure!(nv12.len() == self.frame_len, "wrong NV12 frame size");
        let len = self.frame_len as u32;
        // SAFETY: the buffer is created with room for `len` bytes. Lock hands out a pointer to
        // that memory, which is written only between Lock and Unlock and only `len` bytes.
        unsafe {
            let buf = MFCreateMemoryBuffer(len)?;
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut max = 0u32;
            buf.Lock(&mut data, Some(&mut max), None)?;
            if data.is_null() || max < len {
                let _ = buf.Unlock();
                anyhow::bail!("media buffer too small");
            }
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, nv12.len());
            buf.Unlock()?;
            buf.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(self.stream, &sample)?;
        }
        Ok(())
    }

    /// Flush the encoder and write the MP4 index. Without this the file does not play.
    pub fn finish(self) -> anyhow::Result<()> {
        // SAFETY: plain COM call on the writer created in `create`.
        unsafe { self.writer.Finalize() }.context("finishing the MP4 file failed")
    }
}
