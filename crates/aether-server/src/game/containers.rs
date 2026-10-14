//! Block entities that hold items: chests, furnaces, hoppers and dispensers.
//!
//! Kept in memory once touched and written through to the world's KV store
//! on every change, under their own key prefix, so they survive a restart the
//! same way inventories do. Furnaces also tick: the ones with fuel burning or
//! something to cook are kept in an active set the game tick walks. Hoppers
//! move items on the block-tick scheduler ([`super::blockticks::hopper`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::inventory::Stack;
use crate::session::DemoWorld;

/// What kind of container a block is, by block name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Chest,
    Furnace,
    /// Five slots; also the block that moves items between containers.
    Hopper,
    /// Nine slots, shared by the dispenser and the dropper.
    Dispenser,
}

impl ContainerKind {
    /// The container a block of this name is, if any.
    pub fn of_block(name: &str) -> Option<Self> {
        let short = name.strip_prefix("minecraft:").unwrap_or(name);
        match short {
            "chest" | "trapped_chest" | "barrel" => Some(Self::Chest),
            "furnace" | "smoker" | "blast_furnace" => Some(Self::Furnace),
            "hopper" => Some(Self::Hopper),
            "dispenser" | "dropper" => Some(Self::Dispenser),
            _ => None,
        }
    }

    pub fn slots(self) -> usize {
        match self {
            Self::Chest => 27,
            Self::Furnace => 3,
            Self::Hopper => 5,
            Self::Dispenser => 9,
        }
    }

    /// The byte the KV format stores this kind as.
    fn byte(self) -> u8 {
        match self {
            Self::Chest => 0,
            Self::Furnace => 1,
            Self::Hopper => 2,
            Self::Dispenser => 3,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Chest),
            1 => Some(Self::Furnace),
            2 => Some(Self::Hopper),
            3 => Some(Self::Dispenser),
            _ => None,
        }
    }
}

/// One container's contents and, for a furnace, its fire.
#[derive(Debug, Clone)]
pub struct Container {
    pub kind: ContainerKind,
    pub slots: Vec<Option<Stack>>,
    /// Ticks of fuel left / the fuel's full burn time.
    pub burn: u32,
    pub burn_max: u32,
    /// Ticks the current item has cooked, out of 200.
    pub cook: u32,
    /// Experience a furnace has stored, waiting for a player to take the
    /// output (vanilla's `recipeCookingTime`).
    pub xp: f32,
}

impl Container {
    pub fn new(kind: ContainerKind) -> Self {
        Self {
            kind,
            slots: vec![None; kind.slots()],
            burn: 0,
            burn_max: 0,
            cook: 0,
            xp: 0.0,
        }
    }
}

type Pos = (i32, i32, i32);

fn all() -> &'static Mutex<HashMap<Pos, Container>> {
    static M: OnceLock<Mutex<HashMap<Pos, Container>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active() -> &'static Mutex<HashSet<Pos>> {
    static M: OnceLock<Mutex<HashSet<Pos>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashSet::new()))
}

fn key(pos: Pos) -> [u8; 13] {
    let mut k = [0u8; 13];
    k[0] = b'K';
    k[1..5].copy_from_slice(&pos.0.to_be_bytes());
    k[5..9].copy_from_slice(&pos.1.to_be_bytes());
    k[9..13].copy_from_slice(&pos.2.to_be_bytes());
    k
}

/// Bumped whenever the layout changes. Version 3 replaced version 2's
/// furnace-or-chest flag with a kind byte, so hoppers and dispensers get
/// their own container type; versions 1 and 2 still load.
fn encode(c: &Container) -> Vec<u8> {
    let mut out = vec![3u8, c.kind.byte()];
    out.extend_from_slice(&c.burn.to_be_bytes());
    out.extend_from_slice(&c.burn_max.to_be_bytes());
    out.extend_from_slice(&c.cook.to_be_bytes());
    out.extend_from_slice(&c.xp.to_be_bytes());
    let mut inv = crate::inventory::Inventory::new();
    for (i, s) in c.slots.iter().enumerate() {
        inv.put_slot(i, s.clone());
    }
    out.extend_from_slice(&crate::inventory::encode(&inv));
    out
}

