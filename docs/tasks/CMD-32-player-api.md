# CMD-32 — Core API: player & UI API (PA1)

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-01`, `oc/P1`

## Goal
The host API plugins need for player/UI features (BACKLOG PA1, fed by `docs/design/commands.md` §5):
temporary effects/flight/speed, container windows over another player's inventory, and the text/UI
primitives (title/actionbar/bossbar/scoreboard sidebar/sign & anvil text input), all through the
compat layer so every supported version works. This unblocks essentials-inv (CMD-39),
essentials-player (CMD-38), chat (CMD-40) and moderation (CMD-41).

## API sketch (WIT imports; host adapters in `aether-server`)
- **player**: `set-temporary-effect(player, effect, ticks)`, `set-flight(player, bool)`,
  `set-speed(player, multiplier)` / `set-attribute`, `heal`, `feed`, `set-gamemode`,
  `send-message/title/actionbar`, `read-inventory`, `give/take` (all validated server-side).
- **window (PA1)**: open a server-invented container (chest-style menu, arbitrary slots, paging),
  receive click callbacks (`before-container-click`/an `on-window-click`), update slots/inventory
  from the plugin, close reliably on disconnect. Reuse the existing container/window model
  (`game/containers.rs`, `inventory.rs`) so `/invsee` over another player's inventory is a window
  whose slots are backed by that player's inventory, with a click routed to the owner.
- **UI**: title/subtitle/actionbar with fade/stay/fade, bossbar create/update/remove, scoreboard
  sidebar/team, a sign/anvil-style text input that returns the typed text.
- **scheduler**: `run-after(ticks, callback)` (P1 has a scheduler; verify plugin callbacks run on
  the tick thread at a safe point).
- **services**: `lookup("economy")` / `lookup("permissions")` / `lookup("regions")` returning a
  typed handle from the cross-plugin service registry (P3); a missing service is `None`, not a trap.
- Temporary effects must be **removed on plugin unload** and must not overwrite a permanent
  effect of longer duration/stronger amplifier (EssX semantics).

## Files
- `crates/aether-plugin` WIT + host imports (P1), `aether-server` adapters.
- `game/containers.rs`, `inventory.rs`, `game/effects.rs`, `protocol/` window/text packets.
- the existing `/inventory`, `/inv` command is unrelated (server view); do not change it.

## Edge cases to test explicitly
- opening `/invsee` on another player: edits persist to the owner; closing restores nothing.
- a plugin window on a 1.8.9 codec (no server-side windows) — decide: refuse with a clear error or
  a read-only fallback; document.
- window click with an invalid slot / shift-click / drag is validated, not trusted.
- temporary effect removal on unload; a permanent longer effect is not clobbered.
- a player disconnects with a plugin window open; the plugin gets a close callback and no dangling
  state.
- scheduled callback after the plugin is disabled is dropped, not executed.
- `supports("windows.v1")` gating so a plugin degrades on old codecs.

## Done criteria
- WIT + host adapters + a reference test plugin exercising window, title, effect, scheduler.
- fmt / clippy `-D warnings` / `cargo-locked test`.
