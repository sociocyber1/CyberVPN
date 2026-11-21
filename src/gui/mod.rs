//! The CyberVPN window (GNOME / libadwaita style). It is only a view: all
//! state lives in the tray service, which pushes snapshots 4× per second.
//! Closing the window keeps CyberVPN running in the tray.

mod theme;
mod widgets;

use crate::i18n::{tr, trf};
use crate::protocol::{Command, Snapshot, ToGui, VpnState};
use crate::service;
use crate::store::{Accent, Language, Paths, Settings, Theme, UiMode};
#[allow(unused_imports)]
use crate::store::Accent as _Accent;
use eframe::egui::{self, vec2, Align, Color32, CornerRadius, Layout, Margin, RichText, Stroke, Ui};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use theme::{semibold, Pal};
use widgets::*;

const SIMPLE_SIZE: [f32; 2] = [440.0, 780.0];
const PRO_SIZE: [f32; 2] = [1200.0, 800.0];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page { Home, Dashboard, Connections, Dns, Servers, Log, Settings }

#[derive(Default)]
struct Shared {
    snap: Option<Snapshot>,
    linked: bool,
    focus: bool,
    bye: bool,
}

pub struct App {
    /// streamer mode on, and the frame time (for the glyph animation)
    sm: bool,
    now: f64,
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Command>,
    page: Page,
    pal: Pal,
    applied: Option<(Theme, Accent)>,
    mode: Option<UiMode>,
    draft: Option<Settings>,
    dns_text: String,
    toast: Option<(String, bool, Instant)>,
    last_toast: u64,
    add_text: String,
    rename: Option<(usize, String)>,
    bw: Option<(usize, String, String)>,
    confirm_delete: Option<usize>,
    about: bool,
    filter: String,
    only_open: bool,
    add_domain: String,
    direct_dns_text: String,
}

fn sb(t: impl Into<String>) -> RichText {
    RichText::new(t).family(semibold())
}

// ---------------------------------------------------------------- IPC client

fn client(shared: Arc<Mutex<Shared>>, rx: Receiver<Command>, ctx: egui::Context) {
    let rx = Arc::new(Mutex::new(rx));
    std::thread::spawn(move || {
        let paths = Paths::get();
        let mut misses = 0;
        loop {
            let info = match service::running_instance(&paths) {
                Some(i) => i,
                None => {
                    shared.lock().unwrap().linked = false;
                    ctx.request_repaint();
                    misses += 1;
                    // give a service that is just starting a moment, then (re)start one
                    if (misses == 1 && !paths.ipc_file().exists()) || misses == 4 || misses % 25 == 0 {
                        let _ = crate::platform::start_service();
                    }
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
            };
            let Ok((stream, mut reader)) = crate::ipc::connect(info.port, &info.token) else {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            };
            misses = 0;
            shared.lock().unwrap().linked = true;
            // writer
            let mut w = stream.try_clone().expect("clone");
            let rx2 = rx.clone();
            let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let alive2 = alive.clone();
            std::thread::spawn(move || {
                while alive2.load(std::sync::atomic::Ordering::Relaxed) {
                    let cmd = rx2.lock().unwrap().recv_timeout(Duration::from_millis(200));
                    match cmd {
                        Ok(c) => {
                            if crate::ipc::send(&mut w, &c).is_err() {
                                break;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return,
                    }
                }
            });
            // reader
            loop {
                match crate::ipc::read_line::<ToGui>(&mut reader) {
                    Ok(Some(ToGui::Snapshot(s))) => {
                        shared.lock().unwrap().snap = Some(*s);
                        ctx.request_repaint();
                    }
                    Ok(Some(ToGui::Focus)) => {
                        shared.lock().unwrap().focus = true;
                        ctx.request_repaint();
                    }
                    Ok(Some(ToGui::Bye)) => {
                        shared.lock().unwrap().bye = true;
                        ctx.request_repaint();
                        alive.store(false, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    _ => break,
                }
            }
            alive.store(false, std::sync::atomic::Ordering::Relaxed);
            shared.lock().unwrap().linked = false;
            ctx.request_repaint();
            drop(stream);
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}


/// Inter (GNOME's Adwaita Sans is based on it) + DejaVu Sans for symbols,
/// all bundled: the UI looks the same on every system, Cyrillic included.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let add = |fonts: &mut egui::FontDefinitions, name: &str, data: &'static [u8]| {
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_static(data)));
    };
    add(&mut fonts, "inter", include_bytes!("../../assets/Inter-Regular.ttf"));
    add(&mut fonts, "inter-semibold", include_bytes!("../../assets/Inter-SemiBold.ttf"));
    add(&mut fonts, "dejavu", include_bytes!("../../assets/DejaVuSans.ttf"));
    let prop = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    prop.insert(0, "inter".into());
    prop.push("dejavu".into());
    fonts.families.insert(semibold(), vec!["inter-semibold".into(), "inter".into(), "dejavu".into()]);
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("dejavu".into());
    ctx.set_fonts(fonts);
}

fn gui_pid_file(paths: &Paths) -> std::path::PathBuf {
    paths.ipc_file().with_file_name("window.pid")
}

/// Is a CyberVPN window running right now?
pub fn window_open(paths: &Paths) -> bool {
    let Ok(pid) = std::fs::read_to_string(gui_pid_file(paths)) else { return false };
    let Ok(pid) = pid.trim().parse::<u32>() else { return false };
    #[cfg(unix)]
    {
        pid != std::process::id() && std::path::Path::new(&format!("/proc/{pid}")).exists()
            && std::fs::read_to_string(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.contains("cybervpn"))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

pub fn run() -> anyhow::Result<()> {
    let paths = Paths::get();
    let _ = std::fs::create_dir_all(paths.ipc_file().parent().unwrap());
    let _ = std::fs::write(gui_pid_file(&paths), std::process::id().to_string());
    let store = crate::store::Store::load(&paths).unwrap_or_default();
    crate::i18n::set(store.settings.language);
    let size = if store.settings.mode == UiMode::Pro { PRO_SIZE } else { SIMPLE_SIZE };
    let icon = crate::icon::render(256);
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CyberVPN")
            .with_app_id("cybervpn")
            .with_inner_size(size)
            .with_min_inner_size([400.0, 600.0])
            .with_icon(egui::IconData { rgba: icon, width: 256, height: 256 }),
        ..Default::default()
    };
    eframe::run_native(
        "CyberVPN",
        opts,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            load_textures(&cc.egui_ctx);
            let shared = Arc::new(Mutex::new(Shared::default()));
            let (tx, rx) = channel();
            client(shared.clone(), rx, cc.egui_ctx.clone());
            let pal = Pal::of(store.settings.theme, store.settings.accent);
            Ok(Box::new(App {
                sm: store.settings.streamer, now: 0.0,
                shared, tx,
                page: if store.settings.mode == UiMode::Pro { Page::Dashboard } else { Page::Home },
                pal, applied: None, mode: Some(store.settings.mode), draft: None, dns_text: String::new(),
                toast: None, last_toast: 0, add_text: String::new(), rename: None, bw: None,
                confirm_delete: None, about: false, filter: String::new(), only_open: false,
                add_domain: String::new(), direct_dns_text: String::new(),
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("cannot open the window: {e}  (graphics: needs OpenGL 2+ via X11 or Wayland)"))
}

// ---------------------------------------------------------------- app

impl App {
    fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    fn paste(&mut self) {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => self.send(Command::AddLinks(t)),
            Err(_) => self.toast = Some((tr("Clipboard is empty or unavailable — paste into the box instead").into(), true, Instant::now())),
        }
    }

    fn set_settings(&self, snap: &Snapshot, f: impl FnOnce(&mut Settings)) {
        let mut s = snap.store.settings.clone();
        f(&mut s);
        self.send(Command::SaveSettings(s));
    }

    fn anim(&self, snap: &Snapshot) -> bool {
        snap.store.settings.animations
    }
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        self.pal.bg.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (snap, linked, focus, bye) = {
            let mut s = self.shared.lock().unwrap();
            let f = std::mem::take(&mut s.focus);
            (s.snap.clone(), s.linked, f, s.bye)
        };
        if bye {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if focus {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let settings = snap.as_ref().map(|s| s.store.settings.clone()).unwrap_or_default();
        crate::i18n::set(settings.language);
        let key = (settings.theme, settings.accent);
        if self.applied != Some(key) {
            self.pal = Pal::of(settings.theme, settings.accent);
            theme::apply(&ctx, &self.pal);
            self.applied = Some(key);
        }
        if snap.is_some() && self.mode != Some(settings.mode) {
            self.mode = Some(settings.mode);
            let size = if settings.mode == UiMode::Pro { PRO_SIZE } else { SIMPLE_SIZE };
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
            self.page = if settings.mode == UiMode::Pro { Page::Dashboard } else { Page::Home };
        }
        if let Some(s) = &snap {
            if let Some((id, msg, err)) = &s.toast {
                if *id != self.last_toast {
                    self.last_toast = *id;
                    self.toast = Some((msg.clone(), *err, Instant::now()));
                }
            }
        }

        let p = self.pal;
        let Some(snap) = snap else {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    logo(ui, &p, 72.0, true);
                    ui.add_space(12.0);
                    ui.spinner();
                    ui.label(RichText::new(if linked { tr("Loading…") } else { tr("Starting CyberVPN…") }).color(p.dim));
                });
            });
            ctx.request_repaint_after(Duration::from_millis(200));
            return;
        };
        let pro = settings.mode == UiMode::Pro;
        self.sm = settings.streamer;
        self.now = ctx.input(|i| i.time);

        // footer (full width)
        egui::Panel::bottom("footer")
            .exact_size(30.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.panel).stroke(Stroke::new(1.0, p.border)).inner_margin(Margin::symmetric(12, 5)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let w = 20.0 + 6.0 + 260.0;
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    company_logo(ui, 20.0);
                    ui.label(sb("SocioCyberneering Inc.").size(12.0).color(p.text));
                    ui.label(RichText::new(format!("·  CyberVPN {}", env!("CARGO_PKG_VERSION"))).size(11.5).color(p.dim));
                });
            });
        if pro {
            egui::Panel::left("nav")
                .exact_size(214.0)
                .resizable(false)
                .frame(egui::Frame::new().fill(p.panel).inner_margin(Margin::same(14)).stroke(Stroke::new(1.0, p.border)))
                .show(ui, |ui| self.sidebar(ui, &snap));
        }
        // header bar
        egui::Panel::top("header")
            .exact_size(if pro { 56.0 } else { 54.0 })
            .resizable(false)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(if pro { 24 } else { 16 }, 8)))
            .show(ui, |ui| self.header_bar(ui, &snap, pro));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::symmetric(if pro { 24 } else { 16 }, 8)))
            .show(ui, |ui| {
                if !linked {
                    ui.colored_label(p.warn, tr("⚠ Lost connection to the CyberVPN tray app — restarting it…"));
                }
                match self.page {
                    Page::Home => self.home(ui, &snap),
                    Page::Dashboard => self.dashboard(ui, &snap),
                    Page::Connections => self.connections(ui, &snap),
                    Page::Dns => self.dns(ui, &snap),
                    Page::Servers => self.servers(ui, &snap),
                    Page::Log => self.log(ui, &snap),
                    Page::Settings => self.settings(ui, &snap),
                }
            });
        self.draw_toast(&ctx);
        self.draw_dialogs(&ctx, &snap);
        // Animate only when someone is looking: a hidden or unfocused window
        // costs almost no CPU (important with software OpenGL), leaving it
        // all to the VPN engine.
        let busy = snap.state.is_on() || settings.animations || settings.streamer;
        let (focused, minimized) = ctx.input(|i| (i.viewport().focused.unwrap_or(true), i.viewport().minimized.unwrap_or(false)));
        let ms = if minimized { 1000 } else if !focused { if busy { 250 } else { 1000 } } else if busy { 33 } else { 250 };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }
}

