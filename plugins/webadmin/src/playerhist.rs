//! Per-player history: everything a player placed, broke, took or put,
//! filterable by time and area.
//!
//! The journal already indexes events by actor, so a per-player query costs
//! that player's history and not the world's — the same index a rollback uses.
//! This module is the read side of it: a filter, a bounded query, and a
//! human-readable summary for the web page.

use aether_world::journal::{ActorId, ChatKind, Event, EventBody, Place};
use aether_world::registry::items::ItemTypeId;
use aether_world::StorageError;

use crate::source::HistorySource;

/// What to select from the journal.
///
/// An empty filter is the whole history; the journal's own queries still
/// narrow the scan to the actor index when `actor` is set.
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    /// Only this player's actions.
    pub actor: Option<ActorId>,
    /// Only within `radius` blocks horizontally of `(x, z)`.
    pub area: Option<(i32, i32, u32)>,
    /// Only events at or after this wall-clock time (Unix ms).
    pub since_ms: u64,
    /// Only events at or before this time; `0` means no upper bound.
    pub until_ms: u64,
    /// At most this many events, the **newest** ones. `0` means unlimited.
    pub limit: usize,
}

impl HistoryQuery {
    /// Restrict to one actor.
    pub fn by(mut self, actor: ActorId) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Restrict to a horizontal radius.
    pub fn near(mut self, x: i32, z: i32, radius: u32) -> Self {
        self.area = Some((x, z, radius));
        self
    }

    /// Restrict to a time window.
    pub fn between(mut self, since_ms: u64, until_ms: u64) -> Self {
        self.since_ms = since_ms;
        self.until_ms = until_ms;
        self
    }

    /// Cap the result, newest first.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    fn matches(&self, e: &Event) -> bool {
        if e.at_ms < self.since_ms {
            return false;
        }
        if self.until_ms != 0 && e.at_ms > self.until_ms {
            return false;
        }
        if let Some((cx, cz, r)) = self.area {
            let Some((x, _, z)) = e.position() else {
                return false;
            };
            let (dx, dz) = ((x as i64 - cx as i64), (z as i64 - cz as i64));
            if dx * dx + dz * dz > r as i64 * r as i64 {
                return false;
            }
        }
        true
    }
}

/// Every event `q` selects, oldest first, capped at `limit` (newest kept).
pub fn query(h: &dyn HistorySource, q: &HistoryQuery) -> Result<Vec<Event>, StorageError> {
    let mut out = Vec::new();
    let mut push = |e: &Event| -> Result<(), StorageError> {
        if q.matches(e) {
            out.push(e.clone());
        }
        Ok(())
    };
    match q.actor {
        Some(a) => h.for_each_actor_event(a, &mut push)?,
        None => h.for_each_event(&mut push)?,
    }
    if q.limit != 0 && out.len() > q.limit {
        out.drain(0..out.len() - q.limit);
    }
    Ok(out)
}

/// A short, stable verb for what an event did — the word the history page
/// shows next to the coordinates.
pub fn action(e: &Event) -> &'static str {
    match &e.body {
        EventBody::BlockSet { from, to, .. } => {
            let from_air = *from == aether_world::BlockStateId::AIR;
            let to_air = *to == aether_world::BlockStateId::AIR;
            match (from_air, to_air) {
                (true, false) => "place",
                (false, true) => "break",
                _ => "change",
            }
        }
        EventBody::ItemMint { to, .. } => match to {
            Place::Inventory { .. } => "obtain",
            Place::Ground { .. } => "drop",
            Place::Container { .. } => "store",
            Place::Nowhere => "destroy",
        },
        EventBody::ItemMove { from, to, .. } => move_action(from, to),
        EventBody::ItemDestroy { .. } => "destroy",
        EventBody::ItemMerge { .. } => "merge",
        EventBody::ItemDerive { .. } => "craft",
        EventBody::ChatMessage { kind, .. } => match kind {
            ChatKind::Public => "chat",
            ChatKind::Private => "message",
            ChatKind::Command => "command",
        },
        EventBody::Custom { .. } => "custom",
    }
}

fn move_action(from: &Place, to: &Place) -> &'static str {
    match (from, to) {
        (Place::Ground { .. }, Place::Inventory { .. }) => "pickup",
        (Place::Inventory { .. }, Place::Ground { .. }) => "drop",
        (Place::Inventory { .. }, Place::Container { .. }) => "put",
        (Place::Container { .. }, Place::Inventory { .. }) => "take",
        (Place::Container { .. }, Place::Container { .. }) => "put",
        _ => "move",
    }
}

