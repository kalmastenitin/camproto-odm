//src/discovery.rs

use eframe::egui;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const WS_DISCOVERY_PORT: u16 = 3702;
const LISTEN_WINDOW: Duration = Duration::from_secs(4);
const DEFAULT_HTTP_PORT: u16 = 80;
const HTTP_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_WORKERS: usize = 64;

#[derive(Clone)]
pub struct DiscoveredDevice {
    /// Stable identity: WS-Discovery UUID, or host:port for HTTP-probed devices.
    pub id: String,
    pub uuid: Option<String>,
    pub name: String,
    pub host: String,
    pub xaddrs: Vec<String>,
    pub hardware: Option<String>,
    pub location: Option<String>,
    pub scopes: Vec<String>,
    pub source_ip: IpAddr,
    /// Device clock from GetSystemDateAndTime (targeted HTTP scans only).
    pub system_time: Option<String>,
}

impl DiscoveredDevice {
    /// Fold a freshly-seen record of the same device into this one, keeping the
    /// richest identity fields and the freshest volatile data. Lets a device
    /// found via both WS-Discovery and HTTP collapse into one entry.
    pub fn merge_from(&mut self, other: DiscoveredDevice) {
        if self.uuid.is_none() {
            self.uuid = other.uuid;
        }
        if self.hardware.is_none() {
            self.hardware = other.hardware;
        }
        if self.location.is_none() {
            self.location = other.location;
        }
        if self.scopes.is_empty() {
            self.scopes = other.scopes;
        }
        // A real (scope-derived) name beats an ip:port placeholder.
        if self.name == self.host && other.name != other.host {
            self.name = other.name;
        }
        for x in other.xaddrs {
            if !self.xaddrs.contains(&x) {
                self.xaddrs.push(x);
            }
        }
        if other.system_time.is_some() {
            self.system_time = other.system_time;
        }
    }
}

pub enum DiscoveryEvent {
    Found(DiscoveredDevice),
    Finished,
    Error(String),
}

/// How to probe.
pub enum ScanMode {
    /// Multicast WS-Discovery to 239.255.255.250 — finds everything on the subnet.
    Multicast,
    /// Query specific hosts' ONVIF device service over HTTP.
    Targets(Vec<SocketAddrV4>),
}

/// Kick off a scan on a background thread. Returns the receiver the UI drains.
pub fn spawn(ctx: egui::Context, mode: ScanMode) -> Receiver<DiscoveryEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("odm-discovery".into())
        .spawn(move || {
            if let Err(e) = run(&tx, &ctx, mode) {
                let _ = tx.send(DiscoveryEvent::Error(e));
                ctx.request_repaint();
            }
        })
        .expect("spawn discovery thread");
    rx
}

fn run(tx: &Sender<DiscoveryEvent>, ctx: &egui::Context, mode: ScanMode) -> Result<(), String> {
    match mode {
        ScanMode::Multicast => run_multicast(tx, ctx),
        ScanMode::Targets(addrs) => run_targets(tx, ctx, addrs),
    }
}

// ---------------------------------------------------------------------------
// Multicast WS-Discovery
// ---------------------------------------------------------------------------

fn run_multicast(tx: &Sender<DiscoveryEvent>, ctx: &egui::Context) -> Result<(), String> {
    let mut seen = HashSet::new();
    let dest = SocketAddrV4::new(MULTICAST_ADDR, WS_DISCOVERY_PORT);
    ws_discovery_probe(tx, ctx, &[dest], &mut seen)?;

    let _ = tx.send(DiscoveryEvent::Finished);
    ctx.request_repaint();
    Ok(())
}

