//! OS-specific parts: interface counters, DNS takeover, kernel tuning,
//! privilege checks, and small network probes. Linux + Windows.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub fn resolve(host: &str) -> Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let mut ips: Vec<IpAddr> = (host, 443)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host}"))?
        .map(|a| a.ip())
        .collect();
    ips.sort_by_key(|i| !i.is_ipv4());
    ips.dedup();
    Ok(ips)
}

#[derive(Default, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct IfStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub errors: u64,
    pub dropped: u64,
}

// ---------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;
    use std::path::Path;

    pub fn is_elevated() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    pub fn if_stats(name: &str) -> Option<IfStats> {
        let base = Path::new("/sys/class/net").join(name).join("statistics");
        let rd = |f: &str| fs::read_to_string(base.join(f)).ok()?.trim().parse::<u64>().ok();
        Some(IfStats {
            rx_bytes: rd("rx_bytes")?,
            tx_bytes: rd("tx_bytes")?,
            rx_packets: rd("rx_packets")?,
            tx_packets: rd("tx_packets")?,
            errors: rd("rx_errors").unwrap_or(0) + rd("tx_errors").unwrap_or(0),
            dropped: rd("rx_dropped").unwrap_or(0) + rd("tx_dropped").unwrap_or(0),
        })
    }

    fn sysctl_read(key: &str) -> Option<String> {
        fs::read_to_string(format!("/proc/sys/{}", key.replace('.', "/"))).ok().map(|s| s.trim().to_string())
    }
    fn sysctl_write(key: &str, v: &str) -> bool {
        fs::write(format!("/proc/sys/{}", key.replace('.', "/")), v).is_ok()
    }

    /// Big UDP buffers for QUIC + loose rp_filter for TUN.
    pub fn tune() -> (Vec<String>, Vec<(String, String)>) {
        let mut log = vec![];
        let mut restore = vec![];
        const BUF: u64 = 16 * 1024 * 1024;
        for key in ["net.core.rmem_max", "net.core.wmem_max"] {
            if let Some(cur) = sysctl_read(key).and_then(|v| v.parse::<u64>().ok()) {
                if cur < BUF && sysctl_write(key, &BUF.to_string()) {
                    log.push(format!("{key}: {cur} → {BUF} (faster QUIC)"));
                }
            }
        }
        for key in ["net.ipv4.conf.all.rp_filter", "net.ipv4.conf.default.rp_filter"] {
            if let Some(cur) = sysctl_read(key) {
                if cur != "2" && sysctl_write(key, "2") {
                    restore.push((key.to_string(), cur));
                }
            }
        }
        (log, restore)
    }

    pub fn untune(vals: &[(String, String)]) {
        for (k, v) in vals {
            sysctl_write(k, v);
        }
    }

    const RESOLV: &str = "/etc/resolv.conf";
    const STATE_DIR: &str = "/var/lib/cybervpn";
    const MARK: &str = "# CyberVPN: temporary, restored on disconnect";
    fn bak() -> std::path::PathBuf { Path::new(STATE_DIR).join("resolv.conf.bak") }
    fn link() -> std::path::PathBuf { Path::new(STATE_DIR).join("resolv.conf.link") }

    pub struct DnsGuard {
        content: String,
        active: bool,
    }

    impl DnsGuard {
        pub fn apply(servers: &[String], _tun: &str) -> Result<Self> {
            fs::create_dir_all(STATE_DIR)?;
            if !bak().exists() && !link().exists() {
                match fs::symlink_metadata(RESOLV) {
                    Ok(m) if m.file_type().is_symlink() => {
                        fs::write(link(), fs::read_link(RESOLV)?.as_os_str().as_encoded_bytes())?;
                    }
                    Ok(_) => {
                        fs::copy(RESOLV, bak())?;
                    }
                    Err(_) => fs::write(bak(), "")?,
                }
            }
            let mut content = format!("{MARK}\n");
            for s in servers {
                content.push_str(&format!("nameserver {s}\n"));
            }
            content.push_str("options timeout:2 attempts:2\n");
            let g = DnsGuard { content, active: true };
            g.write()?;
            Ok(g)
        }
        fn write(&self) -> Result<()> {
            if fs::symlink_metadata(RESOLV).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                fs::remove_file(RESOLV)?;
            }
            let tmp = format!("{RESOLV}.cybervpn");
            fs::write(&tmp, &self.content)?;
            fs::rename(&tmp, RESOLV)?;
            Ok(())
        }
        /// NetworkManager/dhcpcd may rewrite resolv.conf; put ours back.
        pub fn ensure(&self) -> bool {
            self.active && fs::read_to_string(RESOLV).unwrap_or_default() != self.content && self.write().is_ok()
        }
        pub fn restore(&mut self) {
            if self.active {
                let _ = restore_dns();
                self.active = false;
            }
        }
    }

    impl Drop for DnsGuard {
        fn drop(&mut self) {
            self.restore();
        }
    }

    pub fn restore_dns() -> Result<bool> {
        if link().exists() {
            let target = fs::read_to_string(link())?;
            let _ = fs::remove_file(RESOLV);
            std::os::unix::fs::symlink(target.trim_end_matches('\n'), RESOLV)?;
            fs::remove_file(link())?;
            Ok(true)
        } else if bak().exists() {
            let data = fs::read(bak())?;
            let tmp = format!("{RESOLV}.cybervpn");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, RESOLV)?;
            fs::remove_file(bak())?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn proc_cpu_rss(pid: u32) -> Option<(u64, u64)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = &stat[stat.rfind(')')? + 2..];
        let f: Vec<&str> = after.split_whitespace().collect();
        let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let rss = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            .unwrap_or(0);
        Some((ticks, rss))
    }

    pub fn clk_tck() -> f64 {
        let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if v > 0 { v as f64 } else { 100.0 }
    }

    // ---------------------------------------------------------------- TUN preflight

    /// Other VPN cores that also take over routing in TUN mode. Two of them at
    /// once share the same routing table (2022) and break each other.
    const CORES: &[&str] = &["hysteria", "sing-box", "nekobox_core", "nekoray_core", "xray", "v2ray"];

    /// Running VPN cores (pid, name), except `except_pid`.
    pub fn running_cores(except_pid: Option<u32>) -> Vec<(u32, String)> {
        let mut out = vec![];
        let Ok(rd) = std::fs::read_dir("/proc") else { return out };
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if Some(pid) == except_pid {
                continue;
            }
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let comm = comm.trim();
            if CORES.contains(&comm) {
                out.push((pid, comm.to_string()));
            }
        }
        out
    }

    fn ip(args: &[&str]) -> Option<String> {
        let o = std::process::Command::new("ip").args(args).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// Before starting TUN mode: refuse if another core owns the routes, and
    /// clean up what a crashed session left behind (TUN device, ip rules and
    /// routes in table 2022). Returns log messages.
    pub fn tun_preflight(own_tun: &str, other_tuns: &[&str]) -> anyhow::Result<Vec<String>> {
        let mut log = vec![];
        let cores = running_cores(None);
        if let Some((pid, name)) = cores.first() {
            let who = match name.as_str() {
                "hysteria" => "another hysteria core (hy2tui or CyberVPN?)",
                "sing-box" | "nekobox_core" | "nekoray_core" => "nekoray / sing-box",
                _ => "another VPN core",
            };
            anyhow::bail!("{who} is running (pid {pid}). Two TUN VPNs at once break each other's routes — turn it off first (or use proxy-only mode)");
        }
        for t in other_tuns {
            if std::path::Path::new("/sys/class/net").join(t).exists() {
                log.push(format!("removing stale TUN device {t}"));
                let _ = ip(&["link", "delete", t]);
            }
        }
        if std::path::Path::new("/sys/class/net").join(own_tun).exists() {
            log.push(format!("removing stale TUN device {own_tun} from a previous session"));
            let _ = ip(&["link", "delete", own_tun]);
        }
        let n = crate::engine::split::Bypass::cleanup_all();
        if n > 0 {
            log.push(format!("removed {n} leftover notToVPN rules"));
        }
        // leftover policy rules of a killed core (sing-tun uses table 2022)
        for fam in ["-4", "-6"] {
            if let Some(rules) = ip(&[fam, "rule", "show"]) {
                for line in rules.lines().filter(|l| l.contains("lookup 2022") || l.contains("table 2022")) {
                    if let Some(pref) = line.split(':').next().map(str::trim) {
                        if ip(&[fam, "rule", "del", "pref", pref]).is_some() {
                            log.push(format!("removed stale ip {fam} rule {pref}"));
                        }
                    }
                }
            }
            let _ = ip(&[fam, "route", "flush", "table", "2022"]);
        }
        Ok(log)
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::process::Command;

    pub fn is_elevated() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elev as *mut _ as *mut _,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && elev.TokenIsElevated != 0
        }
    }

    pub fn if_stats(name: &str) -> Option<IfStats> {
        use windows_sys::Win32::NetworkManagement::IpHelper::{ConvertInterfaceAliasToLuid, GetIfEntry2, MIB_IF_ROW2};
        use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let mut luid: NET_LUID_LH = std::mem::zeroed();
            if ConvertInterfaceAliasToLuid(wide.as_ptr(), &mut luid) != 0 {
                return None;
            }
            let mut row: MIB_IF_ROW2 = std::mem::zeroed();
            row.InterfaceLuid = luid;
            if GetIfEntry2(&mut row) != 0 {
                return None;
            }
            Some(IfStats {
                rx_bytes: row.InOctets,
                tx_bytes: row.OutOctets,
                rx_packets: row.InUcastPkts + row.InNUcastPkts,
                tx_packets: row.OutUcastPkts + row.OutNUcastPkts,
                errors: row.InErrors + row.OutErrors,
                dropped: row.InDiscards + row.OutDiscards,
            })
        }
    }

    pub fn tune() -> (Vec<String>, Vec<(String, String)>) {
        (vec![], vec![])
    }
    pub fn untune(_: &[(String, String)]) {}

    /// Windows: put DNS servers on the TUN adapter (they are then reached
    /// through the tunnel) and lower its metric so it is preferred.
    pub struct DnsGuard {
        tun: String,
    }

    fn netsh(args: &[&str]) -> bool {
        let mut c = Command::new("netsh");
        c.args(args);
        crate::engine::core::no_window(&mut c);
        c.status().map(|s| s.success()).unwrap_or(false)
    }

    impl DnsGuard {
        pub fn apply(servers: &[String], tun: &str) -> Result<Self> {
            // the adapter appears a moment after the core starts
            for _ in 0..40 {
                if if_stats(tun).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            let name = format!("name={tun}");
            for (i, s) in servers.iter().filter(|s| !s.contains(':')).enumerate() {
                let ok = if i == 0 {
                    netsh(&["interface", "ipv4", "set", "dnsservers", &name, "static", s, "primary", "validate=no"])
                } else {
                    netsh(&["interface", "ipv4", "add", "dnsservers", &name, s, &format!("index={}", i + 1), "validate=no"])
                };
                if !ok {
                    anyhow::bail!("netsh could not set DNS on {tun}");
                }
            }
            netsh(&["interface", "ipv4", "set", "interface", tun, "metric=1"]);
            Ok(DnsGuard { tun: tun.to_string() })
        }
        pub fn ensure(&self) -> bool {
            false
        }
        pub fn restore(&mut self) {
            let _ = &self.tun; // adapter disappears with the core; nothing to undo
        }
    }

    pub fn restore_dns() -> Result<bool> {
        Ok(false)
    }

    pub fn proc_cpu_rss(_pid: u32) -> Option<(u64, u64)> {
        None
    }
    pub fn tun_preflight(_own: &str, _others: &[&str]) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    pub fn clk_tck() -> f64 {
        100.0
    }
}

