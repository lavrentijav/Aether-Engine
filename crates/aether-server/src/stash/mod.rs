//! The recovered-blocks container: what a rollback hands back, and the chest
//! window it is handed back through.
//!
//! [`model`] holds the layout and the grouping rules and knows nothing about
//! packets. This module owns the per-player state and drives the window.
//!
//! # The two windows
//!
//! Opening `/stash` shows a 9x6 container framed in grey glass, with the
//! recovered blocks in the 7x4 interior and page arrows on the bottom row.
//! Clicking an entry does *not* pick it up — it opens a second window asking
//! how many, defaulting to one stack, with a cancel and a confirm. Taking a
//! thousand cobblestone by accident and then having to put it back is the
//! failure mode that dialogue exists to prevent.
//!
//! # Versions
//!
//! The window is driven through [`crate::protocol::ServerEvent::OpenContainer`]
//! and friends, which a codec that has no container support encodes as no
//! packets. On those versions `/stash` falls back to a chat listing plus
//! `/stash take <n> [count]` — the same model, a plainer surface.

pub mod model;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use aether_world::journal::Restore;

use crate::commands::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;
use model::{Cell, Stash};

/// Every player's stash, keyed by UUID.
///
/// Deliberately global and deliberately not persisted yet: a stash is the
/// short-lived result of a rollback someone just ran, and the authoritative
/// record of what was rolled back is the journal, which *is* persisted. If a
/// stash is lost to a restart the blocks are not gone — the rollback can be
/// undone and re-run. Persisting it is the obvious next step and is noted in
/// STATUS.
fn stashes() -> &'static Mutex<HashMap<u128, Stash>> {
    static S: OnceLock<Mutex<HashMap<u128, Stash>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Which page of the stash a player is looking at, and which entry they are
/// being asked a quantity for.
#[derive(Default, Clone, Copy)]
pub struct View {
    pub page: usize,
    /// Set while the "how many?" dialogue is open.
    pub asking: Option<Ask>,
}

/// The open quantity dialogue: which entry, and how many of it are currently
/// chosen.
#[derive(Clone, Copy)]
pub struct Ask {
    pub index: usize,
    pub amount: u64,
}

fn views() -> &'static Mutex<HashMap<u128, View>> {
    static V: OnceLock<Mutex<HashMap<u128, View>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Split a stored block name into its name and its state tags.
