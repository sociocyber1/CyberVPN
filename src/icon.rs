//! App icon: the SocioCyber globe (assets/cybervpn.svg, restyled in
//! White · Sky Blue · Black), rendered with resvg for the window, the tray
//! (with a small status badge) and the in-app logo.

use crate::protocol::VpnState;
use resvg::{tiny_skia, usvg};
use std::sync::OnceLock;

const SVG: &str = include_str!("../assets/cybervpn.svg");
/// SocioCyberneering logo, 128×128 straight RGBA.
pub const COMPANY_RGBA: &[u8] = include_bytes!("../assets/sociocyberneering.rgba");
pub const COMPANY_SIZE: usize = 128;

fn tree() -> &'static usvg::Tree {
    static T: OnceLock<usvg::Tree> = OnceLock::new();
    T.get_or_init(|| usvg::Tree::from_str(SVG, &usvg::Options::default()).expect("valid app icon SVG"))
}

/// Premultiplied RGBA (what egui textures want).
pub fn render_premul(size: u32) -> Vec<u8> {
    let t = tree();
    let mut pix = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / t.size().width().max(t.size().height());
    resvg::render(t, tiny_skia::Transform::from_scale(s, s), &mut pix.as_mut());
    pix.take()
}

/// Straight (non-premultiplied) RGBA, for window and tray icons.
pub fn render(size: u32) -> Vec<u8> {
    let mut px = render_premul(size);
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    px
}

const SKY: [u8; 3] = [56, 189, 248];
const RED: [u8; 3] = [239, 68, 68];
const WHITE: [u8; 3] = [255, 255, 255];
const BLACK: [u8; 3] = [6, 8, 12];

fn blend(dst: &mut [u8], c: [u8; 3], a: f32) {
    let a = a.clamp(0.0, 1.0);
    let da = dst[3] as f32 / 255.0;
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 {
        return;
    }
    for i in 0..3 {
        dst[i] = ((c[i] as f32 * a + dst[i] as f32 * da * (1.0 - a)) / oa) as u8;
    }
    dst[3] = (oa * 255.0) as u8;
}

/// Tray icon: the globe, greyed out when off, with a status badge
/// (sky ✓ connected, pulsing sky ring connecting, red ! error).
pub fn tray_rgba(state: &VpnState, size: u32, phase: f32) -> Vec<u8> {
    let mut px = render(size);
    let n = size as f32;
    if matches!(state, VpnState::Off) {
        for p in px.chunks_exact_mut(4) {
            let l = (p[0] as f32 * 0.3 + p[1] as f32 * 0.59 + p[2] as f32 * 0.11) * 0.75;
            p[0] = l as u8;
            p[1] = l as u8;
            p[2] = l as u8;
        }
        return px;
    }
    let (cx, cy, r) = (n * 0.74, n * 0.74, n * 0.21);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let i = ((y * size + x) * 4) as usize;
            let p = &mut px[i..i + 4];
            let cov = |edge: f32| (edge - d + 0.5).clamp(0.0, 1.0);
            // dark outline so the badge reads on any panel colour
            blend(p, BLACK, cov(r + n * 0.035));
            match state {
                VpnState::Connected => {
                    blend(p, SKY, cov(r));
                    // check mark
                    let (u, v) = ((fx - cx) / r, (fy - cy) / r);
                    let seg = |ax: f32, ay: f32, bx: f32, by: f32| {
                        let (dx, dy) = (bx - ax, by - ay);
                        let t = (((u - ax) * dx + (v - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                        ((u - ax - t * dx).powi(2) + (v - ay - t * dy).powi(2)).sqrt()
                    };
                    let dist = seg(-0.5, 0.0, -0.12, 0.38).min(seg(-0.12, 0.38, 0.5, -0.32));
                    blend(p, WHITE, ((0.17 - dist) * r + 0.5).clamp(0.0, 1.0));
                }
                VpnState::Error(_) => {
                    blend(p, RED, cov(r));
                    let (u, v) = ((fx - cx) / r, (fy - cy) / r);
                    let bar = u.abs() < 0.14 && (-0.6..0.15).contains(&v);
                    let dot = (u * u + (v - 0.45).powi(2)).sqrt() < 0.16;
                    if bar || dot {
                        blend(p, WHITE, 1.0);
                    }
                }
                _ => {
                    // connecting: dark disc + sky ring + pulsing dot
                    blend(p, BLACK, cov(r));
                    let ring = (d - r * 0.78).abs();
                    blend(p, SKY, ((n * 0.05) - ring + 0.5).clamp(0.0, 1.0));
                    let pr = r * (0.2 + 0.25 * (phase * std::f32::consts::TAU).sin().abs());
                    blend(p, SKY, cov(pr));
                }
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_icon() {
        let px = super::render(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        let opaque = px.chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(opaque > 64 * 64 / 4, "icon should cover a good part of the square ({opaque})");
        assert_eq!(super::COMPANY_RGBA.len(), super::COMPANY_SIZE * super::COMPANY_SIZE * 4);
    }

    /// cargo test --release dump_icons -- --ignored  → writes PNG previews to /tmp
    #[test]
    #[ignore]
    fn dump_icons() {
        use crate::protocol::VpnState;
        for (name, st) in [("off", VpnState::Off), ("connecting", VpnState::Connecting), ("on", VpnState::Connected), ("err", VpnState::Error("x".into()))] {
            let px = super::tray_rgba(&st, 128, 0.3);
            let mut pm = resvg::tiny_skia::Pixmap::new(128, 128).unwrap();
            for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(px.chunks_exact(4)) {
                let a = s[3] as u32;
                d[0] = (s[0] as u32 * a / 255) as u8;
                d[1] = (s[1] as u32 * a / 255) as u8;
                d[2] = (s[2] as u32 * a / 255) as u8;
                d[3] = s[3];
            }
            std::fs::write(format!("/tmp/tray_{name}.raw"), pm.data()).unwrap();
        }
        std::fs::write("/tmp/icon_512.raw", super::render(512)).unwrap();
    }
}
