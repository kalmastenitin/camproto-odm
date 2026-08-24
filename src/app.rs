// src/app.rs

use eframe::egui;

use chrono::{Duration as ChronoDuration, NaiveDate, NaiveDateTime};
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
#[derive(Clone, Default)]
struct DeviceCreds {
    username: String,
    password: String,
}

struct ConnectMsg {
    id: String,
    result: Result<onvif::DeviceDetails, String>,
}

enum ConnState {
    Connecting,
    Connected(Box<onvif::DeviceDetails>),
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
    Events,
    Playback,
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

// events struct
#[derive(Clone)]
struct LiveEvent {
    at: std::time::SystemTime,
    topic: String,
    source: Option<String>,
    data: Vec<(String, String)>,
    snapshot_texture: Option<egui::TextureHandle>,
}

struct EventsHandle {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct PlaybackSession {
    device_id: String,
    label: String,
    /// GetReplayUri result with the channel already rewritten, NO credentials.
    /// Reused for every seek — we only vary the Range, never re-resolve.
    replay_template: String,
    username: String,
    password: String,
    day: NaiveDate,            // calendar selection
    span_start: NaiveDateTime, // recorded footage bounds (device clock)
    span_end: NaiveDateTime,
    seek_dt: NaiveDateTime, // instant the current stream started from
    seek_at: std::time::Instant,
}

impl Drop for EventsHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
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

    live: Option<(String, String, crate::video::LiveStream)>,
    live_texture: Option<egui::TextureHandle>,

    ptz_presets: HashMap<String, Vec<onvif::PtzPreset>>,
    ptz_preset_name: HashMap<String, String>,
    ptz_active: HashMap<String, (f32, f32, f32)>,
    ptz_speed: HashMap<String, f32>,

    ptz_preset_tx: Sender<(String, Vec<onvif::PtzPreset>)>,
    ptz_preset_rx: Receiver<(String, Vec<onvif::PtzPreset>)>,

    imaging_settings: HashMap<String, onvif::ImagingSettings>,
    imaging_ranges: HashMap<String, onvif::ImagingRanges>,
    imaging_loaded: HashMap<String, bool>,
    imaging_tx: Sender<(String, onvif::ImagingSettings, onvif::ImagingRanges)>,
    imaging_rx: Receiver<(String, onvif::ImagingSettings, onvif::ImagingRanges)>,
    imaging_panel_open: HashMap<String, bool>,

    events_log: HashMap<String, Vec<LiveEvent>>, // per-device event log
    events_handles: HashMap<String, EventsHandle>, // per-device background poller
    events_tx: Sender<(String, onvif::EventNotification, Option<Vec<u8>>)>,
    events_rx: Receiver<(String, onvif::EventNotification, Option<Vec<u8>>)>,

    recordings: HashMap<String, Vec<onvif::Recording>>, // device_id -> recordings
    recordings_loaded: HashMap<String, bool>,
    recordings_tx: Sender<(String, Vec<onvif::Recording>)>,
    recordings_rx: Receiver<(String, Vec<onvif::Recording>)>,
    playback_range: HashMap<String, (String, String)>,

    playback: Option<PlaybackSession>,
}

impl OdmApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (connect_tx, connect_rx) = channel();
        let (snapshot_tx, snapshot_rx) = channel();
        let (action_tx, action_rx) = channel();
        let (ptz_preset_tx, ptz_preset_rx) = channel();
        let (imaging_tx, imaging_rx) = channel();
        let (events_tx, events_rx) = channel();
        let (recordings_tx, recordings_rx) = channel();

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
            ptz_presets: HashMap::new(),
            ptz_preset_name: HashMap::new(),
            ptz_active: HashMap::new(),
            ptz_speed: HashMap::new(),
            ptz_preset_tx,
            ptz_preset_rx,
            imaging_settings: HashMap::new(),
            imaging_ranges: HashMap::new(),
            imaging_loaded: HashMap::new(),
            imaging_tx,
            imaging_rx,
            imaging_panel_open: HashMap::new(),
            events_log: HashMap::new(),
            events_handles: HashMap::new(),
            events_tx,
            events_rx,

            recordings: HashMap::new(), // device_id -> recordings
            recordings_loaded: HashMap::new(),
            recordings_tx,
            recordings_rx,
            playback_range: HashMap::new(),

            playback: None,
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
                DiscoveryEvent::Found(d) => self.upsert(*d),
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
                Ok(info) => ConnState::Connected(Box::new(info)),
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

        while let Ok((key, presets)) = self.ptz_preset_rx.try_recv() {
            self.ptz_presets.insert(key, presets);
        }

        while let Ok((key, settings, ranges)) = self.imaging_rx.try_recv() {
            self.imaging_settings.insert(key.clone(), settings);
            self.imaging_ranges.insert(key.clone(), ranges);
            self.imaging_loaded.insert(key, true);
        }

        while let Ok((key, recs)) = self.recordings_rx.try_recv() {
            self.recordings.insert(key.clone(), recs);
            self.recordings_loaded.insert(key, true);
        }

        while let Ok((device_id, event, snapshot_bytes)) = self.events_rx.try_recv() {
            let snapshot_texture = snapshot_bytes.and_then(|bytes| {
                let img = image::load_from_memory(&bytes).ok()?;
                let rgba = img.to_rgba8();
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    rgba.as_raw(),
                );
                Some(ctx.load_texture(
                    format!(
                        "event-{}-{}",
                        device_id,
                        event.received_at.elapsed().unwrap_or_default().as_millis()
                    ),
                    color,
                    egui::TextureOptions::LINEAR,
                ))
            });