// ---------------------------------------------------------------- chrome

impl App {
    fn header_bar(&mut self, ui: &mut Ui, snap: &Snapshot, pro: bool) {
        let p = self.pal;
        ui.horizontal_centered(|ui| {
            if pro {
                let title = match self.page {
                    Page::Dashboard | Page::Home => tr("Dashboard"),
                    Page::Connections => tr("Connections"),
                    Page::Dns => tr("DNS"),
                    Page::Servers => tr("Servers"),
                    Page::Log => tr("Log"),
                    Page::Settings => tr("Settings"),
                };
                ui.label(sb(title).size(22.0));
            } else {
                logo(ui, &p, 30.0, snap.state == VpnState::Connected);
                ui.vertical(|ui| {
                    ui.add_space(1.0);
                    ui.label(sb("CyberVPN").size(18.0).color(p.text));
                    ui.label(RichText::new("Hysteria2 · TUN").size(10.5).color(p.dim));
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let streamer_btn = |ui: &mut Ui, app: &mut Self| {
                    let on = snap.store.settings.streamer;
                    let b = egui::Button::new(RichText::new(if on { "◉" } else { "◌" }).size(16.0).color(if on { p.on_sky } else { p.text }))
                        .fill(if on { p.sky_deep } else { p.card_hi })
                        .stroke(Stroke::new(1.0, p.border))
                        .corner_radius(CornerRadius::same(10));
                    if ui.add(b).on_hover_text(tr("Streamer mode: hide IPs, servers and sites")).clicked() {
                        app.set_settings(snap, |s| s.streamer = !s.streamer);
                    }
                };
                if pro {
                    streamer_btn(ui, self);
                    let mut on = snap.state.is_on();
                    if toggle(ui, &p, &mut on).changed() {
                        self.send(Command::Toggle);
                    }
                    let col = match snap.state { VpnState::Connected => p.sky, VpnState::Error(_) => p.bad, _ => p.dim };
                    ui.label(RichText::new(snap.state.label()).color(col));
                } else {
                    let settings_open = self.page == Page::Settings;
                    let gear = egui::Button::new(RichText::new(if settings_open { "✕" } else { "⚙" }).size(17.0).color(p.text))
                        .fill(p.card_hi)
                        .stroke(Stroke::new(1.0, p.border))
                        .corner_radius(CornerRadius::same(10));
                    if ui.add(gear).on_hover_text(tr("Settings")).clicked() {
                        self.page = if settings_open { Page::Home } else { Page::Settings };
                    }
                    streamer_btn(ui, self);
                    if let Some(i) = segmented(ui, &p, &[tr("Simple"), tr("Pro")], 0) {
                        if i == 1 {
                            self.set_settings(snap, |s| s.mode = UiMode::Pro);
                        }
                    }
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            logo(ui, &p, 30.0, snap.state == VpnState::Connected);
            ui.label(sb("CyberVPN").size(18.0));
        });
        ui.add_space(14.0);
        let t = snap.tick.as_ref();
        let open = t.map(|t| t.open_tcp + t.open_udp).unwrap_or(0);
        let items: [(Page, NavIcon, String); 6] = [
            (Page::Dashboard, NavIcon::Dashboard, tr("Dashboard").into()),
            (Page::Connections, NavIcon::Connections, format!("{}  {open}", tr("Connections"))),
            (Page::Dns, NavIcon::Dns, tr("DNS").into()),
            (Page::Servers, NavIcon::Servers, format!("{}  {}", tr("Servers"), snap.store.profiles.len())),
            (Page::Log, NavIcon::Log, tr("Log").into()),
            (Page::Settings, NavIcon::Settings, tr("Settings").into()),
        ];
        for (pg, ic, label) in items {
            if nav_item(ui, &p, ic, &label, self.page == pg).clicked() {
                self.page = pg;
            }
        }
        ui.add_space(14.0);
        card(&p).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let col = match snap.state { VpnState::Connected => p.sky, VpnState::Error(_) => p.bad, VpnState::Off => p.dim, _ => p.warn };
            ui.label(sb(snap.state.label()).color(col));
            let name = snap.store.profiles.get(snap.store.selected).map(|x| x.name.clone()).unwrap_or_else(|| tr("no server").into());
            ui.label(RichText::new(name).size(12.0).color(p.dim));
            let on = snap.state.is_on();
            let b = egui::Button::new(sb(if on { tr("Turn OFF") } else { tr("Turn ON") }).color(if on { p.text } else { p.on_sky }))
                .fill(if on { p.card_hi } else { p.sky })
                .stroke(Stroke::new(1.0, if on { p.border } else { p.sky }))
                .corner_radius(CornerRadius::same(10))
                .min_size(vec2(ui.available_width(), 34.0));
            if ui.add(b).clicked() {
                self.send(Command::Toggle);
            }
        });
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let sel = if snap.store.settings.mode == UiMode::Pro { 1 } else { 0 };
            if let Some(i) = segmented(ui, &p, &[tr("Simple"), tr("Pro")], sel) {
                self.set_settings(snap, |s| s.mode = if i == 1 { UiMode::Pro } else { UiMode::Simple });
            }
        });
    }

