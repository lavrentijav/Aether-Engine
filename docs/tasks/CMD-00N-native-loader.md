# CMD-00N — Native plugin loader (in-process tier)

Status: 📋 task brief (CMD0) · size: M · tier: **native infrastructure (N)** · tier: native plugin (N)
Needs: `oc/P1`

## Goal
Add the **native** plugin tier: optional features written as Rust crates in this workspace under
`plugins/<name>/`, linked **in-process**, sharing the **same** plugin API/events as WASM plugins but
with no Wasmtime boundary and full speed. See `docs/design/plugins.md` "Tiers" and
`CONTRIBUTING.md` "Core or plugin?".

This task builds only the **loader/host side**; the plugins themselves are the native CMD tasks
that `needs: oc/CMD-00N` (permissions store, regions/claims, WorldEdit, history `/co`/`/restore`,
profiler, moderation, essentials-*). Until it lands they cannot run.

## Behaviour
- A native plugin is a crate implementing the same world as a WASM plugin (`init(config) ->
  manifest-ack`, `shutdown`, the `before-*`/`after-*` events, `on-tick`, `on-command`, host imports).
  The loader calls it **directly** — no serialization, no epoch/fuel/memory sandbox, no time budget.
- Manifest: reuse `plugin.toml`/`plugin` metadata as far as sensible. A native crate declares its id,
  version, api, capabilities, event subscriptions, command names and priority the same way. Prefer a
  small Rust trait + `inventory`/link-time registration over a runtime `plugin.toml` lookup where the
  crate can just be linked; **the observable contract (events, ordering, services, enable/disable) is
  identical to WASM** so a plugin author sees one API.
- Enabled/disabled in operator config (`[plugins.<id>] enabled`), exactly like WASM. Disabled means
  its events are not called and its services do not resolve.
- Priority ordering is the same total order (priority desc, then id) across **both** tiers, so a
  native region plugin and a WASM plugin interleave correctly.
- Services: a native plugin can provide/consume services (`economy`, `permissions`, `regions`, …)
  through the same registry as WASM ones; a native → WASM service call crosses the boundary, a
  native → native call is a direct Rust call.
- **Never required by the core.** The server builds and runs with the native tier's crates absent or
  all plugins disabled. A native plugin may not patch the engine; if it needs a hook that is missing
  it extends the API (event/registry/import) like any plugin.
- Trust note: native plugins are in-process and can bring the server down; document that they are
  trusted code the operator chooses to build, unlike sandboxed WASM.

## Files
- `crates/aether-plugin` (P1): a native loader module beside the wasmtime one, sharing the event bus,
  manifest validation, priority ordering and service registry.
- `plugins/` workspace members: allow native plugin crates (a `plugins/<name>/Cargo.toml` that is a
  library crate) to register; the server binary links the ones selected by config.
- config `[plugins]` gains nothing new conceptually; `[plugins.native] dir`/build selection decided
  and documented.

## Edge cases to test explicitly
- a native plugin's events fire in the correct order relative to a WASM plugin (same priority → by
  id); a `before-*` cancel from one is seen by the next.
- disabling a native plugin in config stops its events and un-resolves its service; re-enabling at
  restart restores.
- `init` failure disables that plugin and logs, without taking the server down.
- a native plugin calling a host import and a WASM plugin calling the same import behave the same.
- the server builds and passes all tests with **no** native plugins linked.
- a native `on-command` for a name the core owns is refused (ownership rule) — same as WASM.
- priority ordering is a single total order across tiers (test with a mixed set).

## Done criteria
- Loader + registration + tests above; a `plugins/examples/hello-native` reference plugin.
- `docs/design/plugins.md` and the plugin dev guide describe how to write a native plugin.
- fmt / clippy `-D warnings` / `cargo-locked test`; `docs/STATUS.md` notes the native tier exists.
