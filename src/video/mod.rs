// src/video/mod.rs

mod decode;
mod render;
mod session;

pub use render::RgbaFrame;

use decode::LatestFrame;

/// A running live-view session for one RTSP stream. Owns the async RTSP task
/// and the blocking decode thread; dropping it tears both down.
pub struct LiveStream {
    latest: LatestFrame,
    task: tokio::task::JoinHandle<()>,
}

impl LiveStream {
    /// Start pulling and decoding `rtsp_url` (credentials already embedded,
    /// e.g. via `with_credentials`). `stream_id` is just a label for logs.
    pub fn start(rtsp_url: String, stream_id: String) -> LiveStream {
        let latest = decode::new_latest_frame();
        let task = session::spawn(rtsp_url, stream_id, latest.clone());
        LiveStream { latest, task }
    }

    /// Take the most recently decoded frame, if a new one landed since the
    /// last call. Cheap: a mutex lock + `Option::take`.
    pub fn take_frame(&self) -> Option<RgbaFrame> {
        self.latest.lock().ok().and_then(|mut g| g.take())
    }

    pub fn start_playback(
        rtsp_url: String,
        stream_id: String,
        start: String,
        end: String,
    ) -> LiveStream {
        let latest = decode::new_latest_frame();
        let task = session::spawn_playback(rtsp_url, stream_id, latest.clone(), start, end);
        LiveStream { latest, task }
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Embed credentials into an RTSP URL's authority, e.g.
/// `rtsp://host/path` + (user, pass) -> `rtsp://user:pass@host/path`.
/// No-op if the URL isn't `rtsp://` or both credentials are empty.
pub fn with_credentials(rtsp_url: &str, username: &str, password: &str) -> String {
    if username.is_empty() && password.is_empty() {
        return rtsp_url.to_string();
    }
    match rtsp_url.strip_prefix("rtsp://") {
        Some(rest) => format!(
            "rtsp://{}:{}@{rest}",
            pct_encode(username),
            pct_encode(password)
        ),
        None => rtsp_url.to_string(),
    }
}

/// Minimal percent-encoding for URL userinfo — escapes characters that would
/// otherwise break `user:pass@host` framing.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
