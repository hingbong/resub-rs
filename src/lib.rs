//! resub: batch subtitle renamer and Matroska subtitle extractor.

pub mod ass;
pub mod ebml;
#[cfg(feature = "gui")]
pub mod gui;
pub mod mkv;
pub mod rename;
pub mod server;

use std::path::{Path, PathBuf};

use serde::Serialize;

/// A directory listing shared by the native window and the browser UI.
pub struct DirListing {
    pub path: String,
    pub parent: Option<String>,
    pub dirs: Vec<String>,
    pub files: Vec<String>,
    pub error: Option<String>,
}

fn roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut v = Vec::new();
        for c in b'A'..=b'Z' {
            let p = format!("{}:\\", c as char);
            if Path::new(&p).is_dir() {
                v.push(p);
            }
        }
        v
    }
    #[cfg(not(windows))]
    {
        vec!["/".to_string()]
    }
}

/// List a directory. An empty path returns the filesystem roots.
/// When want_file is set only media/subtitle files are listed, so a file can be picked.
pub fn list_dir(raw: &str, want_file: bool) -> DirListing {
    if raw.trim().is_empty() {
        return DirListing {
            path: String::new(),
            parent: None,
            dirs: roots(),
            files: Vec::new(),
            error: None,
        };
    }
    let mut path = PathBuf::from(raw);
    if path.is_file() {
        if let Some(p) = path.parent() {
            path = p.to_path_buf();
        }
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    match std::fs::read_dir(&path) {
        Ok(rd) => {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if entry.path().is_dir() {
                    dirs.push(name);
                } else if want_file {
                    let ext = entry
                        .path()
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if rename::DEFAULT_VIDEO_EXTS.contains(&ext.as_str())
                        || rename::DEFAULT_SUB_EXTS.contains(&ext.as_str())
                    {
                        files.push(name);
                    }
                }
            }
        }
        Err(e) => {
            return DirListing {
                path: path.to_string_lossy().to_string(),
                parent: None,
                dirs: Vec::new(),
                files: Vec::new(),
                error: Some(e.to_string()),
            };
        }
    }
    dirs.sort_by_key(|s| s.to_lowercase());
    files.sort_by_key(|s| s.to_lowercase());
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| !p.is_empty());

    DirListing {
        path: path.to_string_lossy().to_string(),
        parent,
        dirs,
        files,
        error: None,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractedTrack {
    pub number: u64,
    pub codec: String,
    pub name: Option<String>,
    pub language: Option<String>,
    pub compressed: Option<u64>,
    pub events: usize,
    pub file: String,
}

/// Extract every text subtitle track of an MKV into separate .ass/.srt files.
pub fn extract_all(mkv: &Path, out_dir: &Path) -> std::io::Result<Vec<ExtractedTrack>> {
    std::fs::create_dir_all(out_dir)?;
    let tracks = mkv::read_subtitles(mkv)?;
    let stem = mkv
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());

    let mut out = Vec::new();
    for t in &tracks {
        if t.events.is_empty() {
            continue;
        }
        let tag = ass::language_tag(t);
        let ext = ass::file_extension(t);
        let file = out_dir.join(format!("{stem}.{tag}.{ext}"));
        std::fs::write(&file, ass::render(t))?;
        out.push(ExtractedTrack {
            number: t.info.number,
            codec: t.info.codec_id.clone(),
            name: t.info.name.clone(),
            language: t.info.language.clone(),
            compressed: t.info.compression.as_ref().map(|c| c.algo),
            events: t.events.len(),
            file: file.to_string_lossy().to_string(),
        });
    }
    Ok(out)
}
