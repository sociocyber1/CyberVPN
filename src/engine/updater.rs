//! VPN engine (hysteria core) management:
//!  * a copy is embedded in CyberVPN → works offline, no download needed;
//!  * new releases are downloaded in the background while you keep using the
//!    VPN (resumable, SHA-256 checked) and staged as `hysteria.next`;
//!  * the staged engine replaces the old one at the next start / connect,
//!    never under a running VPN.

use super::core::{asset_name, sha256_file, CORE_EXE};
use super::download::{self, Opts, Progress};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(all(embedded_core, target_os = "linux", target_arch = "x86_64"))]
const DATA: &[(&str, &[u8])] = &[
    ("hysteria-linux-amd64-avx", include_bytes!("../../vendor/hysteria-linux-amd64-avx.xz")),
    ("hysteria-linux-amd64", include_bytes!("../../vendor/hysteria-linux-amd64.xz")),
];
#[cfg(all(embedded_core, target_os = "linux", target_arch = "aarch64"))]
const DATA: &[(&str, &[u8])] = &[("hysteria-linux-arm64", include_bytes!("../../vendor/hysteria-linux-arm64.xz"))];
#[cfg(all(embedded_core, windows, target_arch = "x86_64"))]
const DATA: &[(&str, &[u8])] = &[
    ("hysteria-windows-amd64-avx.exe", include_bytes!("../../vendor/hysteria-windows-amd64-avx.exe.xz")),
    ("hysteria-windows-amd64.exe", include_bytes!("../../vendor/hysteria-windows-amd64.exe.xz")),
];
#[cfg(not(embedded_core))]
const DATA: &[(&str, &[u8])] = &[];

#[cfg(embedded_core)]
const EMB_VERSION: &str = include_str!("../../vendor/VERSION");
#[cfg(not(embedded_core))]
const EMB_VERSION: &str = "";
#[cfg(embedded_core)]
const SUMS: &str = include_str!("../../vendor/SHA256SUMS");
#[cfg(not(embedded_core))]
const SUMS: &str = "";

const RELEASES: &str = "https://github.com/apernet/hysteria/releases";

pub fn embedded_version() -> Option<&'static str> {
    (!DATA.is_empty()).then(|| EMB_VERSION.trim())
}

/// "v2.14.0" → (2, 14, 0)
pub fn parse_ver(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim().trim_start_matches(|c: char| !c.is_ascii_digit()).split(|c: char| !c.is_ascii_digit()).filter_map(|x| x.parse().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub fn newer(a: &str, than: &str) -> bool {
    parse_ver(a) > parse_ver(than)
}

fn core_path(bin: &Path) -> PathBuf { bin.join(CORE_EXE) }
fn ver_file(bin: &Path) -> PathBuf { bin.join("hysteria.version") }
fn next_path(bin: &Path) -> PathBuf { bin.join(format!("{CORE_EXE}.next")) }
fn next_ver(bin: &Path) -> PathBuf { bin.join("hysteria.next.version") }
fn next_sum(bin: &Path) -> PathBuf { bin.join("hysteria.next.sha256") }

pub fn installed_version(bin: &Path) -> Option<String> {
    if !core_path(bin).is_file() {
        return None;
    }
    fs::read_to_string(ver_file(bin)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .or_else(|| Some(super::core::core_version(&core_path(bin))))
}

pub fn staged_version(bin: &Path) -> Option<String> {
    (next_path(bin).is_file()).then(|| fs::read_to_string(next_ver(bin)).ok()).flatten().map(|s| s.trim().to_string())
}

fn make_exec(p: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o755))?;
    }
    let _ = p;
    Ok(())
}

/// Atomically put `src` in place of the engine.
fn install_file(bin: &Path, src: &Path, version: &str) -> Result<()> {
    make_exec(src)?;
    let dst = core_path(bin);
    #[cfg(windows)]
    let _ = fs::remove_file(&dst);
    fs::rename(src, &dst)?;
    fs::write(ver_file(bin), version)?;
    Ok(())
}