/// The event kind tag's stable name (`block_set`, `item_move`, …).
pub fn kind(e: &Event) -> &'static str {
    aether_world::journal::EVENT_KINDS
        .iter()
        .find(|(t, _)| *t == e.body.tag())
        .map(|(_, n)| *n)
        .unwrap_or(aether_world::journal::EVENT_KIND_UNKNOWN.1)
}

/// A flat, serialisable read of one event for the web layer.
#[derive(Debug, Clone, PartialEq)]
pub struct EventSummary {
    pub seq: u64,
    pub at_ms: u64,
    pub actor: ActorId,
    pub kind: &'static str,
    pub action: &'static str,
    pub position: Option<(i32, i32, i32)>,
    pub item: Option<ItemTypeId>,
    pub count: u8,
}

/// Summarise `e` for display.
pub fn summarize(e: &Event) -> EventSummary {
    let (item, count) = match &e.body {
        EventBody::ItemMint { item, count, .. } => (Some(*item), *count),
        EventBody::ItemMove { uid: _, count, .. } => (None, *count),
        EventBody::ItemDerive { item, count, .. } => (Some(*item), *count),
        _ => (None, 0),
    };
    EventSummary {
        seq: e.seq,
        at_ms: e.at_ms,
        actor: e.actor,
        kind: kind(e),
        action: action(e),
        position: e.position(),
        item,
        count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::World;
    use aether_world::journal::ItemUid;
    use aether_world::registry::ids;
    use aether_world::{BlockStateId, MemStore};
    use aether_worldgen::FlatGenerator;

    const ALICE: ActorId = ActorId(0xA11CE);
    const BOB: ActorId = ActorId(0xB0B);

    fn world() -> World<MemStore, FlatGenerator> {
        World::new(MemStore::new(), FlatGenerator::classic())
    }

    fn set(
        w: &World<MemStore, FlatGenerator>,
        actor: ActorId,
        x: i32,
        y: i32,
        z: i32,
        id: BlockStateId,
    ) {
        let props = w.props_of(id);
        w.set_block_by(actor, x, y, z, id, props);
    }

    #[test]
    fn query_by_actor_returns_only_their_events() {
        let w = world();
        set(&w, ALICE, 0, 10, 0, ids::STONE);
        set(&w, BOB, 1, 10, 0, ids::DIRT);
        set(&w, ALICE, 2, 10, 0, ids::SAND);
        let all = query(&w, &HistoryQuery::default().by(ALICE)).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|e| e.actor == ALICE));
    }

    #[test]
    fn query_by_area_excludes_far_events() {
        let w = world();
        set(&w, ALICE, 0, 10, 0, ids::STONE);
        set(&w, ALICE, 100, 10, 100, ids::STONE);
        let near = query(&w, &HistoryQuery::default().near(0, 0, 8)).unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].position().unwrap().0, 0);
    }

    #[test]
    fn action_names_place_and_break() {
        let w = world();
        set(&w, ALICE, 0, 10, 0, ids::STONE);
        set(&w, ALICE, 0, 10, 0, ids::AIR);
        let h = query(&w, &HistoryQuery::default().by(ALICE)).unwrap();
        assert_eq!(action(&h[0]), "place");
        assert_eq!(action(&h[1]), "break");
        assert_eq!(kind(&h[0]), "block_set");
    }

    #[test]
    fn item_move_is_named_by_direction() {
        let w = world();
        let e = w
            .journal()
            .append(
                ALICE,
                EventBody::ItemMove {
                    uid: ItemUid(1),
                    from: Place::Ground { x: 0, y: 4, z: 0 },
                    to: Place::Inventory {
                        owner: ALICE,
                        slot: 0,
                    },
                    count: 1,
                },
            )
            .unwrap();
        let ev = query(&w, &HistoryQuery::default().by(ALICE)).unwrap();
        assert_eq!(ev[0].seq, e);
        assert_eq!(action(&ev[0]), "pickup");
        assert_eq!(summarize(&ev[0]).count, 1);
    }

    #[test]
    fn limit_keeps_the_newest() {
        let w = world();
        for i in 0..5 {
            set(&w, ALICE, i, 10, 0, ids::STONE);
        }
        let last2 = query(&w, &HistoryQuery::default().by(ALICE).limit(2)).unwrap();
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[1].position().unwrap().0, 4);
    }
}