            let entry = self.events_log.entry(device_id).or_default();
            entry.push(LiveEvent {
                at: event.received_at,
                topic: event.topic,
                source: event.source,
                data: event.data,
                snapshot_texture,
            });
            // Bounded ring: keep the most recent 100 events per device.
            if entry.len() > 100 {
                let drop_count = entry.len() - 100;
                entry.drain(..drop_count);
            }
        }
    }

    fn upsert(&mut self, d: DiscoveredDevice) {
        if let Some(row) = self.devices.iter_mut().find(|x| x.id == d.id) {
            row.merge_from(d);
        } else {
            self.devices.push(d);
            self.devices.sort_by_key(|a| a.name.to_lowercase());
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
            ui.selectable_value(&mut self.active_tab, Tab::Events, "Events");
            ui.selectable_value(&mut self.active_tab, Tab::Config, "Config");
            ui.selectable_value(&mut self.active_tab, Tab::Playback, "Playback");
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
                Tab::Events => self.detail_events_tab(ctx, ui, &view.id),
                Tab::Playback => self.detail_playback_tab(ctx, ui, &view.id),
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
        let mains: Vec<onvif::MediaProfile> = Self::pick_main_profiles(&profiles)
            .into_iter()
            .cloned()
            .collect();

        if mains.is_empty() {
            ui.weak("No streams found.");
            return;
        }

        let mut to_capture: Vec<(String, String)> = Vec::new();
        let mut start_live: Option<(String, String)> = None; // (rtsp_url, channel_label)

        for p in &mains {
            let key = format!("{id}|{}", p.token);
            let is_playing = self
                .live
                .as_ref()
                .map(|(pid, label, _)| pid == id && label == &p.name)
                .unwrap_or(false);

            ui.horizontal(|ui| {
                // Snapshot thumbnail — passive, auto-loads once. Unchanged.
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
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(p.name.as_str()).strong());
                        if is_playing {
                            ui.colored_label(DOT_OK, "● Live");
                        }
                    });
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

                    // This is the trigger the reference tool calls "Live video" —
                    // clicking it is the ONLY thing that starts a stream. Nothing
                    // here auto-plays.
                    ui.horizontal(|ui| {
                        if let Some(uri) = &p.rtsp_uri {
                            let label = if is_playing {
                                "■ Stop"
                            } else {
                                "▶ Live video"
                            };
                            if ui.button(label).clicked() {
                                if is_playing {
                                    self.live = None; // Drop tears down the RTSP session
                                } else {
                                    start_live = Some((uri.clone(), p.name.clone()));
                                }
                            }
                        }
                    });
                });
            });
            ui.separator();
        }

        for (key, url) in to_capture {
            self.start_capture(ctx, id, key, url);
        }

        if let Some((rtsp_uri, label)) = start_live {
            let creds = self.creds.get(id).cloned().unwrap_or_default();
            let url = crate::video::with_credentials(&rtsp_uri, &creds.username, &creds.password);
            let stream = crate::video::LiveStream::start(url, format!("{id}|{label}"));
            self.live = Some((id.to_string(), label, stream));
            self.live_texture = None;
            self.active_tab = Tab::Live; // jump straight to the viewer, matching the reference
            self.playback = None;
        }
    }

    fn detail_live_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        let playing_here = self
            .live
            .as_ref()
            .map(|(pid, _, _)| pid == id)
            .unwrap_or(false);

        if !playing_here {
            ui.add_space(8.0);
            ui.weak(
                "No stream selected. Go to the Streams tab and click \"Live video\" on a channel.",
            );
            return;
        }

        let event_count = self.events_log.get(id).map(|v| v.len()).unwrap_or(0);
        if event_count > 0 {
            ui.horizontal(|ui| {
                ui.colored_label(DOT_OK, "🔔");
                ui.weak(format!("{event_count} event(s) \u{2014} see Events tab"));
            });
        }

        // Which profile is playing, and does it support PTZ?
        let label = self
            .live
            .as_ref()
            .map(|(_, l, _)| l.clone())
            .unwrap_or_default();
        let ptz = self
            .connections
            .get(id)
            .and_then(|c| match c {
                ConnState::Connected(d) => Some(&d.profiles),
                _ => None,
            })
            .and_then(|profs| profs.iter().find(|p| p.name == label))
            .and_then(|p| p.ptz.clone());

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&label).strong());
            if ptz.is_some() {
                ui.weak("· PTZ available");
            }
            if ui.button("■ Stop").clicked() {
                self.live = None;
                self.live_texture = None;
            }
        });
        ui.add_space(6.0);

        // Refresh the texture from the latest decoded RGBA frame.
        if let Some((_, _, stream)) = &self.live {
            if let Some(rgba) = stream.take_frame() {
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

        // Video + PTZ overlay laid out together.
        match &self.live_texture {
            Some(tex) => {
                let img_size = ui.available_size();
                let response = ui.add(
                    egui::Image::new(tex)
                        .max_size(img_size)
                        .sense(egui::Sense::hover()),
                );
                if let Some(ptz_info) = ptz.clone() {
                    self.ptz_overlay(ctx, ui, id, &ptz_info, response.rect);
                }

                let imaging = self
                    .connections
                    .get(id)
                    .and_then(|c| match c {
                        ConnState::Connected(d) => Some(&d.profiles),
                        _ => None,
                    })
                    .and_then(|profs| profs.iter().find(|p| p.name == label))
                    .and_then(|p| p.imaging.clone());

                if let Some(img_info) = imaging {
                    self.imaging_overlay(ctx, ui, id, &img_info, response.rect);
                }
            }
            None => {
                ui.spinner();
                ui.weak("Waiting for first frame…");
            }
        }

        if self
            .playback
            .as_ref()
            .map(|s| s.device_id == id)
            .unwrap_or(false)
        {
            ui.separator();
            self.playback_scrubber(ctx, ui, id);
        }
        ctx.request_repaint();
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

    fn ptz_dispatch<F>(&self, ptz: onvif::PtzInfo, creds: onvif::Credentials, f: F)
    where
        F: FnOnce(&onvif::PtzInfo, &onvif::Credentials) -> Result<(), String> + Send + 'static,
    {
        std::thread::spawn(move || {
            if let Err(e) = f(&ptz, &creds) {
                eprintln!("PTZ command failed: {e}");
            }
        });
    }

    fn ptz_overlay(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        id: &str,
        ptz: &onvif::PtzInfo,
        video_rect: egui::Rect,
    ) {
        use eframe::egui::{Color32, Rect, Sense, Stroke, UiBuilder, Vec2};

        let creds = self.creds.get(id).cloned().unwrap_or_default();
        let onvif_creds = onvif::Credentials {
            username: creds.username.clone(),
            password: creds.password.clone(),
        };
        let key = ptz.profile_token.clone();

        // Lazy-load presets, actually delivering them back to the UI thread.
        if !self.ptz_presets.contains_key(&key) {
            self.ptz_presets.insert(key.clone(), Vec::new());
            let ptz_clone = ptz.clone();
            let creds_clone = onvif_creds.clone();
            let ctx_clone = ctx.clone();
            let tx = self.ptz_preset_tx.clone();
            let tx_key = key.clone();
            std::thread::spawn(move || {
                if let Ok(presets) = onvif::get_presets(&ptz_clone, &creds_clone) {
                    let _ = tx.send((tx_key, presets));
                }
                ctx_clone.request_repaint();
            });
        }

        // ---- direction pad, drawn as filled triangles instead of unicode glyphs
        // ---- so it looks identical across every OS and font.
        let pad_center = video_rect.left_bottom() + Vec2::new(80.0, -100.0);
        let btn = Vec2::splat(36.0);

        let mut cmd: Option<(f32, f32, f32)> = None;
        let mut do_stop = false;

        // Directions: (label, offset, pan, tilt, zoom, arrow-shape-direction)
        // arrow_dir: 0=up, 1=down, 2=left, 3=right, 4=home (center dot)
        let vel = *self.ptz_speed.get(&key).unwrap_or(&0.5);
        let dirs = [
            (Vec2::new(0.0, -40.0), 0.0, vel, 0.0, 0u8),  // up
            (Vec2::new(0.0, 40.0), 0.0, -vel, 0.0, 1u8),  // down
            (Vec2::new(-40.0, 0.0), -vel, 0.0, 0.0, 2u8), // left
            (Vec2::new(40.0, 0.0), vel, 0.0, 0.0, 3u8),   // right
            (Vec2::new(0.0, 0.0), 0.0, 0.0, 0.0, 4u8),    // home (center)
        ];

        // Zoom buttons at right side of video
        let zoom_center = video_rect.right_center() + Vec2::new(-40.0, 0.0);
        let zooms = [
            (zoom_center + Vec2::new(0.0, -25.0), 0.0, 0.0, vel, b'+'),
            (zoom_center + Vec2::new(0.0, 25.0), 0.0, 0.0, -vel, b'-'),
        ];

        ui.scope_builder(UiBuilder::new().max_rect(video_rect), |ui| {
            let painter = ui.painter().clone();

            for (offset, dp, dt, dz, shape) in dirs {
                let rect = Rect::from_center_size(pad_center + offset, btn);
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                let held = resp.is_pointer_button_down_on();

                // Background
                let bg = if held {
                    Color32::from_rgba_unmultiplied(80, 140, 220, 200)
                } else if resp.hovered() {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 45)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 70) // was 110 — the dark square
                };
                painter.rect_filled(rect, 6.0, bg);
                painter.rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
                    egui::StrokeKind::Middle,
                );

                // Glyph — drawn as a triangle (or dot for home).
                let c = rect.center();
                let s = 9.0; // half-size of the arrow triangle
                let white = Color32::WHITE;
                match shape {
                    0 => painter.add(egui::Shape::convex_polygon(
                        vec![
                            c + Vec2::new(0.0, -s),
                            c + Vec2::new(-s, s),
                            c + Vec2::new(s, s),
                        ],
                        white,
                        Stroke::NONE,
                    )),
                    1 => painter.add(egui::Shape::convex_polygon(
                        vec![
                            c + Vec2::new(0.0, s),
                            c + Vec2::new(-s, -s),
                            c + Vec2::new(s, -s),
                        ],
                        white,
                        Stroke::NONE,
                    )),
                    2 => painter.add(egui::Shape::convex_polygon(
                        vec![
                            c + Vec2::new(-s, 0.0),
                            c + Vec2::new(s, -s),
                            c + Vec2::new(s, s),
                        ],
                        white,
                        Stroke::NONE,
                    )),
                    3 => painter.add(egui::Shape::convex_polygon(
                        vec![
                            c + Vec2::new(s, 0.0),
                            c + Vec2::new(-s, -s),
                            c + Vec2::new(-s, s),
                        ],
                        white,
                        Stroke::NONE,
                    )),
                    _ => painter.add(egui::Shape::circle_filled(c, 5.0, white)),
                };

                // Motion dispatch
                let was_active = self.ptz_active.contains_key(&key);
                if shape == 4 {
                    // Home = GotoPreset("1"), only on release-click (not press-and-hold).
                    if resp.clicked() {
                        let ptz_clone = ptz.clone();
                        let creds_clone = onvif_creds.clone();
                        std::thread::spawn(move || {
                            let _ = onvif::goto_preset(&ptz_clone, &creds_clone, "1");
                        });
                    }
                } else if held && !was_active {
                    cmd = Some((dp, dt, dz));
                }
            }

            // Zoom pair
            for (center, dp, dt, dz, sign) in zooms {
                let rect = Rect::from_center_size(center, btn);
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                let held = resp.is_pointer_button_down_on();
                let bg = if held {
                    Color32::from_rgb(80, 140, 220)
                } else if resp.hovered() {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                } else {
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110)
                };
                painter.rect_filled(rect, 6.0, bg);
                painter.rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 140)),
                    egui::StrokeKind::Middle,
                );

                // Draw a thick "+" or horizontal bar for "-"
                let c = rect.center();
                let bar = 12.0;
                let thick = 3.0;
                painter.line_segment(
                    [c - Vec2::new(bar, 0.0), c + Vec2::new(bar, 0.0)],
                    Stroke::new(thick, Color32::WHITE),
                );
                if sign == b'+' {
                    painter.line_segment(
                        [c - Vec2::new(0.0, bar), c + Vec2::new(0.0, bar)],
                        Stroke::new(thick, Color32::WHITE),
                    );
                }

                let was_active = self.ptz_active.contains_key(&key);
                if held && !was_active {
                    cmd = Some((dp, dt, dz));
                }
            }

            // Speed slider — properly widget-based, below the pad.
            let slider_rect =
                Rect::from_center_size(pad_center + Vec2::new(0.0, 80.0), Vec2::new(160.0, 22.0));
            let entry = self.ptz_speed.entry(key.clone()).or_insert(0.5);
            ui.put(
                slider_rect,
                egui::Slider::new(entry, 0.05..=1.0)
                    .text("Speed")
                    .fixed_decimals(2),
            );

            // If nothing is held anymore and we were commanding motion → Stop.
            if self.ptz_active.contains_key(&key) && !ctx.input(|i| i.pointer.primary_down()) {
                do_stop = true;
            }
        });

        // Dispatch collected commands (outside the closure so borrow is clean).
        if let Some((p, t, z)) = cmd {
            self.ptz_active.insert(key.clone(), (p, t, z));
            let ptz_clone = ptz.clone();
            let creds_clone = onvif_creds.clone();
            self.ptz_dispatch(ptz_clone, creds_clone, move |ptz, creds| {
                onvif::continuous_move(ptz, creds, p, t, z)
            });
        }
        if do_stop {
            self.ptz_active.remove(&key);
            let ptz_clone = ptz.clone();
            let creds_clone = onvif_creds.clone();
            self.ptz_dispatch(ptz_clone, creds_clone, onvif::stop);
        }

        // ---- Presets panel: scrollable, capped height, no run-off.
        let panel_pos = video_rect.right_top() + Vec2::new(-200.0, 20.0);
        let panel_size = Vec2::new(190.0, 260.0);
        let panel_rect = Rect::from_min_size(panel_pos, panel_size);

        ui.scope_builder(UiBuilder::new().max_rect(panel_rect), |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::from_rgba_unmultiplied(15, 15, 18, 170))
                .stroke(Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 50),
                ))
                .show(ui, |ui| {
                    // Blend the buttons / text field into the translucent panel so they
                    // don't punch opaque rectangles over the video.
                    let v = ui.visuals_mut();
                    v.widgets.inactive.weak_bg_fill =
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22);
                    v.widgets.hovered.weak_bg_fill =
                        Color32::from_rgba_unmultiplied(255, 255, 255, 50);
                    v.widgets.active.weak_bg_fill =
                        Color32::from_rgba_unmultiplied(80, 140, 220, 190);
                    v.extreme_bg_color = Color32::from_rgba_unmultiplied(0, 0, 0, 90); // TextEdit bg

                    ui.set_max_width(panel_size.x - 8.0);
                    ui.label(egui::RichText::new("Presets").strong());

                    let name = self.ptz_preset_name.entry(key.clone()).or_default();
                    let mut do_set: Option<String> = None;
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(name)
                                .desired_width(100.0)
                                .hint_text("name"),
                        );
                        if ui.button("Set").clicked() {
                            do_set = Some(name.clone());
                        }
                    });
                    if let Some(preset_name) = do_set {
                        let ptz_clone = ptz.clone();
                        let creds_clone = onvif_creds.clone();
                        let tx = self.ptz_preset_tx.clone();
                        let tx_key = key.clone();
                        let ctx_clone = ctx.clone();
                        std::thread::spawn(move || {
                            if onvif::set_preset(&ptz_clone, &creds_clone, &preset_name).is_ok() {
                                if let Ok(presets) = onvif::get_presets(&ptz_clone, &creds_clone) {
                                    let _ = tx.send((tx_key, presets));
                                }
                            }
                            ctx_clone.request_repaint();
                        });
                    }

                    ui.separator();
                    let presets = self.ptz_presets.get(&key).cloned().unwrap_or_default();
                    if presets.is_empty() {
                        ui.weak("(none)");
                    } else {
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                for preset in &presets {
                                    let display = if preset.name.is_empty() {
                                        preset.token.as_str()
                                    } else {
                                        preset.name.as_str()
                                    };
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(display)
                                                    .min_size(Vec2::new(130.0, 20.0)),
                                            )
                                            .clicked()
                                        {
                                            let token = preset.token.clone();
                                            let ptz_clone = ptz.clone();
                                            let creds_clone = onvif_creds.clone();
                                            std::thread::spawn(move || {
                                                let _ = onvif::goto_preset(
                                                    &ptz_clone,
                                                    &creds_clone,
                                                    &token,
                                                );
                                            });
                                        }
                                        if ui.small_button("🗑").clicked() {
                                            let token = preset.token.clone();
                                            let ptz_clone = ptz.clone();
                                            let creds_clone = onvif_creds.clone();
                                            let tx = self.ptz_preset_tx.clone();
                                            let tx_key = key.clone();
                                            let ctx_clone = ctx.clone();
                                            std::thread::spawn(move || {
                                                if onvif::remove_preset(
                                                    &ptz_clone,
                                                    &creds_clone,
                                                    &token,
                                                )
                                                .is_ok()
                                                {
                                                    if let Ok(p) =
                                                        onvif::get_presets(&ptz_clone, &creds_clone)
                                                    {
                                                        let _ = tx.send((tx_key, p));
                                                    }
                                                }
                                                ctx_clone.request_repaint();
                                            });
                                        }
                                    });
                                }
                            });
                    }
                });
        });
    }

    fn imaging_overlay(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        id: &str,
        imaging: &onvif::ImagingInfo,
        video_rect: egui::Rect,
    ) {
        use eframe::egui::{UiBuilder, Vec2};

        let creds = self.creds.get(id).cloned().unwrap_or_default();
        let onvif_creds = onvif::Credentials {
            username: creds.username.clone(),
            password: creds.password.clone(),
        };
        let key = imaging.source_token.clone();

        // Lazy-load imaging settings + capability ranges (once per source).
        if !self.imaging_loaded.contains_key(&key) {
            self.imaging_loaded.insert(key.clone(), false);
            let img_clone = imaging.clone();
            let creds_clone = onvif_creds.clone();
            let tx = self.imaging_tx.clone();
            let tx_key = key.clone();
            let ctx_clone = ctx.clone();
            std::thread::spawn(move || {
                let settings =
                    onvif::get_imaging_settings(&img_clone, &creds_clone).unwrap_or_default();
                let ranges =
                    onvif::get_imaging_options(&img_clone, &creds_clone).unwrap_or_default();
                let _ = tx.send((tx_key, settings, ranges));
                ctx_clone.request_repaint();
            });
        }

        // Toggle button in the top-left of the video.
        let toggle_rect = egui::Rect::from_min_size(
            video_rect.left_top() + Vec2::new(10.0, 10.0),
            Vec2::new(80.0, 24.0),
        );
        let is_open = *self.imaging_panel_open.get(&key).unwrap_or(&false);
        ui.scope_builder(UiBuilder::new().max_rect(toggle_rect), |ui| {
            if ui
                .button(if is_open { "▼ Image" } else { "▶ Image" })
                .clicked()
            {
                self.imaging_panel_open.insert(key.clone(), !is_open);
            }
        });

        if !is_open {
            return;
        }

        // Full panel, on the left side, below the toggle.
        let panel_pos = video_rect.left_top() + Vec2::new(10.0, 44.0);
        let panel_size = Vec2::new(280.0, 460.0);
        let panel_rect = egui::Rect::from_min_size(panel_pos, panel_size);

        // Snapshot state we need in the closure without keeping self borrowed.
        let mut settings = self.imaging_settings.get(&key).cloned().unwrap_or_default();
        let ranges = self.imaging_ranges.get(&key).cloned().unwrap_or_default();
        let mut changed = false;

        ui.scope_builder(UiBuilder::new().max_rect(panel_rect), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(panel_size.x - 8.0);
                ui.label(egui::RichText::new("Imaging").strong());
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(panel_size.y - 60.0)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        // --- basic sliders (capability-gated on range presence)
                        changed |= slider(
                            ui,
                            "Brightness",
                            &mut settings.brightness,
                            ranges.brightness,
                        );
                        changed |= slider(ui, "Contrast", &mut settings.contrast, ranges.contrast);
                        changed |= slider(
                            ui,
                            "Saturation",
                            &mut settings.color_saturation,
                            ranges.color_saturation,
                        );
                        changed |=
                            slider(ui, "Sharpness", &mut settings.sharpness, ranges.sharpness);

                        // --- exposure
                        if !ranges.exposure_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("Exposure").strong());
                            changed |= combo(
                                ui,
                                "exp_mode",
                                "Mode",
                                &mut settings.exposure_mode,
                                &ranges.exposure_modes,
                            );
                            let manual = settings.exposure_mode.as_deref() == Some("MANUAL");
                            if manual {
                                changed |= slider(
                                    ui,
                                    "Time (µs)",
                                    &mut settings.exposure_time,
                                    ranges.exposure_time,
                                );
                                changed |= slider(
                                    ui,
                                    "Gain",
                                    &mut settings.exposure_gain,
                                    ranges.exposure_gain,
                                );
                                changed |= slider(
                                    ui,
                                    "Iris",
                                    &mut settings.exposure_iris,
                                    ranges.exposure_iris,
                                );
                            }
                        }

                        // --- white balance
                        if !ranges.wb_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("White balance").strong());
                            changed |= combo(
                                ui,
                                "wb_mode",
                                "Mode",
                                &mut settings.white_balance_mode,
                                &ranges.wb_modes,
                            );
                            let manual = settings.white_balance_mode.as_deref() == Some("MANUAL");
                            if manual {
                                changed |= slider(
                                    ui,
                                    "Cr gain",
                                    &mut settings.white_balance_cr_gain,
                                    ranges.wb_cr_gain,
                                );
                                changed |= slider(
                                    ui,
                                    "Cb gain",
                                    &mut settings.white_balance_cb_gain,
                                    ranges.wb_cb_gain,
                                );
                            }
                        }

                        // --- WDR
                        if !ranges.wdr_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("Wide dynamic range").strong());
                            changed |= combo(
                                ui,
                                "wdr_mode",
                                "Mode",
                                &mut settings.wdr_mode,
                                &ranges.wdr_modes,
                            );
                            if settings.wdr_mode.as_deref() == Some("ON") {
                                changed |=
                                    slider(ui, "Level", &mut settings.wdr_level, ranges.wdr_level);
                            }
                        }

                        // --- backlight
                        if !ranges.backlight_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("Backlight compensation").strong());
                            changed |= combo(
                                ui,
                                "bl_mode",
                                "Mode",
                                &mut settings.backlight_mode,
                                &ranges.backlight_modes,
                            );
                            if settings.backlight_mode.as_deref() == Some("ON") {
                                changed |= slider(
                                    ui,
                                    "Level",
                                    &mut settings.backlight_level,
                                    ranges.backlight_level,
                                );
                            }
                        }

                        // --- image stabilization
                        if !ranges.image_stab_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("Image stabilization").strong());
                            changed |= combo(
                                ui,
                                "is_mode",
                                "Mode",
                                &mut settings.image_stab_mode,
                                &ranges.image_stab_modes,
                            );
                            if settings.image_stab_mode.as_deref() != Some("OFF") {
                                changed |= slider(
                                    ui,
                                    "Level",
                                    &mut settings.image_stab_level,
                                    ranges.image_stab_level,
                                );
                            }
                        }

                        // --- IR cut filter (day/night)
                        if !ranges.ir_cut_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("IR cut filter").strong());
                            changed |= combo(
                                ui,
                                "ir_mode",
                                "Mode",
                                &mut settings.ir_cut_filter,
                                &ranges.ir_cut_modes,
                            );
                        }

                        // --- focus (mode + press-and-hold near/far)
                        if !ranges.focus_modes.is_empty() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("Focus").strong());
                            changed |= combo(
                                ui,
                                "focus_mode",
                                "Mode",
                                &mut settings.focus_mode,
                                &ranges.focus_modes,
                            );
                            if settings.focus_mode.as_deref() == Some("MANUAL") {
                                let (near, far) = ui
                                    .horizontal(|ui| {
                                        let near = ui.button("◀ Near");
                                        let far = ui.button("Far ▶");
                                        (near, far)
                                    })
                                    .inner;

                                let speed = ranges.focus_speed.map(|(_, m)| m * 0.5).unwrap_or(0.5);
                                let img_clone = imaging.clone();
                                let creds_clone = onvif_creds.clone();

                                if near.is_pointer_button_down_on() {
                                    let img = img_clone.clone();
                                    let creds = creds_clone.clone();
                                    std::thread::spawn(move || {
                                        let _ = onvif::focus_continuous_move(&img, &creds, -speed);
                                    });
                                }
                                if far.is_pointer_button_down_on() {
                                    let img = img_clone.clone();
                                    let creds = creds_clone.clone();
                                    std::thread::spawn(move || {
                                        let _ = onvif::focus_continuous_move(&img, &creds, speed);
                                    });
                                }
                                // Release detection: fire Stop when neither button is held.
                                if !near.is_pointer_button_down_on()
                                    && !far.is_pointer_button_down_on()
                                    && (near.drag_stopped() || far.drag_stopped())
                                {
                                    let img = img_clone.clone();
                                    let creds = creds_clone.clone();
                                    std::thread::spawn(move || {
                                        let _ = onvif::focus_stop(&img, &creds);
                                    });
                                }
                            }
                        }
                    });
            });
        });

        // Persist any slider/combo change back into our cache and dispatch to
        // the camera. Debounce-lite: this fires per-frame on any modification;
        // for real-world use we'd throttle to release-only, but per-frame is
        // fine given cameras handle it and the request is fire-and-forget.
        if changed {
            self.imaging_settings.insert(key.clone(), settings.clone());
            let img_clone = imaging.clone();
            let creds_clone = onvif_creds.clone();
            std::thread::spawn(move || {
                if let Err(e) = onvif::set_imaging_settings(&img_clone, &creds_clone, &settings) {
                    eprintln!("SetImagingSettings failed: {e}");
                }
            });
        }
    }

    fn detail_events_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        // Check if events service is available on this device.
        let events_info = match self.connections.get(id) {
            Some(ConnState::Connected(d)) => d.events.clone(),
            _ => {
                ui.add_space(8.0);
                ui.weak("Connect on the Info tab first.");
                return;
            }
        };

        let Some(events_info) = events_info else {
            ui.add_space(8.0);
            ui.weak("This device doesn't advertise an ONVIF Events service.");
            return;
        };

        // Find a profile that has a snapshot URI, for on-event capture. Prefer
        // a main-stream profile (via pick_main_profiles) to avoid burdening
        // the sub streams.
        let snapshot_url = self
            .connections
            .get(id)
            .and_then(|c| match c {
                ConnState::Connected(d) => Some(&d.profiles),
                _ => None,
            })
            .and_then(|profs| {
                Self::pick_main_profiles(profs)
                    .into_iter()
                    .find_map(|p| p.snapshot_uri.clone())
            });

        let is_running = self.events_handles.contains_key(id);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if is_running {
                if ui.button("■ Stop").clicked() {
                    self.events_handles.remove(id);
                }
                ui.colored_label(DOT_OK, "● Subscribed");
            } else {
                if ui.button("▶ Start").clicked() {
                    self.start_events_poller(ctx, id, events_info.clone(), snapshot_url.clone());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = self.events_log.get(id).map(|v| v.len()).unwrap_or(0);
                ui.label(egui::RichText::new(format!("{count} event(s)")).weak());
                if !is_running && count > 0 && ui.button("Clear").clicked() {
                    self.events_log.remove(id);
                }
            });
        });
        ui.separator();

        // Reversed order — newest first, most useful for a live log.
        let events = self.events_log.get(id).cloned().unwrap_or_default();
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                if events.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.weak(if is_running {
                            "Waiting for events…"
                        } else {
                            "Click Start to begin receiving events."
                        });
                    });
                    return;
                }

                for evt in events.iter().rev() {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(tex) = &evt.snapshot_texture {
                                ui.add(egui::Image::new(tex).max_width(120.0));
                            }
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(short_topic(&evt.topic)).strong());
                                let secs_ago = evt.at.elapsed().map(|d| d.as_secs()).unwrap_or(0);
                                ui.weak(format!("{secs_ago}s ago"));
                                if let Some(src) = &evt.source {
                                    ui.weak(format!("source: {src}"));
                                }
                                for (k, v) in &evt.data {
                                    ui.label(
                                        egui::RichText::new(format!("{k} = {v}"))
                                            .small()
                                            .monospace(),
                                    );
                                }
                            });
                        });
                    });
                }
            });

        // Keep the "s ago" ticking while running.
        if is_running {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn detail_playback_tab(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        let (replay, profiles) = match self.connections.get(id) {
            Some(ConnState::Connected(d)) => (d.replay.clone(), d.profiles.clone()),
            _ => {
                ui.add_space(8.0);
                ui.weak("Connect on the Info tab first.");
                return;
            }
        };

        let Some(replay_info) = replay else {
            ui.add_space(8.0);
            ui.weak("This device doesn't advertise ONVIF Profile G (recording/replay).");
            return;
        };

        let creds = self.creds.get(id).cloned().unwrap_or_default();
        let onvif_creds = onvif::Credentials {
            username: creds.username.clone(),
            password: creds.password.clone(),
        };

        if !self.recordings_loaded.contains_key(id) {
            self.recordings_loaded.insert(id.to_string(), false);
            let replay_clone = replay_info.clone();
            let creds_clone = onvif_creds.clone();
            let tx = self.recordings_tx.clone();
            let tx_key = id.to_string();
            let ctx_clone = ctx.clone();
            std::thread::spawn(move || {
                let recs = onvif::get_recordings(&replay_clone, &creds_clone).unwrap_or_default();
                let _ = tx.send((tx_key, recs));
                ctx_clone.request_repaint();
            });
        }

        if !self.recordings_loaded.get(id).copied().unwrap_or(false) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Loading recordings from device…");
            });
            return;
        }

        let recordings = self.recordings.get(id).cloned().unwrap_or_default();
        if recordings.is_empty() {
            ui.add_space(8.0);
            ui.weak("No recordings available.");
            ui.weak("The device advertises Profile G but reports no recordings.");
            return;
        }

        let base_recording = recordings
            .iter()
            .max_by_key(|r| r.track_sources.len())
            .cloned()
            .unwrap_or_else(|| recordings[0].clone());

        let mains: Vec<onvif::MediaProfile> = Self::pick_main_profiles(&profiles)
            .into_iter()
            .cloned()
            .collect();

        if mains.is_empty() {
            ui.add_space(8.0);
            ui.weak("No channels found on this device.");
            return;
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!("{} channel(s) — pick one to replay", mains.len()))
                .strong(),
        );
        if let (Some(from), Some(to)) = (&base_recording.earliest, &base_recording.latest) {
            ui.weak(format!(
                "recorded footage spans {} → {}",
                short_iso(from),
                short_iso(to)
            ));
        }
        ui.separator();

        // (label, channel_opt, start, end).  channel_opt = None → single camera,
        // replay the recording URL directly with no channel rewrite.
        let mut launch: Option<(String, Option<u32>, String, String)> = None;

        for p in &mains {
            let channel = channel_from_source_token(p.source_token.as_deref());
            let range_key = format!("{id}|playback|{}", p.token);

            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&p.name).strong());
                        let sub = match channel {
                            Some(ch) => format!(
                                "channel {}  ·  source {}",
                                ch,
                                p.source_token.as_deref().unwrap_or("—")
                            ),
                            None => format!("source {}", p.source_token.as_deref().unwrap_or("—")),
                        };
                        ui.label(egui::RichText::new(sub).small().weak());
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let range = self.playback_range.entry(range_key).or_insert_with(|| {
                            (
                                base_recording.earliest.clone().unwrap_or_default(),
                                base_recording.latest.clone().unwrap_or_default(),
                            )
                        });

                        let play = ui.button("▶ Play").clicked();

                        ui.add(
                            egui::TextEdit::singleline(&mut range.1)
                                .desired_width(170.0)
                                .hint_text("YYYY-MM-DDTHH:MM:SSZ"),
                        );
                        ui.label("to");
                        ui.add(
                            egui::TextEdit::singleline(&mut range.0)
                                .desired_width(170.0)
                                .hint_text("YYYY-MM-DDTHH:MM:SSZ"),
                        );

                        if play {
                            launch =
                                Some((p.name.clone(), channel, range.0.clone(), range.1.clone()));
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }

        if let Some((label, channel, start, end)) = launch {
            match onvif::get_replay_uri(&replay_info, &onvif_creds, &base_recording.token) {
                Ok(template_url) => {
                    let replay_template = match channel {
                        Some(ch) => rewrite_channel(&template_url, ch),
                        None => template_url,
                    };

                    let span_start = base_recording.earliest.as_deref().and_then(parse_device_dt);
                    let span_end = base_recording.latest.as_deref().and_then(parse_device_dt);
                    let seek_dt = parse_device_dt(&start).or(span_start).unwrap_or_default();

                    let url = crate::video::with_credentials(
                        &replay_template,
                        &creds.username,
                        &creds.password,
                    );
                    let stream = crate::video::LiveStream::start_playback(
                        url,
                        format!("{id}|playback|{label}"),
                        start.clone(),
                        end.clone(),
                    );
                    self.live = Some((id.to_string(), format!("{label} (playback)"), stream));
                    self.live_texture = None;

                    if let (Some(span_start), Some(span_end)) = (span_start, span_end) {
                        self.playback = Some(PlaybackSession {
                            device_id: id.to_string(),
                            label,
                            replay_template,
                            username: creds.username.clone(),
                            password: creds.password.clone(),
                            day: seek_dt.date(),
                            span_start,
                            span_end,
                            seek_dt,
                            seek_at: std::time::Instant::now(),
                        });
                    }
                    self.active_tab = Tab::Live;
                }
                Err(e) => eprintln!("GetReplayUri failed for {}: {e}", base_recording.token),
            }
        }
    }

    fn start_events_poller(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        events_info: onvif::EventsInfo,
        snapshot_url: Option<String>,
    ) {
        let creds = self.creds.get(id).cloned().unwrap_or_default();
        let onvif_creds = onvif::Credentials {
            username: creds.username,
            password: creds.password,
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        let device_id = id.to_string();

        std::thread::Builder::new()
            .name(format!("events-{id}"))
            .spawn(move || {
                use std::sync::atomic::Ordering;
                use std::time::{Duration, Instant};

                let sub = match onvif::create_pull_subscription(&events_info, &onvif_creds) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[events {device_id}] subscribe failed: {e}");
                        return;
                    }
                };
                eprintln!("[events {device_id}] subscription ready");

                let mut last_renew = Instant::now();

                while !stop_clone.load(Ordering::Relaxed) {
                    // Renew every 45s to keep the 60s subscription alive.
                    if last_renew.elapsed() > Duration::from_secs(45) {
                        if let Err(e) = onvif::renew_subscription(&sub, &onvif_creds) {
                            eprintln!("[events {device_id}] renew failed: {e}");
                        }
                        last_renew = Instant::now();
                    }

                    // Long-poll: 25s timeout, up to 16 messages per batch.
                    match onvif::pull_messages(&sub, &onvif_creds, 25, 16) {
                        Ok(events) => {
                            for evt in events {
                                // Fetch snapshot if we have a URL.
                                let snapshot_bytes = snapshot_url.as_ref().and_then(|url| {
                                    onvif::capture(url, &onvif_creds).ok().map(|s| s.rgba)
                                });
                                // NOTE: capture() returns Snapshot with decoded RGBA;
                                // we actually want the raw JPEG bytes to re-decode
                                // in drain(). See caveat below.
                                let _ = snapshot_bytes;

                                // For now, send without snapshot — the snapshot
                                // pipeline needs a raw-bytes accessor.
                                let _ = tx.send((device_id.clone(), evt, None));
                            }
                            ctx.request_repaint();
                        }
                        Err(e) => {
                            eprintln!("[events {device_id}] pull failed: {e}");
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
                }

                let _ = onvif::unsubscribe(&sub, &onvif_creds);
                eprintln!("[events {device_id}] subscription closed");
            })
            .expect("failed to spawn events thread");

        self.events_handles
            .insert(id.to_string(), EventsHandle { stop });
    }

    fn playback_scrubber(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &str) {
        use egui::{Color32, Sense, Stroke, Vec2};

        if self
            .playback
            .as_ref()
            .map(|s| s.device_id != id)
            .unwrap_or(true)
        {
            return;
        }

        // --- Calendar + adjacent-day nudge (mutates the selection in place) ---
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Date").weak());

            if let Some(s) = self.playback.as_mut() {
                let mut picked_day: jiff::civil::Date = s
                    .day
                    .format("%Y-%m-%d")
                    .to_string()
                    .parse()
                    .expect("valid date");

                let picker_id = format!("pb-day-{id}");

                ui.add(egui_extras::DatePickerButton::new(&mut picked_day).id_salt(&picker_id));

                if let Ok(new_day) =
                    chrono::NaiveDate::parse_from_str(&picked_day.to_string(), "%Y-%m-%d")
                {
                    s.day = new_day;
                }
            }

            if ui.small_button("◀").clicked() {
                if let Some(s) = self.playback.as_mut() {
                    s.day = s.day.pred_opt().unwrap_or(s.day);
                }
            }

            if ui.small_button("▶").clicked() {
                if let Some(s) = self.playback.as_mut() {
                    s.day = s.day.succ_opt().unwrap_or(s.day);
                }
            }
        });

        // --- Snapshot fields so we can mutate self after drawing ---
        let (day, span_start, span_end, seek_dt, seek_at, template, user, pass, label) = {
            let s = self.playback.as_ref().unwrap();
            (
                s.day,
                s.span_start,
                s.span_end,
                s.seek_dt,
                s.seek_at,
                s.replay_template.clone(),
                s.username.clone(),
                s.password.clone(),
                s.label.clone(),
            )
        };

        let day_start = day.and_hms_opt(0, 0, 0).unwrap();
        let day_end = day_start + ChronoDuration::days(1);
        let frac =
            |t: NaiveDateTime| ((t - day_start).num_seconds() as f32 / 86_400.0).clamp(0.0, 1.0);

        // Playhead estimate: assumes ~realtime replay (rate-control on). Good enough
        // for a scrubber; wire it to real frame PTS later if you want it exact.
        let playhead = seek_dt + ChronoDuration::milliseconds(seek_at.elapsed().as_millis() as i64);

        // --- Timeline bar ---
        ui.add_space(4.0);
        let width = ui.available_width().min(1000.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let w = rect.width();
        let top = rect.top() + 6.0;
        let h = 22.0;
        let track = egui::Rect::from_min_size(egui::pos2(rect.left(), top), Vec2::new(w, h));

        painter.rect_filled(
            track,
            4.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
        );

        // recorded footage on this day = span ∩ day
        let seg_start = span_start.max(day_start);
        let seg_end = span_end.min(day_end);
        if seg_end > seg_start {
            let x0 = rect.left() + frac(seg_start) * w;
            let x1 = rect.left() + frac(seg_end) * w;
            let seg = egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, top + h));
            painter.rect_filled(seg, 4.0, Color32::from_rgb(0x2f, 0x6f, 0xd6));
        }

        // hour ticks every 3h
        for hr in (0..=24).step_by(3) {
            let x = rect.left() + (hr as f32 / 24.0) * w;
            painter.line_segment(
                [egui::pos2(x, top), egui::pos2(x, top + h)],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
            );
            painter.text(
                egui::pos2(x, top + h + 2.0),
                egui::Align2::CENTER_TOP,
                format!("{hr:02}"),
                egui::FontId::proportional(10.0),
                Color32::from_rgba_unmultiplied(255, 255, 255, 140),
            );
        }

        // playhead (only when the day being viewed is the day being played)
        if seek_dt.date() == day && playhead >= day_start && playhead < day_end {
            let x = rect.left() + frac(playhead) * w;
            painter.line_segment(
                [egui::pos2(x, top - 3.0), egui::pos2(x, top + h + 3.0)],
                Stroke::new(2.0, Color32::from_rgb(0xff, 0xd0, 0x3d)),
            );
        }

        // drag/click preview + seek-on-release
        let mut seek_to: Option<NaiveDateTime> = None;
        if resp.dragged() || resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = ((pos.x - rect.left()) / w).clamp(0.0, 1.0);
                let px = rect.left() + f * w;
                painter.line_segment(
                    [egui::pos2(px, top - 3.0), egui::pos2(px, top + h + 3.0)],
                    Stroke::new(1.0, Color32::WHITE),
                );
                let t = day_start + ChronoDuration::seconds((f * 86_400.0) as i64);
                painter.text(
                    egui::pos2(px, rect.bottom() - 1.0),
                    egui::Align2::CENTER_BOTTOM,
                    t.format("%H:%M:%S").to_string(),
                    egui::FontId::proportional(11.0),
                    Color32::WHITE,
                );
            }
        }
        if resp.clicked() || resp.drag_stopped() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = ((pos.x - rect.left()) / w).clamp(0.0, 1.0);
                seek_to = Some(day_start + ChronoDuration::seconds((f * 86_400.0) as i64));
            }
        }

        ui.add_space(2.0);
        ui.weak(format!(
            "{label}  ·  {}",
            playhead.format("%Y-%m-%d %H:%M:%S")
        ));

        // --- commit the seek: restart the RTSP session at the new clock time ---
        if let Some(t) = seek_to {
            let t = t.clamp(span_start, span_end); // don't ask for empty regions
            let url = crate::video::with_credentials(&template, &user, &pass);
            let stream = crate::video::LiveStream::start_playback(
                url,
                format!("{id}|playback|seek|{label}"),
                fmt_device_dt(t),
                fmt_device_dt(span_end),
            );
            self.live = Some((id.to_string(), format!("{label} (playback)"), stream));

            if let Some(s) = self.playback.as_mut() {
                s.seek_dt = t;
                s.seek_at = std::time::Instant::now();
                s.day = t.date();
            }
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
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

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<f32>,
    range: Option<(f32, f32)>,
) -> bool {
    let Some((min, max)) = range else {
        return false;
    };
    let mut cur = value.unwrap_or((min + max) / 2.0);
    let resp = ui.add(egui::Slider::new(&mut cur, min..=max).text(label));
    if resp.changed() {
        *value = Some(cur);
        true
    } else {
        false
    }
}

