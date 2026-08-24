// src/onvif/types.rs

#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
    pub serial: String,
    pub hardware_id: String,
}

#[derive(Clone)]
pub struct MediaProfile {
    pub token: String,
    pub name: String,
    pub encoding: Option<String>,
    pub resolution: Option<(u32, u32)>,
    pub rtsp_uri: Option<String>,
    pub snapshot_uri: Option<String>,
    pub source_token: Option<String>,
    pub ptz: Option<PtzInfo>,
    pub has_ptz_config: bool,

    pub imaging: Option<ImagingInfo>,
    pub video_source_token: Option<String>,
}

#[derive(Clone)]
pub struct DeviceDetails {
    pub info: DeviceInfo,
    pub profiles: Vec<MediaProfile>,
    pub datetime: Option<SystemDateTime>,
    pub network: Option<NetworkConfig>,
    pub ntp: Option<NtpConfig>,
    pub events: Option<EventsInfo>,
    pub replay: Option<ReplayInfo>,
}

pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone)]
pub struct SystemDateTime {
    pub mode: String, // "NTP" | "Manual"
    pub timezone: Option<String>,
    pub utc: Option<String>,
    pub dst: bool,
}

#[derive(Clone, Default)]
pub struct NetworkConfig {
    pub interface_token: String,
    pub mac: Option<String>,
    pub dhcp: bool,
    pub ip: Option<String>,
    pub prefix: Option<u32>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub hostname: Option<String>,
}

pub enum DateTimeMode {
    Ntp,
    SyncToHost,
}

#[derive(Clone, Default)]
pub struct NtpConfig {
    pub from_dhcp: bool,
    pub servers: Vec<String>,
}

pub struct NetworkSetting {
    pub interface_token: String,
    pub dhcp: bool,
    pub ip: String,
    pub prefix: u32,
    pub gateway: String,
    pub dns: Vec<String>,
}

#[derive(Clone)]
pub struct PtzInfo {
    pub service_uri: String,   // the PTZ service XAddr
    pub profile_token: String, // the ONVIF media profile this PTZ controls
    pub device_uri: String,
}

#[derive(Clone)]
pub struct PtzPreset {
    pub token: String,
    pub name: String,
}

// ------ Imaging types -----
#[derive(Clone)]
pub struct ImagingInfo {
    pub service_uri: String,
    pub device_uri: String,
    pub source_token: String,
}

#[derive(Clone, Default)]
pub struct ImagingSettings {
    pub brightness: Option<f32>,
    pub contrast: Option<f32>,
    pub color_saturation: Option<f32>,
    pub sharpness: Option<f32>,

    pub exposure_mode: Option<String>,
    pub exposure_time: Option<f32>,
    pub exposure_gain: Option<f32>,
    pub exposure_iris: Option<f32>,

    pub white_balance_mode: Option<String>,
    pub white_balance_cr_gain: Option<f32>,
    pub white_balance_cb_gain: Option<f32>,

    pub wdr_mode: Option<String>,
    pub wdr_level: Option<f32>,

    pub backlight_mode: Option<String>,
    pub backlight_level: Option<f32>,

    pub image_stab_mode: Option<String>,
    pub image_stab_level: Option<f32>,

    pub ir_cut_filter: Option<String>,
    pub focus_mode: Option<String>,
}

#[derive(Clone, Default)]
pub struct ImagingRanges {
    pub brightness: Option<(f32, f32)>,
    pub contrast: Option<(f32, f32)>,
    pub color_saturation: Option<(f32, f32)>,
    pub sharpness: Option<(f32, f32)>,
    pub exposure_time: Option<(f32, f32)>,
    pub exposure_gain: Option<(f32, f32)>,
    pub exposure_iris: Option<(f32, f32)>,
    pub wb_cr_gain: Option<(f32, f32)>,
    pub wb_cb_gain: Option<(f32, f32)>,
    pub wdr_level: Option<(f32, f32)>,
    pub backlight_level: Option<(f32, f32)>,
    pub image_stab_level: Option<(f32, f32)>,
    pub focus_speed: Option<(f32, f32)>,
    pub exposure_modes: Vec<String>,
    pub wb_modes: Vec<String>,
    pub wdr_modes: Vec<String>,
    pub backlight_modes: Vec<String>,
    pub image_stab_modes: Vec<String>,
    pub ir_cut_modes: Vec<String>,
    pub focus_modes: Vec<String>,
}

#[derive(Clone)]
pub struct EventsInfo {
    pub service_uri: String,
    pub device_uri: String,
}

// #[derive(Clone)]
pub struct EventNotification {
    pub received_at: std::time::SystemTime,
    pub topic: String,
    pub source: Option<String>,
    pub data: Vec<(String, String)>,
    // Kept for a planned raw-XML debug view; not yet surfaced in the UI.
    #[allow(dead_code)]
    pub raw: String,
}

#[derive(Clone)]
pub struct ReplayInfo {
    pub search_uri: String, // Recording Search service XAddr
    pub replay_uri: String, // Replay service XAddr
    pub device_uri: String, // Device service XAddr (for the clock)
}

#[derive(Clone, Debug)]
pub struct Recording {
    pub token: String,
    pub earliest: Option<String>,
    pub latest: Option<String>,
    // Kept for the planned recordings list UI; not yet surfaced there.
    #[allow(dead_code)]
    pub source_name: Option<String>,
    pub track_sources: Vec<String>,
}
