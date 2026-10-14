//! Item types: names ⇄ dense 2-byte ids.
//!
//! Items used to travel as `String`s — through inventories, journal events and
//! the ledger — which cost an allocation and 20–40 bytes per stack and made
//! every comparison a string compare. An [`ItemTypeId`] is two bytes.
//!
//! The scheme is the block registry's ([`super::BlockRegistry`]): every vanilla
//! 1.21.11 item is a compile-time entry at its vanilla id ([`ITEM_NAMES`]), so
//! the ids of everything a stock world holds are the same in every world and
//! every build that pins this version. Names the table lacks — a modded item —
//! are interned from [`ITEM_BEYOND`] up, and only *those* are what a world's
//! saved table has to carry. That table is append-only: an id once handed out
//! never changes meaning.

use std::collections::HashMap;
use std::sync::OnceLock;

pub use super::item_names::ITEM_NAMES;

/// An item type. `ItemTypeId::AIR` (0) is "no item" and never names a real one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemTypeId(pub u16);

impl ItemTypeId {
    /// `minecraft:air`: the empty slot, never a stack's type.
    pub const AIR: ItemTypeId = ItemTypeId(0);

    /// The id of a vanilla item, without any world: vanilla ids are fixed.
    ///
    /// For a name that must exist — a literal in the game's own tables — a
    /// `None` here is a typo, and callers should treat it as a bug.
    pub fn vanilla(name: &str) -> Option<ItemTypeId> {
        vanilla_lookup().get(name).copied()
    }

    /// The vanilla name of this id, if it is a vanilla item.
    pub fn vanilla_name(self) -> Option<&'static str> {
        ITEM_NAMES.get(self.0 as usize).copied()
    }
}

/// The first id that is not a vanilla item.
pub const ITEM_BEYOND: u16 = ITEM_NAMES.len() as u16;

/// Name → id for the vanilla table, built once.
fn vanilla_lookup() -> &'static HashMap<&'static str, ItemTypeId> {
    static L: OnceLock<HashMap<&'static str, ItemTypeId>> = OnceLock::new();
    L.get_or_init(|| {
        ITEM_NAMES
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, ItemTypeId(i as u16)))
            .collect()
    })
}

/// The world's name for an id, installed at startup.
///
/// The journal stores two-byte ids, but the surfaces a human reads — chat,
/// `/audit`, the PostgreSQL mirror — must show names, and a modded item's name
/// lives in the world that interned it and nowhere a library can see. The
/// server therefore hands the world's registry in once (the same shape as
/// [`crate::journal::install_allocator`]); a library with no world falls back
/// to the vanilla table.
static NAME_RESOLVER: OnceLock<Box<dyn Fn(ItemTypeId) -> Option<String> + Send + Sync>> =
    OnceLock::new();

/// The world's id for a name, installed at startup. See [`NAME_RESOLVER`].
static ID_RESOLVER: OnceLock<Box<dyn Fn(&str) -> Option<ItemTypeId> + Send + Sync>> =
    OnceLock::new();

/// Install the world's item name ⇄ id resolvers. Only the first call counts.
pub fn install_resolvers(
    name: Box<dyn Fn(ItemTypeId) -> Option<String> + Send + Sync>,
    id: Box<dyn Fn(&str) -> Option<ItemTypeId> + Send + Sync>,
) {
    let _ = NAME_RESOLVER.set(name);
    let _ = ID_RESOLVER.set(id);
}

/// The display name of an item id: the world's registry if one is installed,
/// else the vanilla table, else `item#N` so a log line never says nothing.
pub fn display_name(id: ItemTypeId) -> String {
    if let Some(f) = NAME_RESOLVER.get() {
        if let Some(n) = f(id) {
            return n;
        }
    }
    id.vanilla_name()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("item#{}", id.0))
}

/// The id of an item name: the world's registry if one is installed, else the
/// vanilla table.
pub fn id_of(name: &str) -> Option<ItemTypeId> {
    ID_RESOLVER
        .get()
        .and_then(|f| f(name))
        .or_else(|| ItemTypeId::vanilla(name))
}

/// Maps item names ⇄ ids for one world: the vanilla table plus whatever this
/// world has interned beyond it.
#[derive(Debug, Clone, Default)]
pub struct ItemRegistry {
    /// Names interned beyond the vanilla table, in id order from [`ITEM_BEYOND`].
    names: Vec<String>,
    lookup: HashMap<String, ItemTypeId>,
}

/// Interning failed: the 2-byte id space is used up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemIdsExhausted;

impl std::fmt::Display for ItemIdsExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "all {} item ids are in use", u16::MAX as u32 + 1)
    }
}

impl std::error::Error for ItemIdsExhausted {}

impl ItemRegistry {
    /// A registry holding only the vanilla items.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a known name, without interning.
    pub fn get(&self, name: &str) -> Option<ItemTypeId> {
        ItemTypeId::vanilla(name).or_else(|| self.lookup.get(name).copied())
    }

    /// Resolve `name`, interning it beyond the vanilla table if it is new.
    pub fn intern(&mut self, name: &str) -> Result<ItemTypeId, ItemIdsExhausted> {
        if let Some(id) = self.get(name) {
            return Ok(id);
        }
        let raw = ITEM_BEYOND as usize + self.names.len();
        let raw = u16::try_from(raw).map_err(|_| ItemIdsExhausted)?;
        let id = ItemTypeId(raw);
        self.names.push(name.to_owned());
        self.lookup.insert(name.to_owned(), id);
        Ok(id)
    }

