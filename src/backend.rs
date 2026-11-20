//! Elevated backend (root / Administrator). Started by the tray service via
//! pkexec (Linux) or UAC (Windows). It runs the hysteria core in TUN mode,
//! takes over DNS, tracks connections and streams a `Tick` every 500 ms.
//! When the service connection closes, everything is stopped and restored.

use crate::engine::core::{self, app_log, Level, LogLine};
use crate::engine::dns::{DnsEvent, DnsServer};
use crate::engine::sys::{self, DnsGuard, ExitInfo, IfStats};
use crate::engine::traffic::{self, Proto, Traffic};
use crate::engine::uri::Hy2Uri;
use crate::ipc;
use crate::protocol::{BackendMsg, BackendStart, ConnView, Phase, Tick};
use anyhow::{bail, Context, Result};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn run_dir() -> PathBuf {
    if cfg!(windows) { std::env::temp_dir().join("CyberVPN") } else { PathBuf::from("/run/cybervpn") }
}

struct Guards {
    child: Option<Child>,
    dns: Option<DnsGuard>,
    _dns_server: Option<DnsServer>,
    untune: Vec<(String, String)>,
    cfg_path: PathBuf,
}

impl Drop for Guards {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            core::stop(&mut c);
        }
        if let Some(mut d) = self.dns.take() {
            d.restore();
        }
        sys::untune(&self.untune);
        let _ = std::fs::remove_file(&self.cfg_path);
    }
}

pub fn run(port: u16, token: &str) -> Result<()> {
    let (mut out, mut inp) = ipc::connect(port, token).context("cannot reach CyberVPN service")?;
    let start: BackendStart = ipc::read_line(&mut inp)?.context("no start request")?;

    // stop when the service goes away (or sends anything)
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::BufRead::read_line(&mut inp, &mut buf);
            stop.store(true, Ordering::Relaxed);
        });
    }
    #[cfg(unix)]
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let stop = stop.clone();
        // minimal signal handling without extra crates: a thread polls a pipe-less flag
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
        std::thread::spawn(move || loop {
            if SIGNALLED.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        });
    }

    match session(&start, &stop, &mut out) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = ipc::send(&mut out, &BackendMsg::Fatal(format!("{e:#}")));
            Err(e)
        }
    }
}

#[cfg(unix)]
static SIGNALLED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

enum Probe { Lat(Option<u64>), Ip(Option<ExitInfo>), Apps(std::collections::HashMap<u16, String>) }