fn combo(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    value: &mut Option<String>,
    options: &[String],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let mut cur = value.clone().unwrap_or_else(|| options[0].clone());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(cur.as_str())
        .show_ui(ui, |ui| {
            for opt in options {
                if ui
                    .selectable_value(&mut cur, opt.clone(), opt.as_str())
                    .changed()
                {
                    changed = true;
                }
            }
        });
    if changed {
        *value = Some(cur);
        ui.label(label);
    }
    changed
}

fn short_topic(topic: &str) -> String {
    // Full topics look like `tns1:RuleEngine/CellMotionDetector/Motion`
    // — the trailing segment is usually enough for a log line.
    topic.rsplit('/').next().unwrap_or(topic).to_string()
}

fn short_iso(iso: &str) -> String {
    iso.get(..16).unwrap_or(iso).replace('T', " ")
}

fn channel_from_source_token(src: Option<&str>) -> Option<u32> {
    let s = src?;
    let start = s
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    s[start..].parse::<u32>().ok().map(|n| n + 1)
}

fn rewrite_channel(url: &str, channel: u32) -> String {
    for sep in ['?', '&'] {
        let needle = format!("{sep}channel=");
        if let Some(pos) = url.find(&needle) {
            let val_start = pos + needle.len();
            let val_end = url[val_start..]
                .find('&')
                .map(|i| val_start + i)
                .unwrap_or(url.len());
            let mut out = String::with_capacity(url.len() + 4);
            out.push_str(&url[..val_start]);
            out.push_str(&channel.to_string());
            out.push_str(&url[val_end..]);
            return out;
        }
    }
    url.to_string()
}

fn parse_device_dt(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .ok()
}

fn fmt_device_dt(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
