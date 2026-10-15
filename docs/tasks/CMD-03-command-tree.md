# CMD-03 — Brigadier command tree (parse, suggest, dispatch)

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-01`

## Goal
Replace the hand-written `match cmd { … }` dispatchers with a **Brigadier-style command tree**: one
tree of literal and argument nodes, one parser that walks text against it, one suggestion engine
for tab-completion, and one execution path. This is the spine all other CMD tasks build on.
Existing commands must keep answering identically (same words, same replies) — port them onto the
tree in this task or expose a compatibility shim; do not break `/gamemode`, `/give`, `/lookup`,
economy, etc.

Vanilla reference: `com.mojang.brigadier` — `CommandDispatcher`, `LiteralArgumentBuilder`,
`RequiredArgumentBuilder`, `StringReader`, `CommandContext`, `SharedSuggestionProvider`. Re-implement
only the subset needed (no reader/redirect modifiers beyond what vanilla uses commonly).

## Required node kinds
- literal (a fixed word, with aliases)
- `string` (single word / greedy phrase / quoted, mirroring `StringArgumentType`)
- `integer`, `long`, `float`, `double`, `bool`
- `entity` / `entities` (feeds CMD-06), `block_pos` / `vec3` / `vec2` (coordinates), `block_state`,
  `item_stack`, `nbt_compound`, `nbt_path`, `component`, `resource_location`, `resource_or_tag`,
  `game_profile`, `message` (greedy), `time`, `gamemode`, `difficulty`, `particle`, `score_holder`,
  `swizzle`, `angle`, `operation`, `rotation`, `uuid`
- `enum` of fixed words

Each node carries: name, type, executable flag, an optional requirement `(source) -> bool`
(the permission check from CMD-01), an executor, and the redirect/child structure. The tree is
built once (lazy static / `OnceLock`) and shared read-only.

## Parsing rules (must match vanilla)
- whitespace-separated tokens; a greedy/phrase argument consumes the remainder.
- quoted strings with `"` and escapes; suggestion context tracks whether a quote is open.
- integer/long overflow → "Invalid integer 'x'"; float/double → "Invalid double 'x'".
- unknown literal / trailing input → standard brigadier errors
  ("Unknown or incomplete command", "Incorrect argument for command").
- `execute`/`return`/`function` chaining is CMD-22/CMD-21; the parser only needs the node kinds.

## Suggestions
- walk the partially-typed token to the deepest matching node; suggest its literal children
  (prefix-filtered) and its argument type's suggestions (entity names, block ids, item ids, …).
- keep suggestions cheap; providers get the read-only world/snapshot, no locks.

## Existing commands
Move the bodies behind executors (or keep the old match as a post-tree fallback during the
transition, but a single command name must not be answered twice). The registry from CMD0b
(`commands/registry.rs`) becomes tree nodes for not-yet-implemented commands.

## Files
- new `crates/aether-server/src/commands/tree.rs` (+ `tree/suggest.rs` if large).
- `crates/aether-server/src/commands/mod.rs`: `dispatch` becomes `tree::execute(...)`.
- `crates/aether-server/src/game/commands.rs`, `economy/commands.rs`: wrappers that register their
  verbs on the tree (bodies unchanged where possible).

## Edge cases to test explicitly
- exact command match, alias match, and case-sensitivity (vanilla is case-sensitive for literals).
- ambiguous prefix (`/t` between `time`, `tp`, `teleport`, `tell`) → suggestions, no execution.
- greedy message with quotes and with a trailing slash.
- type errors produce the vanilla error text, not a panic.
- recursion depth / a tree with 200+ nodes builds once and parses in microseconds (add a bench).
- a node whose requirement fails is *not* suggested and returns "Unknown or incomplete command"
  to unprivileged callers (vanilla hides it), unless the command brief says otherwise.

## Done criteria
- All currently-working commands answer through the tree with byte-identical replies.
- Unit tests per node kind + the edge cases above; a criterion bench for parse/suggest.
- fmt / clippy `-D warnings` / `cargo-locked test`.
