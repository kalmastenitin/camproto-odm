// src/video/decode/mod.rs

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use super::render::RgbaFrame;
use camproto_ingest::frame::{Codec, MediaFrame};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

pub struct YuvFrame {
    pub y_plane: Vec<u8>,
    pub u_plane: Vec<u8>,
    pub v_plane: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub pts: u64,
}

pub type LatestFrame = Arc<Mutex<Option<RgbaFrame>>>;

pub fn new_latest_frame() -> LatestFrame {
    Arc::new(Mutex::new(None))
}

#[derive(Debug)]
pub enum DecodeError {
    InitFailed(String),
    SendFailed(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InitFailed(s) => write!(f, "init failed: {s}"),
            DecodeError::SendFailed(s) => write!(f, "send failed: {s}"),
        }
    }
}

#[cfg(target_os = "macos")]
type PlatformDecoder = macos::VideoToolboxDecoder;

#[cfg(target_os = "windows")]
type PlatformDecoder = windows::FfmpegDecoder;

// Windows and Linux decoders (FFmpeg-based) land in a follow-up step. Until
// then, this platform simply doesn't decode video — everything else
// (discovery, auth, snapshots, config) is unaffected.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
struct PlatformDecoder;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl PlatformDecoder {
    fn new_h264(_sps: &[u8], _pps: &[u8], _latest: LatestFrame) -> Result<Self, DecodeError> {
        Err(DecodeError::InitFailed(
            "Live video isn't implemented on this platform yet".into(),
        ))
    }
    fn new_h265(
        _vps: &[u8],
        _sps: &[u8],
        _pps: &[u8],
        _latest: LatestFrame,
    ) -> Result<Self, DecodeError> {
        Err(DecodeError::InitFailed(
            "Live video isn't implemented on this platform yet".into(),
        ))
    }
    fn send_packet(&mut self, _data: &[u8], _pts: u64) -> Result<(), DecodeError> {
        Ok(())
    }
}

/// Consume MediaFrames from `rx`, lazily creating the platform decoder from
/// the first keyframe's embedded parameter sets, publishing each decoded
/// frame into `latest`. Runs until the channel closes.
pub fn spawn_decode_task(
    stream_id: String,
    mut rx: tokio::sync::broadcast::Receiver<MediaFrame>,
    latest: LatestFrame,
) {
    // Bridge async -> blocking. Bounded to 1 so a slow decoder always works
    // on the newest frame rather than building a backlog.
    let (tx, rx_blocking) = mpsc::sync_channel::<MediaFrame>(1);
    let id_async = stream_id.clone();

    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let _ = tx.try_send(frame);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[{id_async}] dropped {n} frames (decode falling behind)");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let id_thread = stream_id.clone();
    std::thread::Builder::new()
        .name(format!("decode-{stream_id}"))
        .spawn(move || {
            let mut decoder: Option<PlatformDecoder> = None;

            loop {
                let frame = match rx_blocking.recv() {
                    Ok(f) => f,
                    Err(_) => break,
                };

                match &frame.codec {
                    Codec::H265 { vps, sps, pps } => {
                        if decoder.is_none() && frame.is_keyframe {
                            match PlatformDecoder::new_h265(
                                vps.as_ref(),
                                sps.as_ref(),
                                pps.as_ref(),
                                latest.clone(),
                            ) {
                                Ok(dec) => {
                                    println!("[{id_thread}] H265 decoder ready");
                                    decoder = Some(dec);
                                }
                                Err(e) => eprintln!("[{id_thread}] H265 init: {e}"),
                            }
                        }
                        if let Some(dec) = decoder.as_mut() {
                            if let Err(e) = dec.send_packet(frame.data.as_ref(), frame.pts) {
                                eprintln!("[{id_thread}] H265 decode error: {e}");
                                decoder = None; // self-heal: reinit on next keyframe
                            }
                        }
                    }
                    Codec::H264 { sps, pps } => {
                        if decoder.is_none() && frame.is_keyframe {
                            match PlatformDecoder::new_h264(
                                sps.as_ref(),
                                pps.as_ref(),
                                latest.clone(),
                            ) {
                                Ok(dec) => {
                                    println!("[{id_thread}] H264 decoder ready");
                                    decoder = Some(dec);
                                }
                                Err(e) => eprintln!("[{id_thread}] H264 init: {e}"),
                            }
                        }
                        if let Some(dec) = decoder.as_mut() {
                            if let Err(e) = dec.send_packet(frame.data.as_ref(), frame.pts) {
                                eprintln!("[{id_thread}] H264 decode error: {e}");
                                decoder = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("failed to spawn decode thread");
}