/// Send a WS-Discovery Probe to each destination and collect ProbeMatch replies
/// for the listen window. Shared by the multicast ("Discover LAN") scan and the
/// unicast (targeted) scan — both parse the same rich ProbeMatch (name, UUID,
/// hardware, location, scopes), which is what makes targeted results match
/// Discover LAN for any device that answers a unicast probe.
fn ws_discovery_probe(
    tx: &Sender<DiscoveryEvent>,
    ctx: &egui::Context,
    destinations: &[SocketAddrV4],
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("bind: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(400)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;

    let probe = build_probe();
    // Two passes — the first UDP packet is the one most likely to be lost.
    for _ in 0..2 {
        for dest in destinations {
            let _ = socket.send_to(probe.as_bytes(), *dest);
        }
        std::thread::sleep(Duration::from_millis(80));
    }

    let deadline = Instant::now() + LISTEN_WINDOW;
    let mut buf = vec![0u8; 65_536];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    for dev in parse_probe_matches(text, src.ip()) {
                        if seen.insert(dev.id.clone()) {
                            let _ = tx.send(DiscoveryEvent::Found(dev));
                            ctx.request_repaint();
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(e) => return Err(format!("recv: {e}")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Targeted HTTP probing of the ONVIF device service
// ---------------------------------------------------------------------------
fn run_targets(
    tx: &Sender<DiscoveryEvent>,
    ctx: &egui::Context,
    addrs: Vec<SocketAddrV4>,
) -> Result<(), String> {
    // Shared work queue drained by a pool of workers, so a /24 scan doesn't
    // serialize behind each dead host's connect timeout.
    let n_workers = addrs.len().clamp(1, MAX_WORKERS);
    let queue = Arc::new(Mutex::new(addrs));
    let mut handles = Vec::new();

    let agent = build_agent();
    for _ in 0..n_workers {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        let ctx = ctx.clone();
        let agent = agent.clone();
        handles.push(std::thread::spawn(move || loop {
            let addr = match queue.lock() {
                Ok(mut q) => q.pop(),
                Err(_) => None,
            };
            let Some(addr) = addr else { break };
            // Prefer unicast WS-Discovery (rich scopes, exactly like Discover
            // LAN); fall back to the HTTP device service if the host doesn't
            // answer discovery.
            let found = probe_ws_unicast(*addr.ip()).or_else(|| probe_device_service(&agent, addr));
            if let Some(dev) = found {
                let _ = tx.send(DiscoveryEvent::Found(dev));
                ctx.request_repaint();
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    let _ = tx.send(DiscoveryEvent::Finished);
    ctx.request_repaint();
    Ok(())
}

/// Unicast WS-Discovery to a single host: the same rich ProbeMatch (name, uuid,
/// hardware, location, scopes) that "Discover LAN" collects, just aimed at one
/// IP. Unicast routes, so this reaches devices on other subnets too.
fn probe_ws_unicast(ip: Ipv4Addr) -> Option<DiscoveredDevice> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok()?;

    let probe = build_probe();
    socket
        .send_to(probe.as_bytes(), (ip, WS_DISCOVERY_PORT))
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(600);
    let mut buf = vec![0u8; 65_536];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            // Only accept the reply from the host we probed.
            Ok((n, src)) if src.ip() == IpAddr::V4(ip) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    if let Some(dev) = parse_probe_matches(text, src.ip()).into_iter().next() {
                        return Some(dev);
                    }
                }
            }
            Ok(_) => {} // stray packet from another host — ignore
            Err(ref e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    None
}

/// A shared HTTP agent for the whole targeted scan.
///
/// * `timeout_global` bounds each request so a dead host can't stall a worker.
/// * `http_status_as_error(false)` lets a SOAP fault (HTTP 500) arrive as a
///   normal response instead of an `Err` — a fault still means an ONVIF
///   endpoint is answering, and we want to read its body.
fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Probe one host's ONVIF device service over HTTP, no credentials required.
///
/// 1. `GetSystemDateAndTime` (PRE_AUTH) — confirms the endpoint is ONVIF and
///    reachable, and gives us the device clock (needed later for digest auth).
/// 2. `GetEndpointReference` (also PRE_AUTH) — the device's `urn:uuid`, which we
///    show and use to dedup against the same device found via WS-Discovery.
///
/// Name / hardware / location live in the discovery *scopes*, which are
/// `READ_SYSTEM` (auth-only), so they stay empty here until step 3.
fn probe_device_service(agent: &ureq::Agent, addr: SocketAddrV4) -> Option<DiscoveredDevice> {
    let url = format!("http://{}:{}/onvif/device_service", addr.ip(), addr.port());

    let dt_body = post_soap(agent, &url, GET_SYSTEM_DATE_AND_TIME)?;
    let mut dev = parse_device_service(&dt_body, addr, &url)?;

    // Best-effort UUID. If the device doesn't implement GetEndpointReference (or
    // demands auth for it despite the spec), we keep the host:port id.
    if let Some(body) = post_soap(agent, &url, GET_ENDPOINT_REFERENCE) {
        if let Some(guid) = parse_endpoint_reference(&body) {
            dev.id = normalize_uuid(&guid);
            dev.uuid = Some(guid);
        }
    }

    Some(dev)
}

/// POST a SOAP body to a device service and return the response text.
fn post_soap(agent: &ureq::Agent, url: &str, soap: &str) -> Option<String> {
    let mut resp = agent
        .post(url)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .send(soap)
        .ok()?; // transport error: host down, port closed, or timed out
    resp.body_mut().read_to_string().ok()
}

/// Extract the device GUID from a GetEndpointReferenceResponse.
fn parse_endpoint_reference(body: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).ok()?;
    for n in doc.descendants() {
        let name = n.tag_name().name();
        if name == "GUID" || name == "Address" {
            if let Some(t) = n.text().map(str::trim) {
                if !t.is_empty() && (name == "GUID" || t.starts_with("urn:uuid:")) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// Normalize a UUID for use as a device id, so the same device found via
/// WS-Discovery and via GetEndpointReference collapses to one entry regardless
/// of the `urn:uuid:` prefix or case.
fn normalize_uuid(s: &str) -> String {
    s.trim().trim_start_matches("urn:uuid:").to_lowercase()
}

fn parse_device_service(body: &str, addr: SocketAddrV4, url: &str) -> Option<DiscoveredDevice> {
    let doc = roxmltree::Document::parse(body).ok()?;

    // Must be a SOAP envelope, and look like ONVIF (not just any SOAP service).
    let is_soap = doc.descendants().any(|n| n.tag_name().name() == "Envelope");
    let onvif_like = is_soap
        && (body.contains("onvif.org")
            || doc.descendants().any(|n| {
                matches!(
                    n.tag_name().name(),
                    "GetSystemDateAndTimeResponse" | "SystemDateAndTime"
                )
            }));
    if !onvif_like {
        return None;
    }

    let host = format!("{}:{}", addr.ip(), addr.port());
    Some(DiscoveredDevice {
        id: host.clone(),
        uuid: None,
        name: host.clone(),
        host,
        xaddrs: vec![url.to_string()],
        hardware: None,
        location: None,
        scopes: Vec::new(),
        source_ip: IpAddr::V4(*addr.ip()),
        system_time: extract_system_time(&doc),
    })
}

fn extract_system_time(doc: &roxmltree::Document) -> Option<String> {
    let dt = doc
        .descendants()
        .find(|n| n.tag_name().name() == "UTCDateTime")
        .or_else(|| {
            doc.descendants()
                .find(|n| n.tag_name().name() == "LocalDateTime")
        })?;

    let field = |name: &str| -> Option<u32> {
        dt.descendants()
            .find(|n| n.tag_name().name() == name)
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse().ok())
    };

    let (y, mo, d) = (field("Year")?, field("Month")?, field("Day")?);
    let (h, mi, s) = (
        field("Hour").unwrap_or(0),
        field("Minute").unwrap_or(0),
        field("Second").unwrap_or(0),
    );
    Some(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC"))
}

const GET_SYSTEM_DATE_AND_TIME: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" "#,
    r#"xmlns:tds="http://www.onvif.org/ver10/device/wsdl">"#,
    r#"<s:Body><tds:GetSystemDateAndTime/></s:Body></s:Envelope>"#
);

const GET_ENDPOINT_REFERENCE: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" "#,
    r#"xmlns:tds="http://www.onvif.org/ver10/device/wsdl">"#,
    r#"<s:Body><tds:GetEndpointReference/></s:Body></s:Envelope>"#
);

// ---------------------------------------------------------------------------
// WS-Discovery probe construction
// ---------------------------------------------------------------------------

fn build_probe() -> String {
    // SOAP 1.2 WS-Discovery Probe for ONVIF cameras (NetworkVideoTransmitter).
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:dn="http://www.onvif.org/ver10/network/wsdl"><e:Header><w:MessageID>{msg_id}</w:MessageID><w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To><w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header><e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>"#,
        msg_id = message_id()
    )
}

/// A v4-shaped `urn:uuid:` for the Probe's MessageID. Uniqueness (not crypto
/// randomness) is all that's needed, so we mix a timestamp with a counter.
fn message_id() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    let mix = nanos ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!(
        "urn:uuid:{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (mix >> 32) as u32,
        (mix >> 16) as u16,
        (mix & 0xfff) as u16,
        (c & 0xfff) as u16,
        nanos & 0xffff_ffff_ffff
    )
}

// ---------------------------------------------------------------------------
// ProbeMatch parsing (multicast replies)
// ---------------------------------------------------------------------------

fn parse_probe_matches(xml: &str, source_ip: IpAddr) -> Vec<DiscoveredDevice> {
    let doc = match roxmltree::Document::parse(xml) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for pm in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "ProbeMatch")
    {
        let text_of = |local: &str| {
            pm.descendants()
                .find(|n| n.tag_name().name() == local)
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string())
        };

        let xaddrs: Vec<String> = text_of("XAddrs")
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();

        let scopes: Vec<String> = text_of("Scopes")
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();

        let uuid = pm
            .descendants()
            .find(|n| n.tag_name().name() == "Address")
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let host = xaddrs
            .iter()
            .find(|u| u.starts_with("http"))
            .and_then(|u| host_of(u))
            .unwrap_or_else(|| source_ip.to_string());

        let name = scope_value(&scopes, "name")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| host.clone());

        let id = uuid
            .as_deref()
            .map(normalize_uuid)
            .unwrap_or_else(|| host.clone());

        out.push(DiscoveredDevice {
            id,
            uuid,
            name,
            host,
            hardware: scope_value(&scopes, "hardware"),
            location: scope_value(&scopes, "location"),
            xaddrs,
            scopes,
            source_ip,
            system_time: None,
        });
    }
    out
}

fn host_of(url: &str) -> Option<String> {
    let authority = url.split("://").nth(1)?.split('/').next()?;
    (!authority.is_empty()).then(|| authority.to_string())
}

fn scope_value(scopes: &[String], key: &str) -> Option<String> {
    let needle = format!("/{key}/");
    scopes
        .iter()
        .find_map(|s| s.split(&needle).nth(1))
        .map(|v| pct_decode(v.trim_end_matches('/')))
}

fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Target parsing (single IP / range / CIDR, with optional :port)
// ---------------------------------------------------------------------------

/// Turn a user-entered target string into concrete host:port sockets. Accepts:
///   * single IP        `192.168.1.250`
///   * IP with port     `192.168.1.250:8080`
///   * last-octet range `192.168.1.10-50`
///   * full range       `192.168.1.10-192.168.1.60`
///   * CIDR subnet      `192.168.1.0/24`  (network + broadcast excluded)
/// A trailing `:port` (default 80) applies to every host produced.
pub fn parse_targets(input: &str) -> Result<Vec<SocketAddrV4>, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("enter an IP, range, or CIDR".into());
    }

    // IPv4 addresses contain no colon, so a ':' unambiguously marks the port.
    let (addr_part, port) = match s.rsplit_once(':') {
        Some((a, p)) => (
            a.trim(),
            p.trim()
                .parse::<u16>()
                .map_err(|_| format!("bad port: {p}"))?,
        ),
        None => (s, DEFAULT_HTTP_PORT),
    };

    let ips = if let Some((base, prefix)) = addr_part.split_once('/') {
        cidr_hosts(base.trim(), prefix.trim())?
    } else if let Some((a, b)) = addr_part.split_once('-') {
        range_hosts(a.trim(), b.trim())?
    } else {
        vec![addr_part
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("not a valid IPv4 address: {addr_part}"))?]
    };

    Ok(ips
        .into_iter()
        .map(|ip| SocketAddrV4::new(ip, port))
        .collect())
}

fn cidr_hosts(base: &str, prefix: &str) -> Result<Vec<Ipv4Addr>, String> {
    let addr: Ipv4Addr = base
        .parse()
        .map_err(|_| format!("bad network address: {base}"))?;
    let prefix: u32 = prefix
        .parse()
        .map_err(|_| format!("bad prefix length: {prefix}"))?;
    if prefix > 32 {
        return Err("prefix length must be 0–32".into());
    }
    if prefix < 20 {
        return Err("subnet too large (use /20 or narrower)".into());
    }
    let mask: u32 = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(addr) & mask;
    let count = 1u32 << (32 - prefix);
    let (start, end) = if prefix <= 30 {
        (1, count - 1)
    } else {
        (0, count)
    };
    Ok((start..end).map(|i| Ipv4Addr::from(network + i)).collect())
}

fn range_hosts(a: &str, b: &str) -> Result<Vec<Ipv4Addr>, String> {
    let start: Ipv4Addr = a.parse().map_err(|_| format!("bad start address: {a}"))?;
    let end: Ipv4Addr = if b.contains('.') {
        b.parse().map_err(|_| format!("bad end address: {b}"))?
    } else {
        let last: u8 = b.parse().map_err(|_| format!("bad range end: {b}"))?;
        let o = start.octets();
        Ipv4Addr::new(o[0], o[1], o[2], last)
    };
    let (s, e) = (u32::from(start), u32::from(end));
    if e < s {
        return Err("range end is before start".into());
    }
    if e - s > 4096 {
        return Err("range too large (max 4096 hosts)".into());
    }
    Ok((s..=e).map(Ipv4Addr::from).collect())
}