fn decode(b: &[u8]) -> Option<Container> {
    // Version 1 containers predate smelting experience and have none;
    // versions 1 and 2 wrote only a furnace-or-chest flag.
    let (kind, xp, inv_at) = match *b.first()? {
        1 => (ContainerKind::from_byte(*b.get(1)?)?, 0.0f32, 14usize),
        2 => (
            ContainerKind::from_byte(*b.get(1)?)?,
            f32::from_be_bytes(b.get(14..18)?.try_into().unwrap()),
            18usize,
        ),
        3 => (
            ContainerKind::from_byte(*b.get(1)?)?,
            f32::from_be_bytes(b.get(14..18)?.try_into().unwrap()),
            18usize,
        ),
        _ => return None,
    };
    if b.len() < inv_at {
        return None;
    }
    let u = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
    let inv = crate::inventory::decode(&b[inv_at..])?;
    let mut c = Container::new(kind);
    c.burn = u(2);
    c.burn_max = u(6);
    c.cook = u(10);
    c.xp = xp;
    for i in 0..c.slots.len() {
        c.slots[i] = inv.slot(i).cloned();
    }
    Some(c)
}

/// Load the container at `pos` from the world, or a fresh empty one of `kind`.
fn load(world: &DemoWorld, pos: Pos, kind: ContainerKind) -> Container {
    world
        .get_meta(&key(pos))
        .ok()
        .flatten()
        .and_then(|b| decode(&b))
        .filter(|c| c.kind == kind)
        .unwrap_or_else(|| Container::new(kind))
}

/// Run `f` over the container at `pos`, loading or creating it as `kind`.
/// The result is saved afterwards.
pub fn with<R>(
    world: &DemoWorld,
    pos: Pos,
    kind: ContainerKind,
    f: impl FnOnce(&mut Container) -> R,
) -> R {
    let mut map = all().lock().unwrap();
    let c = map.entry(pos).or_insert_with(|| load(world, pos, kind));
    let r = f(c);
    let _ = world.put_meta(&key(pos), &encode(c));
    if c.kind == ContainerKind::Furnace {
        active().lock().unwrap().insert(pos);
    }
    r
}

/// Run `f` over **two** containers at once — a double chest's halves — and
/// save both afterwards.
pub fn with_pair<R>(
    world: &DemoWorld,
    a: Pos,
    b: Pos,
    f: impl FnOnce(&mut Container, &mut Container) -> R,
) -> R {
    let mut map = all().lock().unwrap();
    let mut ca = map
        .remove(&a)
        .unwrap_or_else(|| load(world, a, ContainerKind::Chest));
    let mut cb = map
        .remove(&b)
        .unwrap_or_else(|| load(world, b, ContainerKind::Chest));
    let r = f(&mut ca, &mut cb);
    let _ = world.put_meta(&key(a), &encode(&ca));
    let _ = world.put_meta(&key(b), &encode(&cb));
    map.insert(a, ca);
    map.insert(b, cb);
    r
}

/// A copy of the container at `pos`, if one exists there.
pub fn snapshot(world: &DemoWorld, pos: Pos, kind: ContainerKind) -> Container {
    with(world, pos, kind, |c| c.clone())
}

/// Copies of both halves of a double chest.
pub fn snapshot_pair(world: &DemoWorld, a: Pos, b: Pos) -> (Container, Container) {
    with_pair(world, a, b, |ca, cb| (ca.clone(), cb.clone()))
}

/// The kind of container at `pos` and the positions its slots live in — a
/// doubled chest is two, left half first, in the order the window presents
/// them. `None` when the block is not a container at all.
pub fn parts_of(world: &DemoWorld, pos: Pos) -> Option<(ContainerKind, Vec<Pos>)> {
    let state = world.get_block(pos.0, pos.1, pos.2);
    let name = crate::game::block_name(world, state)?;
    let kind = ContainerKind::of_block(&name)?;
    if kind == ContainerKind::Chest {
        let (left, right) =
            crate::placement::chest_halves(state, pos.0, pos.1, pos.2, &|x, y, z| {
                world.get_block(x, y, z)
            });
        let mut parts = vec![left];
        if let Some(right) = right {
            parts.push(right);
        }
        Some((kind, parts))
    } else {
        Some((kind, vec![pos]))
    }
}