    fn engine_banner(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        let ins = &snap.install;
        if ins.running {
            card(&p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(sb(tr("Downloading the VPN engine…")));
                let frac = if ins.total > 0 { ins.have as f32 / ins.total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).fill(p.sky_deep).animate(true)
                    .text(format!("{} / {}   {}", human(ins.have as f64), human(ins.total as f64), rate(ins.speed as f64))));
                ui.label(RichText::new(tr("If the internet drops, it continues where it stopped.")).size(12.0).color(p.dim));
            });
            ui.add_space(10.0);
        } else if let Some(e) = &ins.error {
            card(&p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(p.bad, trf("Engine download failed: {}", &[e]));
                if primary_button(ui, &p, tr("Try again")).clicked() {
                    self.send(Command::InstallCore);
                }
            });
            ui.add_space(10.0);
        }
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, err, t)) = self.toast.clone() else { return };
        let age = t.elapsed().as_secs_f32();
        if age > 5.0 {
            self.toast = None;
            return;
        }
        let a = (age * 4.0).min(1.0).min((5.0 - age) * 2.0);
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -44.0 + (1.0 - a) * 20.0))
            .show(ctx, |ui| {
                // AdwToast: dark pill, white text
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x35, 0x35, 0x39))
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(Margin::symmetric(18, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if err {
                                ui.label(RichText::new("⚠").color(Color32::from_rgb(0xff, 0x7b, 0x63)));
                            }
                            ui.label(RichText::new(msg).color(Color32::WHITE));
                        });
                    });
            });
        ctx.request_repaint();
    }

    fn draw_dialogs(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let p = self.pal;
        if self.about {
            let r = egui::Modal::new(egui::Id::unique("about")).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    logo(ui, &p, 96.0, true);
                    ui.label(sb("CyberVPN").size(22.0));
                    ui.label(RichText::new(format!("{} {}", tr("Version"), env!("CARGO_PKG_VERSION"))).color(p.dim));
                    ui.add_space(6.0);
                    ui.label(tr("Hysteria2 VPN for the whole system"));
                    ui.label(RichText::new(trf("VPN engine: Hysteria {}", &[&snap.core_version.clone().unwrap_or_else(|| "—".into())])).size(12.5).color(p.dim));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                        company_logo(ui, 28.0);
                        ui.label(sb("SocioCyberneering Inc."));
                    });
                    if ui.link(RichText::new("sociocyber.site").color(self.pal.sky)).clicked() {
                        crate::platform::open_link("https://sociocyber.site");
                    }
                    ui.add_space(8.0);
                    if ghost_button(ui, &p, tr("Close")).clicked() {
                        self.about = false;
                    }
                });
            });
            if r.should_close() {
                self.about = false;
            }
        }
        if let Some(i) = self.confirm_delete {
            let name = snap.store.profiles.get(i).map(|x| x.name.clone()).unwrap_or_default();
            egui::Modal::new(egui::Id::unique("del")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.heading(tr("Delete server?"));
                ui.label(RichText::new(&name).color(p.dim));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(sb(tr("Delete")).color(Color32::WHITE)).fill(p.bad).corner_radius(CornerRadius::same(8))).clicked() {
                        self.send(Command::Delete(i));
                        self.confirm_delete = None;
                    }
                    if ghost_button(ui, &p, tr("Cancel")).clicked() {
                        self.confirm_delete = None;
                    }
                });
            });
        }
        if let Some((i, mut name)) = self.rename.take() {
            let mut keep = true;
            egui::Modal::new(egui::Id::unique("ren")).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading(tr("Rename"));
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                r.request_focus();
                ui.horizontal(|ui| {
                    if primary_button(ui, &p, tr("Save")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.send(Command::Rename(i, name.clone()));
                        keep = false;
                    }
                    if ghost_button(ui, &p, tr("Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                self.rename = Some((i, name));
            }
        }
        if let Some((i, mut up, mut down)) = self.bw.take() {
            let mut keep = true;
            egui::Modal::new(egui::Id::unique("bw")).show(ctx, |ui| {
                ui.set_width(390.0);
                ui.heading(tr("Your internet speed"));
                ui.label(RichText::new(tr("Enter your real line speed. CyberVPN then uses Hysteria's Brutal mode, which is usually much faster on lossy networks. Leave empty for automatic (BBR).")).color(p.dim).size(12.5));
                ui.add_space(6.0);
                egui::Grid::new("bwg").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("Download, Mbit/s"));
                    ui.add(egui::TextEdit::singleline(&mut down).desired_width(120.0).hint_text("200"));
                    ui.end_row();
                    ui.label(tr("Upload, Mbit/s"));
                    ui.add(egui::TextEdit::singleline(&mut up).desired_width(120.0).hint_text("50"));
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, &p, tr("Save")).clicked() {
                        let n = |s: &str| s.trim().parse::<u32>().ok().filter(|x| *x > 0);
                        self.send(Command::Bandwidth(i, n(&up), n(&down)));
                        keep = false;
                    }
                    if ghost_button(ui, &p, tr("Automatic")).clicked() {
                        self.send(Command::Bandwidth(i, None, None));
                        keep = false;
                    }
                    if ghost_button(ui, &p, tr("Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                self.bw = Some((i, up, down));
            }
        }
    }
}

// ---------------------------------------------------------------- simple mode

/// Glass "core" button in the middle of the planet.
fn power_core(ui: &mut Ui, p: &Pal, rect: egui::Rect, st: &VpnState) -> egui::Response {
    let id = ui.scope_id().with("core");
    let resp = ui.interact(rect, id, egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let ctx = ui.ctx().clone();
    let on = ctx.animate_bool_with_time(id.with("on"), *st == VpnState::Connected, 0.5);
    let hover = ctx.animate_bool_with_time(id.with("h"), resp.hovered(), 0.15);
    let press = ctx.animate_bool_with_time(id.with("p"), resp.is_pointer_button_down_on(), 0.08);
    let painter = ui.painter();
    let c = rect.center();
    let r = rect.width() / 2.0 * (1.0 + hover * 0.04 - press * 0.05);
    let err = matches!(st, VpnState::Error(_));
    let t = ui.input(|i| i.time) as f32;
    if on > 0.01 {
        for k in 0..3 {
            let ph = (t * 0.45 + k as f32 / 3.0) % 1.0;
            painter.circle_stroke(c, r + 6.0 + ph * 34.0, Stroke::new(2.0 * (1.0 - ph), with_alpha(p.sky, (1.0 - ph) * 0.55 * on)));
        }
    }
    for i in 0..10 {
        let k = i as f32 / 10.0;
        painter.circle_filled(c, r + 2.0 + k * 20.0, with_alpha(if err { p.bad } else { p.sky }, (0.05 * on + 0.015) * (1.0 - k)));
    }
    let off_fill = if p.dark { Color32::from_rgba_unmultiplied(14, 19, 27, 240) } else { Color32::from_rgba_unmultiplied(255, 255, 255, 245) };
    let fill = egui::lerp(egui::Rgba::from(off_fill)..=egui::Rgba::from(p.sky_deep), on);
    painter.circle_filled(c, r + 6.0, with_alpha(if err { p.bad } else { p.sky }, 0.12 + 0.18 * on));
    painter.circle_filled(c, r, Color32::from(fill));
    painter.circle_stroke(c, r, Stroke::new(2.0, if err { p.bad } else { with_alpha(p.sky, 0.5 + 0.5 * on.max(hover)) }));
    let glyph = if on > 0.5 { p.on_sky } else if err { p.bad } else { p.dim };
    let gr = r * 0.34;
    let gap = 0.75f32;
    let pts: Vec<egui::Pos2> = (0..=40).map(|i| {
        let a = -std::f32::consts::FRAC_PI_2 + gap + (std::f32::consts::TAU - 2.0 * gap) * i as f32 / 40.0;
        c + vec2(a.cos(), a.sin()) * gr + vec2(0.0, gr * 0.12)
    }).collect();
    painter.add(egui::epaint::PathShape::line(pts, Stroke::new(r * 0.085, glyph)));
    painter.line_segment([c + vec2(0.0, -gr * 1.1), c + vec2(0.0, -gr * 0.05)], Stroke::new(r * 0.085, glyph));
    resp
}

impl App {
    /// Planet + core button. Returns true if the button was clicked.
    fn hero(&mut self, ui: &mut Ui, snap: &Snapshot, h: f32, planet_r: f32) -> bool {
        let p = self.pal;
        let full = self.anim(snap);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), egui::Sense::hover());
        let t = ui.input(|i| i.time) as f32;
        let painter = ui.painter_at(rect.expand(2.0));
        let fx = fx_of(&snap.state);
        aurora(&painter, rect, rect.center(), planet_r, &p, fx, t, full);
        if full {
            let k = if p.dark { 1.0 } else { 0.55 };
            plexus(&painter, rect, if fx == Fx::Off { p.dim } else { p.sky }, t, 30, k * if fx == Fx::Off { 0.6 } else { 1.0 });
        }
        let you = tr("YOU");
        let server = snap
            .tick
            .as_ref()
            .and_then(|t| t.exit.as_ref())
            .map(|x| if x.cc.is_empty() { x.country.to_uppercase() } else { x.cc.clone() })
            .unwrap_or_else(|| tr("SERVER").into());
        planet(&painter, rect.center(), planet_r, &p, fx, t, full, &server, you);
        let core = egui::Rect::from_center_size(rect.center(), vec2(planet_r * 0.62, planet_r * 0.62));
        power_core(ui, &p, core, &snap.state).clicked()
    }

    fn home(&mut self, ui: &mut Ui, snap: &Snapshot) {
        if self.page == Page::Settings {
            return self.settings(ui, snap);
        }
        let w = ui.available_width();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // keep clear of the scroll bar, never wider than the window
            ui.set_max_width(w - 12.0);
            self.home_body(ui, snap)
        });
    }

    fn home_body(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        self.engine_banner(ui, snap);
        if snap.kill_switch_on && !snap.state.is_on() {
            egui::Frame::new().fill(with_alpha(p.warn, 0.14)).stroke(Stroke::new(1.0, with_alpha(p.warn, 0.5))).corner_radius(CornerRadius::same(12)).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(tr("Internet is blocked until the VPN is on — nothing leaks.")).color(p.text).size(13.0)).wrap());
            });
            ui.add_space(6.0);
        }
        let st = snap.state.clone();
        let tk = snap.tick.as_ref();
        if self.hero(ui, snap, 290.0, 92.0) {
            self.send(Command::Toggle);
        }
        let full = self.anim(snap);
        ui.vertical_centered(|ui| {
            let col = match st {
                VpnState::Connected => p.sky,
                VpnState::Error(_) => p.bad,
                VpnState::Off => p.text,
                _ => p.sky,
            };
            let title = decrypt_text(ui, "status", &st.label(), full);
            ui.label(sb(title).size(24.0).color(col));
            let sub = match (&st, tk.and_then(|t| t.exit.as_ref())) {
                (VpnState::Connected, Some(x)) => {
                    let place = if x.city.is_empty() { x.country.clone() } else { format!("{}, {}", x.city, x.country) };
                    trf("Your IP: {}  ·  {}", &[&mask(&x.ip, self.sm, self.now), &mask(&place, self.sm, self.now)])
                }
                (VpnState::Connected, None) => tr("Checking your new IP…").into(),
                (VpnState::Off, _) if snap.store.profiles.is_empty() => tr("Add a server to start").into(),
                (VpnState::Off, _) => tr("Tap the button to connect").into(),
                (VpnState::Authorizing, _) => tr("Confirm the password / admin prompt").into(),
                (VpnState::Error(_), _) => tr("Tap to try again").into(),
                _ => tr("Establishing a secure tunnel…").into(),
            };
            let sub = decrypt_text(ui, "sub", &sub, full && !self.sm);
            ui.label(RichText::new(sub).color(p.dim));
            if st == VpnState::Connected {
                ui.label(RichText::new(dur(snap.connected_s)).monospace().size(15.0).color(p.text));
            }
        });
        ui.add_space(10.0);

        if st == VpnState::Connected {
            // what protects you right now, at a glance
            let proxy_mode = tk.is_some_and(|t| t.proxy_mode);
            let mut chips: Vec<String> = vec![tr("Encrypted").into(), "Hysteria2 · QUIC".into()];
            if proxy_mode {
                chips.push(trf("Proxy {}", &[&tk.map(|t| t.proxy_addr.clone()).unwrap_or_default()]));
            } else if snap.store.settings.override_dns {
                chips.push(tr("Private DNS").into());
            }
            if let Some(ms) = tk.and_then(|t| t.latency_ms) {
                chips.push(format!("{ms} ms"));
            }
            // chips in centred rows that fit the window
            let cw = |c: &String| c.chars().count() as f32 * 6.6 + 28.0;
            let maxw = ui.available_width();
            let mut rows: Vec<Vec<&String>> = vec![vec![]];
            let mut used = 0.0;
            for c in &chips {
                if used + cw(c) > maxw && !rows.last().unwrap().is_empty() {
                    rows.push(vec![]);
                    used = 0.0;
                }
                used += cw(c);
                rows.last_mut().unwrap().push(c);
            }
            for row in rows {
                let w: f32 = row.iter().map(|c| cw(c)).sum();
                ui.horizontal(|ui| {
                    ui.add_space(((maxw - w) / 2.0).max(0.0));
                    for c in row {
                        pill(ui, c, p.sky, p.sky_soft);
                    }
                });
            }
            if proxy_mode {
                ui.add_space(4.0);
                let note = if tk.is_some_and(|t| t.system_proxy) {
                    tr("Proxy mode, no admin rights: browsers and apps that use the system proxy go through the VPN.")
                } else {
                    tr("Proxy mode, no admin rights: set this proxy (SOCKS5 or HTTP) in your browser.")
                };
                ui.vertical_centered(|ui| ui.add(egui::Label::new(RichText::new(note).size(12.0).color(p.dim)).wrap()));
            }
            ui.add_space(4.0);
        }
        ui.add_space(6.0);

        let (d, u) = tk.map(|t| (t.down_bps, t.up_bps)).unwrap_or((0.0, 0.0));
        boxed_list(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.columns(2, |cols| {
                for (i, ui) in cols.iter_mut().enumerate() {
                    egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                        let (cap, v, c) = if i == 0 { (tr("▼ Download"), d, p.sky) } else { (tr("▲ Upload"), u, p.up) };
                        stat(ui, &p, cap, cap, if st.is_on() { v } else { 0.0 }, rate, c);
                    });
                }
            });
            if st.is_on() && snap.down_hist.len() > 1 {
                // live speed graph, last minute
                egui::Frame::new().inner_margin(Margin { left: 10, right: 10, top: 0, bottom: 8 }).show(ui, |ui| {
                    let w = ui.available_width();
                    area_chart(ui, &p, vec2(w, 54.0), &[(&snap.down_hist, p.sky), (&snap.up_hist, p.up)], snap.down_hist.len().clamp(20, 120));
                });
            }
        });
        ui.add_space(10.0);

        boxed_list(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if snap.store.profiles.is_empty() {
                egui::Frame::new().inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.label(sb(tr("No servers yet")));
                    ui.label(RichText::new(tr("Get the free SocioCyberneering servers, or paste your own hy2:// link.")).color(p.dim).size(12.5));
                    ui.add_space(6.0);
                    if primary_button(ui, &p, tr("Get free servers")).clicked() {
                        self.send(Command::FetchFreeServers);
                    }
                    ui.add_space(4.0);
                    if primary_button(ui, &p, tr("Paste server link")).clicked() {
                        self.paste();
                    }
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.add_text).hint_text(tr("…or paste it here")).desired_width(ui.available_width() - 80.0));
                        if ghost_button(ui, &p, tr("Add")).clicked() && !self.add_text.trim().is_empty() {
                            let t = std::mem::take(&mut self.add_text);
                            self.send(Command::AddLinks(t));
                        }
                    });
                });
            } else {
                let sel = snap.store.selected.min(snap.store.profiles.len() - 1);
                let cur = snap.store.profiles[sel].clone();
                let host = crate::engine::uri::Hy2Uri::parse(&cur.uri).map(|u| format!("{}  ·  {} {}", mask(&u.host, self.sm, self.now), tr("port"), mask(&u.ports, self.sm, self.now))).unwrap_or_default();
                action_row(ui, &p, tr("Server"), &host, |ui| {
                    egui::ComboBox::from_id_salt("srv").selected_text(sb(mask(&short(&cur.name, 20), self.sm, self.now))).width((ui.available_width() * 0.55).clamp(110.0, 190.0)).show_ui(ui, |ui| {
                        for (i, pr) in snap.store.profiles.iter().enumerate() {
                            let name = mask(&pr.name, self.sm, self.now);
                            let label = if pr.busy { format!("{name}  · {}", tr("busy")) } else { name };
                            let text = if pr.busy { RichText::new(label).color(p.dim) } else { RichText::new(label) };
                            if ui.selectable_label(i == sel, text).clicked() {
                                self.send(Command::Select(i));
                            }
                        }
                    });
                });
                separator(ui, &p);
                action_row(ui, &p, tr("Add a server"), tr("from a hy2:// link in the clipboard"), |ui| {
                    if ghost_button(ui, &p, tr("Paste link")).clicked() {
                        self.paste();
                    }
                });
                if cur.down_mbps.is_none() {
                    separator(ui, &p);
                    action_row(ui, &p, tr("Faster browsing"), tr("Tell CyberVPN your internet speed"), |ui| {
                        if ghost_button(ui, &p, tr("Set speed")).clicked() {
                            self.bw = Some((sel, String::new(), String::new()));
                        }
                    });
                }
            }
        });
        ui.add_space(12.0);
        egui::CollapsingHeader::new(sb(tr("About SocioCyberneering Inc.")).color(p.text))
            .id_salt("about_company")
            .show(ui, |ui| about_company(ui, &p));
        ui.add_space(8.0);
    }

    fn page_title(&self, ui: &mut Ui, sub: &str) {
        if !sub.is_empty() {
            ui.label(RichText::new(sub).color(self.pal.dim));
            ui.add_space(8.0);
        }
    }

    fn dashboard(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        let t = snap.tick.clone().unwrap_or_default();
        let on = snap.state.is_on();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.engine_banner(ui, snap);
            // hero: planet + key numbers
            card(&p).inner_margin(Margin::same(0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let w = (ui.available_width() * 0.5).max(300.0);
                    ui.allocate_ui(vec2(w, 300.0), |ui| {
                        if self.hero(ui, snap, 300.0, 82.0) {
                            self.send(Command::Toggle);
                        }
                    });
                    ui.vertical(|ui| {
                        ui.add_space(22.0);
                        let full = self.anim(snap);
                        let col = match snap.state { VpnState::Connected => p.sky, VpnState::Error(_) => p.bad, _ => p.text };
                        ui.label(sb(decrypt_text(ui, "dstatus", &snap.state.label(), full)).size(26.0).color(col));
                        let sub = match &t.exit {
                            Some(x) if snap.state == VpnState::Connected => trf("Exit {} ({}) · {}", &[&mask(&x.ip, self.sm, self.now), &mask(&x.country, self.sm, self.now), &dur(snap.connected_s)]),
                            _ => snap.store.profiles.get(snap.store.selected).map(|x| x.name.clone()).unwrap_or_default(),
                        };
                        ui.label(RichText::new(sub).color(p.dim));
                        ui.add_space(14.0);
                        let z = |v: f64| if on { v } else { 0.0 };
                        egui::Grid::new("kpi").num_columns(2).min_col_width(150.0).spacing([30.0, 14.0]).show(ui, |ui| {
                            stat(ui, &p, "k0", tr("Download"), z(t.down_bps), rate, p.sky);
                            stat(ui, &p, "k1", tr("Upload"), z(t.up_bps), rate, p.up);
                            ui.end_row();
                            stat(ui, &p, "k2", tr("Data used"), (t.down_total + t.up_total) as f64, human, p.text);
                            stat(ui, &p, "k3", tr("Latency"), t.latency_ms.map(|x| x as f64).unwrap_or(0.0), |v| if v > 0.0 { format!("{v:.0} ms") } else { "—".into() }, p.text);
                            ui.end_row();
                            stat(ui, &p, "k4", tr("Connections"), (t.open_tcp + t.open_udp) as f64, |v| format!("{v:.0}"), p.text);
                            stat(ui, &p, "k5", tr("Packets/s"), z((t.rx_pps + t.tx_pps) as f64), |v| format!("{v:.0}"), p.text);
                            ui.end_row();
                        });
                    });
                });
            });
            ui.add_space(12.0);
            card(&p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(sb(tr("Traffic")));
                    ui.label(RichText::new(tr("last 2 minutes")).size(12.0).color(p.dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("▲ {}", mbit(if on { t.up_bps } else { 0.0 }))).color(p.up));
                        ui.label(RichText::new(format!("▼ {}", mbit(if on { t.down_bps } else { 0.0 }))).color(p.sky));
                    });
                });
                let w = ui.available_width();
                let max = area_chart(ui, &p, vec2(w, 170.0), &[(&snap.up_hist, p.up), (&snap.down_hist, p.sky)], 240);
                ui.label(RichText::new(trf("scale: {}", &[&rate(max)])).size(12.0).color(p.dim));
            });
            ui.add_space(12.0);
            ui.columns(3, |c| {
                card(&p).show(&mut c[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(sb(tr("Latency")));
                    let w = ui.available_width();
                    sparkline(ui, &p, vec2(w, 70.0), &snap.lat_hist, p.sky);
                    let (mn, mx) = snap.lat_hist.iter().fold((u64::MAX, 0), |(a, b), &x| (a.min(x), b.max(x)));
                    if mx > 0 {
                        ui.label(RichText::new(trf("min {} ms · max {} ms", &[&mn, &mx])).size(12.0).color(p.dim));
                    }
                });
                card(&p).show(&mut c[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(sb(tr("Top destinations")));
                    let max = t.top.first().map(|x| x.1).unwrap_or(1).max(1);
                    if t.top.is_empty() {
                        ui.label(RichText::new(tr("waiting for traffic…")).color(p.dim).size(12.0));
                    }
                    for (d, n) in t.top.iter().take(6) {
                        let w = ui.available_width();
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), egui::Sense::hover());
                        let frac = *n as f32 / max as f32;
                        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, vec2(rect.width() * frac, rect.height())), CornerRadius::same(6), p.sky_soft);
                        ui.painter().text(rect.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, d, egui::FontId::proportional(12.5), p.text);
                        ui.painter().text(rect.right_center() - vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, n.to_string(), egui::FontId::proportional(12.5), p.dim);
                    }
                });
                card(&p).show(&mut c[2], |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(sb(tr("Session")));
                    let kv = |ui: &mut Ui, k: &str, v: String| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(k).color(p.dim).size(12.5));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(v).size(12.5)));
                        });
                    };
                    let name = snap.active.and_then(|i| snap.store.profiles.get(i)).map(|x| x.name.clone()).unwrap_or("—".into());
                    kv(ui, tr("Server"), name);
                    kv(ui, tr("Server IP"), if t.server_ip.is_empty() { "—".into() } else { mask(&t.server_ip, self.sm, self.now) });
                    kv(ui, tr("DNS queries"), trf("{} · {} ms avg", &[&t.dns_queries, &t.dns_avg_ms]));
                    if snap.store.settings.not_to_vpn {
                        kv(ui, "notToVPN", trf("{} IPs direct", &[&t.direct_ips]));
                    }
                    kv(ui, tr("Engine"), format!("{} · cpu {:.1}%", snap.core_version.clone().unwrap_or("—".into()), t.cpu));
                    kv(ui, tr("Reconnects"), format!("{}", t.reconnects + t.restarts as u64));
                });
            });
        });
    }

    fn connections(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let (sm, now) = (self.sm, self.now);
        let p = self.pal;
        let t = snap.tick.clone().unwrap_or_default();
        self.page_title(ui, &trf("{} open · {} TCP · {} UDP total · {} errors", &[&(t.open_tcp + t.open_udp), &t.total_tcp, &t.total_udp, &t.conn_errors]));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter by app, domain or IP")).desired_width(320.0));
            ui.add_space(8.0);
            toggle(ui, &p, &mut self.only_open);
            ui.label(tr("only open"));
        });
        ui.add_space(8.0);
        let f = self.filter.to_lowercase();
        let rows: Vec<_> = t.conns.iter()
            .filter(|c| !self.only_open || c.open)
            .filter(|c| f.is_empty() || c.app.to_lowercase().contains(&f) || c.domain.to_lowercase().contains(&f) || c.dst.contains(&f))
            .collect();
        boxed_list(&p).inner_margin(Margin::same(10)).show(ui, |ui| {
            use egui_extras::{Column, TableBuilder};
            TableBuilder::new(ui)
                .striped(true)
                .column(Column::exact(70.0))
                .column(Column::exact(44.0))
                .column(Column::exact(130.0))
                .column(Column::exact(170.0))
                .column(Column::remainder().at_least(160.0))
                .column(Column::exact(70.0))
                .column(Column::exact(110.0))
                .header(26.0, |mut h| {
                    for x in [tr("Time"), "", tr("App"), tr("Address"), tr("Domain"), tr("Duration"), tr("State")] {
                        h.col(|ui| { ui.label(sb(x).size(12.5).color(p.dim)); });
                    }
                })
                .body(|body| {
                    body.rows(26.0, rows.len(), |mut row| {
                        let c = rows[row.index()];
                        row.col(|ui| { ui.label(RichText::new(&c.time).size(12.5).color(p.dim)); });
                        row.col(|ui| { ui.label(RichText::new(if c.tcp { "TCP" } else { "UDP" }).size(12.5).color(p.sky)); });
                        row.col(|ui| { ui.label(if c.app.is_empty() { "·".to_string() } else { mask(&c.app, sm, now) }); });
                        row.col(|ui| { ui.label(RichText::new(mask(&c.dst, sm, now)).monospace()); });
                        row.col(|ui| { ui.label(sb(mask(&c.domain, sm, now))); });
                        row.col(|ui| { ui.label(RichText::new(dur(c.dur_s)).size(12.5).color(p.dim)); });
                        row.col(|ui| {
                            if c.open {
                                pill(ui, tr("open"), p.good, with_alpha(p.good, 0.15));
                            } else if !c.error.is_empty() {
                                pill(ui, tr("error"), p.bad, with_alpha(p.bad, 0.15));
                            } else {
                                ui.label(RichText::new(tr("closed")).color(p.dim));
                            }
                        });
                    });
                });
            if rows.is_empty() {
                ui.label(RichText::new(if snap.state.is_on() { tr("No connections yet") } else { tr("Turn the VPN on to see live connections") }).color(p.dim));
            }
        });
    }

    fn dns(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let (sm, now) = (self.sm, self.now);
        let p = self.pal;
        let t = snap.tick.clone().unwrap_or_default();
        self.page_title(ui, &trf("{} queries · {} ms average · {} failed · resolved through the tunnel", &[&t.dns_queries, &t.dns_avg_ms, &t.dns_fail]));
        boxed_list(&p).inner_margin(Margin::same(10)).show(ui, |ui| {
            use egui_extras::{Column, TableBuilder};
            TableBuilder::new(ui)
                .striped(true)
                .column(Column::exact(70.0))
                .column(Column::exact(64.0))
                .column(Column::exact(60.0))
                .column(Column::remainder().at_least(200.0))
                .column(Column::exact(220.0))
                .column(Column::exact(96.0))
                .header(26.0, |mut h| {
                    for x in [tr("Time"), tr("Speed"), tr("Type"), tr("Domain"), tr("Answer"), tr("Status")] {
                        h.col(|ui| { ui.label(sb(x).size(12.5).color(p.dim)); });
                    }
                })
                .body(|body| {
                    body.rows(26.0, t.dns.len(), |mut row| {
                        let e = &t.dns[row.index()];
                        row.col(|ui| { ui.label(RichText::new(&e.time).size(12.5).color(p.dim)); });
                        let mc = if e.ms < 80 { p.good } else if e.ms < 300 { p.warn } else { p.bad };
                        row.col(|ui| { ui.colored_label(mc, format!("{} ms", e.ms)); });
                        row.col(|ui| {
                            if e.direct {
                                pill(ui, tr("direct"), p.good, with_alpha(p.good, 0.15));
                            } else {
                                ui.label(RichText::new(&e.qtype).size(12.5).color(p.sky));
                            }
                        });
                        row.col(|ui| { ui.label(sb(mask(&e.name, sm, now))); });
                        let ans = if !e.ips.is_empty() {
                            e.ips.iter().take(2).map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
                        } else {
                            e.cname.clone().map(|c| format!("→ {c}")).unwrap_or("—".into())
                        };
                        row.col(|ui| { ui.label(RichText::new(mask(&ans, sm, now)).monospace().size(12.0)); });
                        let sc = if e.status == "NOERROR" { p.good } else if e.status == "NXDOMAIN" { p.warn } else { p.bad };
                        row.col(|ui| { ui.colored_label(sc, &e.status); });
                    });
                });
            if t.dns.is_empty() {
                ui.label(RichText::new(tr("No DNS queries yet")).color(p.dim));
            }
        });
    }

    fn servers(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        self.page_title(ui, tr("Hysteria2 servers. Paste hy2:// links to add more."));
        ui.horizontal(|ui| {
            if primary_button(ui, &p, tr("Paste from clipboard")).clicked() {
                self.paste();
            }
            ui.add(egui::TextEdit::singleline(&mut self.add_text).hint_text("hy2://… / hysteria2://…").desired_width(420.0));
            if ghost_button(ui, &p, tr("Add")).clicked() && !self.add_text.trim().is_empty() {
                let t = std::mem::take(&mut self.add_text);
                self.send(Command::AddLinks(t));
            }
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            boxed_list(&p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let n = snap.store.profiles.len();
                for (i, pr) in snap.store.profiles.iter().enumerate() {
                    let active = snap.active == Some(i) && snap.state.is_on();
                    let info = crate::engine::uri::Hy2Uri::parse(&pr.uri)
                        .map(|u| format!("{}:{}  ·  obfs {}  ·  {}", mask(&u.host, self.sm, self.now), mask(&u.ports, self.sm, self.now), u.obfs.as_deref().unwrap_or("none"),
                            match (pr.up_mbps, pr.down_mbps) { (Some(a), Some(b)) => format!("Brutal ↑{a} ↓{b} Mbit/s"), _ => tr("auto speed (BBR)").into() }))
                        .unwrap_or_else(|e| trf("invalid link: {}", &[&e]));
                    egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            logo(ui, &p, 32.0, active);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(sb(mask(&pr.name, self.sm, self.now)));
                                    if i == snap.store.selected {
                                        pill(ui, tr("selected"), p.sky, p.sky_soft);
                                    }
                                });
                                ui.label(RichText::new(info).size(12.5).color(p.dim));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if destructive_button(ui, &p, "✕").on_hover_text(tr("Delete")).clicked() {
                                    self.confirm_delete = Some(i);
                                }
                                if ghost_button(ui, &p, "✏").on_hover_text(tr("Rename")).clicked() {
                                    self.rename = Some((i, pr.name.clone()));
                                }
                                if ghost_button(ui, &p, tr("Speed")).clicked() {
                                    self.bw = Some((i, pr.up_mbps.map(|x| x.to_string()).unwrap_or_default(), pr.down_mbps.map(|x| x.to_string()).unwrap_or_default()));
                                }
                                if active {
                                    pill(ui, tr("connected"), p.good, with_alpha(p.good, 0.15));
                                } else if primary_button(ui, &p, tr("Connect")).clicked() {
                                    self.send(Command::Connect(Some(i)));
                                }
                            });
                        });
                    });
                    if i + 1 < n {
                        separator(ui, &p);
                    }
                }
                if n == 0 {
                    egui::Frame::new().inner_margin(Margin::same(18)).show(ui, |ui| {
                        ui.label(RichText::new(tr("No servers yet. Copy a hy2:// link and press “Paste from clipboard”.")).color(p.dim));
                    });
                }
            });
        });
    }

    fn log(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        boxed_list(&p).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in &snap.logs {
                    use crate::engine::core::Level;
                    let (tag, c) = match l.level {
                        Level::Error => ("ERR", p.bad),
                        Level::Warn => ("WRN", p.warn),
                        Level::Info => ("INF", p.sky),
                        Level::Debug => ("DBG", p.dim),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&l.time).monospace().color(p.dim));
                        ui.label(RichText::new(tag).monospace().color(c));
                        ui.label(RichText::new(mask(&l.msg, self.sm, self.now)).monospace());
                        if !l.fields.is_empty() {
                            ui.label(RichText::new(mask(&l.fields, self.sm, self.now)).monospace().size(11.5).color(p.dim));
                        }
                    });
                }
            });
        });
    }

    fn group_title(&self, ui: &mut Ui, t: &str) {
        ui.add_space(14.0);
        ui.label(sb(t).size(14.0));
        ui.add_space(4.0);
    }

    fn settings(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let p = self.pal;
        let simple = snap.store.settings.mode == UiMode::Simple;
        if self.draft.as_ref().is_none() || self.draft.as_ref() == Some(&snap.store.settings) {
            self.draft = Some(snap.store.settings.clone());
            self.dns_text = snap.store.settings.dns.join(", ");
            self.direct_dns_text = snap.store.settings.direct_dns.join(", ");
        }
        let mut d = self.draft.clone().unwrap();
        let before = d.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let maxw = 720.0f32.min(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.set_max_width(maxw);
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    // ---- appearance
                    self.group_title(ui, tr("Appearance"));
                    boxed_list(&p).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        action_row(ui, &p, tr("Style"), "", |ui| {
                            let sel = match d.theme { Theme::System => 0, Theme::Light => 1, Theme::Dark => 2 };
                            if let Some(i) = segmented(ui, &p, &[tr("System"), tr("Light"), tr("Dark")], sel) {
                                d.theme = [Theme::System, Theme::Light, Theme::Dark][i];
                            }
                        });
                        separator(ui, &p);
                        action_row(ui, &p, tr("Language"), "", |ui| {
                            let sel = match d.language { Language::Auto => 0, Language::En => 1, Language::Ru => 2 };
                            if let Some(i) = segmented(ui, &p, &[tr("Auto"), "English", "Русский"], sel) {
                                d.language = [Language::Auto, Language::En, Language::Ru][i];
                            }
                        });
                        separator(ui, &p);
                        action_row(ui, &p, tr("Mode"), tr("Pro shows the dashboard, connections and DNS"), |ui| {
                            let sel = if d.mode == UiMode::Pro { 1 } else { 0 };
                            if let Some(i) = segmented(ui, &p, &[tr("Simple"), tr("Pro")], sel) {
                                d.mode = if i == 1 { UiMode::Pro } else { UiMode::Simple };
                            }
                        });
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Animations"), tr("Planet, network and decrypt effects"), &mut d.animations);
                    });

                    // ---- general
                    self.group_title(ui, tr("General"));
                    boxed_list(&p).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        setting_row(ui, &p, tr("Start with the computer"), if cfg!(windows) { tr("Windows login") } else { tr("Desktop login (XDG autostart)") }, &mut d.autostart);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Start hidden in the tray"), tr("Only the tray icon when started automatically"), &mut d.start_minimized);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Connect automatically on start"), "", &mut d.connect_on_start);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Notifications"), tr("Connected, disconnected, errors"), &mut d.notifications);
                        separator(ui, &p);
                        let sub = if d.reminder_minutes == 0 { tr("never").to_string() } else { trf("every {} min", &[&d.reminder_minutes]) };
                        action_row(ui, &p, tr("Remind me when the VPN is off"), &sub, |ui| {
                            ui.add(egui::Slider::new(&mut d.reminder_minutes, 0..=240).step_by(5.0).show_value(false).trailing_fill(true));
                        });
                    });

                    // ---- connection
                    self.group_title(ui, tr("Connection"));
                    boxed_list(&p).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let how = if snap.will_use_tun {
                            tr("Now: whole system (all apps, DNS)")
                        } else {
                            tr("Now: proxy only — no admin password needed")
                        };
                        action_row(ui, &p, tr("VPN mode"), how, |ui| {
                            use crate::store::VpnMode;
                            let cur = match d.vpn_mode { VpnMode::Auto => 0, VpnMode::System => 1, VpnMode::Proxy => 2 };
                            if let Some(i) = segmented(ui, &p, &[tr("Auto"), tr("Whole system"), tr("Proxy")], cur) {
                                d.vpn_mode = [VpnMode::Auto, VpnMode::System, VpnMode::Proxy][i];
                            }
                        });
                        if !snap.will_use_tun {
                            separator(ui, &p);
                            setting_row(ui, &p, tr("Set the system proxy"), tr("Browsers and apps use the VPN automatically; put back when you turn it off"), &mut d.system_proxy);
                        }
                        if !simple {
                            separator(ui, &p);
                            action_row(ui, &p, tr("Proxy address"), tr("SOCKS5 + HTTP on one port, for browsers"), |ui| {
                                ui.add(egui::TextEdit::singleline(&mut d.proxy).desired_width(140.0));
                            });
                        }
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Private DNS through the tunnel"), tr("Stops DNS leaks and DNS blocking by the provider"), &mut d.override_dns);
                        if !simple {
                            separator(ui, &p);
                            action_row(ui, &p, tr("DNS servers"), "", |ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.dns_text).desired_width(220.0));
                            });
                        }
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Local network outside the VPN"), tr("Printers, NAS, router stay reachable"), &mut d.bypass_lan);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Reconnect automatically"), "", &mut d.auto_reconnect);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Block the internet without VPN (kill switch)"), tr("While CyberVPN runs, nothing goes out until you turn it on: your real IP never leaks"), &mut d.kill_switch);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Streamer mode"), tr("IPs, servers, sites and logs turn into shifting glyphs"), &mut d.streamer);
                        if !simple {
                            separator(ui, &p);
                            setting_row(ui, &p, tr("Fast open"), tr("One round-trip less per connection"), &mut d.fast_open);
                            separator(ui, &p);
                            setting_row(ui, &p, tr("Kernel tuning (Linux)"), tr("Larger UDP buffers for QUIC speed"), &mut d.tune_sysctl);
                            separator(ui, &p);
                            let v6_now = match snap.tick.as_ref().and_then(|t| t.ipv6) {
                                Some(true) if snap.state.is_on() => tr("now: on"),
                                Some(false) if snap.state.is_on() => tr("now: hidden — the server has no IPv6"),
                                _ => tr("Auto hides IPv6 when the server has none, so sites don't stall"),
                            };
                            action_row(ui, &p, "IPv6", v6_now, |ui| {
                                let cur = match d.ipv6.as_str() { "on" => 1, "off" => 2, _ => 0 };
                                if let Some(i) = segmented(ui, &p, &[tr("Auto"), tr("On"), tr("Off")], cur) {
                                    d.ipv6 = ["auto", "on", "off"][i].into();
                                }
                            });
                            separator(ui, &p);
                            action_row(ui, &p, tr("TUN interface"), tr("Changes apply on the next connect."), |ui| {
                                ui.add(egui::DragValue::new(&mut d.mtu).range(1280..=9000).prefix("MTU "));
                                ui.add(egui::TextEdit::singleline(&mut d.tun_name).desired_width(100.0));
                            });
                        }
                    });

                    // ---- notToVPN (Pro only)
                    if !simple {
                        self.group_title(ui, tr("Split tunneling — notToVPN"));
                        boxed_list(&p).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            setting_row(ui, &p, tr("Don't tunnel these domains"), tr("Sites in these zones and domains connect directly; everything else stays in the VPN"), &mut d.not_to_vpn);
                            separator(ui, &p);
                            egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
                                ui.label(RichText::new(tr("Zones")).color(p.dim).size(12.5));
                                ui.horizontal_wrapped(|ui| {
                                    for (label, zone) in crate::engine::split::PRESETS {
                                        let on = d.not_to_vpn_domains.iter().any(|x| x == zone);
                                        let txt = sb(*label).size(13.0).color(if on { p.on_sky } else { p.text });
                                        let b = egui::Button::new(txt).fill(if on { p.sky_deep } else { p.card_hi }).corner_radius(CornerRadius::same(14));
                                        if ui.add(b).clicked() {
                                            if on {
                                                d.not_to_vpn_domains.retain(|x| x != zone);
                                            } else {
                                                d.not_to_vpn_domains.push(zone.to_string());
                                            }
                                        }
                                    }
                                });
                                ui.add_space(8.0);
                                ui.label(RichText::new(tr("Domains")).color(p.dim).size(12.5));
                                let presets: Vec<&str> = crate::engine::split::PRESETS.iter().map(|x| x.1).collect();
                                let mut remove = None;
                                ui.horizontal_wrapped(|ui| {
                                    for (i, dom) in d.not_to_vpn_domains.iter().enumerate() {
                                        if presets.contains(&dom.as_str()) {
                                            continue;
                                        }
                                        egui::Frame::new().fill(p.card_hi).corner_radius(CornerRadius::same(14)).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new(dom).size(13.0));
                                                if ui.add(egui::Button::new(RichText::new("✕").size(11.0).color(p.dim)).frame(false)).clicked() {
                                                    remove = Some(i);
                                                }
                                            });
                                        });
                                    }
                                });
                                if let Some(i) = remove {
                                    d.not_to_vpn_domains.remove(i);
                                }
                                ui.horizontal(|ui| {
                                    let r = ui.add(egui::TextEdit::singleline(&mut self.add_domain).hint_text(tr("gosuslugi.ru, vk.com, yandex.com…")).desired_width(300.0));
                                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if (ghost_button(ui, &p, tr("Add")).clicked() || enter) && !self.add_domain.trim().is_empty() {
                                        for part in self.add_domain.split([',', ' ', ';']) {
                                            if let Some(n) = crate::engine::split::normalize(part) {
                                                let shown = idna::domain_to_unicode(&n).0;
                                                if !d.not_to_vpn_domains.contains(&shown) {
                                                    d.not_to_vpn_domains.push(shown);
                                                }
                                            }
                                        }
                                        self.add_domain.clear();
                                    }
                                });
                            });
                            separator(ui, &p);
                            action_row(ui, &p, tr("Direct DNS"), tr("Resolves the listed domains outside the tunnel"), |ui| {
                                if ui.add(egui::TextEdit::singleline(&mut self.direct_dns_text).desired_width(200.0)).changed() {
                                    let v: Vec<String> = self.direct_dns_text.split([',', ' ']).map(str::trim).filter(|x| x.parse::<std::net::IpAddr>().is_ok()).map(String::from).collect();
                                    if !v.is_empty() {
                                        d.direct_dns = v;
                                    }
                                }
                            });
                            egui::Frame::new().inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                                ui.label(RichText::new(tr("Works in TUN mode through CyberVPN's private DNS. Only these sites leave the tunnel; all other traffic and DNS stay protected. Sites sharing an IP with a listed one (big CDNs) also go direct. Changes apply on the next connect.")).size(12.0).color(p.dim));
                            });
                        });
                    }

                    // ---- engine
                    self.group_title(ui, tr("VPN engine"));
                    let e = snap.engine.clone();
                    boxed_list(&p).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let v = snap.core_version.clone().unwrap_or_else(|| tr("not installed").into());
                        let built_in = e.embedded.is_some() && e.embedded.as_deref() == snap.core_version.as_deref();
                        let status = if let Some(st) = &e.staged {
                            trf("{} downloaded — used the next time you connect", &[st])
                        } else if e.status.is_empty() {
                            if d.auto_update_engine { tr("checks for updates in the background").into() } else { tr("automatic updates are off").into() }
                        } else {
                            e.status.clone()
                        };
                        let title = format!("Hysteria {v}{}", if built_in { format!("  ({})", tr("built in")) } else { String::new() });
                        action_row(ui, &p, &title, &status, |ui| {
                            if snap.core_version.is_none() {
                                if ui.add_enabled(!snap.install.running, egui::Button::new(tr("Download"))).clicked() {
                                    self.send(Command::InstallCore);
                                }
                            } else if e.checking {
                                ui.spinner();
                            } else if ghost_button(ui, &p, tr("Check now")).clicked() {
                                self.send(Command::CheckEngineUpdate);
                            }
                        });
                        if e.download.running && e.download.total > 0 {
                            egui::Frame::new().inner_margin(Margin::symmetric(14, 6)).show(ui, |ui| {
                                let f = e.download.have as f32 / e.download.total as f32;
                                ui.add(egui::ProgressBar::new(f).fill(p.sky_deep).animate(true).text(trf(
                                    "downloading update {} / {} — the VPN keeps working",
                                    &[&human(e.download.have as f64), &human(e.download.total as f64)],
                                )));
                            });
                        }
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Update the engine automatically"), tr("Downloads in the background without interrupting the VPN; switches on the next connect"), &mut d.auto_update_engine);
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Tell me when the engine is updated"), "", &mut d.notify_engine_update);
                        if !simple {
                            separator(ui, &p);
                            action_row(ui, &p, tr("Custom core path"), "", |ui| {
                                ui.add(egui::TextEdit::singleline(&mut d.core_path).hint_text(tr("auto")).desired_width(260.0));
                            });
                        }
                        if snap.can_skip_password {
                            separator(ui, &p);
                            action_row(ui, &p, tr("Don't ask for the password"), tr("Asks for the password once; after that the whole-system VPN starts without asking (Auto mode uses it)."), |ui| {
                                if ghost_button(ui, &p, tr("Allow")).clicked() {
                                    self.send(Command::AllowWithoutPassword);
                                }
                            });
                        }
                    });
                    self.group_title(ui, tr("Servers"));
                    boxed_list(&p).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let n = snap.store.profiles.iter().filter(|x| x.free).count();
                        let b = snap.store.profiles.iter().filter(|x| x.free && x.busy).count();
                        let sub = trf("{} keys from github.com/sociocyber1/txt, {} busy. Dead keys are removed, new ones added.", &[&n, &b]);
                        action_row(ui, &p, tr("Free SocioCyberneering servers"), &sub, |ui| {
                            if ghost_button(ui, &p, tr("Update")).clicked() {
                                self.send(Command::FetchFreeServers);
                            }
                        });
                        separator(ui, &p);
                        setting_row(ui, &p, tr("Update them at every start"), "", &mut d.update_free_servers);
                    });
                    self.group_title(ui, tr("About SocioCyberneering Inc."));
                    about_company(ui, &p);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.link(tr("About CyberVPN")).clicked() {
                            self.about = true;
                        }
                        ui.label(RichText::new(tr("Closing the window keeps CyberVPN in the tray.")).size(12.5).color(p.dim));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if destructive_button(ui, &p, tr("Quit CyberVPN")).clicked() {
                                self.send(Command::QuitApp);
                            }
                        });
                    });
                    ui.add_space(10.0);
                });
            });
        });
        d.dns = self.dns_text.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        if d.dns.is_empty() {
            d.dns = before.dns.clone();
        }
        if d != before {
            self.send(Command::SaveSettings(d.clone()));
        }
        self.draft = Some(d);
    }
}


