//! resub: batch subtitle renamer and Matroska subtitle extractor.

pub mod ass;
pub mod ebml;
pub mod mkv;
pub mod rename;
pub mod server;

use std::path::Path;

use serde::Serialize;

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
/// Returns one entry per written track; the caller decides the directory.
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