///
/// The registry interns whatever name a client sent, and a client sends a
/// block state as `minecraft:oak_stairs[facing=north,half=top]`. Splitting here
/// rather than at deposit time keeps the journal storing exactly what was
/// placed.
pub fn split_tags(full: &str) -> (String, Vec<String>) {
    match full.split_once('[') {
        Some((name, rest)) => (
            name.to_owned(),
            rest.trim_end_matches(']')
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (full.to_owned(), Vec::new()),
    }
}

/// Credit `plan`'s recovered blocks to this player's stash.
///
/// Returns the number of distinct entries now held. Uses each restoration's
/// `removed` state — what the undo took away — never the world's current
/// content; see [`model`] for why that distinction is the whole point.
pub fn deposit(handle: &PlayerHandle, world: &DemoWorld, plan: &[Restore]) -> usize {
    let recovered = plan.iter().filter_map(|r| {
        let name = world.block_name_of(r.removed)?;
        let (block, tags) = split_tags(&name);
        (block != "minecraft:air").then_some((block, tags))
    });
    let mut all = stashes().lock().unwrap();
    let stash = all.entry(handle.uuid).or_default();
    stash.deposit_all(
        model::Stash::from_recovered(recovered)
            .entries()
            .iter()
            .map(|e| (e.block.clone(), e.tags.clone(), e.count)),
    );
    stash.len()
}

/// `/stash` — open the window, or list it in chat on a version without
/// container support.
pub fn open_command(handle: &PlayerHandle, world: &DemoWorld) -> Reply {
    let all = stashes().lock().unwrap();
    let Some(stash) = all.get(&handle.uuid) else {
        return Reply(vec![
            "your stash is empty — nothing has been rolled back".into()
        ]);
    };
    if stash.is_empty() {
        return Reply(vec!["your stash is empty".into()]);
    }
    views().lock().unwrap().insert(handle.uuid, View::default());
    if handle.codec.supports_containers() {
        drop(all);
        refresh(handle, world);
        return Reply(vec![]);
    }
    // Text fallback.
    let mut lines = vec![format!(
        "Stash — {} type(s), {} block(s). /stash take <n> [count]",
        stash.len(),
        stash.total()
    )];
    for (i, e) in stash.entries().iter().enumerate().take(20) {
        lines.push(format!("  [{i}] {}", e.describe()));
    }
    Reply(lines)
}

/// Push the current page of the stash into the player's open window.
pub fn refresh(handle: &PlayerHandle, world: &DemoWorld) {
    let all = stashes().lock().unwrap();
    let Some(stash) = all.get(&handle.uuid) else {
        return;
    };
    let view = views()
        .lock()
        .unwrap()
        .get(&handle.uuid)
        .copied()
        .unwrap_or_default();

    let (title, slots) = match view.asking {
        Some(ask) => ask_window(stash, ask),
        None => browse_window(stash, view.page),
    };
    handle.emit(
        &crate::protocol::ServerEvent::OpenContainer { title, slots },
        world,
    );
}

/// The browsing window: recovered blocks, framed, with page arrows.
fn browse_window(stash: &Stash, page: usize) -> (String, Vec<crate::protocol::ContainerSlot>) {
    let slots = model::layout(stash, page)
        .iter()
        .map(|c| match c {
            Cell::Filler => crate::protocol::ContainerSlot::filler(),
            Cell::PrevPage => crate::protocol::ContainerSlot::arrow(false),
            Cell::NextPage => crate::protocol::ContainerSlot::arrow(true),
            Cell::Item { index } => {
                let e = &stash.entries()[*index];
                crate::protocol::ContainerSlot::block(&e.block, e.count, &e.short_label())
                    .with_quantity()
            }
        })
        .collect();
    (
        format!(
            "Recovered — {} block(s), page {} of {}",
            stash.total(),
            page + 1,
            model::pages(stash)
        ),
        slots,
    )
}

/// The quantity dialogue: eight adjust buttons around the item, then cancel
/// and confirm.
fn ask_window(stash: &Stash, ask: Ask) -> (String, Vec<crate::protocol::ContainerSlot>) {
    use crate::protocol::ContainerSlot;
    let Some(entry) = stash.entries().get(ask.index) else {
        return browse_window(stash, 0);
    };
    let slots = model::ask_layout(ask.amount, entry.count)
        .iter()
        .map(|c| match c {
            model::AskCell::Filler => ContainerSlot::filler(),
            model::AskCell::Adjust(d) => ContainerSlot::button(
                if *d > 0 {
                    "minecraft:lime_stained_glass_pane"
                } else {
                    "minecraft:red_stained_glass_pane"
                },
                // The button shows its own step as a count, so the four sizes
                // are distinguishable at a glance without reading the label.
                d.unsigned_abs(),
                &format!("{d:+}"),
            ),
            model::AskCell::Amount => ContainerSlot::block(
                &entry.block,
                ask.amount,
                &format!("Take {} of {}", ask.amount, entry.count),
            )
            .with_quantity(),
            model::AskCell::Cancel => ContainerSlot::button("minecraft:barrier", 1, "Cancel"),
            model::AskCell::Confirm => ContainerSlot::button(
                "minecraft:lime_concrete",
                1,
                &format!("Take {}", ask.amount),
            ),
        })
        .collect();
    (
        format!(
            "{} — take {} of {}",
            short_name(&entry.block),
            ask.amount,
            entry.count
        ),
        slots,
    )
}

/// `minecraft:oak_stairs` -> `oak_stairs`. Window titles are narrow.
fn short_name(block: &str) -> &str {
    block.split_once(':').map_or(block, |(_, n)| n)
}

/// A click landed in the stash window. Returns lines to send back, if any.
pub fn on_click(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    slot: usize,
) -> Vec<String> {
    let mut view = views()
        .lock()
        .unwrap()
        .get(&handle.uuid)
        .copied()
        .unwrap_or_default();

    // The quantity dialogue is a second window over the same container.
    if let Some(mut ask) = view.asking {
        let available = stashes()
            .lock()
            .unwrap()
            .get(&handle.uuid)
            .and_then(|s| s.entries().get(ask.index).map(|e| e.count))
            .unwrap_or(0);
        let cells = model::ask_layout(ask.amount, available);
        let out = match cells.get(slot) {
            Some(model::AskCell::Adjust(d)) => {
                ask.amount = model::adjust(ask.amount, *d, available);
                view.asking = Some(ask);
                Vec::new()
            }
            Some(model::AskCell::Cancel) => {
                view.asking = None;
                vec!["cancelled".to_string()]
            }
            Some(model::AskCell::Confirm) => {
                let name = stashes()
                    .lock()
                    .unwrap()
                    .get(&handle.uuid)
                    .and_then(|s| s.entries().get(ask.index).map(|e| e.block.clone()))
                    .unwrap_or_default();
                let (taken, dropped) =
                    take_named(handle, registry, world, ask.index, ask.amount, &name);
                view.asking = None;
                let mut out = vec![format!("took {taken}x {name}")];
                if dropped > 0 {
                    out.push(format!("{dropped} would not fit and fell at your feet"));
                }
                out
            }
            _ => Vec::new(),
        };
        views().lock().unwrap().insert(handle.uuid, view);
        refresh(handle, world);
        return out;
    }

    let all = stashes().lock().unwrap();
    let Some(stash) = all.get(&handle.uuid) else {
        return Vec::new();
    };
    let cells = model::layout(stash, view.page);
    let out = match cells.get(slot) {
        Some(Cell::PrevPage) => {
            view.page = view.page.saturating_sub(1);
            Vec::new()
        }
        Some(Cell::NextPage) => {
            view.page = (view.page + 1).min(model::pages(stash) - 1);
            Vec::new()
        }
        Some(Cell::Item { index }) => {
            let entry = &stash.entries()[*index];
            view.asking = Some(Ask {
                index: *index,
                amount: model::default_take(entry.count),
            });
            Vec::new()
        }
        _ => Vec::new(),
    };
    drop(all);
    views().lock().unwrap().insert(handle.uuid, view);
    refresh(handle, world);
    out
}

/// Take `count` of entry `index` and hand it over.
///
/// Removes from the stash first, then gives. The amount is split into stacks
/// of 64 on the way into the inventory and whatever will not fit lands at the
/// player's feet — which is why any amount up to the whole entry is a legal
/// thing to ask for, however far past a stack it is.
///
/// `name` is read by the caller before the removal, because after it the entry
/// may be gone. Returns `(taken, dropped)`.
pub fn take_named(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    index: usize,
    count: u64,
    name: &str,
) -> (u64, u64) {
    let taken = {
        let mut all = stashes().lock().unwrap();
        let Some(stash) = all.get_mut(&handle.uuid) else {
            return (0, 0);
        };
        stash.take(index, count)
    };
    if taken == 0 {
        return (0, 0);
    }
    // The stash holds *block* names; a player carries the item that places
    // them. They agree for almost everything, and where they do not the block
    // has no item at all — so the transfer is still recorded as taken, and the
    // ground layer simply has nothing to show.
    let (_, dropped) = crate::ground::give_or_drop(handle, registry, world, name, taken);
    (taken, dropped)
}

/// Drop a player's stash when they disconnect.
pub fn forget(uuid: u128) {
    stashes().lock().unwrap().remove(&uuid);
    views().lock().unwrap().remove(&uuid);
}

#[cfg(test)]
mod tests {
    use super::split_tags;

    #[test]
    fn a_plain_block_name_has_no_tags() {
        assert_eq!(
            split_tags("minecraft:stone"),
            ("minecraft:stone".to_string(), vec![])
        );
    }

    #[test]
    fn a_block_state_splits_into_a_name_and_its_properties() {
        assert_eq!(
            split_tags("minecraft:oak_stairs[facing=north,half=top]"),
            (
                "minecraft:oak_stairs".to_string(),
                vec!["facing=north".to_string(), "half=top".to_string()]
            )
        );
    }

    #[test]
    fn an_empty_property_list_is_not_a_tag() {
        assert_eq!(
            split_tags("minecraft:stone[]"),
            ("minecraft:stone".to_string(), vec![])
        );
    }
}
