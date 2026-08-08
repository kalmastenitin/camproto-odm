// src/app.rs

use eframe::egui;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use crate::discovery::{self, DiscoveredDevice, DiscoveryEvent, ScanMode};
use crate::onvif;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x3b, 0x82, 0xf6);
const DOT_NEUTRAL: egui::Color32 = egui::Color32::from_rgb(0x8a, 0x8a, 0x8a);
const DOT_CONNECTING: egui::Color32 = egui::Color32::from_rgb(0xe0, 0xb0, 0x3d);
const DOT_OK: egui::Color32 = egui::Color32::from_rgb(0x38, 0xd9, 0x66);
const DOT_ERR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x3d, 0x3d);

/// Per-device credentials. Held in memory only — never logged or persisted.
#[derive(Clone)]
struct DeviceCreds {
    username: String,
    password: String,
}

impl Default for DeviceCreds {
    fn default() -> Self {
        // "admin" is the near-universal ONVIF default; pre-fill to save typing.
        Self {
            username: String::new(),
            password: String::new(),
        }
    }
}

struct ConnectMsg {
    id: String,
    result: Result<onvif::DeviceDetails, String>,
}

enum ConnState {
    Connecting,
    Connected(onvif::DeviceDetails),
    Failed(String),
}

enum SnapState {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

struct SnapshotMsg {
    key: String,
    result: Result<onvif::Snapshot, String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Info,
    Streams,
    Live,
    Config,
}

#[derive(Default, Clone)]
struct NetEdit {
    token: String,
    dhcp: bool,
    ip: String,
    prefix: String,
    gateway: String,
    dns: String,
}

#[derive(Default, Clone)]
struct ConfigEdit {
    name: String,
    location: String,
    tz: String,
    dst: bool,
    ntp_from_dhcp: bool,
    ntp_server: String,
    hostname: String,
    net: NetEdit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmKind {
    ApplyNetwork,
    Reboot,
}

#[derive(Clone, Copy)]
enum ActionKind {
    DateTimeNtp,
    DateTimeSync,
    SetIdentification,
    SetNtp,
    ApplyNetwork,
    Reboot,
}

struct ActionMsg {
    id: String,
    result: Result<String, String>,
}

/// Snapshot of the selected device's discovery data, cloned so we can release
/// the borrow on `self.devices` before mutating self in the tab bodies.

#[derive(Debug)]
struct DetailView {
    id: String,
    uuid: Option<String>,
    hardware: Option<String>,
    location: Option<String>,
    source_ip: std::net::IpAddr,
    system_time: Option<String>,
    xaddrs: Vec<String>,
    scopes: Vec<String>,
    service_uri: Option<String>,
    name: String,
}

impl ConnState {
    fn dot(&self) -> egui::Color32 {
        match self {
            ConnState::Connecting => DOT_CONNECTING,
            ConnState::Connected(_) => DOT_OK,
            ConnState::Failed(_) => DOT_ERR,
        }
    }
}

pub struct OdmApp {
    rx: Option<Receiver<DiscoveryEvent>>,
    devices: Vec<DiscoveredDevice>,
    selected: Option<String>,
    scanning: bool,
    status: String,
    target_input: String,

    // Per-device credentials + connection state, keyed by device id.
    creds: HashMap<String, DeviceCreds>,
    connections: HashMap<String, ConnState>,
    connect_tx: Sender<ConnectMsg>,
    connect_rx: Receiver<ConnectMsg>,

    active_tab: Tab,

    snapshots: HashMap<String, SnapState>,
    snapshot_tx: Sender<SnapshotMsg>,
    snapshot_rx: Receiver<SnapshotMsg>,

    edits: HashMap<String, ConfigEdit>,
    confirm: HashMap<String, ConfirmKind>,
    actions: HashMap<String, (std::time::Instant, Result<String, String>)>,
    action_tx: Sender<ActionMsg>,
    action_rx: Receiver<ActionMsg>,

    active_captures: std::sync::Arc<std::sync::atomic::AtomicUsize>,

