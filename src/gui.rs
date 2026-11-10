//! Native window front-end (eframe/egui). Everything is compiled into the executable:
//! no browser, no server, no runtime to install.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;
use serde_json::Value;
use walkdir::WalkDir;

use crate::remote::{self, Endpoint};
use crate::rename::{self, Mode, Status};

/// Look for a Noto Sans family font to use as the primary UI font. Noto is commonly
/// present on Linux and often installed on Windows; when it is missing we keep
/// eframe's bundled faces plus the system font provider, which still supplies CJK.
/// Only .ttf/.otf are accepted: egui cannot reliably parse font collections.
fn find_noto() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(windir) = std::env::var("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        } else {
            dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(&home).join(".local").join("share").join("fonts"));
            dirs.push(PathBuf::from(&home).join(".fonts"));
        }
    }

    let mut best: Option<(u8, PathBuf)> = None;
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir).max_depth(3).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !(name.ends_with(".ttf") || name.ends_with(".otf")) || !name.contains("noto") {
                continue;
            }
            let score = if name.contains("notosanscjk") || (name.contains("notosans") && name.contains("cjk")) {
                0
            } else if name.contains("sc") {
                1
            } else if name.contains("notosans") {
                2
            } else {
                continue;
            };
            if best.as_ref().map(|(b, _)| score < *b).unwrap_or(true) {
                best = Some((score, entry.path().to_path_buf()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Prefer Noto Sans for proportional text, keeping the bundled faces as fallback.
fn install_fonts(ctx: &egui::Context) {
    let Some(path) = find_noto() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "noto".to_owned());
    // Append rather than prepend for monospace so tabular alignment is preserved.
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("noto".to_owned());
    ctx.set_fonts(fonts);
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 740.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("resub"),
        ..Default::default()
    };
    eframe::run_native(
        "resub",
        options,
        Box::new(|cc| {
            install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::default()))
        }),
    )
}

/// Native folder picker. This is the OS dialog, so mapped drives, UNC paths and the
/// Network neighbourhood are all reachable - which the in-app browser could not do.
fn pick_folder(title: &str, current: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        let p = Path::new(trimmed);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        } else if let Some(parent) = p.parent() {
            if parent.is_dir() {
                dialog = dialog.set_directory(parent);
            }
        }
    }
    dialog.pick_folder()
}

fn pick_file(title: &str, current: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("Matroska", &["mkv"])
        .add_filter("视频", &["mkv", "mp4", "m2ts", "ts", "avi"]);
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        let p = Path::new(trimmed);
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        } else if let Some(parent) = p.parent() {
            if parent.is_dir() {
                dialog = dialog.set_directory(parent);
            }
        }
    }
    dialog.pick_file()
}

/// Messages from the extraction worker thread.
enum Msg {
    Progress { done: usize, total: usize, file: String },
    Tracks(Vec<crate::ExtractedTrack>),
    Error(String),
    Finished,
}

struct App {
    videos: String,
    subs: String,
    suffix: String,
    mode: Mode,
    plan: Option<rename::Plan>,
    message: String,
    mkv: String,
    out: String,
    extract_rx: Option<Receiver<Msg>>,
    extract_progress: String,
    extract_tracks: Vec<crate::ExtractedTrack>,
    extract_errors: Vec<String>,
    /// "user@nas:2222" to drive the other machine over ssh, or "http://nas:8080" to
    /// talk to a running resub server. Empty means work locally.
    remote: String,
    remote_status: String,
    /// Path of the resub binary on the other machine.
    nas_bin: String,
    /// Local copy of the Linux binary, uploaded by the deploy button.
    nas_local: String,
    skip_existing: bool,
    browse: Option<Browse>,
    job_rx: Option<Receiver<Job>>,
    /// Where the remote browser was last time, so it reopens there.
    remote_dir: String,
    diag: Option<String>,
    /// Serialised form of everything worth keeping; compared against the live state.
    saved: String,
    dirty_since: Option<std::time::Instant>,
}

