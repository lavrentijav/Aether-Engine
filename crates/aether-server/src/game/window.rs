//! Inventory windows: clicks computed on the server, every slot sent back.
//!
//! Since 1.21.5 a click carries only hashes of what the client *thinks*
//! changed, which is a hint and not an instruction. The server runs the click
//! itself, against its own inventory, and answers with the whole window — so
//! a client that predicted wrong is corrected on the next frame, and a client
//! that lies gets nothing it did not already have.
//!
//! Layouts follow the protocol's own numbering:
//!
//! | window          | slots                                                    |
//! |-----------------|----------------------------------------------------------|
//! | player (id 0)   | 0 result, 1–4 grid, 5–8 armour, 9–35 main, 36–44 hotbar, 45 offhand |
//! | crafting table  | 0 result, 1–9 grid, 10–36 main, 37–45 hotbar             |
//! | chest           | 0–26 chest, 27–53 main, 54–62 hotbar                     |
//! | furnace         | 0 input, 1 fuel, 2 output, 3–29 main, 30–38 hotbar       |

use super::containers::{self, ContainerKind};
use super::player::{Drag, OpenWindow, PlayerState, WindowKind};
use super::recipes;
use super::tables;
use crate::inventory::{Inventory, Stack};
use crate::players::PlayerHandle;
use crate::protocol::{BlockSource, Menu, ServerEvent};
use crate::session::DemoWorld;

type Pos = (i32, i32, i32);
/// A window's id, its slots, and its `(property, value)` pairs — a furnace's
/// four, a brewing stand's two.
type OpenSlots = (u8, Vec<Option<Stack>>, Vec<(i16, i16)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Player,
    Crafting,
    Chest,
    Furnace,
    Hopper,
    Dispenser,
    BrewingStand,
}

