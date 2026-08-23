// /src/video/decode/windows.rs

#![cfg(target_os = "windows")]
#![allow(non_snake_case, unused_imports, dead_code)]

use super::{DecodeError, LatestFrame, YuvFrame};
use crate::video::render;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

pub struct MediaFoundationDecoder {
    transform: IMFTransform,
    #[allow(dead_code)]
    input_stream_id: u32,
    #[allow(dead_code)]
    output_stream_id: u32,
    latest: LatestFrame,
    width: u32,
    height: u32,
}

// Send is safe here: the IMFTransform is used from a single decode thread.
unsafe impl Send for MediaFoundationDecoder {}

impl Drop for MediaFoundationDecoder {
    fn drop(&mut self) {
        // MFShutdown once per app is officially required but repeated calls are
        // no-ops. Skip it — it's global state and would break other decoders
        // if we ran multiple concurrent streams later.
    }
}

impl MediaFoundationDecoder {
    pub fn new_h264(_sps: &[u8], _pps: &[u8], latest: LatestFrame) -> Result<Self, DecodeError> {
        Self::new(MFVideoFormat_H264, latest)
    }

    pub fn new_h265(
        _vps: &[u8],
        _sps: &[u8],
        _pps: &[u8],
        latest: LatestFrame,
    ) -> Result<Self, DecodeError> {
        Self::new(MFVideoFormat_HEVC, latest)
    }

    fn new(subtype: GUID, latest: LatestFrame) -> Result<Self, DecodeError> {
        unsafe {
            // COM apartment + MF startup. Safe to call repeatedly.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_SDK_VERSION << 16 | MF_API_VERSION, MFSTARTUP_FULL)
                .map_err(|e| DecodeError::InitFailed(format!("MFStartup: {e}")))?;

            // Find a software-only decoder for our codec.
            let mut activates: Vec<IMFActivate> = Vec::new();
            let info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: subtype,
            };
            let mut count: u32 = 0;
            let mut activate_array: *mut Option<IMFActivate> = std::ptr::null_mut();
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT, // exclude async / hardware MFTs
                Some(&info),
                None,
                &mut activate_array,
                &mut count,
            )
            .map_err(|e| DecodeError::InitFailed(format!("MFTEnumEx: {e}")))?;

            if count == 0 || activate_array.is_null() {
                return Err(DecodeError::InitFailed(
                    "no software MFT found for this codec".into(),
                ));
            }

            for i in 0..count as isize {
                if let Some(a) = (*activate_array.offset(i)).take() {
                    activates.push(a);
                }
            }
            // Free the C array MFTEnumEx allocated. CoTaskMemFree lives in
            // windows::Win32::System::Com::CoTaskMemFree.
            windows::Win32::System::Com::CoTaskMemFree(Some(activate_array as *const _));

            // Activate the first candidate into a live IMFTransform.
            let transform: IMFTransform = activates[0]
                .ActivateObject()
                .map_err(|e| DecodeError::InitFailed(format!("ActivateObject: {e}")))?;

            // Every MFT reports its input/output stream IDs — usually just 0/0
            // but we look them up officially rather than assume.
            let mut input_ids = [0u32; 1];
            let mut output_ids = [0u32; 1];
            let _ = transform.GetStreamIDs(&mut input_ids, &mut output_ids); // OK to ignore E_NOTIMPL

            let input_stream_id = input_ids[0];
            let output_stream_id = output_ids[0];

