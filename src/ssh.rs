//! Drive a resub binary on another machine by shelling out to the system ssh.
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
    // Spelling the argv out on failure is the difference between a report someone can
    // act on and "it did not work".
    let shown = format!("{cmd:?}");
    cmd.stdin(Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("无法执行 {what}: {e}（系统里没有这个命令？）\n命令: {shown}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(if msg.is_empty() {
            format!("{what} 退出码 {}\n命令: {shown}", out.status.code().unwrap_or(-1))
        } else {
            format!("{msg}\n命令: {shown}")
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

/// A single round trip that answers "why can it not list /volume1" without guessing:
/// who we are, where home is, what the permissions look like, and whether the binary runs.
pub fn diagnose(t: &Target, bin: &str) -> Result<String, String> {
    let remote = format!(
        "echo \"user=$(id -un 2>/dev/null || echo '?')\"; \
         echo \"home=$HOME\"; \
         echo '--- ls -ld $HOME ---'; ls -ld \"$HOME\" 2>&1; \
         echo '--- ls -ld /volume1 ---'; ls -ld /volume1 2>&1; \
         echo '--- ls /volume1 ---'; ls -l /volume1 2>&1 | head -20; \
         echo '--- ls -l {bin} ---'; ls -l {bin} 2>&1; \
         echo '--- run ---'; {bin} --help 2>&1 | head -3; \
         exit 0"
    );
    let out = run(t.ssh(&remote), "ssh")?;
    Ok(out)
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
///
/// Deliberately not scp: OpenSSH 9 made scp speak SFTP first, and a NAS sshd normally
/// has no sftp subsystem enabled, which fails with "subsystem request failed". The
/// shell is always there, so the bytes go through stdin of a plain ssh session.
pub fn deploy(t: &Target, bin: &str, local: &Path) -> Result<String, String> {
    let size = std::fs::metadata(local)
        .map_err(|e| format!("读取 {} 失败: {e}", local.display()))?
        .len();

    let mut c = t.ssh(&format!("cat > {bin}"));
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|e| format!("无法执行 ssh: {e}（系统里没有 ssh？）"))?;
    // If the remote side dies first (unwritable path, full disk) our write breaks with
    // a bare EPIPE, which says nothing useful. Keep going, collect its stderr and let
    // that speak instead.
    let mut copy_err = None;
    {
        let mut file = std::fs::File::open(local)
            .map_err(|e| format!("读取 {} 失败: {e}", local.display()))?;
        match child.stdin.take() {
            Some(mut sink) => {
                if let Err(e) = std::io::copy(&mut file, &mut sink) {
                    copy_err = Some(format!("传输中断: {e}"));
                }
            }
            None => copy_err = Some("拿不到 ssh 的标准输入".to_string()),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("等待 ssh 结束失败: {e}"))?;
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(e) = copy_err {
        return Err(if err.is_empty() {
            e
        } else {
            format!("{e}\n远程: {err}")
        });
    }
    if !out.status.success() {
        return Err(if err.is_empty() {
            format!("写入 {bin} 失败，退出码 {}", out.status.code().unwrap_or(-1))
        } else {
            err
        });
    }

    // Byte count is the cheapest proof the stream arrived intact.
    let remote = format!(
        "chmod +x {bin} 2>/dev/null; S=$(wc -c < {bin} 2>/dev/null); \
         if [ \"$S\" != \"{size}\" ]; then echo RESUB_SIZE; \
         elif {bin} --help >/dev/null 2>&1; then echo RESUB_OK; else echo RESUB_BROKEN; fi"
    );
    let check = run(t.ssh(&remote), "ssh")?;
    if check.contains("RESUB_SIZE") {
        return Err(format!(
            "传上去的字节数不对（本地 {size} 字节）。可能是连接中断，重试一次。"
        ));
    }
    if check.contains("RESUB_BROKEN") {
        return Err(format!("{bin} 传上去了但跑不起来，可能是架构不对。"));
    }
    if !check.contains("RESUB_OK") {
        return Err(format!("部署后自检没有通过，原样返回: {}", check.trim()));
    }
    Ok(format!("已上传到 {}:{bin}（{size} 字节）", t.dest()))
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
