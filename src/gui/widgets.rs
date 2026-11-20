//! Custom painted widgets: power button, gradient charts, toggle, cards.

use super::theme::Pal;
use crate::protocol::VpnState;
use eframe::egui::{
    self, epaint::PathShape, pos2, vec2, Align2, Color32, CornerRadius, FontId, Margin, Mesh, Pos2, Rect,
    Response, RichText, Sense, Shape, Stroke, Ui,
};

pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn lerp_c(a: Color32, b: Color32, t: f32) -> Color32 {
    let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), m(a[3], b[3]))
}

pub fn card(p: &Pal) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(16))
}

// ---------------------------------------------------------------- logo

pub static LOGO: std::sync::OnceLock<egui::TextureHandle> = std::sync::OnceLock::new();
pub static COMPANY: std::sync::OnceLock<egui::TextureHandle> = std::sync::OnceLock::new();

/// Load the app icon (SVG) and the company logo as textures, once.
pub fn load_textures(ctx: &egui::Context) {
    LOGO.get_or_init(|| {
        let px = crate::icon::render_premul(256);
        let img = egui::ColorImage::from_rgba_premultiplied([256, 256], &px);
        ctx.load_texture("cybervpn-logo", img, egui::TextureOptions::LINEAR)
    });
    COMPANY.get_or_init(|| {
        let n = crate::icon::COMPANY_SIZE;
        let img = egui::ColorImage::from_rgba_unmultiplied([n, n], crate::icon::COMPANY_RGBA);
        ctx.load_texture("sociocyberneering", img, egui::TextureOptions::LINEAR)
    });
}

/// The CyberVPN globe. Slightly dimmed while not protected.
pub fn logo(ui: &mut Ui, _p: &Pal, size: f32, on: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if let Some(t) = LOGO.get() {
        let tint = if on { Color32::WHITE } else { Color32::from_gray(205) };
        egui::Image::new(t).tint(tint).paint_at(ui, rect);
    }
}

/// SocioCyberneering logo on a black rounded badge (reads on light and dark).
pub fn company_logo(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same((size * 0.25) as u8), Color32::from_rgb(6, 8, 12));
    if let Some(t) = COMPANY.get() {
        egui::Image::new(t).paint_at(ui, rect.shrink(size * 0.1));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NavIcon { Dashboard, Connections, Dns, Servers, Log, Settings }

/// Small vector icons for the sidebar (no font glyphs needed).
pub fn nav_icon(painter: &egui::Painter, r: Rect, kind: NavIcon, c: Color32) {
    let s = Stroke::new(1.6, c);
    let cx = r.center();
    let u = r.width() / 2.0;
    match kind {
        NavIcon::Dashboard => {
            let g = u * 0.42;
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let ctr = cx + vec2(dx * g * 1.15, dy * g * 1.15);
                painter.rect_stroke(Rect::from_center_size(ctr, vec2(g * 1.6, g * 1.6)), CornerRadius::same(2), s, egui::StrokeKind::Middle);
            }
        }
        NavIcon::Connections => {
            for (y, dir) in [(-0.35f32, 1.0f32), (0.35, -1.0)] {
                let a = cx + vec2(-u * 0.8 * dir, y * u);
                let b = cx + vec2(u * 0.8 * dir, y * u);
                painter.line_segment([a, b], s);
                painter.line_segment([b, b + vec2(-u * 0.35 * dir, -u * 0.3)], s);
                painter.line_segment([b, b + vec2(-u * 0.35 * dir, u * 0.3)], s);
            }
        }
        NavIcon::Dns => {
            painter.circle_stroke(cx, u * 0.8, s);
            painter.line_segment([cx + vec2(-u * 0.8, 0.0), cx + vec2(u * 0.8, 0.0)], s);
            let pts: Vec<Pos2> = (0..=16).map(|i| {
                let t = -1.0 + 2.0 * i as f32 / 16.0;
                cx + vec2(u * 0.35 * (1.0 - t * t).sqrt(), t * u * 0.8)
            }).collect();
            painter.add(PathShape::line(pts.clone(), s));
            painter.add(PathShape::line(pts.iter().map(|p| pos2(2.0 * cx.x - p.x, p.y)).collect(), s));
        }
        NavIcon::Servers => {
            for dy in [-0.45f32, 0.45] {
                let rr = Rect::from_center_size(cx + vec2(0.0, dy * u), vec2(u * 1.7, u * 0.7));
                painter.rect_stroke(rr, CornerRadius::same(2), s, egui::StrokeKind::Middle);
                painter.circle_filled(rr.right_center() - vec2(u * 0.3, 0.0), 1.6, c);
            }
        }
        NavIcon::Log => {
            for (i, w) in [1.6f32, 1.2, 1.5, 0.9].iter().enumerate() {
                let y = cx.y - u * 0.6 + i as f32 * u * 0.4;
                painter.line_segment([pos2(cx.x - u * 0.8, y), pos2(cx.x - u * 0.8 + u * w, y)], s);
            }
        }
        NavIcon::Settings => {
            painter.circle_stroke(cx, u * 0.3, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment([cx + d * u * 0.5, cx + d * u * 0.85], Stroke::new(2.4, c));
            }
            painter.circle_stroke(cx, u * 0.58, s);
        }
    }
}

