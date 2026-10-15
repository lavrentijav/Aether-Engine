//! Read-only, paged queries over the journal — the History API (PA2).
//!
//! The journal already stores everything anyone did; this module is the reading
//! side an operator tool, a plugin or a web admin asks questions with. Six
//! questions, all read-only:
//!
//! * **who changed this block, and when** — [`Journal::block_history`], the
//!   full per-block timeline, newest first;
//! * **is this block worldgen-original** — [`Journal::is_worldgen_original`],
//!   true when nobody has ever touched it;
//! * **what happened in this area, at this time** — [`Journal::history_page`]
//!   over a [`Window`] of actor/area/time/kind;
//! * **where did this item come from** — [`Journal::item_chain`], the
//!   provenance graph walked backwards through derives and merges;
//! * **who accessed this container** — [`Journal::container_log`];
//! * **what has this player been doing** — [`Journal::player_events`].
//!
//! # Paging and cost
//!
//! Every query returns one [`Page`] at a time and carries a cursor (`next`)
//! for the page below it, so a caller never materialises a history it does not
//! read. A query that names a block or a container walks only that column's
//! index, never the whole journal: the per-column index the writer already
//! keeps is what makes "one block" cheap. A query that names an actor walks
//! that actor's index; only a fully unrestricted time/area sweep falls back to
//! walking sequence numbers, and even then it stops the moment `limit` matches
//! are found and only ever looks at the newest events first.
//!
//! # Determinism
//!
//! Every page is newest-first and the cursor is a sequence number, so two
//! readers paging the same query see the same order regardless of wall clock,
//! and an append landed between two pages appears as a newer page, not as a
//! reordering of an old one.

use std::collections::HashSet;

use super::{column_prefix, item_prefix, ActorId, Event, ItemUid, Journal};
use crate::storage::{KvBackend, StorageError};

/// One page of events, **newest first**.
///
/// `next` is the sequence number to pass as the `before` cursor of the next
/// call; `None` means the query is exhausted. A page with fewer than `limit`
/// events and `next == None` is the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The matching events, newest first.
    pub events: Vec<Event>,
    /// The cursor for the next (older) page, or `None` when there is none.
    pub next: Option<u64>,
}

impl Page {
    /// An empty, exhausted page.
    pub fn empty() -> Page {
        Page {
            events: Vec::new(),
            next: None,
        }
    }

    /// How many events this page holds.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the page holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// A spatial restriction: a box or a horizontal radius, matching the journal's
/// own filters so an operator's `radius:16` and a web map's box mean the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// An inclusive axis-aligned box.
    Box {
        x1: i32,
        y1: i32,
        z1: i32,
        x2: i32,
        y2: i32,
        z2: i32,
    },
    /// Everything within `r` blocks horizontally (X/Z) of `(x, y, z)`,
    /// regardless of height — "clear this area" reaches from bedrock to sky.
    Radius { x: i32, y: i32, z: i32, r: u32 },
}

impl Area {
    /// The box spanned by two corners, in any order.
    pub fn between(a: (i32, i32, i32), b: (i32, i32, i32)) -> Area {
        Area::Box {
            x1: a.0.min(b.0),
            y1: a.1.min(b.1),
            z1: a.2.min(b.2),
            x2: a.0.max(b.0),
            y2: a.1.max(b.1),
            z2: a.2.max(b.2),
        }
    }

    /// Whether `(x, y, z)` lies in this area.
    pub fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        match *self {
            Area::Box {
                x1,
                y1,
                z1,
                x2,
                y2,
                z2,
            } => x >= x1 && x <= x2 && y >= y1 && y <= y2 && z >= z1 && z <= z2,
            Area::Radius {
                x: cx,
                y: _,
                z: cz,
                r,
            } => {
                let (dx, dz) = ((x - cx) as i64, (z - cz) as i64);
                dx * dx + dz * dz <= (r as i64) * (r as i64)
            }
        }
    }
}

