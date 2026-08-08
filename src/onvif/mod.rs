// src/onvif/mod.rs

mod config;
mod device;
mod media;
mod snapshot;
mod soap;
mod types;

pub use config::{reboot, set_datetime, set_hostname, set_identification, set_network, set_ntp};
pub use snapshot::capture;
pub use types::{
    Credentials, DateTimeMode, DeviceDetails, MediaProfile, NetworkConfig, NetworkSetting,
    NtpConfig, Snapshot, SystemDateTime,
};

use soap::{build_agent, device_created};

/// Authenticate and fetch device info + media profiles (with RTSP stream URIs).
pub fn connect(service_uri: &str, creds: &Credentials) -> Result<DeviceDetails, String> {
    let agent = build_agent();
    let created = device_created(&agent, service_uri)
        .ok_or_else(|| "couldn't read the device clock (needed for authentication)".to_string())?;

    let info = device::get_device_information(&agent, service_uri, creds, &created)?;

    // The media service may live at its own endpoint (GetServices tells us).
    // Many devices route everything through the device service, so fall back.
    let media_uri = device::get_media_uri(&agent, service_uri, creds, &created)
        .unwrap_or_else(|| service_uri.to_string());

    let mut profiles = media::get_profiles(&agent, &media_uri, creds, &created)?;
    let datetime = device::get_datetime(&agent, service_uri, creds, &created);
    let network = device::get_network(&agent, service_uri, creds, &created);
    for p in &mut profiles {
        p.rtsp_uri = media::get_stream_uri(&agent, &media_uri, creds, &created, &p.token).ok();
        p.snapshot_uri =
            media::get_snapshot_uri(&agent, &media_uri, creds, &created, &p.token).ok();
    }

    let ntp = device::get_ntp(&agent, service_uri, creds, &created);

    Ok(DeviceDetails {
        info,
        profiles,
        datetime,
        network,
        ntp,
    })
}
