// src/onvif/mod.rs

mod config;
mod device;
mod events;
mod imaging;
mod media;
mod ptz;
mod replay;
mod snapshot;
mod soap;
mod types;

pub use config::{reboot, set_datetime, set_hostname, set_identification, set_network, set_ntp};
pub use ptz::{continuous_move, get_presets, goto_preset, remove_preset, set_preset, stop};
pub use snapshot::capture;
pub use types::{
    Credentials, DateTimeMode, DeviceDetails, MediaProfile, NetworkConfig, NetworkSetting,
    NtpConfig, Snapshot, SystemDateTime,
};
pub use types::{PtzInfo, PtzPreset};

use soap::{build_agent, device_created};

pub use events::{create_pull_subscription, pull_messages, renew_subscription, unsubscribe};
pub use imaging::{
    focus_continuous_move, focus_stop, get_imaging_options, get_imaging_settings,
    set_imaging_settings,
};
pub use replay::{get_recordings, get_replay_uri};
pub use types::Recording;
pub use types::{EventNotification, EventsInfo};
pub use types::{ImagingInfo, ImagingRanges, ImagingSettings};

/// Authenticate and fetch device info + media profiles (with RTSP stream URIs).
pub fn connect(service_uri: &str, creds: &Credentials) -> Result<DeviceDetails, String> {
    let agent = build_agent();
    let created = device_created(&agent, service_uri)
        .ok_or_else(|| "couldn't read the device clock (needed for authentication)".to_string())?;

    let info = device::get_device_information(&agent, service_uri, creds, &created)?;

    let media_uri = device::get_media_uri(&agent, service_uri, creds, &created)
        .unwrap_or_else(|| service_uri.to_string());

    let mut profiles = media::get_profiles(&agent, &media_uri, creds, &created)?;
    let events_uri = device::get_events_uri(&agent, service_uri, creds, &created);

    let ptz_uri = device::get_ptz_uri(&agent, service_uri, creds, &created);
    let imaging_uri = device::get_imaging_uri(&agent, service_uri, creds, &created);

    let datetime = device::get_datetime(&agent, service_uri, creds, &created);
    let network = device::get_network(&agent, service_uri, creds, &created);
    let replay = device::get_replay_info(&agent, service_uri, creds, &created);

    for p in &mut profiles {
        p.rtsp_uri = media::get_stream_uri(&agent, &media_uri, creds, &created, &p.token).ok();
        p.snapshot_uri =
            media::get_snapshot_uri(&agent, &media_uri, creds, &created, &p.token).ok();

        // ptz uri profile
        if let (Some(uri), true) = (ptz_uri.as_ref(), p.has_ptz_config) {
            p.ptz = Some(types::PtzInfo {
                service_uri: uri.clone(),
                device_uri: service_uri.to_string(),
                profile_token: p.token.clone(),
            });
        }

        // imaging settings uri
        if let (Some(uri), Some(src)) = (imaging_uri.as_ref(), p.video_source_token.as_ref()) {
            p.imaging = Some(types::ImagingInfo {
                service_uri: uri.clone(),
                device_uri: service_uri.to_string(),
                source_token: src.clone(),
            });
        }
    }

    let ntp = device::get_ntp(&agent, service_uri, creds, &created);

    let events = events_uri.map(|uri| types::EventsInfo {
        service_uri: uri,
        device_uri: service_uri.to_string(),
    });

    Ok(DeviceDetails {
        info,
        profiles,
        datetime,
        network,
        ntp,
        events,
        replay,
    })
}