/// A time/actor/area/kind restriction for a history sweep.
///
/// The conditions combine with AND; an all-default window selects everything,
/// which is only useful with a small `limit`. `until_ms == 0` means unbounded,
/// mirroring the journal's `since_ms == 0`.
#[derive(Debug, Clone, Default)]
pub struct Window {
    /// Only this actor's events.
    pub actor: Option<ActorId>,
    /// Only events in this area.
    pub area: Option<Area>,
    /// Only events at or after this Unix time in milliseconds (`0` = all).
    pub since_ms: u64,
    /// Only events strictly before this Unix time in milliseconds (`0` = all).
    pub until_ms: u64,
    /// Only these event tags ([`EventBody::tag`](super::EventBody::tag)); empty
    /// selects every kind.
    pub kinds: Vec<u8>,
}

impl Window {
    /// An unrestricted window.
    pub fn new() -> Window {
        Window::default()
    }

    /// Restrict to one actor.
    pub fn actor(mut self, actor: ActorId) -> Window {
        self.actor = Some(actor);
        self
    }

    /// Restrict to one area.
    pub fn area(mut self, area: Area) -> Window {
        self.area = Some(area);
        self
    }

    /// Restrict to events at or after `since_ms`.
    pub fn since(mut self, since_ms: u64) -> Window {
        self.since_ms = since_ms;
        self
    }

    /// Restrict to events strictly before `until_ms`.
    pub fn until(mut self, until_ms: u64) -> Window {
        self.until_ms = until_ms;
        self
    }

    /// Restrict to a set of event kinds.
    pub fn kinds(mut self, kinds: &[u8]) -> Window {
        self.kinds = kinds.to_vec();
        self
    }

    /// Whether `e` is selected by this window.
    pub fn matches(&self, e: &Event) -> bool {
        if let Some(a) = self.actor {
            if e.actor != a {
                return false;
            }
        }
        if e.at_ms < self.since_ms {
            return false;
        }
        if self.until_ms != 0 && e.at_ms >= self.until_ms {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&e.body.tag()) {
            return false;
        }
        if let Some(area) = &self.area {
            let Some((x, y, z)) = e.position() else {
                // Positionless events belong to no area, so an area query
                // leaves them out rather than guessing.
                return false;
            };
            if !area.contains(x, y, z) {
                return false;
            }
        }
        true
    }
}

/// Clamp a caller-supplied page size so a bug or a hostile plugin cannot ask
/// for the whole journal in one call.
pub const MAX_PAGE: usize = 1000;

fn clamp_limit(limit: usize) -> usize {
    limit.min(MAX_PAGE)
}