fn session(st: &BackendStart, stop: &AtomicBool, out: &mut std::net::TcpStream) -> Result<()> {
    let s = &st.settings;
    if !sys::is_elevated() {
        bail!("backend is not running as administrator/root");
    }
    #[cfg(target_os = "linux")]
    if let Ok(true) = sys::restore_dns() {
        // left over from a crash
    }
    let uri = Hy2Uri::parse(&st.profile.uri)?;
    let server_ips: Vec<IpAddr> = sys::resolve(&uri.host)?;
    if server_ips.is_empty() {
        bail!("cannot resolve {}", uri.host);
    }
    let mut pending_logs: Vec<LogLine> = vec![];
    let mut log = |l: Level, m: String| pending_logs.push(app_log(l, m));

    log(Level::Info, format!(
        "server {} → {} (sni {})",
        uri.server_with(&uri.host),
        server_ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "),
        uri.sni_or_host()
    ));
    match (st.profile.up_mbps, st.profile.down_mbps) {
        (None, None) => log(Level::Info, "congestion control: BBR (set bandwidth for Brutal)".into()),
        (u, d) => log(Level::Info, format!("congestion control: Brutal ↑{:?} ↓{:?} Mbit/s", u, d)),
    }

    // never run next to another TUN core (e.g. hy2tui): they share routing table 2022
    for m in sys::tun_preflight(&s.tun_name, &["hytun"])? {
        log(Level::Warn, m);
    }
    let cfg = core::build_config(&st.profile, &uri, s, &server_ips);
    let cfg_path = core::write_config(&run_dir(), &cfg)?;
    let mut untune = vec![];
    if s.tune_sysctl {
        let (msgs, r) = sys::tune();
        for m in msgs {
            log(Level::Info, m);
        }
        untune = r;
    }

    let (log_tx, log_rx): (Sender<LogLine>, Receiver<LogLine>) = channel();
    let child = core::spawn(&st.core_path, &cfg_path, log_tx.clone())?;
    let mut g = Guards { child: Some(child), dns: None, _dns_server: None, untune, cfg_path: cfg_path.clone() };

    let (dns_tx, dns_rx): (Sender<DnsEvent>, Receiver<DnsEvent>) = channel();
    #[allow(unused_mut)]
    let mut bypass: Option<std::sync::Arc<crate::engine::split::Bypass>> = None;
    #[cfg(target_os = "linux")]
    if s.not_to_vpn {
        if s.override_dns {
            let b = std::sync::Arc::new(crate::engine::split::Bypass::new(&s.not_to_vpn_domains, &s.direct_dns));
            log(Level::Info, format!("notToVPN: {} go direct (DNS {})", b.patterns().join(", "), s.direct_dns.join(", ")));
            bypass = Some(b);
        } else {
            log(Level::Warn, "notToVPN needs “Private DNS through the tunnel” to be on".into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    if s.not_to_vpn {
        log(Level::Warn, "notToVPN is not available on this system yet".into());
    }
    if s.override_dns {
        #[cfg(target_os = "linux")]
        let servers = match DnsServer::start(&s.dns, dns_tx.clone(), bypass.clone()) {
            Ok(srv) => {
                g._dns_server = Some(srv);
                vec![crate::engine::dns::LISTEN_IP.to_string()]
            }
            Err(e) => {
                log(Level::Warn, format!("DNS logger unavailable: {e}"));
                s.dns.clone()
            }
        };
        #[cfg(not(target_os = "linux"))]
        let servers = s.dns.clone();
        match DnsGuard::apply(&servers, &s.tun_name) {
            Ok(d) => {
                g.dns = Some(d);
                log(Level::Info, format!("DNS → {} through the tunnel", s.dns.join(", ")));
            }
            Err(e) => log(Level::Warn, format!("DNS override failed: {e}")),
        }
    }
    let _keep_tx = dns_tx;

    // ---------------------------------------------------------------- loop
    let (probe_tx, probe_rx) = channel::<Probe>();
    let mut traffic = Traffic::default();
    let mut tick = Tick { server_ip: server_ips[0].to_string(), ..Default::default() };
    let mut last_if: Option<(IfStats, Instant)> = None;
    let mut last_cpu: Option<(u64, Instant)> = None;
    let clk = sys::clk_tck();
    let mut dns_log: std::collections::VecDeque<DnsEvent> = Default::default();
    let mut dns_ms_sum = 0u64;
    let (mut probing_lat, mut probing_ip, mut probing_apps) = (false, false, false);
    let (mut last_lat, mut last_ip, mut last_apps) = (None::<Instant>, None::<Instant>, Instant::now());
    let mut next_restart: Option<Instant> = None;
    let mut last_dns_check = Instant::now();
    let mut connected = false;

    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        // core logs
        while let Ok(l) = log_rx.try_recv() {
            if l.msg == "connected to server" {
                connected = true;
                tick.phase = Phase::Connected;
                tick.reconnects = l.count.unwrap_or(1).saturating_sub(1);
                tick.restarts = 0;
                last_ip = None;
            }
            if traffic.on_log(&l) || l.level == Level::Debug {
                continue;
            }
            pending_logs.push(l);
        }
        while let Ok(ev) = dns_rx.try_recv() {
            tick.dns_queries += 1;
            dns_ms_sum += ev.ms;
            if ev.status != "NOERROR" {
                tick.dns_fail += 1;
            }
            if !ev.ips.is_empty() {
                traffic.learn_dns(&ev.name, &ev.ips);
            }
            dns_log.push_back(ev);
            if dns_log.len() > 200 {
                dns_log.pop_front();
            }
        }
        while let Ok(p) = probe_rx.try_recv() {
            match p {
                Probe::Lat(v) => { probing_lat = false; tick.latency_ms = v; }
                Probe::Ip(v) => { probing_ip = false; if v.is_some() { tick.exit = v; } }
                Probe::Apps(m) => { probing_apps = false; traffic.set_apps(&m); }
            }
        }

        // supervise the core
        if let Some(c) = &mut g.child {
            if let Ok(Some(status)) = c.try_wait() {
                g.child = None;
                connected = false;
                tick.restarts += 1;
                let delay = (2u64 << tick.restarts.min(4)).min(30);
                pending_logs.push(app_log(Level::Error, format!("core exited ({status}); restart in {delay}s")));
                tick.phase = Phase::Reconnecting(tick.restarts);
                if s.auto_reconnect {
                    next_restart = Some(Instant::now() + Duration::from_secs(delay));
                } else {
                    bail!("core stopped");
                }
            }
        } else if next_restart.is_some_and(|t| Instant::now() >= t) {
            next_restart = None;
            match core::spawn(&st.core_path, &cfg_path, log_tx.clone()) {
                Ok(c) => g.child = Some(c),
                Err(e) => {
                    pending_logs.push(app_log(Level::Error, format!("{e:#}")));
                    next_restart = Some(Instant::now() + Duration::from_secs(10));
                }
            }
        }

        // interface counters
        let now = Instant::now();
        if let Some(cur) = sys::if_stats(&s.tun_name) {
            if let Some((l, lt)) = last_if {
                let dt = now.duration_since(lt).as_secs_f64().max(0.001);
                let d = |a: u64, b: u64| if a >= b { a - b } else { a };
                let (drx, dtx) = (d(cur.rx_bytes, l.rx_bytes), d(cur.tx_bytes, l.tx_bytes));
                let (prx, ptx) = (d(cur.rx_packets, l.rx_packets), d(cur.tx_packets, l.tx_packets));
                tick.down_total += drx;
                tick.up_total += dtx;
                tick.rx_packets += prx;
                tick.tx_packets += ptx;
                tick.down_bps = drx as f64 / dt;
                tick.up_bps = dtx as f64 / dt;
                tick.rx_pps = (prx as f64 / dt) as u64;
                tick.tx_pps = (ptx as f64 / dt) as u64;
            }
            tick.if_errors = cur.errors;
            tick.if_dropped = cur.dropped;
            last_if = Some((cur, now));
        } else {
            tick.down_bps = 0.0;
            tick.up_bps = 0.0;
            last_if = None;
        }
        if let Some(pid) = g.child.as_ref().map(|c| c.id()) {
            if let Some((ticks, rss)) = sys::proc_cpu_rss(pid) {
                if let Some((lt, lw)) = last_cpu {
                    tick.cpu = ticks.saturating_sub(lt) as f64 / clk / now.duration_since(lw).as_secs_f64().max(0.001) * 100.0;
                }
                last_cpu = Some((ticks, now));
                tick.rss_kb = rss;
            }
        }

        // probes
        if connected && !probing_lat && last_lat.is_none_or(|t| t.elapsed() >= Duration::from_secs(3)) {
            probing_lat = true;
            last_lat = Some(now);
            let tx = probe_tx.clone();
            std::thread::spawn(move || { let _ = tx.send(Probe::Lat(sys::latency_ms())); });
        }
        if connected && !probing_ip && last_ip.is_none_or(|t| t.elapsed() >= Duration::from_secs(120)) {
            probing_ip = true;
            last_ip = Some(now);
            let tx = probe_tx.clone();
            std::thread::spawn(move || { let _ = tx.send(Probe::Ip(sys::public_ip())); });
        }
        if !probing_apps && last_apps.elapsed() >= Duration::from_secs(1) {
            last_apps = now;
            let ports = traffic.pending_app_ports();
            if !ports.is_empty() {
                probing_apps = true;
                let tx = probe_tx.clone();
                std::thread::spawn(move || { let _ = tx.send(Probe::Apps(traffic::apps_for_ports(&ports))); });
            }
        }
        if last_dns_check.elapsed() >= Duration::from_secs(3) {
            last_dns_check = now;
            if g.dns.as_ref().is_some_and(|d| d.ensure()) {
                pending_logs.push(app_log(Level::Warn, "system rewrote resolv.conf — DNS override re-applied"));
            }
        }

        // build + send
        let (ot, ou) = traffic.open_count();
        tick.open_tcp = ot;
        tick.open_udp = ou;
        tick.total_tcp = traffic.total_tcp;
        tick.total_udp = traffic.total_udp;
        tick.conn_errors = traffic.errors;
        tick.dns_avg_ms = dns_ms_sum.checked_div(tick.dns_queries).unwrap_or(0);
        tick.direct_ips = bypass.as_ref().map(|b| b.count()).unwrap_or(0);
        tick.top = traffic.top_domains(8);
        tick.conns = traffic
            .conns
            .iter()
            .rev()
            .take(150)
            .map(|c| ConnView {
                time: c.time.clone(),
                tcp: c.proto == Proto::Tcp,
                app: c.app.clone().unwrap_or_default(),
                dst: c.dst.clone().unwrap_or_else(|| "udp session".into()),
                domain: c.domain.clone().unwrap_or_default(),
                open: c.is_open(),
                error: c.error.clone().unwrap_or_default(),
                dur_s: c.closed.unwrap_or_else(Instant::now).duration_since(c.opened).as_secs(),
            })
            .collect();
        tick.dns = dns_log.iter().rev().take(150).cloned().collect();
        tick.logs = std::mem::take(&mut pending_logs);
        if ipc::send(out, &BackendMsg::Tick(Box::new(tick.clone()))).is_err() {
            break; // service gone
        }
        let spent = t0.elapsed();
        if spent < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(500) - spent);
        }
    }
    drop(g); // stop core, restore DNS + sysctl
    if bypass.take().is_some() {
        crate::engine::split::Bypass::cleanup_all();
    }
    Ok(())
}

/// Linux: let `user` start this exact binary through pkexec without a password.
#[cfg(target_os = "linux")]
pub fn install_polkit_rule(user: &str) -> Result<()> {
    if !sys::is_elevated() {
        bail!("must run as root");
    }
    if !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        bail!("bad user name");
    }
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let rule = format!(
        "// Installed by CyberVPN: start the VPN backend without a password.\n\
         polkit.addRule(function(action, subject) {{\n    \
             if (action.id == \"org.freedesktop.policykit.exec\" &&\n        \
                 action.lookup(\"program\") == \"{exe}\" &&\n        \
                 subject.user == \"{user}\" && subject.local && subject.active) {{\n        \
                 return polkit.Result.YES;\n    \
             }}\n\
         }});\n"
    );
    let dir = std::path::Path::new("/etc/polkit-1/rules.d");
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("49-cybervpn.rules"), rule)?;
    Ok(())
}
