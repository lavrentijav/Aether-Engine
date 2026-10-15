# CMD-26 — `/aether world` management

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-05`, `oc/CMD-01`; a multi-world/dimension core (BACKLOG G9) must exist or this
brief splits: the command surface is implementable once worlds can be loaded/unloaded.

## Goal
List, load and unload worlds at runtime. Extends the `/aether` root from CMD-24. Until the engine
has multiple worlds/dimensions this command is a stub (CMD0b) and the brief is a specification.

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/aether world list` | `aether.command.world` | 4 | `Worlds (<n>): overworld [loaded], nether [loaded], ...` |
| `/aether world load <name>` | `aether.command.world` | 4 | `Loaded world <name>` |
| `/aether world unload <name>` | `aether.command.world` | 4 | `Unloaded world <name>` |
| `/aether world info <name>` | `aether.command.world` | 4 | seed, spawn, border, dimension type, gamerules |

- Unloading a world with players in it is refused (`<name> still has <n> players`); the overworld
  and the default dimension cannot be unloaded.
- A world name maps to a directory under the world root; loading an unknown directory errors, unless
  a `[worlds]` config lists external storage (then it may create it like vanilla).
- Loading a dimension registers it, starts ticking it, and lets players teleport in; unloading
  flushes and removes it from the tick set.

## Files
- new `crates/aether-server/src/commands/world_admin.rs`.
- the world/dimension registry (G9); `stored.rs` for load/flush; config `[worlds]`.

## Edge cases to test explicitly
- load an already-loaded world is a no-op/error naming it.
- unload with a player inside; unload the default world; unload a non-existent world.
- a world directory whose format version is too new is refused with a clear message.
- a world unload flushes pending block ticks and journal entries (durable scheduler, CMD-13).
- re-loading after unload yields identical state (round-trip test).

## Done criteria
- If the multi-world core is not merged: the command answers the CMD0b stub and this brief carries a
  clear "blocked on G9" note; do not fake dimensions.
- Otherwise: implementation + round-trip tests + fmt / clippy / `cargo-locked test`.
