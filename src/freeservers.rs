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

/// hy2:// links of the list with their "busy" mark: a line is busy when it
/// says so after the link (e.g. `hy2://… # busy` or `… занят`), which the
/// server-side status script writes for keys that are online right now.
pub fn parse(text: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    for line in text.lines() {
        let Some(pos) = line.find("hy2://").or_else(|| line.find("hysteria2://")) else { continue };
        let rest = &line[pos..];
        let end = rest.find(|c: char| c.is_whitespace() || c == '`' || c == ')' || c == '<').unwrap_or(rest.len());
        let uri = rest[..end].to_string();
        let after = rest[end..].to_lowercase();
        let busy = after.contains("busy") || after.contains("занят");
        if !out.iter().any(|(u, _)| *u == uri) {
            out.push((uri, busy));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_busy_marks() {
        let t = "hy2://a@h:1#x-1\nhy2://b@h:1#x-2  # busy\n`hy2://c@h:1#x-3` занят\ntext\nhy2://a@h:1#x-1\n";
        let v = super::parse(t);
        assert_eq!(v, vec![("hy2://a@h:1#x-1".into(), false), ("hy2://b@h:1#x-2".into(), true), ("hy2://c@h:1#x-3".into(), true)]);
    }
}