pub use imp::*;

// ---------------------------------------------------------------- probes

/// TCP to host:port, directly (through the TUN when it is up) or through
/// our local SOCKS5 proxy in proxy-only mode.
fn open(host: &str, port: u16, via: Option<SocketAddr>, timeout: Duration) -> Result<TcpStream> {
    if let Some(p) = via {
        return Ok(crate::engine::mixed::socks5_connect(p, host, port, timeout)?);
    }
    let addr = (host, port).to_socket_addrs()?.find(|a| a.is_ipv4()).context("no IPv4 address")?;
    Ok(TcpStream::connect_timeout(&addr, timeout)?)
}

fn http_get(host: &str, path: &str, via: Option<SocketAddr>, timeout: Duration) -> Result<(Duration, String)> {
    let t0 = Instant::now();
    let mut s = open(host, 80, via, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: CyberVPN\r\nConnection: close\r\n\r\n")?;
    let mut first = [0u8; 1];
    s.read_exact(&mut first)?;
    let ttfb = t0.elapsed();
    let mut rest = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut rest);
    let mut all = vec![first[0]];
    all.extend(rest);
    Ok((ttfb, String::from_utf8_lossy(&all).into_owned()))
}

/// Round trip through the tunnel (HTTP time-to-first-byte from 1.1.1.1).
pub fn latency_ms(via: Option<SocketAddr>) -> Option<u64> {
    http_get("1.1.1.1", "/", via, Duration::from_secs(5)).ok().map(|(d, _)| d.as_millis() as u64)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExitInfo {
    pub ip: String,
    pub country: String,
    pub city: String,
    pub cc: String,
}

/// Exit IP + location as the internet sees it.
pub fn public_ip(via: Option<SocketAddr>) -> Option<ExitInfo> {
    let (_, resp) = http_get("ip-api.com", "/json/?fields=query,country,city,countryCode", via, Duration::from_secs(8)).ok()?;
    let (_, body) = resp.split_once("\r\n\r\n")?;
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(&body[start..=end]).ok()?;
    let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let (ip, country, city, cc) = (g("query"), g("country"), g("city"), g("countryCode"));
    if ip.is_empty() {
        return None;
    }
    Some(ExitInfo { ip, country, city, cc })
}
