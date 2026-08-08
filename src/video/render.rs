// src/video/render.rs

use super::decode::YuvFrame;

pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// BT.601 full-range YUV->RGB — the standard default for consumer IP camera
/// streams (most either use BT.601 or don't signal a matrix at all).
pub fn yuv_to_rgba(frame: &YuvFrame) -> RgbaFrame {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let chroma_w = (w + 1) / 2;

    for y in 0..h {
        let y_row = &frame.y_plane[y * w..y * w + w];
        let c_row = y / 2;
        for x in 0..w {
            let yy = y_row[x] as i32;
            let ci = c_row * chroma_w + x / 2;
            let u = *frame.u_plane.get(ci).unwrap_or(&128) as i32 - 128;
            let v = *frame.v_plane.get(ci).unwrap_or(&128) as i32 - 128;

            let r = yy + ((91_881 * v) >> 16);
            let g = yy - ((22_554 * u + 46_802 * v) >> 16);
            let b = yy + ((116_130 * u) >> 16);

            let o = (y * w + x) * 4;
            rgba[o] = r.clamp(0, 255) as u8;
            rgba[o + 1] = g.clamp(0, 255) as u8;
            rgba[o + 2] = b.clamp(0, 255) as u8;
            rgba[o + 3] = 255;
        }
    }

    RgbaFrame {
        width: frame.width,
        height: frame.height,
        rgba,
    }
}
