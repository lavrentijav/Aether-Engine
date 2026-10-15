# History API (PA2)

Status: ✅ implemented (2026-10-15), stage PA2 of [`QUEUE-WEB-admin.md`](../tasks/QUEUE-WEB-admin.md).

The journal has always held every change to the world; PA2 is the **reading
side** an operator tool, a stats plugin or the web admin asks questions with.
It is read-only, paged, and gated behind a capability. Two halves: a core query
engine in `aether-world::journal::history`, and an SDK service `history`
(`aether:sdk/history@0.1`) that exposes it to native and WASM plugins.

## The questions

| question | core | service |
|---|---|---|
| who changed block `(x,y,z)`, and when — full per-block timeline | `Journal::block_history` | `block-history` |
| is this block worldgen-original? | `Journal::is_worldgen_original` | `worldgen-original` |
| what happened in this area, at this time? | `Journal::history_page` | `events` |
| where did this item come from? | `Journal::item_chain` | `item-chain` |
| who accessed this container? | `Journal::container_log` | `container-log` |
| what has this player been doing? | `Journal::player_events` | `player-events` |

## Cost: one index per object, never the whole journal

The journal already writes three payload-free indexes beside each event: `E`
(the event), `X cx cz seq` (the column), `P actor seq` (the actor). PA2 adds a
fourth, `I uid seq` (the item instance), so an item's whole timeline is a
prefix read of its own entries.

* A **block** or a **container** query walks only that column's `X` index
  backwards and stops after `limit` hits at that exact position. The column may
  be busy, but the journal may be 100 M events and the cost is unchanged.
* A **player** query walks that actor's `P` index, not the newest few of
  everyone's.
* An **item** query and the provenance chain walk the `I` index.
* Only a fully unrestricted time/area sweep falls back to walking sequence
  numbers down from the cursor, and it stops the moment `limit` matches are
  found: newest first, bounded work per page.

Pages are newest-first and the cursor is a **sequence number**, so paging is
deterministic regardless of wall clock, and an append between two pages shows
up as a newer page rather than reordering an older one. `limit` is clamped to
`MAX_PAGE` (1000) so no caller can ask for the whole journal in one call.

## Worldgen-original

`is_worldgen_original(x, y, z)` is true when `last_change_at` finds no event
for the position: the generator is deterministic, so a block nobody journalled
is exactly what the seed made. A block a player placed, broke and replaced is
reported **false** — the journal remembers the touch even though the current
block reads as natural. That is the check a survival server needs before paying
for a block, and what the `rewards`/`stats` plugins (PM2/CMD-47) read.

## Provenance

`item_chain(root, depth)` walks the item index backwards through `item_derive`
(a craft/smelt output and its consumed ingredients) and `item_merge` (a stack
topped up from another) up to `depth` generations, cycle-safe, and returns the
events oldest-first. `item_move` links nothing: the same instance in a new
place is not a new item.

## Capability

The service is gated on the caller's `history.read` capability, applied **once**
in the SDK plugin's provider (`plugins/sdk`), reading the caller's effective
grant from the host. A dependent asks for the capability in `plugin.toml`; the
operator grants it. The host bridge stays capability-unaware, exactly as for
`permissions.admin`, so a native dependent and a WASM one are checked alike.

## Encoding at the boundary

The WIT `history` records carry both the compact ids (`from-block`/`to-block`
state ids, `uid`, `item` id) and the resolved names, plus a rendered place
string. The core returns `aether_world::journal::Event`s; the server maps them
to the plugin-facing records (`crates/aether-server/src/plugins.rs`), resolving
names through the world registry. Unknown event-kind names in a window select
**nothing** rather than silently widening the query.

## Limits

* The item index is written from the moment the feature ships; a journal
  written by an older build has no `I` entries for events already on disk, so
  `item_events`/`item_chain` see the item's history from the upgrade on. A
  world may be wiped (storage-v2 amendment 2, no migration), and the index is
  derived and can be rebuilt in a future pass if a server needs it.
* A time window has no time index: a sweep resolves the head and walks
  sequence numbers down, stopping at `limit` matches or the window's lower
  time bound. Object queries (block/container/player/item) use their index and
  never do this.
* `history_page` with an actor but also an area/time filter reads that actor's
  whole index per page. Fine for one operator's tool; a hot path can add a
  paged index cursor later.

## Tests

* `crates/aether-world/src/journal/history.rs` — block history order and
  paging, worldgen-original, window predicates, item index, provenance chain,
  container log.
* `crates/aether-plugin/src/sdk.rs` — the bridge forwards the caller id and the
  page, and answers the neutral value with no provider.
* `plugins/sdk/src/lib.rs` — the `history.read` gate admits and refuses.
* `crates/aether-server/src/plugins.rs` — uuid split, window mapping (including
  an unknown kind), place rendering.
* `crates/aether-plugin/tests/wit_world.rs` — the SDK services world still
  imports every interface, `history` included.
