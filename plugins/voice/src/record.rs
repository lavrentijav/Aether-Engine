//! Recording to Ogg/Opus, one file per speaker per session.
//!
//! An Ogg page is `OggS`, a version, a header-type byte, a 64-bit granule
//! position, a stream serial, a page sequence, a CRC32 and a segment table
//! (`[lacing values][payload]`). This is a minimal single-stream muxer: one
//! packet per page, which is valid Ogg and what a per-frame recorder wants
//! anyway. The Opus header and the session metadata (who, when, where, group)
//! ride in the `OpusHead`/`OpusTags` packets, so the file is self-describing.
//!
//! Recording is **off by default**; the plugin turns it on per player, group or
//! globally at an operator's request, and `notify_recorded` tells the recorded
//! player.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Samples per Opus frame at 48 kHz for the 20 ms frame both mods use. The
/// granule position is derived from this, since the plugin forwards Opus
/// packets without decoding them and so cannot count samples itself.
pub const SAMPLES_PER_FRAME: u64 = 960;

/// The Ogg stream serial for the one logical stream in a recording.
const SERIAL: u32 = 0x4145_5448; // "AETH"

/// A single-stream Ogg/Opus writer.
pub struct OggOpusWriter {
    file: File,
    sequence: u32,
    granule: u64,
}

impl OggOpusWriter {
    /// Create a recording at `path` with one mono Opus stream, and write the
    /// `OpusHead` and `OpusTags` pages. `comments` are `KEY=value` tags.
    pub fn create(path: &Path, comments: &[(String, String)]) -> io::Result<OggOpusWriter> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        let mut w = OggOpusWriter {
            file,
            sequence: 0,
            granule: 0,
        };
        w.write_packet(&opus_head(), 0, 0x02)?; // BOS
        w.write_packet(&opus_tags(comments), 0, 0x00)?;
        Ok(w)
    }

    /// Append one Opus frame. The granule advances by [`SAMPLES_PER_FRAME`].
    pub fn write_frame(&mut self, data: &[u8]) -> io::Result<()> {
        self.granule += SAMPLES_PER_FRAME;
        // Drop a frame that would not fit a single page's segment table (255
        // segments of 255 bytes each); a real Opus frame is far below this.
        if data.len() > 255 * 255 {
            return Ok(());
        }
        self.write_packet(data, self.granule, 0x00)
    }

    /// Flush the file.
    pub fn finish(mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Build and write one page holding one packet.
    fn write_packet(&mut self, packet: &[u8], granule: u64, header_type: u8) -> io::Result<()> {
        let segments = packet.len() / 255 + 1;
        let mut page = Vec::with_capacity(27 + segments + packet.len());
        page.extend_from_slice(b"OggS");
        page.push(0); // version
        page.push(header_type);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&SERIAL.to_le_bytes());
        page.extend_from_slice(&self.sequence.to_le_bytes());
        page.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
        page.push(segments as u8);
        let mut remaining = packet.len();
        while remaining >= 255 {
            page.push(255);
            remaining -= 255;
        }
        page.push(remaining as u8);
        page.extend_from_slice(packet);
        let crc = ogg_crc(&page);
        let at = 22;
        page[at..at + 4].copy_from_slice(&crc.to_le_bytes());
        self.file.write_all(&page)?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }
}

/// The `OpusHead` identification packet (19 bytes, mono, 48 kHz).
pub fn opus_head() -> Vec<u8> {
    let mut v = Vec::with_capacity(19);
    v.extend_from_slice(b"OpusHead");
    v.push(1); // version
    v.push(1); // channel count (mono)
    v.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
    v.extend_from_slice(&48_000u32.to_le_bytes()); // input sample rate
    v.extend_from_slice(&0i16.to_le_bytes()); // output gain
    v.push(0); // channel mapping family
    v
}

/// The `OpusTags` comment packet.
pub fn opus_tags(comments: &[(String, String)]) -> Vec<u8> {
    let vendor = b"aether-voice";
    let mut v = Vec::new();
    v.extend_from_slice(b"OpusTags");
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor);
    v.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (k, val) in comments {
        let entry = format!("{k}={val}");
        v.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        v.extend_from_slice(entry.as_bytes());
    }
    v
}

/// The Ogg CRC32: polynomial `0x04c11db7`, no reflection, init 0, no final xor.
pub fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04c1_1db7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// One speaker's recording session: the file and its metadata.
pub struct Session {
    pub writer: OggOpusWriter,
    pub path: PathBuf,
    pub started_ms: i64,
    pub sender: u128,
    pub group: Option<u128>,
}

