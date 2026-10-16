# Plugin developer guide

This is the practical companion to [`docs/design/plugins.md`](../design/plugins.md)
(the tier model) and [`docs/design/plugin-sdk.md`](../design/plugin-sdk.md) (the
shared SDK services). It shows how to actually write a plugin and where it goes.

## Which tier?

| tier | where | written as | for |
|---|---|---|---|
| **C — core** | `crates/` | engine code | what **every** server needs, and hot-path mechanisms with no policy |
| **N — native plugin** | `plugins/<name>/` | a Rust crate linked **in-process** | optional but **performance-sensitive or deeply integrated** |
| **W — WASM plugin** | `plugins/<name>/` | a sandboxed `wasm32-wasip2` component | optional, **not** performance-critical, safe to sandbox |

Rules of thumb (from `CONTRIBUTING.md`):

- Would a vanilla-like server run without it? Yes → plugin.
- Optional but performance-sensitive or deeply integrated? → **native**.
- Optional, not performance-critical, safe to sandbox? → **WASM**.
- On the per-tick hot path, or needed by every server? → **core**, and the
  decision lives in a plugin behind a hook.
- Need data the API does not expose? Extend the API (event, registry, host
  import), do not patch the engine.

A plugin may never patch the engine, and the core must build and run with the
whole native tier absent or every plugin disabled.

## The two tiers share one contract

A native plugin and a WASM plugin implement the **same** world: `init` /
`shutdown`, the `before-*` / `after-*` events, `on-tick`, `on-command` and the
host imports. The difference is only how the host reaches the code — directly,
or across the Wasmtime boundary — so an author sees one API and an operator sees
one config:

- same `plugin.toml` metadata (id, version, api, priority, capabilities,
  `subscribe`, `requires`, commands);
- same event ordering: **priority descending, then id** — a total order across
  **both** tiers, so a native region plugin and a WASM plugin interleave;
- same enable/disable rule: `[plugins.<id>] enabled` in the server config;
  disabled means its events are not called and its services do not resolve;
- same services: a native plugin provides/consumes through the one service
  registry; a native → WASM call crosses the boundary, native → native is a
  direct Rust call;
- same host imports, each behind a capability.

**Trust.** Native plugins are in-process and trusted-by-install: they can block
or bring the server down, with no epoch/fuel/memory sandbox around them. That is
the price of full speed. WASM plugins are untrusted third-party-style code and
are sandboxed. Install native plugins deliberately.

**Selecting native plugins.** There is no `[plugins.native] dir`. A native
plugin is chosen at **build** time — the server links the crates behind its
`plugins` cargo feature — and enabled at runtime by `[plugins.<id>] enabled`,
exactly like a WASM plugin. `[plugins] dir` is the master switch for the whole
plugin subsystem and the scan path for WASM `plugin.toml` files; when it is
empty the subsystem (native included) is off.

## Writing a native plugin

The reference is
[`plugins/examples/hello-native`](../../plugins/examples/hello-native); copy it.

### 1. The crate

Create `plugins/<name>/` as a normal library crate with `aether-plugin` as a
dependency, and add it to the root `Cargo.toml` `[workspace] members`:

```toml
[package]
name = "aether-<name>-plugin"
# ...
[lib]
name = "aether_<name>_plugin"
[dependencies]
aether-plugin = { workspace = true }
```

Unlike the WASM examples, a native plugin is a workspace member: it is compiled
into the server, so it builds and is linted with the rest of the tree.

### 2. Implement `NativePlugin`

```rust
use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

#[derive(Default)]
pub struct MyPlugin;

impl NativePlugin for MyPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Registration (services, load-time state) happens only here. `ctx`
        // carries the id, the operator's granted capabilities and config, the
        // shared service registry and the host backend.
        ctx.log(LogLevel::Info, "my plugin ready");
        Ok(())
    }

    fn before_block_break(
        &mut self, _ctx: &mut NativeContext, _p: &Player,
        _x: i32, _y: i32, _z: i32, block: &str,
    ) -> Decision {
        if block.ends_with("bedrock") {
            Decision::Cancel("Bedrock is indestructible.".to_string())
        } else {
            Decision::Allow
        }
    }
}
```

