# CMD-32b — Core API: window / container API (PA1)

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`, `oc/CMD-32a`
Parent/umbrella: `docs/tasks/CMD-32-player-api.md`

## Goal
Server-invented container windows for plugins (BACKLOG PA1): chest-style menus with click
callbacks, paging, and views over another player's inventory (`/invsee`), reusing the existing
container/inventory model.

## API sketch
- Open a window with arbitrary slots; receive `before-container-click`/`on-window-click`; update
  slots/inventory from the plugin; close reliably on disconnect.
- `/invsee` over another player's inventory is a window whose slots are backed by that player's
  inventory, with clicks routed to the owner.
- Compat layer: works on every version that has server-side windows; on versions without them,
  refuse with a clear error (decide and document).

## Files
- `crates/aether-plugin` WIT + host imports (P1), `aether-server` adapters.
- `game/containers.rs`, `inventory.rs`, per-codec window packets.

## Edge cases to test explicitly
- `/invsee` edits persist to the owner; closing restores nothing.
- window click with an invalid slot / shift-click / drag is validated, not trusted.
- a player disconnects with a window open; the plugin gets a close callback and no dangling state.
- `supports("windows.v1")` gates old codecs with a clear error.

## Done criteria
- WIT + adapters + a reference test plugin exercising a menu and `/invsee`.
- fmt / clippy `-D warnings` / `cargo-locked test`.