impl Layout {
    /// Whether the layout's container slots sit in `cont` before the player's
    /// own 36 slots.
    fn is_container(self) -> bool {
        matches!(
            self,
            Layout::Chest
                | Layout::Furnace
                | Layout::Hopper
                | Layout::Dispenser
                | Layout::BrewingStand
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ref {
    Inv(usize),
    Grid(usize),
    Cont(usize),
    Result,
}

struct Ctx<'a> {
    layout: Layout,
    inv: &'a mut Inventory,
    cursor: &'a mut Option<Stack>,
    grid: Option<&'a mut Vec<Option<Stack>>>,
    cont: Option<&'a mut Vec<Option<Stack>>>,
    creative: bool,
    drops: Vec<Stack>,
    /// What this click crafted, for the `after-craft` plugin event. Only the
    /// player grid and the crafting table can craft, so a container click
    /// always leaves this empty.
    crafted: Vec<(aether_world::ItemTypeId, u8)>,
}

impl<'a> Ctx<'a> {
    fn len(&self) -> usize {
        if self.layout.is_container() {
            // A chest is 27 slots, or 54 doubled; a furnace 3; a hopper 5;
            // a dispenser 9 — then the player's own 36.
            self.cont.as_ref().map_or(0, |c| c.len()) + 36
        } else {
            46
        }
    }

    /// The first window index of the player's main inventory.
    fn main_start(&self) -> usize {
        match self.layout {
            Layout::Player => 9,
            Layout::Crafting => 10,
            _ => self.cont.as_ref().map_or(0, |c| c.len()),
        }
    }

    fn resolve(&self, slot: i16) -> Option<Ref> {
        let slot = usize::try_from(slot).ok()?;
        if slot >= self.len() {
            return None;
        }
        let main = self.main_start();
        Some(match self.layout {
            Layout::Player => {
                if slot == 0 {
                    Ref::Result
                } else {
                    Ref::Inv(slot)
                }
            }
            Layout::Crafting => match slot {
                0 => Ref::Result,
                1..=9 => Ref::Grid(slot - 1),
                _ => Ref::Inv(slot - main + 9),
            },
            _ => {
                if slot < main {
                    Ref::Cont(slot)
                } else {
                    Ref::Inv(slot - main + 9)
                }
            }
        })
    }

    fn grid_cells(&self) -> (Vec<Option<aether_world::ItemTypeId>>, usize) {
        match self.layout {
            Layout::Player => (
                (1..=4).map(|i| self.inv.slot(i).map(|s| s.item)).collect(),
                2,
            ),
            Layout::Crafting => (
                self.grid
                    .as_ref()
                    .map(|g| g.iter().map(|s| s.as_ref().map(|s| s.item)).collect())
                    .unwrap_or_default(),
                3,
            ),
            _ => (Vec::new(), 0),
        }
    }

    fn craft_result(&self) -> Option<Stack> {
        let (cells, w) = self.grid_cells();
        if w == 0 {
            return None;
        }
        // The recipe book (pack-loaded, with the compiled-in table as the
        // no-pack fallback) matches on item ids, so a modded stack can match
        // a recipe keyed by its own interned id.
        recipes::craft(&cells, w).map(|(item, n)| Stack::new(item, n))
    }

    /// Use up one of every ingredient.
    fn consume_grid(&mut self) {
        let cells: Vec<Ref> = match self.layout {
            Layout::Player => (1..=4).map(Ref::Inv).collect(),
            Layout::Crafting => (0..9).map(Ref::Grid).collect(),
            _ => return,
        };
        for r in cells {
            if let Some(mut s) = self.get(r) {
                let remainder = recipes::remainder(s.item);
                s.count -= 1;
                if s.count == 0 {
                    // The cell empties, so a remainder takes its place.
                    self.put(r, remainder.map(|i| Stack::new(i, 1)));
                } else {
                    self.put(r, Some(s));
                    // Items are left in the cell; a different remainder goes
                    // to the player's inventory, or the ground — vanilla's
                    // `ResultSlot.onTake` never discards it.
                    if let Some(rem) = remainder {
                        if let Some(rest) = self.inv.insert(Stack::new(rem, 1)) {
                            self.drops.push(rest);
                        }
                    }
                }
            }
        }
    }

    fn get(&self, r: Ref) -> Option<Stack> {
        match r {
            Ref::Inv(i) => self.inv.slot(i).cloned(),
            Ref::Grid(i) => self.grid.as_ref().and_then(|g| g.get(i).cloned().flatten()),
            Ref::Cont(i) => self.cont.as_ref().and_then(|c| c.get(i).cloned().flatten()),
            Ref::Result => self.craft_result(),
        }
    }

    fn put(&mut self, r: Ref, s: Option<Stack>) {
        let s = s.filter(|s| s.count > 0);
        match r {
            Ref::Inv(i) => {
                self.inv.put_slot(i, s);
            }
            Ref::Grid(i) => {
                if let Some(g) = self.grid.as_mut() {
                    if let Some(c) = g.get_mut(i) {
                        *c = s;
                    }
                }
            }
            Ref::Cont(i) => {
                if let Some(c) = self.cont.as_mut() {
                    if let Some(c) = c.get_mut(i) {
                        *c = s;
                    }
                }
            }
            Ref::Result => {}
        }
    }

    /// Whether `s` may be put in `r` at all.
    fn accepts(&self, r: Ref, s: &Stack) -> bool {
        match r {
            Ref::Result => false,
            Ref::Inv(i @ 5..=8) => tables::armor_slot_id(s.item) == Some(i),
            Ref::Cont(2) if self.layout == Layout::Furnace => false,
            Ref::Cont(1) if self.layout == Layout::Furnace => {
                tables::fuel_ticks_id(s.item) > 0
                    || s.item == crate::inventory::item("minecraft:bucket")
            }
            Ref::Cont(0..=2) if self.layout == Layout::BrewingStand => s
                .item
                .vanilla_name()
                .is_some_and(super::brewing::is_bottle_item),
            Ref::Cont(3) if self.layout == Layout::BrewingStand => s
                .item
                .vanilla_name()
                .is_some_and(super::brewing::is_ingredient),
            Ref::Cont(4) if self.layout == Layout::BrewingStand => {
                s.item.vanilla_name().is_some_and(super::brewing::is_fuel)
            }
            _ => true,
        }
    }

    /// How many of `s` fit in `r` in total.
    fn cap(&self, r: Ref, s: &Stack) -> u8 {
        match r {
            Ref::Inv(5..=8) => 1,
            Ref::Cont(0..=2) if self.layout == Layout::BrewingStand => 1,
            _ => s.max_stack(),
        }
    }

    /// Put as much of `stack` as fits into `targets`, merging first. Returns
    /// what is left.
    fn move_to(&mut self, mut stack: Stack, targets: &[usize]) -> Option<Stack> {
        for pass in 0..2 {
            for &t in targets {
                if stack.count == 0 {
                    return None;
                }
                let Some(r) = self.resolve(t as i16) else {
                    continue;
                };
                if !self.accepts(r, &stack) {
                    continue;
                }
                let cap = self.cap(r, &stack);
                match self.get(r) {
                    Some(mut have) if pass == 0 && have.stacks_with(&stack) && have.count < cap => {
                        let n = (cap - have.count).min(stack.count);
                        have.count += n;
                        stack.count -= n;
                        self.put(r, Some(have));
                    }
                    None if pass == 1 => {
                        let n = stack.count.min(cap);
                        let part = stack.split(n);
                        self.put(r, Some(part));
                    }
                    _ => {}
                }
            }
        }
        (stack.count > 0).then_some(stack)
    }

    /// Where a shift-click from window slot `slot` sends its stack.
    fn shift_targets(&self, slot: usize, stack: &Stack) -> Vec<usize> {
        let main = self.main_start();
        let hotbar = main + 27;
        let end = main + 36;
        let fwd = |a: usize, b: usize| (a..b).collect::<Vec<_>>();
        let rev = |a: usize, b: usize| (a..b).rev().collect::<Vec<_>>();
        match self.layout {
            Layout::Player => match slot {
                0 => rev(9, 45),
                1..=8 | 45 => fwd(9, 45),
                _ => {
                    if let Some(a) = tables::armor_slot_id(stack.item) {
                        if self.inv.slot(a).is_none() {
                            return vec![a];
                        }
                    }
                    if slot < 36 {
                        fwd(36, 45)
                    } else {
                        fwd(9, 36)
                    }
                }
            },
            Layout::Crafting => match slot {
                0 => rev(main, end),
                1..=9 => fwd(main, end),
                s if s < hotbar => fwd(hotbar, end),
                _ => fwd(main, hotbar),
            },
            Layout::Chest | Layout::Hopper | Layout::Dispenser => {
                if slot < main {
                    rev(main, end)
                } else {
                    fwd(0, main)
                }
            }
            Layout::BrewingStand => {
                if slot < main {
                    // A bottle, the ingredient or the fuel slides straight
                    // into the player's own inventory.
                    rev(main, end)
                } else if stack
                    .item
                    .vanilla_name()
                    .is_some_and(super::brewing::is_fuel)
                {
                    vec![4]
                } else if stack
                    .item
                    .vanilla_name()
                    .is_some_and(super::brewing::is_ingredient)
                {
                    vec![3]
                } else if stack
                    .item
                    .vanilla_name()
                    .is_some_and(super::brewing::is_bottle_item)
                {
                    fwd(0, 3)
                } else if slot < hotbar {
                    fwd(hotbar, end)
                } else {
                    fwd(main, hotbar)
                }
            }
            Layout::Furnace => {
                if slot < main {
                    rev(main, end)
                } else if recipes::cooks(stack.item) {
                    vec![0]
                } else if tables::fuel_ticks_id(stack.item) > 0 {
                    vec![1]
                } else if slot < hotbar {
                    fwd(hotbar, end)
                } else {
                    fwd(main, hotbar)
                }
            }
        }
    }

    fn take_result(&mut self) -> Option<Stack> {
        let r = self.craft_result()?;
        self.consume_grid();
        self.crafted.push((r.item, r.count));
        Some(r)
    }

    fn click(&mut self, slot: i16, button: i8, mode: i32, drag: &mut Option<Drag>) {
        match mode {
            0 => self.pickup(slot, button),
            1 => self.shift(slot),
            2 => self.swap(slot, button),
            3 => {
                if self.creative && self.cursor.is_none() {
                    if let Some(s) = self.resolve(slot).and_then(|r| self.get(r)) {
                        let mut c = s.clone();
                        c.count = c.max_stack();
                        c.uid = aether_world::journal::ledger::mint_uid();
                        *self.cursor = Some(c);
                    }
                }
            }
            4 => self.throw(slot, button),
            5 => self.drag(slot, button, drag),
            6 => self.collect(),
            _ => {}
        }
    }

    fn pickup(&mut self, slot: i16, button: i8) {
        if slot == -999 {
            if let Some(mut c) = self.cursor.take() {
                if button == 1 && c.count > 1 {
                    self.drops.push(c.split(1));
                    *self.cursor = Some(c);
                } else {
                    self.drops.push(c);
                }
            }
            return;
        }
        let Some(r) = self.resolve(slot) else { return };
        if r == Ref::Result {
            let Some(res) = self.craft_result() else {
                return;
            };
            match self.cursor.as_mut() {
                None => {
                    self.consume_grid();
                    self.crafted.push((res.item, res.count));
                    *self.cursor = Some(res);
                }
                Some(c) if c.stacks_with(&res) && c.count + res.count <= c.max_stack() => {
                    c.count += res.count;
                    self.consume_grid();
                    self.crafted.push((res.item, res.count));
                }
                _ => {}
            }
            return;
        }
        let have = self.get(r);
        let cursor = self.cursor.take();
        match (have, cursor) {
            (None, None) => {}
            (Some(mut h), None) => {
                if button == 1 {
                    let half = h.count.div_ceil(2);
                    let taken = h.split(half);
                    self.put(r, Some(h));
                    *self.cursor = Some(taken);
                } else {
                    self.put(r, None);
                    *self.cursor = Some(h);
                }
            }
            (None, Some(mut c)) => {
                if !self.accepts(r, &c) {
                    *self.cursor = Some(c);
                    return;
                }
                let cap = self.cap(r, &c);
                let n = if button == 1 { 1 } else { c.count.min(cap) };
                let part = c.split(n);
                self.put(r, Some(part));
                *self.cursor = (c.count > 0).then_some(c);
            }
            (Some(mut h), Some(mut c)) => {
                if h.stacks_with(&c) && self.accepts(r, &c) {
                    let cap = self.cap(r, &c);
                    let room = cap.saturating_sub(h.count);
                    let n = if button == 1 {
                        1.min(room)
                    } else {
                        room.min(c.count)
                    };
                    h.count += n;
                    c.count -= n;
                    self.put(r, Some(h));
                    *self.cursor = (c.count > 0).then_some(c);
                } else if self.accepts(r, &c) && c.count <= self.cap(r, &c) {
                    self.put(r, Some(c));
                    *self.cursor = Some(h);
                } else {
                    *self.cursor = Some(c);
                }
            }
        }
    }

    fn shift(&mut self, slot: i16) {
        let Some(r) = self.resolve(slot) else { return };
        let idx = slot as usize;
        if r == Ref::Result {
            for _ in 0..64 {
                let Some(res) = self.craft_result() else {
                    break;
                };
                let crafted = (res.item, res.count);
                let targets = self.shift_targets(idx, &res);
                // Only craft what fits whole: try on a copy first.
                let before_inv = self.inv.clone();
                if self.move_to(res, &targets).is_some() {
                    *self.inv = before_inv;
                    break;
                }
                self.consume_grid();
                self.crafted.push(crafted);
            }
            return;
        }
        let Some(s) = self.get(r) else { return };
        let targets = self.shift_targets(idx, &s);
        self.put(r, None);
        let rest = self.move_to(s, &targets);
        self.put(r, rest);
    }

    fn swap(&mut self, slot: i16, button: i8) {
        let Some(r) = self.resolve(slot) else { return };
        let hot = match button {
            0..=8 => Ref::Inv(36 + button as usize),
            40 => Ref::Inv(45),
            _ => return,
        };
        if r == Ref::Result {
            if self.get(hot).is_none() {
                if let Some(res) = self.take_result() {
                    self.put(hot, Some(res));
                }
            }
            return;
        }
        let a = self.get(r);
        let b = self.get(hot);
        if let Some(b) = &b {
            if !self.accepts(r, b) || b.count > self.cap(r, b) {
                return;
            }
        }
        self.put(r, b);
        self.put(hot, a);
    }

    fn throw(&mut self, slot: i16, button: i8) {
        if slot == -999 {
            return;
        }
        let Some(r) = self.resolve(slot) else { return };
        if r == Ref::Result {
            if let Some(res) = self.take_result() {
                self.drops.push(res);
            }
            return;
        }
        let Some(mut s) = self.get(r) else { return };
        if button == 1 {
            self.put(r, None);
            self.drops.push(s);
        } else {
            let one = s.split(1);
            self.put(r, Some(s));
            self.drops.push(one);
        }
    }

    fn drag(&mut self, slot: i16, button: i8, drag: &mut Option<Drag>) {
        match button {
            0 | 4 | 8 => {
                *drag = Some(Drag {
                    kind: (button / 4) as u8,
                    slots: Vec::new(),
                });
            }
            1 | 5 | 9 => {
                if let Some(d) = drag {
                    if !d.slots.contains(&slot) {
                        d.slots.push(slot);
                    }
                }
            }
            2 | 6 | 10 => {
                let Some(d) = drag.take() else { return };
                let Some(mut c) = self.cursor.take() else {
                    return;
                };
                let refs: Vec<Ref> = d
                    .slots
                    .iter()
                    .filter_map(|s| self.resolve(*s))
                    .filter(|r| {
                        *r != Ref::Result
                            && self.accepts(*r, &c)
                            && self.get(*r).map_or(true, |h| h.stacks_with(&c))
                    })
                    .collect();
                if refs.is_empty() {
                    *self.cursor = Some(c);
                    return;
                }
                let per = match d.kind {
                    0 => (c.count as usize / refs.len()).max(1) as u8,
                    1 => 1,
                    _ => {
                        if !self.creative {
                            *self.cursor = Some(c);
                            return;
                        }
                        c.max_stack()
                    }
                };
                for r in refs {
                    if c.count == 0 && d.kind != 2 {
                        break;
                    }
                    let cap = self.cap(r, &c);
                    let mut h = self.get(r).unwrap_or_else(|| {
                        let mut e = c.clone();
                        e.count = 0;
                        e.uid = aether_world::journal::ledger::mint_uid();
                        e
                    });
                    let n = per.min(cap.saturating_sub(h.count));
                    let n = if d.kind == 2 { n } else { n.min(c.count) };
                    h.count += n;
                    if d.kind != 2 {
                        c.count -= n;
                    }
                    self.put(r, Some(h));
                }
                *self.cursor = (c.count > 0).then_some(c);
            }
            _ => {}
        }
    }

    fn collect(&mut self) {
        let Some(mut c) = self.cursor.take() else {
            return;
        };
        let max = c.max_stack();
        for pass in 0..2 {
            for i in 0..self.len() {
                if c.count >= max {
                    break;
                }
                let Some(r) = self.resolve(i as i16) else {
                    continue;
                };
                if r == Ref::Result {
                    continue;
                }
                let Some(mut h) = self.get(r) else { continue };
                if !h.stacks_with(&c) || (pass == 0 && h.count >= h.max_stack()) {
                    continue;
                }
                let n = (max - c.count).min(h.count);
                h.count -= n;
                c.count += n;
                self.put(r, (h.count > 0).then_some(h));
            }
        }
        *self.cursor = Some(c);
    }
}

/// Which container(s) a window is attached to.
enum ContainerTarget {
    /// A single block entity.
    Single(ContainerKind, Pos),
    /// A double chest: its left half, then its right.
    Double(Pos, Pos),
}

fn layout_of(w: &OpenWindow) -> (Layout, Option<ContainerTarget>) {
    match &w.kind {
        WindowKind::Crafting { .. } => (Layout::Crafting, None),
        WindowKind::Chest { left, right } => (
            Layout::Chest,
            Some(match right {
                Some(r) => ContainerTarget::Double(*left, *r),
                None => ContainerTarget::Single(ContainerKind::Chest, *left),
            }),
        ),
        WindowKind::Furnace { pos } => (
            Layout::Furnace,
            Some(ContainerTarget::Single(ContainerKind::Furnace, *pos)),
        ),
        WindowKind::Hopper { pos } => (
            Layout::Hopper,
            Some(ContainerTarget::Single(ContainerKind::Hopper, *pos)),
        ),
        WindowKind::Dispenser { pos } => (
            Layout::Dispenser,
            Some(ContainerTarget::Single(ContainerKind::Dispenser, *pos)),
        ),
        WindowKind::BrewingStand { pos } => (
            Layout::BrewingStand,
            Some(ContainerTarget::Single(ContainerKind::BrewingStand, *pos)),
        ),
    }
}

/// Handle one click. Returns stacks to throw from the player.
pub fn click(
    handle: &PlayerHandle,
    world: &DemoWorld,
    window: u8,
    slot: i16,
    button: i8,
    mode: i32,
) {
    let mut crafted: Vec<(aether_world::ItemTypeId, u8)> = Vec::new();
    let drops = {
        let mut st = handle.game();
        let creative = !st.survival();
        let open = st.window.clone();
        let target = if window == 0 {
            None
        } else {
            match &open {
                Some(w) if w.id == window => Some(w.clone()),
                _ => {
                    drop(st);
                    sync(handle, world);
                    return;
                }
            }
        };
        let PlayerState {
            cursor,
            drag,
            window: open_window,
            ..
        } = &mut *st;
        let mut inv = handle.inventory();
        match target {
            None => {
                let mut ctx = Ctx {
                    layout: Layout::Player,
                    inv: &mut inv,
                    cursor,
                    grid: None,
                    cont: None,
                    creative,
                    drops: Vec::new(),
                    crafted: Vec::new(),
                };
                ctx.click(slot, button, mode, drag);
                crafted.extend(ctx.crafted);
                ctx.drops
            }
            Some(w) => {
                let (layout, container) = layout_of(&w);
                match container {
                    None => {
                        let grid = match open_window.as_mut().map(|w| &mut w.kind) {
                            Some(WindowKind::Crafting { grid }) => grid,
                            _ => return,
                        };
                        let mut ctx = Ctx {
                            layout,
                            inv: &mut inv,
                            cursor,
                            grid: Some(grid),
                            cont: None,
                            creative,
                            drops: Vec::new(),
                            crafted: Vec::new(),
                        };
                        ctx.click(slot, button, mode, drag);
                        crafted.extend(ctx.crafted);
                        ctx.drops
                    }
                    Some(ContainerTarget::Single(kind, pos)) => {
                        let (drops, xp) = containers::with(world, pos, kind, |c| {
                            let furnace = c.kind == ContainerKind::Furnace;
                            let before = if furnace {
                                c.slots
                                    .get(2)
                                    .and_then(|s| s.as_ref())
                                    .map_or(0, |s| s.count)
                            } else {
                                0
                            };
                            let mut ctx = Ctx {
                                layout,
                                inv: &mut inv,
                                cursor,
                                grid: None,
                                cont: Some(&mut c.slots),
                                creative,
                                drops: Vec::new(),
                                crafted: Vec::new(),
                            };
                            ctx.click(slot, button, mode, drag);
                            let drops = ctx.drops;
                            // Taking from a furnace's output releases the
                            // experience its smelting stored, as orbs where
                            // the player stands — vanilla's `onTake`.
                            let xp = if furnace {
                                let after = c
                                    .slots
                                    .get(2)
                                    .and_then(|s| s.as_ref())
                                    .map_or(0, |s| s.count);
                                if after < before {
                                    let whole = c.xp.floor();
                                    c.xp -= whole;
                                    whole as i32
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                            (drops, xp)
                        });
                        if xp > 0 {
                            let p = handle.pos();
                            super::entities::award_xp(aether_api::Vector3::new(p.x, p.y, p.z), xp);
                        }
                        drops
                    }
                    Some(ContainerTarget::Double(a, b)) => {
                        containers::with_pair(world, a, b, |ca, cb| {
                            let mut slots: Vec<Option<Stack>> = ca
                                .slots
                                .iter()
                                .cloned()
                                .chain(cb.slots.iter().cloned())
                                .collect();
                            let drops = {
                                let mut ctx = Ctx {
                                    layout,
                                    inv: &mut inv,
                                    cursor,
                                    grid: None,
                                    cont: Some(&mut slots),
                                    creative,
                                    drops: Vec::new(),
                                    crafted: Vec::new(),
                                };
                                ctx.click(slot, button, mode, drag);
                                ctx.drops
                            };
                            let (ls, rs) = slots.split_at(ca.slots.len());
                            ca.slots = ls.to_vec();
                            cb.slots = rs.to_vec();
                            drops
                        })
                    }
                }
            }
        }
    };
    // Crafting happened: tell the plugins that subscribed to `after-craft`.
    for (item, count) in crafted {
        if let Some(name) = world.item_name(item) {
            crate::plugins::after_craft(handle, &name, u32::from(count));
        }
    }
    for s in drops {
        super::entities::throw_from(handle, s);
    }
    sync(handle, world);
    // The player may have moved an item into or out of a hopper, or a
    // container a hopper pulls from or pushes to.
    for pos in container_positions(handle) {
        super::blockticks::hopper::notify(world, pos.0, pos.1, pos.2);
    }
    if let Some(pos) = viewing_container(handle) {
        refresh_viewers(pos, world, Some(handle.entity_id));
    }
}

/// Every container position the open window is attached to — a double chest
/// contributes both halves.
fn container_positions(handle: &PlayerHandle) -> Vec<(i32, i32, i32)> {
    match handle.game().window.as_ref().map(|w| &w.kind) {
        Some(WindowKind::Chest { left, right }) => {
            let mut v = vec![*left];
            if let Some(r) = right {
                v.push(*r);
            }
            v
        }
        Some(WindowKind::Furnace { pos })
        | Some(WindowKind::Hopper { pos })
        | Some(WindowKind::Dispenser { pos })
        | Some(WindowKind::BrewingStand { pos }) => vec![*pos],
        _ => Vec::new(),
    }
}

/// The container position this player is looking into, if any — for a double
/// chest, its left half, which both halves agree on.
fn viewing_container(handle: &PlayerHandle) -> Option<(i32, i32, i32)> {
    match handle.game().window.as_ref().map(|w| &w.kind) {
        Some(WindowKind::Chest { left, .. }) => Some(*left),
        Some(WindowKind::Furnace { pos })
        | Some(WindowKind::Hopper { pos })
        | Some(WindowKind::Dispenser { pos })
        | Some(WindowKind::BrewingStand { pos }) => Some(*pos),
        _ => None,
    }
}

/// Whether `w` is the window showing the container at `pos`.
fn window_shows(w: &OpenWindow, pos: (i32, i32, i32)) -> bool {
    match &w.kind {
        WindowKind::Chest { left, right } => *left == pos || *right == Some(pos),
        WindowKind::Furnace { pos: p }
        | WindowKind::Hopper { pos: p }
        | WindowKind::Dispenser { pos: p }
        | WindowKind::BrewingStand { pos: p } => *p == pos,
        _ => false,
    }
}

/// Re-send a container to everyone looking into it.
pub fn refresh_viewers(pos: (i32, i32, i32), world: &DemoWorld, except: Option<i32>) {
    let Some(registry) = super::registry() else {
        return;
    };
    for p in registry.snapshot() {
        if Some(p.entity_id) == except {
            continue;
        }
        let shows = p
            .game()
            .window
            .as_ref()
            .is_some_and(|w| window_shows(w, pos));
        if shows {
            sync_open(&p, world);
        }
    }
}

/// The container at `pos` is gone: shut the window of everyone looking in.
pub fn close_viewers(pos: (i32, i32, i32), world: &DemoWorld) {
    let Some(registry) = super::registry() else {
        return;
    };
    for p in registry.snapshot() {
        let shows = p
            .game()
            .window
            .as_ref()
            .is_some_and(|w| window_shows(w, pos));
        if shows {
            let id = {
                let mut st = p.game();
                st.window.take().map(|w| w.id)
            };
            if let Some(id) = id {
                p.emit(&ServerEvent::CloseWindow(id), world);
                sync(&p, world);
            }
        }
    }
}

/// Every slot of the window this player has open, as the server sees it.
fn open_window_slots(st: &PlayerState, inv: &Inventory, world: &DemoWorld) -> Option<OpenSlots> {
    let w = st.window.as_ref()?;
    let mut player_part: Vec<Option<Stack>> = (9..45).map(|i| inv.slot(i).cloned()).collect();
    Some(match &w.kind {
        WindowKind::Crafting { grid } => {
            let cells: Vec<Option<aether_world::ItemTypeId>> =
                grid.iter().map(|s| s.as_ref().map(|s| s.item)).collect();
            let res = recipes::craft(&cells, 3).map(|(i, n)| Stack::new(i, n));
            let mut v = vec![res];
            v.extend(grid.iter().cloned());
            v.append(&mut player_part);
            (w.id, v, Vec::new())
        }
        WindowKind::Chest { left, right } => {
            let mut v = match right {
                Some(r) => {
                    let (a, b) = containers::snapshot_pair(world, *left, *r);
                    let mut v = a.slots;
                    v.extend(b.slots);
                    v
                }
                None => containers::snapshot(world, *left, ContainerKind::Chest).slots,
            };
            v.append(&mut player_part);
            (w.id, v, Vec::new())
        }
        WindowKind::Furnace { pos } => {
            let c = containers::snapshot(world, *pos, ContainerKind::Furnace);
            // The progress bar's denominator is the recipe's own cooking time
            // (200 for a furnace, 100 for a smoker or blast furnace).
            let method = recipes::method_for_state(world.get_block(pos.0, pos.1, pos.2));
            let total = c
                .slots
                .first()
                .and_then(|s| s.as_ref())
                .and_then(|s| recipes::cook(method, s.item))
                .map(|c| c.ticks as i16)
                .unwrap_or(200);
            let props = vec![
                (0, c.burn as i16),
                (1, c.burn_max as i16),
                (2, c.cook as i16),
                (3, total),
            ];
            let mut v = c.slots;
            v.append(&mut player_part);
            (w.id, v, props)
        }
        WindowKind::Hopper { pos } => {
            let mut v = containers::snapshot(world, *pos, ContainerKind::Hopper).slots;
            v.append(&mut player_part);
            (w.id, v, Vec::new())
        }
        WindowKind::Dispenser { pos } => {
            let mut v = containers::snapshot(world, *pos, ContainerKind::Dispenser).slots;
            v.append(&mut player_part);
            (w.id, v, Vec::new())
        }
        WindowKind::BrewingStand { pos } => {
            let c = containers::snapshot(world, *pos, ContainerKind::BrewingStand);
            // Property 0 is the brew time out of 400, property 1 the fuel
            // out of 20, vanilla's `BrewingStandBlockEntity` data slots.
            let props = vec![(0, c.cook as i16), (1, c.burn as i16)];
            let mut v = c.slots;
            v.append(&mut player_part);
            (w.id, v, props)
        }
    })
}

fn player_window_slots(inv: &Inventory) -> Vec<Option<Stack>> {
    let cells: Vec<Option<aether_world::ItemTypeId>> =
        (1..=4).map(|i| inv.slot(i).map(|s| s.item)).collect();
    let res = recipes::craft(&cells, 2).map(|(i, n)| Stack::new(i, n));
    let mut v = vec![res];
    v.extend((1..46).map(|i| inv.slot(i).cloned()));
    v
}

/// Send the player's own window, and the open one if there is one.
pub fn sync(handle: &PlayerHandle, world: &dyn BlockSource) {
    let (slots, cursor, state) = {
        let mut st = handle.game();
        let inv = handle.inventory();
        let state = st.next_state();
        (player_window_slots(&inv), st.cursor.clone(), state)
    };
    handle.emit(
        &ServerEvent::WindowContents {
            window_id: 0,
            state_id: state,
            slots,
            cursor,
        },
        world,
    );
    if let Some(w) = world_ref() {
        sync_open(handle, w);
    }
}

fn world_ref() -> Option<&'static DemoWorld> {
    super::world()
}

/// Send only the open game window.
pub fn sync_open(handle: &PlayerHandle, world: &DemoWorld) {
    let msg = {
        let mut st = handle.game();
        let inv = handle.inventory();
        let Some((id, slots, props)) = open_window_slots(&st, &inv, world) else {
            return;
        };
        let state = st.next_state();
        (id, slots, props, st.cursor.clone(), state)
    };
    let (id, slots, props, cursor, state) = msg;
    handle.emit(
        &ServerEvent::WindowContents {
            window_id: id,
            state_id: state,
            slots,
            cursor,
        },
        world,
    );
    for (property, value) in props {
        handle.emit(
            &ServerEvent::WindowProperty {
                window_id: id,
                property,
                value,
            },
            world,
        );
    }
}

/// Open a game window for the block at `pos`.
pub fn open(handle: &PlayerHandle, world: &DemoWorld, kind: WindowKind) {
    let (id, menu, title) = {
        let mut st = handle.game();
        let id = st.next_window_id();
        let (menu, title) = match &kind {
            WindowKind::Crafting { .. } => (Menu::Crafting, "Crafting"),
            WindowKind::Chest { right, .. } if right.is_some() => (Menu::LargeChest, "Large Chest"),
            WindowKind::Chest { .. } => (Menu::Chest, "Chest"),
            WindowKind::Furnace { .. } => (Menu::Furnace, "Furnace"),
            WindowKind::Hopper { .. } => (Menu::Hopper, "Hopper"),
            WindowKind::Dispenser { .. } => (Menu::Dispenser, "Dispenser"),
            WindowKind::BrewingStand { .. } => (Menu::BrewingStand, "Brewing Stand"),
        };
        st.window = Some(OpenWindow { id, kind });
        (id, menu, title)
    };
    handle.emit(
        &ServerEvent::OpenWindow {
            window_id: id,
            menu,
            title: title.into(),
        },
        world,
    );
    sync_open(handle, world);
}

/// The client closed whatever it had open: hand back the crafting grid and
/// the cursor.
pub fn close(handle: &PlayerHandle, world: &DemoWorld) {
    let leftovers = {
        let mut st = handle.game();
        let mut back: Vec<Stack> = Vec::new();
        if let Some(OpenWindow {
            kind: WindowKind::Crafting { grid },
            ..
        }) = st.window.take()
        {
            back.extend(grid.into_iter().flatten());
        }
        if let Some(c) = st.cursor.take() {
            back.push(c);
        }
        st.drag = None;
        let mut inv = handle.inventory();
        for i in 1..=4 {
            if let Some(s) = inv.take_slot(i) {
                back.push(s);
            }
        }
        back.into_iter()
            .filter_map(|s| inv.insert(s))
            .collect::<Vec<_>>()
    };
    for s in leftovers {
        super::entities::throw_from(handle, s);
    }
    sync(handle, world);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(inv: &'a mut Inventory, cursor: &'a mut Option<Stack>) -> Ctx<'a> {
        Ctx {
            layout: Layout::Player,
            inv,
            cursor,
            grid: None,
            cont: None,
            creative: false,
            drops: Vec::new(),
            crafted: Vec::new(),
        }
    }

    #[test]
    fn left_click_picks_up_and_puts_down() {
        let mut inv = Inventory::new();
        inv.put_slot(36, Some(Stack::named("minecraft:dirt", 10)));
        let mut cur = None;
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(36, 0, 0, &mut drag);
        c.click(9, 0, 0, &mut drag);
        assert!(c.cursor.is_none());
        assert_eq!(c.inv.slot(9).map(|s| s.count), Some(10));
        assert!(c.inv.slot(36).is_none());
    }

    #[test]
    fn right_click_splits_and_places_one() {
        let mut inv = Inventory::new();
        inv.put_slot(36, Some(Stack::named("minecraft:dirt", 9)));
        let mut cur = None;
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(36, 1, 0, &mut drag);
        assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(5));
        assert_eq!(c.inv.slot(36).map(|s| s.count), Some(4));
        c.click(10, 1, 0, &mut drag);
        assert_eq!(c.inv.slot(10).map(|s| s.count), Some(1));
        assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(4));
    }

