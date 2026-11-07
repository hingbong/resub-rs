//! Render extracted subtitle events back into a standalone ASS/SSA or SRT file.

use std::fmt::Write as _;

use crate::mkv::{Event, SubtitleTrack};

const DEFAULT_ASS_HEADER: &str = "[Script Info]
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080
WrapStyle: 0
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,64,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
";

/// ASS timestamp: H:MM:SS.cc
fn ass_time(ms: i64) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1_000) % 60;
    let cs = (ms % 1_000) / 10;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

/// SRT timestamp: HH:MM:SS,mmm
fn srt_time(ms: i64) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1_000) % 60;
    let milli = ms % 1_000;
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

/// Keep the muxer's own script header when it stored a full one in CodecPrivate.
fn header_for(track: &SubtitleTrack) -> String {
    let cp = String::from_utf8_lossy(&track.info.codec_private).to_string();
    let lower = cp.to_ascii_lowercase();
    let has_header = lower.contains("[script info]") || lower.contains("[events]");
    if has_header && !cp.trim().is_empty() {
        let mut h = cp.trim_end_matches(['\r', '\n', '\0']).replace("\r\n", "\n");
        if !h.to_ascii_lowercase().contains("[events]") {
            h.push_str("\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text");
        }
        h.push('\n');
        h
    } else {
        DEFAULT_ASS_HEADER.to_string()
    }
}

/// Render a track as ASS text. The stored payload is the dialogue body with its
/// ReadOrder field first; ASS output drops ReadOrder and adds Start/End.
pub fn render_ass(track: &SubtitleTrack) -> String {
    let mut out = header_for(track);
    for ev in &track.events {
        let fields: Vec<&str> = ev.raw.splitn(9, ',').collect();
        if fields.len() != 9 {
            continue;
        }
        let _read_order = fields[0];
        let layer = fields[1];
        let style = fields[2];
        let name = fields[3];
        let ml = fields[4];
        let mr = fields[5];
        let mv = fields[6];
        let effect = fields[7];
        let text = fields[8];
        let start = ass_time(ev.start_ms);
        let end = ass_time(ev.end_ms);
        let _ = writeln!(
            out,
            "Dialogue: {layer},{start},{end},{style},{name},{ml},{mr},{mv},{effect},{text}"
        );
    }
    out
}

/// Render an S_TEXT/UTF8 track. Each block already carries its own timing line.
pub fn render_srt(track: &SubtitleTrack) -> String {
    let mut out = String::new();
    for (i, ev) in track.events.iter().enumerate() {
        let _ = writeln!(out, "{}", i + 1);
        if ev.raw.contains("-->") {
            let _ = writeln!(out, "{}", ev.raw.trim_end());
        } else {
            let _ = writeln!(
                out,
                "{} --> {}",
                srt_time(ev.start_ms),
                srt_time(ev.end_ms)
            );
            let _ = writeln!(out, "{}", ev.raw.trim_end());
        }
        let _ = writeln!(out);
    }
    out
}

/// Pick a readable suffix for a track: Chinese simplified/traditional first, then language.
pub fn language_tag(track: &SubtitleTrack) -> String {
    let name = track.info.name.clone().unwrap_or_default();
    let lower = name.to_lowercase();
    if name.contains('简') || name.contains('簡') || lower.contains("simplified") || lower.contains("chs") {
        return "chs".into();
    }
    if name.contains('繁') || lower.contains("traditional") || lower.contains("cht") {
        return "cht".into();
    }
    let lang = track.info.language.clone().unwrap_or_default().to_lowercase();
    match lang.as_str() {
        "chi" | "zho" | "zh" => "zh".into(),
        "" | "und" => format!("track{}", track.info.number),
        other => other.to_string(),
    }
}

pub fn file_extension(track: &SubtitleTrack) -> &'static str {
    if track.info.is_utf8() {
        "srt"
    } else {
        "ass"
    }
}

pub fn render(track: &SubtitleTrack) -> String {
    if track.info.is_utf8() {
        render_srt(track)
    } else {
        render_ass(track)
    }
}

pub fn event_count(track: &SubtitleTrack) -> usize {
    track.events.len()
}

#[allow(dead_code)]
fn _unused(events: &[Event]) -> usize {
    events.len()
}
