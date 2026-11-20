//! System tray: shield icon (colour = state) and quick menu.
//! The tray lives on its own thread with its own event pump (GTK on Linux,
//! Win32 messages on Windows); it receives updates through a channel and
//! reports clicks through tray-icon's global MenuEvent channel.

use crate::protocol::VpnState;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};

pub enum TrayCmd {
    Update { ru: bool, state: VpnState, profiles: Vec<String>, selected: usize, speed: String },
    Quit,
}

// ---------------------------------------------------------------- icon

pub fn icon(state: &VpnState, phase: f32) -> Icon {
    Icon::from_rgba(crate::icon::tray_rgba(state, 64, phase), 64, 64).expect("icon")
}

// ---------------------------------------------------------------- thread

pub fn spawn(rx: Receiver<TrayCmd>) {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = run(rx) {
                eprintln!("tray unavailable: {e}");
            }
        })
        .expect("tray thread");
}

fn pump() {
    #[cfg(target_os = "linux")]
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn run(rx: Receiver<TrayCmd>) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    gtk::init()?;

    let status = MenuItem::with_id("status", "CyberVPN — Not protected", false, None);
    let toggle = MenuItem::with_id("toggle", "Turn ON", true, None);
    let speed = MenuItem::with_id("speed", "", false, None);
    let profiles = Submenu::with_id("profiles", "Server", true);
    let open = MenuItem::with_id("open", "Open CyberVPN", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status,
        &speed,
        &PredefinedMenuItem::separator(),
        &toggle,
        &profiles,
        &PredefinedMenuItem::separator(),
        &open,
        &quit,
    ])?;

    let off = VpnState::Off;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("CyberVPN — Not protected")
        .with_icon(icon(&off, 0.0))
        .with_menu_on_left_click(cfg!(not(windows)))
        .build()?;

    let mut state = off;
    let mut prof_names: Vec<String> = vec![];
    let mut prof_items: Vec<CheckMenuItem> = vec![];
    let mut sel = usize::MAX;
    let mut phase = 0f32;
    let mut lang_ru = false;
    loop {
        pump();
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(TrayCmd::Quit) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(TrayCmd::Update { ru, state: st, profiles: names, selected, speed: sp }) => {
                use crate::i18n::tr;
                if ru != lang_ru {
                    lang_ru = ru;
                    open.set_text(tr("Open CyberVPN"));
                    quit.set_text(tr("Quit"));
                    profiles.set_text(tr("Server"));
                    state = VpnState::Reconnecting(u32::MAX); // force relabel below
                }
                if st != state {
                    let _ = tray.set_icon(Some(icon(&st, phase)));
                    let _ = tray.set_tooltip(Some(format!("CyberVPN — {}", st.label())));
                    status.set_text(format!("CyberVPN — {}", st.label()));
                    toggle.set_text(if st.is_on() { tr("Turn OFF") } else { tr("Turn ON") });
                    state = st;
                }
                speed.set_text(&sp);
                if names != prof_names || selected != sel {
                    for it in prof_items.drain(..) {
                        let _ = profiles.remove(&it);
                    }
                    for (i, n) in names.iter().enumerate() {
                        let it = CheckMenuItem::with_id(format!("p:{i}"), n, true, i == selected, None);
                        let _ = profiles.append(&it);
                        prof_items.push(it);
                    }
                    if names.is_empty() {
                        profiles.set_text(tr("Server (none — add one in the window)"));
                    } else {
                        profiles.set_text(tr("Server"));
                    }
                    prof_names = names;
                    sel = selected;
                }
            }
            Err(_) => {}
        }
        // gentle pulse while connecting
        if matches!(state, VpnState::Connecting | VpnState::Authorizing | VpnState::Reconnecting(_)) {
            phase = (phase + 0.04) % 1.0;
            if (phase * 100.0) as u32 % 5 == 0 {
                let _ = tray.set_icon(Some(icon(&state, phase)));
            }
        }
    }
    drop(tray);
    Ok(())
}
