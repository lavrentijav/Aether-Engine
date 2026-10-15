# Plugins: core / native / WASM tiers, low-level API, economy as a plugin, custom enchantments

Status: 📋 design (2026-10-14). ROADMAP Phase 4 "Plugin API: Native C-ABI tier + WebAssembly
(Wasmtime) sandbox + `engine.supports()`" — this document is that plan, pulled forward.

## Goals

1. A plugin system with a **low-level adapter API** in the spirit of Fabric API: plugins hook
   engine events and registries; they do not patch the engine.
2. **Economy moves out** of the server into a WASM plugin behind a stable interface. The server keeps
   only the interface; any economy (the reference one, a shop plugin, a bridge to a web shop) plugs in.
3. **Enchantments become data**: vanilla ones from the data pack, level caps configurable or off,
   custom enchantments defined by config files or plugins.

Non-goals now: the native **C-ABI** tier for third parties (later; same WIT world, different
loader — distinct from the in-workspace native tier below, which is plain Rust), hot reload
(later), client-side mods.

## Tiers: core, native, WASM

There are three places a feature can live. A feature moves **out** of the core whenever it is
optional, i.e. whenever a vanilla-like server would run without it.

| tier | where | written as | for |
|---|---|---|---|
| **C — core** | `crates/` | engine code | only what **every** server needs **and** hot-path mechanisms **with no policy** |
| **N — native plugin** | `plugins/<name>/` (Rust crate in this workspace) | a workspace crate linked **in-process**, same plugin API/events as WASM, **no WASM boundary**, full speed | optional but **performance-sensitive or deeply integrated** |
| **W — WASM plugin** | `plugins/<name>/` (`.wasm` component) | a sandboxed Wasmtime component (untrusted third-party style) | optional, **not** performance-critical |

**Core rules.**
- The core is the world model, storage/journal, worldgen, physics, the tick, redstone, light, the
  protocol and compat layer, and the *mechanisms* other tiers use.
- The core keeps **no policy**: a decision that an operator could want to change belongs in a
  plugin. When the core must run code at a hot point (a block break, a command) it calls a fixed
  **hook**; the hook has no default behaviour beyond "allow".
- The core must build and run correctly with **every** plugin disabled. `config.operators` is the
  seed for the built-in `op` group, not a dependency on the permissions plugin.

**Native rules.**
- A native plugin is a normal Rust crate in this workspace under `plugins/<name>/`. It implements
  the same `aether:plugin` world (lifecycle, events, host imports) as a WASM plugin, but it is
  linked into the server binary and called **directly** — no serialization, no sandbox, no epoch
  budget. It may use an internal fast path the WIT boundary cannot express.
- Enabled/disabled in operator config like any plugin; **never required by the core**. A native
  plugin that needs a core hook that does not exist extends the API (event/registry/import), it does
  not patch the engine.
- The **native plugin loader** is a task of its own (`CMD-00N`, on the P1 API): it discovers
  native plugins, runs their `init`/`shutdown`, dispatches the same events with priority ordering,
  and exposes the same services. See `docs/tasks/CMD-00N-native-loader.md`.
- Because they are in-process, native plugins can degrade the whole server; they are trusted code
  the operator chooses to build (or install from the same source), not arbitrary third-party code.

**WASM rules.**
- Untrusted or third-party-style code, sandboxed by Wasmtime (epoch/fuel/memory limits, capability
  checks). Slower: pay the boundary and the sandbox.
- Use for optional, non-hot-path features (economy, shops, chat formatting, social, fun,
  integrations, stats).

**Choosing:**
- *Does every server need it, or is it a hot-path mechanism with no policy?* → **core**.
- *Is it optional but performance-sensitive or deeply integrated with engine data?* → **native**.
- *Is it optional, not performance-critical, and safe to sandbox?* → **WASM**.

## Layering

```
core (crates/aether-server, …)      hot path + mechanisms + the event bus
   │  hooks: before-*/after-* events, registries, host imports
native plugins (plugins/<name>/)    Rust crates linked in-process, same API, full speed
   │  (no boundary)
WASM plugins (plugins/<name>/*.wasm) sandboxed, untrusted, WIT: aether:plugin/*@0.1
   │
crates/aether-plugin                 host: loader (native + wasmtime), manifest, event bus,
   │                                 registries, capability checks, limits
crates/aether-server                 calls the bus at fixed hook points; services (Economy, …)
                                     resolve to a plugin implementation or a built-in one
```

## The WIT API (`wit/aether-plugin.wit`, package `aether:plugin@0.1.0`)

**Lifecycle.** A plugin exports `init(config: string) -> result<manifest-ack, string>` and
`shutdown()`. Registration (commands, enchantments, items, services) happens only inside `init`.

**Events — the Fabric-like core.** One export per event family, called by the host on the tick
thread at a safe point:
- `before-*` events return `allow | cancel(reason)` (e.g. `before-block-break`, `before-block-place`,
  `before-use-item`, `before-attack`, `before-chat`, `before-container-click`);
- `after-*` events are observe-only (`after-player-join/leave`, `after-block-break`,
  `after-entity-death`, `after-damage`, `after-craft`, `after-enchant`).
- `on-tick(n)` only if the plugin subscribed, at most every N ticks (manifest).
- `on-command(player, name, args) -> command-result` for commands it registered.
A plugin subscribes in its manifest; unsubscribed events cost nothing (no call).

