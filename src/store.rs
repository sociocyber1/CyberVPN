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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UiMode { #[default] Simple, Pro }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Theme { #[default] System, Light, Dark }

/// GNOME accent colors; System = follow the desktop setting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Accent { #[default] System, Blue, Teal, Green, Yellow, Orange, Red, Pink, Purple, Slate }

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
            autostart: false,
            start_minimized: true,
            connect_on_start: false,
            notifications: true,
            reminder_minutes: 30,
            auto_update_engine: true,
            notify_engine_update: true,
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
            core_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Store {
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
            Ok(s) => toml::from_str(&s).with_context(|| format!("cannot parse {}", p.config_file().display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, p: &Paths) -> Result<()> {
        let text = format!("# CyberVPN config (written by the app)\n\n{}", toml::to_string_pretty(self)?);
        write_private(&p.config_file(), text.as_bytes())
    }

    /// Add every hy2:// link in `text`. Returns (added names, errors).
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
                        self.profiles.push(Profile { name: u.name.clone(), uri: tok.to_string(), up_mbps: None, down_mbps: None });
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
