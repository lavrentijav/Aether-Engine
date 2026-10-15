# CMD-22b — V-data: /execute conditions, store, run

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-15a`, `oc/CMD-22a`
Parent/umbrella: `docs/tasks/CMD-22-execute.md`

## Goal
The condition, `store` and `run` half of `/execute`. Returns the success/result count of the inner
command (0 if a condition fails).

## Grammar (vanilla 1.21.11)
Conditions: `if|unless` with `block <pos> <block>`, `blocks <begin> <end> <destination> <mode>`,
`data block|entity|storage …`, `entity <entities>`, `score <target> <objective> <op> <source> <sourceObjective>`,
`biome <pos> <biome>`, `dimension <id>`, `loaded <pos>`, `function <name>`, `predicate <id>`.
Store: `store result|success block <pos> <path> <type> <scale>`, `… entity <target> <path> <type> <scale>`,
`… score <targets> <objective>`, `… storage <id> <path> <type> <scale>`.
Terminator: `run <command>`.

## Files
- `crates/aether-server/src/commands/execute.rs`.
- scoreboard (CMD-15a), NBT/data (CMD-14a/14b), function (CMD-21b), bossbar (CMD-17b).

## Edge cases to test explicitly
- `/execute if entity @e[type=!player] run …` returns 0 and does not run when none.
- `store result score …` stores the inner result; `success` stores 0/1; `store … run` still runs
  the command and returns its value.
- a failing condition stops the chain with no message; a syntax error is a command error.
- `if blocks` compares two volumes in all four modes (all/masked).
- `store` into a bad path/type errors; `/execute` with no `run` errors (`Expected "run"`).

## Done criteria
- Implementation + a condition/store test matrix; `/execute` inside a function preserves the stack.
- fmt / clippy `-D warnings` / `cargo-locked test`.