    /// The name of an id, vanilla or interned.
    pub fn name(&self, id: ItemTypeId) -> Option<&str> {
        match id.vanilla_name() {
            Some(n) => Some(n),
            None => self
                .names
                .get((id.0 - ITEM_BEYOND) as usize)
                .map(String::as_str),
        }
    }

    /// Every interned (non-vanilla) name, in id order from [`ITEM_BEYOND`]:
    /// what a world's saved table carries.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Rebuild from a saved [`Self::names`] table: re-intern in order, so
    /// every id keeps the meaning it was saved with.
    ///
    /// A table naming a vanilla item cannot have come from this build — this
    /// build would have answered that name from the vanilla table and never
    /// interned it — so it was written against a different version pin, and
    /// re-interning it would shift every id after it. That is refused.
    pub fn restore(table: &[String]) -> Result<Self, RestoreError> {
        let mut r = Self::new();
        for (i, name) in table.iter().enumerate() {
            if ItemTypeId::vanilla(name).is_some() {
                return Err(RestoreError::NamesVanillaItem {
                    index: i,
                    name: name.clone(),
                });
            }
            if r.lookup.contains_key(name) {
                return Err(RestoreError::Duplicate { name: name.clone() });
            }
            r.intern(name).map_err(|_| RestoreError::TooLong)?;
        }
        Ok(r)
    }

    /// Number of addressable item types: vanilla plus interned.
    pub fn len(&self) -> usize {
        ITEM_BEYOND as usize + self.names.len()
    }

    /// Never true: the vanilla table is always there.
    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Why a saved item table could not be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    /// The table names a vanilla item: written by a build with another pin.
    NamesVanillaItem { index: usize, name: String },
    /// The same name twice: not something this registry ever writes.
    Duplicate { name: String },
    /// More names than the id space holds.
    TooLong,
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::NamesVanillaItem { index, name } => write!(
                f,
                "saved item table entry {index} is the vanilla item {name}: written by a build \
                 with a different version pin"
            ),
            RestoreError::Duplicate { name } => write!(f, "saved item table repeats {name}"),
            RestoreError::TooLong => write!(f, "saved item table exceeds the id space"),
        }
    }
}

impl std::error::Error for RestoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_ids_are_the_1_21_11_item_ids() {
        assert_eq!(ItemTypeId::vanilla("minecraft:air"), Some(ItemTypeId::AIR));
        assert_eq!(ItemTypeId::vanilla("minecraft:stone"), Some(ItemTypeId(1)));
        assert_eq!(ItemTypeId(1).vanilla_name(), Some("minecraft:stone"));
        assert_eq!(ItemTypeId::vanilla("minecraft:no_such_item"), None);
    }

    #[test]
    fn every_vanilla_name_maps_back_to_its_own_id() {
        for (i, n) in ITEM_NAMES.iter().enumerate() {
            assert_eq!(ItemTypeId::vanilla(n), Some(ItemTypeId(i as u16)), "{n}");
        }
    }

    #[test]
    fn a_modded_item_is_interned_beyond_vanilla_and_is_stable() {
        let mut r = ItemRegistry::new();
        let a = r.intern("mymod:gizmo").unwrap();
        let b = r.intern("mymod:widget").unwrap();
        assert_eq!(a, ItemTypeId(ITEM_BEYOND));
        assert_eq!(b, ItemTypeId(ITEM_BEYOND + 1));
        assert_eq!(
            r.intern("mymod:gizmo").unwrap(),
            a,
            "interning twice is a lookup"
        );
        assert_eq!(r.name(b), Some("mymod:widget"));
        assert_eq!(
            r.intern("minecraft:stone").unwrap(),
            ItemTypeId(1),
            "vanilla is never interned"
        );
        assert_eq!(r.names(), ["mymod:gizmo", "mymod:widget"]);
    }

    #[test]
    fn a_restored_table_keeps_every_id() {
        let mut r = ItemRegistry::new();
        let ids: Vec<_> = ["a:x", "b:y", "c:z"]
            .iter()
            .map(|n| r.intern(n).unwrap())
            .collect();
        let back = ItemRegistry::restore(r.names()).unwrap();
        for (n, id) in ["a:x", "b:y", "c:z"].iter().zip(ids) {
            assert_eq!(back.get(n), Some(id));
        }
    }

    #[test]
    fn a_table_from_another_version_pin_is_refused_not_reinterpreted() {
        let table = vec!["a:x".to_string(), "minecraft:stone".to_string()];
        assert!(matches!(
            ItemRegistry::restore(&table),
            Err(RestoreError::NamesVanillaItem { index: 1, .. })
        ));
    }

    #[test]
    fn a_table_with_a_repeated_name_is_refused() {
        let table = vec!["a:x".to_string(), "a:x".to_string()];
        assert!(matches!(
            ItemRegistry::restore(&table),
            Err(RestoreError::Duplicate { .. })
        ));
    }

    #[test]
    fn the_id_space_running_out_is_an_error_not_a_wraparound() {
        let mut r = ItemRegistry::new();
        let room = u16::MAX as usize + 1 - ITEM_BEYOND as usize;
        for i in 0..room {
            r.intern(&format!("m:{i}")).unwrap();
        }
        assert_eq!(r.intern("m:one_too_many"), Err(ItemIdsExhausted));
    }
}