/// "very long server name" → "very long serv…"
fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

const SBER_CARD: &str = "2202 2081 1034 0461";
const VIDEO: &str = "https://youtu.be/258cx_PsrRA?t=1360";
/// the author's contact (edit assets/contact.txt)
const CONTACT: &str = include_str!("../../assets/contact.txt");

/// About the company, contacts and how to support the project.
fn about_company(ui: &mut Ui, p: &Pal) {
    let contact = CONTACT.trim();
    boxed_list(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.horizontal(|ui| {
                company_logo(ui, 28.0);
                ui.vertical(|ui| {
                    ui.label(sb("SocioCyberneering Inc.").color(p.text));
                    if ui.link(RichText::new(format!("▶ {}", tr("Video about the project"))).color(p.sky)).clicked() {
                        crate::platform::open_link(VIDEO);
                    }
                });
            });
        });
        separator(ui, p);
        action_row(ui, p, tr("Contacts"), contact, |ui| {
            if ghost_button(ui, p, tr("Write")).clicked() {
                let url = if contact.contains('@') { format!("mailto:{contact}") } else { format!("https://{contact}") };
                crate::platform::open_link(&url);
            }
        });
        separator(ui, p);
        egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.label(sb(tr("Support the author")).color(p.text));
            ui.add(egui::Label::new(RichText::new(tr("Keep the project and the servers running.")).color(p.sky).size(12.5)).wrap());
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(tr("If you find this work useful, you can help with money. In short, without drama: the author does everything himself, out of his own pocket and for free, while subscriptions, tools, servers and the time spent on them cost real money.")).color(p.dim).size(12.5)).wrap());
        });
        action_row(ui, p, tr("Sber card"), SBER_CARD, |ui| {
            let id = egui::Id::unique("card_copied");
            let copied: f64 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(-10.0);
            let now = ui.input(|i| i.time);
            let label = if now - copied < 2.0 { tr("Copied ✓") } else { tr("Copy") };
            if ghost_button(ui, p, label).clicked() && crate::platform::copy_text(&SBER_CARD.replace(' ', "")) {
                ui.ctx().data_mut(|d| d.insert_temp(id, now));
            }
        });
    });
}
