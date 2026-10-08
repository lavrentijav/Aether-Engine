//! Block entities that hold items: chests and furnaces.
//!
//! Kept in memory once touched and written through to the world's KV store
//! on every change, under their own key prefix, so they survive a restart the
//! same way inventories do. Furnaces also tick: the ones with fuel burning or
//! something to cook are kept in an active set the game tick walks.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::inventory::Stack;
use crate::session::DemoWorld;

/// What kind of container a block is, by block name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Chest,
    Furnace,
}

impl ContainerKind {
    /// The container a block of this name is, if any.
    pub fn of_block(name: &str) -> Option<Self> {
        let short = name.strip_prefix("minecraft:").unwrap_or(name);
        match short {
            "chest" | "trapped_chest" | "barrel" => Some(Self::Chest),
            "furnace" | "smoker" | "blast_furnace" => Some(Self::Furnace),
            _ => None,
        }
    }

    pub fn slots(self) -> usize {
        match self {
            Self::Chest => 27,
            Self::Furnace => 3,
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
}

impl Container {
    pub fn new(kind: ContainerKind) -> Self {
        Self {
            kind,
            slots: vec![None; kind.slots()],
            burn: 0,
            burn_max: 0,
            cook: 0,
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

fn encode(c: &Container) -> Vec<u8> {
    let mut out = vec![1u8, matches!(c.kind, ContainerKind::Furnace) as u8];
    out.extend_from_slice(&c.burn.to_be_bytes());
    out.extend_from_slice(&c.burn_max.to_be_bytes());
    out.extend_from_slice(&c.cook.to_be_bytes());
    let mut inv = crate::inventory::Inventory::new();
    for (i, s) in c.slots.iter().enumerate() {
        inv.put_slot(i, s.clone());
    }
    out.extend_from_slice(&crate::inventory::encode(&inv));
    out
}

fn decode(b: &[u8]) -> Option<Container> {
    if b.len() < 14 || b[0] != 1 {
        return None;
    }
    let kind = if b[1] == 1 {
        ContainerKind::Furnace
    } else {
        ContainerKind::Chest
    };
    let u = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
    let inv = crate::inventory::decode(&b[14..])?;
    let mut c = Container::new(kind);
    c.burn = u(2);
    c.burn_max = u(6);
    c.cook = u(10);
    for i in 0..c.slots.len() {
        c.slots[i] = inv.slot(i).cloned();
    }
    Some(c)
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
    let c = map.entry(pos).or_insert_with(|| {
        world
            .get_meta(&key(pos))
            .ok()
            .flatten()
            .and_then(|b| decode(&b))
            .filter(|c| c.kind == kind)
            .unwrap_or_else(|| Container::new(kind))
    });
    let r = f(c);
    let _ = world.put_meta(&key(pos), &encode(c));
    if c.kind == ContainerKind::Furnace {
        active().lock().unwrap().insert(pos);
    }
    r
}

/// A copy of the container at `pos`, if one exists there.
pub fn snapshot(world: &DemoWorld, pos: Pos, kind: ContainerKind) -> Container {
    with(world, pos, kind, |c| c.clone())
}

/// The block at `pos` is gone: forget its container and return the items.
pub fn remove(world: &DemoWorld, pos: Pos, kind: ContainerKind) -> Vec<Stack> {
    let items = with(world, pos, kind, |c| {
        c.slots.iter_mut().filter_map(Option::take).collect()
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
            let result = c.slots[0]
                .as_ref()
                .and_then(|s| super::tables::smelt(&s.item));
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
                    let t = super::tables::fuel_ticks(&fuel.item);
                    if t > 0 {
                        c.burn = t;
                        c.burn_max = t;
                        let empty = fuel.item == "minecraft:lava_bucket";
                        fuel.count -= 1;
                        if empty {
                            c.slots[1] = Some(Stack::new("minecraft:bucket", 1));
                        } else if fuel.count == 0 {
                            c.slots[1] = None;
                        }
                        dirty = true;
                    }
                }
            }
            if c.burn > 0 && output_ok {
                c.cook += 1;
                dirty = true;
                if c.cook >= 200 {
                    c.cook = 0;
                    let r = result.expect("output_ok implies a result");
                    if let Some(input) = &mut c.slots[0] {
                        input.count -= 1;
                        if input.count == 0 {
                            c.slots[0] = None;
                        }
                    }
                    match &mut c.slots[2] {
                        Some(o) => o.count += 1,
                        None => c.slots[2] = Some(Stack::new(r, 1)),
                    }
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
        c.slots[0] = Some(Stack::new("minecraft:raw_iron", 5));
        c.burn = 120;
        c.burn_max = 1600;
        c.cook = 33;
        let back = decode(&encode(&c)).unwrap();
        assert_eq!(back.slots, c.slots);
        assert_eq!((back.burn, back.burn_max, back.cook), (120, 1600, 33));
    }
}