Every method has a neutral default: a `before-*` returns `Decision::Allow`, an
`after-*` and `on-tick` do nothing, `on-command` returns `None` (fall through).
Override only what you subscribe to. The manifest decides which events the host
calls, so an unsubscribed event costs nothing.

`NativeContext` gives you:

- `id`, `capabilities`, `config` — identity and operator values;
- `services` — the shared `ServiceRegistry` (`ctx.services.register(...)` in
  `init`, `.get` at call time);
- `backend` — the host services: `log`, `storage.kv`, `config`, `supports`,
  command registration, player and world hooks.

### 3. Declare the manifest

```rust
pub fn manifest() -> Manifest {
    Manifest {
        id: "my-plugin".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),          // compiled in; no component file
        name: "My Plugin".to_string(),
        priority: 10,                  // higher runs first; ties by id
        capabilities: Vec::new(),      // trusted and direct; asks for nothing
        subscribe: vec!["before-block-break".to_string(), "on-command".to_string()],
        commands: vec![CommandDecl::Plain("mycmd".to_string())],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),          // services you need: "sdk.permissions@0.1"
    }
}

pub fn registration() -> NativeRegistration {
    NativeRegistration { manifest: manifest(), new: || Box::new(MyPlugin) }
}
```

A native plugin has no runtime `plugin.toml`; the manifest *is* this Rust value,
collected from `registration()` at the server's build time. Keep a `plugin.toml`
next to it only as human-readable documentation if you like (as `hello-native`
does).

### 4. Wire it into the server

In `crates/aether-server/src/plugins.rs`:

1. add the crate as an **optional** dependency behind the `plugins` feature in
   `crates/aether-server/Cargo.toml`;
2. call `install_native_one(host, cfg, grant_sink, aether_<name>_plugin::registration(), "native <name>")`
   from `install_native`, in dependency order (providers before dependents).

`install_native_one` already honours `[plugins.<id>] enabled`, narrows the
capability grant, registers the plugin's commands through the core command
registry (so a core-owned name is **refused**, not shadowed) and logs the load.
A plugin with a `requires` that is not registered is refused with the service
name in the message.

### 5. Services

Declare the services you need in `requires` (`"name@version"`); the host refuses
the load if one is missing. Publish a service in `init`:

```rust
ctx.services.register("myplugin.thing", "0.1", std::sync::Arc::new(handle))?;
```

A dependent resolves it from `ctx.services`. The same registry backs WASM
plugins through the generated SDK imports, so a native provider can serve a WASM
dependent.

## Writing a WASM plugin

A WASM plugin is a `wasm32-wasip2` component exporting the `aether:plugin@0.1`
world. Start from [`plugins/examples/hello`](../../plugins/examples/hello) or
`greet`; they are built with `make` into a `.wasm` component and dropped into
the directory named by `[plugins] dir`, with a `plugin.toml`. The API is
**frozen** at `aether:plugin@0.1.0`: adding an export means a new package
version, never a changed one. See [`docs/design/plugins.md`](../design/plugins.md).

## Testing

- **Native plugin:** add `#[cfg(test)]` tests to the crate (see
  `hello-native`). They build a `PluginHost` with an in-memory `HostBackend`,
  `add_native` the registration, fire events and assert. Or drive the plugin
  directly by calling the `NativePlugin` methods.
- **Cross-tier behaviour** — ordering against a WASM plugin, a cancel crossing
  tiers, enable/disable and service resolution — is covered in
  `crates/aether-plugin/tests/plugin_host.rs`.
- Run the standard checks before pushing: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.

## Reference

- Tier model and WIT API: [`docs/design/plugins.md`](../design/plugins.md)
- Shared SDK services: [`docs/design/plugin-sdk.md`](../design/plugin-sdk.md)
- Menus plugin: [`menus.md`](menus.md)
- Reference native plugin: [`plugins/examples/hello-native`](../../plugins/examples/hello-native)
- Reference WASM plugin: [`plugins/examples/hello`](../../plugins/examples/hello)
