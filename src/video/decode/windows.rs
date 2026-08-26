// src/video/decode/windows.rs
//
// Windows H264/H265 decoder backed by FFmpeg (libavcodec), ported from
// camproto-nvr. Tries D3D11VA hardware decode first and falls back to software
// (YUV420P) transparently. Decoded frames are converted to RGBA on this decode
// thread — same discipline as the macOS VideoToolbox path, so the UI thread
// never touches pixels — and published into `LatestFrame`.

#![cfg(target_os = "windows")]
#![allow(dead_code)]

use super::{DecodeError, LatestFrame, YuvFrame};
use crate::video::render;
use ffmpeg_sys_next::{
    AVCodecID::{AV_CODEC_ID_H264, AV_CODEC_ID_HEVC},
    *,
};
use std::ptr;

// ── get_format callback ───────────────────────────────────────────────────────
// Prefer D3D11 hardware surfaces when the decoder offers them.

unsafe extern "C" fn get_format(
    _ctx: *mut AVCodecContext,
    fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    let mut p = fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == AVPixelFormat::AV_PIX_FMT_D3D11 {
            return AVPixelFormat::AV_PIX_FMT_D3D11;
        }
        p = p.add(1);
    }
    *fmts
}

// ── Decoder ───────────────────────────────────────────────────────────────────

pub struct FfmpegDecoder {
    ctx: *mut AVCodecContext,
    pkt: *mut AVPacket,
    frame: *mut AVFrame,
    sw_frame: *mut AVFrame,
    annexb_buf: Vec<u8>,
    // Reusable plane buffers to prevent per-frame heap allocations
    y_buf: Vec<u8>,
    u_buf: Vec<u8>,
    v_buf: Vec<u8>,
    is_hw: bool,
    latest: LatestFrame,
    frames_decoded: u32,
}