fn extract_embedded(bin: &Path) -> Result<String> {
    let want_asset = asset_name()?;
    let (asset, data) = DATA.iter().find(|(n, _)| *n == want_asset).or_else(|| DATA.last()).context("no embedded engine")?;
    let want = SUMS
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let (h, f) = (it.next()?, it.next()?);
            (f == *asset).then(|| h.to_lowercase())
        })
        .context("embedded checksum missing")?;
    fs::create_dir_all(bin)?;
    let tmp = bin.join("hysteria.unpack");
    {
        let mut out = BufWriter::new(fs::File::create(&tmp)?);
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(*data), &mut out).context("embedded engine is corrupt")?;
        out.flush()?;
    }
    if sha256_file(&tmp)? != want {
        let _ = fs::remove_file(&tmp);
        bail!("embedded engine failed the SHA-256 check");
    }
    let v = EMB_VERSION.trim().to_string();
    install_file(bin, &tmp, &v)?;
    Ok(v)
}

#[derive(Debug, PartialEq)]
pub enum Prepared {
    /// nothing changed
    Same(String),
    /// first install from the embedded copy
    Installed(String),
    /// a staged/bundled newer engine replaced the old one
    Updated { from: String, to: String },
}

/// Call when no VPN is running (app start, before connecting): applies a
/// staged update, or installs/upgrades from the embedded copy.
pub fn prepare(bin: &Path) -> Result<Prepared> {
    let cur = installed_version(bin);
    // 1) staged download from the background updater
    if let Some(v) = staged_version(bin) {
        let ok = fs::read_to_string(next_sum(bin)).ok().map(|s| s.trim().to_string()) == sha256_file(&next_path(bin)).ok();
        let applicable = cur.as_deref().is_none_or(|c| newer(&v, c));
        if ok && applicable {
            install_file(bin, &next_path(bin), &v)?;
            let _ = fs::remove_file(next_ver(bin));
            let _ = fs::remove_file(next_sum(bin));
            return Ok(match cur {
                Some(from) => Prepared::Updated { from, to: v },
                None => Prepared::Installed(v),
            });
        }
        for f in [next_path(bin), next_ver(bin), next_sum(bin)] {
            let _ = fs::remove_file(f);
        }
    }
    // 2) embedded copy: install if missing or newer (app was updated)
    if let Some(ev) = embedded_version() {
        match &cur {
            None => return extract_embedded(bin).map(Prepared::Installed),
            Some(c) if newer(ev, c) => {
                let from = c.clone();
                return extract_embedded(bin).map(|to| Prepared::Updated { from, to });
            }
            _ => {}
        }
    }
    match cur {
        Some(c) => Ok(Prepared::Same(c)),
        None => bail!("VPN engine not installed and none is embedded in this build"),
    }
}

/// Latest release tag, e.g. "app/v2.15.0" (learned from GitHub's redirect).
async fn latest_tag(o: &Opts, asset: &str) -> Result<String> {
    let c = download::client(o, false)?;
    let r = download::resolve(&c, &format!("{RELEASES}/latest/download/{asset}")).await?;
    r.hops
        .iter()
        .find_map(|h| {
            let rest = h.split("/releases/download/").nth(1)?;
            let t = rest.strip_suffix(&format!("/{asset}"))?;
            (t != "latest").then(|| percent_encoding::percent_decode_str(t).decode_utf8_lossy().into_owned())
        })
        .context("could not read the latest version")
}

