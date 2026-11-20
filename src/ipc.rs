//! Local IPC: JSON lines over 127.0.0.1 with a random token.

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone)]
pub struct IpcInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn random_token() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{:?}{}{:?}", std::time::SystemTime::now(), std::process::id(), std::thread::current().id()));
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut b = [0u8; 32];
        if f.read_exact(&mut b).is_ok() {
            h.update(b);
        }
    }
    let addr = &h as *const _ as usize; // ASLR adds a little more entropy on Windows
    h.update(addr.to_le_bytes());
    format!("{:x}", h.finalize())
}

pub fn send<T: Serialize>(s: &mut TcpStream, msg: &T) -> Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    s.write_all(line.as_bytes())?;
    Ok(())
}

pub fn read_line<T: DeserializeOwned>(r: &mut BufReader<TcpStream>) -> Result<Option<T>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim_end())?))
}

/// Bind a local listener on a random port.
pub fn listen() -> Result<(TcpListener, u16)> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let port = l.local_addr()?.port();
    Ok((l, port))
}

/// Accept one connection and check its token line.
pub fn accept_auth(l: &TcpListener, token: &str) -> Result<(TcpStream, BufReader<TcpStream>)> {
    let (s, _) = l.accept()?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    if line.trim_end() != token {
        bail!("bad token");
    }
    s.set_read_timeout(None)?;
    Ok((s, r))
}

pub fn connect(port: u16, token: &str) -> Result<(TcpStream, BufReader<TcpStream>)> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)).context("connect")?;
    s.set_nodelay(true)?;
    s.write_all(format!("{token}\n").as_bytes())?;
    let r = BufReader::new(s.try_clone()?);
    Ok((s, r))
}
