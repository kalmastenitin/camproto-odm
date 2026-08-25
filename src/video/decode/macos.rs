// src/video/decode/macos.rs

#![cfg(target_os = "macos")]
#![allow(dead_code, unused_imports)]

use super::{DecodeError, LatestFrame, YuvFrame};
use std::ffi::c_void;

type OSStatus = i32;
type CVPixelBufferRef = *mut c_void;
type VTSessionRef = *mut c_void;
type CMFormatDescRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;

type VTDecompressionOutputCallback =
    unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CVPixelBufferRef, CMTime, CMTime);

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    fn from_pts_us(pts_us: u64) -> Self {
        CMTime {
            value: pts_us as i64,
            timescale: 1_000_000,
            flags: 1,
            epoch: 0,
        }
    }
    fn zero() -> Self {
        CMTime {
            value: 0,
            timescale: 1_000_000,
            flags: 1,
            epoch: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_timestamp: CMTime,
    decode_timestamp: CMTime,
}

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: Option<VTDecompressionOutputCallback>,
    refcon: *mut c_void,
}

// Three distinct frameworks, not a duplicate — clippy's duplicated_attributes
// lint flags repeated `#[link]` paths without looking at the `name` argument.
#[allow(clippy::duplicated_attributes)]
#[link(name = "VideoToolbox", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: *const c_void,
        parameter_set_count: usize,
        parameter_set_ptrs: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        extensions: *const c_void,
        format_desc_out: *mut CMFormatDescRef,
    ) -> OSStatus;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: *const c_void,
        parameter_set_count: usize,
        parameter_set_ptrs: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_desc_out: *mut CMFormatDescRef,
    ) -> OSStatus;

    fn VTDecompressionSessionCreate(
        allocator: *const c_void,
        video_format_desc: CMFormatDescRef,
        video_decoder_spec: *const c_void,
        dest_image_buf_attrs: *const c_void,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        session_out: *mut VTSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionDecodeFrame(
        session: VTSessionRef,
        sample_buf: CMSampleBufferRef,
        decode_flags: u32,
        source_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    fn VTDecompressionSessionInvalidate(session: VTSessionRef);

    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: *const c_void,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: *const c_void,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buf_out: *mut CMBlockBufferRef,
    ) -> OSStatus;

    fn CMSampleBufferCreateReady(
        allocator: *const c_void,
        data_buffer: CMBlockBufferRef,
        format_description: CMFormatDescRef,
        num_samples: i64,
        num_sample_timing: i64,
        sample_timing_arr: *const CMSampleTimingInfo,
        num_sample_sizes: i64,
        sample_sizes_arr: *const usize,
        sample_buf_out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn CFRelease(cf: *const c_void);

    fn CVPixelBufferLockBaseAddress(buf: CVPixelBufferRef, flags: u64) -> OSStatus;
    fn CVPixelBufferUnlockBaseAddress(buf: CVPixelBufferRef, flags: u64) -> OSStatus;
    fn CVPixelBufferGetWidth(buf: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(buf: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(buf: CVPixelBufferRef, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buf: CVPixelBufferRef, plane: usize) -> usize;
}

pub struct VideoToolboxDecoder {
    session: VTSessionRef,
    format_desc: CMFormatDescRef,
    #[allow(dead_code)]
    latest: LatestFrame,
}

unsafe impl Send for VideoToolboxDecoder {}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        unsafe {
            VTDecompressionSessionInvalidate(self.session);
            CFRelease(self.session as *const c_void);
            CFRelease(self.format_desc as *const c_void);
        }
    }
}

impl VideoToolboxDecoder {
    pub fn new_h265(
        vps: &[u8],
        sps: &[u8],
        pps: &[u8],
        latest: LatestFrame,
    ) -> Result<Self, DecodeError> {
        unsafe {
            let param_ptrs = [vps.as_ptr(), sps.as_ptr(), pps.as_ptr()];
            let param_sizes = [vps.len(), sps.len(), pps.len()];

            let mut format_desc: CMFormatDescRef = std::ptr::null_mut();
            let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                std::ptr::null(),
                3,
                param_ptrs.as_ptr(),
                param_sizes.as_ptr(),
                4,
                std::ptr::null(),
                &mut format_desc,
            );
            if status != 0 {
                return Err(DecodeError::InitFailed(format!(
                    "CMVideoFormatDescriptionCreateFromHEVCParameterSets: {status}"
                )));
            }

            let boxed = Box::new(latest.clone());
            let refcon = Box::into_raw(boxed) as *mut c_void;
            let callback_record = VTDecompressionOutputCallbackRecord {
                callback: Some(decompress_callback),
                refcon,
            };

            let mut session: VTSessionRef = std::ptr::null_mut();
            let status = VTDecompressionSessionCreate(
                std::ptr::null(),
                format_desc,
                std::ptr::null(),
                std::ptr::null(),
                &callback_record,
                &mut session,
            );
            if status != 0 {
                CFRelease(format_desc as *const c_void);
                return Err(DecodeError::InitFailed(format!(
                    "VTDecompressionSessionCreate (H265): {status}"
                )));
            }

            Ok(Self {
                session,
                format_desc,
                latest,
            })
        }
    }

    pub fn new_h264(sps: &[u8], pps: &[u8], latest: LatestFrame) -> Result<Self, DecodeError> {
        unsafe {
            let param_ptrs = [sps.as_ptr(), pps.as_ptr()];
            let param_sizes = [sps.len(), pps.len()];

            let mut format_desc: CMFormatDescRef = std::ptr::null_mut();
            let status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                std::ptr::null(),
                2,
                param_ptrs.as_ptr(),
                param_sizes.as_ptr(),
                4,
                &mut format_desc,
            );
            if status != 0 {
                return Err(DecodeError::InitFailed(format!(
                    "CMVideoFormatDescriptionCreateFromH264ParameterSets: {status}"
                )));
            }

            let boxed = Box::new(latest.clone());
            let refcon = Box::into_raw(boxed) as *mut c_void;
            let callback_record = VTDecompressionOutputCallbackRecord {
                callback: Some(decompress_callback),
                refcon,
            };

            let mut session: VTSessionRef = std::ptr::null_mut();
            let status = VTDecompressionSessionCreate(
                std::ptr::null(),
                format_desc,
                std::ptr::null(),
                std::ptr::null(),
                &callback_record,
                &mut session,
            );
            if status != 0 {
                CFRelease(format_desc as *const c_void);
                return Err(DecodeError::InitFailed(format!(
                    "VTDecompressionSessionCreate (H264): {status}"
                )));
            }

            Ok(Self {
                session,
                format_desc,
                latest,
            })
        }
    }

    pub fn send_packet(&mut self, data: &[u8], pts: u64) -> Result<(), DecodeError> {
        unsafe {
            let mut data_copy = data.to_vec();
            let ptr = data_copy.as_mut_ptr();
            let len = data_copy.len();
            std::mem::forget(data_copy); // leaked to CoreMedia; freed via its default allocator

            let mut block_buf: CMBlockBufferRef = std::ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                ptr as *mut c_void,
                len,
                std::ptr::null(),
                std::ptr::null(),
                0,
                len,
                0,
                &mut block_buf,
            );
            if status != 0 {
                return Err(DecodeError::SendFailed(format!(
                    "CMBlockBufferCreateWithMemoryBlock: {status}"
                )));
            }

            let timing = CMSampleTimingInfo {
                duration: CMTime::zero(),
                presentation_timestamp: CMTime::from_pts_us(pts),
                decode_timestamp: CMTime::from_pts_us(pts),
            };
            let sample_size = data.len();
            let mut sample_buf: CMSampleBufferRef = std::ptr::null_mut();

            let status = CMSampleBufferCreateReady(
                std::ptr::null(),
                block_buf,
                self.format_desc,
                1,
                1,
                &timing,
                1,
                &sample_size,
                &mut sample_buf,
            );
            CFRelease(block_buf as *const c_void);

            if status != 0 {
                return Err(DecodeError::SendFailed(format!(
                    "CMSampleBufferCreateReady: {status}"
                )));
            }

            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buf,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            CFRelease(sample_buf as *const c_void);

            if status != 0 {
                return Err(DecodeError::SendFailed(format!(
                    "VTDecompressionSessionDecodeFrame: {status}"
                )));
            }

            Ok(())
        }
    }
}