impl Default for App {
    fn default() -> Self {
        let cfg = crate::config::Config::load();
        let nas_bin = if cfg.nas_bin.trim().is_empty() {
            "~/resub-linux-x86_64".to_string()
        } else {
            cfg.nas_bin
        };
        let nas_local = if cfg.nas_local.trim().is_empty() {
            guess_local_bin()
        } else {
            cfg.nas_local
        };
        let mut app = Self {
            videos: String::new(),
            subs: String::new(),
            suffix: ".chs".to_string(),
            mode: Mode::Copy,
            plan: None,
            message: String::new(),
            mkv: cfg.mkv,
            out: cfg.out,
            extract_rx: None,
            extract_progress: String::new(),
            extract_tracks: Vec::new(),
            extract_errors: Vec::new(),
            remote: cfg.remote,
            remote_status: String::new(),
            nas_bin,
            nas_local,
            skip_existing: cfg.skip_existing,
            browse: None,
            job_rx: None,
            remote_dir: cfg.remote_dir,
            diag: None,
            saved: String::new(),
            dirty_since: None,
        };
        app.saved = app.snapshot();
        app
    }
}

fn status_ui(status: Status) -> (&'static str, egui::Color32) {
    match status {
        Status::Ready => ("就绪", egui::Color32::from_rgb(60, 130, 246)),
        Status::TargetExists => ("目标已存在", egui::Color32::from_rgb(196, 140, 0)),
        Status::AlreadyNamed => ("已是目标名", egui::Color32::from_rgb(120, 120, 120)),
        Status::NoVideo => ("找不到视频", egui::Color32::from_rgb(200, 60, 60)),
        Status::Ambiguous => ("集数重复", egui::Color32::from_rgb(196, 140, 0)),
    }
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// Names out of a JSON array field, tolerating anything unexpected.
fn names(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Directory listing fetched from the other machine.
#[derive(Clone)]
struct Browse {
    path: String,
    input: String,
    parent: Option<String>,
    dirs: Vec<String>,
    files: Vec<String>,
    error: Option<String>,
    loading: bool,
}

impl Browse {
    fn failed(path: String, error: String) -> Self {
        Self {
            input: path.clone(),
            path,
            parent: None,
            dirs: Vec::new(),
            files: Vec::new(),
            error: Some(error),
            loading: false,
        }
    }

    fn from_json(v: &Value, fallback: &str) -> Self {
        let path = v
            .get("path")
            .and_then(|x| x.as_str())
            .unwrap_or(fallback)
            .to_string();
        Self {
            input: path.clone(),
            path,
            parent: v.get("parent").and_then(|x| x.as_str()).map(|s| s.to_string()),
            dirs: names(v, "dirs"),
            files: names(v, "files"),
            error: v.get("error").and_then(|x| x.as_str()).map(|s| s.to_string()),
            loading: false,
        }
    }
}

/// Work handed to a background thread: an ssh round trip is slow enough to be visible.
enum Job {
    Browse(Browse),
    Status(String),
    /// Deployment finished; remembers where the binary came from so the next one
    /// does not download it again.
    Deployed { local: String, msg: String },
    Diag(String),
    Failed(String),
}

/// Pick any file, no extension filter (the Linux binary has none worth filtering on).
fn pick_any(title: &str, current: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        if let Some(parent) = Path::new(trimmed).parent() {
            if parent.is_dir() {
                dialog = dialog.set_directory(parent);
            }
        }
    }
    dialog.pick_file()
}

/// Look for the Linux binary sitting next to resub.exe, which is where people put it.
fn guess_local_bin() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("resub-linux-x86_64");
            if p.is_file() {
                return p.to_string_lossy().to_string();
            }
        }
    }
    String::new()
}

fn as_string(p: PathBuf) -> String {
    p.to_string_lossy().to_string()
}

