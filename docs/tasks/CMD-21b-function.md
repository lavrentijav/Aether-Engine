# CMD-21b — V-data: /function, /return, /test

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-21a`
Parent/umbrella: `docs/tasks/CMD-21-function-datapack.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/function <name> [<arguments>]` | `minecraft.command.function` | 2 | `Executed <n> command(s) from function <name>` |
| `/return <value>` | `minecraft.command.return` | 2 | (stops function execution with the value) |
| `/test …` | `minecraft.command.test` | 2 | per subcommand (game-test framework) |

A function is compiled once to a resolved command list (tree nodes + args) at load; a macro line
(`$…` with `$(name)`) expands at run time. `/function <tag>` runs every function in the tag in
order. The command source stack (position/rotation/anchor/entity) is preserved, so `/execute`
inside a function is exact.

## Files
- new `crates/aether-server/src/commands/function.rs`.
- the dispatcher from CMD-03 gains a source stack; functions execute their node list, not a re-parse.

## Edge cases to test explicitly
- a function calling itself is caught at `maxCommandChainLength`, error reported (no stack blow).
- a `$` macro expands arguments; a missing macro arg errors.
- `/function #tag` runs members in declared order.
- `/return` inside a nested function returns only that function.
- `/execute … run function` preserves position/rotation/entity across the call.
- a function line without a slash is skipped or errors like vanilla (record which).

## Done criteria
- Implementation + tests + a fixture function pack; fmt / clippy `-D warnings` / `cargo-locked test`.
