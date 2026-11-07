//! Match subtitle files to video files by episode number and build the rename plan.
//!
//! This is what the original U2 tool did, minus its guessing: the episode number is
//! taken from the most specific pattern that matches, and language tags that the
//! subtitle name already carries (.sc/.tc/.chs/SumiSora/...) are preserved.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use walkdir::WalkDir;

pub const DEFAULT_VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m2ts", "ts", "flv", "avi", "wmv", "mov", "webm", "rmvb", "rm", "m4v",
];
pub const DEFAULT_SUB_EXTS: &[&str] = &["ass", "ssa", "srt", "sub", "idx", "str", "aas", "vtt"];

/// Language / release-group tags that may already be part of a subtitle file name.
const LANG_TAGS: &[&str] = &[
    "sc", "tc", "gb", "big5", "chs", "cht", "jpsc", "jptc", "uni_gb", "uni_big5", "简体",
    "繁体", "简", "繁", "baha", "ktxp", "caso", "popgo", "sumisora", "lolihouse", "loli",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Rename the subtitle where it already sits.
    Rename,
    /// Copy the subtitle next to the video it belongs to.
    Copy,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "rename" | "r" => Some(Mode::Rename),
            "copy" | "c" => Some(Mode::Copy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ready,
    TargetExists,
    AlreadyNamed,
    NoVideo,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    pub subtitle: String,
    pub video: Option<String>,
    pub target: String,
    pub episode: Option<u32>,
    pub suffix: String,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    pub video_count: usize,
    pub subtitle_count: usize,
}

fn re_bangumi() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"第\s*(\d{1,4})\s*[話话集]").unwrap())
}

fn re_se() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)S\d{1,2}[\s._-]?E(\d{1,4})").unwrap())
}

fn re_dash() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s-\s(\d{1,4})(?:v\d)?(?:[\s\[\]\(\)\._]|$)").unwrap())
}

fn re_bracket() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[\[\(](\d{1,4})(?:v\d)?[\]\)]").unwrap())
}

fn re_dotnum() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[\._](\d{1,4})(?:v\d)?[\._ ]").unwrap())
}

/// Numbers that are far more likely to be a resolution or codec than an episode.
const IGNORED_NUMBERS: [u32; 7] = [264, 265, 480, 576, 720, 1080, 2160];

fn plausible(v: u32) -> bool {
    v <= 999 && !IGNORED_NUMBERS.contains(&v) && !(1900..=2100).contains(&v)
}

/// Episode number of a file stem, or None when nothing plausible is found.
pub fn episode(stem: &str) -> Option<u32> {
    for re in [re_bangumi(), re_se(), re_dash(), re_bracket(), re_dotnum()] {
        if let Some(c) = re.captures(stem) {
            if let Some(v) = c.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) {
                if plausible(v) {
                    return Some(v);
                }
            }
        }
    }
    fallback_episode(stem)
}

fn fallback_episode(stem: &str) -> Option<u32> {
    let b = stem.as_bytes();
    let mut best = None;
    let mut i = 0usize;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let num: u32 = match stem[start..i].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let before = if start > 0 { b[start - 1] } else { b' ' };
        let after = if i < b.len() { b[i] } else { b' ' };
        // Skip codec / resolution markers such as x264, h265, 1080p, 10bit, 23.976fps.
        let reject = matches!(before, b'x' | b'X' | b'h' | b'H')
            || matches!(after, b'p' | b'P' | b'b' | b'B' | b'k' | b'K' | b'f' | b'F')
            || !plausible(num);
        if !reject {
            best = Some(num);
        }
        // Consume a trailing v2 / v3 revision marker so it cannot be read as the episode.
        if matches!(after, b'v' | b'V') && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    best
}

/// Trailing dotted language tags already present in a subtitle name, e.g. ".SumiSora.sc".
pub fn detect_suffix(stem: &str) -> Option<String> {
    let parts: Vec<&str> = stem.split('.').collect();
    let mut taken: Vec<&str> = Vec::new();
    for seg in parts.iter().rev() {
        let low = seg.to_lowercase();
        if LANG_TAGS.contains(&low.as_str()) {
            taken.push(seg);
        } else {
            break;
        }
    }
    if taken.is_empty() {
        return None;
    }
    taken.reverse();
    Some(format!(".{}", taken.join(".")))
}