// Safe: the decoder is only ever used from the single decode thread that owns it.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    pub fn new_h264(sps: &[u8], pps: &[u8], latest: LatestFrame) -> Result<Self, DecodeError> {
        unsafe { Self::create(AV_CODEC_ID_H264, &[], sps, pps, latest) }
    }

    pub fn new_h265(
        vps: &[u8],
        sps: &[u8],
        pps: &[u8],
        latest: LatestFrame,
    ) -> Result<Self, DecodeError> {
        unsafe { Self::create(AV_CODEC_ID_HEVC, vps, sps, pps, latest) }
    }

    pub fn is_hardware(&self) -> bool {
        self.is_hw
    }

    pub fn frames_decoded(&self) -> u32 {
        self.frames_decoded
    }

    unsafe fn create(
        codec_id: AVCodecID,
        vps: &[u8],
        sps: &[u8],
        pps: &[u8],
        latest: LatestFrame,
    ) -> Result<Self, DecodeError> {
        let codec = avcodec_find_decoder(codec_id);
        if codec.is_null() {
            return Err(DecodeError::InitFailed("codec not found".into()));
        }

        let ctx = avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return Err(DecodeError::InitFailed("context alloc failed".into()));
        }

        // Try D3D11VA. If it fails we run software — no error either way.
        let mut hw_dev_ctx: *mut AVBufferRef = ptr::null_mut();
        let gpu = av_hwdevice_ctx_create(
            &mut hw_dev_ctx,
            AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            ptr::null(),
            ptr::null_mut(),
            0,
        ) >= 0
            && !hw_dev_ctx.is_null();

        if gpu {
            (*ctx).hw_device_ctx = av_buffer_ref(hw_dev_ctx);
            av_buffer_unref(&mut hw_dev_ctx);
            (*ctx).get_format = Some(get_format);
            (*ctx).thread_count = 1;
        } else {
            (*ctx).thread_count = 1;
            (*ctx).skip_frame = AVDiscard::AVDISCARD_NONREF;
            (*ctx).refs = 2;
        }

        // Annex-B extradata from the SDP parameter sets.
        let extra = build_extradata(vps, sps, pps);
        if !extra.is_empty() {
            let buf = av_mallocz(extra.len() + AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8;
            if !buf.is_null() {
                ptr::copy_nonoverlapping(extra.as_ptr(), buf, extra.len());
                (*ctx).extradata = buf;
                (*ctx).extradata_size = extra.len() as i32;
            }
        }

        if avcodec_open2(ctx, codec, ptr::null_mut()) < 0 {
            avcodec_free_context(&mut (ctx as *mut _));
            return Err(DecodeError::InitFailed("avcodec_open2 failed".into()));
        }
        av_log_set_level(-8); // silence FFmpeg's stderr chatter

        let is_hw = !(*ctx).hw_device_ctx.is_null();
        let pkt = av_packet_alloc();
        let frame = av_frame_alloc();
        let sw_frame = av_frame_alloc();

        if pkt.is_null() || frame.is_null() || sw_frame.is_null() {
            avcodec_free_context(&mut (ctx as *mut _));
            return Err(DecodeError::InitFailed("alloc failed".into()));
        }

        // Pre-allocate buffer capacities for up to 1080p stream frames
        Ok(Self {
            ctx,
            pkt,
            frame,
            sw_frame,
            annexb_buf: Vec::with_capacity(512 * 1024),
            y_buf: Vec::with_capacity(1920 * 1080),
            u_buf: Vec::with_capacity(960 * 540),
            v_buf: Vec::with_capacity(960 * 540),
            is_hw,
            latest,
            frames_decoded: 0,
        })
    }

    pub fn send_packet(&mut self, data: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        unsafe {
            // Incoming NALs are AVCC (4-byte length prefix); FFmpeg wants Annex-B.
            avcc_to_annexb_into(data, &mut self.annexb_buf);

            av_packet_unref(self.pkt);
            (*self.pkt).data = self.annexb_buf.as_ptr() as *mut u8;
            (*self.pkt).size = self.annexb_buf.len() as i32;
            (*self.pkt).pts = pts_us as i64;
            (*self.pkt).buf = ptr::null_mut();

            let ret = avcodec_send_packet(self.ctx, self.pkt);
            // Detach our borrowed buffer before it goes out of scope.
            (*self.pkt).data = ptr::null_mut();
            (*self.pkt).size = 0;

            if ret < 0 {
                if is_eagain(ret) {
                    // Decoder is full: drain, then retry the send as a flush.
                    self.drain_frames()?;
                    avcodec_send_packet(self.ctx, ptr::null());
                } else {
                    return Err(DecodeError::SendFailed(format!(
                        "avcodec_send_packet: {ret}"
                    )));
                }
            }
            self.drain_frames()
        }
    }

    unsafe fn drain_frames(&mut self) -> Result<(), DecodeError> {
        loop {
            av_frame_unref(self.frame);
            let ret = avcodec_receive_frame(self.ctx, self.frame);
            if is_eagain(ret) || ret == AVERROR_EOF {
                break;
            }
            if ret < 0 {
                break;
            }

            let fmt = (*self.frame).format;
            let frame_is_hw = fmt == AVPixelFormat::AV_PIX_FMT_D3D11 as i32;

            // Hardware frames live in GPU memory — pull them down to system RAM.
            let display = if frame_is_hw {
                av_frame_unref(self.sw_frame);
                let r = av_hwframe_transfer_data(self.sw_frame, self.frame, 0);
                if r < 0 {
                    continue;
                }
                self.sw_frame
            } else {
                self.frame
            };

            if let Err(e) = self.extract_yuv(display) {
                eprintln!("[FFmpeg] extract: {}", e);
            }
        }
        Ok(())
    }

    unsafe fn extract_yuv(&mut self, frame: *mut AVFrame) -> Result<(), DecodeError> {
        let w = (*frame).width as usize;
        let h = (*frame).height as usize;
        let fmt = (*frame).format;
        if w == 0 || h == 0 {
            return Ok(());
        }

        let uv_w = w / 2;
        let uv_h = h / 2;

        // Take pre-allocated buffers without allocating new heap memory
        let mut y = std::mem::take(&mut self.y_buf);
        let mut u = std::mem::take(&mut self.u_buf);
        let mut v = std::mem::take(&mut self.v_buf);

        y.clear();
        u.clear();
        v.clear();

        // Ensure capacity for higher resolution streams (e.g. 4K)
        if y.capacity() < w * h {
            y.reserve((w * h) - y.capacity());
        }
        if u.capacity() < uv_w * uv_h {
            u.reserve((uv_w * uv_h) - u.capacity());
        }
        if v.capacity() < uv_w * uv_h {
            v.reserve((uv_w * uv_h) - v.capacity());
        }

        // Full-resolution plane copy respecting stride; no downsampling.
        if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
            // NV12: full-res Y + interleaved half-res UV (hardware transfer output).
            let y_stride = (*frame).linesize[0] as usize;
            let uv_stride = (*frame).linesize[1] as usize;
            let y_ptr = (*frame).data[0];
            let uv_ptr = (*frame).data[1];
            if y_ptr.is_null() || uv_ptr.is_null() {
                self.reclaim_buffers(y, u, v);
                return Ok(());
            }

            for row in 0..h {
                y.extend_from_slice(std::slice::from_raw_parts(y_ptr.add(row * y_stride), w));
            }

            for row in 0..uv_h {
                let src = std::slice::from_raw_parts(uv_ptr.add(row * uv_stride), uv_w * 2);
                for col in 0..uv_w {
                    u.push(src[col * 2]);
                    v.push(src[col * 2 + 1]);
                }
            }
        } else if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 {
            // YUV420P: separate planes (software decode output).
            let y_stride = (*frame).linesize[0] as usize;
            let u_stride = (*frame).linesize[1] as usize;
            let v_stride = (*frame).linesize[2] as usize;
            let y_ptr = (*frame).data[0];
            let u_ptr = (*frame).data[1];
            let v_ptr = (*frame).data[2];
            if y_ptr.is_null() || u_ptr.is_null() || v_ptr.is_null() {
                self.reclaim_buffers(y, u, v);
                return Ok(());
            }

            for row in 0..h {
                y.extend_from_slice(std::slice::from_raw_parts(y_ptr.add(row * y_stride), w));
            }
            for row in 0..uv_h {
                u.extend_from_slice(std::slice::from_raw_parts(u_ptr.add(row * u_stride), uv_w));
            }
            for row in 0..uv_h {
                v.extend_from_slice(std::slice::from_raw_parts(v_ptr.add(row * v_stride), uv_w));
            }
        } else {
            self.reclaim_buffers(y, u, v);
            return Ok(()); // unexpected format, skip
        };

        self.frames_decoded += 1;

        let yuv = YuvFrame {
            y_plane: y,
            u_plane: u,
            v_plane: v,
            width: w as u32,
            height: h as u32,
        };

        // Convert to RGBA off the UI thread, then publish latest-wins.
        let rgba = render::yuv_to_rgba(&yuv);

        // Reclaim allocated vectors back into `FfmpegDecoder` for the next frame
        self.reclaim_buffers(yuv.y_plane, yuv.u_plane, yuv.v_plane);

        if let Ok(mut guard) = self.latest.lock() {
            *guard = Some(rgba);
        }
        Ok(())
    }

    #[inline]
    fn reclaim_buffers(&mut self, y: Vec<u8>, u: Vec<u8>, v: Vec<u8>) {
        self.y_buf = y;
        self.u_buf = u;
        self.v_buf = v;
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            av_frame_free(&mut self.sw_frame);
            av_packet_free(&mut self.pkt);
            avcodec_free_context(&mut self.ctx);
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[inline]
fn is_eagain(ret: i32) -> bool {
    ret == -11 || ret == ffmpeg_sys_next::AVERROR_EXIT
}

fn build_extradata(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    if !vps.is_empty() {
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(vps);
    }
    v.extend_from_slice(&[0, 0, 0, 1]);
    v.extend_from_slice(sps);
    v.extend_from_slice(&[0, 0, 0, 1]);
    v.extend_from_slice(pps);
    v
}

fn avcc_to_annexb_into(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let nal_len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if nal_len == 0 || pos + nal_len > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[pos..pos + nal_len]);
        pos += nal_len;
    }
}
