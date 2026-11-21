//! xui-status — mark busy keys in the free server list, for CyberVPN.
//!
//! Runs on the VPN server (cron / systemd timer). Asks the 3x-ui panel which
//! clients are online, rewrites the list in a local clone of the `txt` repo
//! ("  # busy" after the link of every online key, removed from the others)
//! and commits + pushes only when something changed. CyberVPN reads the
//! marks, shows "busy" and picks a free key. The panel password stays here.
//!
//! Settings (environment, or a file given with --env FILE, KEY=VALUE lines):
//!   XUI_URL   panel address incl. base path, e.g. http://127.0.0.1:2053/abc
//!   XUI_USER  panel login
//!   XUI_PASS  panel password
//!   TXT_REPO  path to the git clone of the list (default /opt/txt)
//!   TXT_FILE  file with the links (default README.md)
//!
//! Usage:
//!   xui-status [--env /etc/xui-status.env] [--dry-run] [--watch SECONDS]

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

const MARK: &str = "  # busy";

struct Cfg {
    url: String,
    user: String,
    pass: String,
    repo: String,
    file: String,
}

fn load_env(path: Option<&str>) -> Result<Cfg, String> {
    let mut m: HashMap<String, String> = std::env::vars().collect();
    if let Some(p) = path {
        let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        for l in t.lines() {
            let l = l.trim().trim_start_matches("export ");
            if l.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                m.insert(k.trim().into(), v.trim().trim_matches('"').trim_matches('\'').into());
            }
        }
    }
    let g = |k: &str, d: &str| m.get(k).cloned().unwrap_or_else(|| d.to_string());
    let c = Cfg {
        url: g("XUI_URL", "http://127.0.0.1:2053").trim_end_matches('/').to_string(),
        user: g("XUI_USER", ""),
        pass: g("XUI_PASS", ""),
        repo: g("TXT_REPO", "/opt/txt"),
        file: g("TXT_FILE", "README.md"),
    };
    if c.user.is_empty() {
        return Err("XUI_USER is not set".into());
    }
    Ok(c)
}

/// Emails of the clients online right now.
fn online(c: &Cfg) -> Result<Vec<String>, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let r = agent
        .post(&format!("{}/login", c.url))
        .send_form(&[("username", c.user.as_str()), ("password", c.pass.as_str())])
        .map_err(|e| format!("login: {e}"))?;
    let cookies: Vec<String> = r.all("set-cookie").iter().filter_map(|v| v.split(';').next()).map(String::from).collect();
    let body: serde_json::Value = r.into_json().map_err(|e| format!("login reply: {e}"))?;
    if body["success"] != true {
        return Err(format!("login failed: {}", body["msg"]));
    }
    let r = agent
        .post(&format!("{}/panel/api/inbounds/onlines", c.url))
        .set("Cookie", &cookies.join("; "))
        .call()
        .map_err(|e| format!("onlines: {e}"))?;
    let body: serde_json::Value = r.into_json().map_err(|e| format!("onlines reply: {e}"))?;
    Ok(body["obj"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default())
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// New text of the list with the busy marks set. A line is busy when an
/// online client's email appears in the link's name (3x-ui names links
/// "<remark>-<email>").
pub fn mark(text: &str, online: &[String]) -> String {
    text.split('\n')
        .map(|line| {
            let Some(p) = line.find("hy2://").or_else(|| line.find("hysteria2://")) else { return line.to_string() };
            let end = line[p..].find(char::is_whitespace).map(|e| p + e).unwrap_or(line.len());
            let link = &line[..end];
            let name = link.split_once('#').map(|(_, n)| unescape(n)).unwrap_or_default();
            let busy = online.iter().any(|e| name.contains(e.as_str()));
            if busy { format!("{link}{MARK}") } else { link.to_string() }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn git(repo: &str, args: &[&str]) -> Result<(), String> {
    let st = Command::new("git").arg("-C").arg(repo).args(args).status().map_err(|e| format!("git: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("git {} failed", args.join(" "))) }
}

fn once(c: &Cfg, dry: bool) -> Result<(), String> {
    if !dry {
        git(&c.repo, &["pull", "-q", "--rebase"])?; // your newest keys first
    }
    let on = online(c)?;
    let path = format!("{}/{}", c.repo, c.file);
    let old = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let new = mark(&old, &on);
    let busy = new.matches(MARK).count();
    if new == old {
        println!("no change: {} online, {busy} keys busy", on.len());
        return Ok(());
    }
    if dry {
        println!("--- would write ({busy} busy):\n{new}");
        return Ok(());
    }
    std::fs::write(&path, &new).map_err(|e| format!("{path}: {e}"))?;
    git(&c.repo, &["commit", "-qam", &format!("status: {busy} keys busy")])?;
    git(&c.repo, &["push", "-q"])?;
    println!("updated: {busy} keys busy");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(String::as_str);
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("xui-status [--env FILE] [--dry-run] [--watch SECONDS]\nsee the top of src/main.rs for the settings");
        return;
    }
    let cfg = match load_env(opt("--env")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xui-status: {e}");
            std::process::exit(2);
        }
    };
    let dry = args.iter().any(|a| a == "--dry-run");
    let watch: Option<u64> = opt("--watch").and_then(|v| v.parse().ok());
    loop {
        if let Err(e) = once(&cfg, dry) {
            eprintln!("xui-status: {e}");
            if watch.is_none() {
                std::process::exit(1);
            }
        }
        match watch {
            Some(s) => std::thread::sleep(Duration::from_secs(s.max(30))),
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_online_keys_only() {
        let t = "# keys\nhy2://a@h:1?x=1#sclab%20(NL)-aaa-sce-1\nhy2://b@h:1#sclab%20(NL)-bbb-sce-2  # busy\n";
        let r = mark(t, &["aaa-sce-1".into()]);
        assert_eq!(r, "# keys\nhy2://a@h:1?x=1#sclab%20(NL)-aaa-sce-1  # busy\nhy2://b@h:1#sclab%20(NL)-bbb-sce-2\n");
    }
}