    live: Option<(String, crate::video::LiveStream)>,
    live_texture: Option<egui::TextureHandle>,
}

impl OdmApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (connect_tx, connect_rx) = channel();
        let (snapshot_tx, snapshot_rx) = channel();
        let (action_tx, action_rx) = channel();

        Self {
            rx: None,
            devices: Vec::new(),
            selected: None,
            scanning: false,
            status: "Ready".into(),
            target_input: String::new(),
            creds: HashMap::new(),
            connections: HashMap::new(),
            connect_tx,
            connect_rx,
            active_tab: Tab::Info,
            snapshots: HashMap::new(),
            snapshot_tx,
            snapshot_rx,
            edits: HashMap::new(),
            confirm: HashMap::new(),
            actions: HashMap::new(),
            action_tx,
            action_rx,
            active_captures: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            live: None,
            live_texture: None,
        }
    }

    // --- scanning ---

    fn start_scan(&mut self, ctx: &egui::Context, mode: ScanMode) {
        self.scanning = true;
        self.status = match &mode {
            ScanMode::Multicast => "Scanning LAN…".into(),
            ScanMode::Targets(t) => format!("Scanning {} host(s)…", t.len()),
        };
        self.rx = Some(discovery::spawn(ctx.clone(), mode));
    }

    fn start_targeted(&mut self, ctx: &egui::Context) {
        match discovery::parse_targets(&self.target_input) {
            Ok(targets) => self.start_scan(ctx, ScanMode::Targets(targets)),
            Err(e) => self.status = format!("Invalid target: {e}"),
        }
    }

    // --- connect ---

