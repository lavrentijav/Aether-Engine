# CMD-01 — Permissions core

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: none

## Goal
Introduce the core permission model that every command and hot-path check uses. Today the only
gate is `crate::is_operator(handle)` (name in `config.operators`, see
`crates/aether-server/src/main.rs:392`). Replace that boolean with a small, fast permission
resolver: **nodes** (`aether.command.ban`, `worldguard.flag.pvp`, …), **op levels 0–4**, and a
per-node **default** of `true` / `false` / `op`. Keep `is_operator` as a thin wrapper so existing
call sites (`economy/commands.rs:88`, `game/commands.rs:19`, `session.rs:850`) keep working.
Groups, inheritance and per-world context are CMD-02; this task is the node/value model and the
`check` API only.

## Behaviour (vanilla permission levels)
A player's base level is what the client is told (`session.rs:1038`): 4 when in `config.operators`,
else 0. A command entry (see CMD-03) names an op level; running it requires `level >= entry.op`.
A node resolves in this order:
1. explicit per-player grant/deny (empty for now; CMD-02 fills it),
2. node default (`true` / `false` / `op`),
3. fall back to `level >= entry.op`.

`op` as a node default means "allowed iff the player is an operator (level ≥ 1)". Node names are
lower-case, dot-separated, `[a-z0-9_.-]`; validation rejects empty segments and upper case.
Unknown nodes default to `op` (deny for non-ops), never to `true` — a typo must fail closed.

### Operators config is a seed, not the source of truth
The existing `config.operators` list stays, but only as a **seed**. On startup every entry is
applied as membership of the built-in group `op` (level 4); the permission store is the source of
truth from then on. A later `/op`/`/deop` (CMD-08) and group edits (CMD-02) change the store, not
the config. On restart the seed is reconciled (an entry the store already knows is not duplicated;
removing a name from the config does **not** silently deop — record the chosen rule in the module
docs and test it). `is_operator` becomes `permissions::level(handle) >= 1`; the built-in group `op`
must exist and be non-deletable.

Reference levels (vanilla `CommandSourceStack`): 0 everyone, 1 bypass spawn protection, 2
gamemasters (most gameplay commands), 3 admins (kick/ban/op/whitelist), 4 owners (stop/save/reload).
Levels are stored as `u8` and clamped to `0..=4`.

## Files
- new `crates/aether-server/src/permissions.rs` (or `crates/aether-api` if other crates must read
  it — decide and say so; the server is the only consumer today).
- `crates/aether-server/src/main.rs`: declare the module, keep `is_operator` but express it as
  `permissions::level(handle) >= 1`.
- `crates/aether-server/src/commands/mod.rs`: a `permitted(handle, node, op_level)` helper used by
  the registry fallback (CMD0b stub) and by every new command brief.

## Edge cases to test explicitly
- An op (level 4) passes every op level 0–4; a non-op passes only 0.
- A node with default `true` passes for a non-op; default `false` fails for an op at level 4 too.
- A node with default `op` passes for a level-1+ operator and fails for level 0.
- Unknown / malformed node names fail closed (deny).
- Level clamping: a configured level › 4 behaves as 4; a negative/overflow parse is rejected.
- `is_operator` behaviour is unchanged for the existing economy/game/session call sites.
- **operators seed:** a config entry becomes an `op`-group member on start; the group is level 4;
  the `op` group cannot be deleted; an entry already in the store is not duplicated on restart;
  changing the config does not retroactively deop (per the documented rule).

## Done criteria
- New module with unit tests for the resolution order above.
- No regressions: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -j4 -- -D warnings`,
  `cargo-locked test -p aether-server`.
- `docs/KNOWN_ISSUES.md` / `docs/STATUS.md` note the permission core exists.
