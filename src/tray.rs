//! System tray: the CyberVPN globe (badge colour = state) and a quick menu.
//!
//! Linux: a StatusNotifierItem spoken directly over D-Bus (pure Rust, `ksni`)
//! — no GTK, no libappindicator, no libxdo. Works in XFCE, KDE, Cinnamon,
//! MATE, LXQt, Budgie, waybar, and GNOME with the AppIndicator extension.
//! Windows: the native notification-area icon (`tray-icon`).
//!
//! The tray runs on its own thread, receives `TrayCmd` updates and reports
//! clicks as `TrayEvent`s.

use crate::protocol::VpnState;
use std::sync::mpsc::{channel, Receiver, Sender};

pub enum TrayCmd {
    Update { ru: bool, state: VpnState, profiles: Vec<String>, selected: usize, speed: String },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrayEvent {
    Toggle,
    Open,
    Quit,
    Select(usize),
}

/// Start the tray; returns the channel with its clicks.
pub fn spawn(rx: Receiver<TrayCmd>) -> Receiver<TrayEvent> {
    let (tx, events) = channel();
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = imp::run(rx, tx) {
                eprintln!("tray unavailable: {e}");
            }
        })
        .expect("tray thread");
    events
}

fn pulsing(s: &VpnState) -> bool {
    matches!(s, VpnState::Connecting | VpnState::Authorizing | VpnState::Reconnecting(_))
}

// ---------------------------------------------------------------- Linux

#[cfg(not(windows))]
mod imp {
    use super::*;
    use crate::i18n::tr;
    use ksni::blocking::TrayMethods;
    use std::time::Duration;

    struct Tray {
        tx: Sender<TrayEvent>,
        state: VpnState,
        names: Vec<String>,
        sel: usize,
        speed: String,
        icon: Vec<ksni::Icon>,
    }

