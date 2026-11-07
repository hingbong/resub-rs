//! Matroska reader: find text subtitle tracks and decode their blocks.
//!
//! Unlike Media3's MatroskaExtractor this treats a ContentCompression element with an
//! omitted ContentCompAlgo as the spec default, algorithm 0 (zlib). That omission is
//! exactly what makes some fansub releases unreadable in ExoPlayer-based players.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::ebml::{self, SliceReader};

const ID_SEGMENT: u64 = 0x1853_8067;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u64 = 0xAE;
const ID_TRACK_NUMBER: u64 = 0xD7;
const ID_TRACK_TYPE: u64 = 0x83;
const ID_FLAG_DEFAULT: u64 = 0x88;
const ID_CODEC_ID: u64 = 0x86;
const ID_NAME: u64 = 0x536E;
const ID_LANGUAGE: u64 = 0x22B5_9C;
const ID_CODEC_PRIVATE: u64 = 0x63A2;
const ID_CONTENT_ENCODINGS: u64 = 0x6D80;
const ID_CONTENT_ENCODING: u64 = 0x6240;
const ID_CONTENT_COMPRESSION: u64 = 0x5034;
const ID_CONTENT_COMP_ALGO: u64 = 0x4254;
const ID_CONTENT_COMP_SETTINGS: u64 = 0x4255;
const ID_CONTENT_ENCRYPTION: u64 = 0x5035;
const ID_CLUSTER: u64 = 0x1F43_B675;
const ID_TIMESTAMP: u64 = 0xE7;
const ID_SIMPLE_BLOCK: u64 = 0xA3;
const ID_BLOCK_GROUP: u64 = 0xA0;
const ID_BLOCK: u64 = 0xA1;
const ID_BLOCK_DURATION: u64 = 0x9B;

const TRACK_TYPE_SUBTITLE: u64 = 0x11;
const MAX_CLUSTER_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Compression {
    /// Matroska default when ContentCompAlgo is absent is 0 (zlib), not "none".
    pub algo: u64,
    pub settings: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub number: u64,
    pub codec_id: String,
    pub name: Option<String>,
    pub language: Option<String>,
    pub default: bool,
    pub codec_private: Vec<u8>,
    pub compression: Option<Compression>,
    pub encrypted: bool,
}

impl TrackInfo {
    pub fn is_ass(&self) -> bool {
        let c = self.codec_id.as_str();
        c == "S_TEXT/ASS" || c == "S_TEXT/SSA" || c == "S_ASS" || c == "S_SSA"
    }
    pub fn is_utf8(&self) -> bool {
        self.codec_id == "S_TEXT/UTF8"
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub start_ms: i64,
    pub end_ms: i64,
    /// ASS: "ReadOrder,Layer,Style,..." without the "Dialogue: " prefix.
    /// BlockDuration in milliseconds when the muxer stored one.
    pub duration: Option<u64>,
    /// SRT: the block payload with its own two-line timing.
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct SubtitleTrack {
    pub info: TrackInfo,
    pub events: Vec<Event>,
    pub skipped_laced: usize,
    pub skipped_undecodable: usize,
}

struct RawBlock {
    track: u64,
    ts: i64,
    duration: Option<u64>,
    payload: Vec<u8>,
}

fn read_str<R: Read>(r: &mut R, len: u64) -> io::Result<String> {
    let b = ebml::read_bytes(r, len)?;
    Ok(String::from_utf8_lossy(&b).trim_end_matches('\0').to_string())
}

/// Walk the children of an element whose payload spans [start, end).
fn each_child<R: Read + Seek, F>(r: &mut R, start: u64, end: u64, mut f: F) -> io::Result<()>
where
    F: FnMut(&mut R, u64, u64, u64) -> io::Result<()>,
{
    let mut pos = start;
    while pos < end {
        r.seek(SeekFrom::Start(pos))?;
        let id = match ebml::read_id(r)? {
            Some(id) => id,
            None => break,
        };
        let size = match ebml::read_size(r)? {
            Some(s) => s,
            None => break,
        };
        let data_start = r.stream_position()?;
        let data_end = data_start.saturating_add(size).min(end);
        f(r, id, data_start, data_end)?;
        pos = data_end;
    }
    Ok(())
}

/// Read every text subtitle track of a Matroska file, blocks decoded and sorted by time.
pub fn read_subtitles(path: &Path) -> io::Result<Vec<SubtitleTrack>> {
    let file = File::open(path)?;
    let total = file.metadata()?.len();
    let mut r = BufReader::with_capacity(1 << 20, file);

    // Find the Segment. The EBML header is a single element at the start.
    let mut segment: Option<(u64, Option<u64>)> = None;
    let mut pos = 0u64;
    while pos < total {
        r.seek(SeekFrom::Start(pos))?;
        let id = match ebml::read_id(&mut r)? {
            Some(id) => id,
            None => break,
        };
        let size = ebml::read_size(&mut r)?;
        let data_start = r.stream_position()?;
        if id == ID_SEGMENT {
            segment = Some((data_start, size));
            break;
        }
        pos = match size {
            Some(s) => data_start.saturating_add(s),
            None => break,
        };
    }
    let (seg_start, seg_size) = segment.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "no Matroska Segment found")
    })?;
    let seg_end = match seg_size {
        Some(s) => seg_start.saturating_add(s).min(total),
        None => total,
    };

    let mut tracks: Vec<TrackInfo> = Vec::new();
    let mut blocks: Vec<RawBlock> = Vec::new();
    let mut skipped_laced = 0usize;
    let mut sub_numbers: HashSet<u64> = HashSet::new();

    let mut pos = seg_start;
    while pos < seg_end {
        r.seek(SeekFrom::Start(pos))?;
        let id = match ebml::read_id(&mut r)? {
            Some(id) => id,
            None => break,
        };
        let size = ebml::read_size(&mut r)?;
        let data_start = r.stream_position()?;
        let data_end = match size {
            Some(s) => data_start.saturating_add(s).min(seg_end),
            None => seg_end,
        };
        match id {
            ID_TRACKS => {
                read_tracks(&mut r, data_start, data_end, &mut tracks)?;
                sub_numbers = tracks.iter().map(|t| t.number).collect();
            }
            ID_CLUSTER => read_cluster(
                &mut r,
                data_start,
                data_end,
                &mut blocks,
                &sub_numbers,
                &mut skipped_laced,
            )?,
            _ => {}
        }
        pos = data_end;
    }

    let mut out: Vec<SubtitleTrack> = tracks
        .into_iter()
        .map(|info| SubtitleTrack {
            info,
            events: Vec::new(),
            skipped_laced,
            skipped_undecodable: 0,
        })
        .collect();

    for b in blocks {
        let idx = match out.iter().position(|t| t.info.number == b.track) {
            Some(i) => i,
            None => continue,
        };
        match decode_payload(&out[idx].info, &b.payload) {
            Ok(raw) => {
                let text = String::from_utf8_lossy(&raw)
                    .trim_end_matches(['\0', '\r'])
                    .to_string();
                if text.is_empty() {
                    continue;
                }
                out[idx].events.push(Event {
                    start_ms: b.ts,
                    end_ms: 0,
                    duration: b.duration,
                    raw: text,
                });
            }
            Err(_) => out[idx].skipped_undecodable += 1,
        }
    }

    for t in out.iter_mut() {
        t.events.sort_by_key(|e| e.start_ms);
        fill_end_times(&mut t.events);
    }
    Ok(out)
}

