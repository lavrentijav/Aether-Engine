# MN1 — GUI menus: native plugin `plugins/menus` (chest/inventory menus for every other plugin)

Read `docs/tasks/COMMON.md`, `docs/design/plugins.md` (tiers C/N/W), `docs/design/plugin-sdk.md` (SDK is a
native plugin exporting services), `docs/tasks/CMD-OVERVIEW.ru.md` (CMD-32 player/UI API), the server
window layer (`game/window.rs`, containers, `protocol/compat` item tables).

## Why
Shops, donations, economy, kits, warps, settings, moderation — almost every plugin needs clickable
inventory menus. One shared, safe implementation instead of fifteen half-broken ones.

## Tier
**Native plugin** (`plugins/menus`, tier N) on top of the SDK; it exports a `menus` service (WIT) so any
native or WASM plugin opens a menu with one call. Core only provides the low-level window API (open a
virtual container of N rows / type, set slots, receive clicks, close) — add it if CMD-32 has not.

## Features
1. **API for plugins**: builder — title, rows (or hopper/dispenser/anvil-like types where the protocol
   allows), items (any item + name + lore + glint + model id), per-slot click handlers (left/right/shift/
   middle/number key/drop), pagination helper, borders/fillers, live update of one slot, close callback,
   per-player state, open from command/event/NPC.
2. **Declarative menus** from files (`plugins/menus/menus/*.toml`, DeluxeMenus-like): items, conditions
   (permission, economy balance, placeholder compare), actions (`[command]`, `[console]`, `[message]`,
   `[open] other_menu`, `[close]`, `[sound]`, `[economy] take 100`, `[give]`), placeholders
   (`%player%`, `%balance%`, plugin-provided), open commands, reload.
3. **Security (must)**: menu items are virtual — the server never lets them enter a real inventory: every
   click type (shift, double-click collect, drag, number-key swap, offhand swap, drop, creative middle-click)
   on a menu is cancelled and resynced; menus survive client desync and lag; items are not journalled as
   real items; a closed/expired menu ignores late clicks; rate limit per player.
4. **All protocol versions** we speak: titles as JSON/NBT per version, item stand-ins through
   `protocol::compat` for old clients, window type ids per release.
5. Integrations through services only (economy, permissions, placeholders) — optional dependencies.

## Tests
Unit tests for the builder, action parsing, conditions; server-side tests that every click type on a menu
leaves the player's real inventory unchanged (property test over random click sequences); a declarative
menu end-to-end; old-protocol rendering. Live test via the V1 harness (MCC opens a menu and clicks).

Commit per step (MN1a core window API, MN1b service + builder, MN1c declarative files + actions,
MN1d security property tests, MN1e docs EN+RU + example menus). Build only via cargo-locked.
