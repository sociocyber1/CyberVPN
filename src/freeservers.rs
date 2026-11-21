//! Free SocioCyberneering servers: a public list of hy2:// keys kept in
//! github.com/sociocyber1/txt. Fetched on the first start (and on request);
//! mirrors are tried when GitHub itself is blocked.

use anyhow::{bail, Result};
use std::time::Duration;

const SOURCES: &[&str] = &[
    "https://raw.githubusercontent.com/sociocyber1/txt/main/README.md",
    "https://cdn.jsdelivr.net/gh/sociocyber1/txt@main/README.md",
    "https://github.com/sociocyber1/txt/raw/main/README.md",
    "https://fastly.jsdelivr.net/gh/sociocyber1/txt@main/README.md",
];

/// The text of the list (contains hy2:// links).
pub fn fetch() -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .connect_timeout(Duration::from_secs(6))
            .user_agent(concat!("CyberVPN/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mut last = String::new();
        for url in SOURCES {
            match client.get(*url).send().await.and_then(|r| r.error_for_status()) {
                Ok(r) => match r.text().await {
                    Ok(t) if t.contains("hy2://") || t.contains("hysteria2://") => return Ok(t),
                    Ok(_) => last = format!("{url}: no hy2:// links"),
                    Err(e) => last = format!("{url}: {e}"),
                },
                Err(e) => last = format!("{url}: {e}"),
            }
        }
        bail!("could not download the server list ({last})")
    })
}