fn fill_end_times(events: &mut [Event]) {
    for i in 0..events.len() {
        let start = events[i].start_ms;
        let end = match events[i].duration {
            Some(d) if d > 0 => start + d as i64,
            _ => events.get(i + 1).map(|e| e.start_ms).unwrap_or(start + 5000),
        };
        events[i].end_ms = if end > start { end } else { start + 1000 };
    }
}

fn decode_payload(info: &TrackInfo, payload: &[u8]) -> io::Result<Vec<u8>> {
    if info.encrypted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "track uses ContentEncryption, which is not supported",
        ));
    }
    let comp = match &info.compression {
        None => return Ok(payload.to_vec()),
        Some(c) => c,
    };
    match comp.algo {
        0 => {
            let mut out = Vec::with_capacity(payload.len().saturating_mul(4));
            flate2::read::ZlibDecoder::new(payload).read_to_end(&mut out)?;
            Ok(out)
        }
        3 => {
            let mut out = Vec::with_capacity(comp.settings.len() + payload.len());
            out.extend_from_slice(&comp.settings);
            out.extend_from_slice(payload);
            Ok(out)
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported ContentCompAlgo {other}"),
        )),
    }
}

fn read_tracks<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    out: &mut Vec<TrackInfo>,
) -> io::Result<()> {
    each_child(r, start, end, |r, id, ds, de| {
        if id != ID_TRACK_ENTRY {
            return Ok(());
        }
        let mut t = TrackInfo::default();
        let mut track_type = 0u64;
        each_child(r, ds, de, |r, fid, fds, fde| {
            let len = fde - fds;
            match fid {
                ID_TRACK_NUMBER => t.number = ebml::read_uint(r, len)?,
                ID_TRACK_TYPE => track_type = ebml::read_uint(r, len)?,
                ID_FLAG_DEFAULT => t.default = ebml::read_uint(r, len)? != 0,
                ID_CODEC_ID => t.codec_id = read_str(r, len)?,
                ID_NAME => t.name = Some(read_str(r, len)?),
                ID_LANGUAGE => t.language = Some(read_str(r, len)?),
                ID_CODEC_PRIVATE => t.codec_private = ebml::read_bytes(r, len)?,
                ID_CONTENT_ENCODINGS => read_encodings(r, fds, fde, &mut t)?,
                _ => {}
            }
            Ok(())
        })?;
        if track_type == TRACK_TYPE_SUBTITLE && (t.is_ass() || t.is_utf8()) {
            out.push(t);
        }
        Ok(())
    })
}

