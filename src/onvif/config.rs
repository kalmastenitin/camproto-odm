// src/onvif/config.rs

use super::soap::{build_agent, device_created, soap_call, xml_escape, NS_DEVICE};
use super::types::{Credentials, DateTimeMode, NetworkSetting};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn set_datetime(
    service_uri: &str,
    creds: &Credentials,
    mode: DateTimeMode,
    tz: Option<&str>,
    _dst: bool,
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;

    // Schema order for SetSystemDateAndTime is strict:
    //   DateTimeType, DaylightSavings, TimeZone?, UTCDateTime?
    let (dt_type, utc_xml) = match mode {
        DateTimeMode::Ntp => ("NTP".to_string(), String::new()),
        DateTimeMode::SyncToHost => {
            let (y, mo, d, h, mi, s) = host_utc();
            (
                "Manual".to_string(),
                format!(
                    "<tds:UTCDateTime>\
                     <tt:Time><tt:Hour>{h}</tt:Hour><tt:Minute>{mi}</tt:Minute><tt:Second>{s}</tt:Second></tt:Time>\
                     <tt:Date><tt:Year>{y}</tt:Year><tt:Month>{mo}</tt:Month><tt:Day>{d}</tt:Day></tt:Date>\
                     </tds:UTCDateTime>"
                ),
            )
        }
    };

    // Echo back the device's own TZ if we have it; omit the element otherwise.
    let tz_xml = match tz {
        Some(t) if !t.trim().is_empty() => {
            format!(
                "<tds:TimeZone><tt:TZ>{}</tt:TZ></tds:TimeZone>",
                xml_escape(t)
            )
        }
        _ => String::new(),
    };

    let body = format!(
        "<tds:SetSystemDateAndTime>\
         <tds:DateTimeType>{dt_type}</tds:DateTimeType>\
         <tds:DaylightSavings>false</tds:DaylightSavings>\
         {tz_xml}\
         {utc_xml}\
         </tds:SetSystemDateAndTime>"
    );

    soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SetSystemDateAndTime"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

/// Returns true if the device says a reboot is needed for the change to apply.
pub fn set_network(
    service_uri: &str,
    creds: &Credentials,
    s: &NetworkSetting,
) -> Result<bool, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;

    let iface_body = format!(
        "<tds:SetNetworkInterfaces><tds:InterfaceToken>{token}</tds:InterfaceToken>\
         <tds:NetworkInterface><tt:Enabled>true</tt:Enabled><tt:IPv4><tt:Enabled>true</tt:Enabled>\
         <tt:Manual><tt:Address>{ip}</tt:Address><tt:PrefixLength>{prefix}</tt:PrefixLength></tt:Manual>\
         <tt:DHCP>{dhcp}</tt:DHCP></tt:IPv4></tds:NetworkInterface></tds:SetNetworkInterfaces>",
        token = xml_escape(&s.interface_token),
        ip = xml_escape(&s.ip),
        prefix = s.prefix,
        dhcp = s.dhcp,
    );
    let resp = soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SetNetworkInterfaces"),
        creds,
        &created,
        &iface_body,
    )?;
    let reboot_needed = roxmltree::Document::parse(&resp)
        .ok()
        .and_then(|doc| {
            doc.descendants()
                .find(|n| n.tag_name().name() == "RebootNeeded")
                .and_then(|n| n.text())
                .map(|t| t.trim().eq_ignore_ascii_case("true"))
        })
        .unwrap_or(false);

    // Gateway + DNS only make sense for a static config.
    if !s.dhcp && !s.gateway.is_empty() {
        let gw = format!(
            "<tds:SetNetworkDefaultGateway><tds:IPv4Address>{}</tds:IPv4Address></tds:SetNetworkDefaultGateway>",
            xml_escape(&s.gateway)
        );
        let _ = soap_call(
            &agent,
            service_uri,
            &format!("{NS_DEVICE}/SetNetworkDefaultGateway"),
            creds,
            &created,
            &gw,
        );
    }
    if !s.dns.is_empty() {
        let items: String = s
            .dns
            .iter()
            .map(|d| format!("<tds:DNSManual><tt:Type>IPv4</tt:Type><tt:IPv4Address>{}</tt:IPv4Address></tds:DNSManual>", xml_escape(d)))
            .collect();
        let dns = format!(
            "<tds:SetDNS><tds:FromDHCP>{}</tds:FromDHCP>{}</tds:SetDNS>",
            s.dhcp,
            if s.dhcp { String::new() } else { items }
        );
        let _ = soap_call(
            &agent,
            service_uri,
            &format!("{NS_DEVICE}/SetDNS"),
            creds,
            &created,
            &dns,
        );
    }

    Ok(reboot_needed)
}

pub fn reboot(service_uri: &str, creds: &Credentials) -> Result<String, String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;
    let resp = soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SystemReboot"),
        creds,
        &created,
        "<tds:SystemReboot/>",
    )?;
    let doc = roxmltree::Document::parse(&resp).map_err(|e| format!("bad XML: {e}"))?;
    Ok(doc
        .descendants()
        .find(|n| n.tag_name().name() == "Message")
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "reboot requested".to_string()))
}

/// Host UTC broken into (Y, M, D, h, m, s) — no chrono (Howard Hinnant's algorithm).
fn host_utc() -> (i64, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = (secs % 86400) as u32;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d, hh, mm, ss)
}

pub fn set_identification(
    service_uri: &str,
    creds: &Credentials,
    name: &str,
    location: &str,
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;
    let mut scopes = String::new();
    if !name.trim().is_empty() {
        scopes.push_str(&format!(
            "<tds:Scopes>onvif://www.onvif.org/name/{}</tds:Scopes>",
            scope_encode(name.trim())
        ));
    }
    if !location.trim().is_empty() {
        scopes.push_str(&format!(
            "<tds:Scopes>onvif://www.onvif.org/location/{}</tds:Scopes>",
            scope_encode(location.trim())
        ));
    }
    let body = format!("<tds:SetScopes>{scopes}</tds:SetScopes>");
    soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SetScopes"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

pub fn set_hostname(service_uri: &str, creds: &Credentials, name: &str) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;
    let body = format!(
        "<tds:SetHostname><tds:Name>{}</tds:Name></tds:SetHostname>",
        xml_escape(name.trim())
    );
    soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SetHostname"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

pub fn set_ntp(
    service_uri: &str,
    creds: &Credentials,
    from_dhcp: bool,
    servers: &[String],
) -> Result<(), String> {
    let agent = build_agent();
    let created =
        device_created(&agent, service_uri).ok_or_else(|| "no device clock".to_string())?;
    let manual: String = if from_dhcp {
        String::new()
    } else {
        servers
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                let (ty, tag) = if s.parse::<std::net::Ipv4Addr>().is_ok() {
                    ("IPv4", "IPv4Address")
                } else {
                    ("DNS", "DNSname")
                };
                format!(
                    "<tds:NTPManual><tt:Type>{ty}</tt:Type><tt:{tag}>{}</tt:{tag}></tds:NTPManual>",
                    xml_escape(s)
                )
            })
            .collect()
    };
    let body = format!("<tds:SetNTP><tds:FromDHCP>{from_dhcp}</tds:FromDHCP>{manual}</tds:SetNTP>");
    soap_call(
        &agent,
        service_uri,
        &format!("{NS_DEVICE}/SetNTP"),
        creds,
        &created,
        &body,
    )?;
    Ok(())
}

/// Scope values are URL-encoded; minimal space handling + XML escape.
fn scope_encode(s: &str) -> String {
    xml_escape(&s.replace(' ', "%20"))
}
