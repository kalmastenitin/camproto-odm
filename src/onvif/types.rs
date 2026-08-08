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
}

#[derive(Clone)]
pub struct DeviceDetails {
    pub info: DeviceInfo,
    pub profiles: Vec<MediaProfile>,
    pub datetime: Option<SystemDateTime>,
    pub network: Option<NetworkConfig>,
    pub ntp: Option<NtpConfig>,
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
