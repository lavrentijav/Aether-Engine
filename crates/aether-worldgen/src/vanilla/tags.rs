//! Block tags from the pack (`data/minecraft/tags/block/*.json`), resolved to
//! sets of block ids, plus `HolderSet`-style "a tag or a list of blocks".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aether_world::registry::blocks;
use aether_world::BlockStateId;

use super::blockinfo;
use super::density::DataPack;
use super::json::Json;

/// A set of blocks (not states).
#[derive(Debug, Clone)]
pub struct BlockSet(Arc<Vec<bool>>);

impl BlockSet {
    /// The empty set.
    pub fn empty() -> Self {
        Self(Arc::new(vec![false; blocks::BLOCKS.len()]))
    }

    /// Whether a state's block is in the set.
    #[inline]
    pub fn contains(&self, s: BlockStateId) -> bool {
        self.0.get(blockinfo::block_of(s) as usize).copied().unwrap_or(false)
    }

    /// Whether a block id is in the set.
    #[inline]
    pub fn contains_block(&self, b: u16) -> bool {
        self.0.get(b as usize).copied().unwrap_or(false)
    }
}

/// Resolves and memoizes block tags.
pub struct Tags {
    pack: DataPack,
    cache: Mutex<HashMap<String, BlockSet>>,
}

impl Tags {
    /// Over a pack.
    pub fn new(pack: &DataPack) -> Self {
        Self {
            pack: pack.clone(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The block tag `id` (with or without `#`), recursively resolved. A
    /// missing tag is empty.
    pub fn block_tag(&self, id: &str) -> BlockSet {
        let id = id.trim_start_matches('#');
        let id = if id.contains(':') { id.to_string() } else { format!("minecraft:{id}") };
        if let Some(s) = self.cache.lock().unwrap().get(&id) {
            return s.clone();
        }
        let mut bits = vec![false; blocks::BLOCKS.len()];
        self.collect(&id, &mut bits, 0);
        let set = BlockSet(Arc::new(bits));
        self.cache.lock().unwrap().insert(id, set.clone());
        set
    }

    fn collect(&self, id: &str, bits: &mut [bool], depth: usize) {
        if depth > 32 {
            return;
        }
        let Some(j) = self.pack.tag_json("block", id) else {
            return;
        };
        for v in j.get("values").and_then(Json::as_arr).unwrap_or(&[]) {
            let name = match v {
                Json::Str(s) => s.as_str(),
                Json::Obj(_) => v.str_of("id").unwrap_or(""),
                _ => continue,
            };
            if let Some(tag) = name.strip_prefix('#') {
                self.collect(tag, bits, depth + 1);
            } else if let Some(b) = blocks::block_id_of(name) {
                bits[b as usize] = true;
            }
        }
    }

    /// A `HolderSet<Block>`: `"#tag"`, `"minecraft:block"` or a list of
    /// blocks.
    pub fn holder_set(&self, j: &Json) -> BlockSet {
        match j {
            Json::Str(s) if s.starts_with('#') => self.block_tag(s),
            Json::Str(s) => {
                let mut bits = vec![false; blocks::BLOCKS.len()];
                if let Some(b) = blocks::block_id_of(s) {
                    bits[b as usize] = true;
                }
                BlockSet(Arc::new(bits))
            }
            Json::Arr(a) => {
                let mut bits = vec![false; blocks::BLOCKS.len()];
                for v in a {
                    if let Some(b) = v.as_str().and_then(blocks::block_id_of) {
                        bits[b as usize] = true;
                    }
                }
                BlockSet(Arc::new(bits))
            }
            _ => BlockSet::empty(),
        }
    }
}
