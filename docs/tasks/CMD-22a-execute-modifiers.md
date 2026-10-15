# CMD-22a — V-data: /execute modifiers

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-21b`
Parent/umbrella: `docs/tasks/CMD-22-execute.md`

## Goal
The chainable modifier half of `/execute`, which reshapes the command source stack and runs the
inner command once per selected entity.

## Modifiers (vanilla 1.21.11)
`as <targets>`, `at <target>`, `positioned <pos>`, `positioned as <targets>`, `rotated <rotation>` /
`rotated as <target>`, `facing <pos>` / `facing entity <target> <anchor>`, `align <axes>`,
`anchored eyes|feet`, `in <dimension>`, `on <relation>` (`attacker|controller|leash|owner|passengers|
target|vehicle`), `summon <entity>`.

Semantics: a modifier with a selector resolving to **0** entities fails silently (no error). A
multi-entity modifier (`as`, `at`, `positioned as`, `rotated as`, `facing entity`, `on`) runs the
chain once per entity unless it is the final source. Terminator `run <command>` is added in
CMD-22b.

## Files
- new `crates/aether-server/src/commands/execute.rs`.
- command tree source stack (CMD-03), selectors (CMD-06).

## Edge cases to test explicitly
- `/execute as @a at @s …` runs per player with their own position.
- `/execute positioned ~ ~1 ~ rotated as @s …` composes left-to-right.
- `/execute align xz` floors the source; `align xx` (repeated axis) is a parse error.
- `/execute on owner|passengers|vehicle|target|attacker` selects the right relation.
- a 0-entity modifier fails silently; a syntax error is a command error.

## Done criteria
- Implementation + a modifier test matrix; fmt / clippy `-D warnings` / `cargo-locked test`.
