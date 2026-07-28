//! Anvil region (`.mca`) reading and chunk-section decoding.
//!
//! An Anvil region file stores a 32×32 grid of chunks behind an 8 KiB header
//! (1024 location entries + 1024 timestamps). Each present chunk is a
//! length-prefixed, compressed NBT blob. This module extracts those blobs,
//! decompresses them (zlib / uncompressed), and decodes the modern (1.16+)
//! `block_states` palette + packed `data` layout into engine [`SubChunk`]s.

use crate::block_map::Interner;
use crate::nbt::{self, Nbt};
use aether_world::SubChunk;
use std::path::Path;

/// Upper bound on a single decompressed chunk's NBT (16 MiB). Real chunks are
/// far smaller; the cap stops a crafted region file from forcing an unbounded
/// allocation during zlib inflation.
const MAX_CHUNK_NBT: usize = 16 * 1024 * 1024;

/// Errors from reading an Anvil region or decoding a chunk.
#[derive(Debug)]
pub enum AnvilError {
    /// I/O failure reading the file.
    Io(std::io::Error),
    /// The region file is smaller than its 8 KiB header.
    ShortHeader,
    /// A chunk's declared extent runs past the end of the file.
    BadChunkExtent,
    /// The chunk used a compression scheme this reader does not implement.
    UnsupportedCompression(u8),
    /// Decompression failed.
    Inflate(String),
    /// The chunk NBT could not be parsed.
    Nbt(nbt::NbtError),
    /// The region file name was not `r.<x>.<z>.mca`.
    BadRegionName,
}

impl std::fmt::Display for AnvilError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnvilError::Io(e) => write!(f, "io error: {e}"),
            AnvilError::ShortHeader => f.write_str("region file shorter than its header"),
            AnvilError::BadChunkExtent => f.write_str("chunk extent runs past end of file"),
            AnvilError::UnsupportedCompression(c) => {
                write!(f, "unsupported chunk compression scheme {c}")
            }
            AnvilError::Inflate(e) => write!(f, "decompression failed: {e}"),
            AnvilError::Nbt(e) => write!(f, "nbt error: {e}"),
            AnvilError::BadRegionName => f.write_str("region file name is not r.<x>.<z>.mca"),
        }
    }
}

impl std::error::Error for AnvilError {}

impl From<std::io::Error> for AnvilError {
    fn from(e: std::io::Error) -> Self {
        AnvilError::Io(e)
    }
}

/// A loaded region file (raw bytes) plus its region grid coordinates.
pub struct Region {
    data: Vec<u8>,
    region_x: i32,
    region_z: i32,
}

/// One decoded chunk: global chunk column coordinates and its sub-chunks.
pub struct DecodedChunk {
    /// Global chunk X.
    pub cx: i32,
    /// Global chunk Z.
    pub cz: i32,
    /// `(sub-chunk Y index, sub-chunk)` for every non-empty section.
    pub sections: Vec<(i8, SubChunk)>,
}

/// Parse the region grid coordinates from a `r.<x>.<z>.mca` path.
pub fn region_coords_from_path(path: &Path) -> Result<(i32, i32), AnvilError> {
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(AnvilError::BadRegionName)?;
    // r.<x>.<z>.mca
    let parts: Vec<&str> = stem.split('.').collect();
    if parts.len() != 4 || parts[0] != "r" || parts[3] != "mca" {
        return Err(AnvilError::BadRegionName);
    }
    let x = parts[1]
        .parse::<i32>()
        .map_err(|_| AnvilError::BadRegionName)?;
    let z = parts[2]
        .parse::<i32>()
        .map_err(|_| AnvilError::BadRegionName)?;
    Ok((x, z))
}