/// Background check: if GitHub has a newer engine than `current`, download
/// it next to the running one (VPN keeps working) and stage it.
/// Returns the staged version, or None when already up to date.
pub fn check_and_stage(bin: &Path, current: &str, progress: Option<Arc<Progress>>) -> Result<Option<String>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let asset = asset_name()?;
        let o = Opts { jobs: 6, proxy: None };
        let tag = latest_tag(&o, &asset).await?;
        let version = tag.rsplit('/').next().unwrap_or(&tag).to_string();
        if !newer(&version, current) {
            return Ok(None);
        }
        if staged_version(bin).as_deref() == Some(version.as_str()) {
            return Ok(Some(version)); // already downloaded
        }
        fs::create_dir_all(bin)?;
        let tmp = bin.join("hysteria.next.download");
        download::download(&format!("{RELEASES}/download/{tag}/{asset}"), &tmp, &format!("{tag}/{asset}"), &o, progress).await?;
        let hashes = download::fetch_text(&download::client(&o, true)?, &format!("{RELEASES}/download/{tag}/hashes.txt")).await?;
        let want = hashes
            .lines()
            .find_map(|l| {
                let mut it = l.split_whitespace();
                let (h, f) = (it.next()?, it.next()?);
                (f.rsplit('/').next() == Some(asset.as_str())).then(|| h.to_lowercase())
            })
            .context("hashes.txt has no entry for this engine")?;
        let got = sha256_file(&tmp)?;
        if got != want {
            let _ = fs::remove_file(&tmp);
            bail!("downloaded engine failed the SHA-256 check");
        }
        make_exec(&tmp)?;
        fs::rename(&tmp, next_path(bin))?;
        fs::write(next_sum(bin), &got)?;
        fs::write(next_ver(bin), &version)?;
        Ok(Some(version))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v2.15.0", "v2.14.9"));
        assert!(newer("app/v2.14.1", "v2.14.0"));
        assert!(!newer("v2.14.0", "v2.14.0"));
        assert!(!newer("v2.9.0", "v2.14.0"));
    }

    #[test]
    #[cfg(embedded_core)]
    fn embedded_install_then_staged_update() {
        let bin = std::env::temp_dir().join(format!("cvpn-upd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        // first start: installs the embedded engine
        assert_eq!(prepare(&bin).unwrap(), Prepared::Installed(embedded_version().unwrap().to_string()));
        assert_eq!(prepare(&bin).unwrap(), Prepared::Same(embedded_version().unwrap().to_string()));
        // simulate a background download of v9.9.9
        fs::copy(core_path(&bin), next_path(&bin)).unwrap();
        fs::write(next_sum(&bin), sha256_file(&next_path(&bin)).unwrap()).unwrap();
        fs::write(next_ver(&bin), "v9.9.9").unwrap();
        // next start: applied
        assert_eq!(
            prepare(&bin).unwrap(),
            Prepared::Updated { from: embedded_version().unwrap().to_string(), to: "v9.9.9".into() }
        );
        assert_eq!(installed_version(&bin).as_deref(), Some("v9.9.9"));
        assert!(!next_path(&bin).exists());
        // a corrupted staged file is discarded, not installed
        fs::write(next_path(&bin), b"garbage").unwrap();
        fs::write(next_sum(&bin), "0000").unwrap();
        fs::write(next_ver(&bin), "v10.0.0").unwrap();
        assert_eq!(prepare(&bin).unwrap(), Prepared::Same("v9.9.9".into()));
        assert!(!next_path(&bin).exists());
        let _ = fs::remove_dir_all(&bin);
    }

    /// Real network test: cargo test --release real_background_update -- --ignored --nocapture
    #[test]
    #[ignore]
    #[cfg(embedded_core)]
    fn real_background_update() {
        let bin = std::env::temp_dir().join(format!("cvpn-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        prepare(&bin).unwrap();
        fs::write(ver_file(&bin), "v2.13.0").unwrap(); // pretend an older engine runs
        let old_sum = sha256_file(&core_path(&bin)).unwrap();
        let p = Arc::new(Progress::default());
        let staged = check_and_stage(&bin, "v2.13.0", Some(p.clone())).unwrap();
        println!("staged: {staged:?}, downloaded {} bytes", p.have.load(std::sync::atomic::Ordering::Relaxed));
        assert!(staged.is_some());
        // the running engine was not touched while downloading
        assert_eq!(sha256_file(&core_path(&bin)).unwrap(), old_sum);
        assert!(next_path(&bin).exists());
        // second check: already staged, no new download
        assert_eq!(check_and_stage(&bin, "v2.13.0", None).unwrap(), staged);
        // "next start"
        let r = prepare(&bin).unwrap();
        println!("prepare: {r:?}");
        assert!(matches!(r, Prepared::Updated { .. }));
        assert!(!next_path(&bin).exists());
        // and now it is current
        assert_eq!(check_and_stage(&bin, &installed_version(&bin).unwrap(), None).unwrap(), None);
        let _ = fs::remove_dir_all(&bin);
    }
}