    #[test]
    fn crafting_in_the_inventory_grid() {
        let mut inv = Inventory::new();
        inv.put_slot(1, Some(Stack::named("minecraft:oak_log", 2)));
        let mut cur = None;
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        assert_eq!(
            c.craft_result().map(|s| s.item),
            Some(crate::inventory::item("minecraft:oak_planks"))
        );
        c.click(0, 0, 0, &mut drag);
        assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(4));
        assert_eq!(c.inv.slot(1).map(|s| s.count), Some(1));
        // Shift-click crafts the rest straight into the inventory.
        *c.cursor = None;
        c.click(0, 0, 1, &mut drag);
        assert!(c.inv.slot(1).is_none());
        assert_eq!(
            c.inv
                .count_of(crate::inventory::item("minecraft:oak_planks")),
            4
        );
    }

    #[test]
    fn consuming_a_bucket_leaves_an_empty_bucket() {
        let mut inv = Inventory::new();
        inv.put_slot(1, Some(Stack::named("minecraft:milk_bucket", 1)));
        let mut cur = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.consume_grid();
        assert_eq!(
            c.inv.slot(1).map(|s| s.item),
            Some(crate::inventory::item("minecraft:bucket")),
            "a consumed milk bucket leaves a bucket"
        );
    }

    #[test]
    fn a_remainder_is_not_lost_when_the_ingredient_stacks() {
        // 16 honey bottles in one cell: one is consumed and 15 remain, so the
        // glass bottle goes to the inventory rather than into the cell.
        let mut inv = Inventory::new();
        inv.put_slot(1, Some(Stack::named("minecraft:honey_bottle", 16)));
        let mut cur = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.consume_grid();
        assert_eq!(
            c.inv.slot(1).map(|s| (s.item, s.count)),
            Some((crate::inventory::item("minecraft:honey_bottle"), 15)),
        );
        assert_eq!(
            c.inv
                .count_of(crate::inventory::item("minecraft:glass_bottle")),
            1,
            "the glass bottle is not lost"
        );
    }

    #[test]
    fn armour_slots_take_only_matching_armour() {
        let mut inv = Inventory::new();
        let mut cur = Some(Stack::named("minecraft:dirt", 1));
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(5, 0, 0, &mut drag);
        assert!(c.inv.slot(5).is_none(), "dirt is not a helmet");
        *c.cursor = Some(Stack::named("minecraft:iron_helmet", 1));
        c.click(5, 0, 0, &mut drag);
        assert_eq!(
            c.inv.slot(5).map(|s| s.item),
            Some(crate::inventory::item("minecraft:iron_helmet"))
        );
    }

    #[test]
    fn shift_click_moves_between_hotbar_and_main() {
        let mut inv = Inventory::new();
        inv.put_slot(36, Some(Stack::named("minecraft:cobblestone", 64)));
        let mut cur = None;
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(36, 0, 1, &mut drag);
        assert!(c.inv.slot(36).is_none());
        assert_eq!(c.inv.slot(9).map(|s| s.count), Some(64));
    }

    #[test]
    fn dragging_splits_evenly() {
        let mut inv = Inventory::new();
        let mut cur = Some(Stack::named("minecraft:dirt", 10));
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(-999, 0, 5, &mut drag);
        for s in [9, 10, 11] {
            c.click(s, 1, 5, &mut drag);
        }
        c.click(-999, 2, 5, &mut drag);
        for s in [9, 10, 11] {
            assert_eq!(c.inv.slot(s).map(|s| s.count), Some(3));
        }
        assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(1));
    }

    #[test]
    fn clicking_outside_drops_the_cursor() {
        let mut inv = Inventory::new();
        let mut cur = Some(Stack::named("minecraft:dirt", 10));
        let mut drag = None;
        let mut c = ctx(&mut inv, &mut cur);
        c.click(-999, 1, 0, &mut drag);
        assert_eq!(c.drops.len(), 1);
        assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(9));
        c.click(-999, 0, 0, &mut drag);
        assert!(c.cursor.is_none());
        assert_eq!(c.drops.iter().map(|s| s.count as u32).sum::<u32>(), 10);
    }

    #[test]
    fn a_double_chest_shows_fifty_four_slots() {
        // A doubled chest is 54 slots, the player's 36 behind it.
        let mut inv = Inventory::new();
        let mut cur = None;
        let mut cont: Vec<Option<Stack>> = vec![None; 54];
        let c = Ctx {
            layout: Layout::Chest,
            inv: &mut inv,
            cursor: &mut cur,
            grid: None,
            cont: Some(&mut cont),
            creative: false,
            drops: Vec::new(),
            crafted: Vec::new(),
        };
        assert_eq!(c.len(), 90);
        assert_eq!(c.main_start(), 54);
        // Slot 30 lives in the right half, and still maps past the player
        // inventory.
        assert_eq!(c.resolve(30), Some(Ref::Cont(30)));
        assert_eq!(c.resolve(54), Some(Ref::Inv(9)));
    }

    #[test]
    fn a_double_chest_moves_items_from_its_second_half() {
        let mut inv = Inventory::new();
        let mut cur = None;
        let mut cont: Vec<Option<Stack>> = vec![None; 54];
        cont[30] = Some(Stack::named("minecraft:diamond", 3));
        let mut drag = None;
        {
            let mut c = Ctx {
                layout: Layout::Chest,
                inv: &mut inv,
                cursor: &mut cur,
                grid: None,
                cont: Some(&mut cont),
                creative: false,
                drops: Vec::new(),
                crafted: Vec::new(),
            };
            // Shift-click the right half straight into the inventory.
            c.click(30, 0, 1, &mut drag);
            assert!(c.cont.as_ref().unwrap()[30].is_none());
            assert_eq!(
                c.inv.count_of(crate::inventory::item("minecraft:diamond")),
                3
            );
        }
        cont[40] = Some(Stack::named("minecraft:gold_ingot", 5));
        {
            let mut c = Ctx {
                layout: Layout::Chest,
                inv: &mut inv,
                cursor: &mut cur,
                grid: None,
                cont: Some(&mut cont),
                creative: false,
                drops: Vec::new(),
                crafted: Vec::new(),
            };
            // Left-click a right-half slot picks it up on the cursor.
            c.click(40, 0, 0, &mut drag);
            assert_eq!(c.cursor.as_ref().map(|s| s.count), Some(5));
            assert!(c.cont.as_ref().unwrap()[40].is_none());
        }
    }
}