/// Sidebar entry: icon + label, sky highlight when selected.
pub fn nav_item(ui: &mut Ui, p: &Pal, kind: NavIcon, label: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered() || selected, 0.12);
    let bg = if selected { p.sky } else { with_alpha(p.sky, 0.10 * hover) };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    let fg = if selected { p.on_sky } else { p.text };
    let ir = Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(16.0, 16.0));
    nav_icon(ui.painter(), ir, kind, fg);
    ui.painter().text(pos2(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(14.0), fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---------------------------------------------------------------- power button

// ---------------------------------------------------------------- charts

/// Smooth filled area chart with vertical gradient. `series` share one scale.
pub fn area_chart(ui: &mut Ui, p: &Pal, size: egui::Vec2, series: &[(&[f64], Color32)], points: usize) -> f64 {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let max = series
        .iter()
        .flat_map(|(d, _)| d.iter().rev().take(points))
        .fold(0.0f64, |a, &b| a.max(b))
        .max(64.0 * 1024.0)
        * 1.2;
    // grid
    for i in 1..4 {
        let y = rect.top() + rect.height() * i as f32 / 4.0;
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, with_alpha(p.border, 0.6)));
    }
    for (data, col) in series {
        let n = data.len().min(points);
        if n < 2 {
            continue;
        }
        let slice = &data[data.len() - n..];
        let step = rect.width() / (points - 1) as f32;
        let x0 = rect.right() - step * (n - 1) as f32;
        let pts: Vec<Pos2> = slice
            .iter()
            .enumerate()
            .map(|(i, v)| pos2(x0 + step * i as f32, rect.bottom() - (*v / max) as f32 * rect.height()))
            .collect();
        // gradient fill
        let mut mesh = Mesh::default();
        for (i, pt) in pts.iter().enumerate() {
            mesh.colored_vertex(*pt, with_alpha(*col, 0.38));
            mesh.colored_vertex(pos2(pt.x, rect.bottom()), with_alpha(*col, 0.0));
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.add(PathShape::line(pts.clone(), Stroke::new(2.0, *col)));
        if let Some(last) = pts.last() {
            let t = ui.input(|i| i.time) as f32;
            painter.circle_filled(*last, 6.0 + 2.0 * (t * 4.0).sin(), with_alpha(*col, 0.25));
            painter.circle_filled(*last, 3.5, *col);
        }
    }
    max / 1.2
}

pub fn sparkline(ui: &mut Ui, p: &Pal, size: egui::Vec2, data: &[u64], col: Color32) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if data.len() < 2 {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "measuring…", FontId::proportional(11.0), p.dim);
        return;
    }
    let max = *data.iter().max().unwrap_or(&1) as f32 * 1.15;
    let min = (*data.iter().min().unwrap_or(&0) as f32 * 0.85).min(max - 1.0);
    let step = rect.width() / (data.len() - 1) as f32;
    let pts: Vec<Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(rect.left() + step * i as f32, rect.bottom() - (*v as f32 - min) / (max - min) * rect.height()))
        .collect();
    ui.painter().add(PathShape::line(pts.clone(), Stroke::new(2.0, col)));
    if let Some(l) = pts.last() {
        ui.painter().circle_filled(*l, 3.5, col);
    }
}