/// Move a single item from the container at `from` into the container at `to`,
/// the way a hopper does: merge into a matching stack first, otherwise the
/// first empty slot. Returns whether an item moved.
///
/// A double chest is handled as one inventory, both halves merged, so an item
/// can be pushed into or pulled from either half.
pub fn move_one(world: &DemoWorld, from: Pos, to: Pos) -> bool {
    if from == to {
        return false;
    }
    let (Some((from_kind, from_parts)), Some((to_kind, to_parts))) =
        (parts_of(world, from), parts_of(world, to))
    else {
        return false;
    };
    if from_parts.iter().any(|p| to_parts.contains(p)) {
        return false;
    }
    {
        let mut map = all().lock().unwrap();
        let load_parts = |map: &mut HashMap<Pos, Container>,
                          kind: ContainerKind,
                          parts: &[Pos]|
         -> Vec<Container> {
            parts
                .iter()
                .map(|p| {
                    map.entry(*p)
                        .or_insert_with(|| load(world, *p, kind))
                        .clone()
                })
                .collect()
        };
        let from_conts = load_parts(&mut map, from_kind, &from_parts);
        let to_conts = load_parts(&mut map, to_kind, &to_parts);
        let mut src: Vec<Option<Stack>> = from_conts
            .iter()
            .flat_map(|c| c.slots.iter().cloned())
            .collect();
        let mut dst: Vec<Option<Stack>> = to_conts
            .iter()
            .flat_map(|c| c.slots.iter().cloned())
            .collect();
        if !transfer_one(&mut src, &mut dst) {
            return false;
        }
        // Split the merged lists back into their halves and re-encode.
        let mut at = 0;
        for (i, c) in from_conts.iter().enumerate() {
            let n = c.slots.len();
            let mut c = c.clone();
            c.slots.clone_from_slice(&src[at..at + n]);
            at += n;
            let _ = world.put_meta(&key(from_parts[i]), &encode(&c));
            map.insert(from_parts[i], c);
        }
        let mut at = 0;
        for (i, c) in to_conts.iter().enumerate() {
            let n = c.slots.len();
            let mut c = c.clone();
            c.slots.clone_from_slice(&dst[at..at + n]);
            at += n;
            let _ = world.put_meta(&key(to_parts[i]), &encode(&c));
            map.insert(to_parts[i], c);
        }
    }
    // An item arriving at a furnace may start it cooking.
    if to_kind == ContainerKind::Furnace {
        for p in &to_parts {
            active().lock().unwrap().insert(*p);
        }
    }
    true
}

/// Move exactly one item from `src` into `dst`, vanilla's hopper transfer.
/// Both are merged slot lists; either may be empty. Returns whether anything
/// moved.
fn transfer_one(src: &mut [Option<Stack>], dst: &mut [Option<Stack>]) -> bool {
    for slot in src.iter_mut() {
        let Some(stack) = slot.as_ref() else {
            continue;
        };
        if stack.count == 0 {
            continue;
        }
        let max = stack.max_stack();
        let merge = dst.iter().position(|d| {
            d.as_ref()
                .is_some_and(|e| e.stacks_with(stack) && e.count < max)
        });
        if let Some(j) = merge {
            dst[j].as_mut().unwrap().count += 1;
            let s = slot.as_mut().unwrap();
            s.count -= 1;
            if s.count == 0 {
                *slot = None;
            }
            return true;
        }
        let Some(j) = dst.iter().position(|d| d.is_none()) else {
            continue;
        };
        if slot.as_ref().unwrap().count == 1 {
            dst[j] = slot.take();
        } else {
            dst[j] = Some(slot.as_mut().unwrap().split(1));
        }
        return true;
    }
    false
}

