# Lock order in `aether-server`

Status: **audit done 2026-10-14**, one inversion fixed. Scope: the three
in-server modules whose mutexes are taken from more than one module — the
global **entity** list, the per-player handles, and the global **container**
map. World/backend locks (`aether-api`) sit below all of these and are not
ordered here.

## The locks

| module | lock | holds |
|---|---|---|
| `game/entities.rs` | `entities()` | every entity (mobs, arrows, dropped items) |
| `game/entities.rs` | `rng()` | the entity tick's RNG |
| `players.rs` | `PlayerHandle::game` | health, hunger, window, cursor, digging |
| `players.rs` | `PlayerHandle::inventory` | the 46 slots |
| `players.rs` | `PlayerHandle::tracked` | entity ids this client has spawned |
| `players.rs` | `PlayerHandle::pos` | position/look (a `Copy` value, never held across a call) |
| `game/containers.rs` | `all()` | chest/furnace contents by block position |
| `game/containers.rs` | `active()` | furnaces that are currently lit |
| `stash/mod.rs` | `stashes()` | recovered-block stashes by player |
| `stash/mod.rs` | `views()` | each player's stash page / dialogue |
| `region.rs` | `RegionStore::current` (read) | an `Arc<RegionIndex>` snapshot; released before the query |

`pos` is called and released within a single expression everywhere
(`PlayerHandle::pos` returns a copy), so it never nests.

## The order

Whenever two of the three modules are held at once, they are taken in this
order — outermost first:

```
entities{}  →  rng{}  →  PlayerHandle::game  →  PlayerHandle::inventory
            →  containers::all  →  containers::active  →  (world/backend locks)
```

`PlayerHandle::tracked` is a leaf and is only ever taken while `entities` is
held (in `entities::update_tracking`), which the order already permits.

The region index's snapshot lock is a leaf too: the protection hooks
(`session::break_block`/`place_block`, `game/interact`, `game/combat::explode`)
clone the `Arc` before holding any player or entity lock across the check, and
`RegionIndex::check` then runs lock-free on the snapshot. The check never holds
a region lock across a world read, so it cannot participate in an inversion
with the order above.

The order is **set by the entity tick**, because that is the one path that
genuinely needs two modules at once:

* `game/entities.rs::tick` holds `entities()` (and `rng()`) across its whole
  body. In the pickup loop it calls `provenance::Scope::begin`, which takes the
  player's `game`, then `inventory`, and — through `capture` — the container
  map. That is the origin of `entities → rng → game → inventory → all`.
* `entities::update_tracking` likewise holds `entities()` and takes each
  player's `pos` and `tracked`.

Everything on the connection threads fits under it:

* `game/provenance.rs::capture` (used by every item action) takes
  `game`, then `inventory`, then `containers::snapshot` — in order, but
  sequentially, not nested.
* `game/window.rs::click` holds `game` and `inventory` while
  `containers::with` holds `all` — `game → inventory → all`.
* `game/window.rs::sync_open` is the same shape.
* `game/containers.rs::with` takes `all` and, still holding it, `active`
  (to mark a lit furnace); `tick_furnaces` does the same — `all → active`.
* `stash/mod.rs::refresh` takes `stashes` then `views`; `on_click` ends with
  the same pair — `stashes → views`.

The rule of thumb: **the tick path is outermost**. A connection-thread handler
may take player locks and then the container map, but it must never take a
player lock and then the entity list, because the tick is doing the opposite.

## The inversion that was fixed

`commands::inventory` (`/inv`) held `handle.inventory()` for the whole function
— including the call to `entities::owned_items(handle.uuid)`, which takes
`entities()`. That is `inventory → entities`, the reverse of the tick's
`entities → inventory`. Two threads could then wedge:

* the tick holds `entities`, and waits for that player's `inventory`;
* `/inv` holds `inventory`, and waits for `entities`.

Fixed by dropping the inventory guard before reading the entity list
(`crates/aether-server/src/commands/mod.rs`).

Every other `handle.inventory()` / `handle.game()` held across a call into
another module was checked and is either released first (`interact::use_item`,
`interact::use_entity`, `interact::drop_held` all `drop(inv)` before
`entities::throw_from`; `combat::respawn` releases `game` before
`entities::reset_tracking`; `stash::take_named` releases `stashes` before
`ground::give_or_drop`; `ground::give_or_drop` releases the inventory before
`drop_at_feet`) or is already in the order above.

## Poisoning

Every lock is taken with `.lock().unwrap()` (or `.read()/.write().unwrap()`):
a thread that panics while holding one poisons it, and the next taker panics
too. That is deliberate fail-stop, not an oversight — the alternative,
ignoring poisoning, would let the server keep serving from a half-mutated
entity list or container map, and a wrong world is worse than a dead one.
Treat a poisoned lock as "this process is no longer trustworthy": log it and
let it come down, rather than recovering the guard. No change was needed here;
the audit found no place that catches a poisoning panic and continues.

## Checking it

```text
rg -n '\.lock\(\)\.unwrap\(\)|\.read\(\)\.unwrap\(\)|\.write\(\)\.unwrap\(\)' crates/aether-server/src
```

For each hit, look for a call into `game::entities`, `game::containers` or a
`PlayerHandle` method before the guard goes out of scope, and check it against
the order above.
