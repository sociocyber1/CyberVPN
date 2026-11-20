First release of **CyberVPN**: a desktop Hysteria2 VPN by SocioCyberneering Inc.

### Download
- **Linux:** `cybervpn-linux-x86_64`. It needs only `polkit` for the admin prompt; the tray needs no other libraries.
- **Windows:** `cybervpn-windows-x86_64.exe` is built by GitHub Actions and attached here.

```bash
chmod +x cybervpn-linux-x86_64 && sudo install -m755 cybervpn-linux-x86_64 /usr/local/bin/cybervpn
cybervpn
```

### Features
- One-click on/off, tray icon with status and quick menu, notifications and a "VPN is off" reminder
- **Simple** and **Pro** modes: Pro has a dashboard, connections, DNS log, servers and logs
- Animated planet UI, dark/light, Russian/English chosen from the system language
- Built-in VPN engine (hysteria v2.14.0) with background auto-updates that apply on the next start
- **notToVPN** split tunneling by domain/zone (Pro, Linux)
- **IPv6 auto-check**: stops stalls on servers without IPv6 (YouTube, web apps, `curl`)
- Autostart on Linux and Windows

Check a binary: `sha256sum -c <file>.sha256`