impl Region {
    /// Load a region file from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AnvilError> {
        let path = path.as_ref();
        let (region_x, region_z) = region_coords_from_path(path)?;
        let data = std::fs::read(path)?;
        if data.len() < 8192 {
            return Err(AnvilError::ShortHeader);
        }
        Ok(Self {
            data,
            region_x,
            region_z,
        })
    }

    /// Construct from in-memory bytes (used by tests).
    pub fn from_bytes(data: Vec<u8>, region_x: i32, region_z: i32) -> Result<Self, AnvilError> {
        if data.len() < 8192 {
            return Err(AnvilError::ShortHeader);
        }
        Ok(Self {
            data,
            region_x,
            region_z,
        })
    }

    /// Decompressed NBT bytes for the chunk at local `(lx, lz)` (each `0..32`),
    /// or `None` if that slot is empty.
    fn chunk_nbt(&self, lx: usize, lz: usize) -> Result<Option<Vec<u8>>, AnvilError> {
        let header_index = (lx + lz * 32) * 4;
        let loc = &self.data[header_index..header_index + 4];
        let sector_offset = ((loc[0] as usize) << 16) | ((loc[1] as usize) << 8) | loc[2] as usize;
        let sector_count = loc[3] as usize;
        if sector_offset == 0 || sector_count == 0 {
            return Ok(None);
        }
        let start = sector_offset * 4096;
        if start + 5 > self.data.len() {
            return Err(AnvilError::BadChunkExtent);
        }
        let length = u32::from_be_bytes(self.data[start..start + 4].try_into().unwrap()) as usize;
        if length == 0 {
            return Ok(None);
        }
        let compression = self.data[start + 4];
        let payload_start = start + 5;
        let payload_end = start + 4 + length;
        if payload_end > self.data.len() {
            return Err(AnvilError::BadChunkExtent);
        }
        let payload = &self.data[payload_start..payload_end];
        let raw = match compression {
            // 2 = zlib (Minecraft default), 3 = uncompressed.
            // Bound the output so a crafted payload can't drive an unbounded
            // allocation; a single chunk's NBT is comfortably under this.
            2 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(payload, MAX_CHUNK_NBT)
                .map_err(|e| AnvilError::Inflate(format!("{e:?}")))?,
            3 => payload.to_vec(),
            other => return Err(AnvilError::UnsupportedCompression(other)),
        };
        Ok(Some(raw))
    }

    /// Decode every present chunk in the region.
    ///
    /// `registry` is shared so block ids stay consistent across the whole world.
    /// Per-chunk errors are collected into `errors` rather than aborting.
    pub fn decode_all<I: Interner>(
        &self,
        registry: &mut I,
        errors: &mut Vec<String>,
    ) -> Vec<DecodedChunk> {
        let mut chunks = Vec::new();
        for lz in 0..32 {
            for lx in 0..32 {
                match self.chunk_nbt(lx, lz) {
                    Ok(None) => {}
                    Ok(Some(bytes)) => match nbt::parse(&bytes) {
                        Ok(root) => {
                            let cx = self.region_x * 32 + lx as i32;
                            let cz = self.region_z * 32 + lz as i32;
                            match decode_sections(&root, registry) {
                                Ok(sections) => chunks.push(DecodedChunk { cx, cz, sections }),
                                Err(e) => errors.push(format!("chunk ({cx},{cz}): {e}")),
                            }
                        }
                        Err(e) => errors.push(format!("chunk local ({lx},{lz}): nbt {e}")),
                    },
                    Err(e) => errors.push(format!("chunk local ({lx},{lz}): {e}")),
                }
            }
        }
        chunks
    }
}

