//! resub: batch subtitle renamer and Matroska subtitle extractor.

pub mod ass;
pub mod ebml;
#[cfg(feature = "gui")]
pub mod gui;
pub mod mkv;
pub mod remote;
pub mod rename;
pub mod ssh;
pub mod server;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// A directory listing shared by the native window, the browser UI and the ssh caller.
#[derive(Serialize)]
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

/// Every Matroska file under a path. A single file is returned as-is so the CLI and
/// the UI can treat "one file" and "a folder" the same way.
pub fn collect_mkv(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = WalkDir::new(path)
        .max_depth(5)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("mkv"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedTrack {
    pub number: u64,
    pub codec: String,
    pub name: Option<String>,
    pub language: Option<String>,
    pub compressed: Option<u64>,
    pub events: usize,
    pub file: String,
    /// True when the output file already existed and was left untouched.
    pub skipped: bool,
}

/// Extract the text subtitle tracks of one MKV into separate .ass/.srt files.
///
/// Passing a track number restricts the work to that one track. With skip_existing a
/// track whose output file already exists is reported but not rewritten -- and because
/// the output names are planned from the Tracks element alone, a file whose subtitles
/// are all present is finished after reading a few kilobytes of its header.
pub fn extract_all(
    mkv: &Path,
    out_dir: &Path,
    only: Option<u64>,
    skip_existing: bool,
) -> std::io::Result<Vec<ExtractedTrack>> {
    std::fs::create_dir_all(out_dir)?;
    let stem = mkv
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());

    let mut out: Vec<ExtractedTrack> = Vec::new();
    let mut want: HashSet<u64> = HashSet::new();
    for info in mkv::read_tracks(mkv)? {
        if let Some(number) = only {
            if info.number != number {
                continue;
            }
        }
        let path = out_dir.join(format!(
            "{stem}.{}.{}",
            ass::language_tag(&info),
            ass::file_extension(&info)
        ));
        let skipped = skip_existing && path.exists();
        if !skipped {
            want.insert(info.number);
        }
        out.push(ExtractedTrack {
            number: info.number,
            codec: info.codec_id.clone(),
            name: info.name.clone(),
            language: info.language.clone(),
            compressed: info.compression.as_ref().map(|c| c.algo),
            events: 0,
            file: path.to_string_lossy().to_string(),
            skipped,
        });
    }
    if want.is_empty() {
        return Ok(out);
    }

    let tracks = mkv::read_subtitles_where(mkv, Some(&want))?;
    let mut done: Vec<ExtractedTrack> = Vec::with_capacity(out.len());
    for entry in out {
        if entry.skipped {
            done.push(entry);
            continue;
        }
        let Some(t) = tracks.iter().find(|t| t.info.number == entry.number) else {
            continue;
        };
        if t.events.is_empty() {
            continue;
        }
        std::fs::write(&entry.file, ass::render(t))?;
        done.push(ExtractedTrack {
            events: t.events.len(),
            ..entry
        });
    }
    Ok(done)
}
