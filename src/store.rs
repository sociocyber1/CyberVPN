//! Profiles + settings (config.toml). Only the tray service writes it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_mbps: Option<u32>,
    /// from the free SocioCyberneering list (replaced when the list changes)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free: bool,
    /// the list marks this key as taken (someone is using it)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UiMode { #[default] Simple, Pro }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Theme { #[default] System, Light, Dark }

/// GNOME accent colors; System = follow the desktop setting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Accent { #[default] System, Blue, Teal, Green, Yellow, Orange, Red, Pink, Purple, Slate }

/// How the VPN is applied.
/// Auto: whole system when that needs no password (admin, or "Allow without
/// password" was set up once), otherwise proxy only — never asks for root.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum VpnMode { #[default] Auto, System, Proxy }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Language { #[default] Auto, En, Ru }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    // ---- app
    pub mode: UiMode,
    pub theme: Theme,
    pub accent: Accent,
    pub language: Language,
    /// planet / network animations (off = calm, less CPU)
    pub animations: bool,
    pub autostart: bool,
    /// When started at login: stay in the tray (true) or open the window.
    pub start_minimized: bool,
    pub connect_on_start: bool,
    pub notifications: bool,
    /// Remind every N minutes while the VPN is off (0 = never).
    pub reminder_minutes: u32,
    /// Download new VPN engine releases in the background.
    pub auto_update_engine: bool,
    /// Notify when a new engine was downloaded / installed.
    pub notify_engine_update: bool,
    // ---- vpn
    pub vpn_mode: VpnMode,
    /// kill switch: while CyberVPN runs, block the internet unless the VPN is on
    pub kill_switch: bool,
    /// streamer mode: IPs, servers, domains are shown as shifting glyphs
    pub streamer: bool,
    /// new defaults applied once (whole system, autostart + connect, kill switch)
    pub defaults_v2: bool,
    /// local SOCKS5 + HTTP proxy port (both on one port), e.g. 127.0.0.1:2080
    pub proxy: String,
    /// proxy mode: point the desktop's proxy setting at it while connected
    pub system_proxy: bool,
    /// refresh the free SocioCyberneering servers at every start
    pub update_free_servers: bool,
    pub override_dns: bool,
    pub dns: Vec<String>,
    pub bypass_lan: bool,
    pub tun_name: String,
    pub mtu: u32,
    pub tune_sysctl: bool,
    pub fast_open: bool,
    pub auto_reconnect: bool,
    pub exclude: Vec<String>,
    /// notToVPN: domains/zones that bypass the tunnel. Off by default, Pro only.
    pub not_to_vpn: bool,
    pub not_to_vpn_domains: Vec<String>,
    /// DNS used for notToVPN names (reached outside the tunnel)
    pub direct_dns: Vec<String>,
    /// IPv6 through the tunnel: "auto" (hide IPv6 if the server has none), "on", "off"
    pub ipv6: String,
    pub core_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: UiMode::Simple,
            theme: Theme::System,
            accent: Accent::System,
            language: Language::Auto,
            animations: true,
            autostart: true,
            start_minimized: true,
            connect_on_start: true,
            notifications: true,
            reminder_minutes: 30,
            auto_update_engine: true,
            notify_engine_update: true,
            vpn_mode: VpnMode::System,
            kill_switch: true,
            streamer: false,
            defaults_v2: true,
            proxy: "127.0.0.1:2080".into(),
            system_proxy: true,
            update_free_servers: true,
            override_dns: true,
            dns: vec!["1.1.1.1".into(), "8.8.8.8".into()],
            bypass_lan: true,
            tun_name: if cfg!(windows) { "CyberVPN".into() } else { "cybervpn".into() },
            mtu: 1500,
            tune_sysctl: true,
            fast_open: true,
            auto_reconnect: true,
            exclude: vec![],
            not_to_vpn: false,
            not_to_vpn_domains: ["ru", "рф", "su", "by", "kz", "ua"].iter().map(|s| s.to_string()).collect(),
            direct_dns: vec!["77.88.8.8".into(), "77.88.8.1".into()],
            ipv6: "auto".into(),
            core_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Store {
    /// the free SocioCyberneering servers were fetched once (first start)
    pub free_servers_done: bool,
    pub selected: usize,
    pub settings: Settings,
    pub profiles: Vec<Profile>,
}

pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn get() -> Self {
        #[cfg(windows)]
        {
            let app = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| app.clone());
            Paths { config_dir: app.join("CyberVPN"), data_dir: local.join("CyberVPN") }
        }
        #[cfg(not(windows))]
        {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
            let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
            let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
            Paths { config_dir: cfg.join("cybervpn"), data_dir: data.join("cybervpn") }
        }
    }
    pub fn config_file(&self) -> PathBuf { self.config_dir.join("config.toml") }
    pub fn ipc_file(&self) -> PathBuf { self.config_dir.join("ipc.json") }
    pub fn bin_dir(&self) -> PathBuf { self.data_dir.join("bin") }
    /// desktop proxy settings saved before CyberVPN changed them
    pub fn sysproxy_backup(&self) -> PathBuf { self.config_dir.join("system-proxy.backup.json") }
    /// per-user runtime files (engine config in proxy mode)
    pub fn run_dir(&self) -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("cybervpn")
    }
}

