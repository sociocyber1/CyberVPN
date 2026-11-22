//! Tray service: the long-running user process. Owns the config, the tray,
//! notifications/reminders, the elevated backend, and serves snapshots to
//! any number of open windows.

use crate::engine::core::{self, app_log, Level, LogLine};
use crate::engine::download::Progress;
use crate::ipc::{self, IpcInfo};
use crate::platform;
use crate::i18n::{tr, trf};
use crate::protocol::*;
use crate::store::{Paths, Store};
use crate::tray::{self, TrayCmd, TrayEvent};
use anyhow::Result;
use std::collections::VecDeque;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::process::Child;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HIST: usize = 240; // 2 min at 2 samples/s

enum BEvent { Accepted(TcpStream, BufReader<TcpStream>), AcceptFailed(String), Msg(BackendMsg), Closed }

struct Backend {
    demo: bool,
    tx: Sender<BEvent>,
    child: Option<Child>,
    stream: Option<TcpStream>,
    rx: Receiver<BEvent>,
    started: Instant,
    /// proxy mode: the session runs in this process; set to stop it
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
}

struct Svc {
    paths: Paths,
    store: Store,
    state: VpnState,
    active: Option<usize>,
    backend: Option<Backend>,
    want_on: bool,
    connected_at: Option<Instant>,
    tick: Option<Tick>,
    down: VecDeque<f64>,
    up: VecDeque<f64>,
    lat: VecDeque<u64>,
    logs: VecDeque<LogLine>,
    core_version: Option<String>,
    install: InstallState,
    install_progress: Option<Arc<Progress>>,
    install_rx: Option<Receiver<Result<std::path::PathBuf, String>>>,
    connect_after_install: bool,
    toast: Option<(u64, String, bool)>,
    toast_id: u64,
    /// open windows: (id, queue to its writer thread)
    clients: Arc<Mutex<Vec<(u64, std::sync::mpsc::SyncSender<Arc<String>>)>>>,
    tray_tx: Sender<TrayCmd>,
    last_remind: Instant,
    notified_exit: bool,
    engine: EngineInfo,
    update_trigger: Sender<()>,
    update_rx: Receiver<UpdateMsg>,
    update_progress: Arc<Progress>,
    /// free server list being downloaded: (result channel, started by the user?)
    free_rx: Option<(Receiver<Result<String, String>>, bool)>,
    /// free list could not be downloaded (internet blocked): retry once connected
    free_retry: bool,
    /// connect as soon as the free servers arrive (first start with autostart)
    connect_after_free: bool,
    /// kill switch in place, and a running change: (result, turning on?)
    ks_on: bool,
    ks_rx: Option<(Receiver<Result<(), String>>, bool)>,
    /// server hosts the kill switch lets through
    ks_hosts: Vec<String>,
}

enum UpdateMsg {
    Checking,
    Done(Result<Option<String>, String>),
}

impl Svc {
    fn log(&mut self, l: Level, m: impl Into<String>) {
        self.logs.push_back(app_log(l, m.into()));
        while self.logs.len() > 300 {
            self.logs.pop_front();
        }
    }
    fn toast(&mut self, m: impl Into<String>, err: bool) {
        self.toast_id += 1;
        self.toast = Some((self.toast_id, m.into(), err));
    }
    fn save(&mut self) {
        if let Err(e) = self.store.save(&self.paths) {
            self.toast(format!("Cannot save settings: {e}"), true);
        }
    }
    /// Apply a staged update / install the embedded engine. Only when no VPN runs.
    fn prepare_engine(&mut self) {
        use crate::engine::updater::{self, Prepared};
        if self.backend.is_some() {
            return;
        }
        match updater::prepare(&self.paths.bin_dir()) {
            Ok(Prepared::Updated { from, to }) => {
                self.log(Level::Info, format!("VPN engine updated {from} → {to}"));
                if self.store.settings.notifications && self.store.settings.notify_engine_update {
                    platform::notify(&format!("CyberVPN — {}", tr("engine updated")), &trf("The VPN engine was updated from {} to {}.", &[&from, &to]));
                }
                self.toast(trf("VPN engine updated to {}", &[&to]), false);
                self.engine.status = trf("updated to {}", &[&to]);
            }
            Ok(Prepared::Installed(v)) => self.log(Level::Info, format!("VPN engine {v} installed (built in)")),
            Ok(Prepared::Same(_)) => {}
            Err(e) => self.log(Level::Warn, format!("VPN engine: {e:#}")),
        }
        self.engine.staged = updater::staged_version(&self.paths.bin_dir());
        self.core_version = updater::installed_version(&self.paths.bin_dir()).or_else(|| self.core_path().map(|p| core::core_version(&p)));
    }