// ---------------------------------------------------------------- controls (GNOME)

/// iOS-style animated switch.
pub fn toggle(ui: &mut Ui, p: &Pal, on: &mut bool) -> Response {
    let size = vec2(42.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.18);
    ui.painter().rect_filled(rect, CornerRadius::same(12), lerp_c(p.border, p.sky, t));
    let x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Rounded "boxed list" container (AdwPreferencesGroup).
pub fn boxed_list(p: &Pal) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::ZERO)
}

pub fn separator(ui: &mut Ui, p: &Pal) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, p.border);
}

/// Preference row: title, optional subtitle, widget on the right.
pub fn action_row<R>(ui: &mut Ui, p: &Pal, title: &str, sub: &str, suffix: impl FnOnce(&mut Ui) -> R) -> R {
    let mut out = None;
    egui::Frame::new().inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_min_height(34.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).color(p.text));
                if !sub.is_empty() {
                    ui.label(RichText::new(sub).color(p.dim).size(12.5));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                out = Some(suffix(ui));
            });
        });
    });
    out.unwrap()
}

/// Row with a switch. Returns true when changed.
pub fn setting_row(ui: &mut Ui, p: &Pal, title: &str, sub: &str, on: &mut bool) -> bool {
    action_row(ui, p, title, sub, |ui| toggle(ui, p, on).changed())
}

/// Segmented control. Returns the new index if changed.
pub fn segmented(ui: &mut Ui, p: &Pal, items: &[&str], sel: usize) -> Option<usize> {
    let mut out = None;
    egui::Frame::new()
        .fill(p.card_hi)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                let mut order: Vec<usize> = (0..items.len()).collect();
                if ui.layout().prefer_right_to_left() {
                    order.reverse();
                }
                for i in order {
                    let selected = i == sel;
                    let txt = RichText::new(items[i]).size(12.5).color(if selected { p.on_sky } else { p.dim });
                    let b = egui::Button::new(txt)
                        .fill(if selected { p.sky } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(8));
                    if ui.add(b).clicked() && !selected {
                        out = Some(i);
                    }
                }
            });
        });
    out
}

/// Primary action: sky button.
pub fn primary_button(ui: &mut Ui, p: &Pal, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(p.on_sky).family(super::theme::semibold()))
            .fill(p.sky)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(10)),
    )
}

pub fn ghost_button(ui: &mut Ui, p: &Pal, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(p.text))
            .fill(p.card_hi)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10)),
    )
}

pub fn destructive_button(ui: &mut Ui, p: &Pal, text: &str) -> Response {
    ui.add(egui::Button::new(RichText::new(text).color(p.bad).family(super::theme::semibold())).corner_radius(CornerRadius::same(8)))
}

/// Pill status badge.
pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(12)).inner_margin(Margin::symmetric(10, 3)).show(ui, |ui| {
        ui.label(RichText::new(text).size(12.5).color(fg).family(super::theme::semibold()));
    });
}

/// Big number with caption, value animated smoothly.
pub fn stat(ui: &mut Ui, p: &Pal, id: &str, caption: &str, value: f64, fmt: impl Fn(f64) -> String, col: Color32) {
    let v = ui.ctx().animate_value_with_time(egui::Id::unique(id), value as f32, 0.45) as f64;
    ui.vertical(|ui| {
        ui.add(egui::Label::new(RichText::new(caption).color(p.dim).size(12.5)).extend());
        ui.add(egui::Label::new(RichText::new(fmt(v)).color(col).size(22.0).family(super::theme::semibold())).extend());
    });
}