    fn start_connect(&mut self, ctx: &egui::Context, id: String, service_uri: String) {
        let creds = self.creds.get(&id).cloned().unwrap_or_default();
        self.connections.insert(id.clone(), ConnState::Connecting);

        let onvif_creds = onvif::Credentials {
            username: creds.username,
            password: creds.password,
        };
        let tx = self.connect_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = onvif::connect(&service_uri, &onvif_creds);
            let _ = tx.send(ConnectMsg { id, result });
            ctx.request_repaint();
        });
    }

    fn run_action(&mut self, ctx: &egui::Context, id: &str, service_uri: &str, kind: ActionKind) {
        self.actions.remove(id);
        let creds = self.creds.get(id).cloned().unwrap_or_default();
        let onvif_creds = onvif::Credentials {
            username: creds.username,
            password: creds.password,
        };
        let e = self.edits.get(id).cloned().unwrap_or_default();
        let tz = {
            let t = e.tz.trim().to_string();
            (!t.is_empty()).then_some(t)
        };
        let id = id.to_string();
        let uri = service_uri.to_string();
        let tx = self.action_tx.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let result = match kind {
                ActionKind::DateTimeNtp => onvif::set_datetime(
                    &uri,
                    &onvif_creds,
                    onvif::DateTimeMode::Ntp,
                    tz.as_deref(),
                    e.dst,
                )
                .map(|_| "Switched to NTP".to_string()),
                ActionKind::DateTimeSync => onvif::set_datetime(
                    &uri,
                    &onvif_creds,
                    onvif::DateTimeMode::SyncToHost,
                    tz.as_deref(),
                    e.dst,
                )
                .map(|_| "Synced to this computer".to_string()),
                ActionKind::SetIdentification => {
                    onvif::set_identification(&uri, &onvif_creds, e.name.trim(), e.location.trim())
                        .map(|_| "Identification updated".to_string())
                }
                ActionKind::SetNtp => {
                    let servers: Vec<String> = e
                        .ntp_server
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    onvif::set_ntp(&uri, &onvif_creds, e.ntp_from_dhcp, &servers)
                        .map(|_| "NTP updated".to_string())
                }
                ActionKind::Reboot => onvif::reboot(&uri, &onvif_creds),
                ActionKind::ApplyNetwork => {
                    if !e.hostname.trim().is_empty() {
                        let _ = onvif::set_hostname(&uri, &onvif_creds, e.hostname.trim());
                    }
                    let setting = onvif::NetworkSetting {
                        interface_token: e.net.token,
                        dhcp: e.net.dhcp,
                        ip: e.net.ip.trim().to_string(),
                        prefix: e.net.prefix.trim().parse().unwrap_or(24),
                        gateway: e.net.gateway.trim().to_string(),
                        dns: e
                            .net
                            .dns
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    };
                    onvif::set_network(&uri, &onvif_creds, &setting).map(|r| {
                        if r {
                            "Applied — reboot needed".into()
                        } else {
                            "Applied".into()
                        }
                    })
                }
            };
            let _ = tx.send(ActionMsg { id, result });
            ctx.request_repaint();
        });
    }

    fn start_capture(&mut self, ctx: &egui::Context, device_id: &str, key: String, url: String) {
        const MAX_CONCURRENT: usize = 3;
        if self
            .active_captures
            .load(std::sync::atomic::Ordering::Relaxed)
            >= MAX_CONCURRENT
        {
            return; // try again next frame once a slot frees up
        }

        let creds = self.creds.get(device_id).cloned().unwrap_or_default();
        self.snapshots.insert(key.clone(), SnapState::Loading);

        let onvif_creds = onvif::Credentials {
            username: creds.username,
            password: creds.password,
        };
        let tx = self.snapshot_tx.clone();
        let ctx = ctx.clone();
        let counter = self.active_captures.clone();
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        std::thread::spawn(move || {
            let result = onvif::capture(&url, &onvif_creds);
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(SnapshotMsg { key, result });
            ctx.request_repaint();
        });
    }

    // --- events ---

    fn drain(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(evt) => events.push(evt),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for evt in events {
            match evt {
                DiscoveryEvent::Found(d) => self.upsert(d),
                DiscoveryEvent::Finished => {
                    self.scanning = false;
                    self.status = format!("{} device(s)", self.devices.len());
                    self.rx = None;
                }
                DiscoveryEvent::Error(e) => {
                    self.scanning = false;
                    self.status = format!("Error: {e}");
                    self.rx = None;
                }
            }
        }
        if disconnected && self.rx.is_some() {
            self.rx = None;
            if self.scanning {
                self.scanning = false;
                self.status = format!("{} device(s)", self.devices.len());
            }
        }

        while let Ok(msg) = self.connect_rx.try_recv() {
            let state = match msg.result {
                Ok(info) => ConnState::Connected(info),
                Err(e) => ConnState::Failed(e),
            };
            self.connections.insert(msg.id, state);
        }

        while let Ok(msg) = self.snapshot_rx.try_recv() {
            let state = match msg.result {
                Ok(snap) => {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [snap.width as usize, snap.height as usize],
                        &snap.rgba,
                    );
                    let tex = ctx.load_texture(
                        format!("snap-{}", msg.key),
                        color,
                        egui::TextureOptions::LINEAR,
                    );
                    SnapState::Ready(tex)
                }
                Err(e) => SnapState::Failed(e),
            };
            self.snapshots.insert(msg.key, state);
        }

        while let Ok(msg) = self.action_rx.try_recv() {
            self.actions
                .insert(msg.id, (std::time::Instant::now(), msg.result));
        }
    }

    fn upsert(&mut self, d: DiscoveredDevice) {
        if let Some(row) = self.devices.iter_mut().find(|x| x.id == d.id) {
            row.merge_from(d);
        } else {
            self.devices.push(d);
            self.devices
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        }
    }

    // --- panels ---

    fn toolbar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("ONVIF Devices");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.scanning {
                    ui.spinner();
                }
                ui.label(egui::RichText::new(self.status.as_str()).weak());
            });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.scanning, egui::Button::new("Discover LAN"))
                .clicked()
            {
                self.start_scan(ctx, ScanMode::Multicast);
            }
            ui.separator();
            ui.label("Target:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.target_input)
                    .hint_text("e.g. 192.168.1.250,  192.168.1.250:8080,  192.168.1.0/24")
                    .desired_width(320.0),
            );
            let submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let scan_clicked = ui
                .add_enabled(!self.scanning, egui::Button::new("Scan"))
                .clicked();
            if (scan_clicked || submit) && !self.scanning {
                self.start_targeted(ctx);
            }
            if !self.devices.is_empty() {
                ui.separator();
                if ui.button("Clear").clicked() {
                    self.devices.clear();
                    self.selected = None;
                    self.status = "Ready".into();
                }
            }
        });
        ui.add_space(6.0);
    }

    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        if self.devices.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.weak(if self.scanning {
                    "Searching…"
                } else {
                    "No devices yet.\nClick Discover LAN, or enter an IP and Scan."
                });
            });
            return;
        }

        let mut clicked: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for dev in &self.devices {
                    let selected = self.selected.as_deref() == Some(dev.id.as_str());
                    let dot = self
                        .connections
                        .get(&dev.id)
                        .map(ConnState::dot)
                        .unwrap_or(DOT_NEUTRAL);
                    if device_row(ui, dev, selected, dot).clicked() {
                        clicked = Some(dev.id.clone());
                    }
                }
            });
        if let Some(id) = clicked {
            self.selected = Some(id);
        }
    }

    fn detail(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let Some(id) = self.selected.clone() else {
            ui.centered_and_justified(|ui| {
                ui.weak("Select a device to see details");
            });
            return;
        };
        let Some(dev) = self.devices.iter().find(|d| d.id == id) else {
            return;
        };

        // Snapshot for display; releases the borrow on self.devices.
        let name = dev.name.clone();
        let host = dev.host.clone();

        let view = DetailView {
            id: id.clone(),
            uuid: dev.uuid.clone(),
            hardware: dev.hardware.clone(),
            location: dev.location.clone(),
            source_ip: dev.source_ip,
            system_time: dev.system_time.clone(),
            xaddrs: dev.xaddrs.clone(),
            scopes: dev.scopes.clone(),
            service_uri: device_service_uri(dev).map(str::to_string),
            name: name.clone(),
        };

        // Fixed header + tab bar (these never scroll away).
        ui.add_space(6.0);
        ui.heading(name.as_str());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Host").weak());
            ui.label(egui::RichText::new(host.as_str()).monospace());
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active_tab, Tab::Info, "Info");
            ui.selectable_value(&mut self.active_tab, Tab::Streams, "Streams");
            ui.selectable_value(&mut self.active_tab, Tab::Live, "Live");
            for soon in ["PTZ", "Events"] {
                ui.add_enabled(false, egui::Button::selectable(false, soon))
                    .on_disabled_hover_text("coming soon");
            }
            ui.selectable_value(&mut self.active_tab, Tab::Config, "Config");
        });
        ui.separator();

        // Scrollable tab body.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.active_tab {
                Tab::Info => self.detail_info_tab(ctx, ui, &view),
                Tab::Streams => self.detail_streams_tab(ctx, ui, &view.id),
                Tab::Config => self.detail_config_tab(ctx, ui, &view),
                Tab::Live => self.detail_live_tab(ctx, ui, &view.id),
            });
    }

    fn detail_info_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, view: &DetailView) {
        egui::Grid::new("meta")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                meta_row(ui, "UUID", view.uuid.as_deref().unwrap_or("—"));
                meta_row(ui, "Hardware", view.hardware.as_deref().unwrap_or("—"));
                meta_row(ui, "Location", view.location.as_deref().unwrap_or("—"));
                if let Some(t) = &view.system_time {
                    meta_row(ui, "Device time", t);
                }
                meta_row(ui, "Replied from", &view.source_ip.to_string());
            });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Service endpoints (XAddrs)").strong());
        if view.xaddrs.is_empty() {
            ui.weak("none advertised");
        }
        for x in &view.xaddrs {
            ui.label(egui::RichText::new(x.as_str()).monospace().weak());
        }

        ui.add_space(8.0);
        ui.collapsing("Raw scopes", |ui| {
            for s in &view.scopes {
                ui.label(egui::RichText::new(s.as_str()).small().monospace().weak());
            }
        });

        ui.add_space(14.0);
        ui.separator();
        ui.label(egui::RichText::new("Connect").strong().color(ACCENT));

        let Some(service_uri) = view.service_uri.clone() else {
            ui.weak("No service endpoint to connect to.");
            return;
        };
        let id = view.id.clone();

        {
            let creds = self.creds.entry(id.clone()).or_default();
            egui::Grid::new("creds")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Username");
                    ui.add(egui::TextEdit::singleline(&mut creds.username).desired_width(200.0));
                    ui.end_row();
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut creds.password)
                            .password(true)
                            .desired_width(200.0),
                    );
                    ui.end_row();
                });
        }

        ui.add_space(4.0);
        let connecting = matches!(self.connections.get(&id), Some(ConnState::Connecting));
        let label = match self.connections.get(&id) {
            Some(ConnState::Connected(_)) => "Reconnect",
            Some(ConnState::Connecting) => "Connecting…",
            _ => "Connect",
        };
        let mut do_connect = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!connecting, egui::Button::new(label))
                .clicked()
            {
                do_connect = true;
            }
            if connecting {
                ui.spinner();
            }
        });
        if do_connect {
            self.start_connect(ctx, id.clone(), service_uri.clone());
        }

        ui.add_space(8.0);
        let connected = match self.connections.get(&id) {
            Some(ConnState::Failed(e)) => {
                ui.colored_label(DOT_ERR, format!("Failed: {e}"));
                None
            }
            Some(ConnState::Connected(d)) => Some((d.info.clone(), d.profiles.len())),
            _ => None,
        };

        if let Some((info, profile_count)) = connected {
            ui.label(egui::RichText::new("Device Information").strong());
            ui.add_space(2.0);
            egui::Grid::new("devinfo")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    meta_row(ui, "Manufacturer", &info.manufacturer);
                    meta_row(ui, "Model", &info.model);
                    meta_row(ui, "Firmware", &info.firmware);
                    meta_row(ui, "Serial", &info.serial);
                    meta_row(ui, "Hardware ID", &info.hardware_id);
                });
            ui.add_space(4.0);
            ui.weak(format!(
                "{profile_count} media profile(s) — see the Streams tab"
            ));
        }
    }

    fn detail_streams_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        let profiles = match self.connections.get(id) {
            Some(ConnState::Connected(details)) => details.profiles.clone(),
            _ => {
                ui.add_space(8.0);
                ui.weak("Connect on the Info tab to load media profiles.");
                return;
            }
        };

        ui.add_space(6.0);

        // One row per channel: its main stream's URL + one snapshot. Sub/third
        // profiles exist on the device but aren't shown here — this is a
        // channel list, not a raw profile dump.
        let mains: Vec<onvif::MediaProfile> = Self::pick_main_profiles(&profiles)
            .into_iter()
            .cloned()
            .collect();
        if mains.is_empty() {
            ui.weak("No streams found.");
            return;
        }

        let mut to_capture: Vec<(String, String)> = Vec::new();

        for p in &mains {
            let key = format!("{id}|{}", p.token);

            ui.horizontal(|ui| {
                match &p.snapshot_uri {
                    None => {
                        ui.weak("(no snapshot)");
                    }
                    Some(url) => match self.snapshots.get(&key) {
                        Some(SnapState::Ready(tex)) => {
                            ui.add(egui::Image::new(tex).max_width(160.0));
                        }
                        Some(SnapState::Failed(e)) => {
                            ui.colored_label(DOT_ERR, e.as_str());
                        }
                        Some(SnapState::Loading) => {
                            ui.spinner();
                        }
                        None => {
                            ui.spinner();
                            to_capture.push((key.clone(), url.clone()));
                        }
                    },
                }

                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(p.name.as_str()).strong());
                    let mut meta = String::new();
                    if let Some(enc) = &p.encoding {
                        meta.push_str(enc);
                    }
                    if let Some((w, h)) = p.resolution {
                        if !meta.is_empty() {
                            meta.push_str(" · ");
                        }
                        meta.push_str(&format!("{w}×{h}"));
                    }
                    if !meta.is_empty() {
                        ui.label(egui::RichText::new(meta).weak());
                    }
                    if let Some(uri) = &p.rtsp_uri {
                        let mut shown = uri.clone();
                        ui.add(
                            egui::TextEdit::singleline(&mut shown)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                    }
                });
            });
            ui.separator();
        }

        for (key, url) in to_capture {
            self.start_capture(ctx, id, key, url);
        }
    }

    fn detail_live_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        let profiles = match self.connections.get(id) {
            Some(ConnState::Connected(d)) => d.profiles.clone(),
            _ => {
                ui.add_space(8.0);
                ui.weak("Connect on the Info tab first.");
                return;
            }
        };

        let mains = Self::pick_main_profiles(&profiles);
        let Some(profile) = mains.iter().next() else {
            ui.add_space(8.0);
            ui.weak("No stream available");
            return;
        };

        let Some(rtsp_uri) = profile.rtsp_uri.clone() else {
            ui.add_space(8.0);
            ui.weak("No RTSP URI for this profile.");
            return;
        };

        let playing = self
            .live
            .as_ref()
            .map(|(pid, _)| pid == id)
            .unwrap_or(false);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if !playing {
                if ui.button("▶ Play").clicked() {
                    let creds = self.creds.get(id).cloned().unwrap_or_default();
                    let url =
                        crate::video::with_credentials(&rtsp_uri, &creds.username, &creds.password);
                    let stream = crate::video::LiveStream::start(url, id.to_string());
                    self.live = Some((id.to_string(), stream));
                    self.live_texture = None;
                }
            } else if ui.button("■ Stop").clicked() {
                self.live = None;
                self.live_texture = None;
            }
        });

        ui.add_space(6.0);

        if playing {
            if let Some((_, stream)) = &self.live {
                if let Some(yuv) = stream.take_frame() {
                    let rgba = crate::video::yuv_to_rgba(&yuv);
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width as usize, rgba.height as usize],
                        &rgba.rgba,
                    );
                    match &mut self.live_texture {
                        Some(tex) => tex.set(color, egui::TextureOptions::LINEAR),
                        None => {
                            self.live_texture =
                                Some(ctx.load_texture("live", color, egui::TextureOptions::LINEAR));
                        }
                    }
                }
            }

            match &self.live_texture {
                Some(tex) => {
                    ui.add(egui::Image::new(tex).max_width(ui.available_width()));
                }
                None => {
                    ui.spinner();
                    ui.weak("Waiting for first frame…");
                }
            }
            ctx.request_repaint(); // it's live video — keep redrawing
        }
    }

    fn pick_main_profiles(profiles: &[onvif::MediaProfile]) -> Vec<&onvif::MediaProfile> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in profiles {
            match p.source_token.as_deref() {
                Some(tok) => {
                    if seen.insert(tok) {
                        out.push(p);
                    }
                }
                None => out.push(p), // no source identity to group by — show as-is
            }
        }
        out
    }

    fn detail_config_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, view: &DetailView) {
        let id = view.id.clone();
        let Some(service_uri) = view.service_uri.clone() else {
            ui.add_space(8.0);
            ui.weak("No service endpoint.");
            return;
        };
        let cfg = match self.connections.get(&id) {
            Some(ConnState::Connected(d)) => {
                Some((d.datetime.clone(), d.network.clone(), d.ntp.clone()))
            }
            _ => None,
        };
        let Some((datetime, network, ntp)) = cfg else {
            ui.add_space(8.0);
            ui.weak("Connect on the Info tab to load device configuration.");
            return;
        };
        let name = view.name.clone();
        let location = view.location.clone().unwrap_or_default();
        // let Some((datetime, network, ntp)) = cfg else { … };
        self.render_config(
            ctx,
            ui,
            &id,
            &service_uri,
            &datetime,
            &network,
            &ntp,
            &name,
            &location,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_config(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        id: &str,
        service_uri: &str,
        datetime: &Option<onvif::SystemDateTime>,
        network: &Option<onvif::NetworkConfig>,
        ntp: &Option<onvif::NtpConfig>,
        dev_name: &str,
        dev_location: &str,
    ) {
        // Snapshot confirm + last-result into locals so the card closures never
        // borrow self; work on a clone of the edit buffer, then write it back.
        let net_confirm = self.confirm.get(id).copied() == Some(ConfirmKind::ApplyNetwork);
        let reboot_confirm = self.confirm.get(id).copied() == Some(ConfirmKind::Reboot);
        let last_result = self.actions.get(id).cloned();

        let mut edit = self
            .edits
            .entry(id.to_string())
            .or_insert_with(|| config_edit_from(datetime, network, ntp, dev_name, dev_location))
            .clone();

        let mut action: Option<ActionKind> = None;
        let mut confirm_set: Option<ConfirmKind> = None;
        let mut confirm_clear = false;

        ui.add_space(6.0);

        if let Some((when, res)) = &last_result {
            if when.elapsed().as_secs() < 6 {
                match res {
                    Ok(m) => ui.colored_label(DOT_OK, m.as_str()),
                    Err(e) => ui.colored_label(DOT_ERR, format!("Failed: {e}")),
                };
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                ui.add_space(4.0);
            }
        }

        // Identification
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("Identification").strong());
            egui::Grid::new("ident")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut edit.name).desired_width(220.0));
                    ui.end_row();
                    ui.label("Location");
                    ui.add(egui::TextEdit::singleline(&mut edit.location).desired_width(220.0));
                    ui.end_row();
                });
            if ui.button("Apply identification").clicked() {
                action = Some(ActionKind::SetIdentification);
            }
        });
        ui.add_space(8.0);

        // Date & Time
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("Date & Time").strong());
            if let Some(dt) = datetime {
                ui.weak(format!(
                    "{}  ·  {}",
                    dt.mode,
                    dt.utc.as_deref().unwrap_or("—")
                ));
            }
            egui::Grid::new("dt")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Time zone (POSIX)");
                    ui.add(egui::TextEdit::singleline(&mut edit.tz).desired_width(160.0));
                    ui.end_row();
                    ui.label("Daylight saving");
                    ui.checkbox(&mut edit.dst, "");
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                if ui.button("Use NTP").clicked() {
                    action = Some(ActionKind::DateTimeNtp);
                }
                if ui.button("Sync to this computer").clicked() {
                    action = Some(ActionKind::DateTimeSync);
                }
            });
        });
        ui.add_space(8.0);

        // NTP
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("NTP").strong());
            egui::Grid::new("ntp")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("From DHCP");
                    ui.checkbox(&mut edit.ntp_from_dhcp, "");
                    ui.end_row();
                    ui.label("Server(s)");
                    ui.add_enabled(
                        !edit.ntp_from_dhcp,
                        egui::TextEdit::singleline(&mut edit.ntp_server).desired_width(220.0),
                    );
                    ui.end_row();
                });
            if ui.button("Apply NTP").clicked() {
                action = Some(ActionKind::SetNtp);
            }
            ui.weak("ONVIF NTP is server + FromDHCP only (port/interval are vendor-specific).");
        });
        ui.add_space(8.0);

        // Network
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("Network").strong());
            egui::Grid::new("net")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    if let Some(mac) = network.as_ref().and_then(|n| n.mac.as_deref()) {
                        ui.label("MAC");
                        ui.label(egui::RichText::new(mac).monospace());
                        ui.end_row();
                    }
                    ui.label("Hostname");
                    ui.add(egui::TextEdit::singleline(&mut edit.hostname).desired_width(180.0));
                    ui.end_row();
                    ui.label("DHCP");
                    ui.checkbox(&mut edit.net.dhcp, "");
                    ui.end_row();
                    ui.label("IP");
                    ui.add_enabled(
                        !edit.net.dhcp,
                        egui::TextEdit::singleline(&mut edit.net.ip).desired_width(180.0),
                    );
                    ui.end_row();
                    ui.label("Prefix");
                    ui.add_enabled(
                        !edit.net.dhcp,
                        egui::TextEdit::singleline(&mut edit.net.prefix).desired_width(60.0),
                    );
                    ui.end_row();
                    ui.label("Gateway");
                    ui.add_enabled(
                        !edit.net.dhcp,
                        egui::TextEdit::singleline(&mut edit.net.gateway).desired_width(180.0),
                    );
                    ui.end_row();
                    ui.label("DNS");
                    ui.add(egui::TextEdit::singleline(&mut edit.net.dns).desired_width(220.0));
                    ui.end_row();
                });
            if net_confirm {
                ui.colored_label(
                    DOT_ERR,
                    "⚠ Can change the camera's IP and disconnect it. Confirm?",
                );
                ui.horizontal(|ui| {
                    if ui.button("Confirm apply").clicked() {
                        action = Some(ActionKind::ApplyNetwork);
                        confirm_clear = true;
                    }
                    if ui.button("Cancel").clicked() {
                        confirm_clear = true;
                    }
                });
            } else if ui.button("Apply network…").clicked() {
                confirm_set = Some(ConfirmKind::ApplyNetwork);
            }
        });
        ui.add_space(8.0);

        // Actions
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new("Actions").strong());
            if reboot_confirm {
                ui.horizontal(|ui| {
                    if ui.button("Confirm reboot").clicked() {
                        action = Some(ActionKind::Reboot);
                        confirm_clear = true;
                    }
                    if ui.button("Cancel").clicked() {
                        confirm_clear = true;
                    }
                });
            } else if ui.button("Reboot…").clicked() {
                confirm_set = Some(ConfirmKind::Reboot);
            }
        });

        // Write back + apply deferred mutations.
        self.edits.insert(id.to_string(), edit);
        if confirm_clear {
            self.confirm.remove(id);
        }
        if let Some(k) = confirm_set {
            self.confirm.insert(id.to_string(), k);
        }
        if let Some(a) = action {
            self.run_action(ctx, id, service_uri, a);
        }
    }
}