    fn poll_updates(&mut self) {
        let p = &self.update_progress;
        self.engine.download.have = p.have.load(Ordering::Relaxed);
        self.engine.download.total = p.total.load(Ordering::Relaxed);
        self.engine.download.speed = p.speed.load(Ordering::Relaxed);
        while let Ok(m) = self.update_rx.try_recv() {
            match m {
                UpdateMsg::Checking => {
                    self.engine.checking = true;
                    self.engine.download = InstallState { running: true, ..Default::default() };
                    for a in [&self.update_progress.have, &self.update_progress.total, &self.update_progress.speed] {
                        a.store(0, Ordering::Relaxed);
                    }
                    self.engine.status = tr("checking for a new version…").into();
                }
                UpdateMsg::Done(r) => {
                    self.engine.checking = false;
                    self.engine.download.running = false;
                    let now = crate::engine::core::now_hms();
                    match r {
                        Ok(None) => self.engine.status = trf("up to date (checked {})", &[&now]),
                        Ok(Some(v)) => {
                            self.engine.staged = Some(v.clone());
                            self.log(Level::Info, format!("VPN engine {v} downloaded in the background"));
                            if self.backend.is_none() && !self.want_on {
                                // nothing running: switch right away
                                self.prepare_engine();
                            } else {
                                self.engine.status = trf("{} downloaded — used the next time you connect", &[&v]);
                                if self.store.settings.notifications && self.store.settings.notify_engine_update {
                                    platform::notify(&format!("CyberVPN — {}", tr("engine update ready")), &trf("VPN engine {} was downloaded. Your connection was not interrupted; it will be used the next time you connect.", &[&v]));
                                }
                            }
                        }
                        Err(e) => {
                            self.engine.status = trf("update check failed ({}): {}", &[&now, &e.chars().take(80).collect::<String>()]);
                            self.log(Level::Warn, format!("engine update check failed: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn core_path(&self) -> Option<std::path::PathBuf> {
        core::find_core(&self.paths.bin_dir(), &self.store.settings)
    }
    fn set_state(&mut self, s: VpnState) {
        if s == self.state {
            return;
        }
        let notif = self.store.settings.notifications;
        let name = self.active.and_then(|i| self.store.profiles.get(i)).map(|p| p.name.clone()).unwrap_or_default();
        match &s {
            VpnState::Connected if notif => platform::notify(&format!("CyberVPN — {}", tr("Protected")), &trf("Connected to {}", &[&name])),
            VpnState::Reconnecting(1) if notif => platform::notify(&format!("CyberVPN — {}", tr("Reconnecting")), tr("Connection lost, trying again…")),
            VpnState::Error(e) if notif => platform::notify(&format!("CyberVPN — {}", tr("Error")), e),
            VpnState::Off if notif && self.state.is_on() => platform::notify(&format!("CyberVPN — {}", tr("Off")), tr("You are not protected")),
            _ => {}
        }
        if s == VpnState::Connected && self.connected_at.is_none() {
            self.connected_at = Some(Instant::now());
        }
        if !s.is_on() {
            self.connected_at = None;
            self.last_remind = Instant::now();
        }
        self.state = s;
    }

    // ------------------------------------------------------------ actions

    fn connect(&mut self, idx: Option<usize>) {
        let idx = idx.unwrap_or(self.store.selected);
        if self.store.profiles.get(idx).is_none() {
            self.toast(tr("Add a server first: copy a hy2:// link and press “Paste link”"), true);
            return;
        }
        self.disconnect(true);
        self.store.selected = idx;
        self.save();
        self.want_on = true;
        self.active = Some(idx);
        if std::env::var_os("CYBERVPN_DEMO").is_some() {
            // UI demo / screenshots: simulated traffic, no root, no network changes
            let (tx, rx) = channel();
            demo_backend(tx.clone(), self.store.profiles[idx].clone());
            self.backend = Some(Backend { demo: true, tx, child: None, stream: None, rx, started: Instant::now(), stop: None });
            self.set_state(VpnState::Connecting);
            return;
        }
        // a downloaded engine update replaces the old one now — nothing is running
        let backend = self.backend.take();
        self.prepare_engine();
        self.backend = backend;
        if self.core_path().is_none() {
            self.connect_after_install = true;
            self.log(Level::Info, "VPN engine (hysteria core) not installed — downloading it first");
            self.start_install();
            self.set_state(VpnState::Connecting);
            return;
        }
        self.tick = None;
        self.down.clear();
        self.up.clear();
        self.lat.clear();
        self.notified_exit = false;
        if !self.use_tun() {
            // proxy mode: no admin rights, the session runs right here
            let start = BackendStart {
                profile: self.store.profiles[idx].clone(),
                settings: self.store.settings.clone(),
                core_path: self.core_path().unwrap_or_default(),
                tun: false,
            };
            let (tx, rx) = channel();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let (tx, stop) = (tx.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut send = |m: BackendMsg| tx.send(BEvent::Msg(m)).is_ok();
                    if let Err(e) = crate::backend::session(&start, &stop, &mut send) {
                        let _ = tx.send(BEvent::Msg(BackendMsg::Fatal(format!("{e:#}"))));
                    }
                    let _ = tx.send(BEvent::Closed);
                });
            }
            self.backend = Some(Backend { demo: false, tx, child: None, stream: None, rx, started: Instant::now(), stop: Some(stop) });
            self.set_state(VpnState::Connecting);
            self.log(Level::Info, format!("starting VPN (proxy mode) for {}", self.store.profiles[idx].name));
            return;
        }
        let (listener, port) = match ipc::listen() {
            Ok(x) => x,
            Err(e) => return self.fail(format!("{e}")),
        };
        let token = ipc::random_token();
        let child = match platform::spawn_backend(port, &token) {
            Ok(c) => c,
            Err(e) => return self.fail(format!("{e:#}")),
        };
        let (tx, rx) = channel();
        accept_backend(listener, token, tx.clone());
        self.backend = Some(Backend { demo: false, tx, child, stream: None, rx, started: Instant::now(), stop: None });
        self.set_state(VpnState::Authorizing);
        self.log(Level::Info, format!("starting VPN for {}", self.store.profiles[idx].name));
    }

    /// Whole system (TUN, admin) or proxy only? Auto never asks for a password.
    fn use_tun(&self) -> bool {
        match self.store.settings.vpn_mode {
            crate::store::VpnMode::System => true,
            crate::store::VpnMode::Proxy => false,
            crate::store::VpnMode::Auto => crate::engine::sys::is_elevated() || platform::polkit_rule_installed(),
        }
    }

    /// Download the free SocioCyberneering servers in the background.
    fn fetch_free(&mut self, manual: bool) {
        if self.free_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || { let _ = tx.send(crate::freeservers::fetch().map_err(|e| format!("{e:#}"))); });
        self.free_rx = Some((rx, manual));
        if manual {
            self.toast(tr("Getting the free servers…"), false);
        }
    }

    // ------------------------------------------------------------ kill switch

    fn server_hosts(&self) -> Vec<String> {
        let mut h: Vec<String> = self.store.profiles.iter().filter_map(|p| crate::engine::uri::Hy2Uri::parse(&p.uri).ok()).map(|u| u.host).collect();
        h.sort();
        h.dedup();
        h
    }

    /// Turn the kill switch on/off (root helper; asks for the password once
    /// and installs "don't ask again", so later changes are silent).
    fn ks_apply(&mut self, on: bool) {
        if !cfg!(target_os = "linux") || self.ks_rx.is_some() || std::env::var_os("CYBERVPN_DEMO").is_some() {
            return; // demo mode never touches the firewall
        }
        let s = &self.store.settings;
        let hosts = self.server_hosts();
        let mut args = vec!["--killswitch".to_string()];
        if on {
            args.extend(["on".into(), s.tun_name.clone(), if s.bypass_lan { "1" } else { "0" }.into()]);
            args.extend(hosts.iter().cloned());
        } else {
            args.push("off".into());
        }
        self.ks_hosts = hosts;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = (|| {
                if !crate::engine::sys::is_elevated() && !platform::polkit_rule_installed() {
                    platform::allow_without_password().map_err(|e| format!("{e:#}"))?;
                }
                platform::run_as_root(&args).map_err(|e| format!("{e:#}"))
            })();
            let _ = tx.send(r);
        });
        self.ks_rx = Some((rx, on));
    }

    fn poll_ks(&mut self) {
        let Some((rx, on)) = &self.ks_rx else { return };
        let on = *on;
        let Ok(r) = rx.try_recv() else { return };
        self.ks_rx = None;
        match r {
            Ok(()) => {
                self.ks_on = on;
                self.log(Level::Info, if on { "kill switch on: the internet is blocked unless the VPN is on" } else { "kill switch off" });
                if on && !self.state.is_on() {
                    self.toast(tr("Internet blocked until you turn the VPN on (kill switch)"), false);
                }
            }
            Err(e) => {
                self.log(Level::Warn, format!("kill switch: {e}"));
                if on {
                    self.toast(trf("Kill switch could not start: {}", &[&e]), true);
                }
            }
        }
        // the server list changed meanwhile: let the new hosts through
        if self.ks_on && self.store.settings.kill_switch && self.server_hosts() != self.ks_hosts {
            self.ks_apply(true);
        }
    }

    fn poll_free(&mut self) {
        let Some((rx, manual)) = &self.free_rx else { return };
        let manual = *manual;
        let Ok(r) = rx.try_recv() else { return };
        self.free_rx = None;
        match r {
            Ok(text) => {
                let first = self.store.profiles.is_empty();
                let active_uri = self.active.filter(|_| self.state.is_on()).and_then(|i| self.store.profiles.get(i)).map(|p| p.uri.clone());
                let (added, removed, _) = self.store.sync_free(&text);
                self.store.free_servers_done = true;
                // keep pointing at the server in use, wherever it moved
                if let Some(u) = active_uri {
                    self.active = self.store.profiles.iter().position(|p| p.uri == u);
                }
                self.save();
                self.free_retry = false;
                if self.ks_on && self.server_hosts() != self.ks_hosts {
                    self.ks_apply(true);
                }
                if std::mem::take(&mut self.connect_after_free) && !self.store.profiles.is_empty() && !self.state.is_on() {
                    self.connect(None);
                }
                let busy = self.store.profiles.iter().filter(|p| p.free && p.busy).count();
                self.log(Level::Info, format!("free SocioCyberneering servers: +{added} new, −{removed} gone, {busy} busy"));
                if first && added > 0 {
                    self.toast(trf("Free servers ready: {} added — press the button to connect", &[&added]), false);
                } else if manual || added + removed > 0 {
                    self.toast(trf("Free servers updated: +{} new, −{} removed", &[&added, &removed]), false);
                }
            }
            Err(e) => {
                self.free_retry = true;
                self.log(Level::Warn, format!("free servers: {e}"));
                if manual {
                    self.toast(tr("Could not download the free servers — check the internet and try again"), true);
                }
            }
        }
    }

    fn fail(&mut self, msg: String) {
        self.log(Level::Error, msg.clone());
        self.disconnect(true);
        self.want_on = false;
        self.set_state(VpnState::Error(msg));
    }

    fn disconnect(&mut self, quiet: bool) {
        self.want_on = false;
        if let Some(mut b) = self.backend.take() {
            if let Some(stop) = &b.stop {
                stop.store(true, Ordering::Relaxed); // proxy session: restores the desktop proxy and stops
            }
            if let Some(s) = b.stream.take() {
                let _ = s.shutdown(std::net::Shutdown::Both); // backend cleans up and exits
            }
            if let Some(mut c) = b.child.take() {
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        if let Ok(Some(_)) = c.try_wait() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    let _ = c.kill();
                });
            }
            if !quiet {
                self.log(Level::Info, "VPN turned off; routes and DNS restored");
            }
        }
        self.connect_after_install = false;
        if !quiet || self.state.is_on() {
            self.set_state(VpnState::Off);
        }
        if let Some(t) = &mut self.tick {
            t.down_bps = 0.0;
            t.up_bps = 0.0;
        }
    }

