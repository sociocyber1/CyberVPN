//! GNOME / libadwaita look: Adwaita palette, light + dark, system accent color.

use crate::store::{Accent, Theme};
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Pal {
    pub bg: Color32,       // window_bg
    pub view: Color32,     // view_bg (lists, tables)
    pub panel: Color32,    // headerbar / sidebar
    pub card: Color32,     // card_bg
    pub card_hi: Color32,  // hover / pressed
    pub border: Color32,   // separators, borders
    pub text: Color32,
    pub dim: Color32,
    pub sky: Color32,      // accent_color (text, lines)
    pub sky_deep: Color32, // accent_bg_color
    pub sky_soft: Color32, // accent, translucent
    pub up: Color32,       // second chart series
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
    pub on_sky: Color32,   // accent_fg_color
    pub dark: bool,
}

/// libadwaita accent backgrounds (GNOME 47+ accent colors).
#[allow(dead_code)]
pub fn accent_bg(a: Accent) -> Color32 {
    match a {
        Accent::Blue | Accent::System => Color32::from_rgb(0x35, 0x84, 0xe4),
        Accent::Teal => Color32::from_rgb(0x21, 0x90, 0xa4),
        Accent::Green => Color32::from_rgb(0x3a, 0x94, 0x4a),
        Accent::Yellow => Color32::from_rgb(0xc8, 0x88, 0x00),
        Accent::Orange => Color32::from_rgb(0xed, 0x5b, 0x00),
        Accent::Red => Color32::from_rgb(0xe6, 0x2d, 0x42),
        Accent::Pink => Color32::from_rgb(0xd5, 0x61, 0x99),
        Accent::Purple => Color32::from_rgb(0x91, 0x41, 0xac),
        Accent::Slate => Color32::from_rgb(0x6f, 0x83, 0x96),
    }
}

#[allow(dead_code)]
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn gsettings(key: &str) -> Option<String> {
    let o = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", key]).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
}

/// Is the system in dark mode? (GNOME color-scheme, GTK_THEME, else dark)
pub fn system_dark() -> bool {
    if let Some(cs) = gsettings("color-scheme") {
        if cs.contains("dark") {
            return true;
        }
        if cs.contains("light") {
            return false;
        }
    }
    if let Ok(t) = std::env::var("GTK_THEME") {
        return t.to_lowercase().contains("dark");
    }
    if let Some(t) = gsettings("gtk-theme") {
        return t.to_lowercase().contains("dark");
    }
    #[cfg(windows)]
    {
        // HKCU\...\Themes\Personalize AppsUseLightTheme = 0 → dark
        if let Ok(o) = std::process::Command::new("reg")
            .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "/v", "AppsUseLightTheme"])
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout);
            if s.contains("0x1") {
                return false;
            }
        }
    }
    true
}

/// GNOME 47+ accent color setting.
#[allow(dead_code)]
pub fn system_accent() -> Accent {
    match gsettings("accent-color").as_deref() {
        Some("teal") => Accent::Teal,
        Some("green") => Accent::Green,
        Some("yellow") => Accent::Yellow,
        Some("orange") => Accent::Orange,
        Some("red") => Accent::Red,
        Some("pink") => Accent::Pink,
        Some("purple") => Accent::Purple,
        Some("slate") => Accent::Slate,
        _ => Accent::Blue,
    }
}

pub const SKY: Color32 = Color32::from_rgb(56, 189, 248);

impl Pal {
    /// White · Sky Blue · Black. `System` follows the desktop's dark/light.
    pub fn of(theme: Theme, _accent: Accent) -> Pal {
        let dark = match theme {
            Theme::Dark => true,
            Theme::Light => false,
            Theme::System => system_dark(),
        };
        if dark {
            Pal {
                bg: Color32::from_rgb(6, 8, 12),
                view: Color32::from_rgb(9, 12, 17),
                panel: Color32::from_rgb(11, 14, 20),
                card: Color32::from_rgb(16, 21, 29),
                card_hi: Color32::from_rgb(22, 29, 40),
                border: Color32::from_rgb(30, 41, 56),
                text: Color32::from_rgb(244, 248, 255),
                dim: Color32::from_rgb(132, 148, 168),
                sky: SKY,
                sky_deep: Color32::from_rgb(14, 165, 233),
                sky_soft: Color32::from_rgba_unmultiplied(56, 189, 248, 38),
                up: Color32::from_rgb(226, 232, 240),
                good: Color32::from_rgb(52, 211, 153),
                warn: Color32::from_rgb(251, 191, 36),
                bad: Color32::from_rgb(248, 113, 113),
                on_sky: Color32::from_rgb(4, 10, 18),
                dark: true,
            }
        } else {
            Pal {
                bg: Color32::from_rgb(244, 249, 255),
                view: Color32::from_rgb(255, 255, 255),
                panel: Color32::from_rgb(255, 255, 255),
                card: Color32::from_rgb(255, 255, 255),
                card_hi: Color32::from_rgb(236, 246, 254),
                border: Color32::from_rgb(214, 228, 242),
                text: Color32::from_rgb(8, 12, 18),
                dim: Color32::from_rgb(92, 108, 128),
                sky: Color32::from_rgb(14, 165, 233),
                sky_deep: Color32::from_rgb(2, 132, 199),
                sky_soft: Color32::from_rgba_unmultiplied(14, 165, 233, 30),
                up: Color32::from_rgb(15, 23, 42),
                good: Color32::from_rgb(5, 150, 105),
                warn: Color32::from_rgb(217, 119, 6),
                bad: Color32::from_rgb(220, 38, 38),
                on_sky: Color32::WHITE,
                dark: false,
            }
        }
    }
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn apply(ctx: &egui::Context, p: &Pal) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.view;
    v.faint_bg_color = p.card_hi;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.sky;
    v.selection.bg_fill = p.sky_soft;
    v.selection.stroke = Stroke::new(1.0, p.sky);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(12);
    let r = CornerRadius::same(10);
    for (w, fill) in [
        (&mut v.widgets.noninteractive, p.card),
        (&mut v.widgets.inactive, p.card_hi),
        (&mut v.widgets.hovered, p.card_hi),
        (&mut v.widgets.active, p.sky_soft),
        (&mut v.widgets.open, p.card_hi),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = r;
        w.bg_stroke = Stroke::new(1.0, p.border);
        w.fg_stroke = Stroke::new(1.0, p.text);
    }
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.dim);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.sky);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.sky);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, semibold())),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
    });
}