**Host imports (what a plugin may call), each behind a capability:**
- `world.read`: block state / biome / light at a position (loaded chunks only).
- `world.write`: queue block changes / item drops / effects; applied by the host at the safe point,
  journalled with actor `plugin:<id>` (plugins never mutate state directly → determinism, audit).
- `players`: online list, send message/title/actionbar, give/take items, read inventory.
- `scheduler`: run a callback after N ticks.
- `storage.kv`: per-plugin namespaced KV (persisted in the world store under `PL/<id>/…`).
- `storage.ledger`: **transactional** KV for money-grade state: `transaction(ops)` with
  compare-and-set / atomic multi-key apply, serializable; backed by Postgres when configured, else the
  world KV store. This replaces `SELECT … FOR UPDATE` that a sandboxed plugin cannot do.
- `log`, `config`, `supports(feature) -> bool` (capability/feature probing, versioned names like
  `economy.v1`, `enchantments.custom.v1`).

**Limits (sandbox).** Per call: wasmtime epoch interruption (default 5 ms budget, configurable) and
fuel for `init`; per plugin: memory cap (default 64 MiB), no WASI sockets/filesystem unless the
manifest requests and the operator grants them. A trap or overrun disables that call's result
(treated as `allow` for `before-*`), logs, and after K faults disables the plugin.

**Manifest** `plugin.toml`: id, version, api version (`0.1`), entry `.wasm`, requested capabilities,
event subscriptions, priority (ordering among plugins for the same event), provided services.
Operator config `[plugins] dir`, `[plugins.<id>] enabled`, granted capabilities, per-plugin config.

## Services: economy as the first one

- WIT interface `aether:plugin/economy@0.1` mirrors today's `trait Economy`
  (`crates/aether-server/src/economy/mod.rs`: balance, pay, mint, list, buy, cancel, market,
  listings-of) with the same error enum (`TradeError`).
- Host adapter `WasmEconomy: Economy` — the rest of the server (commands, trades) is unchanged.
- `plugins/economy` (Rust → `wasm32-wasip2` component): the reference economy, ported from
  `economy/pg.rs` onto `storage.ledger`. Integrity rules stay: one transaction per trade, balance
  never negative, listing count and both balances move together or not at all.
- **Conformance suite**: one test suite written against `trait Economy` and run against both the
  built-in Postgres implementation and the WASM plugin (with a memory ledger). The built-in stays as
  fallback until the plugin passes it; then the built-in is removed or kept behind a feature.
- `[economy] provider = "plugin:economy" | "builtin"`.

## Enchantments: data-driven, uncapped, extensible

- `EnchantmentRegistry` loaded from the data pack's `enchantment/*.json` (1.21 enchantments are data:
  max level, weight, supported items tag, exclusive set, costs, effects).
- Effects the engine implements are a fixed set of **effect kinds** (damage bonus, protection,
  mining speed, durability, loot bonus, silk touch, knockback, fire aspect, …); a definition is a
  combination of kinds + level curves. Plugins can add effect behaviour through events
  (`after-damage`, `before-block-break`…) keyed by their enchantment id.
- Config:
  ```toml
  [enchantments]
  level_cap = "vanilla"     # "vanilla" | "none" | <number> ; hard ceiling 255 (wire format)
  anvil_too_expensive = 40  # number | "off"
  allow_incompatible = false
  custom_dir = "enchantments"   # extra JSON definitions, same schema as the data pack
  ```
- Custom enchantments: from `custom_dir` or registered by a plugin in `init`.
- Client display: 1.21+ clients receive enchantments as a synced registry → custom enchantments show
  their real name and level; older clients get a lore line through the compat layer.

## Stages (agents)

- **E1** (with the enchanting work, queue T2): registry from the data pack, effect kinds, config
  caps, custom JSON dir, registry sync to 1.21+ clients; plugin hook points left as no-op calls.
- **P1**: `crates/aether-plugin` — wasmtime embed, WIT package, manifest/loader, limits, event bus
  with the events above wired at their hook points in `aether-server`, `storage.kv`, `log`,
  `supports`; `plugins/examples/hello` (registers a command, cancels breaking bedrock, logs joins);
  tests: load, event order by priority, cancel, trap isolation, time budget.
- **P1-N (CMD-00N)**: the **native** plugin loader — the same API/events/bus as P1 but for Rust
  crates linked in-process (no wasmtime, no boundary), enabled/disabled by config. This unblocks the
  native tier (permissions, regions/claims, WorldEdit, history, profiler, moderation, essentials).
- **P2**: economy service: WIT economy interface, `storage.ledger`, `WasmEconomy` adapter,
  conformance suite, `plugins/economy` port; switch by config.
- **P3**: registries for plugins: custom enchantments and custom items (shown via compat stand-ins),
  more events; plugin developer guide `docs/plugins/README.md`.

## Risks

- wasmtime is a large dependency (build time, RAM); keep it in its own crate and behind a cargo
  feature `plugins` (default on for the server binary, off for tests that do not need it).
- Event calls on the tick thread: budget per tick across all plugins; a slow plugin must not stall
  the tick (epoch interruption + fault counter).
- API stability: version the WIT package; `supports()` for optional features; never break 0.x
  without bumping the package version.
