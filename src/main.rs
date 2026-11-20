//! CyberVPN — desktop Hysteria2 VPN.
//!
//!   cybervpn                 open the window (starts the tray service if needed)
//!   cybervpn --service       tray service (used by autostart)
//!   cybervpn on|off|toggle   control the VPN from scripts / hotkeys
//!   cybervpn add LINK        add a hy2:// server
//!   cybervpn install-core    download the hysteria engine
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod backend;
mod engine;
mod gui;
mod i18n;
mod icon;
mod ipc;
mod platform;
mod protocol;
mod service;
mod store;
mod tray;

use protocol::Command;
use store::{Paths, Store};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = |i: usize| args.get(i).map(String::as_str);
    let res: anyhow::Result<()> = match a(0) {
        Some("--backend") => {
            let port = a(1).and_then(|p| p.parse().ok()).unwrap_or(0);
            backend::run(port, a(2).unwrap_or(""))
        }
        #[cfg(target_os = "linux")]
        Some("--install-polkit") => backend::install_polkit_rule(a(1).unwrap_or("")),
        Some("--service") => service::run(args.iter().any(|x| x == "--autostart"), args.iter().any(|x| x == "--background")),
        Some(cmd @ ("on" | "off" | "toggle" | "quit" | "update-engine")) => {
            let c = match cmd {
                "update-engine" => Command::CheckEngineUpdate,
                "on" => Command::Connect(None),
                "off" => Command::Disconnect,
                "quit" => Command::QuitApp,
                _ => Command::Toggle,
            };
            let paths = Paths::get();
            if service::running_instance(&paths).is_none() && cmd != "off" && cmd != "quit" && cmd != "update-engine" {
                let _ = platform::start_service();
                std::thread::sleep(std::time::Duration::from_millis(1500));
            }
            match service::running_instance(&paths) {
                Some(info) => service::send_command(&info, &c),
                None => Err(anyhow::anyhow!("CyberVPN is not running")),
            }
        }
        Some("add") => {
            let text = args[1..].join(" ");
            let paths = Paths::get();
            if let Some(info) = service::running_instance(&paths) {
                service::send_command(&info, &Command::AddLinks(text))
            } else {
                let mut s = Store::load(&paths).unwrap_or_default();
                let (ok, bad) = s.add_from_text(&text);
                for n in &ok { println!("added: {n}"); }
                for e in &bad { eprintln!("skipped: {e}"); }
                s.save(&paths)
            }
        }
        Some("install-core") => {
            let paths = Paths::get();
            let mut o = engine::core::InstallOpts::default();
            let mut it = args[1..].iter();
            while let Some(x) = it.next() {
                match x.as_str() {
                    "-j" | "--jobs" => o.jobs = it.next().and_then(|v| v.parse().ok()).unwrap_or(8),
                    "--proxy" => o.proxy = it.next().cloned(),
                    "--force" | "-f" => o.force = true,
                    _ => {}
                }
            }
            engine::core::install_core(&paths.bin_dir(), &o, None).map(|_| ())
        }
        Some("-h") | Some("--help") | Some("help") => {
            println!("CyberVPN {}\n\n  cybervpn                 open the window\n  cybervpn on|off|toggle   control the VPN\n  cybervpn add LINK        add a hy2:// server\n  cybervpn update-engine    check for a newer VPN engine now\n  cybervpn install-core [-j N] [--proxy URL]\n  cybervpn quit            stop the tray app", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // started by the tray service: always open a window
        Some("--window") => gui::run(),
        // started by the user: if CyberVPN is already open, just bring it up
        _ => {
            let paths = Paths::get();
            match service::running_instance(&paths) {
                Some(info) if service::send_command(&info, &Command::OpenWindow).is_ok() => Ok(()),
                _ => gui::run(),
            }
        }
    };
    if let Err(e) = res {
        eprintln!("CyberVPN: {e:#}");
        std::process::exit(1);
    }
}