impl App {
    fn preview(&mut self) {
        self.plan = None;
        self.message.clear();
        let videos_dir = PathBuf::from(self.videos.trim());
        let subs_dir = PathBuf::from(self.subs.trim());
        if !videos_dir.is_dir() {
            self.message = format!("视频目录不存在: {}", videos_dir.display());
            return;
        }
        if !subs_dir.is_dir() {
            self.message = format!("字幕目录不存在: {}", subs_dir.display());
            return;
        }
        let videos = rename::collect(&videos_dir, rename::DEFAULT_VIDEO_EXTS);
        let subs = rename::collect(&subs_dir, rename::DEFAULT_SUB_EXTS);
        if videos.is_empty() {
            self.message = "没有找到视频文件".to_string();
            return;
        }
        if subs.is_empty() {
            self.message = "没有找到字幕文件".to_string();
            return;
        }
        let suffix = if self.suffix.trim().is_empty() {
            ".chs".to_string()
        } else {
            self.suffix.trim().to_string()
        };
        let plan = rename::plan(&videos, &subs, self.mode, &suffix);
        let ready = plan.items.iter().filter(|i| i.status == Status::Ready).count();
        self.message = format!("可执行 {ready} 条 / 共 {} 条", plan.items.len());
        self.plan = Some(plan);
    }

