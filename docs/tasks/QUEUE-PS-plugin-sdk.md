# PS1 — plugin SDK: one-call helpers for common patterns (design + implementation)

Read `docs/tasks/COMMON.md`, `docs/design/plugins.md`, `docs/design/commands.md`, the P1 host on this
branch (`crates/aether-plugin*`, WIT files), and `docs/tasks/CMD-OVERVIEW.ru.md` on `oc/CMD0` (tiers
C / N native / W wasm). Base: branch `oc/P1` (the host is not on main yet).

## Goal
Plugin authors (native and WASM alike) write the same patterns again and again. Put them behind one
call each in a thin SDK layer over the raw host API, so a typical plugin is a few lines. Owner's
example: permissions — `check(player, "node")`, `grant/revoke`, `level(player)`, `has_level(player, 2)`,
add a level/group — one function each, not a hand-rolled store lookup.

## Step 1 — survey (commit `docs/design/plugin-sdk.md`)
Go through the command catalogue and the 15 plugin stubs (`plugins/*/README.md` on oc/CMD0) and list
the repeated patterns. Expected at least:
- permissions: check / require (auto "no permission" reply) / grant / revoke / levels / groups / meta
- commands: register with args + permission + tab-complete in one builder; subcommands; usage text
- messages: send with colour/format codes, translatable keys, action bar, title, broadcast, prefix
- players: by name/uuid, online list, offline lookup, teleport (safe), give item, inventory ops
- config: typed config file with defaults and reload; per-plugin data dir; simple KV/persistent store
- scheduler: run later / repeat / cancel, async with result back on the main tick
- events: subscribe with a closure, cancel, priority; cooldowns; confirm-prompts ("type again to confirm")
- economy / regions access through their services if present (optional dependency pattern)
For each: the one-call signature, which raw host calls it wraps, cost (no hidden per-tick allocations),
what is native-only vs shared. Decide what belongs in the WIT (needs the host) vs pure SDK sugar.

## Step 2 — implement
- `crates/aether-plugin-sdk` (Rust, usable from native plugins and compiled into WASM guests): the
  helpers + a `prelude`. Missing host functions go into the WIT / host (keep capability checks:
  e.g. grant needs the permissions-admin capability).
- Port one native and one WASM example plugin to it and show the line count before/after in the doc.
- Tests for every helper (host side mocked); clippy/fmt clean; `cargo-locked test`.

Commit per step (PS1a design, PS1b permissions+commands, PS1c messages/players/config, PS1d
scheduler/events/cooldowns, PS1e examples+docs). EN+RU docs updated.
