//! Elevation, autostart, notifications, opening the window.

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// This program. After an update replaced the file, the running process
/// sees ".../cybervpn (deleted)": then start the new file at the same path.
fn exe() -> PathBuf {
    match std::env::current_exe() {
        Ok(p) if p.exists() => p,
        Ok(p) => {
            let s = p.to_string_lossy().trim_end_matches(" (deleted)").to_string();
            let p = PathBuf::from(s);
            if p.exists() { p } else { PathBuf::from("cybervpn") }
        }
        Err(_) => PathBuf::from("cybervpn"),
    }
}

/// stderr of background processes goes here (for troubleshooting)
fn log_file(name: &str) -> Stdio {
    let dir = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cybervpn");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::File::create(dir.join(name)).map(Stdio::from).unwrap_or_else(|_| Stdio::null())
}

// ---------------------------------------------------------------- elevation

/// Start `cybervpn --backend PORT TOKEN` as root / Administrator.
/// Returns the child when we can watch it (pkexec / direct).
pub fn spawn_backend(port: u16, token: &str) -> Result<Option<Child>> {
    let args = ["--backend".to_string(), port.to_string(), token.to_string()];
    if crate::engine::sys::is_elevated() {
        let mut c = Command::new(exe());
        c.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::engine::core::no_window(&mut c);
        return Ok(Some(c.spawn()?));
    }
    #[cfg(target_os = "linux")]
    {
        let c = Command::new("pkexec")
            .arg(exe())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match c {
            Ok(c) => Ok(Some(c)),
            Err(_) => bail!("pkexec not found — install polkit (sudo pacman -S polkit) or run CyberVPN as root"),
        }
    }
    #[cfg(windows)]
    {
        runas(&args.join(" "))?;
        Ok(None)
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UAC prompt: ShellExecuteEx with the "runas" verb, hidden window.
#[cfg(windows)]
fn runas(params: &str) -> Result<()> {
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let verb = wide("runas");
    let file = wide(&exe().to_string_lossy());
    let par = wide(params);
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = par.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            bail!("administrator permission was not granted");
        }
    }
    Ok(())
}

/// Linux: one password prompt to install a polkit rule; afterwards
/// connecting never asks again.
pub fn allow_without_password() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME"))?;
        let st = Command::new("pkexec").arg(exe()).args(["--install-polkit", &user]).status()?;
        if !st.success() {
            bail!("not allowed (password dialog cancelled?)");
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    bail!("on Windows, start CyberVPN as administrator (autostart does this) to avoid UAC prompts")
}

pub fn polkit_rule_installed() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new("/etc/polkit-1/rules.d/49-cybervpn.rules").exists()
}

// ---------------------------------------------------------------- autostart

pub fn set_autostart(on: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
            .join("autostart");
        let file = dir.join("cybervpn.desktop");
        if on {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                &file,
                format!(
                    "[Desktop Entry]\nType=Application\nName=CyberVPN\nComment=Hysteria2 VPN\n\
                     Exec=\"{}\" --service --autostart\nIcon=network-vpn\nTerminal=false\n\
                     X-GNOME-Autostart-enabled=true\n",
                    exe().display()
                ),
            )?;
        } else if file.exists() {
            std::fs::remove_file(file)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut c = Command::new("reg");
        if on {
            let val = format!("\"{}\" --service --autostart", exe().display());
            c.args(["add", key, "/v", "CyberVPN", "/t", "REG_SZ", "/d", &val, "/f"]);
        } else {
            c.args(["delete", key, "/v", "CyberVPN", "/f"]);
        }
        crate::engine::core::no_window(&mut c);
        let st = c.status()?;
        if on && !st.success() {
            bail!("could not write the autostart registry key");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- notifications

pub fn notify(summary: &str, body: &str) {
    let (s, b) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .appname("CyberVPN")
            .summary(&s)
            .body(&b)
            .icon("network-vpn")
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show();
    });
}

/// "VPN is off" reminder. On Linux it has a "Turn on" button.
pub fn remind(on_click: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("CyberVPN")
            .summary(crate::i18n::tr("You are not protected"))
            .body(crate::i18n::tr("CyberVPN is off. Turn it on to protect your connection."))
            .icon("network-vpn-disconnected")
            .timeout(notify_rust::Timeout::Milliseconds(15000));
        #[cfg(target_os = "linux")]
        {
            n.action("connect", crate::i18n::tr("Turn on"));
            if let Ok(h) = n.show() {
                h.wait_for_action(|a| {
                    if a == "connect" {
                        on_click();
                    }
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = n.show();
            let _ = on_click;
        }
    });
}

// ---------------------------------------------------------------- window

/// Start a GUI window process.
pub fn open_window() {
    let mut c = Command::new(exe());
    c.arg("--window");
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log_file("window.log"));
    let _ = c.spawn();
}

/// Start the tray service in the background (detached).
pub fn start_service() -> Result<()> {
    let mut c = Command::new(exe());
    c.args(["--service", "--background"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log_file("service.log"));
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    crate::engine::core::no_window(&mut c);
    c.spawn()?;
    Ok(())
}
