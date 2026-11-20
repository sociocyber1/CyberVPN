#!/bin/sh
# CyberVPN installer for Arch / Artix (and other Linux).
set -e
command -v cargo >/dev/null || { echo "install Rust first: sudo pacman -S rust"; exit 1; }
echo "==> dependencies (GTK tray, notifications, polkit)"
if command -v pacman >/dev/null; then
  sudo pacman -S --needed gtk3 libayatana-appindicator polkit
fi
echo "==> building"
cargo build --release
sudo install -m755 target/release/cybervpn /usr/local/bin/cybervpn
mkdir -p ~/.local/share/applications
cat > ~/.local/share/applications/cybervpn.desktop <<DESK
[Desktop Entry]
Type=Application
Name=CyberVPN
Comment=Hysteria2 VPN
Exec=/usr/local/bin/cybervpn
Icon=network-vpn
Categories=Network;
Terminal=false
DESK
echo "==> done. Start it from your app menu or run: cybervpn"
echo "    Hyprland: a polkit agent must run for the password dialog, e.g. exec-once = systemctl --user start hyprpolkitagent"
echo "    (on Artix without systemd: exec-once = /usr/lib/hyprpolkitagent/hyprpolkitagent)"
