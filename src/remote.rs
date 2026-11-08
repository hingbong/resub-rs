//! Minimal HTTP/1.1 client for driving a resub server on another machine.
//!
//! The NAS runs "resub serve", the window on the PC talks to it. Only the protocol the
//! two ends already share is implemented: plain HTTP, JSON bodies, no TLS (LAN only).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

pub struct Endpoint {
    host: String,
    port: u16,
    label: String,
}

impl Endpoint {
    /// Accepts "nas", "nas:8080" or "http://nas:8080".
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Err("远程地址为空".into());
        }
        let s = s.strip_prefix("http://").unwrap_or(s);
        let s = s.split('/').next().unwrap_or(s);
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>().map_err(|_| format!("端口不对: {p}"))?,
            ),
            None => (s.to_string(), 8080),
        };
        if host.is_empty() {
            return Err("远程地址缺少主机名".into());
        }
        Ok(Self {
            label: format!("{host}:{port}"),
            host,
            port,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.label)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Tiny chunked decoder; the server sets Content-Length, but a proxy might not.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(pos) = find(rest, b"\r\n") else { break };
        let hex = rest[..pos].split(|&c| c == b';').next().unwrap_or(b"");
        let parsed = std::str::from_utf8(hex)
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim(), 16).ok());
        let Some(size) = parsed else { break };
        if size == 0 {
            break;
        }
        let start = pos + 2;
        if rest.len() < start + size {
            break;
        }
        out.extend_from_slice(&rest[start..start + size]);
        let after = start + size;
        rest = if rest.len() >= after + 2 {
            &rest[after + 2..]
        } else {
            &[]
        };
    }
    out
}

fn request(
    ep: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let addr = format!("{}:{}", ep.host, ep.port);
    let mut last = String::new();
    let mut stream = None;
    for sa in addr
        .to_socket_addrs()
        .map_err(|e| format!("解析 {addr} 失败: {e}"))?
    {
        match TcpStream::connect_timeout(&sa, Duration::from_secs(5)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let mut stream = stream.ok_or_else(|| format!("连接 {addr} 失败: {last}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(Duration::from_secs(15))).ok();

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        ep.label
    );
    if let Some(b) = body {
        head.push_str("Content-Type: application/json; charset=utf-8\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .map_err(|e| format!("发送请求失败: {e}"))?;
    if let Some(b) = body {
        stream
            .write_all(b.as_bytes())
            .map_err(|e| format!("发送请求失败: {e}"))?;
    }
    stream.flush().ok();

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取响应失败: {e}"))?;

    let split = find(&raw, b"\r\n\r\n").ok_or("响应格式不对")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut payload = raw[split + 4..].to_vec();
    let status = head.lines().next().unwrap_or("").to_string();
    if !status.contains(" 200") {
        return Err(format!("服务端返回: {status}"));
    }
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        payload = dechunk(&payload);
    }
    Ok(String::from_utf8_lossy(&payload).to_string())
}

fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// List a directory on the remote machine. An empty path returns its roots.
pub fn browse(ep: &Endpoint, path: &str) -> Result<Value, String> {
    let q = encode_path(path);
    let body = request(
        ep,
        "GET",
        &format!("/api/browse?path={q}&files=1"),
        None,
        Duration::from_secs(20),
    )?;
    serde_json::from_str(&body).map_err(|e| format!("解析响应失败: {e}"))
}

/// Ask the remote machine to extract subtitles from one file or a whole directory.
/// Long timeout: the work happens on that machine's own disk.
pub fn extract(ep: &Endpoint, file: &str, out: &str, skip_existing: bool) -> Result<Value, String> {
    let body = json!({
        "file": file,
        "out": out,
        "skip_existing": skip_existing,
    })
    .to_string();
    let text = request(
        ep,
        "POST",
        "/api/extract",
        Some(&body),
        Duration::from_secs(6 * 3600),
    )?;
    serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))
}

/// The rename flow, previewed or applied on the remote machine.
pub fn plan(
    ep: &Endpoint,
    videos: &str,
    subs: &str,
    suffix: &str,
    mode: &str,
    execute: bool,
) -> Result<Value, String> {
    let body = json!({
        "videos": videos,
        "subs": subs,
        "suffix": suffix,
        "mode": mode,
    })
    .to_string();
    let route = if execute { "/api/apply" } else { "/api/plan" };
    let text = request(ep, "POST", route, Some(&body), Duration::from_secs(600))?;
    serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))
}
