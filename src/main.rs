#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui::ViewportBuilder;
use eframe::{App, Frame, egui};
use rosc::{OscMessage, OscPacket, OscType};
use std::fs;
use std::io;
use std::net::{ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
struct CueInfo {
    number: String,
    text: String,
    color: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct CueState {
    current: CueInfo,
    next: CueInfo,
    connected: bool,
    last_rx: Option<Instant>,
}

enum NetEvent {
    CueFired(CueInfo),
    #[allow(dead_code)]
    SubscribeOk(u32),
    SubscribeFail,
    Thump,
}

enum NetCmd {
    SetHost(String),
}

struct TheatreMixApp {
    state: CueState,
    rx: Receiver<NetEvent>,
    cmd_tx: Sender<NetCmd>,
    host: String,
    status: String,
    host_edit: String,
    always_on_top: bool,
    config_path: Option<PathBuf>,
    show_settings: bool,
}

impl TheatreMixApp {
    fn new(
        host: String,
        rx: Receiver<NetEvent>,
        cmd_tx: Sender<NetCmd>,
        config_path: Option<PathBuf>,
    ) -> Self {
        let mut state = CueState::default();
        state.next.text = "(not provided by OSC)".to_string();
        Self {
            state,
            rx,
            cmd_tx,
            host,
            status: "Connecting...".to_string(),
            host_edit: String::new(),
            always_on_top: false,
            config_path,
            show_settings: false,
        }
    }

    fn apply_event(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::CueFired(info) => {
                self.state.current = info;
                self.state.last_rx = Some(Instant::now());
            }
            NetEvent::SubscribeOk(_) => {
                self.state.connected = true;
                self.status = "Subscribed".to_string();
            }
            NetEvent::SubscribeFail => {
                self.state.connected = false;
                self.status = "Subscription failed".to_string();
            }
            NetEvent::Thump => {
                self.state.last_rx = Some(Instant::now());
            }
        }
    }
}

impl App for TheatreMixApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        while let Ok(ev) = self.rx.try_recv() {
            self.apply_event(ev);
        }

        let connected = self.state.connected;
        let last_rx = self.state.last_rx;
        let host = self.host.clone();
        let status = self.status.clone();

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TheatreMix");
                ui.add_space(8.0);
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Host: {host}"));
                ui.separator();
                ui.label(format!(
                    "Status: {status}{}",
                    if connected { "" } else { " (waiting)" }
                ));
                ui.separator();
                if let Some(t) = last_rx {
                    let age = t.elapsed().as_secs_f32();
                    ui.label(format!("Last OSC: {:.1}s ago", age));
                } else {
                    ui.label("Last OSC: n/a");
                }
            });
            ui.add_space(6.0);

            ui.label("Current Cue");
            cue_block(ui, &self.state.current);
        });

        let mut settings_open = self.show_settings;
        let mut close_clicked = false;
        egui::Window::new("Settings")
            .open(&mut settings_open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("TheatreMix Host");
                    if self.host_edit.is_empty() {
                        self.host_edit = self.host.clone();
                    }
                    ui.text_edit_singleline(&mut self.host_edit);
                });

                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        let new_host = self.host_edit.trim().to_string();
                        if !new_host.is_empty() && new_host != self.host {
                            self.host = new_host.clone();
                            let _ = self.cmd_tx.send(NetCmd::SetHost(new_host));
                            self.status = "Reconnecting...".to_string();
                            self.state.connected = false;
                            if let Some(path) = &self.config_path {
                                let _ = save_host(path, &self.host);
                            }
                        }
                    }
                    if ui.button("Close").clicked() {
                        close_clicked = true;
                    }
                });

                ui.separator();

                let mut on_top = self.always_on_top;
                if ui.checkbox(&mut on_top, "Always on top").changed() {
                    self.always_on_top = on_top;
                    let level = if on_top {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    };
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                }
            });
        if close_clicked {
            settings_open = false;
        }
        self.show_settings = settings_open;

        // No auto-resize: keep the window size stable to avoid event-loop hangs.

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn cue_block(ui: &mut egui::Ui, cue: &CueInfo) {
    let title = if cue.number.is_empty() {
        "—".to_string()
    } else {
        cue.number.clone()
    };

    let text = if cue.text.is_empty() {
        "—".to_string()
    } else {
        cue.text.clone()
    };

    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("Cue {title}"))
                    .size(26.0)
                    .strong(),
            )
            .wrap(),
        );
        ui.add(egui::Label::new(egui::RichText::new(text).size(20.0)).wrap());
    });

    ui.label(format!("Color: {}", cue.color.as_deref().unwrap_or("—")));
}

