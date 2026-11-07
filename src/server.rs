//! Tiny local HTTP server. The browser is only the UI; all filesystem access happens
//! here, which is why this works in Firefox (no File System Access API needed).

use std::io;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::rename::{self, Mode};

const INDEX_HTML: &str = include_str!("../ui/index.html");

#[derive(Deserialize)]
struct PlanReq {
    videos: String,
    subs: String,
    #[serde(default)]
    suffix: String,
    #[serde(default)]
    mode: String,
}

#[derive(Deserialize)]
struct ExtractReq {
    file: String,
    #[serde(default)]
    out: String,
}

pub fn run(port: u16, open: bool) -> io::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| io::Error::new(io::ErrorKind::AddrInUse, e.to_string()))?;
    let actual = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);
    let url = format!("http://127.0.0.1:{actual}/");
    println!("resub 已启动: {url}  (Ctrl+C 退出)");
    if open {
        open_browser(&url);
    }
    for request in server.incoming_requests() {
        if let Err(e) = handle(request) {
            eprintln!("请求处理失败: {e}");
        }
    }
    Ok(())
}

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap()
}

fn html_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap()
}

fn reply(req: Request, body: String, header: Header) -> io::Result<()> {
    let _ = req.respond(Response::from_string(body).with_header(header));
    Ok(())
}

fn reply_json(req: Request, v: Value) -> io::Result<()> {
    reply(req, v.to_string(), json_header())
}

fn read_body(req: &mut Request) -> io::Result<String> {
    let mut s = String::new();
    req.as_reader().read_to_string(&mut s)?;
    Ok(s)
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = &s[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return Some(decode(v));
        }
    }
    None
}

fn handle(mut req: Request) -> io::Result<()> {
    let method = req.method().clone();
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url.clone(), String::new()),
    };

    match (&method, path.as_str()) {
        (Method::Get, "/") => reply(req, INDEX_HTML.to_string(), html_header()),
        (Method::Get, "/api/browse") => {
            let p = query_param(&query, "path").unwrap_or_default();
            reply_json(req, browse(&p))
        }
        (Method::Post, "/api/plan") => {
            let body = read_body(&mut req)?;
            reply_json(req, handle_plan(&body, false))
        }
        (Method::Post, "/api/apply") => {
            let body = read_body(&mut req)?;
            reply_json(req, handle_plan(&body, true))
        }
        (Method::Post, "/api/extract") => {
            let body = read_body(&mut req)?;
            reply_json(req, handle_extract(&body))
        }
        _ => reply_json(req, json!({ "error": "not found" })),
    }
}

fn roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut v = Vec::new();
        for c in b'A'..=b'Z' {
            let p = format!("{}:\\", c as char);
            if std::path::Path::new(&p).is_dir() {
                v.push(p);
            }
        }
        return v;
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

fn browse(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({ "path": "", "parent": Value::Null, "dirs": roots(), "files": [] });
    }
    let mut path = PathBuf::from(raw);
    if path.is_file() {
        if let Some(p) = path.parent() {
            path = p.to_path_buf();
        }
    }
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    match std::fs::read_dir(&path) {
        Ok(rd) => {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if entry.path().is_dir() {
                    dirs.push(name);
                } else {
                    files.push(name);
                }
            }
        }
        Err(e) => {
            return json!({ "path": path.to_string_lossy(), "error": e.to_string(), "dirs": [], "files": [] });
        }
    }
    dirs.sort_by_key(|s| s.to_lowercase());
    files.sort_by_key(|s| s.to_lowercase());
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| String::new());
    json!({
        "path": path.to_string_lossy(),
        "parent": if parent.is_empty() { Value::Null } else { Value::String(parent) },
        "dirs": dirs,
        "files": files,
    })
}

fn handle_plan(body: &str, execute: bool) -> Value {
    let req: PlanReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let videos_dir = PathBuf::from(&req.videos);
    let subs_dir = PathBuf::from(&req.subs);
    if !videos_dir.is_dir() {
        return json!({ "error": format!("视频目录不存在: {}", videos_dir.display()) });
    }
    if !subs_dir.is_dir() {
        return json!({ "error": format!("字幕目录不存在: {}", subs_dir.display()) });
    }
    let mode = Mode::parse(&req.mode).unwrap_or(Mode::Copy);
    let suffix = if req.suffix.trim().is_empty() {
        ".chs".to_string()
    } else {
        req.suffix.trim().to_string()
    };

    let videos = rename::collect(&videos_dir, rename::DEFAULT_VIDEO_EXTS);
    let subs = rename::collect(&subs_dir, rename::DEFAULT_SUB_EXTS);
    let plan = rename::plan(&videos, &subs, mode, &suffix);
    if !execute {
        return json!({ "plan": plan });
    }
    let applied = rename::apply(&plan, mode);
    let ok = applied.iter().filter(|a| a.ok).count();
    json!({ "plan": plan, "applied": applied, "ok": ok, "total": applied.len() })
}

fn handle_extract(body: &str) -> Value {
    let req: ExtractReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let file = PathBuf::from(&req.file);
    if !file.is_file() {
        return json!({ "error": format!("文件不存在: {}", file.display()) });
    }
    let out = if req.out.trim().is_empty() {
        file.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(req.out.trim())
    };
    match crate::extract_all(&file, &out) {
        Ok(tracks) => json!({ "tracks": tracks, "out": out.to_string_lossy() }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}
