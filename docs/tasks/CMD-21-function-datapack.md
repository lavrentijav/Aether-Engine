# CMD-21 — V-data: functions, data packs, reload, return, test

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-20`

## Split (CMD0 review)
This L group is split into; the **data pack loader is its own task**:
| id | brief | contents | size |
|---|---|---|---|
| CMD-21a | `docs/tasks/CMD-21a-datapack-loader.md` | data pack loader: `pack.mcmeta`, pack_format, registries, tags, load-error handling | M |
| CMD-21b | `docs/tasks/CMD-21b-function.md` | `/function` `/return` `/test` + function execution/macros | M |
| CMD-21c | `docs/tasks/CMD-21c-datapack-reload.md` | `/datapack` `/reload` | S |

## Goal
Load a data pack's `function/*.mcfunction` files and run them, with `/reload`, command blocks'
shared engine, `/return`, and the `/datapack` management command. This is the largest V-data
dependency: `/execute`, `/schedule`, `/loot`, `/recipe` and `/advancement` all lean on it.

## Data pack loader
- Discover packs in `world/datapacks/*` and the engine's built-in pack (the operator's
  `data/minecraft/`), honour `pack.mcmeta` `pack_format` compatibility and `pack_format` for
  1.21.11, and the `filter` section.
- Registries to load: `function`, `advancement`, `loot_table`, `predicate`, `item_modifier`,
  `tags/*`, `dimension`, `worldgen/*`, `enchantment` (E1), `recipe`. Load errors are reported and
  the pack is disabled rather than crashing the server (like a broken pack in vanilla).
- Functions are compiled once to a resolved command list (tree nodes + args) at load; a macro
  line (`$...` with `$(name)`) expands at run time.
- Execution context: command source stack (entity/position/rotation/anchor), recursion depth cap
  (`maxCommandForkCount`, `maxCommandChainLength`), and `execute ... run function` sharing state.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/function <name> [<arguments>]` | `minecraft.command.function` | 2 | `Executed <n> command(s) from function <name>` |
| `/datapack enable <name> [first\|last\|before\|after <existing>]` | `minecraft.command.datapack` | 2 | `Enabled data pack <name>` |
| `/datapack disable <name>` | `minecraft.command.datapack` | 2 | `Disabled data pack <name>` |
| `/datapack list [available\|enabled]` | `minecraft.command.datapack` | 2 | `There are <n> data packs:` |
| `/reload` | `minecraft.command.reload` | 4 | `Reloading!` |
| `/return <value>` | `minecraft.command.return` | 2 | (stops function execution with the value) |
| `/test ...` | `minecraft.command.test` | 2 | per subcommand (game-test framework) |

`/function`'s name may be a tag (`#ns:tag`) → run every function in it in order. A bad function
name errors. Macro arguments are a compound tag.

## Files
- new `crates/aether-server/src/datapack.rs` (loader) + `commands/function.rs`, `datapack.rs`.
- the command dispatcher from CMD-03 gains a source stack; a function executes its node list, not a
  re-parse, so `/execute` inside functions is exact.
- `/reload` re-runs the loader and atomically swaps the loaded packs at a safe point.

## Edge cases to test explicitly
- a function calling itself (recursion) is caught at `maxCommandChainLength`, error reported.
- a function with `$` macros expands arguments; a missing macro arg errors.
- `/function #tag` runs members in declared order.
- `/reload` while a function is executing does not observe a half-swapped pack.
- a pack with `pack_format` too new is refused with a clear message; too old is accepted if the
  loader can read it (decide and record).
- `/datapack disable` the engine's own/built-in pack is refused.
- `/return` inside a nested function returns only that function.
- a function line that is a chat message (no slash) is skipped or errors like vanilla.
- `/execute ... run function` preserves position/rotation/entity across the call.

## Done criteria
- Loader + `/function` + `/reload` + tests; a fixture data pack in the test tree.
- fmt / clippy `-D warnings` / `cargo-locked test`.