/// Decode the `sections` list of a chunk NBT into non-empty sub-chunks.
pub fn decode_sections<I: Interner>(
    root: &Nbt,
    registry: &mut I,
) -> Result<Vec<(i8, SubChunk)>, AnvilError> {
    let sections = match root.get("sections").and_then(Nbt::as_list) {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for section in sections {
        let y = match section.get("Y").and_then(Nbt::as_i64) {
            Some(y) => y as i8,
            None => continue,
        };
        let block_states = match section.get("block_states") {
            Some(bs) => bs,
            None => continue,
        };
        let palette = match block_states.get("palette").and_then(Nbt::as_list) {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };

        // Resolve every palette entry to an engine (id, props) once.
        let resolved: Vec<_> = palette
            .iter()
            .map(|entry| {
                let name = entry
                    .get("Name")
                    .and_then(Nbt::as_str)
                    .unwrap_or("minecraft:air");
                registry.intern(name)
            })
            .collect();

        // Single-entry palette: whole section is one block; skip if it's air.
        let data = block_states.get("data").and_then(Nbt::as_long_array);
        if resolved.len() == 1 {
            let (id, props) = resolved[0];
            if id == aether_world::BlockStateId::AIR {
                continue;
            }
            let mut sc = SubChunk::new();
            for y2 in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        sc.set(x, y2, z, id, props);
                    }
                }
            }
            out.push((y, sc));
            continue;
        }

        let Some(data) = data else { continue };
        let bits = bits_per_index(resolved.len());
        let per_long = 64 / bits;
        let mask = (1u64 << bits) - 1;
        let mut sc = SubChunk::new();
        let mut any = false;
        for i in 0..4096usize {
            let long_idx = i / per_long;
            let within = i % per_long;
            let value = match data.get(long_idx) {
                Some(&w) => ((w as u64) >> (within * bits)) & mask,
                None => 0,
            } as usize;
            if value >= resolved.len() {
                continue; // corrupt index; leave as air
            }
            let (id, props) = resolved[value];
            if id == aether_world::BlockStateId::AIR {
                continue;
            }
            // Anvil block order within a section is YZX.
            let x = i & 15;
            let z = (i >> 4) & 15;
            let by = (i >> 8) & 15;
            sc.set(x, by, z, id, props);
            any = true;
        }
        if any {
            out.push((y, sc));
        }
    }
    Ok(out)
}

/// Bits per packed index for a palette of `len` entries (Anvil: min 4).
fn bits_per_index(len: usize) -> usize {
    let needed = usize::BITS - (len - 1).leading_zeros();
    (needed as usize).max(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_map::BlockRegistry;

    #[test]
    fn bits_per_index_matches_anvil() {
        assert_eq!(bits_per_index(1), 4);
        assert_eq!(bits_per_index(16), 4);
        assert_eq!(bits_per_index(17), 5);
        assert_eq!(bits_per_index(256), 8);
        assert_eq!(bits_per_index(257), 9);
    }

    #[test]
    fn region_name_parsing() {
        let p = Path::new("/world/region/r.-1.2.mca");
        assert_eq!(region_coords_from_path(p).unwrap(), (-1, 2));
        assert!(region_coords_from_path(Path::new("bogus.mca")).is_err());
    }

    #[test]
    fn short_file_is_rejected() {
        assert!(matches!(
            Region::from_bytes(vec![0u8; 10], 0, 0),
            Err(AnvilError::ShortHeader)
        ));
    }

    #[test]
    fn decodes_a_single_block_palette_section() {
        // Build a chunk NBT with one section whose palette is [air, stone] and
        // data selects stone for block index 0 only.
        let root = build_chunk_nbt();
        let mut reg = BlockRegistry::new();
        let sections = decode_sections(&root, &mut reg).unwrap();
        assert_eq!(sections.len(), 1);
        let (y, sc) = &sections[0];
        assert_eq!(*y, 0);
        // block index 0 => (x=0,y=0,z=0) is stone, rest air
        assert_ne!(sc.get(0, 0, 0), aether_world::BlockStateId::AIR);
        assert_eq!(sc.get(1, 0, 0), aether_world::BlockStateId::AIR);
    }

    fn build_chunk_nbt() -> Nbt {
        // palette entry compounds
        let air = Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:air".into()))]);
        let stone = Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:stone".into()))]);
        // bits=4, per_long=16; index 0 => stone(1), others air(0). Only first long's
        // lowest nibble is 1.
        let mut data = vec![0i64; 256];
        data[0] = 1; // block 0 -> palette index 1
        let block_states = Nbt::Compound(vec![
            ("palette".into(), Nbt::List(vec![air, stone])),
            ("data".into(), Nbt::LongArray(data)),
        ]);
        let section = Nbt::Compound(vec![
            ("Y".into(), Nbt::Byte(0)),
            ("block_states".into(), block_states),
        ]);
        Nbt::Compound(vec![("sections".into(), Nbt::List(vec![section]))])
    }
}
