// src/onvif/device.rs

use super::soap::{soap_call, NS_DEVICE, NS_MEDIA};
use super::types::{Credentials, DeviceInfo, NetworkConfig, NtpConfig, SystemDateTime};

pub(super) fn get_device_information(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Result<DeviceInfo, String> {
    let action = format!("{NS_DEVICE}/GetDeviceInformation");
    let resp = soap_call(
        agent,
        uri,
        &action,
        creds,
        created,
        "<tds:GetDeviceInformation/>",
    )?;

    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    let get = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Ok(DeviceInfo {
        manufacturer: get("Manufacturer"),
        model: get("Model"),
        firmware: get("FirmwareVersion"),
        serial: get("SerialNumber"),
        hardware_id: get("HardwareId"),
    })
}

/// Locate the Media (ver10) service endpoint via GetServices.
pub(super) fn get_media_uri(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<String> {
    let action = format!("{NS_DEVICE}/GetServices");
    let body =
        "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
    let resp = soap_call(agent, uri, &action, creds, created, body).ok()?;

    let doc = roxmltree::Document::parse(&resp).ok()?;
    for svc in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Service")
    {
        let ns = svc
            .children()
            .find(|n| n.tag_name().name() == "Namespace")
            .and_then(|n| n.text())
            .unwrap_or("");
        if ns == NS_MEDIA {
            return svc
                .children()
                .find(|n| n.tag_name().name() == "XAddr")
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string());
        }
    }
    None
}

pub(super) fn get_datetime(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<SystemDateTime> {
    let action = format!("{NS_DEVICE}/GetSystemDateAndTime");
    let resp = soap_call(
        agent,
        uri,
        &action,
        creds,
        created,
        "<tds:GetSystemDateAndTime/>",
    )
    .ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;

    let find = |name: &str| {
        doc.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
    };
    let utc = doc
        .descendants()
        .find(|n| n.tag_name().name() == "UTCDateTime")
        .and_then(|dt| {
            let f = |name: &str| {
                dt.descendants()
                    .find(|n| n.tag_name().name() == name)
                    .and_then(|n| n.text())
                    .and_then(|t| t.trim().parse::<u32>().ok())
            };
            Some(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
                f("Year")?,
                f("Month")?,
                f("Day")?,
                f("Hour").unwrap_or(0),
                f("Minute").unwrap_or(0),
                f("Second").unwrap_or(0)
            ))
        });

    Some(SystemDateTime {
        mode: find("DateTimeType").unwrap_or_else(|| "Manual".into()),
        timezone: find("TZ"),
        utc,
        dst: find("DaylightSavings")
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    })
}

pub(super) fn get_network(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<NetworkConfig> {
    let action = format!("{NS_DEVICE}/GetNetworkInterfaces");
    let resp = soap_call(
        agent,
        uri,
        &action,
        creds,
        created,
        "<tds:GetNetworkInterfaces/>",
    )
    .ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;

    let iface = doc
        .descendants()
        .find(|n| n.tag_name().name() == "NetworkInterfaces")?;
    let token = iface.attribute("token").unwrap_or_default().to_string();
    let mac = iface
        .descendants()
        .find(|n| n.tag_name().name() == "HwAddress")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string());

    let ipv4 = iface.descendants().find(|n| n.tag_name().name() == "IPv4");
    let dhcp = ipv4
        .and_then(|v| v.descendants().find(|n| n.tag_name().name() == "DHCP"))
        .and_then(|n| n.text())
        .map(|t| t.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let ip = ipv4
        .and_then(|v| v.descendants().find(|n| n.tag_name().name() == "Address"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string());
    let prefix = ipv4
        .and_then(|v| {
            v.descendants()
                .find(|n| n.tag_name().name() == "PrefixLength")
        })
        .and_then(|n| n.text())
        .and_then(|t| t.trim().parse::<u32>().ok());

    Some(NetworkConfig {
        interface_token: token,
        mac,
        dhcp,
        ip,
        prefix,
        gateway: get_default_gateway(agent, uri, creds, created),
        dns: get_dns(agent, uri, creds, created),
        hostname: get_hostname(agent, uri, creds, created),
    })
}

fn get_default_gateway(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<String> {
    let action = format!("{NS_DEVICE}/GetNetworkDefaultGateway");
    let resp = soap_call(
        agent,
        uri,
        &action,
        creds,
        created,
        "<tds:GetNetworkDefaultGateway/>",
    )
    .ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "IPv4Address")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn get_dns(agent: &ureq::Agent, uri: &str, creds: &Credentials, created: &str) -> Vec<String> {
    let action = format!("{NS_DEVICE}/GetDNS");
    let Ok(resp) = soap_call(agent, uri, &action, creds, created, "<tds:GetDNS/>") else {
        return Vec::new();
    };
    let Ok(doc) = roxmltree::Document::parse(&resp) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.tag_name().name() == "IPv4Address")
        .filter_map(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub(super) fn get_ntp(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<NtpConfig> {
    let resp = soap_call(
        agent,
        uri,
        &format!("{NS_DEVICE}/GetNTP"),
        creds,
        created,
        "<tds:GetNTP/>",
    )
    .ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;
    let from_dhcp = doc
        .descendants()
        .find(|n| n.tag_name().name() == "FromDHCP")
        .and_then(|n| n.text())
        .map(|t| t.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let servers = doc
        .descendants()
        .filter(|n| matches!(n.tag_name().name(), "NTPManual" | "NTPFromDHCP"))
        .filter_map(|h| {
            h.descendants()
                .find(|n| matches!(n.tag_name().name(), "IPv4Address" | "DNSname"))
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .collect();
    Some(NtpConfig { from_dhcp, servers })
}

fn get_hostname(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<String> {
    let resp = soap_call(
        agent,
        uri,
        &format!("{NS_DEVICE}/GetHostname"),
        creds,
        created,
        "<tds:GetHostname/>",
    )
    .ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;
    doc.descendants()
        .find(|n| n.tag_name().name() == "Name")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(super) fn get_ptz_uri(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<String> {
    use super::soap::NS_PTZ;
    let action = format!("{NS_DEVICE}/GetServices");
    let body =
        "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
    let resp = soap_call(agent, uri, &action, creds, created, body).ok()?;

    let doc = roxmltree::Document::parse(&resp).ok()?;
    for svc in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Service")
    {
        let ns = svc
            .children()
            .find(|n| n.tag_name().name() == "Namespace")
            .and_then(|n| n.text())
            .unwrap_or("");
        if ns == NS_PTZ {
            return svc
                .children()
                .find(|n| n.tag_name().name() == "XAddr")
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string());
        }
    }
    None
}

pub(super) fn get_imaging_uri(
    agent: &ureq::Agent,
    uri: &str,
    creads: &Credentials,
    created: &str,
) -> Option<String> {
    use super::soap::NS_IMAGING;

    let action = format!("{NS_DEVICE}/GetServices");
    let body =
        "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
    let resp = soap_call(agent, uri, &action, creads, created, body).ok()?;

    let doc = roxmltree::Document::parse(&resp).ok()?;

    for svc in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Service")
    {
        let ns = svc
            .children()
            .find(|n| n.tag_name().name() == "Namespace")
            .and_then(|n| n.text())
            .unwrap_or("");
        if ns == NS_IMAGING {
            return svc
                .children()
                .find(|n| n.tag_name().name() == "XAddr")
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string());
        }
    }
    None
}

pub(super) fn get_events_uri(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<String> {
    use super::soap::NS_EVENTS;
    let action = format!("{NS_DEVICE}/GetServices");
    let body =
        "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
    let resp = soap_call(agent, uri, &action, creds, created, body).ok()?;

    let doc = roxmltree::Document::parse(&resp).ok()?;
    for svc in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Service")
    {
        let ns = svc
            .children()
            .find(|n| n.tag_name().name() == "Namespace")
            .and_then(|n| n.text())
            .unwrap_or("");
        if ns == NS_EVENTS {
            return svc
                .children()
                .find(|n| n.tag_name().name() == "XAddr")
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string());
        }
    }
    None
}

pub(super) fn get_replay_info(
    agent: &ureq::Agent,
    uri: &str,
    creds: &Credentials,
    created: &str,
) -> Option<super::types::ReplayInfo> {
    use super::soap::{NS_REPLAY, NS_SEARCH};
    let action = format!("{NS_DEVICE}/GetServices");
    let body =
        "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
    let resp = soap_call(agent, uri, &action, creds, created, body).ok()?;
    let doc = roxmltree::Document::parse(&resp).ok()?;

    let mut search = None;
    let mut replay = None;
    for svc in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Service")
    {
        let ns = svc
            .children()
            .find(|n| n.tag_name().name() == "Namespace")
            .and_then(|n| n.text())
            .unwrap_or("");
        let xaddr = svc
            .children()
            .find(|n| n.tag_name().name() == "XAddr")
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string());
        if ns == NS_SEARCH {
            search = xaddr;
        } else if ns == NS_REPLAY {
            replay = xaddr;
        }
    }
    // Both are required for playback to work — if either is absent, the
    // device doesn't do Profile G properly.
    Some(super::types::ReplayInfo {
        search_uri: search?,
        replay_uri: replay?,
        device_uri: uri.to_string(),
    })
}
