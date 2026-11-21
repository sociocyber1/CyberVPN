//! Kill switch (Linux): while CyberVPN runs, nothing may leave the computer
//! except through the VPN. iptables/ip6tables chain CYBERVPN_KS hooked into
//! OUTPUT; allowed: loopback, the TUN device, the local network (router,
//! printers, DHCP), the VPN servers' addresses, and DNS from root (so the
//! backend can find the server). Everything else is rejected at once, so
//! apps fail fast instead of hanging — and your real IP never leaks.
//!
//! The rules live in the kernel until removed: on Quit, by turning the kill
//! switch off, or with `sudo cybervpn --killswitch off`.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::net::IpAddr;
use std::process::{Command, Stdio};

const CHAIN: &str = "CYBERVPN_KS";
const LAN4: &[&str] = &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16", "224.0.0.0/4", "255.255.255.255/32"];
const LAN6: &[&str] = &["fe80::/10", "fc00::/7", "ff00::/8"];

fn run(tool: &str, args: &[&str]) -> bool {
    Command::new(tool).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn tools() -> [(&'static str, bool); 2] {
    [("iptables", false), ("ip6tables", true)]
}

pub fn available() -> bool {
    run("iptables", &["-w", "-S", "OUTPUT"])
}

/// Is the kill switch in place?
pub fn active() -> bool {
    run("iptables", &["-w", "-C", "OUTPUT", "-j", CHAIN])
}

/// Block everything except the VPN. `hosts`: VPN server names/IPs to allow.
pub fn on(tun: &str, lan: bool, hosts: &[String]) -> Result<Vec<IpAddr>, String> {
    if !available() {
        return Err("iptables not found (install iptables-nft)".into());
    }
    // resolve the servers first, while DNS still works
    let mut ips: Vec<IpAddr> = vec![];
    for h in hosts {
        if let Ok(v) = crate::engine::sys::resolve(h) {
            ips.extend(v);
        }
    }
    ips.sort();
    ips.dedup();
    for (t, v6) in tools() {
        run(t, &["-w", "-N", CHAIN]);
        run(t, &["-w", "-F", CHAIN]);
        let a = |args: &[&str]| {
            let mut v = vec!["-w", "-A", CHAIN];
            v.extend_from_slice(args);
            run(t, &v)
        };
        a(&["-o", "lo", "-j", "ACCEPT"]);
        a(&["-o", tun, "-j", "ACCEPT"]);
        if lan {
            for n in if v6 { LAN6 } else { LAN4 } {
                a(&["-d", n, "-j", "ACCEPT"]);
            }
        }
        if !v6 {
            a(&["-p", "udp", "--dport", "67:68", "-j", "ACCEPT"]); // DHCP
        } else {
            a(&["-p", "ipv6-icmp", "-j", "ACCEPT"]); // neighbour discovery
        }
        for proto in ["udp", "tcp"] {
            a(&["-p", proto, "--dport", "53", "-m", "owner", "--uid-owner", "0", "-j", "ACCEPT"]);
        }
        for ip in ips.iter().filter(|i| i.is_ipv6() == v6) {
            a(&["-d", &ip.to_string(), "-j", "ACCEPT"]);
        }
        a(&["-j", "REJECT"]);
        if !run(t, &["-w", "-C", "OUTPUT", "-j", CHAIN]) {
            run(t, &["-w", "-I", "OUTPUT", "1", "-j", CHAIN]);
        }
    }
    if !active() {
        return Err("could not install the firewall rules".into());
    }
    Ok(ips)
}

/// Let these addresses through too (new server, notToVPN sites).
pub fn allow(ips: &[IpAddr]) {
    if !active() {
        return;
    }
    for ip in ips {
        let t = if ip.is_ipv6() { "ip6tables" } else { "iptables" };
        let s = ip.to_string();
        if !run(t, &["-w", "-C", CHAIN, "-d", &s, "-j", "ACCEPT"]) {
            run(t, &["-w", "-I", CHAIN, "1", "-d", &s, "-j", "ACCEPT"]);
        }
    }
}

/// Remove the kill switch completely.
pub fn off() {
    for (t, _) in tools() {
        while run(t, &["-w", "-D", "OUTPUT", "-j", CHAIN]) {}
        run(t, &["-w", "-F", CHAIN]);
        run(t, &["-w", "-X", CHAIN]);
    }
}
