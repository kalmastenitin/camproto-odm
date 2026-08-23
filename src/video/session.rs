// src/video/session.rs

use super::decode::{self, LatestFrame};
use camproto_ingest::rtsp::{RtspClient, RtspConfig};
use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// One shared multi-thread runtime for every live session, created lazily.
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("failed to start the tokio runtime for RTSP/video"))
}

/// Spawn the RTSP session + decode pipeline for one stream. The returned
/// handle, when aborted (via `LiveStream::drop`), tears the whole thing down.
pub(super) fn spawn(
    rtsp_url: String,
    stream_id: String,
    latest: LatestFrame,
) -> tokio::task::JoinHandle<()> {
    runtime().spawn(async move {
        let config = RtspConfig {
            url: rtsp_url,
            camera_id: stream_id.clone(),
            ..Default::default()
        };
        eprintln!("DEBUG rtsp url: {}", config.url);
        let (client, rx) = RtspClient::new(config);

        // Decode does blocking FFI work — must run on its own OS thread, not
        // a tokio worker. This just bridges the async broadcast to it.
        decode::spawn_decode_task(stream_id.clone(), rx, latest);

        if let Err(e) = client.run().await {
            eprintln!("[{stream_id}] RTSP session ended: {e}");
        }
    })
}

pub(super) fn spawn_playback(
    rtsp_url: String,
    stream_id: String,
    latest: LatestFrame,
    playback_start: String,
    playback_end: String,
) -> tokio::task::JoinHandle<()> {
    runtime().spawn(async move {
        let config = RtspConfig {
            url: rtsp_url,
            camera_id: stream_id.clone(),
            playback_start: Some(playback_start),
            playback_end: Some(playback_end),
            playback_scale: None, // 1x for v1
        };
        eprintln!("DEBUG rtsp playback url: {}", config.url);
        let (client, rx) = RtspClient::new(config);
        decode::spawn_decode_task(stream_id.clone(), rx, latest);
        if let Err(e) = client.run().await {
            eprintln!("[{stream_id}] RTSP playback session ended: {e}");
        }
    })
}
