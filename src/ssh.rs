//! Drive a resub binary on another machine by shelling out to the system ssh/scp.
//!
//! Windows 10 1809+ and every Linux ship OpenSSH, so this needs no new dependency and
//! no daemon on the other side: each action is a single ssh invocation that prints JSON
//! on stdout. Nothing but the command string ever crosses the network.
//!
//! Authentication is by key. BatchMode makes ssh fail fast instead of waiting on a
//! password prompt that a windowed program can never answer.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

pub struct Target {
    user: String,
    host: String,
    /// None means "say nothing", so ssh reads Port from ~/.ssh/config.
    port: Option<u16>,
}

impl Target {
    /// Accepts "nas", "user@nas" and "user@nas:2222".
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Err("NAS 地址为空".into());
        }
        if s.starts_with("http://") {
            return Err("这是 HTTP 地址，不是 SSH 地址".into());
        }
        let (user, rest) = match s.rsplit_once('@') {
            Some((u, r)) => (u.to_string(), r.to_string()),
            None => (String::new(), s.to_string()),
        };
        // No port in the address means the ssh config keeps the last word. Passing
        // -p 22 unconditionally would override a Port line that is already there.
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                Some(p.parse::<u16>().map_err(|_| format!("端口不对: {p}"))?),
            ),
            None => (rest, None),
        };
        if host.is_empty() {
            return Err("NAS 地址缺少主机名".into());
        }
        Ok(Self { user, host, port })
    }

    pub fn dest(&self) -> String {
        if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    fn options(&self) -> Vec<String> {
        ["-o", "BatchMode=yes"]
            .iter()
            .chain(["-o", "ConnectTimeout=8"].iter())
            .chain(["-o", "StrictHostKeyChecking=accept-new"].iter())
            .chain(["-o", "NumberOfPasswordPrompts=0"].iter())
            .map(|s| s.to_string())
            .collect()
    }

    fn ssh(&self, remote: &str) -> Command {
        let mut c = Command::new("ssh");
        if let Some(p) = self.port {
            c.arg("-p").arg(p.to_string());
        }
        for o in self.options() {
            c.arg(o);
        }
        c.arg(self.dest()).arg(remote);
        c.stdin(Stdio::null());
        quiet(&mut c);
        c
    }
}

/// Keep the child from flashing a console window when resub.exe is started from Explorer.
#[cfg(windows)]
fn quiet(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn quiet(_c: &mut Command) {}

/// Wrap a remote path for a POSIX shell. Single quotes disable every expansion, so a
/// directory named "a b&c" survives intact.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn run(mut cmd: Command, what: &str) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("无法执行 {what}: {e}（系统里没有这个命令？）"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(if msg.is_empty() {
            format!("{what} 退出码 {}", out.status.code().unwrap_or(-1))
        } else {
            msg
        });
    }
    Ok(stdout)
}

/// A login banner or a shell notice can end up in front of our own output.
fn last_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .next_back()
        .unwrap_or("")
        .to_string()
}

/// The JSON payload is always one line; anything else is the shell's own noise.
fn last_json(s: &str) -> Option<&str> {
    s.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{') && l.ends_with('}'))
        .filter(|l| serde_json::from_str::<Value>(l).is_ok())
        .next_back()
}

/// Check that the remote binary exists and runs.
///
/// The tokens are searched for anywhere in the output rather than taken from the last
/// line: a login banner or a shell notice may turn up before or after our own text,
/// and an implementation detail of the other end must not be mistaken for an answer.
pub fn probe(t: &Target, bin: &str) -> Result<String, String> {
    let remote = format!(
        "if [ -x {bin} ]; then {bin} --help >/dev/null 2>&1 && echo RESUB_OK || echo RESUB_BROKEN; else echo RESUB_MISSING; fi"
    );
    let out = run(t.ssh(&remote), "ssh")?;
    if out.contains("RESUB_OK") {
        Ok(format!("{} 上的 resub 可用", t.dest()))
    } else if out.contains("RESUB_MISSING") {
        Err(format!(
            "{} 上还没有 resub（{bin}）。先点「部署」。",
            t.dest()
        ))
    } else if out.contains("RESUB_BROKEN") {
        Err(format!(
            "{} 上的文件跑不起来（{bin}），可能架构不对或没有执行权限",
            t.dest()
        ))
    } else {
        Err(format!("意外输出: {}", last_line(&out)))
    }
}

/// Fetch the x86_64 Linux build into a local path. Uses curl, which ships with
/// Windows 10 1803+ and with virtually every Linux.
pub fn fetch_binary(dest: &std::path::Path) -> Result<(), String> {
    let mut c = Command::new("curl");
    c.args(["-L", "--fail", "--silent", "--show-error"])
        .arg("-o")
        .arg(dest)
        .arg(LINUX_ASSET_URL);
    c.stdin(Stdio::null());
    quiet(&mut c);
    run(c, "curl")?;
    if !dest.is_file() {
        return Err("curl 没有生成文件".into());
    }
    Ok(())
}

pub const LINUX_ASSET_URL: &str =
    "https://github.com/hingbong/resub-rs/releases/download/nightly/resub-linux-x86_64";

/// Copy the local Linux binary over and make it executable.
pub fn deploy(t: &Target, bin: &str, local: &Path) -> Result<String, String> {
    let mut c = Command::new("scp");
    if let Some(p) = t.port {
        c.arg("-P").arg(p.to_string());
    }
    for o in t.options() {
        c.arg(o);
    }
    c.arg(local).arg(format!("{}:{bin}", t.dest()));
    c.stdin(Stdio::null());
    quiet(&mut c);
    run(c, "scp")?;

    let remote = format!("chmod +x {bin} && {bin} --help >/dev/null && echo ok");
    run(t.ssh(&remote), "ssh")?;
    Ok(format!("已上传到 {}:{bin}", t.dest()))
}

/// Directory listing, in the same shape the HTTP route returns.
pub fn ls(t: &Target, bin: &str, path: &str) -> Result<Value, String> {
    let remote = format!("{bin} ls {}", shq(path));
    let out = run(t.ssh(&remote), "ssh")?;
    let json = last_json(&out)
        .ok_or_else(|| format!("输出里没有 JSON，原样返回: {}", out.trim()))?;
    serde_json::from_str(json).map_err(|e| format!("解析输出失败: {e}"))
}

/// Extract on the remote machine; returns its JSON summary.
pub fn extract(
    t: &Target,
    bin: &str,
    path: &str,
    out: &str,
    skip_existing: bool,
) -> Result<Value, String> {
    let mut remote = format!("{bin} extract {} --json", shq(path));
    if !out.trim().is_empty() {
        remote.push_str(&format!(" --out {}", shq(out.trim())));
    }
    if skip_existing {
        remote.push_str(" --skip-existing");
    }
    let stdout = run(t.ssh(&remote), "ssh")?;
    let json = last_json(&stdout)
        .ok_or_else(|| format!("输出里没有 JSON，原样返回: {}", stdout.trim()))?;
    serde_json::from_str(json).map_err(|e| format!("解析输出失败: {e}"))
}