/// The block at `pos` is gone: forget its container and return the items,
/// each with the slot it was in.
pub fn remove(world: &DemoWorld, pos: Pos, kind: ContainerKind) -> Vec<(usize, Stack)> {
    let items = with(world, pos, kind, |c| {
        c.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| s.take().map(|s| (i, s)))
            .collect()
    });
    all().lock().unwrap().remove(&pos);
    active().lock().unwrap().remove(&pos);
    let _ = world.put_meta(&key(pos), &[]);
    items
}

/// Step every lit furnace one tick. Returns the positions that changed, so
/// their viewers can be updated.
pub fn tick_furnaces(world: &DemoWorld) -> Vec<Pos> {
    let positions: Vec<Pos> = active().lock().unwrap().iter().copied().collect();
    let mut changed = Vec::new();
    for pos in positions {
        let mut idle = false;
        let mut dirty = false;
        {
            let mut map = all().lock().unwrap();
            let Some(c) = map.get_mut(&pos) else {
                active().lock().unwrap().remove(&pos);
                continue;
            };
            // Fuel burnt, input smelted, output made: journalled as the
            // difference, by the server. Which cooking method applies is the
            // block's own (furnace, smoker or blast furnace); the recipe
            // carries its cooking time and experience.
            let before = c.slots.clone();
            let method = crate::game::block_name(world, world.get_block(pos.0, pos.1, pos.2))
                .map(|n| {
                    super::recipes::method_for_block(
                        n.strip_prefix("minecraft:").unwrap_or(n.as_str()),
                    )
                })
                .unwrap_or(super::recipes::Cook::Smelting);
            let input = c.slots[0].as_ref().map(|s| s.item);
            let recipe = input.and_then(|i| super::recipes::cook(method, i));
            let result = recipe.map(|r| r.result);
            let output_ok = result.is_some_and(|r| match &c.slots[2] {
                None => true,
                Some(o) => o.item == r && o.count < o.max_stack(),
            });
            if c.burn > 0 {
                c.burn -= 1;
                dirty = true;
            }
            if c.burn == 0 && output_ok {
                if let Some(fuel) = &mut c.slots[1] {
                    let t = super::tables::fuel_ticks_id(fuel.item);
                    if t > 0 {
                        c.burn = t;
                        c.burn_max = t;
                        let empty = fuel.item == crate::inventory::item("minecraft:lava_bucket");
                        fuel.count -= 1;
                        if empty {
                            c.slots[1] = Some(Stack::named("minecraft:bucket", 1));
                        } else if fuel.count == 0 {
                            c.slots[1] = None;
                        }
                        dirty = true;
                    }
                }
            }
            if c.burn > 0 && output_ok {
                let done = recipe.map_or(200, |r| r.ticks);
                c.cook += 1;
                dirty = true;
                if c.cook >= done {
                    c.cook = 0;
                    let r = result.expect("output_ok implies a result");
                    let count = recipe.map_or(1, |r| r.count);
                    if let Some(input) = &mut c.slots[0] {
                        input.count -= 1;
                        if input.count == 0 {
                            c.slots[0] = None;
                        }
                    }
                    match &mut c.slots[2] {
                        Some(o) => o.count = o.count.saturating_add(count),
                        None => c.slots[2] = Some(Stack::new(r, count)),
                    }
                    // The smelted item's experience waits in the furnace
                    // until a player takes the output.
                    c.xp += recipe.map_or(0.0, |r| r.xp);
                }
            } else if c.cook > 0 {
                c.cook = c.cook.saturating_sub(2);
                dirty = true;
            }
            if c.burn == 0 && c.cook == 0 {
                idle = true;
            }
            if dirty {
                let _ = world.put_meta(&key(pos), &encode(c));
            }
            super::provenance::container_changed(world, pos, &before, &c.slots);
        }
        if idle {
            active().lock().unwrap().remove(&pos);
        }
        if dirty {
            changed.push(pos);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_round_trip() {
        let mut c = Container::new(ContainerKind::Furnace);
        c.slots[0] = Some(Stack::named("minecraft:raw_iron", 5));
        c.burn = 120;
        c.burn_max = 1600;
        c.cook = 33;
        c.xp = 2.8;
        let back = decode(&encode(&c)).unwrap();
        assert_eq!(back.slots, c.slots);
        assert_eq!((back.burn, back.burn_max, back.cook), (120, 1600, 33));
        assert_eq!(back.xp, 2.8);
    }

    #[test]
    fn a_version_one_container_has_no_stored_experience() {
        // A blob written before smelting experience existed still loads.
        let mut old = vec![1u8, 1u8];
        old.extend_from_slice(&0u32.to_be_bytes());
        old.extend_from_slice(&0u32.to_be_bytes());
        old.extend_from_slice(&0u32.to_be_bytes());
        old.extend_from_slice(&crate::inventory::encode(
            &crate::inventory::Inventory::new(),
        ));
        let back = decode(&old).expect("a version 1 blob decodes");
        assert_eq!(back.xp, 0.0);
    }

    #[test]
    fn the_new_kinds_survive_a_round_trip() {
        for kind in [ContainerKind::Hopper, ContainerKind::Dispenser] {
            let mut c = Container::new(kind);
            c.slots[0] = Some(Stack::named("minecraft:stone", 7));
            let back = decode(&encode(&c)).unwrap();
            assert_eq!(back.kind, kind);
            assert_eq!(back.slots, c.slots);
        }
    }

    #[test]
    fn the_container_kinds_have_the_slot_counts_vanilla_gives_them() {
        assert_eq!(ContainerKind::Chest.slots(), 27);
        assert_eq!(ContainerKind::Furnace.slots(), 3);
        assert_eq!(ContainerKind::Hopper.slots(), 5);
        assert_eq!(ContainerKind::Dispenser.slots(), 9);
        assert_eq!(
            ContainerKind::of_block("minecraft:hopper"),
            Some(ContainerKind::Hopper)
        );
        assert_eq!(
            ContainerKind::of_block("minecraft:dropper"),
            Some(ContainerKind::Dispenser)
        );
        assert_eq!(
            ContainerKind::of_block("minecraft:dispenser"),
            Some(ContainerKind::Dispenser)
        );
    }

    #[test]
    fn a_transfer_moves_one_item_and_merges() {
        let mut src = vec![Some(Stack::named("minecraft:stone", 64)), None, None];
        let mut dst = vec![Some(Stack::named("minecraft:stone", 10)), None];
        assert!(transfer_one(&mut src, &mut dst));
        assert_eq!(src[0].as_ref().map(|s| s.count), Some(63));
        assert_eq!(dst[0].as_ref().map(|s| s.count), Some(11));
    }

    #[test]
    fn a_transfer_uses_an_empty_slot_when_it_cannot_merge() {
        let mut src = vec![Some(Stack::named("minecraft:stone", 1)), None];
        let mut dst = vec![Some(Stack::named("minecraft:dirt", 1)), None];
        assert!(transfer_one(&mut src, &mut dst));
        assert!(src[0].is_none());
        assert_eq!(
            dst[1].as_ref().map(|s| s.item),
            Some(crate::inventory::item("minecraft:stone"))
        );
    }

    #[test]
    fn a_transfer_that_cannot_fit_reports_failure() {
        let mut src = vec![Some(Stack::named("minecraft:stone", 1))];
        let mut dst = vec![Some(Stack::named("minecraft:dirt", 64))];
        assert!(!transfer_one(&mut src, &mut dst));
        assert_eq!(src[0].as_ref().map(|s| s.count), Some(1));
    }

    #[test]
    fn a_transfer_keeps_the_uid_of_a_whole_stack() {
        let mut src = vec![Some(Stack::named("minecraft:diamond_sword", 1))];
        let uid = src[0].as_ref().unwrap().uid;
        let mut dst = vec![None];
        assert!(transfer_one(&mut src, &mut dst));
        assert_eq!(dst[0].as_ref().unwrap().uid, uid);
    }
}