    fn start_install(&mut self) {
        if self.install.running {
            return;
        }
        let progress = Arc::new(Progress::default());
        self.install_progress = Some(progress.clone());
        self.install = InstallState { running: true, ..Default::default() };
        let (tx, rx) = channel();
        self.install_rx = Some(rx);
        let bin = self.paths.bin_dir();
        std::thread::spawn(move || {
            let r = core::install_core(&bin, &core::InstallOpts::default(), Some(progress)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }

    fn handle(&mut self, c: Command) {
        match c {
            Command::Connect(i) => self.connect(i),
            Command::Disconnect => self.disconnect(false),
            Command::Toggle => {
                if self.state.is_on() || self.want_on { self.disconnect(false) } else { self.connect(None) }
            }
            Command::Select(i) => {
                if i < self.store.profiles.len() {
                    let was_on = self.state.is_on();
                    self.store.selected = i;
                    self.save();
                    if was_on && self.active != Some(i) {
                        self.connect(Some(i));
                    }
                }
            }
            Command::AddLinks(t) => {
                let (ok, bad) = self.store.add_from_text(&t);
                if !ok.is_empty() {
                    self.save();
                    self.toast(trf("Added: {}", &[&ok.join(", ")]), false);
                } else if let Some(e) = bad.first() {
                    self.toast(trf("Not a valid hy2:// link: {}", &[e]), true);
                } else {
                    self.toast(tr("No hy2:// or hysteria2:// link found in the clipboard"), true);
                }
            }
            Command::Delete(i) => {
                if i < self.store.profiles.len() {
                    if self.active == Some(i) && self.state.is_on() {
                        self.disconnect(false);
                    }
                    let p = self.store.profiles.remove(i);
                    self.store.selected = self.store.selected.min(self.store.profiles.len().saturating_sub(1));
                    self.active = None;
                    self.save();
                    self.toast(trf("Deleted {}", &[&p.name]), false);
                }
            }
            Command::Rename(i, n) => {
                if let Some(p) = self.store.profiles.get_mut(i) {
                    if !n.trim().is_empty() {
                        p.name = n.trim().to_string();
                        self.save();
                    }
                }
            }
            Command::Bandwidth(i, u, d) => {
                if let Some(p) = self.store.profiles.get_mut(i) {
                    p.up_mbps = u;
                    p.down_mbps = d;
                    self.save();
                    self.toast(tr("Speed saved — reconnect to apply"), false);
                }
            }
            Command::SaveSettings(s) => {
                crate::i18n::set(s.language);
                if s.kill_switch != self.store.settings.kill_switch {
                    let on = s.kill_switch;
                    self.store.settings.kill_switch = on;
                    self.ks_apply(on);
                }
                if s.autostart != self.store.settings.autostart {
                    if let Err(e) = platform::set_autostart(s.autostart) {
                        self.toast(format!("Autostart: {e}"), true);
                    }
                }
                self.store.settings = s;
                self.save();
            }
            Command::InstallCore => self.start_install(),
            Command::CheckEngineUpdate => { let _ = self.update_trigger.send(()); }
            Command::FetchFreeServers => self.fetch_free(true),
            Command::AllowWithoutPassword => {
                // pkexec shows a password dialog — don't block the tray meanwhile
                std::thread::spawn(|| match platform::allow_without_password() {
                    Ok(()) => platform::notify("CyberVPN", tr("Done — turning the VPN on will not ask for a password anymore")),
                    Err(e) => platform::notify("CyberVPN", &format!("{e:#}")),
                });
            }
            Command::OpenWindow => {
                let has_gui = !self.clients.lock().unwrap().is_empty();
                if has_gui {
                    self.broadcast(&ToGui::Focus);
                } else {
                    platform::open_window();
                }
            }
            Command::QuitApp => {}
        }
    }

    // ------------------------------------------------------------ backend

    fn poll_backend(&mut self) {
        let mut events = vec![];
        if let Some(b) = &self.backend {
            while let Ok(e) = b.rx.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                BEvent::Accepted(mut s, r) => {
                    let Some(i) = self.active else { continue };
                    let Some(core_path) = self.core_path() else { continue };
                    let start = BackendStart {
                        profile: self.store.profiles[i].clone(),
                        settings: self.store.settings.clone(),
                        core_path,
                        tun: true,
                    };
                    if ipc::send(&mut s, &start).is_err() {
                        self.fail("backend did not accept the start request".into());
                        return;
                    }
                    if let Some(b) = &mut self.backend {
                        b.stream = Some(s);
                        read_backend(r, b.tx.clone());
                    }
                    self.set_state(VpnState::Connecting);
                }
                BEvent::AcceptFailed(m) => self.fail(m),
                BEvent::Msg(BackendMsg::Tick(t)) => {
                    let mut t = *t;
                    for l in t.logs.drain(..) {
                        self.logs.push_back(l);
                    }
                    while self.logs.len() > 300 {
                        self.logs.pop_front();
                    }
                    self.down.push_back(t.down_bps);
                    self.up.push_back(t.up_bps);
                    while self.down.len() > HIST {
                        self.down.pop_front();
                        self.up.pop_front();
                    }
                    if let Some(l) = t.latency_ms {
                        if self.lat.back() != Some(&l) || self.lat.len() < 2 {
                            self.lat.push_back(l);
                        }
                        while self.lat.len() > 60 {
                            self.lat.pop_front();
                        }
                    }
                    let st = match t.phase {
                        Phase::Connecting => VpnState::Connecting,
                        Phase::Connected => VpnState::Connected,
                        Phase::Reconnecting(n) => VpnState::Reconnecting(n),
                    };
                    if !self.notified_exit && st == VpnState::Connected {
                        if let Some(x) = &t.exit {
                            self.notified_exit = true;
                            if self.store.settings.notifications {
                                platform::notify(&format!("CyberVPN — {}", tr("Protected")), &trf("Your IP is now {} ({})", &[&x.ip, &x.country]));
                            }
                        }
                    }
                    if st == VpnState::Connected && self.free_retry && self.free_rx.is_none() {
                        self.fetch_free(false); // now the list can be downloaded through the VPN
                    }
                    self.tick = Some(t);
                    self.set_state(st);
                }
                BEvent::Msg(BackendMsg::Fatal(m)) => self.fail(m),
                BEvent::Closed => {
                    if self.want_on {
                        self.fail(tr("VPN backend stopped unexpectedly").into());
                    }
                }
            }
        }
        // pkexec cancelled / wrong password → child exits before connecting
        let mut fail = None;
        if let Some(b) = &mut self.backend {
            if b.stream.is_none() && !b.demo && b.stop.is_none() {
                if let Some(c) = &mut b.child {
                    if let Ok(Some(st)) = c.try_wait() {
                        fail = Some(match st.code() {
                            Some(126) | Some(127) => tr("Permission denied (password dialog cancelled)").to_string(),
                            _ => format!("backend exited ({st})"),
                        });
                    }
                }
                if b.started.elapsed() > Duration::from_secs(120) {
                    fail = Some(tr("Timed out waiting for administrator permission").into());
                }
            }
        }
        if let Some(m) = fail {
            self.fail(m);
        }
    }

    fn poll_install(&mut self) {
        if let Some(p) = &self.install_progress {
            self.install.have = p.have.load(Ordering::Relaxed);
            self.install.total = p.total.load(Ordering::Relaxed);
            self.install.speed = p.speed.load(Ordering::Relaxed);
        }
        let done = self.install_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(r) = done {
            self.install_rx = None;
            self.install.running = false;
            match r {
                Ok(p) => {
                    self.core_version = Some(core::core_version(&p));
                    self.install.error = None;
                    self.log(Level::Info, format!("VPN engine ready: {}", p.display()));
                    if self.connect_after_install {
                        self.connect_after_install = false;
                        self.connect(self.active);
                    }
                }
                Err(e) => {
                    self.install.error = Some(e.clone());
                    if self.connect_after_install {
                        self.connect_after_install = false;
                        self.fail(trf("Could not download the VPN engine: {}", &[&e]));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ output

    fn snapshot(&self) -> Snapshot {
        let mut tick = self.tick.clone();
        if let Some(t) = &mut tick {
            if !self.state.is_on() {
                t.conns.clear();
            }
        }
        Snapshot {
            state: self.state.clone(),
            store: self.store.clone(),
            active: self.active,
            connected_s: self.connected_at.map(|t| t.elapsed().as_secs()).unwrap_or(0),
            tick,
            down_hist: self.down.iter().copied().collect(),
            up_hist: self.up.iter().copied().collect(),
            lat_hist: self.lat.iter().copied().collect(),
            logs: self.logs.iter().skip(self.logs.len().saturating_sub(150)).cloned().collect(),
            core_version: self.core_version.clone(),
            engine: self.engine.clone(),
            install: self.install.clone(),
            toast: self.toast.clone(),
            can_skip_password: cfg!(target_os = "linux") && !platform::polkit_rule_installed() && !crate::engine::sys::is_elevated(),
            platform_note: if cfg!(windows) { "Windows".into() } else { "Linux".into() },
            will_use_tun: self.use_tun(),
            kill_switch_on: self.ks_on,
        }
    }

    fn broadcast(&self, msg: &ToGui) {
        let Ok(mut line) = serde_json::to_string(msg) else { return };
        line.push('\n');
        // never block the service: each window has its own writer thread;
        // a slow or stuck window just misses a snapshot
        let line = Arc::new(line);
        let mut cl = self.clients.lock().unwrap();
        cl.retain(|(_, tx)| !matches!(tx.try_send(line.clone()), Err(std::sync::mpsc::TrySendError::Disconnected(_))));
    }

    fn tray_update(&self) {
        let speed = match (&self.tick, self.state.is_on()) {
            (Some(t), true) => format!("▼ {}/s   ▲ {}/s", human(t.down_bps), human(t.up_bps)),
            _ => String::new(),
        };
        let _ = self.tray_tx.send(TrayCmd::Update {
            ru: crate::i18n::is_ru(),
            state: self.state.clone(),
            profiles: self.store.profiles.iter().map(|p| p.name.clone()).collect(),
            selected: self.store.selected,
            speed,
        });
    }
}

fn accept_backend(l: TcpListener, token: String, tx: Sender<BEvent>) {
    std::thread::spawn(move || {
        let _ = l.set_nonblocking(false);
        match ipc::accept_auth(&l, &token) {
            Ok((s, r)) => { let _ = tx.send(BEvent::Accepted(s, r)); }
            Err(e) => { let _ = tx.send(BEvent::AcceptFailed(format!("backend connection: {e}"))); }
        }
    });
}

fn read_backend(mut r: BufReader<TcpStream>, tx: Sender<BEvent>) {
    std::thread::spawn(move || {
        loop {
            match ipc::read_line::<BackendMsg>(&mut r) {
                Ok(Some(m)) => {
                    if tx.send(BEvent::Msg(m)).is_err() {
                        return;
                    }
                }
                _ => break,
            }
        }
        let _ = tx.send(BEvent::Closed);
    });
}

/// Simulated backend for `CYBERVPN_DEMO=1` (screenshots, UI work).
fn demo_backend(tx: Sender<BEvent>, profile: crate::store::Profile) {
    std::thread::spawn(move || {
        let sites = [
            ("github.com", "140.82.121.4", "firefox"), ("www.youtube.com", "142.250.74.14", "firefox"),
            ("api.telegram.org", "149.154.167.220", "telegram-desktop"), ("discord.com", "162.159.137.232", "Discord"),
            ("rr3.googlevideo.com", "173.194.18.40", "firefox"), ("crates.io", "18.66.196.110", "cargo"),
            ("cdn.jsdelivr.net", "151.101.1.229", "firefox"), ("yandex.ru", "77.88.55.88", "firefox"),
        ];
        let mut t = Tick { server_ip: "203.0.113.7".into(), ..Default::default() };
        let mut conns: Vec<ConnView> = vec![];
        let mut dns = vec![];
        for i in 0u64.. {
            std::thread::sleep(Duration::from_millis(500));
            let x = i as f64 / 6.0;
            if i == 3 {
                t.phase = Phase::Connected;
                t.logs.push(app_log(Level::Info, format!("connected to {}", profile.name)));
            }
            if i >= 3 {
                t.down_bps = ((x.sin() * 0.5 + 0.6) * 9.0 + ((i * 7) % 5) as f64 * 0.4) * 1048576.0;
                t.up_bps = ((x * 0.7).cos().abs() * 1.4 + 0.2) * 1048576.0;
                t.down_total += (t.down_bps / 2.0) as u64;
                t.up_total += (t.up_bps / 2.0) as u64;
                t.rx_pps = (t.down_bps / 1200.0) as u64;
                t.tx_pps = (t.up_bps / 600.0) as u64;
                t.latency_ms = Some(88 + (i * 13 % 37));
                t.exit = Some(crate::engine::sys::ExitInfo { ip: "203.0.113.7".into(), country: "Netherlands".into(), city: "Amsterdam".into(), cc: "NL".into() });
                t.cpu = 4.0 + (i % 5) as f64;
                if i % 2 == 0 {
                    let (d, ip, app) = sites[(i as usize / 2) % sites.len()];
                    conns.insert(0, ConnView { time: crate::engine::core::now_hms(), tcp: i % 7 != 0, app: app.into(), dst: format!("{ip}:443"), domain: d.into(), open: true, error: String::new(), dur_s: 0, via: "tun".into() });
                    dns.insert(0, crate::engine::dns::DnsEvent { time: crate::engine::core::now_hms(), name: d.into(), qtype: "A".into(), ips: vec![ip.parse().unwrap()], cname: None, ms: 30 + i % 90, status: "NOERROR".into(), tcp: false, direct: d.ends_with(".ru") });
                    t.total_tcp += 1;
                    t.dns_queries += 1;
                }
                for (k, c) in conns.iter_mut().enumerate() {
                    c.dur_s += if c.open { 1 } else { 0 };
                    if k > 6 && c.open && (k + i as usize) % 4 == 0 { c.open = false; }
                }
                conns.truncate(60);
                dns.truncate(60);
                t.open_tcp = conns.iter().filter(|c| c.open && c.tcp).count();
                t.open_udp = conns.iter().filter(|c| c.open && !c.tcp).count();
                t.conns = conns.clone();
                t.dns = dns.clone();
                t.dns_avg_ms = 64;
                t.direct_ips = dns.iter().filter(|e| e.direct).count();
                let mut top: std::collections::HashMap<String, u64> = Default::default();
                for c in &conns { *top.entry(c.domain.clone()).or_default() += 1; }
                let mut v: Vec<_> = top.into_iter().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                t.top = v.into_iter().take(6).collect();
            }
            if tx.send(BEvent::Msg(BackendMsg::Tick(Box::new(t.clone())))).is_err() {
                return;
            }
            t.logs.clear();
        }
    });
}

pub fn human(b: f64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut v, mut i) = (b, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", U[i]) } else { format!("{v:.1} {}", U[i]) }
}

/// Is a service already running? Returns its IPC info if it answers.
pub fn running_instance(paths: &Paths) -> Option<IpcInfo> {
    let info: IpcInfo = serde_json::from_str(&std::fs::read_to_string(paths.ipc_file()).ok()?).ok()?;
    ipc::connect(info.port, &info.token).ok().map(|_| info)
}

pub fn send_command(info: &IpcInfo, cmd: &Command) -> Result<()> {
    let (mut s, _) = ipc::connect(info.port, &info.token)?;
    ipc::send(&mut s, cmd)
}

pub fn run(autostart: bool, background: bool) -> Result<()> {
    let _ = ipc::build_id(); // remember which binary we are, before it may be replaced
    let paths = Paths::get();
    if let Some(info) = running_instance(&paths) {
        if !autostart && !background {
            let _ = send_command(&info, &Command::OpenWindow);
        }
        return Ok(());
    }
    let store = Store::load(&paths).unwrap_or_default();
    crate::i18n::set(store.settings.language);

    // IPC for windows
    let (listener, port) = ipc::listen()?;
    let token = ipc::random_token();
    crate::store::write_private(
        &paths.ipc_file(),
        serde_json::to_string(&IpcInfo { port, token: token.clone(), pid: std::process::id(), build: ipc::build_id() })?.as_bytes(),
    )?;
    let (cmd_tx, cmd_rx) = channel::<Command>();
    let clients: Arc<Mutex<Vec<(u64, std::sync::mpsc::SyncSender<Arc<String>>)>>> = Arc::default();
    {
        let (clients, cmd_tx) = (clients.clone(), cmd_tx.clone());
        std::thread::spawn(move || {
            let mut next_id = 0u64;
            loop {
                let Ok((s, mut r)) = ipc::accept_auth(&listener, &token) else { continue };
                let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                let _ = s.set_nodelay(true);
                next_id += 1;
                let id = next_id;
                let (wtx, wrx) = std::sync::mpsc::sync_channel::<Arc<String>>(4);
                if let Ok(mut w) = s.try_clone() {
                    clients.lock().unwrap().push((id, wtx));
                    std::thread::spawn(move || {
                        while let Ok(line) = wrx.recv() {
                            if std::io::Write::write_all(&mut w, line.as_bytes()).is_err() {
                                break;
                            }
                        }
                        let _ = w.shutdown(std::net::Shutdown::Both);
                    });
                }
                let (tx, clients) = (cmd_tx.clone(), clients.clone());
                std::thread::spawn(move || {
                    while let Ok(Some(c)) = ipc::read_line::<Command>(&mut r) {
                        if tx.send(c).is_err() {
                            break;
                        }
                    }
                    // window gone: forget it (so "open window" really opens one)
                    clients.lock().unwrap().retain(|(i, _)| *i != id);
                });
            }
        });
    }

    let (tray_tx, tray_rx) = channel();
    let tray_events = tray::spawn(tray_rx);

    let core_version = crate::engine::updater::installed_version(&paths.bin_dir());
    // background engine updater: first check after 90 s, then every 6 h, or on demand
    let (update_trigger, trigger_rx) = channel::<()>();
    let (update_tx, update_rx) = channel::<UpdateMsg>();
    let update_progress = Arc::new(Progress::default());
    let auto_update = Arc::new(std::sync::atomic::AtomicBool::new(store.settings.auto_update_engine));
    {
        let (bin, progress, auto) = (paths.bin_dir(), update_progress.clone(), auto_update.clone());
        std::thread::spawn(move || {
            let mut wait = Duration::from_secs(90);
            loop {
                let manual = match trigger_rx.recv_timeout(wait) {
                    Ok(()) => true,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                    Err(_) => return,
                };
                wait = Duration::from_secs(6 * 3600);
                if !manual && !auto.load(Ordering::Relaxed) {
                    continue;
                }
                let Some(cur) = crate::engine::updater::installed_version(&bin) else { continue };
                let _ = update_tx.send(UpdateMsg::Checking);
                let r = crate::engine::updater::check_and_stage(&bin, &cur, Some(progress.clone())).map_err(|e| format!("{e:#}"));
                let _ = update_tx.send(UpdateMsg::Done(r));
            }
        });
    }
    let mut svc = Svc {
        paths, store, state: VpnState::Off, active: None, backend: None, want_on: false, connected_at: None,
        tick: None, down: VecDeque::new(), up: VecDeque::new(), lat: VecDeque::new(), logs: VecDeque::new(),
        core_version, install: InstallState::default(), install_progress: None, install_rx: None,
        connect_after_install: false, toast: None, toast_id: 0, clients, tray_tx,
        last_remind: Instant::now(), notified_exit: false,
        engine: EngineInfo { embedded: crate::engine::updater::embedded_version().map(String::from), ..Default::default() },
        update_trigger, update_rx, update_progress, free_rx: None, free_retry: false, connect_after_free: false, ks_on: false, ks_rx: None, ks_hosts: vec![],
    };
    svc.prepare_engine(); // apply a staged update / install the built-in engine
    if crate::sysproxy::restore(&svc.paths.sysproxy_backup()) {
        svc.log(Level::Warn, "desktop proxy settings from a crashed session restored");
    }
    svc.log(Level::Info, "CyberVPN started");
    if svc.store.settings.update_free_servers || svc.store.profiles.is_empty() {
        svc.fetch_free(false); // keep the free servers fresh (keys get re-made when a server dies)
    }
    #[cfg(target_os = "linux")]
    if !crate::engine::sys::is_elevated() && std::path::Path::new("/var/lib/cybervpn/resolv.conf.bak").exists() {
        svc.log(Level::Warn, "DNS settings from a crashed session will be restored on the next connect");
    }

    if svc.store.settings.autostart {
        let _ = platform::set_autostart(true); // keep the login entry pointing at this binary
    }
    if svc.store.settings.kill_switch {
        svc.ks_apply(true);
    }
    if autostart {
        if svc.store.settings.connect_on_start && svc.store.profiles.is_empty() {
            svc.connect_after_free = true;
        } else if svc.store.settings.connect_on_start {
            svc.connect(None);
        } else if svc.store.settings.reminder_minutes > 0 && svc.store.settings.notifications {
            let tx = cmd_tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(8));
                platform::remind(move || { let _ = tx.send(Command::Connect(None)); });
            });
        }
        if !svc.store.settings.start_minimized {
            platform::open_window();
        }
    }

    let mut last_snap = Instant::now() - Duration::from_secs(1);
    let mut last_tray = Instant::now() - Duration::from_secs(1);
    let mut last_tray_state = (VpnState::Off, usize::MAX, 0usize, false);
    loop {
        // commands from windows
        while let Ok(c) = cmd_rx.try_recv() {
            if matches!(c, Command::QuitApp) {
                svc.disconnect(true);
                if svc.ks_on {
                    // leaving: give the internet back
                    let _ = platform::run_as_root(&["--killswitch".into(), "off".into()]);
                }
                // give a proxy session time to put the desktop proxy back
                for _ in 0..30 {
                    if !svc.paths.sysproxy_backup().exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                svc.broadcast(&ToGui::Bye);
                let _ = svc.tray_tx.send(TrayCmd::Quit);
                let _ = std::fs::remove_file(svc.paths.ipc_file());
                std::thread::sleep(Duration::from_millis(300));
                return Ok(());
            }
            svc.handle(c);
        }
        // tray clicks
        while let Ok(ev) = tray_events.try_recv() {
            match ev {
                TrayEvent::Toggle => svc.handle(Command::Toggle),
                TrayEvent::Open => svc.handle(Command::OpenWindow),
                TrayEvent::Quit => { let _ = cmd_tx.send(Command::QuitApp); }
                TrayEvent::Select(i) => svc.handle(Command::Select(i)),
            }
        }

        svc.poll_backend();
        svc.poll_install();
        svc.poll_updates();
        svc.poll_free();
        svc.poll_ks();
        auto_update.store(svc.store.settings.auto_update_engine, Ordering::Relaxed);

        // reminder while off
        let m = svc.store.settings.reminder_minutes;
        if m > 0 && svc.store.settings.notifications && !svc.state.is_on() && !svc.want_on
            && svc.last_remind.elapsed() >= Duration::from_secs(m as u64 * 60)
        {
            svc.last_remind = Instant::now();
            let tx = cmd_tx.clone();
            platform::remind(move || { let _ = tx.send(Command::Connect(None)); });
        }

        if last_snap.elapsed() >= Duration::from_millis(250) {
            last_snap = Instant::now();
            let snap = svc.snapshot();
            svc.broadcast(&ToGui::Snapshot(Box::new(snap)));
        }
        let ts = (svc.state.clone(), svc.store.selected, svc.store.profiles.len(), crate::i18n::is_ru());
        if ts != last_tray_state || last_tray.elapsed() >= Duration::from_secs(1) {
            last_tray = Instant::now();
            last_tray_state = ts;
            svc.tray_update();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