fn private_dir(d: &std::path::Path) -> Result<()> {
    fs::create_dir_all(d)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(d, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Write a file only the current user can read (passwords, IPC token).
pub fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        private_dir(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

impl Store {
    pub fn load(p: &Paths) -> Result<Self> {
        match fs::read_to_string(p.config_file()) {
            Ok(s) => {
                let mut st: Store = toml::from_str(&s).with_context(|| format!("cannot parse {}", p.config_file().display()))?;
                if !st.settings.defaults_v2 {
                    // once: whole-system VPN, start with the computer and connect, kill switch
                    let x = &mut st.settings;
                    x.vpn_mode = VpnMode::System;
                    x.autostart = true;
                    x.connect_on_start = true;
                    x.kill_switch = true;
                    x.defaults_v2 = true;
                }
                Ok(st)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, p: &Paths) -> Result<()> {
        let text = format!("# CyberVPN config (written by the app)\n\n{}", toml::to_string_pretty(self)?);
        write_private(&p.config_file(), text.as_bytes())
    }

    /// Add every hy2:// link in `text`. Returns (added names, errors).
    /// Sync the free SocioCyberneering keys with the downloaded list: new keys
    /// are added, keys gone from the list (dead server, re-made keys) removed,
    /// "busy" marks updated. Your own servers are never touched.
    /// Returns (added, removed, selection changed).
    pub fn sync_free(&mut self, text: &str) -> (usize, usize, bool) {
        let list = crate::freeservers::parse(text);
        if list.is_empty() {
            return (0, 0, false);
        }
        let cur_uri = self.profiles.get(self.selected).map(|p| p.uri.clone());
        let before = self.profiles.len();
        self.profiles.retain(|p| !p.free || list.iter().any(|(u, _)| *u == p.uri));
        let removed = before - self.profiles.len();
        let mut added = 0;
        for (uri, busy) in &list {
            if let Some(p) = self.profiles.iter_mut().find(|p| &p.uri == uri) {
                p.busy = *busy;
                continue;
            }
            if let Ok(u) = crate::engine::uri::Hy2Uri::parse(uri) {
                self.profiles.push(Profile { name: u.name, uri: uri.clone(), up_mbps: None, down_mbps: None, free: true, busy: *busy });
                added += 1;
            }
        }
        // keep the chosen server if it still exists and is free to use
        let keep = cur_uri.as_ref().and_then(|u| self.profiles.iter().position(|p| &p.uri == u));
        let changed = match keep {
            Some(i) if !self.profiles[i].busy || !self.profiles[i].free => {
                self.selected = i;
                false
            }
            _ => {
                let free: Vec<usize> = self.profiles.iter().enumerate().filter(|(_, p)| p.free && !p.busy).map(|(i, _)| i).collect();
                let pool = if free.is_empty() { (0..self.profiles.len()).collect() } else { free };
                if !pool.is_empty() {
                    let r = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize ^ std::process::id() as usize).unwrap_or(0);
                    self.selected = pool[r % pool.len()];
                }
                true
            }
        };
        (added, removed, changed)
    }

    pub fn add_from_text(&mut self, text: &str) -> (Vec<String>, Vec<String>) {
        let mut ok = vec![];
        let mut bad = vec![];
        for tok in text.split_whitespace() {
            let l = tok.to_ascii_lowercase();
            if !(l.starts_with("hy2://") || l.starts_with("hysteria2://")) {
                continue;
            }
            match crate::engine::uri::Hy2Uri::parse(tok) {
                Ok(u) => {
                    if let Some(i) = self.profiles.iter().position(|p| p.uri == tok) {
                        self.selected = i;
                        ok.push(format!("{} (already added)", self.profiles[i].name));
                    } else {
                        self.profiles.push(Profile { name: u.name.clone(), uri: tok.to_string(), up_mbps: None, down_mbps: None, free: false, busy: false });
                        self.selected = self.profiles.len() - 1;
                        ok.push(u.name);
                    }
                }
                Err(e) => bad.push(e.to_string()),
            }
        }
        (ok, bad)
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn free_keys_follow_the_list() {
        let mut s = Store::default();
        s.add_from_text("hy2://mine@my.host:443#Mine");
        let (a, r, _) = s.sync_free("hy2://k1@h:1#s-1\nhy2://k2@h:1#s-2 # busy\n");
        assert_eq!((a, r), (2, 0));
        assert!(s.profiles[2].busy);
        assert!(!s.profiles[s.selected].busy);
        // server died, keys re-made: k1 and k2 gone, k3 new; my own server stays
        s.selected = 1; // was on k1
        let (a, r, changed) = s.sync_free("hy2://k3@h:1#s-3\n");
        assert_eq!((a, r), (1, 2));
        assert!(changed);
        assert_eq!(s.profiles.len(), 2);
        assert_eq!(s.profiles[0].name, "Mine");
        assert!(s.profiles.iter().any(|p| p.uri.contains("k3")));
    }
}