pub fn human(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b.max(0.0), 0);
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", U[i]) } else if v >= 100.0 { format!("{v:.0} {}", U[i]) } else { format!("{v:.1} {}", U[i]) }
}

pub fn rate(b: f64) -> String {
    format!("{}/s", human(b))
}

pub fn mbit(b: f64) -> String {
    format!("{:.1} Mbit/s", b * 8.0 / 1e6)
}

pub fn dur(s: u64) -> String {
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    format!("{h:02}:{m:02}:{sec:02}")
}

// ---------------------------------------------------------------- Watch Dogs FX

/// Cheap deterministic hash → 0..1
pub fn hash(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

#[derive(Clone, Copy, PartialEq)]
pub enum Fx { Off, Busy, On, Err }

pub fn fx_of(st: &VpnState) -> Fx {
    match st {
        VpnState::Off => Fx::Off,
        VpnState::Connected => Fx::On,
        VpnState::Error(_) => Fx::Err,
        _ => Fx::Busy,
    }
}

/// ctOS-style corner brackets around a rect.
pub fn brackets(painter: &egui::Painter, r: Rect, c: Color32, len: f32) {
    let s = Stroke::new(1.5, c);
    for (corner, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)] {
        painter.line_segment([corner, corner + vec2(len * dx, 0.0)], s);
        painter.line_segment([corner, corner + vec2(0.0, len * dy)], s);
    }
}

/// Drifting points joined by thin lines ("plexus" network).
pub fn plexus(painter: &egui::Painter, rect: Rect, col: Color32, t: f32, n: u32, strength: f32) {
    let pts: Vec<Pos2> = (0..n)
        .map(|i| {
            let bx = rect.left() + hash(i * 3 + 1) * rect.width();
            let by = rect.top() + hash(i * 3 + 2) * rect.height();
            let ph = hash(i * 3 + 3) * 6.28;
            pos2(bx + (t * 0.21 + ph).sin() * 26.0, by + (t * 0.17 + ph * 1.3).cos() * 22.0)
        })
        .collect();
    let maxd = 120.0;
    for i in 0..pts.len() {
        for j in i + 1..pts.len() {
            let d = pts[i].distance(pts[j]);
            if d < maxd {
                painter.line_segment([pts[i], pts[j]], Stroke::new(1.0, with_alpha(col, (1.0 - d / maxd) * 0.22 * strength)));
            }
        }
    }
    for (i, p) in pts.iter().enumerate() {
        let pulse = 0.5 + 0.5 * (t * 1.3 + i as f32).sin();
        painter.circle_filled(*p, 1.6 + pulse, with_alpha(col, (0.25 + 0.35 * pulse) * strength));
    }
}

/// "Decrypting" text: characters resolve left → right after a change.
pub fn decrypt_text(ui: &Ui, id: &str, text: &str, enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    let key = egui::Id::unique(("decrypt", id));
    let now = ui.input(|i| i.time);
    let (last, start): (String, f64) = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
    let start = if last != text {
        ui.ctx().data_mut(|d| d.insert_temp(key, (text.to_string(), now)));
        now
    } else {
        start
    };
    let age = (now - start) as f32;
    let n = text.chars().count().max(1);
    let shown = ((age / 0.7) * n as f32) as usize;
    if shown >= n {
        return text.to_string();
    }
    ui.ctx().request_repaint();
    const GL: &[char] = &['0', '1', '#', '$', '%', '&', '*', '<', '>', '/', '\\', '|', 'A', 'F', '7', 'X', '█', '▓', '░'];
    let frame = (now * 30.0) as u32;
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            if i < shown || c == ' ' {
                c
            } else {
                GL[(hash(frame.wrapping_mul(31) + i as u32) * (GL.len() - 1) as f32) as usize]
            }
        })
        .collect()
}

