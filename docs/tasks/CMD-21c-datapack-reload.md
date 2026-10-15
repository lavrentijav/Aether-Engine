# CMD-21c — V-data: /datapack, /reload

Status: 📋 task brief (CMD0) · size: S · tier: core (C)
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-21a`
Parent/umbrella: `docs/tasks/CMD-21-function-datapack.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/datapack enable <name> [first\|last\|before\|after <existing>]` | `minecraft.command.datapack` | 2 | `Enabled data pack <name>` |
| `/datapack disable <name>` | `minecraft.command.datapack` | 2 | `Disabled data pack <name>` |
| `/datapack list [available\|enabled]` | `minecraft.command.datapack` | 2 | `There are <n> data packs:` |
| `/reload` | `minecraft.command.reload` | 4 | `Reloading!` |

`/reload` re-runs the CMD-21a loader and **atomically swaps** the loaded packs at a safe point, so
a running function never observes a half-swapped pack.

## Files
- `crates/aether-server/src/commands/datapack.rs`; the loader (CMD-21a).

## Edge cases to test explicitly
- `/datapack disable` the built-in pack is refused.
- enable with `before`/`after` reorders; an unknown ordering target errors.
- `/reload` during a running function does not tear its command list.
- `/datapack list available` includes disabled packs.

## Done criteria
- Implementation + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
