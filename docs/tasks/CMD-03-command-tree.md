# CMD-03 — Brigadier command tree (parse, suggest, dispatch)

Status: 📋 task brief (CMD0) · size: L · tier: core (C)
Needs: `oc/CMD-01`

## Goal
Replace the hand-written `match cmd { … }` dispatchers with a **Brigadier-style command tree**: one
tree of literal and argument nodes, one parser that walks text against it, one suggestion engine
for tab-completion, and one execution path. This is the spine all other CMD tasks build on.
Existing commands must keep answering identically (same words, same replies) — port them onto the
tree in this task. **There must be exactly one dispatcher after this task**: every command that
works today is migrated onto the tree here, and the old name-`match` paths are deleted in the same
task. No parallel old path is allowed — a command name is answered by the tree and nothing else.

Vanilla reference: `com.mojang.brigadier` — `CommandDispatcher`, `LiteralArgumentBuilder`,
`RequiredArgumentBuilder`, `StringReader`, `CommandContext`, `SharedSuggestionProvider`. Re-implement
only the subset needed (no reader/redirect modifiers beyond what vanilla uses commonly).

## Required node kinds
- literal (a fixed word, with aliases)
- `string` (single word / greedy phrase / quoted, mirroring `StringArgumentType`)
- `integer`, `long`, `float`, `double`, `bool`
- `entity` / `entities` (feeds CMD-06b), `block_pos` / `vec3` / `vec2` (coordinates), `block_state`,
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

## Existing commands to migrate (must all work through the tree)

Every command the server answers today becomes a tree node in this task. The bodies may stay in
their modules, but the *dispatch* moves to the tree; the leftover `match cmd` arms and the flat
`COMMANDS` list are removed.

| from | commands |
|---|---|
| `commands/mod.rs` (history) | `/inspect` `/i` `/lookup` `/l` `/rollback` `/rb` `/page` `/pg` `/inv` `/inventory` `/stash` `/audit` `/cache` |
| `game/commands.rs` | `/gamemode` `/gm` `/time` `/give` `/summon` `/killall` `/kill` `/spawn` `/tp` `/block` `/food` `/heal` `/visibility` |
| `economy/commands.rs` | `/balance` `/bal` `/money` `/pay` `/sell` `/market` `/buy` `/listings` `/unlist` `/econ` |
| CMD0b stub registry | every core command in `commands/registry.rs` becomes a tree node that answers `not implemented yet (CMD-<id>)` and enforces its node/op level |

`/help` from CMD0b is replaced by the tree-driven `/help` (CMD-05, a later task); until then it
stays and lists the tree.

The not-yet-implemented commands from `registry.rs` are registered on the tree now so completion and
permissions are correct from day one, even before their CMD task lands.

## Suggestions
- walk the partially-typed token to the deepest matching node; suggest its literal children
  (prefix-filtered) and its argument type's suggestions (entity names, block ids, item ids, …).
- keep suggestions cheap; providers get the read-only world/snapshot, no locks.

## Files
- new `crates/aether-server/src/commands/tree.rs` (+ `tree/suggest.rs` if large).
- `crates/aether-server/src/commands/mod.rs`: `dispatch` becomes `tree::execute(...)`; the old
  `match` and `usage()` fallback go away.
- `crates/aether-server/src/game/commands.rs`, `economy/commands.rs`: bodies wrapped as executors,
  registered on the tree (bodies unchanged where possible).
- `crates/aether-server/src/protocol/commands.rs`: the flat `COMMANDS` `const` is deleted; the
  Declare Commands packet (CMD-04) is generated from the tree instead.

## Edge cases to test explicitly
- exact command match, alias match, and case-sensitivity (vanilla is case-sensitive for literals).
- ambiguous prefix (`/t` between `time`, `tp`, `teleport`, `tell`) → suggestions, no execution.
- greedy message with quotes and with a trailing slash.
- type errors produce the vanilla error text, not a panic.
- recursion depth / a tree with 200+ nodes builds once and parses in microseconds (add a bench).
- a node whose requirement fails is *not* suggested and returns "Unknown or incomplete command"
  to unprivileged callers (vanilla hides it), unless the command brief says otherwise.
- **no old path remains:** a test asserts `commands/mod.rs`, `game/commands.rs` and
  `economy/commands.rs` contain no `match` on the command name and no second dispatcher.

## Migration tests (one per migrated command, must still pass)

For **every** name in the table above, a test that runs it through `tree::execute` and asserts the
reply is byte-identical to the previous behaviour:

- **history:** `/inspect` toggles and toggles back; `/lookup`/`/rollback` with a filter return the
  same lines; `/page` before a lookup says "nothing to page"; `/inv`, `/stash`, `/audit`, `/cache`
  produce their existing output; aliases `/i /l /rb /pg` route identically.
- **game:** `/gamemode creative` on an op works and a non-op is denied; `/gm s`; `/time set day`
  and `/time` query; `/give stone 3`; `/summon`; `/killall`; `/kill` self; `/spawn`; `/tp x y z`
  and `/tp player`; `/block x y z`; `/food`; `/heal`; `/visibility` toggles.
- **economy:** with the economy disabled the existing "economy is not enabled" reply is unchanged;
  `/balance`, `/bal`, `/money`, `/listings` route identically; `/econ` permission-gated.
- **aliases and permission parity:** each alias maps to the same node; the op-level gate is
  enforced by the tree, not by the body.

## Done criteria
- All currently-working commands answer through the tree with byte-identical replies; the migration
  tests above pass; no second dispatcher remains.
- Unit tests per node kind + the edge cases above; a criterion bench for parse/suggest.
- fmt / clippy `-D warnings` / `cargo-locked test`.
