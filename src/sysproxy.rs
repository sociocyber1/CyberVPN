//! Desktop proxy setting for proxy-only mode (no admin rights needed).
//!
//! While connected, the desktop's "network proxy" points at CyberVPN's local
//! SOCKS5/HTTP port, so browsers and most apps go through the VPN without a
//! TUN device. The previous values are saved to a file first and put back
//! on disconnect — and on the next start if CyberVPN was killed, so a dead
//! proxy never blocks the internet.
//!
//! Linux: GNOME-style gsettings (GNOME, Cinnamon, MATE, Budgie, XFCE apps
//! that read them, Firefox / Chromium "use system proxy") and KDE's
//! kioslaverc. Windows: the Internet Settings proxy (all browsers).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One saved value: (backend, key, old value; None = was not set).
#[derive(Serialize, Deserialize, Default)]
struct Backup {
    values: Vec<(String, String, Option<String>)>,
}

pub struct SystemProxy {
    file: PathBuf,
}

impl Drop for SystemProxy {
    fn drop(&mut self) {
        restore(&self.file);
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    crate::engine::core::no_window(&mut c);
    let o = c.output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

impl SystemProxy {
    /// Point the desktop proxy at `addr`. Returns what was changed (for the
    /// log); Err if this desktop has no proxy setting we know.
    pub fn apply(addr: SocketAddr, backup_file: &Path) -> Result<(Self, Vec<String>)> {
        restore(backup_file); // leftovers of a crash first
        let mut b = Backup::default();
        let done = imp::apply(addr, &mut b);
        if done.is_empty() {
            anyhow::bail!("no desktop proxy setting found");
        }
        if let Some(d) = backup_file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(backup_file, serde_json::to_vec(&b)?)?;
        Ok((SystemProxy { file: backup_file.to_path_buf() }, done))
    }
}

/// Put back the saved desktop proxy settings (no-op without a backup).
pub fn restore(backup_file: &Path) -> bool {
    let Ok(raw) = std::fs::read(backup_file) else { return false };
    if let Ok(b) = serde_json::from_slice::<Backup>(&raw) {
        imp::restore(&b);
    }
    let _ = std::fs::remove_file(backup_file);
    true
}

// ---------------------------------------------------------------- Linux

#[cfg(not(windows))]
mod imp {
    use super::*;

    const G: &str = "org.gnome.system.proxy";
    const NO_PROXY: &str = "['localhost', '127.0.0.0/8', '::1', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '*.local']";

    fn gsettings_ok() -> bool {
        run("gsettings", &["list-keys", G]).is_some_and(|k| k.lines().any(|l| l == "mode"))
    }

    fn kde_tool(read: bool) -> Option<&'static str> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
        if !desktop.contains("KDE") {
            return None;
        }
        let names: &[&str] = if read { &["kreadconfig6", "kreadconfig5"] } else { &["kwriteconfig6", "kwriteconfig5"] };
        names.iter().copied().find(|n| run("which", &[n]).is_some())
    }

    pub fn apply(addr: SocketAddr, b: &mut Backup) -> Vec<String> {
        let (host, port) = (addr.ip().to_string(), addr.port().to_string());
        let mut done = vec![];
        if gsettings_ok() {
            let keys: [(&str, &str, String); 8] = [
                (G, "ignore-hosts", NO_PROXY.into()),
                ("org.gnome.system.proxy.http", "host", host.clone()),
                ("org.gnome.system.proxy.http", "port", port.clone()),
                ("org.gnome.system.proxy.https", "host", host.clone()),
                ("org.gnome.system.proxy.https", "port", port.clone()),
                ("org.gnome.system.proxy.socks", "host", host.clone()),
                ("org.gnome.system.proxy.socks", "port", port.clone()),
                (G, "mode", "manual".into()),
            ];
            for (schema, key, val) in keys {
                let old = run("gsettings", &["get", schema, key]);
                b.values.push(("gsettings".into(), format!("{schema} {key}"), old));
                run("gsettings", &["set", schema, key, &val]);
            }
            done.push("desktop proxy (gsettings)".to_string());
        }
        if let (Some(r), Some(w)) = (kde_tool(true), kde_tool(false)) {
            let vals = [
                ("ProxyType", "1".to_string()),
                ("httpProxy", format!("http://{host} {port}")),
                ("httpsProxy", format!("http://{host} {port}")),
                ("socksProxy", format!("socks://{host} {port}")),
                ("NoProxyFor", "localhost,127.0.0.0/8,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16".to_string()),
            ];
            for (key, val) in vals {
                let old = run(r, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key]).filter(|v| !v.is_empty());
                b.values.push(("kde".into(), key.into(), old));
                run(w, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key, &val]);
            }
            kde_reload();
            done.push("KDE proxy".to_string());
        }
        done
    }

    fn kde_reload() {
        run("dbus-send", &["--type=signal", "/KIO/Scheduler", "org.kde.KIO.Scheduler.reparseSlaveConfiguration", "string:"]);
    }

    pub fn restore(b: &Backup) {
        let mut kde = false;
        // mode first, so apps stop using the proxy before its address changes
        for (tool, key, old) in b.values.iter().rev() {
            match tool.as_str() {
                "gsettings" => {
                    let Some((schema, k)) = key.split_once(' ') else { continue };
                    match old {
                        Some(v) => { run("gsettings", &["set", schema, k, v]); }
                        None => { run("gsettings", &["reset", schema, k]); }
                    }
                }
                "kde" => {
                    if let Some(w) = kde_tool(false) {
                        kde = true;
                        match old {
                            Some(v) => { run(w, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key, v]); }
                            None => { run(w, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key, "--delete"]); }
                        }
                    }
                }
                _ => {}
            }
        }
        if kde {
            kde_reload();
        }
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::*;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    fn get(name: &str) -> Option<String> {
        let out = run("reg", &["query", KEY, "/v", name])?;
        let line = out.lines().find(|l| l.trim_start().starts_with(name))?;
        let mut parts = line.split_whitespace();
        parts.next()?; // name
        let ty = parts.next()?;
        let val = parts.collect::<Vec<_>>().join(" ");
        Some(format!("{ty}|{val}"))
    }

    fn set(name: &str, ty: &str, val: &str) {
        run("reg", &["add", KEY, "/v", name, "/t", ty, "/d", val, "/f"]);
    }

    fn refresh() {
        use windows_sys::Win32::Networking::WinInet::{InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED};
        unsafe {
            InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
            InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
        }
    }

    pub fn apply(addr: SocketAddr, b: &mut Backup) -> Vec<String> {
        for name in ["ProxyEnable", "ProxyServer", "ProxyOverride"] {
            b.values.push(("reg".into(), name.into(), get(name)));
        }
        set("ProxyServer", "REG_SZ", &addr.to_string());
        set("ProxyOverride", "REG_SZ", "localhost;127.*;10.*;172.16.*;192.168.*;<local>");
        set("ProxyEnable", "REG_DWORD", "1");
        refresh();
        vec!["Windows proxy".to_string()]
    }

    pub fn restore(b: &Backup) {
        for (_, name, old) in &b.values {
            match old.as_deref().and_then(|v| v.split_once('|')) {
                Some((ty, val)) => set(name, ty, val),
                None => { run("reg", &["delete", KEY, "/v", name, "/f"]); }
            }
        }
        refresh();
    }
}