fn read_encodings<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    t: &mut TrackInfo,
) -> io::Result<()> {
    each_child(r, start, end, |r, id, ds, de| {
        if id != ID_CONTENT_ENCODING {
            return Ok(());
        }
        each_child(r, ds, de, |r, cid, cds, cde| {
            match cid {
                ID_CONTENT_COMPRESSION => {
                    let mut comp = Compression::default();
                    each_child(r, cds, cde, |r, gid, gds, gde| {
                        let len = gde - gds;
                        match gid {
                            ID_CONTENT_COMP_ALGO => comp.algo = ebml::read_uint(r, len)?,
                            ID_CONTENT_COMP_SETTINGS => comp.settings = ebml::read_bytes(r, len)?,
                            _ => {}
                        }
                        Ok(())
                    })?;
                    t.compression = Some(comp);
                }
                ID_CONTENT_ENCRYPTION => t.encrypted = true,
                _ => {}
            }
            Ok(())
        })
    })
}

fn read_cluster<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    out: &mut Vec<RawBlock>,
    sub_numbers: &HashSet<u64>,
    skipped_laced: &mut usize,
) -> io::Result<()> {
    let len = end.saturating_sub(start);
    if len > MAX_CLUSTER_BYTES {
        return Ok(());
    }
    let mut buf = vec![0u8; len as usize];
    r.seek(SeekFrom::Start(start))?;
    r.read_exact(&mut buf)?;

    let mut cluster_ts: i64 = 0;
    let mut p = 0usize;
    while p < buf.len() {
        let mut c = SliceReader::new(&buf[p..]);
        let id = match ebml::read_id(&mut c) {
            Ok(Some(id)) => id,
            _ => break,
        };
        let size = match ebml::read_size(&mut c) {
            Ok(Some(s)) => s,
            _ => break,
        };
        let ds = p + c.pos();
        if id == ID_TIMESTAMP {
            if let Ok(v) = ebml::read_uint(&mut SliceReader::new(&buf[ds..]), size) {
                cluster_ts = v as i64;
            }
            break;
        }
        p = ds.saturating_add(size as usize);
    }

    let mut sr = SliceReader::new(&buf);
    while !sr.is_empty() {
        let id = match ebml::read_id(&mut sr) {
            Ok(Some(id)) => id,
            _ => break,
        };
        let size = match ebml::read_size(&mut sr) {
            Ok(Some(s)) => s,
            _ => break,
        };
        let ds = sr.pos();
        let de = ds.saturating_add(size as usize).min(buf.len());
        match id {
            ID_SIMPLE_BLOCK => {
                read_block(&buf[ds..de], cluster_ts, None, out, sub_numbers, skipped_laced);
            }
            ID_BLOCK_GROUP => {
                let mut block: Option<(usize, usize)> = None;
                let mut duration: Option<u64> = None;
                let mut p = ds;
                while p < de {
                    let mut c = SliceReader::new(&buf[p..de]);
                    let cid = match ebml::read_id(&mut c) {
                        Ok(Some(v)) => v,
                        _ => break,
                    };
                    let csize = match ebml::read_size(&mut c) {
                        Ok(Some(v)) => v,
                        _ => break,
                    };
                    let cds = p + c.pos();
                    let cde = cds.saturating_add(csize as usize).min(de);
                    match cid {
                        ID_BLOCK => block = Some((cds, cde)),
                        ID_BLOCK_DURATION => {
                            if let Ok(v) =
                                ebml::read_uint(&mut SliceReader::new(&buf[cds..cde]), csize)
                            {
                                duration = Some(v);
                            }
                        }
                        _ => {}
                    }
                    p = cde;
                }
                if let Some((s, e)) = block {
                    read_block(&buf[s..e], cluster_ts, duration, out, sub_numbers, skipped_laced);
                }
            }
            _ => {}
        }
        sr.skip(de.saturating_sub(ds));
    }
    Ok(())
}

fn read_block(
    data: &[u8],
    cluster_ts: i64,
    duration: Option<u64>,
    out: &mut Vec<RawBlock>,
    sub_numbers: &HashSet<u64>,
    skipped_laced: &mut usize,
) {
    let mut sr = SliceReader::new(data);
    let track = match ebml::read_size(&mut sr) {
        Ok(Some(t)) => t,
        _ => return,
    };
    let mut tb = [0u8; 2];
    if sr.read_exact(&mut tb).is_err() {
        return;
    }
    let rel = i16::from_be_bytes(tb) as i64;
    let mut fb = [0u8; 1];
    if sr.read_exact(&mut fb).is_err() {
        return;
    }
    let lacing = (fb[0] >> 1) & 0x03;
    if lacing != 0 {
        // Lacing is normal for audio and video; only subtitle blocks would lose data here.
        if sub_numbers.contains(&track) {
            *skipped_laced += 1;
        }
        return;
    }
    out.push(RawBlock {
        track,
        ts: cluster_ts + rel,
        duration,
        payload: sr.remaining().to_vec(),
    });
}