impl Session {
    /// Open a session under `dir`, with a path and comments built from the
    /// speaker's identity and where they were.
    pub fn open(
        dir: &str,
        sender: u128,
        sender_name: &str,
        world: &str,
        group: Option<u128>,
    ) -> io::Result<Session> {
        let started_ms = now_ms();
        let dir_name = format!(
            "{}-{:016x}",
            sanitize(sender_name),
            sender & 0xffff_ffff_ffff_ffff
        );
        let path = PathBuf::from(dir)
            .join(dir_name)
            .join(format!("{started_ms}.ogg"));
        let mut comments = vec![
            ("TITLE".to_string(), format!("{sender_name} voice session")),
            ("ARTIST".to_string(), sender_name.to_string()),
            ("DATE".to_string(), started_ms.to_string()),
            ("LOCATION".to_string(), world.to_string()),
            ("UUID".to_string(), format!("{sender:032x}")),
        ];
        if let Some(g) = group {
            comments.push(("GROUP".to_string(), format!("{g:032x}")));
        }
        let writer = OggOpusWriter::create(&path, &comments)?;
        Ok(Session {
            writer,
            path,
            started_ms,
            sender,
            group,
        })
    }
}

/// A recording destination: off, one player, a group, or everyone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Target {
    #[default]
    None,
    Player(u128),
    Group(u128),
    All,
}

impl Target {
    /// Whether a speaker/group pair is captured under this target.
    pub fn captures(&self, sender: u128, group: Option<u128>) -> bool {
        match self {
            Target::None => false,
            Target::Player(p) => *p == sender,
            Target::Group(g) => group == Some(*g),
            Target::All => true,
        }
    }
}

/// Make a name safe for a filesystem path element.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Delete recordings older than `retention_days` (0 keeps everything) and, if
/// `max_mb` is non-zero, trim the oldest files until the directory fits.
///
/// Best effort: a filesystem error is logged by the caller, not fatal.
pub fn enforce_retention(dir: &str, retention_days: u32, max_mb: u64) -> io::Result<()> {
    let root = Path::new(dir);
    if !root.exists() {
        return Ok(());
    }
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    collect_files(root, &mut files)?;
    if retention_days > 0 {
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(u64::from(retention_days) * 86_400);
        for (mtime, _, path) in &files {
            if *mtime < cutoff {
                let _ = std::fs::remove_file(path);
            }
        }
        files.retain(|(mtime, _, _)| *mtime >= cutoff);
    }
    if max_mb > 0 {
        let cap = max_mb * 1024 * 1024;
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        files.sort_by_key(|(mtime, _, _)| *mtime);
        for (_, size, path) in files {
            if total <= cap {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
    Ok(())
}

fn collect_files(
    dir: &Path,
    out: &mut Vec<(std::time::SystemTime, u64, PathBuf)>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            collect_files(&path, out)?;
        } else if path.extension().map(|e| e == "ogg").unwrap_or(false) {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            out.push((mtime, meta.len(), path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_starts_with_an_ogg_page() {
        let dir = std::env::temp_dir().join(format!("aether-voice-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("s.ogg");
        {
            let mut w = OggOpusWriter::create(&path, &[("ARTIST".into(), "Steve".into())]).unwrap();
            w.write_frame(&[0xaa; 40]).unwrap();
            w.finish().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"OggS");
        // The first page is a BOS page holding OpusHead.
        assert_eq!(bytes[5] & 0x02, 0x02, "beginning-of-stream flag");
        let head_at = 27 + bytes[26] as usize;
        assert_eq!(&bytes[head_at..head_at + 8], b"OpusHead");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_crc_is_recorded_and_verifiable() {
        let head = opus_head();
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(0x02);
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&SERIAL.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        let crc_at = page.len();
        page.extend_from_slice(&[0, 0, 0, 0]);
        page.push(1);
        page.push(head.len() as u8);
        page.extend_from_slice(&head);
        let crc = ogg_crc(&page);
        page[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
        // Recomputing over the page with its own CRC zeroed gives the same
        // value, which is what a decoder checks.
        let mut check = page.clone();
        check[crc_at..crc_at + 4].fill(0);
        assert_eq!(ogg_crc(&check), crc);
        // A flipped byte changes it.
        let mut bad = check.clone();
        bad[crc_at + 5] ^= 0xff;
        assert_ne!(ogg_crc(&bad), crc);
    }

    #[test]
    fn targets_match_the_right_speakers() {
        assert!(Target::All.captures(1, None));
        assert!(Target::Player(1).captures(1, None));
        assert!(!Target::Player(2).captures(1, None));
        assert!(Target::Group(9).captures(1, Some(9)));
        assert!(!Target::Group(9).captures(1, Some(7)));
        assert!(!Target::None.captures(1, None));
    }
}