impl eframe::App for OdmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(&ctx, ui));

        egui::Panel::left("device_list")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| self.device_list(ui));

        egui::CentralPanel::default().show(ui, |ui| self.detail(&ctx, ui));
    }
}

// --- small view helpers ---

fn device_row(
    ui: &mut egui::Ui,
    dev: &DiscoveredDevice,
    selected: bool,
    dot: egui::Color32,
) -> egui::Response {
    let resp = ui
        .horizontal(|ui| {
            ui.colored_label(dot, "●");
            ui.selectable_label(selected, dev.name.as_str())
        })
        .inner;

    ui.horizontal(|ui| {
        ui.add_space(22.0);
        ui.label(
            egui::RichText::new(dev.host.as_str())
                .small()
                .weak()
                .monospace(),
        );
    });
    ui.add_space(4.0);

    resp
}

fn meta_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(egui::RichText::new(key).weak());
    ui.label(egui::RichText::new(value).monospace());
    ui.end_row();
}

fn net_edit_from(network: &Option<onvif::NetworkConfig>) -> NetEdit {
    match network {
        Some(n) => NetEdit {
            token: n.interface_token.clone(),
            dhcp: n.dhcp,
            ip: n.ip.clone().unwrap_or_default(),
            prefix: n
                .prefix
                .map(|p| p.to_string())
                .unwrap_or_else(|| "24".into()),
            gateway: n.gateway.clone().unwrap_or_default(),
            dns: n.dns.join(", "),
        },
        None => NetEdit::default(),
    }
}

