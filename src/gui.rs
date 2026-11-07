//! Native window front-end (eframe/egui). Everything is compiled into the executable:
//! no browser, no server, no runtime to install.

use std::path::PathBuf;

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

struct App {
    videos: String,
    subs: String,
    suffix: String,
    mode: Mode,
    plan: Option<rename::Plan>,
    message: String,
    mkv: String,
    out: String,
    extract: Option<(String, Vec<crate::ExtractedTrack>)>,
    picker: Option<Picker>,
    initialized: bool,
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
            extract: None,
            picker: None,
            initialized: false,
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
        let videos_dir = std::path::PathBuf::from(self.videos.trim());
        let subs_dir = std::path::PathBuf::from(self.subs.trim());
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

    fn run_extract(&mut self) {
        self.extract = None;
        let file = std::path::PathBuf::from(self.mkv.trim());
        if !file.is_file() {
            self.message = format!("文件不存在: {}", file.display());
            return;
        }
        let out = if self.out.trim().is_empty() {
            file.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."))
        } else {
            std::path::PathBuf::from(self.out.trim())
        };
        match crate::extract_all(&file, &out) {
            Ok(tracks) => {
                let n = tracks.len();
                self.extract = Some((out.to_string_lossy().to_string(), tracks));
                self.message = format!("抽出 {n} 条字幕轨道");
            }
            Err(e) => self.message = format!("抽取失败: {e}"),
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
                .max_height(300.0)
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
                                ui.monospace(if item.video.is_some() {
                                    basename(item.video.as_deref().unwrap_or(""))
                                } else {
                                    "-"
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
                ui.label("MKV 文件");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mkv)
                        .desired_width(560.0)
                        .hint_text("选择 .mkv 文件"),
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
                        .hint_text("留空 = 与 MKV 同目录"),
                );
                if ui.button("浏览").clicked() {
                    let start = self.out.clone();
                    self.open_picker(Target::Out, false, start);
                }
                ui.end_row();
            });
        if ui.button("抽取").clicked() {
            self.run_extract();
        }

        if let Some((out, tracks)) = &self.extract {
            ui.add_space(4.0);
            ui.label(format!("输出到 {out}"));
            egui::Grid::new("extract_result")
                .num_columns(3)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for t in tracks {
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
                        ui.label(format!("{} 条 / {}", t.events, comp));
                        ui.end_row();
                    }
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
        if !self.initialized {
            self.initialized = true;
        }
        let ctx = ui.ctx().clone();
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