fn ext_lower(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Every file under dir whose extension is in exts, sorted for stable plans.
pub fn collect(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir).max_depth(3).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if exts.contains(&ext.as_str()) {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    out
}

pub fn plan(videos: &[PathBuf], subs: &[PathBuf], mode: Mode, suffix: &str) -> Plan {
    let mut by_episode: HashMap<u32, Vec<&PathBuf>> = HashMap::new();
    for v in videos {
        if let Some(ep) = episode(&stem_of(v)) {
            by_episode.entry(ep).or_default().push(v);
        }
    }

    let mut items = Vec::with_capacity(subs.len());
    for s in subs {
        let stem = stem_of(s);
        let ep = episode(&stem);
        let candidates = ep.and_then(|e| by_episode.get(&e).cloned()).unwrap_or_default();
        let video = candidates.first().map(|v| (*v).clone());

        let suffix_used = detect_suffix(&stem).unwrap_or_else(|| suffix.to_string());
        let ext = ext_lower(s);

        let (target, status) = match &video {
            None => (String::new(), Status::NoVideo),
            Some(v) => {
                let new_stem = format!("{}{}", stem_of(v), suffix_used);
                let new_name = if ext.is_empty() {
                    new_stem
                } else {
                    format!("{new_stem}.{ext}")
                };
                let dir: PathBuf = match mode {
                    Mode::Copy => v.parent().unwrap_or(Path::new(".")).to_path_buf(),
                    Mode::Rename => s.parent().unwrap_or(Path::new(".")).to_path_buf(),
                };
                let target = dir.join(&new_name);
                let status = if target == *s {
                    Status::AlreadyNamed
                } else if candidates.len() > 1 {
                    Status::Ambiguous
                } else if target.exists() {
                    Status::TargetExists
                } else {
                    Status::Ready
                };
                (target.to_string_lossy().to_string(), status)
            }
        };

        items.push(PlanItem {
            subtitle: s.to_string_lossy().to_string(),
            video: video.map(|v| v.to_string_lossy().to_string()),
            target,
            episode: ep,
            suffix: suffix_used,
            status,
        });
    }

    Plan {
        items,
        video_count: videos.len(),
        subtitle_count: subs.len(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    pub target: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Execute a plan. Anything that is not Ready is left alone.
pub fn apply(plan: &Plan, mode: Mode) -> Vec<Applied> {
    let mut out = Vec::new();
    for item in &plan.items {
        if item.status != Status::Ready {
            continue;
        }
        let src = PathBuf::from(&item.subtitle);
        let dst = PathBuf::from(&item.target);
        let result = match mode {
            Mode::Rename => fs::rename(&src, &dst),
            Mode::Copy => fs::copy(&src, &dst).map(|_| ()),
        };
        out.push(Applied {
            target: item.target.clone(),
            ok: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_patterns() {
        assert_eq!(episode("[Group] Title - 01 [1080p]"), Some(1));
        assert_eq!(episode("Title 第12話 [BDRip]"), Some(12));
        assert_eq!(episode("Title.S01E03.1080p.WEB-DL"), Some(3));
        assert_eq!(episode("[Group] Title - 07v2 [x264 10bit 1080p]"), Some(7));
        assert_eq!(episode("[Group][Title][08][BDRip]"), Some(8));
        assert_eq!(episode("Title.10.1080p"), Some(10));
        assert_eq!(episode("Title - Movie [1080p FLAC]"), None);
        assert_eq!(episode("Special.2024.1080p"), None);
    }

    #[test]
    fn suffix_detection() {
        assert_eq!(detect_suffix("Title 02.SumiSora.tc").as_deref(), Some(".SumiSora.tc"));
        assert_eq!(detect_suffix("Title 01.sc").as_deref(), Some(".sc"));
        assert_eq!(detect_suffix("Title 01"), None);
        assert_eq!(detect_suffix("Title 01.GB").as_deref(), Some(".GB"));
    }

    #[test]
    fn plan_roundtrip() {
        let videos = vec![PathBuf::from("/v/Show - 01.mkv"), PathBuf::from("/v/Show - 02.mkv")];
        let subs = vec![PathBuf::from("/s/Show 01.sc.ass"), PathBuf::from("/s/Show 02.ass")];
        let p = plan(&videos, &subs, Mode::Copy, ".chs");
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.items[0].suffix, ".sc");
        assert_eq!(p.items[1].suffix, ".chs");
        assert!(p.items[0].target.ends_with("Show - 01.sc.ass"));
    }
}
