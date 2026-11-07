use std::path::{Path, PathBuf};
use std::process::ExitCode;

use resub::rename::{self, Mode, Status};

fn usage() {
    println!(
        "resub - 字幕批量改名 / MKV 内嵌字幕抽取

用法:
  resub extract <文件.mkv> [--out 目录] [--track 轨道号]
  resub plan    --videos <视频目录> --subs <字幕目录> [--suffix .sc] [--mode copy|rename]
  resub apply   --videos <视频目录> --subs <字幕目录> [--suffix .sc] [--mode copy|rename]
  resub gui                                     打开图形界面（原生窗口）
  resub serve   [--port 8080] [--no-open]      本地网页界面（浏览器版）

plan 只预览，apply 才真正执行。默认 mode=copy，把字幕复制到视频旁边。"
    );
}

fn flags(args: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let mut named = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(rest) = a.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                named.push((k.to_string(), v.to_string()));
            } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                named.push((rest.to_string(), args[i + 1].clone()));
                i += 1;
            } else {
                named.push((rest.to_string(), String::new()));
            }
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    (named, positional)
}

fn opt<'a>(named: &'a [(String, String)], key: &str) -> Option<&'a str> {
    named
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
        return ExitCode::SUCCESS;
    };
    let (named, positional) = flags(&args[1..]);
    match cmd {
        "extract" => cmd_extract(&named, &positional),
        "gui" => cmd_gui(&named),
        "serve" => cmd_serve(&named),
        "plan" => cmd_plan(&named, false),
        "apply" => cmd_plan(&named, true),
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未知子命令: {other}\n");
            usage();
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "gui")]
fn cmd_gui(_named: &[(String, String)]) -> ExitCode {
    match resub::gui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("界面启动失败: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "gui"))]
fn cmd_gui(named: &[(String, String)]) -> ExitCode {
    cmd_serve(named)
}

fn cmd_serve(named: &[(String, String)]) -> ExitCode {
    let port = opt(named, "port")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let no_open = named.iter().any(|(k, _)| k == "no-open");
    match resub::server::run(port, !no_open) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("启动失败: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_extract(named: &[(String, String)], positional: &[String]) -> ExitCode {
    let Some(input) = positional.first() else {
        eprintln!("需要指定 MKV 文件");
        return ExitCode::FAILURE;
    };
    let input = PathBuf::from(input);
    if !input.is_file() {
        eprintln!("文件不存在: {}", input.display());
        return ExitCode::FAILURE;
    }

    let tracks = match resub::mkv::read_subtitles(&input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读取失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    if tracks.is_empty() {
        println!("没有找到文本字幕轨道。");
        return ExitCode::SUCCESS;
    }

    println!("{:>3}  {:<12} {:<22} {:<6} {:>6}  说明", "轨", "编码", "名称", "语言", "事件");
    for t in &tracks {
        let comp = t
            .info
            .compression
            .as_ref()
            .map(|c| match c.algo {
                0 => "zlib".to_string(),
                3 => "header-strip".to_string(),
                other => format!("algo{other}"),
            })
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:>3}  {:<12} {:<22} {:<6} {:>6}  {}",
            t.info.number,
            t.info.codec_id,
            t.info.name.clone().unwrap_or_default(),
            t.info.language.clone().unwrap_or_default(),
            t.events.len(),
            comp
        );
        if t.skipped_laced > 0 {
            println!("     ! 跳过 {} 个带 lacing 的块", t.skipped_laced);
        }
        if t.skipped_undecodable > 0 {
            println!("     ! {} 个块无法解码", t.skipped_undecodable);
        }
    }

    let only = opt(named, "track").and_then(|v| v.parse::<u64>().ok());
    let out_dir = opt(named, "out")
        .map(PathBuf::from)
        .unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")).to_path_buf());

    if std::fs::create_dir_all(&out_dir).is_err() {
        eprintln!("无法创建输出目录: {}", out_dir.display());
        return ExitCode::FAILURE;
    }
    let mut written = 0usize;
    for t in &tracks {
        if let Some(n) = only {
            if t.info.number != n {
                continue;
            }
        }
        if t.events.is_empty() {
            continue;
        }
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let tag = resub::ass::language_tag(t);
        let ext = resub::ass::file_extension(t);
        let path = out_dir.join(format!("{stem}.{tag}.{ext}"));
        match std::fs::write(&path, resub::ass::render(t)) {
            Ok(()) => {
                println!("写出 {} ({} 条)", path.display(), t.events.len());
                written += 1;
            }
            Err(e) => eprintln!("写入 {} 失败: {e}", path.display()),
        }
    }
    if written == 0 {
        eprintln!("没有写出任何文件");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn cmd_plan(named: &[(String, String)], execute: bool) -> ExitCode {
    let (Some(videos_dir), Some(subs_dir)) = (opt(named, "videos"), opt(named, "subs")) else {
        eprintln!("需要 --videos 和 --subs\n");
        usage();
        return ExitCode::FAILURE;
    };
    let videos_dir = PathBuf::from(videos_dir);
    let subs_dir = PathBuf::from(subs_dir);
    if !videos_dir.is_dir() || !subs_dir.is_dir() {
        eprintln!("目录不存在: {}", if !videos_dir.is_dir() { videos_dir.display() } else { subs_dir.display() });
        return ExitCode::FAILURE;
    }

    let mode = opt(named, "mode")
        .and_then(Mode::parse)
        .unwrap_or(Mode::Copy);
    let suffix = opt(named, "suffix").unwrap_or(".sc").to_string();

    let videos = rename::collect(&videos_dir, rename::DEFAULT_VIDEO_EXTS);
    let subs = rename::collect(&subs_dir, rename::DEFAULT_SUB_EXTS);
    if videos.is_empty() {
        eprintln!("没有在 {} 找到视频文件", videos_dir.display());
        return ExitCode::FAILURE;
    }
    if subs.is_empty() {
        eprintln!("没有在 {} 找到字幕文件", subs_dir.display());
        return ExitCode::FAILURE;
    }

    let plan = rename::plan(&videos, &subs, mode, &suffix);
    let ready = plan.items.iter().filter(|i| i.status == Status::Ready).count();
    println!(
        "视频 {} 个 / 字幕 {} 个 / 可执行 {} 条  (mode={:?})",
        plan.video_count, plan.subtitle_count, ready, mode
    );

    for item in &plan.items {
        let src = Path::new(&item.subtitle)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let dst = Path::new(&item.target)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let label = match item.status {
            Status::Ready => "就绪",
            Status::TargetExists => "目标已存在",
            Status::AlreadyNamed => "已是目标名",
            Status::NoVideo => "找不到对应视频",
            Status::Ambiguous => "集数重复",
        };
        println!("  {src:<44} -> {dst:<44} {label}");
    }

    if !execute {
        return ExitCode::SUCCESS;
    }
    let applied = rename::apply(&plan, mode);
    let ok = applied.iter().filter(|a| a.ok).count();
    for a in &applied {
        if !a.ok {
            eprintln!("失败 {}: {}", a.target, a.error.clone().unwrap_or_default());
        }
    }
    println!("完成 {ok} / {}", applied.len());
    ExitCode::SUCCESS
}