impl<B: KvBackend> Journal<B> {
    /// The `limit` newest events matching `window`, newest first.
    ///
    /// `before` is the cursor returned by the previous page; `None` starts at
    /// the head. When a single actor is named the actor index is walked;
    /// otherwise sequence numbers are walked down from the cursor.
    pub fn history_page(
        &self,
        window: &Window,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Page, StorageError> {
        let limit = clamp_limit(limit);
        if limit == 0 {
            return Ok(Page::empty());
        }
        let cursor = before.unwrap_or_else(|| self.head());
        match window.actor {
            // A named actor uses their index, so the sweep is that player's
            // history rather than everyone's.
            Some(a) => {
                let seqs = self.index_seqs_below(&super::actor_prefix(a), 17, cursor)?;
                self.page_from_seqs(seqs, window, limit)
            }
            None => self.scan_down(cursor, window, limit, |_| true),
        }
    }

    /// The full timeline of the block at `(x, y, z)`, newest first.
    ///
    /// Walks the column's index backwards and stops after `limit` hits at this
    /// exact position, so the cost is the number of edits to the column after
    /// the cursor — never a scan of the whole journal.
    pub fn block_history(
        &self,
        x: i32,
        y: i32,
        z: i32,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Page, StorageError> {
        self.positional_page(x, z, before, limit, &|e| e.position() == Some((x, y, z)))
    }

    /// The container access log for the block at `(x, y, z)`, newest first.
    ///
    /// An item event names the container as a place, so the log is the item
    /// events in that column whose container place is this block — the chest's
    /// own column index, not the world's history.
    pub fn container_log(
        &self,
        x: i32,
        y: i32,
        z: i32,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Page, StorageError> {
        self.positional_page(x, z, before, limit, &|e| e.body.touches_container(x, y, z))
    }

    /// Whether nobody has ever changed the block at `(x, y, z)`.
    ///
    /// This is the "worldgen-original" question a survival server asks before
    /// paying for a block: a diamond placed, broken and replaced by a player is
    /// not original even though it now reads as diamond, because the journal
    /// records that someone touched it.
    pub fn is_worldgen_original(&self, x: i32, y: i32, z: i32) -> Result<bool, StorageError> {
        Ok(self.last_change_at(x, y, z)?.is_none())
    }

    /// One actor's events, newest first, paged.
    ///
    /// Reads the actor index, so this is that player's history and not a walk
    /// of everyone's.
    pub fn player_events(
        &self,
        actor: ActorId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Page, StorageError> {
        let limit = clamp_limit(limit);
        if limit == 0 {
            return Ok(Page::empty());
        }
        let cursor = before.unwrap_or(u64::MAX);
        let seqs = self.index_seqs_below(&super::actor_prefix(actor), 17, cursor)?;
        self.decode_page(seqs, limit)
    }

    /// One item instance's events, newest first, paged.
    ///
    /// Reads the item index, so this is that instance's whole timeline rather
    /// than a journal scan.
    pub fn item_events(
        &self,
        uid: ItemUid,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Page, StorageError> {
        let limit = clamp_limit(limit);
        if limit == 0 {
            return Ok(Page::empty());
        }
        let cursor = before.unwrap_or(u64::MAX);
        let seqs = self.index_seqs_below(&item_prefix(uid), 9, cursor)?;
        self.decode_page(seqs, limit)
    }

    /// The provenance chain of item `root`, oldest first, walked backwards
    /// through derives and merges up to `depth` generations.
    ///
    /// The result is every event for the root and for the instances it was
    /// made from (a craft's ingredients, a stack a merge drew from), so a
    /// reader can answer "where did #51 come from" without a loop of its own.
    /// Cycle-safe: an instance is visited once however many edges reach it.
    pub fn item_chain(&self, root: ItemUid, depth: usize) -> Result<Vec<Event>, StorageError> {
        let mut seen_uids: HashSet<ItemUid> = HashSet::new();
        let mut seen_seqs: HashSet<u64> = HashSet::new();
        let mut out: Vec<Event> = Vec::new();
        let mut frontier = vec![root];
        seen_uids.insert(root);
        // `depth` generations of ancestors: depth 0 is the root alone.
        for _ in 0..=depth {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for uid in frontier.drain(..) {
                for (k, _) in self.backend.scan_prefix(&item_prefix(uid))? {
                    if k.len() < 17 {
                        continue;
                    }
                    let seq = u64::from_be_bytes(k[9..17].try_into().unwrap());
                    if !seen_seqs.insert(seq) {
                        continue;
                    }
                    let Some(e) = self.get(seq)? else { continue };
                    for linked in linked_uids(&e, uid) {
                        if seen_uids.insert(linked) {
                            next.push(linked);
                        }
                    }
                    out.push(e);
                }
            }
            frontier = next;
        }
        out.sort_by_key(|e| e.seq);
        Ok(out)
    }

    // -- internals ---------------------------------------------------------

    /// A page over one column, keeping only events `accept` selects.
    fn positional_page(
        &self,
        x: i32,
        z: i32,
        before: Option<u64>,
        limit: usize,
        accept: &dyn Fn(&Event) -> bool,
    ) -> Result<Page, StorageError> {
        let limit = clamp_limit(limit);
        if limit == 0 {
            return Ok(Page::empty());
        }
        let cursor = before.unwrap_or(u64::MAX);
        let keys = self.backend.scan_prefix(&column_prefix(x >> 4, z >> 4))?;
        let mut events = Vec::new();
        let mut next = None;
        for (k, _) in keys.iter().rev() {
            if k.len() < 17 {
                continue;
            }
            let seq = u64::from_be_bytes(k[9..17].try_into().unwrap());
            if seq >= cursor {
                continue;
            }
            let Some(e) = self.get(seq)? else { continue };
            if accept(&e) {
                events.push(e);
                if events.len() == limit {
                    next = Some(seq);
                    break;
                }
            }
        }
        Ok(Page { events, next })
    }

    /// Walk sequence numbers below `cursor`, newest first, keeping the first
    /// `limit` that match `window` and the extra predicate.
    fn scan_down(
        &self,
        cursor: u64,
        window: &Window,
        limit: usize,
        extra: impl Fn(&Event) -> bool,
    ) -> Result<Page, StorageError> {
        let mut events = Vec::new();
        let mut seq = cursor;
        while events.len() < limit && seq > 0 {
            seq -= 1;
            let Some(e) = self.get(seq)? else { continue };
            if window.matches(&e) && extra(&e) {
                events.push(e);
            }
        }
        let next = if events.len() == limit && seq > 0 {
            Some(seq)
        } else {
            None
        };
        Ok(Page { events, next })
    }

    /// The sequence numbers under `prefix` that are below `cursor`, ascending.
    fn index_seqs_below(
        &self,
        prefix: &[u8],
        seq_at: usize,
        cursor: u64,
    ) -> Result<Vec<u64>, StorageError> {
        let mut seqs = Vec::new();
        for (k, _) in self.backend.scan_prefix(prefix)? {
            if k.len() < seq_at + 8 {
                continue;
            }
            let seq = u64::from_be_bytes(k[seq_at..seq_at + 8].try_into().unwrap());
            if seq < cursor {
                seqs.push(seq);
            }
        }
        Ok(seqs)
    }

    /// Build a newest-first page from ascending `seqs`, applying `window`.
    fn page_from_seqs(
        &self,
        seqs: Vec<u64>,
        window: &Window,
        limit: usize,
    ) -> Result<Page, StorageError> {
        let mut events = Vec::new();
        let mut next = None;
        for &seq in seqs.iter().rev() {
            let Some(e) = self.get(seq)? else { continue };
            // The actor condition is already satisfied by the index; the rest
            // of the window still applies.
            if window.matches(&e) {
                events.push(e);
                if events.len() == limit {
                    next = Some(seq);
                    break;
                }
            }
        }
        Ok(Page { events, next })
    }

    /// Decode the newest `limit` of ascending `seqs`, newest first.
    fn decode_page(&self, seqs: Vec<u64>, limit: usize) -> Result<Page, StorageError> {
        let mut events = Vec::new();
        let mut next = None;
        for &seq in seqs.iter().rev() {
            let Some(e) = self.get(seq)? else { continue };
            events.push(e);
            if events.len() == limit {
                next = Some(seq);
                break;
            }
        }
        Ok(Page { events, next })
    }
}

/// The instances linked to `uid` by this event, for the item-chain walk.
///
/// A derive links its output to its consumed sources in both directions (the
/// ingredient's own event stream finds the craft that used it); a merge links
/// the two stacks that met. A move links nothing: it is the same instance in a
/// new place, not a new one.
fn linked_uids(e: &Event, uid: ItemUid) -> Vec<ItemUid> {
    match &e.body {
        super::EventBody::ItemDerive { uid: out, from, .. } => {
            if *out == uid {
                from.clone()
            } else if from.contains(&uid) {
                vec![*out]
            } else {
                Vec::new()
            }
        }
        super::EventBody::ItemMerge { into, from, .. } => {
            if *into == uid {
                vec![*from]
            } else if *from == uid {
                vec![*into]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{ActorId, EventBody, Place};
    use crate::storage::MemStore;
    use crate::{BlockStateId, ItemTypeId};

    const STONE: BlockStateId = BlockStateId(1);
    const DIRT: BlockStateId = BlockStateId(3);
    const AIR: BlockStateId = BlockStateId(0);

    fn alice() -> ActorId {
        ActorId(0x11)
    }
    fn bob() -> ActorId {
        ActorId(0x22)
    }

    fn block(x: i32, y: i32, z: i32, from: BlockStateId, to: BlockStateId) -> EventBody {
        EventBody::BlockSet { x, y, z, from, to }
    }

    fn journal() -> Journal<MemStore> {
        Journal::open(MemStore::new()).unwrap()
    }

    #[test]
    fn block_history_is_newest_first_and_only_that_position() {
        let j = journal();
        j.append_at(alice(), block(10, 64, 10, STONE, AIR), 1_000)
            .unwrap();
        // A different block in the same column must not appear.
        j.append_at(bob(), block(11, 64, 10, DIRT, AIR), 2_000)
            .unwrap();
        let s = j
            .append_at(bob(), block(10, 64, 10, DIRT, STONE), 3_000)
            .unwrap();

        let page = j.block_history(10, 64, 10, None, 10).unwrap();
        assert_eq!(page.events.len(), 2, "{:?}", page.events);
        assert_eq!(page.events[0].seq, s, "newest first");
        assert_eq!(page.events[1].actor, alice());
        assert!(page.next.is_none());
    }

    #[test]
    fn block_history_pages_with_a_cursor() {
        let j = journal();
        for i in 0..5 {
            j.append_at(alice(), block(0, 64, 0, DIRT, STONE), i)
                .unwrap();
        }
        let first = j.block_history(0, 64, 0, None, 2).unwrap();
        assert_eq!(first.events.len(), 2);
        let cursor = first.next.expect("more pages");
        let second = j.block_history(0, 64, 0, Some(cursor), 2).unwrap();
        assert_eq!(second.events.len(), 2);
        // No overlap between the pages.
        assert!(second.events[0].seq < first.events[1].seq);
        let third = j.block_history(0, 64, 0, second.next, 2).unwrap();
        assert_eq!(third.events.len(), 1);
        assert!(third.next.is_none(), "exhausted");
    }

    #[test]
    fn worldgen_original_is_false_once_anyone_has_touched_the_block() {
        let j = journal();
        assert!(j.is_worldgen_original(1, 2, 3).unwrap());
        j.append_at(alice(), block(1, 2, 3, STONE, AIR), 1).unwrap();
        assert!(!j.is_worldgen_original(1, 2, 3).unwrap());
        assert!(j.is_worldgen_original(1, 2, 4).unwrap());
    }

    #[test]
    fn a_window_filters_by_actor_area_time_and_kind() {
        let j = journal();
        // Alice at the origin, later.
        j.append_at(alice(), block(1, 64, 1, STONE, AIR), 5_000)
            .unwrap();
        // Bob far away.
        j.append_at(bob(), block(400, 64, 400, STONE, AIR), 5_000)
            .unwrap();
        // Alice, but a different kind, inside the area.
        j.append_at(
            alice(),
            EventBody::ItemMint {
                uid: ItemUid(51),
                item: ItemTypeId::vanilla("minecraft:diamond").unwrap(),
                count: 1,
                to: Place::Ground { x: 1, y: 64, z: 1 },
            },
            5_000,
        )
        .unwrap();

        // Actor + area + kind together.
        let w = Window::new()
            .actor(alice())
            .area(Area::Radius {
                x: 0,
                y: 0,
                z: 0,
                r: 16,
            })
            .kinds(&[1]);
        let page = j.history_page(&w, None, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].actor, alice());

        // Area excludes the far event.
        let w = Window::new().area(Area::Radius {
            x: 0,
            y: 0,
            z: 0,
            r: 16,
        });
        assert_eq!(j.history_page(&w, None, 10).unwrap().events.len(), 2);

        // A time window before anything happened is empty.
        let w = Window::new().since(9_999);
        assert!(j.history_page(&w, None, 10).unwrap().is_empty());
    }

    #[test]
    fn a_box_area_matches_every_axis() {
        let j = journal();
        j.append_at(alice(), block(0, 10, 0, DIRT, STONE), 1)
            .unwrap();
        j.append_at(alice(), block(0, 20, 0, DIRT, STONE), 1)
            .unwrap();
        let w = Window::new().area(Area::between((0, 0, 0), (0, 15, 0)));
        let page = j.history_page(&w, None, 10).unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].body, block(0, 10, 0, DIRT, STONE));
    }

    #[test]
    fn item_events_use_the_item_index() {
        let j = journal();
        let uid = ItemUid(7);
        j.append_at(
            alice(),
            EventBody::ItemMint {
                uid,
                item: ItemTypeId::vanilla("minecraft:diamond").unwrap(),
                count: 3,
                to: Place::Inventory {
                    owner: alice(),
                    slot: 0,
                },
            },
            1,
        )
        .unwrap();
        j.append_at(
            alice(),
            EventBody::ItemMove {
                uid,
                from: Place::Inventory {
                    owner: alice(),
                    slot: 0,
                },
                to: Place::Ground { x: 5, y: 64, z: 5 },
                count: 3,
            },
            2,
        )
        .unwrap();
        // Another uid's event must not appear.
        j.append_at(
            bob(),
            EventBody::ItemMint {
                uid: ItemUid(8),
                item: ItemTypeId::vanilla("minecraft:stone").unwrap(),
                count: 1,
                to: Place::Nowhere,
            },
            3,
        )
        .unwrap();

        let page = j.item_events(uid, None, 10).unwrap();
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].at_ms, 2, "newest first");
    }

    #[test]
    fn item_chain_walks_back_through_a_derive() {
        let j = journal();
        let ore = ItemUid(1);
        let ingot = ItemUid(2);
        let sword = ItemUid(3);
        j.append_at(
            alice(),
            EventBody::ItemMint {
                uid: ingot,
                item: ItemTypeId::vanilla("minecraft:iron_ingot").unwrap(),
                count: 1,
                to: Place::Nowhere,
            },
            1,
        )
        .unwrap();
        j.append_at(
            alice(),
            EventBody::ItemDerive {
                uid: sword,
                item: ItemTypeId::vanilla("minecraft:iron_sword").unwrap(),
                count: 1,
                to: Place::Inventory {
                    owner: alice(),
                    slot: 0,
                },
                from: vec![ingot, ore],
            },
            2,
        )
        .unwrap();
        // The ore was minted too, so the chain reaches it.
        j.append_at(
            alice(),
            EventBody::ItemMint {
                uid: ore,
                item: ItemTypeId::vanilla("minecraft:iron_ore").unwrap(),
                count: 1,
                to: Place::Nowhere,
            },
            3,
        )
        .unwrap();

        let chain = j.item_chain(sword, 2).unwrap();
        let uids: Vec<ItemUid> = chain
            .iter()
            .filter_map(|e| e.body.item_uids().first().copied())
            .collect();
        assert!(uids.contains(&sword), "output in chain");
        assert!(uids.contains(&ingot), "ingredient in chain");
        assert!(uids.contains(&ore), "source in chain");
        // Oldest first, so the root can be read last.
        assert!(chain.windows(2).all(|w| w[0].seq <= w[1].seq));
    }

    #[test]
    fn container_log_tracks_items_moving_in_and_out() {
        let j = journal();
        let (cx, cy, cz) = (10, 64, 10);
        let uid = ItemUid(9);
        // Deposit: inventory -> container.
        j.append_at(
            alice(),
            EventBody::ItemMove {
                uid,
                from: Place::Inventory {
                    owner: alice(),
                    slot: 0,
                },
                to: Place::Container {
                    x: cx,
                    y: cy,
                    z: cz,
                    slot: 3,
                },
                count: 1,
            },
            1,
        )
        .unwrap();
        // Withdraw: container -> ground.
        j.append_at(
            bob(),
            EventBody::ItemMove {
                uid,
                from: Place::Container {
                    x: cx,
                    y: cy,
                    z: cz,
                    slot: 3,
                },
                to: Place::Ground {
                    x: cx,
                    y: cy,
                    z: cz,
                },
                count: 1,
            },
            2,
        )
        .unwrap();
        // A different container is not attributed here.
        j.append_at(
            alice(),
            EventBody::ItemMove {
                uid: ItemUid(10),
                from: Place::Container {
                    x: cx,
                    y: cy,
                    z: cz + 1,
                    slot: 0,
                },
                to: Place::Nowhere,
                count: 1,
            },
            3,
        )
        .unwrap();

        let log = j.container_log(cx, cy, cz, None, 10).unwrap();
        assert_eq!(log.events.len(), 2, "{:?}", log.events);
        assert_eq!(log.events[0].actor, bob(), "newest first");
        assert_eq!(log.events[1].actor, alice());
    }
}