    /// RGBA → ARGB32 (network byte order), as the SNI spec wants.
    fn pixmap(state: &VpnState, phase: f32) -> Vec<ksni::Icon> {
        [24u32, 48, 64]
            .iter()
            .map(|&n| {
                let rgba = crate::icon::tray_rgba(state, n, phase);
                let mut data = Vec::with_capacity(rgba.len());
                for p in rgba.chunks_exact(4) {
                    data.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
                }
                ksni::Icon { width: n as i32, height: n as i32, data }
            })
            .collect()
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "cybervpn".into()
        }
        fn title(&self) -> String {
            format!("CyberVPN — {}", self.state.label())
        }
        fn category(&self) -> ksni::Category {
            ksni::Category::Communications
        }
        fn status(&self) -> ksni::Status {
            ksni::Status::Active
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: format!("CyberVPN — {}", self.state.label()), description: self.speed.clone(), ..Default::default() }
        }
        /// left click: open the window
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(TrayEvent::Open);
        }
        /// middle click: quick on/off
        fn secondary_activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(TrayEvent::Toggle);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;
            let mut m: Vec<ksni::MenuItem<Self>> = vec![StandardItem {
                label: format!("CyberVPN — {}", self.state.label()),
                enabled: false,
                ..Default::default()
            }
            .into()];
            if !self.speed.is_empty() {
                m.push(StandardItem { label: self.speed.clone(), enabled: false, ..Default::default() }.into());
            }
            m.push(MenuItem::Separator);
            m.push(
                StandardItem {
                    label: if self.state.is_on() { tr("Turn OFF") } else { tr("Turn ON") }.into(),
                    icon_name: if self.state.is_on() { "network-vpn-disconnected" } else { "network-vpn" }.into(),
                    activate: Box::new(|t: &mut Self| { let _ = t.tx.send(TrayEvent::Toggle); }),
                    ..Default::default()
                }
                .into(),
            );
            if self.names.is_empty() {
                m.push(StandardItem { label: tr("Server (none — add one in the window)").into(), enabled: false, ..Default::default() }.into());
            } else {
                m.push(
                    SubMenu {
                        label: tr("Server").into(),
                        submenu: vec![RadioGroup {
                            selected: self.sel.min(self.names.len() - 1),
                            select: Box::new(|t: &mut Self, i| { let _ = t.tx.send(TrayEvent::Select(i)); }),
                            options: self.names.iter().map(|n| RadioItem { label: n.clone(), ..Default::default() }).collect(),
                        }
                        .into()],
                        ..Default::default()
                    }
                    .into(),
                );
            }
            m.push(MenuItem::Separator);
            m.push(
                StandardItem {
                    label: tr("Open CyberVPN").into(),
                    activate: Box::new(|t: &mut Self| { let _ = t.tx.send(TrayEvent::Open); }),
                    ..Default::default()
                }
                .into(),
            );
            m.push(
                StandardItem {
                    label: tr("Quit").into(),
                    icon_name: "application-exit".into(),
                    activate: Box::new(|t: &mut Self| { let _ = t.tx.send(TrayEvent::Quit); }),
                    ..Default::default()
                }
                .into(),
            );
            m
        }
    }

    pub fn run(rx: Receiver<TrayCmd>, tx: Sender<TrayEvent>) -> anyhow::Result<()> {
        let off = VpnState::Off;
        let tray = Tray { tx, icon: pixmap(&off, 0.0), state: off, names: vec![], sel: 0, speed: String::new() };
        // keep waiting if the panel starts after us (autostart at login)
        let handle = tray.assume_sni_available(true).spawn().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut phase = 0f32;
        let mut last_ru = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(120)) {
                Ok(TrayCmd::Quit) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(TrayCmd::Update { ru, state, profiles, selected, speed }) => {
                    let relabel = last_ru != Some(ru);
                    last_ru = Some(ru);
                    handle.update(|t| {
                        if t.state != state || relabel {
                            t.icon = pixmap(&state, phase);
                            t.state = state;
                        }
                        t.names = profiles;
                        t.sel = selected;
                        t.speed = speed;
                    });
                }
                Err(_) => {}
            }
            // gentle pulse while connecting
            if handle.update(|t| pulsing(&t.state)).unwrap_or(false) {
                phase = (phase + 0.08) % 1.0;
                handle.update(|t| t.icon = pixmap(&t.state, phase));
            }
        }
        handle.shutdown().wait();
        Ok(())
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::Duration;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    fn icon(state: &VpnState, phase: f32) -> Icon {
        Icon::from_rgba(crate::icon::tray_rgba(state, 64, phase), 64, 64).expect("icon")
    }

    fn pump() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    pub fn run(rx: Receiver<TrayCmd>, tx: Sender<TrayEvent>) -> anyhow::Result<()> {
        use crate::i18n::tr;
        let status = MenuItem::with_id("status", "CyberVPN — Not protected", false, None);
        let toggle = MenuItem::with_id("toggle", "Turn ON", true, None);
        let speed = MenuItem::with_id("speed", "", false, None);
        let profiles = Submenu::with_id("profiles", "Server", true);
        let open = MenuItem::with_id("open", "Open CyberVPN", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&status, &speed, &PredefinedMenuItem::separator(), &toggle, &profiles, &PredefinedMenuItem::separator(), &open, &quit])?;
        let off = VpnState::Off;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("CyberVPN — Not protected")
            .with_icon(icon(&off, 0.0))
            .with_menu_on_left_click(false)
            .build()?;

        let mut state = off;
        let mut prof_names: Vec<String> = vec![];
        let mut prof_items: Vec<CheckMenuItem> = vec![];
        let mut sel = usize::MAX;
        let mut phase = 0f32;
        let mut lang_ru = false;
        loop {
            pump();
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                let id = ev.id.0.as_str();
                let e = match id {
                    "toggle" => Some(TrayEvent::Toggle),
                    "open" => Some(TrayEvent::Open),
                    "quit" => Some(TrayEvent::Quit),
                    p if p.starts_with("p:") => p[2..].parse().ok().map(TrayEvent::Select),
                    _ => None,
                };
                if let Some(e) = e {
                    let _ = tx.send(e);
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                match ev {
                    TrayIconEvent::DoubleClick { .. } => { let _ = tx.send(TrayEvent::Open); }
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => { let _ = tx.send(TrayEvent::Open); }
                    _ => {}
                }
            }
            match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(TrayCmd::Quit) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(TrayCmd::Update { ru, state: st, profiles: names, selected, speed: sp }) => {
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
                        profiles.set_text(if names.is_empty() { tr("Server (none — add one in the window)") } else { tr("Server") });
                        prof_names = names;
                        sel = selected;
                    }
                }
                Err(_) => {}
            }
            if pulsing(&state) {
                phase = (phase + 0.04) % 1.0;
                if (phase * 100.0) as u32 % 5 == 0 {
                    let _ = tray.set_icon(Some(icon(&state, phase)));
                }
            }
        }
        drop(tray);
        Ok(())
    }
}