fn config_edit_from(
    datetime: &Option<onvif::SystemDateTime>,
    network: &Option<onvif::NetworkConfig>,
    ntp: &Option<onvif::NtpConfig>,
    dev_name: &str,
    dev_location: &str,
) -> ConfigEdit {
    ConfigEdit {
        name: dev_name.to_string(),
        location: dev_location.to_string(),
        tz: datetime
            .as_ref()
            .and_then(|d| d.timezone.clone())
            .unwrap_or_default(),
        dst: datetime.as_ref().map(|d| d.dst).unwrap_or(false),
        ntp_from_dhcp: ntp.as_ref().map(|n| n.from_dhcp).unwrap_or(false),
        ntp_server: ntp
            .as_ref()
            .map(|n| n.servers.join(", "))
            .unwrap_or_default(),
        hostname: network
            .as_ref()
            .and_then(|n| n.hostname.clone())
            .unwrap_or_default(),
        net: net_edit_from(network),
    }
}

/// The device service endpoint to talk to — prefer an IPv4 http XAddr.
fn device_service_uri(dev: &DiscoveredDevice) -> Option<&str> {
    dev.xaddrs
        .iter()
        .map(String::as_str)
        .find(|u| u.starts_with("http") && !u.contains('['))
        .or_else(|| dev.xaddrs.first().map(String::as_str))
}