fn spawn_osc_thread(host: String, tx: Sender<NetEvent>, cmd_rx: Receiver<NetCmd>) {
    thread::spawn(move || {
        let mut current_host = host;
        let mut socket = None;

        let mut last_subscribe = Instant::now() - Duration::from_secs(10);
        let mut subscription_expiry = 0u32;
        let mut last_thump = Instant::now() - Duration::from_secs(10);

        loop {
            match cmd_rx.try_recv() {
                Ok(NetCmd::SetHost(new_host)) => {
                    current_host = new_host;
                    socket = None;
                    subscription_expiry = 0;
                    last_subscribe = Instant::now() - Duration::from_secs(10);
                    last_thump = Instant::now() - Duration::from_secs(10);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break,
            }

            let subscribe_interval = if subscription_expiry > 0 {
                Duration::from_secs((subscription_expiry / 2).max(2) as u64)
            } else {
                Duration::from_secs(2)
            };

            if socket.is_none() {
                match bind_socket(&current_host) {
                    Ok(new_socket) => socket = Some(new_socket),
                    Err(err) => {
                        eprintln!(
                            "OSC socket setup failed for host '{}': {err}",
                            current_host
                        );
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }

            let Some(socket_ref) = socket.as_ref() else {
                continue;
            };
            let mut reconnect = false;

            if last_subscribe.elapsed() >= subscribe_interval {
                if !send_osc(socket_ref, "/subscribe", &[]) {
                    reconnect = true;
                }
                last_subscribe = Instant::now();
            }

            if last_thump.elapsed() >= Duration::from_secs(2) {
                // Keep session alive
                if !send_osc(socket_ref, "/thump", &[]) {
                    reconnect = true;
                }
                last_thump = Instant::now();
            }

            let mut buf = [0u8; 1536];
            match socket_ref.recv(&mut buf) {
                Ok(n) => {
                    if let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) {
                        match packet {
                            OscPacket::Message(msg) => {
                                if let Some(ev) = handle_message(msg, &mut subscription_expiry) {
                                    let _ = tx.send(ev);
                                }
                            }
                            OscPacket::Bundle(bundle) => {
                                for pkt in bundle.content {
                                    if let OscPacket::Message(msg) = pkt {
                                        if let Some(ev) =
                                            handle_message(msg, &mut subscription_expiry)
                                        {
                                            let _ = tx.send(ev);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    if err.kind() != io::ErrorKind::WouldBlock
                        && err.kind() != io::ErrorKind::TimedOut
                    {
                        reconnect = true;
                    }
                }
            }

            if reconnect {
                socket = None;
            }

            thread::sleep(Duration::from_millis(100));
        }
    });
}

fn bind_socket(host: &str) -> io::Result<UdpSocket> {
    let remote_addr = format!("{host}:32000")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "host resolved to no address"))?;
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    socket.connect(remote_addr)?;
    Ok(socket)
}

fn config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("theatremix-remote-display").join("host.txt"))
}

fn load_host(path: &PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_host(path: &PathBuf, host: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, host)
}

fn handle_message(msg: OscMessage, subscription_expiry: &mut u32) -> Option<NetEvent> {
    match msg.addr.as_str() {
        "/subscribeok" => {
            if let Some(OscType::Int(exp)) = msg.args.get(0) {
                *subscription_expiry = (*exp).max(2) as u32;
                return Some(NetEvent::SubscribeOk(*subscription_expiry));
            }
            None
        }
        "/subscribefail" => Some(NetEvent::SubscribeFail),
        "/thump" => Some(NetEvent::Thump),
        "/cuefired" => {
            let mut info = CueInfo::default();
            if let Some(OscType::String(num)) = msg.args.get(0) {
                info.number = num.clone();
            }
            if let Some(OscType::String(text)) = msg.args.get(1) {
                info.text = text.clone();
            }
            if let Some(OscType::String(color)) = msg.args.get(2) {
                info.color = Some(color.clone());
            }
            Some(NetEvent::CueFired(info))
        }
        _ => None,
    }
}

fn send_osc(socket: &UdpSocket, addr: &str, args: &[OscType]) -> bool {
    let msg = OscMessage {
        addr: addr.to_string(),
        args: args.to_vec(),
    };
    if let Ok(buf) = rosc::encoder::encode(&OscPacket::Message(msg)) {
        return socket.send(&buf).is_ok();
    }
    false
}

fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/Mac.png");
    let image = image::load_from_memory(bytes)
        .expect("load icon")
        .to_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result<()> {
    let arg_host = std::env::args().nth(1);
    let cfg_path = config_path();
    let stored_host = cfg_path.as_ref().and_then(load_host);
    let host = arg_host
        .clone()
        .or(stored_host)
        .unwrap_or_else(|| "127.0.0.1".to_string());
    if let (Some(path), Some(arg)) = (&cfg_path, arg_host) {
        let _ = save_host(path, &arg);
    }

    let (tx, rx) = mpsc::channel::<NetEvent>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<NetCmd>();
    spawn_osc_thread(host.clone(), tx, cmd_rx);

    let mut native_options = eframe::NativeOptions::default();
    native_options.viewport = ViewportBuilder::default()
        .with_inner_size([720.0, 200.0])
        .with_icon(load_icon());
    #[cfg(target_os = "windows")]
    {
        native_options.renderer = eframe::Renderer::Wgpu;
    }
    eframe::run_native(
        "TheatreMix Remote Display",
        native_options,
        Box::new(|_cc| Ok(Box::new(TheatreMixApp::new(host, rx, cmd_tx, cfg_path)))),
    )
}
