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
    sysproxy: Option<crate::sysproxy::SystemProxy>,
    _mixed: Option<crate::engine::mixed::MixedProxy>,
    child: Option<Child>,
    dns: Option<DnsGuard>,
    _dns_server: Option<DnsServer>,
    untune: Vec<(String, String)>,
    cfg_path: PathBuf,
}

impl Drop for Guards {
    fn drop(&mut self) {
        drop(self.sysproxy.take()); // browsers stop using the proxy first
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

    let mut send = |m: BackendMsg| ipc::send(&mut out, &m).is_ok();
    match session(&start, &stop, &mut send) {
        Ok(()) => Ok(()),
        Err(e) => {
            send(BackendMsg::Fatal(format!("{e:#}")));
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

enum Probe { Lat(Option<u64>), Ip(Option<ExitInfo>), Apps(std::collections::HashMap<u16, String>), Ipv6(Option<bool>) }

/// One VPN session. `st.tun`: whole system through a TUN device (needs
/// admin). Otherwise proxy only: runs as the normal user, nothing in the
/// system is changed except (optionally) the desktop proxy setting.
pub fn session(st: &BackendStart, stop: &AtomicBool, send: &mut dyn FnMut(BackendMsg) -> bool) -> Result<()> {
    let s = &st.settings;
    let tun = st.tun;
    if tun && !sys::is_elevated() {
        bail!("backend is not running as administrator/root");
    }
    #[cfg(target_os = "linux")]
    if tun {
        let _ = sys::restore_dns(); // left over from a crash
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
    if tun {
        for m in sys::tun_preflight(&s.tun_name, &["hytun"])? {
            log(Level::Warn, m);
        }
    }
    // one local port for browsers and apps: SOCKS5 + HTTP (127.0.0.1:2080)
    let (mut mixed, mut proxy_ports) = (None, None);
    if !s.proxy.trim().is_empty() {
        let (sp, hp) = (crate::engine::mixed::free_port(), crate::engine::mixed::free_port());
        match crate::engine::mixed::MixedProxy::start(s.proxy.trim(), sp, hp) {
            Ok(m) => {
                mixed = Some(m);
                proxy_ports = Some((sp, hp));
            }
            Err(e) if tun => log(Level::Warn, format!("proxy port {} unavailable: {e}", s.proxy.trim())),
            Err(e) => bail!("cannot open the proxy port {}: {e} (is another VPN app using it?)", s.proxy.trim()),
        }
    } else if !tun {
        bail!("proxy mode needs a proxy address (Settings → Proxy address)");
    }
    let socks_via: Option<std::net::SocketAddr> = proxy_ports.map(|(sp, _)| std::net::SocketAddr::from(([127, 0, 0, 1], sp)));
    let probe_via = if tun { None } else { socks_via };
    let cfg = core::build_config(&st.profile, &uri, s, &server_ips, tun, proxy_ports);
    let dir = if tun { run_dir() } else { crate::store::Paths::get().run_dir() };
    let cfg_path = core::write_config(&dir, &cfg)?;
    let mut untune = vec![];
    if tun && s.tune_sysctl {
        let (msgs, r) = sys::tune();
        for m in msgs {
            log(Level::Info, m);
        }
        untune = r;
    }

    let (log_tx, log_rx): (Sender<LogLine>, Receiver<LogLine>) = channel();
    let child = core::spawn(&st.core_path, &cfg_path, log_tx.clone())?;
    let proxy_addr = mixed.as_ref().map(|m| m.listen.to_string()).unwrap_or_default();
    let mut g = Guards { sysproxy: None, _mixed: None, child: Some(child), dns: None, _dns_server: None, untune, cfg_path: cfg_path.clone() };
    if !tun {
        log(Level::Info, format!("proxy mode (no admin rights): SOCKS5 + HTTP on {proxy_addr}"));
        if s.system_proxy {
            if let Some(m) = &mixed {
                match crate::sysproxy::SystemProxy::apply(m.listen, &crate::store::Paths::get().sysproxy_backup()) {
                    Ok((sp, what)) => {
                        log(Level::Info, format!("{} → {proxy_addr} (restored when you turn the VPN off)", what.join(", ")));
                        g.sysproxy = Some(sp);
                    }
                    Err(e) => log(Level::Warn, format!("{e} — set the proxy {proxy_addr} in your browser")),
                }
            }
        }
    } else if !proxy_addr.is_empty() {
        log(Level::Info, format!("browser proxy: SOCKS5 + HTTP on {proxy_addr}"));
    }

    let (dns_tx, dns_rx): (Sender<DnsEvent>, Receiver<DnsEvent>) = channel();
    #[allow(unused_mut)]
    let mut bypass: Option<std::sync::Arc<crate::engine::split::Bypass>> = None;
    #[cfg(target_os = "linux")]
    if tun && s.not_to_vpn {
        if s.override_dns {
            let b = std::sync::Arc::new(crate::engine::split::Bypass::new(&s.not_to_vpn_domains, &s.direct_dns));
            log(Level::Info, format!("notToVPN: {} go direct (DNS {})", b.patterns().join(", "), s.direct_dns.join(", ")));
            bypass = Some(b);
        } else {
            log(Level::Warn, "notToVPN needs “Private DNS through the tunnel” to be on".into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    if tun && s.not_to_vpn {
        log(Level::Warn, "notToVPN is not available on this system yet".into());
    }
    if tun && s.override_dns {
        #[cfg(target_os = "linux")]
        let servers = match DnsServer::start(&s.dns, dns_tx.clone(), bypass.clone(), s.ipv6 == "on") {
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
    let mut tick = Tick { server_ip: server_ips[0].to_string(), proxy_mode: !tun, proxy_addr: proxy_addr.clone(), system_proxy: g.sysproxy.is_some(), ..Default::default() };
    let mut last_proxy: Option<(u64, u64, Instant)> = None;
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
    // watchdog: Wi-Fi off/on or another network → fresh core; TUN missing → fresh core
    let mut net_sig = sys::default_route();
    let mut net_changed: Option<Instant> = None;
    let mut last_watch = Instant::now();
    let mut last_fix: Option<Instant> = None;
    let mut lat_fail = 0u32;
    let core_started = Instant::now();

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
                if s.ipv6 == "auto" && g._dns_server.is_some() {
                    let tx = probe_tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(800));
                        let _ = tx.send(Probe::Ipv6(crate::engine::dns::ipv6_works()));
                    });
                }
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
                Probe::Lat(v) => {
                    probing_lat = false;
                    tick.latency_ms = v;
                    lat_fail = if v.is_some() { 0 } else { lat_fail + 1 };
                }
                Probe::Ip(v) => { probing_ip = false; if v.is_some() { tick.exit = v; } }
                Probe::Apps(m) => { probing_apps = false; traffic.set_apps(&m); }
                Probe::Ipv6(v) => {
                    if let (Some(srv), Some(ok)) = (&g._dns_server, v) {
                        if ok != srv.ipv6_on() || !ok {
                            pending_logs.push(app_log(Level::Info, if ok {
                                "IPv6 works through the server — IPv6 enabled".to_string()
                            } else {
                                "server has no IPv6 — sites use IPv4 (IPv6 addresses hidden, nothing leaks)".to_string()
                            }));
                        }
                        srv.set_ipv6(ok);
                    }
                }
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

        // watchdog (Linux): restart the core when the network changed, the TUN
        // device is missing, or nothing gets through — like toggling Wi-Fi did
        #[cfg(target_os = "linux")]
        if last_watch.elapsed() >= Duration::from_secs(1) && s.auto_reconnect {
            last_watch = Instant::now();
            let sig = sys::default_route();
            if sig != net_sig {
                pending_logs.push(app_log(Level::Info, match &sig { Some(x) => format!("network: {x}"), None => "network: offline".into() }));
                net_sig = sig;
                net_changed = Some(Instant::now());
            }
            let calm = last_fix.is_none_or(|t| t.elapsed() > Duration::from_secs(20));
            let why = if net_sig.is_none() || !calm || g.child.is_none() {
                None
            } else if net_changed.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
                net_changed = None;
                Some("the network changed")
            } else if tun && core_started.elapsed() > Duration::from_secs(6) && sys::if_stats(&s.tun_name).is_none() {
                Some("the TUN device is missing")
            } else if connected && lat_fail >= 5 {
                Some("connected, but nothing gets through")
            } else {
                None
            };
            if let Some(why) = why {
                last_fix = Some(Instant::now());
                lat_fail = 0;
                pending_logs.push(app_log(Level::Warn, format!("{why} — restarting the VPN engine")));
                if let Some(mut c) = g.child.take() {
                    core::stop(&mut c);
                }
                if tun {
                    let _ = sys::tun_preflight(&s.tun_name, &["hytun"]); // clear stale TUN / routes
                }
                connected = false;
                tick.phase = Phase::Reconnecting(tick.restarts + 1);
                next_restart = Some(Instant::now() + Duration::from_millis(500));
            }
        }

        // interface counters
        let now = Instant::now();
        if let Some(cur) = if tun { sys::if_stats(&s.tun_name) } else { None } {
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
        // proxy traffic does not pass the TUN device: add it
        if let Some(m) = &mixed {
            use std::sync::atomic::Ordering::Relaxed;
            let (d, u) = (m.c.down.load(Relaxed), m.c.up.load(Relaxed));
            if let Some((ld, lu, lt)) = last_proxy {
                let dt = now.duration_since(lt).as_secs_f64().max(0.001);
                tick.down_total += d - ld;
                tick.up_total += u - lu;
                tick.down_bps += (d - ld) as f64 / dt;
                tick.up_bps += (u - lu) as f64 / dt;
            }
            last_proxy = Some((d, u, now));
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
            std::thread::spawn(move || { let _ = tx.send(Probe::Lat(sys::latency_ms(probe_via))); });
        }
        if connected && !probing_ip && last_ip.is_none_or(|t| t.elapsed() >= Duration::from_secs(120)) {
            probing_ip = true;
            last_ip = Some(now);
            let tx = probe_tx.clone();
            std::thread::spawn(move || { let _ = tx.send(Probe::Ip(sys::public_ip(probe_via))); });
        }
        if !probing_apps && last_apps.elapsed() >= Duration::from_secs(1) {
            last_apps = now;
            let ports = traffic.pending_app_ports();
            if !ports.is_empty() {
                probing_apps = true;
                let tx = probe_tx.clone();
                // proxy connections: the core sees our relay socket; look up the app's own port instead
                let map: std::collections::HashMap<u16, u16> = mixed.as_ref().map(|m| m.c.ports.lock().unwrap().clone()).unwrap_or_default();
                std::thread::spawn(move || {
                    let real: Vec<u16> = ports.iter().map(|p| *map.get(p).unwrap_or(p)).collect();
                    let found = traffic::apps_for_ports(&real);
                    let back = ports.iter().filter_map(|p| found.get(map.get(p).unwrap_or(p)).map(|a| (*p, a.clone()))).collect();
                    let _ = tx.send(Probe::Apps(back));
                });
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
        tick.ipv6 = g._dns_server.as_ref().map(|d| d.ipv6_on());
        tick.top = traffic.top_domains(8);
        tick.conns = traffic
            .conns
            .iter()
            .rev()
            .take(100)
            .map(|c| ConnView {
                time: c.time.clone(),
                tcp: c.proto == Proto::Tcp,
                app: c.app.clone().unwrap_or_default(),
                dst: c.dst.clone().unwrap_or_else(|| "udp session".into()),
                domain: c.domain.clone().unwrap_or_default(),
                open: c.is_open(),
                error: c.error.clone().unwrap_or_default(),
                dur_s: c.closed.unwrap_or_else(Instant::now).duration_since(c.opened).as_secs(),
                via: c.via.to_string(),
            })
            .collect();
        tick.dns = dns_log.iter().rev().take(100).cloned().collect();
        tick.logs = std::mem::take(&mut pending_logs);
        if !send(BackendMsg::Tick(Box::new(tick.clone()))) {
            break; // service gone
        }
        let spent = t0.elapsed();
        if spent < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(500) - spent);
        }
    }
    g._mixed = mixed.take();
    drop(g); // desktop proxy back, stop core, restore DNS + sysctl
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
