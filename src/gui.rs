//! Native window front-end (eframe/egui). Everything is compiled into the executable:
//! no browser, no server, no runtime to install.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;
use walkdir::WalkDir;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Videos,
    Subs,
    Mkv,
    Out,
}

struct Picker {
    target: Target,
    want_file: bool,
    path: String,
    parent: Option<String>,
    dirs: Vec<String>,
    files: Vec<String>,
    error: Option<String>,
}

impl Picker {
    fn new(target: Target, want_file: bool, start: String) -> Self {
        let mut p = Picker {
            target,
            want_file,
            path: String::new(),
            parent: None,
            dirs: Vec::new(),
            files: Vec::new(),
            error: None,
        };
        p.load(&start);
        p
    }

    fn load(&mut self, path: &str) {
        let listing = crate::list_dir(path, self.want_file);
        self.path = listing.path;
        self.parent = listing.parent;
        self.dirs = listing.dirs;
        self.files = listing.files;
        self.error = listing.error;
    }
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
    picker: Option<Picker>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            videos: String::new(),
            subs: String::new(),
            suffix: ".chs".to_string(),
            mode: Mode::Copy,
            plan: None,
            message: String::new(),
            mkv: String::new(),
            out: String::new(),
            extract_rx: None,
            extract_progress: String::new(),
            extract_tracks: Vec::new(),
            extract_errors: Vec::new(),
            picker: None,
        }
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

impl App {
    fn open_picker(&mut self, target: Target, want_file: bool, start: String) {
        self.picker = Some(Picker::new(target, want_file, start));
    }

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
                match crate::extract_all(file, &out_dir, None) {
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

    fn section_rename(&mut self, ui: &mut egui::Ui) {
        ui.heading("批量改名");
        ui.add_space(4.0);
        egui::Grid::new("rename_grid")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("视频目录");
                ui.add(
                    egui::TextEdit::singleline(&mut self.videos)
                        .desired_width(560.0)
                        .hint_text("包含视频文件的目录"),
                );
                if ui.button("浏览").clicked() {
                    let start = self.videos.clone();
                    self.open_picker(Target::Videos, false, start);
                }
                ui.end_row();

                ui.label("字幕目录");
                ui.add(
                    egui::TextEdit::singleline(&mut self.subs)
                        .desired_width(560.0)
                        .hint_text("包含字幕文件的目录"),
                );
                if ui.button("浏览").clicked() {
                    let start = self.subs.clone();
                    self.open_picker(Target::Subs, false, start);
                }
                ui.end_row();

                ui.label("后缀");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.suffix).desired_width(90.0));
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
        egui::Grid::new("extract_grid")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("输入");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mkv)
                        .desired_width(560.0)
                        .hint_text("单个 .mkv，或包含 MKV 的目录（会递归）"),
                );
                if ui.button("浏览").clicked() {
                    let start = self.mkv.clone();
                    self.open_picker(Target::Mkv, true, start);
                }
                ui.end_row();

                ui.label("输出目录");
                ui.add(
                    egui::TextEdit::singleline(&mut self.out)
                        .desired_width(560.0)
                        .hint_text("留空 = 每个 MKV 旁边"),
                );
                if ui.button("浏览").clicked() {
                    let start = self.out.clone();
                    self.open_picker(Target::Out, false, start);
                }
                ui.end_row();
            });

        let busy = self.extract_rx.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("抽取"))
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.start_extract(ctx);
            }
            if busy {
                ui.spinner();
            }
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
                                ui.label(format!("{} 条", t.events));
                                ui.end_row();
                            }
                        });
                });
        }
    }

    fn show_picker(&mut self, ctx: &egui::Context) {
        let Some(mut picker) = self.picker.take() else {
            return;
        };
        let mut open = true;
        let mut chosen: Option<String> = None;
        let mut nav: Option<String> = None;

        egui::Window::new("选择路径")
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(if picker.path.is_empty() {
                    "(根目录)"
                } else {
                    picker.path.as_str()
                });
                if let Some(err) = &picker.error {
                    ui.colored_label(egui::Color32::from_rgb(200, 60, 60), err);
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("picker_scroll")
                    .max_height(340.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(parent) = picker.parent.clone() {
                            if ui.button("..").clicked() {
                                nav = Some(parent);
                            }
                        }
                        for d in &picker.dirs {
                            if ui.button(format!("[目录] {d}")).clicked() {
                                nav = Some(join_path(&picker.path, d));
                            }
                        }
                        for f in &picker.files {
                            if ui.button(f).clicked() {
                                nav = Some(join_path(&picker.path, f));
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("选择此目录").clicked() {
                        chosen = Some(picker.path.clone());
                    }
                    if picker.want_file {
                        ui.label("点击文件名即可选中该文件");
                    }
                });
            });

        if let Some(p) = nav {
            picker.load(&p);
        }
        if let Some(path) = chosen {
            match picker.target {
                Target::Videos => self.videos = path,
                Target::Subs => self.subs = path,
                Target::Mkv => self.mkv = path,
                Target::Out => self.out = path,
            }
            return;
        }
        if open {
            self.picker = Some(picker);
        }
    }
}

fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') || base.ends_with('\\') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.extract_rx.is_some() {
            self.poll_extract(&ctx);
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
        self.show_picker(&ctx);
    }
}
