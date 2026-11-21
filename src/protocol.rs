//! Messages between the three processes:
//!   window (GUI)  ⇄  tray service  ⇄  backend (elevated: core, routes, DNS)
//! Transport: newline-delimited JSON over 127.0.0.1 TCP, first line = token.

use crate::engine::core::LogLine;
use crate::engine::dns::DnsEvent;
use crate::engine::sys::ExitInfo;
use crate::store::{Profile, Settings, Store};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------- backend

#[derive(Serialize, Deserialize, Clone)]
pub struct BackendStart {
    pub profile: Profile,
    pub settings: Settings,
    pub core_path: PathBuf,
    /// whole-system TUN (elevated backend) or proxy only (runs as the user)
    #[serde(default = "yes")]
    pub tun: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase { #[default] Connecting, Connected, Reconnecting(u32) }

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct ConnView {
    pub time: String,
    pub tcp: bool,
    pub app: String,
    pub dst: String,
    pub domain: String,
    pub open: bool,
    pub error: String,
    pub dur_s: u64,
    /// "tun", "socks" or "http"
    #[serde(default)]
    pub via: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Tick {
    pub phase: Phase,
    /// proxy-only mode (no TUN, no admin)
    #[serde(default)]
    pub proxy_mode: bool,
    /// local SOCKS5/HTTP port ("" = off)
    #[serde(default)]
    pub proxy_addr: String,
    /// the desktop proxy setting points at us
    #[serde(default)]
    pub system_proxy: bool,
    pub server_ip: String,
    pub down_bps: f64,
    pub up_bps: f64,
    pub rx_pps: u64,
    pub tx_pps: u64,
    pub down_total: u64,
    pub up_total: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub if_errors: u64,
    pub if_dropped: u64,
    pub latency_ms: Option<u64>,
    pub exit: Option<ExitInfo>,
    pub cpu: f64,
    pub rss_kb: u64,
    pub reconnects: u64,
    pub restarts: u32,
    pub open_tcp: usize,
    pub open_udp: usize,
    pub total_tcp: u64,
    pub total_udp: u64,
    pub conn_errors: u64,
    pub dns_queries: u64,
    pub dns_fail: u64,
    pub dns_avg_ms: u64,
    /// IPs routed outside the tunnel by notToVPN
    #[serde(default)]
    pub direct_ips: usize,
    /// IPv6 answers allowed by the DNS forwarder (None = no forwarder)
    #[serde(default)]
    pub ipv6: Option<bool>,
    pub conns: Vec<ConnView>,
    pub dns: Vec<DnsEvent>,
    pub top: Vec<(String, u64)>,
    /// log lines produced since the previous tick
    pub logs: Vec<LogLine>,
}

#[derive(Serialize, Deserialize)]
pub enum BackendMsg {
    Tick(Box<Tick>),
    Fatal(String),
}

// ---------------------------------------------------------------- GUI

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub enum VpnState {
    #[default]
    Off,
    /// waiting for the admin password / UAC prompt
    Authorizing,
    Connecting,
    Connected,
    Reconnecting(u32),
    Error(String),
}

impl VpnState {
    pub fn is_on(&self) -> bool { !matches!(self, VpnState::Off | VpnState::Error(_)) }
    pub fn label(&self) -> String {
        use crate::i18n::{tr, trf};
        match self {
            VpnState::Off => tr("Not protected").into(),
            VpnState::Authorizing => tr("Waiting for permission…").into(),
            VpnState::Connecting => tr("Connecting…").into(),
            VpnState::Connected => tr("Protected").into(),
            VpnState::Reconnecting(n) => trf("Reconnecting… ({})", &[n]),
            VpnState::Error(e) => trf("Error: {}", &[e]),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct InstallState {
    pub running: bool,
    pub have: u64,
    pub total: u64,
    pub speed: u64,
    pub error: Option<String>,
}

/// VPN engine (hysteria core) status for the settings page.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct EngineInfo {
    pub embedded: Option<String>,
    /// downloaded, used on the next connect / start
    pub staged: Option<String>,
    pub checking: bool,
    pub status: String,
    pub download: InstallState,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Snapshot {
    pub state: VpnState,
    pub store: Store,
    /// index of the profile in use
    pub active: Option<usize>,
    pub connected_s: u64,
    pub tick: Option<Tick>,
    pub down_hist: Vec<f64>,
    pub up_hist: Vec<f64>,
    pub lat_hist: Vec<u64>,
    pub logs: Vec<LogLine>,
    pub core_version: Option<String>,
    pub install: InstallState,
    pub engine: EngineInfo,
    /// (id, text, is_error) — id changes for every new message
    pub toast: Option<(u64, String, bool)>,
    pub can_skip_password: bool,
    pub platform_note: String,
    /// the next "Turn on" covers the whole system (TUN); false = proxy only
    #[serde(default)]
    pub will_use_tun: bool,
    /// the kill switch blocks the internet right now (unless the VPN is on)
    #[serde(default)]
    pub kill_switch_on: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Command {
    Connect(Option<usize>),
    Disconnect,
    Toggle,
    Select(usize),
    AddLinks(String),
    Delete(usize),
    Rename(usize, String),
    Bandwidth(usize, Option<u32>, Option<u32>),
    SaveSettings(Settings),
    InstallCore,
    /// look for a newer engine now
    CheckEngineUpdate,
    /// download the free SocioCyberneering server list
    FetchFreeServers,
    /// Linux: install a polkit rule so connecting does not ask for a password
    AllowWithoutPassword,
    OpenWindow,
    QuitApp,
}

#[derive(Serialize, Deserialize)]
pub enum ToGui {
    Snapshot(Box<Snapshot>),
    Focus,
    Bye,
}