            // Build and set the input media type.
            let input_type: IMFMediaType = MFCreateMediaType()
                .map_err(|e| DecodeError::InitFailed(format!("MFCreateMediaType input: {e}")))?;
            input_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| DecodeError::InitFailed(format!("SetGUID MAJOR_TYPE: {e}")))?;
            input_type
                .SetGUID(&MF_MT_SUBTYPE, &subtype)
                .map_err(|e| DecodeError::InitFailed(format!("SetGUID SUBTYPE: {e}")))?;
            input_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|e| DecodeError::InitFailed(format!("SetUINT32 INTERLACE: {e}")))?;

            transform
                .SetInputType(input_stream_id, &input_type, 0)
                .map_err(|e| DecodeError::InitFailed(format!("SetInputType: {e}")))?;

            // Enumerate candidate output types and pick the first that accepts.
            // We want NV12 (the standard planar 4:2:0 format used everywhere
            // MF talks video) — matches VideoToolbox's output format exactly.
            let mut set_output = false;
            for i in 0.. {
                match transform.GetOutputAvailableType(output_stream_id, i) {
                    Ok(candidate) => {
                        let sub = candidate.GetGUID(&MF_MT_SUBTYPE);
                        if let Ok(sub) = sub {
                            if sub == MFVideoFormat_NV12 {
                                transform
                                    .SetOutputType(output_stream_id, &candidate, 0)
                                    .map_err(|e| {
                                        DecodeError::InitFailed(format!("SetOutputType NV12: {e}"))
                                    })?;
                                set_output = true;
                                break;
                            }
                        }
                    }
                    Err(_) => break, // MF_E_NO_MORE_TYPES = we've enumerated them all
                }
            }
            if !set_output {
                return Err(DecodeError::InitFailed(
                    "no compatible NV12 output type from MFT".into(),
                ));
            }

            // Tell the MFT we're ready to stream.
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| DecodeError::InitFailed(format!("BEGIN_STREAMING: {e}")))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| DecodeError::InitFailed(format!("START_OF_STREAM: {e}")))?;

            Ok(Self {
                transform,
                input_stream_id,
                output_stream_id,
                latest,
                width: 0,
                height: 0,
            })
        }
    }

    pub fn send_packet(&mut self, data: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        unsafe {
            // Wrap the compressed bytes in an IMFSample.
            let media_buffer: IMFMediaBuffer = MFCreateMemoryBuffer(data.len() as u32)
                .map_err(|e| DecodeError::SendFailed(format!("MFCreateMemoryBuffer: {e}")))?;

            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut _max_len: u32 = 0;
            let mut _cur_len: u32 = 0;
            media_buffer
                .Lock(&mut ptr, Some(&mut _max_len), Some(&mut _cur_len))
                .map_err(|e| DecodeError::SendFailed(format!("Lock: {e}")))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            media_buffer
                .SetCurrentLength(data.len() as u32)
                .map_err(|e| DecodeError::SendFailed(format!("SetCurrentLength: {e}")))?;
            media_buffer
                .Unlock()
                .map_err(|e| DecodeError::SendFailed(format!("Unlock: {e}")))?;

            let sample: IMFSample = MFCreateSample()
                .map_err(|e| DecodeError::SendFailed(format!("MFCreateSample: {e}")))?;
            sample
                .AddBuffer(&media_buffer)
                .map_err(|e| DecodeError::SendFailed(format!("AddBuffer: {e}")))?;
            // 100-ns units per MF convention: 10 ticks per us.
            sample
                .SetSampleTime((pts_us as i64) * 10)
                .map_err(|e| DecodeError::SendFailed(format!("SetSampleTime: {e}")))?;

            self.transform
                .ProcessInput(self.input_stream_id, &sample, 0)
                .map_err(|e| DecodeError::SendFailed(format!("ProcessInput: {e}")))?;

            // Drain ready outputs. ProcessOutput returns MF_E_TRANSFORM_NEED_MORE_INPUT
            // when there's nothing to give us — that's the normal exit condition.
            loop {
                match self.pull_output() {
                    Ok(true) => continue,    // got a frame, may be more waiting
                    Ok(false) => break,      // need more input, normal
                    Err(e) => return Err(e), // real error
                }
            }
        }
        Ok(())
    }

    /// Returns Ok(true) if a frame was decoded, Ok(false) if MFT needs more
    /// input, Err on real failure.
    unsafe fn pull_output(&mut self) -> Result<bool, DecodeError> {
        // Software MFTs allocate their own output samples, so we pass an empty
        // buffer descriptor and let the MFT fill in ppSample.
        let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_stream_id,
            pSample: std::mem::ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        };
        let mut status: u32 = 0;

        let hr =
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &mut status);

        match hr {
            Ok(()) => {
                // Frame produced. Extract, deinterleave NV12, publish.
                if let Some(sample) = std::mem::ManuallyDrop::take(&mut output_buffer.pSample) {
                    self.publish_frame(&sample)?;
                }
                Ok(true)
            }
            Err(e) => {
                let code = e.code();
                // MF_E_TRANSFORM_NEED_MORE_INPUT: not an error, just "give me more"
                if code == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    Ok(false)
                }
                // MF_E_TRANSFORM_STREAM_CHANGE: the MFT wants to renegotiate the
                // output type (usually resolution change). Reset the output type
                // and retry once.
                else if code == MF_E_TRANSFORM_STREAM_CHANGE {
                    self.renegotiate_output()?;
                    Ok(true)
                } else {
                    Err(DecodeError::SendFailed(format!(
                        "ProcessOutput: 0x{:08x}",
                        code.0
                    )))
                }
            }
        }
    }

    unsafe fn renegotiate_output(&mut self) -> Result<(), DecodeError> {
        for i in 0.. {
            match self
                .transform
                .GetOutputAvailableType(self.output_stream_id, i)
            {
                Ok(t) => {
                    if let Ok(sub) = t.GetGUID(&MF_MT_SUBTYPE) {
                        if sub == MFVideoFormat_NV12 {
                            self.transform
                                .SetOutputType(self.output_stream_id, &t, 0)
                                .map_err(|e| {
                                    DecodeError::SendFailed(format!(
                                        "SetOutputType (renegotiate): {e}"
                                    ))
                                })?;
                            return Ok(());
                        }
                    }
                }
                Err(_) => break,
            }
        }
        Err(DecodeError::SendFailed(
            "no NV12 output after STREAM_CHANGE".into(),
        ))
    }

    unsafe fn publish_frame(&mut self, sample: &IMFSample) -> Result<(), DecodeError> {
        let buffer: IMFMediaBuffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| DecodeError::SendFailed(format!("ConvertToContiguousBuffer: {e}")))?;

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut _max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        buffer
            .Lock(&mut ptr, Some(&mut _max_len), Some(&mut cur_len))
            .map_err(|e| DecodeError::SendFailed(format!("Lock output: {e}")))?;

        // Extract width/height from the current output type once we have data
        // (they can change mid-stream via STREAM_CHANGE).
        if self.width == 0 || self.height == 0 {
            if let Ok(t) = self.transform.GetOutputCurrentType(self.output_stream_id) {
                if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                    // High 32 bits = width, low 32 bits = height (MF convention)
                    self.width = (size >> 32) as u32;
                    self.height = (size & 0xFFFF_FFFF) as u32;
                }
            }
        }
        let w = self.width as usize;
        let h = self.height as usize;

        if w == 0 || h == 0 || cur_len < (w * h * 3 / 2) as u32 {
            buffer.Unlock().ok();
            return Ok(()); // malformed, skip
        }

        // NV12 layout: full-res Y plane, then interleaved UV plane at half
        // both dimensions.
        let src = std::slice::from_raw_parts(ptr, cur_len as usize);
        let mut y_plane = Vec::with_capacity(w * h);
        y_plane.extend_from_slice(&src[..w * h]);

        let uv_start = w * h;
        let uv_w = (w + 1) / 2;
        let uv_h = (h + 1) / 2;
        let mut u_plane = Vec::with_capacity(uv_w * uv_h);
        let mut v_plane = Vec::with_capacity(uv_w * uv_h);
        for row in 0..uv_h {
            let row_start = uv_start + row * uv_w * 2;
            for col in 0..uv_w {
                u_plane.push(src[row_start + col * 2]);
                v_plane.push(src[row_start + col * 2 + 1]);
            }
        }

        buffer.Unlock().ok();

        // Convert to RGBA off the UI thread (same discipline as macOS path).
        let yuv = YuvFrame {
            y_plane,
            u_plane,
            v_plane,
            width: self.width,
            height: self.height,
            pts: 0,
        };
        let rgba = render::yuv_to_rgba(&yuv);
        if let Ok(mut guard) = self.latest.lock() {
            *guard = Some(rgba);
        }
        Ok(())
    }
}
