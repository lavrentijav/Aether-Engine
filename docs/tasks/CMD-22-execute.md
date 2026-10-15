# CMD-22 — V-data: `/execute`

Status: 📋 task brief (CMD0) · size: L · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-15`, `oc/CMD-21`

## Split (CMD0 review)
This L task is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-22a | `docs/tasks/CMD-22a-execute-modifiers.md` | modifiers: `as/at/positioned/rotated/facing/align/anchored/in/on/summon` | M |
| CMD-22b | `docs/tasks/CMD-22b-execute-conditions-store.md` | `if/unless` conditions, `store`, `run` | M |

## Goal
The full vanilla `/execute`: modifier subcommands that reshape the command source stack, condition
subcommands, and `store`. This is the most-used command in data packs and must match vanilla
grammar and semantics exactly. Do this **after** the command tree, selectors, scoreboard and
functions are in.

## Grammar (vanilla 1.21.11)
Modifiers (chainable, in order):
`as <targets>`, `at <target>`, `positioned <pos>`, `positioned as <targets>`,
`rotated <rotation>` / `rotated as <target>`, `facing <pos>` / `facing entity <target> <anchor>`,
`align <axes>`, `anchored eyes|feet`, `in <dimension>`, `on <relation>`, `summon <entity>`.
Conditions: `if|unless` with one of
`block <pos> <block>`, `blocks <begin> <end> <destination> <mode>`, `data block|entity|storage ...`,
`entity <entities>`, `score <target> <objective> <op> <source> <sourceObjective>`,
`biome <pos> <biome>`, `dimension <id>`, `loaded <pos>`, `function <name>`, `predicate <id>`.
Store: `store result|success block <pos> <path> <type> <scale>`,
`... entity <target> <path> <type> <scale>`, `... score <targets> <objective>`, `... storage <id> <path> <type> <scale>`, `... bossbar <id> value|max`.
Terminator: `run <command>`.

Semantics: a modifier with an entity selector that resolves to **0** entities makes the chain
**fail silently** (no error, stores 0); the chain runs once per entity for multi-entity modifiers
(`as`, `at`, `positioned as`, `rotated as`, `facing entity`, `on`) unless it is the final source.
`/execute` returns the success/result count of the inner command (0 if conditions fail).

## Files
- new `crates/aether-server/src/commands/execute.rs`.
- command tree (CMD-03) source-stack model; selectors (CMD-06b); scoreboard (CMD-15); `/data`
  write path (CMD-14); bossbar (CMD-17); function (CMD-21).

## Edge cases to test explicitly
- `/execute as @a at @s run ...` runs per player with their own position.
- `/execute if entity @e[type=!player] run ...` returns 0 and does not run when none.
- `/execute store result score ...` stores the inner result; `success` stores 0/1; `/execute
  store ... run` still runs the command and returns its value.
- a failing condition stops the chain with no message; a syntax error is a command error.
- `/execute positioned ~ ~1 ~ rotated as @s ...` composes left-to-right.
- `/execute align xz` floors the source; `align` with repeated axes (`xx`) is a parse error.
- `if blocks` compares two volumes correctly with all four comparison modes (all/masked).
- `/execute on owner|passengers|vehicle|target|attacker` selects the right relation.
- `store` into a full/foreign slot type is validated; a bad path errors.
- `/execute` with no `run` and no terminal modifier errors (`Expected "run"`).

## Done criteria
- Implementation + a broad test matrix for modifiers/conditions/store.
- `/execute` inside a function (CMD-21) preserves the stack; `/return` interacts correctly.
- fmt / clippy `-D warnings` / `cargo-locked test`.