/// Point on the unit sphere → rotated (spin around Y, tilt around X).
fn sph(lat: f32, lon: f32, spin: f32, tilt: f32) -> [f32; 3] {
    let (x, y, z) = (lat.cos() * (lon + spin).sin(), lat.sin(), lat.cos() * (lon + spin).cos());
    let (st, ct) = (tilt.sin(), tilt.cos());
    [x, y * ct - z * st, y * st + z * ct]
}

fn slerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let dot = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0);
    let om = dot.acos();
    if om < 1e-4 {
        return a;
    }
    let (s1, s2) = (((1.0 - t) * om).sin() / om.sin(), (t * om).sin() / om.sin());
    [a[0] * s1 + b[0] * s2, a[1] * s1 + b[1] * s2, a[2] * s1 + b[2] * s2]
}

/// The planet: rotating wireframe globe, network nodes and arcs, orbiting
/// satellites and a HUD ring. `you`/`server` label the highlighted link.
#[allow(clippy::too_many_arguments)]
pub fn planet(painter: &egui::Painter, c: Pos2, r: f32, p: &Pal, fx: Fx, t: f32, full: bool, server: &str, you: &str) {
    let col = match fx {
        Fx::Off => p.dim,
        Fx::Err => p.bad,
        _ => p.sky,
    };
    let spin_speed = match fx { Fx::Busy => 0.9, Fx::On => 0.22, _ => 0.08 };
    let spin = t * spin_speed;
    let tilt = 0.38;
    let pr = |v: [f32; 3], k: f32| pos2(c.x + v[0] * r * k, c.y - v[1] * r * k);
    let vis = |z: f32| if z >= 0.0 { 0.35 + 0.65 * z } else { 0.10 * (1.0 + z) };

    // atmosphere
    for i in 0..12 {
        let k = i as f32 / 12.0;
        painter.circle_filled(c, r * (1.0 + 0.16 * k), with_alpha(col, 0.035 * (1.0 - k) * if fx == Fx::Off { 0.5 } else { 1.0 }));
    }
    painter.circle_filled(c, r, with_alpha(if p.dark { Color32::from_rgb(10, 14, 22) } else { Color32::from_rgb(230, 238, 248) }, 0.92));

    // graticule
    let grid = |v: f32| with_alpha(col, v * if full { 0.55 } else { 0.4 });
    for k in 0..12 {
        let lon = k as f32 * std::f32::consts::PI / 6.0;
        let pts: Vec<([f32; 3], Pos2)> = (0..=32).map(|i| {
            let lat = -1.5708 + 3.1416 * i as f32 / 32.0;
            let v = sph(lat, lon, spin, tilt);
            (v, pr(v, 1.0))
        }).collect();
        for w in pts.windows(2) {
            painter.line_segment([w[0].1, w[1].1], Stroke::new(1.0, grid(vis((w[0].0[2] + w[1].0[2]) / 2.0))));
        }
    }
    for lat_deg in [-60.0f32, -30.0, 0.0, 30.0, 60.0] {
        let lat = lat_deg.to_radians();
        let pts: Vec<([f32; 3], Pos2)> = (0..=48).map(|i| {
            let v = sph(lat, 6.2832 * i as f32 / 48.0, spin, tilt);
            (v, pr(v, 1.0))
        }).collect();
        for w in pts.windows(2) {
            painter.line_segment([w[0].1, w[1].1], Stroke::new(1.0, grid(vis((w[0].0[2] + w[1].0[2]) / 2.0))));
        }
    }
    painter.circle_stroke(c, r, Stroke::new(1.5, with_alpha(col, 0.7)));

    // network nodes (pseudo-random cities)
    let nodes: Vec<(f32, f32)> = (0..34).map(|i| ((hash(i * 7 + 11) - 0.5) * 2.4, hash(i * 7 + 13) * 6.2832)).collect();
    // links: node 0 = you, node 1 = server
    let links: Vec<(usize, usize)> = (0..14).map(|i| ((hash(i * 5 + 3) * 33.0) as usize + 1, (hash(i * 5 + 4) * 33.0) as usize + 1)).filter(|(a, b)| a != b).collect();
    let mut all_links = vec![(0usize, 1usize)];
    all_links.extend(links);
    for (li, (a, b)) in all_links.iter().enumerate() {
        let va = sph(nodes[*a].0, nodes[*a].1, spin, tilt);
        let vb = sph(nodes[*b].0, nodes[*b].1, spin, tilt);
        let main = li == 0;
        let n = 28;
        let pts: Vec<([f32; 3], Pos2)> = (0..=n).map(|i| {
            let s = i as f32 / n as f32;
            let v = slerp(va, vb, s);
            let lift = 1.0 + (if main { 0.32 } else { 0.18 }) * (s * std::f32::consts::PI).sin();
            (v, pr(v, lift))
        }).collect();
        if !main && fx == Fx::Off {
            continue;
        }
        let base_a = if main { 0.95 } else { 0.35 };
        let lc = if main && fx == Fx::On { p.up } else { col };
        for w in pts.windows(2) {
            let z = (w[0].0[2] + w[1].0[2]) / 2.0;
            painter.line_segment([w[0].1, w[1].1], Stroke::new(if main { 2.0 } else { 1.0 }, with_alpha(lc, base_a * vis(z))));
        }
        // data packets
        if fx == Fx::On || (fx == Fx::Busy && main) {
            for k in 0..if main { 3 } else { 1 } {
                let s = ((t * if main { 0.55 } else { 0.35 } + hash(li as u32 * 9 + k) + k as f32 / 3.0) % 1.0) as f32;
                let idx = (s * n as f32) as usize;
                let (v, pp) = pts[idx.min(n)];
                if v[2] > -0.1 {
                    painter.circle_filled(pp, if main { 4.0 } else { 2.5 }, with_alpha(lc, 0.35));
                    painter.circle_filled(pp, if main { 2.2 } else { 1.4 }, Color32::WHITE);
                }
            }
        }
    }
    for (i, (lat, lon)) in nodes.iter().enumerate() {
        let v = sph(*lat, *lon, spin, tilt);
        if v[2] < 0.0 {
            continue;
        }
        let pp = pr(v, 1.0);
        let pulse = 0.5 + 0.5 * (t * 2.0 + i as f32).sin();
        let a = vis(v[2]);
        painter.circle_filled(pp, 2.0 + pulse * 2.5, with_alpha(col, 0.18 * a));
        painter.circle_filled(pp, 1.8, with_alpha(if i < 2 { Color32::WHITE } else { col }, a));
    }
    // labels for you → server (ctOS tags)
    if fx != Fx::Off {
        for (i, label) in [(0usize, you), (1usize, server)] {
            let v = sph(nodes[i].0, nodes[i].1, spin, tilt);
            if v[2] < 0.15 || label.is_empty() {
                continue;
            }
            let pp = pr(v, 1.0);
            let tp = pp + vec2(14.0, -16.0);
            let galley = painter.layout_no_wrap(label.to_string(), FontId::monospace(10.5), Color32::WHITE);
            let br = Rect::from_min_size(tp, galley.size()).expand2(vec2(5.0, 3.0));
            painter.rect_filled(br, CornerRadius::same(2), with_alpha(Color32::BLACK, 0.55));
            brackets(painter, br, col, 5.0);
            painter.line_segment([pp, br.left_bottom()], Stroke::new(1.0, with_alpha(col, 0.8)));
            painter.galley(tp, galley, Color32::WHITE);
        }
    }

    if !full {
        return;
    }
    // orbits with satellites
    for (k, (incl, rad, speed)) in [(0.95f32, 1.3f32, 0.5f32), (-0.6, 1.42, -0.32)].iter().enumerate() {
        let ring: Vec<([f32; 3], Pos2)> = (0..=64).map(|i| {
            let a = 6.2832 * i as f32 / 64.0;
            let v = [a.cos() * rad, a.sin() * rad * incl.sin() * 0.35, a.sin() * rad * incl.cos()];
            (v, pos2(c.x + v[0] * r, c.y - v[1] * r - v[2] * r * 0.12))
        }).collect();
        for (i, w) in ring.windows(2).enumerate() {
            if i % 2 == 0 {
                continue; // dashed
            }
            let z = w[0].0[2];
            painter.line_segment([w[0].1, w[1].1], Stroke::new(1.0, with_alpha(col, if z > 0.0 { 0.45 } else { 0.15 })));
        }
        let a = t * speed + k as f32 * 2.0;
        let v = [a.cos() * rad, a.sin() * rad * incl.sin() * 0.35, a.sin() * rad * incl.cos()];
        let sp = pos2(c.x + v[0] * r, c.y - v[1] * r - v[2] * r * 0.12);
        if v[2] > -0.2 || (sp.distance(c) > r) {
            painter.circle_filled(sp, 5.0, with_alpha(col, 0.25));
            painter.rect_filled(Rect::from_center_size(sp, vec2(5.0, 5.0)), CornerRadius::same(1), Color32::WHITE);
            painter.line_segment([sp - vec2(7.0, 0.0), sp + vec2(7.0, 0.0)], Stroke::new(1.5, with_alpha(col, 0.9)));
        }
    }
    // HUD ring
    let hr = r * 1.52;
    let rot = t * match fx { Fx::Busy => 1.4, Fx::On => 0.15, _ => 0.05 };
    for seg in 0..6 {
        let a0 = rot + seg as f32 * 6.2832 / 6.0;
        let pts: Vec<Pos2> = (0..=12).map(|i| {
            let a = a0 + 0.75 * i as f32 / 12.0;
            c + vec2(a.cos(), a.sin()) * hr
        }).collect();
        painter.add(PathShape::line(pts, Stroke::new(2.0, with_alpha(col, 0.55))));
    }
    for i in 0..72 {
        let a = -rot * 0.5 + i as f32 * 6.2832 / 72.0;
        let l = if i % 6 == 0 { 7.0 } else { 3.0 };
        let d = vec2(a.cos(), a.sin());
        painter.line_segment([c + d * (hr + 6.0), c + d * (hr + 6.0 + l)], Stroke::new(1.0, with_alpha(col, 0.35)));
    }
    // scanning beam while connecting
    if fx == Fx::Busy {
        let a = t * 2.6;
        let mut mesh = Mesh::default();
        mesh.colored_vertex(c, with_alpha(col, 0.0));
        for i in 0..=16 {
            let aa = a - 0.6 * i as f32 / 16.0;
            mesh.colored_vertex(c + vec2(aa.cos(), aa.sin()) * hr, with_alpha(col, 0.22 * (1.0 - i as f32 / 16.0)));
            if i > 0 {
                mesh.add_triangle(0, i as u32, i as u32 + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
    }
    // glitch bars on error / while connecting
    if matches!(fx, Fx::Err | Fx::Busy) {
        let f = (t * 14.0) as u32;
        for k in 0..3u32 {
            if hash(f * 7 + k) > if fx == Fx::Err { 0.55 } else { 0.85 } {
                let y = c.y + (hash(f * 13 + k) - 0.5) * r * 2.2;
                let x = c.x + (hash(f * 17 + k) - 0.5) * r * 1.6;
                let w = 20.0 + hash(f * 19 + k) * r;
                painter.rect_filled(Rect::from_min_size(pos2(x - w / 2.0, y), vec2(w, 2.0 + hash(f + k) * 3.0)), CornerRadius::ZERO, with_alpha(col, 0.5));
            }
        }
    }
}
