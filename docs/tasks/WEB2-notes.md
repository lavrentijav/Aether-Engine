# WEB2 notes — map + time machine

Branch `oc/WEB2` from `main` (`4552af1`). Brief: `docs/tasks/QUEUE-WEB-admin.md` §WEB2.

## Dependencies not on `main`

WEB2 is specified to depend on **PA2** (the read-only History API, a core + SDK service) and **WEB1**
(the plugin HTTP server and login). Neither is merged:

- `oc/PA2` and `oc/WEB1` both sit at `4552af1` (no commits).
- The brief anticipated this: *"if WEB1 is not yet in `main`, start with the map rendering in a
  separate module."*

So this stage was scoped to everything WEB2 can be without them, and it does not assume either
interface exists.

## What shipped

`plugins/webadmin` — a native plugin crate, its logic in host-independent modules:

- **`color`** — block name → RGB palette (lifted from the offline `crates/aether-worldgen/examples/
  vanilla_map.rs`, so the two cannot drift), with a hash fallback for modded blocks. Vanilla block
  names resolve through `registry::blocks::block_of_state`, so `color_of(id)` needs no world.
- **`source`** — `MapSource` and `HistorySource`, the two read-only views, with blanket impls for
  `aether_api::World<B, G>`. This is the seam PA2/WEB1 plug into: one `Arc<World>` coerces to both
  views.
- **`map`** — `MapRenderer`: top-block-per-column tiles, height shading, water depth tint, a tile
  cache keyed `(zoom, tx, tz)`, and `invalidate(x, z)` that drops every covering tile at every zoom.
- **`timemachine`** — `TimeMachine`: `block_history`, `block_at`, `changes_in_area`, `surface_at`,
  and `tile_at` (replayed tiles). The replay rebuilds a column's generator baseline from the first
  event's `from` (the live world can't answer it, having every edit applied), then applies events up
  to the chosen sequence.
- **`playerhist`** — `HistoryQuery` (actor/area/time/limit) over the actor index, plus `action`/
  `kind`/`summarize` for display.
- **`http`** — a small `std::net` HTTP/1.1 server, an inline HTML+JS page (pan/zoom, time slider,
  click-a-block history, overlay layer), and JSON route handlers. No framework, no crate, no CDN.
  `WebAdmin::handle` is a pure function, so every route is tested without a socket.
- **`WebAdminPlugin`** — a thin native-tier wrapper owning the server lifecycle; invalidates tiles on
  block changes.

Everything is read-only.

## Tests

`cargo-locked test -p aether-webadmin-plugin`:

- 27 unit tests: palette; tile render / cache / invalidation / zoom; replay between edits; area
  filtering; per-player queries and naming; the token gate; a real TCP request.
- 3 end-to-end tests: a live `World` edited by two players, served through the public API — live
  tile vs. replayed tiles, per-block history, per-player history, area changes, head, and the cache
  rebuild after `invalidate`.

## Open items / follow-ups

1. **Server wiring.** The plugin host gives a native plugin no world handle, so `registration()`
   loads without one and logs that the map is disabled. The server bootstrap (WEB1) should build a
   `WebAdmin` from its `World` and add `WebAdminPlugin::new(admin, bind)` through `add_native`. When
   that happens, the map also wants an explicit invalidation call at the world-write hook (today it
   relies on the `before-block-place` / `after-block-break` events, which miss plugin writes).
2. **PA2 seam.** `HistorySource` is deliberately minimal; when PA2 lands its paged, capability-gated
   `history` service, this trait should delegate to it rather than to `Journal` directly, and the
   per-player/area queries should use PA2's indexes.
3. **Performance at scale.** A tile at zoom 0 scans every column once; the replay path scans a
   column's whole event list per pixel. Neither is a problem at the sizes tested, but a production
   map wants a heightmap cache and a per-column replay cache. The live path is fine (tiles are
   cached and only invalidated regions rebuild).
4. **Palette fidelity.** The palette is an operator overview, not vanilla grass/foliage tinting.

## Deviations

Recorded in `KNOWN_ISSUES §A20` and `STATUS.md` Phase 4.