unsafe extern "C" fn decompress_callback(
    refcon: *mut c_void,
    _source_ref: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    image_buffer: CVPixelBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    if status != 0 || image_buffer.is_null() {
        return;
    }

    CVPixelBufferLockBaseAddress(image_buffer, 0);

    let width = CVPixelBufferGetWidth(image_buffer) as u32;
    let height = CVPixelBufferGetHeight(image_buffer) as u32;

    // Y: full resolution, straight row copy (fixed vs. the original's every-
    // other-pixel skip, which was the source of the motion-fading artifact —
    // luma should never be subsampled in 4:2:0, only chroma is).
    let y_ptr = CVPixelBufferGetBaseAddressOfPlane(image_buffer, 0);
    let y_stride = CVPixelBufferGetBytesPerRowOfPlane(image_buffer, 0);
    let mut y_plane = Vec::with_capacity((width * height) as usize);
    for row in 0..height as usize {
        let src = std::slice::from_raw_parts(y_ptr.add(row * y_stride), width as usize);
        y_plane.extend_from_slice(src);
    }

    // UV: NV12 interleaved, naturally half-resolution in both dimensions —
    // that's standard 4:2:0 chroma, not an extra downsample. De-interleave
    // into separate U/V planes at that natural resolution.
    let uv_ptr = CVPixelBufferGetBaseAddressOfPlane(image_buffer, 1);
    let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(image_buffer, 1);
    let uv_width = width.div_ceil(2) as usize;
    let uv_height = height.div_ceil(2) as usize;

    let mut u_plane = Vec::with_capacity(uv_width * uv_height);
    let mut v_plane = Vec::with_capacity(uv_width * uv_height);
    for row in 0..uv_height {
        let row_bytes = (uv_width * 2).min(uv_stride);
        let src = std::slice::from_raw_parts(uv_ptr.add(row * uv_stride), row_bytes);
        for col in 0..uv_width {
            u_plane.push(src[col * 2]);
            v_plane.push(src[col * 2 + 1]);
        }
    }

    CVPixelBufferUnlockBaseAddress(image_buffer, 0);

    let yuv = YuvFrame {
        y_plane,
        u_plane,
        v_plane,
        width,
        height,
    };
    let rgba = super::super::render::yuv_to_rgba(&yuv);

    let latest = &*(refcon as *const LatestFrame);
    if let Ok(mut guard) = latest.lock() {
        *guard = Some(rgba);
    }
}
