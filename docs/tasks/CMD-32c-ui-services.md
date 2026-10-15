# CMD-32c — Core API: UI primitives + service lookup

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`, `oc/P1`, `oc/CMD-32a`
Parent/umbrella: `docs/tasks/CMD-32-player-api.md`

## Goal
The text/UI primitives and cross-plugin service lookup plugins need, on top of CMD-32a.

## API sketch (WIT imports)
- **text:** `send-message`, `send-title`/`actionbar` with fade/stay/fade, `bossbar` create/update/
  remove, scoreboard sidebar/team, a sign/anvil-style **text input** that returns the typed text.
- **services:** `lookup("economy")` / `lookup("permissions")` / `lookup("regions")` returning a
  typed handle from the cross-plugin service registry (P3); a missing service is `None`, not a trap.
- All UI goes through the compat layer so every supported version renders it.
- `supports(feature)` gates versioned names (`economy.v1`, `ui.title.v1`, …).

## Files
- `crates/aether-plugin` WIT + host imports (P1), `aether-server` adapters.
- `protocol/` title/bossbar/scoreboard packets; the service registry (P3).

## Edge cases to test explicitly
- title/bossbar/sidebar render on 1.21.11 and one older codec; fade times honoured.
- sign/anvil text input returns the typed text and handles cancel.
- `lookup` of an absent service returns `None`; a disabled service's handle is not dangling.
- `supports` returns the documented true/false per codec.

## Done criteria
- WIT + adapters + a reference test plugin exercising title, bossbar, text input and lookup.
- fmt / clippy `-D warnings` / `cargo-locked test`.
