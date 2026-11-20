// Embed the hysteria core (vendor/*.xz) for the target OS/CPU when present.
use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(embedded_core)");
    println!("cargo::rerun-if-changed=vendor");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let needed: &[&str] = match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => &["hysteria-linux-amd64.xz", "hysteria-linux-amd64-avx.xz"],
        ("linux", "aarch64") => &["hysteria-linux-arm64.xz"],
        ("windows", "x86_64") => &["hysteria-windows-amd64.exe.xz", "hysteria-windows-amd64-avx.exe.xz"],
        _ => &[],
    };
    let ok = !needed.is_empty()
        && needed.iter().all(|f| Path::new("vendor").join(f).is_file())
        && Path::new("vendor/SHA256SUMS").is_file()
        && Path::new("vendor/VERSION").is_file();
    if ok {
        println!("cargo::rustc-cfg=embedded_core");
    } else {
        println!("cargo::warning=VPN engine NOT embedded (vendor/ files missing) — it will be downloaded on first connect");
    }
    #[cfg(windows)]
    {}
}