    fn apply(&mut self) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let applied = rename::apply(&plan, self.mode);
        let ok = applied.iter().filter(|a| a.ok).count();
        let failed: Vec<String> = applied
            .iter()
            .filter(|a| !a.ok)
            .map(|a| format!("{}: {}", a.target, a.error.clone().unwrap_or_default()))
            .collect();
        self.plan = None;
        self.message = if failed.is_empty() {
            format!("已执行 {ok} 条")
        } else {
            format!("已执行 {ok} 条，失败 {}: {}", failed.len(), failed.join("; "))
        };
    }

    /// Extraction runs on a worker thread: a folder of MKVs takes seconds to minutes
    /// and would otherwise freeze the window.
    fn start_extract(&mut self, ctx: egui::Context) {
        if self.extract_rx.is_some() {
            return;
        }
        let raw = self.mkv.trim().to_string();
        let out = self.out.trim().to_string();
        let skip = self.skip_existing;

        // Remote: one request. The other machine walks its own disk, so nothing is
        // pulled across the network.
        if !self.remote.trim().is_empty() {
            self.start_remote_extract(ctx, raw, out, skip);
            return;
        }

        let path = PathBuf::from(&raw);
        if !path.exists() {
            self.extract_progress = format!("路径不存在: {raw}");
            return;
        }
        let files = crate::collect_mkv(&path);
        if files.is_empty() {
            self.extract_progress = format!("在 {raw} 里没有找到 .mkv 文件");
            return;
        }
        let total = files.len();
        self.extract_tracks.clear();
        self.extract_errors.clear();
        self.extract_progress = format!("0/{total}");

        let (tx, rx) = std::sync::mpsc::channel();
        self.extract_rx = Some(rx);
        std::thread::spawn(move || {
            for (i, file) in files.iter().enumerate() {
                let name = file
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let _ = tx.send(Msg::Progress {
                    done: i,
                    total,
                    file: name,
                });
                let out_dir = if out.is_empty() {
                    file.parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."))
                } else {
                    PathBuf::from(&out)
                };
                match crate::extract_all(file, &out_dir, None, skip) {
                    Ok(tracks) => {
                        let _ = tx.send(Msg::Tracks(tracks));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(format!("{}: {e}", file.display())));
                    }
                }
                ctx.request_repaint();
            }
            let _ = tx.send(Msg::Finished);
            ctx.request_repaint();
        });
    }

    fn poll_extract(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.extract_rx.take() else {
            return;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Progress { done, total, file }) => {
                    self.extract_progress = format!("{done}/{total}  {file}");
                }
                Ok(Msg::Tracks(t)) => self.extract_tracks.extend(t),
                Ok(Msg::Error(e)) => self.extract_errors.push(e),
                Ok(Msg::Finished) => {
                    finished = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.extract_progress = format!(
                "完成：{} 条轨道{}",
                self.extract_tracks.len(),
                if self.extract_errors.is_empty() {
                    String::new()
                } else {
                    format!("，{} 个文件失败", self.extract_errors.len())
                }
            );
        } else {
            self.extract_rx = Some(rx);
            ctx.request_repaint();
        }
    }

    fn endpoint(&mut self) -> Option<Endpoint> {
        match Endpoint::parse(&self.remote) {
            Ok(ep) => {
                self.remote_status.clear();
                Some(ep)
            }
            Err(e) => {
                self.remote_status = e;
                None
            }
        }
    }

    /// Everything worth keeping, in one comparable string.
    fn snapshot(&self) -> String {
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            self.remote,
            self.nas_bin,
            self.nas_local,
            self.skip_existing,
            self.mkv,
            self.out,
            self.remote_dir
        )
    }

    fn save_config(&self) {
        let cfg = crate::config::Config {
            remote: self.remote.clone(),
            nas_bin: self.nas_bin.clone(),
            nas_local: self.nas_local.clone(),
            skip_existing: self.skip_existing,
            mkv: self.mkv.clone(),
            out: self.out.clone(),
            remote_dir: self.remote_dir.clone(),
        };
        cfg.save();
    }

    /// Write the settings out once the user stops fiddling, not on every keystroke.
    fn persist(&mut self, ctx: &egui::Context) {
        let now = self.snapshot();
        if now != self.saved {
            self.saved = now;
            self.dirty_since = Some(std::time::Instant::now());
        }
        if let Some(t) = self.dirty_since {
            if t.elapsed() >= std::time::Duration::from_millis(600) {
                self.dirty_since = None;
                self.save_config();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
    }

    /// "http://..." means a resub server; anything else is an ssh target.
    fn is_http(&self) -> bool {
        self.remote.trim().starts_with("http://")
    }

    fn ssh_target(&mut self) -> Option<crate::ssh::Target> {
        match crate::ssh::Target::parse(&self.remote) {
            Ok(t) => Some(t),
            Err(e) => {
                self.remote_status = e;
                None
            }
        }
    }

    /// Run a blocking round trip off the UI thread and feed the result back.
    fn spawn_job<F>(&mut self, ctx: &egui::Context, f: F)
    where
        F: FnOnce() -> Job + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.job_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.job_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(Job::Browse(b)) => {
                    self.remote_dir = b.path.clone();
                    self.browse = Some(b);
                }
                Ok(Job::Diag(t)) => {
                    self.remote_status.clear();
                    self.diag = Some(t);
                }
                Ok(Job::Status(s)) => {
                    self.remote_status = s;
                    if let Some(st) = self.browse.as_mut() {
                        st.loading = false;
                    }
                }
                Ok(Job::Deployed { local, msg }) => {
                    self.nas_local = local;
                    self.remote_status = format!("{msg}，点「连接」验证");
                    if let Some(st) = self.browse.as_mut() {
                        st.loading = false;
                    }
                }
                Ok(Job::Failed(e)) => {
                    self.remote_status = e.clone();
                    if let Some(st) = self.browse.as_mut() {
                        st.loading = false;
                        st.error = Some(e);
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.job_rx = Some(rx);
                    ctx.request_repaint();
                    return;
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn test_remote(&mut self, ctx: &egui::Context) {
        if !self.is_http() {
            let Some(t) = self.ssh_target() else {
                return;
            };
            let bin = self.nas_bin.trim().to_string();
            self.remote_status = format!("正在连接 {} …", t.dest());
            self.spawn_job(ctx, move || match crate::ssh::probe(&t, &bin) {
                Ok(s) => Job::Status(s),
                Err(e) => Job::Failed(format!("连接失败: {e}")),
            });
            return;
        }
        let Some(ep) = self.endpoint() else {
            return;
        };
        match remote::browse(&ep, "") {
            Ok(v) => {
                let n = names(&v, "dirs").len();
                self.remote_status = format!("已连接 {}（{} 个入口）", ep.label(), n);
            }
            Err(e) => self.remote_status = format!("连接失败: {e}"),
        }
    }

    /// Upload the Linux binary once; afterwards every action is a single ssh call.
    fn deploy(&mut self, ctx: &egui::Context) {
        if self.is_http() {
            self.remote_status = "HTTP 模式不需要部署，服务端已经跑着了".to_string();
            return;
        }
        let Some(t) = self.ssh_target() else {
            return;
        };
        let bin = self.nas_bin.trim().to_string();
        if bin.is_empty() {
            self.remote_status = "远程程序路径不能为空".to_string();
            return;
        }
        // Nothing picked locally? Fetch the release asset, so deployment is one click.
        let chosen = match self.nas_local.trim() {
            "" => None,
            local => {
                let p = PathBuf::from(local);
                if !p.is_file() {
                    self.remote_status = format!("找不到: {local}");
                    return;
                }
                Some(p)
            }
        };
        self.remote_status = match &chosen {
            Some(p) => format!("正在上传 {} …", p.display()),
            None => "本地没有二进制，先从 nightly 下载…".to_string(),
        };
        self.spawn_job(ctx, move || {
            let path = match chosen {
                Some(p) => p,
                None => {
                    let dest = std::env::temp_dir().join("resub-linux-x86_64");
                    if let Err(e) = crate::ssh::fetch_binary(&dest) {
                        return Job::Failed(format!("下载失败: {e}"));
                    }
                    dest
                }
            };
            match crate::ssh::deploy(&t, &bin, &path) {
                Ok(msg) => Job::Deployed {
                    local: path.to_string_lossy().to_string(),
                    msg,
                },
                Err(e) => Job::Failed(format!("部署失败: {e}")),
            }
        });
    }

    fn start_ssh_extract(&mut self, ctx: egui::Context, raw: String, out: String, skip: bool) {
        let Some(t) = self.ssh_target() else {
            return;
        };
        let bin = self.nas_bin.trim().to_string();
        if bin.is_empty() {
            self.extract_progress = "远程程序路径不能为空".to_string();
            return;
        }
        self.extract_tracks.clear();
        self.extract_errors.clear();
        self.extract_progress = format!("{} 正在处理 {raw}", t.dest());

        let (tx, rx) = std::sync::mpsc::channel();
        self.extract_rx = Some(rx);
        std::thread::spawn(move || {
            match crate::ssh::extract(&t, &bin, &raw, &out, skip) {
                Ok(v) => {
                    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                        let _ = tx.send(Msg::Error(err.to_string()));
                    } else {
                        let tracks: Vec<crate::ExtractedTrack> = v
                            .get("tracks")
                            .cloned()
                            .and_then(|t| serde_json::from_value(t).ok())
                            .unwrap_or_default();
                        let _ = tx.send(Msg::Tracks(tracks));
                        if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
                            for e in errs {
                                let _ = tx.send(Msg::Error(
                                    e.as_str().unwrap_or_default().to_string(),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(e));
                }
            }
            let _ = tx.send(Msg::Finished);
            ctx.request_repaint();
        });
    }

    fn start_remote_extract(&mut self, ctx: egui::Context, raw: String, out: String, skip: bool) {
        if !self.is_http() {
            self.start_ssh_extract(ctx, raw, out, skip);
            return;
        }
        let Some(ep) = self.endpoint() else {
            return;
        };
        self.extract_tracks.clear();
        self.extract_errors.clear();
        self.extract_progress = format!("{} 正在处理 {raw}", ep.label());

        let (tx, rx) = std::sync::mpsc::channel();
        self.extract_rx = Some(rx);
        std::thread::spawn(move || {
            match remote::extract(&ep, &raw, &out, skip) {
                Ok(v) => {
                    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                        let _ = tx.send(Msg::Error(err.to_string()));
                    } else {
                        let tracks: Vec<crate::ExtractedTrack> = v
                            .get("tracks")
                            .cloned()
                            .and_then(|t| serde_json::from_value(t).ok())
                            .unwrap_or_default();
                        let _ = tx.send(Msg::Tracks(tracks));
                        if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
                            for e in errs {
                                let _ = tx.send(Msg::Error(
                                    e.as_str().unwrap_or_default().to_string(),
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(e));
                }
            }
            let _ = tx.send(Msg::Finished);
            ctx.request_repaint();
        });
    }

    /// One round trip that prints who we are, what the paths look like and whether the
    /// binary runs, so "cannot list /volume1" can be answered instead of guessed at.
    fn diagnose(&mut self, ctx: &egui::Context) {
        if self.is_http() {
            self.remote_status = "HTTP 模式下服务端在跑就说明连得上，没有额外诊断".to_string();
            return;
        }
        let Some(t) = self.ssh_target() else {
            return;
        };
        let bin = self.nas_bin.trim().to_string();
        self.remote_status = "正在收集远端信息…".to_string();
        self.spawn_job(ctx, move || match crate::ssh::diagnose(&t, &bin) {
            Ok(s) => Job::Diag(s),
            Err(e) => Job::Failed(format!("诊断失败: {e}")),
        });
    }

    fn open_browse(&mut self, ctx: &egui::Context) {
        let typed = self.mkv.trim().to_string();
        // A Windows path left over from local mode means nothing on the other machine.
        let looks_remote = typed.starts_with('/') && !typed.contains('\\');
        let start = if looks_remote {
            typed
        } else if !self.remote_dir.trim().is_empty() {
            self.remote_dir.clone()
        } else {
            String::new()
        };
        self.load_browse(ctx, &start);
    }

    fn load_browse(&mut self, ctx: &egui::Context, path: &str) {
        if self.is_http() {
            let Some(ep) = self.endpoint() else {
                return;
            };
            let b = match remote::browse(&ep, path) {
                Ok(v) => Browse::from_json(&v, path),
                Err(e) => Browse::failed(path.to_string(), e),
            };
            self.remote_dir = b.path.clone();
            self.browse = Some(b);
            return;
        }

        let Some(t) = self.ssh_target() else {
            return;
        };
        let bin = self.nas_bin.trim().to_string();
        self.browse = Some(Browse {
            input: path.to_string(),
            path: path.to_string(),
            parent: None,
            dirs: Vec::new(),
            files: Vec::new(),
            error: None,
            loading: true,
        });
        let p = path.to_string();
        self.spawn_job(ctx, move || match crate::ssh::ls(&t, &bin, &p) {
            Ok(v) => Job::Browse(Browse::from_json(&v, &p)),
            Err(e) => Job::Failed(e),
        });
    }

    fn browse_window(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.browse.take() else {
            return;
        };
        let mut nav: Option<String> = None;
        let mut pick: Option<String> = None;
        let mut close = false;
        egui::Window::new("远程目录")
            .default_width(680.0)
            .default_height(480.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("↑ 上级").clicked() {
                        if let Some(p) = st.parent.clone() {
                            nav = Some(p);
                        }
                    }
                    if ui.button("刷新").clicked() {
                        nav = Some(st.path.clone());
                    }
                    ui.add_sized(
                        [330.0, ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut st.input),
                    );
                    if ui.button("转到").clicked() {
                        nav = Some(st.input.clone());
                    }
                    if ui
                        .button("✓ 用这个目录")
                        .on_hover_text("把当前所在的目录整个作为抽取输入")
                        .clicked()
                    {
                        pick = Some(st.path.clone());
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });

                if st.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在连接…");
                    });
                }

                let cur = if st.path.is_empty() {
                    "/".to_string()
                } else {
                    st.path.clone()
                };
                ui.label(format!("当前: {cur}"));
                if let Some(e) = &st.error {
                    ui.colored_label(egui::Color32::from_rgb(200, 60, 60), e);
                }
                ui.label(format!(
                    "{} 个子目录 · {} 个视频/字幕文件",
                    st.dirs.len(),
                    st.files.len()
                ));
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("remote_browse")
                    .max_height(360.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if !st.loading && st.dirs.is_empty() && st.files.is_empty() {
                            ui.weak(if st.error.is_some() {
                                "这个目录读不出来，具体原因见上面的红字。"
                            } else {
                                "这个目录是空的，或者里面没有视频/字幕文件。"
                            });
                        }
                        for d in &st.dirs {
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(false, format!("DIR  {d}"))
                                    .on_hover_text("点击进入这个目录")
                                    .clicked()
                                {
                                    nav = Some(join_path(&st.path, d));
                                }
                                if ui
                                    .small_button("选这个目录")
                                    .on_hover_text("直接把整个目录作为输入，不用再进去")
                                    .clicked()
                                {
                                    pick = Some(join_path(&st.path, d));
                                }
                            });
                        }
                        for f in &st.files {
                            if ui
                                .selectable_label(false, format!("FILE {f}"))
                                .on_hover_text("点击选用这个文件")
                                .clicked()
                            {
                                pick = Some(join_path(&st.path, f));
                            }
                        }
                    });
            });

        if st.loading {
            self.browse = Some(st);
            return;
        }
        if close {
            self.browse = None;
            return;
        }
        if let Some(p) = pick {
            self.mkv = p;
            self.browse = None;
            return;
        }
        if let Some(p) = nav {
            self.load_browse(ctx, &p);
            return;
        }
        self.browse = Some(st);
    }

    fn diag_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.diag.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("远端诊断")
            .default_width(760.0)
            .default_height(400.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("这些是在 NAS 上执行命令的真实输出，可以直接复制。");
                ui.separator();
                egui::ScrollArea::both()
                    .id_salt("diag_scroll")
                    .max_height(330.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.monospace(&text);
                    });
            });
        if !open {
            self.diag = None;
        }
    }

    fn section_rename(&mut self, ui: &mut egui::Ui) {
        ui.heading("批量改名");
        ui.add_space(4.0);
        egui::Grid::new("rename_grid")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("视频目录");
                ui.add_sized(
                    [560.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.videos)
                        .hint_text("包含视频文件的目录"),
                );
                if ui.button("浏览").clicked() {
                    if let Some(p) = pick_folder("选择视频目录", &self.videos) {
                        self.videos = as_string(p);
                    }
                }
                ui.end_row();

                ui.label("字幕目录");
                ui.add_sized(
                    [560.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.subs)
                        .hint_text("包含字幕文件的目录"),
                );
                if ui.button("浏览").clicked() {
                    if let Some(p) = pick_folder("选择字幕目录", &self.subs) {
                        self.subs = as_string(p);
                    }
                }
                ui.end_row();

                ui.label("后缀");
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut self.suffix),
                    );
                    ui.label("字幕名里已带 .sc / .tc / .SumiSora 等标记时会沿用原名里的");
                });
                ui.label("");
                ui.end_row();
            });

        ui.horizontal(|ui| {
            ui.radio_value(&mut self.mode, Mode::Copy, "复制到视频目录");
            ui.radio_value(&mut self.mode, Mode::Rename, "就地重命名");
            ui.add_space(16.0);
            if ui.button("预览").clicked() {
                self.preview();
            }
            let actionable = self
                .plan
                .as_ref()
                .map(|p| p.items.iter().filter(|i| i.status == Status::Ready).count())
                .unwrap_or(0);
            let label = if actionable > 0 {
                format!("执行 {} 条", actionable)
            } else {
                "执行".to_string()
            };
            if ui
                .add_enabled(actionable > 0, egui::Button::new(label))
                .clicked()
            {
                self.apply();
            }
        });

        if !self.message.is_empty() {
            ui.add_space(4.0);
            ui.label(&self.message);
        }

        if let Some(plan) = &self.plan {
            ui.add_space(6.0);
            ui.label(format!(
                "视频 {} 个 / 字幕 {} 个",
                plan.video_count, plan.subtitle_count
            ));
            egui::ScrollArea::vertical()
                .id_salt("plan_scroll")
                .max_height(280.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("plan_grid")
                        .num_columns(4)
                        .striped(true)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            ui.strong("字幕文件");
                            ui.strong("对应视频");
                            ui.strong("重命名为");
                            ui.strong("状态");
                            ui.end_row();
                            for item in &plan.items {
                                ui.monospace(basename(&item.subtitle));
                                ui.monospace(match item.video.as_deref() {
                                    Some(v) => basename(v),
                                    None => "-",
                                });
                                ui.monospace(if item.target.is_empty() {
                                    "-"
                                } else {
                                    basename(&item.target)
                                });
                                let (text, color) = status_ui(item.status);
                                ui.colored_label(color, text);
                                ui.end_row();
                            }
                        });
                });
        }
    }

    fn section_extract(&mut self, ui: &mut egui::Ui) {
        ui.heading("抽取 MKV 内嵌字幕");
        ui.add_space(4.0);
        let remote_mode = !self.remote.trim().is_empty();
        let http_mode = self.is_http();
        egui::Grid::new("extract_grid")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("NAS 地址");
                ui.add_sized(
                    [560.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.remote).hint_text(
                        "留空 = 本机；user@192.168.1.10:2222 = ssh 直连，NAS 上不用常驻进程",
                    ),
                );
                ui.horizontal(|ui| {
                    if ui.button("连接").clicked() {
                        self.test_remote(ui.ctx());
                    }
                    if remote_mode && !http_mode && ui.button("部署").clicked() {
                        self.deploy(ui.ctx());
                    }
                    if remote_mode && !http_mode && ui.button("诊断").clicked() {
                        self.diagnose(ui.ctx());
                    }
                    if remote_mode && http_mode && ui.button("打开网页").clicked() {
                        if let Ok(ep) = Endpoint::parse(&self.remote) {
                            crate::server::open_browser(&ep.base_url());
                        }
                    }
                });
                ui.end_row();

                if remote_mode && !http_mode {
                    ui.label("远程程序");
                    ui.add_sized(
                        [560.0, ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut self.nas_bin)
                            .hint_text("resub 二进制在 NAS 上的路径"),
                    );
                    ui.label("");
                    ui.end_row();

                    ui.label("本地程序");
                    ui.add_sized(
                        [560.0, ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut self.nas_local)
                            .hint_text("留空 = 自动从 nightly 下载"),
                    );
                    if ui.button("选择").clicked() {
                        if let Some(p) = pick_any("选择 resub-linux-x86_64", &self.nas_local) {
                            self.nas_local = as_string(p);
                        }
                    }
                    ui.end_row();
                }

                ui.label("输入");
                ui.add_sized(
                    [560.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.mkv)
                        .hint_text("单个 .mkv，或包含 MKV 的目录（会递归）"),
                );
                if remote_mode {
                    ui.horizontal(|ui| {
                        if ui.button("远程浏览…").clicked() {
                            self.open_browse(ui.ctx());
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("选文件").clicked() {
                            if let Some(p) = pick_file("选择 MKV 文件", &self.mkv) {
                                self.mkv = as_string(p);
                            }
                        }
                        if ui.button("选目录").clicked() {
                            if let Some(p) = pick_folder("选择包含 MKV 的目录", &self.mkv) {
                                self.mkv = as_string(p);
                            }
                        }
                    });
                }
                ui.end_row();

                ui.label("输出目录");
                ui.add_sized(
                    [560.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut self.out)
                        .hint_text("留空 = 每个 MKV 旁边"),
                );
                if remote_mode {
                    ui.label("");
                } else if ui.button("浏览").clicked() {
                    if let Some(p) = pick_folder("选择输出目录", &self.out) {
                        self.out = as_string(p);
                    }
                }
                ui.end_row();
            });

        if !self.remote_status.is_empty() {
            ui.add_space(2.0);
            ui.label(&self.remote_status);
        }

        let busy = self.extract_rx.is_some();
        ui.horizontal(|ui| {
            let label = if remote_mode { "在 NAS 上抽取" } else { "抽取" };
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                let ctx = ui.ctx().clone();
                self.start_extract(ctx);
            }
            if busy {
                ui.spinner();
            }
            ui.checkbox(&mut self.skip_existing, "跳过已存在的字幕");
            if !self.extract_progress.is_empty() {
                ui.label(&self.extract_progress);
            }
        });

        for err in &self.extract_errors {
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), err);
        }

        if !self.extract_tracks.is_empty() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("extract_result")
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("extract_grid_result")
                        .num_columns(4)
                        .striped(true)
                        .spacing([12.0, 4.0])
                        .show(ui, |ui| {
                            for t in &self.extract_tracks {
                                ui.monospace(basename(&t.file));
                                ui.label(format!(
                                    "{} [{}]",
                                    t.name.clone().unwrap_or_default(),
                                    t.codec
                                ));
                                let comp = match t.compressed {
                                    Some(0) => "zlib 压缩",
                                    Some(3) => "header-strip",
                                    Some(_) => "其他压缩",
                                    None => "未压缩",
                                };
                                ui.label(comp);
                                if t.skipped {
                                    ui.label("已存在");
                                } else {
                                    ui.label(format!("{} 条", t.events));
                                }
                                ui.end_row();
                            }
                        });
                });
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.extract_rx.is_some() {
            self.poll_extract(&ctx);
        }
        if self.job_rx.is_some() {
            self.poll_jobs(&ctx);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("main_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("resub");
                    ui.label("字幕批量改名 / MKV 内嵌字幕抽取");
                    ui.separator();
                    self.section_rename(ui);
                    ui.add_space(14.0);
                    ui.separator();
                    self.section_extract(ui);
                    ui.add_space(14.0);
                });
        });
        self.browse_window(&ctx);
        self.diag_window(&ctx);
        self.persist(&ctx);
    }
}
