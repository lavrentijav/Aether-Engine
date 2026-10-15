# Plugin SDK — native service plugin + thin guest client (design)

Status: 📋 design (PS1a, 2026-10-15). Step 1 of [`QUEUE-PS-plugin-sdk.md`](../tasks/QUEUE-PS-plugin-sdk.md).
It surveys the repeated patterns in the command catalogue (`docs/design/commands.md` on `oc/CMD0`),
the 15 plugin stubs (`plugins/*/README.md` on `oc/CMD0`) and the P1 host on this branch, then fixes
the shape of the SDK before any code is written. Step 2 implements it.

Base: `oc/PS1`, built on `oc/P1` (the WASM plugin host is not on `main` yet). The SDK sits on top of
P1's raw API. Parts of the survey that need core APIs not yet landed (permissions CMD-01/02, regions
CMD-29, bulk-edit CMD-30, teleport CMD-31, player/UI CMD-32, economy P2) are recorded as
dependencies and are reached through versioned services, never assumed present.

## 1. Owner decision: the SDK is a native plugin, not a library

The SDK is **not** a guest-side helper library that every plugin statically links. It is a **native
plugin**, `plugins/sdk`, in the sense of the "N" tier in `docs/tasks/CMD-OVERVIEW.ru.md` (tiers
**C** core / **N** native / **W** wasm). It runs **in-process** and exposes its helpers as
**services** through the plugin API. A dependent plugin — native or WASM — declares that it needs
the SDK service and then gets every helper with **one call over one boundary crossing**.

Consequences, which the rest of this document is structured around:

- **The runtime logic lives in the SDK plugin**, not in a client crate: the permission facade, the
  command builder and its registry, message formatting and translation, the scheduler's timer
  wheel, cooldowns, confirm prompts, typed config/KV. Each is a service the plugin owns and can
  keep state for (which is exactly why it is a plugin and not sugar: timers, confirmations and
  per-dependent cooldowns must outlive a single call and be driven by a tick).
- **`crates/aether-plugin-sdk` is a thin typed client for guests only** — generated WIT bindings, a
  prelude and the shared world declaration. No formatting, parsing, registry or scheduling logic;
  those crossed into the native plugin. ("No logic": the crate re-exports what the host provides.)
- **Native dependents call the same services in-process.** They resolve the SDK service from the
  host's service registry and call a typed Rust trait object; no linkage to the SDK crate, so the
  SDK stays disableable.
- **Core stays unaware of the SDK.** No game or server-core subsystem references it. The plugin
  *host* binds service interfaces (that is its job as the ABI), but nothing in the engine's
  gameplay, storage or protocol layers depends on the SDK.
- **It can be disabled like any plugin.** With `[plugins.sdk] enabled = false` the services are not
  registered; every dependent that declared a requirement **fails to load with a clear error**
  (see §5) rather than half-working.

## 2. Tiers and what exists today (P1)

The host is `crates/aether-plugin`. Today it ships one tier:

- **W (wasm).** A `wasm32-wasip2` component exporting the world
  `aether:plugin/plugin@0.1.0` (`crates/aether-plugin/wit/aether-plugin.wit`). Four capability-gated
  host imports (`log`, `storage.kv`, `config`, `supports`). The **export set is frozen**
  (`tests/wit_world.rs`): `init`/`shutdown`; six `before-*` → `decision`; seven `after-*`;
  `on-tick(n)`; `on-command(p, name, args) -> command-result { handled, lines }`. A `plugin.toml`
  declares `capabilities`, `subscribe`, `commands`, `priority`, `tick_period`, `deny_on_fault`. The
  host orders plugins by priority and calls only subscribed events.

Two facts drive the design:

1. **Imports are additive; exports are not.** A component *requests* imports; the host linker
   satisfying a superset is fine, so adding host imports (and therefore new service interfaces the
   host provides) does not refuse components already built against `@0.1.0`. Adding or changing an
   *export*, or a type an export uses, changes the world and refuses them. The checked-in
   `hello.wasm` fixture is the regression guard. Anything that needs a new guest callback or a
   changed export type goes to a new package version (`aether:plugin@0.2.0`).
2. **The SDK's own logic is native, so it does not widen the frozen world at all.** Only the thin
   guest *client* imports new interfaces, and only dependents that opt in.

What the **native (N) tier** needs, and P1 does not yet have: a `NativePlugin` trait, a
`NativeContext` with the shared **service registry**, and wiring so native plugins sit on the same
event bus, with the same priority/manifest/enable semantics as WASM ones. PS1 introduces this
minimal in-process native tier because the SDK plugin requires it. It is deliberately *not* the
native C-ABI tier from [`plugins.md`](plugins.md) (still "later"); it is in-process Rust.

## 3. Architecture

```
 core (game, storage, protocol)          ← no reference to the SDK
   │  native Rust APIs
   ├──────────────┐
   │              │
 aether-server ──► plugin host (crates/aether-plugin)
                    ├─ W wasm plugins   (wasmtime, frozen world)
                    ├─ N  native plugins (in-process Rust: plugins/sdk, natives)
                    └─ service registry (name@version → provider)
                            ▲            ▲
       native dependent ─────┘            └───── WASM dependent
       (typed Rust call)                 (one WIT call: guest → host → SDK)

 plugins/sdk  (native plugin, tier N)
   services: permissions, commands, messages, players,
             config, scheduler, cooldowns, confirm, services
 crates/aether-plugin-sdk  (thin guest client)
   generated bindings + prelude + `dependent` world; no logic
```

- **Native tier.** `plugin.toml` gains `tier = "native"` (default `"wasm"`). A native plugin is a
  Rust type implementing `NativePlugin`, included at server build time and registered with the host
  under its manifest id. It goes through the same enable/priority/subscribe logic; the host calls
  its Rust event hooks directly instead of a wasmtime export.
- **Service registry.** The host owns `ServiceRegistry: (name, version) → Arc<dyn Any + Send + Sync>`.
  A provider registers in `init`; a consumer resolves in `init`. The registry is shared by both
  tiers, so a native provider serves WASM consumers and vice versa.
- **SDK plugin.** `plugins/sdk` implements `NativePlugin`; in `init` it registers each service. It
  keeps the state the helpers need (timer wheel, confirmations, cooldowns, command registry,
  message tables) and drives it from its own `on-tick`. It subscribes to no gameplay events; it is
  a service host.
- **WASM dependent.** Its world imports the SDK service interfaces (via the thin client's
  `dependent` world, §5). Each helper call is one component-model crossing: guest → host function →
  the host reads the caller id from `StoreData` → the host calls the registered native SDK provider
  in-process. One crossing total, no second hop.
- **Native dependent.** Resolves `ctx.service::<dyn SdkService>("sdk", "0.1")` in `init` and calls
  it directly. One in-process call; a services lookup at load.

## 4. The service interface (WIT)

Services live in their own package, versioned independently of the frozen `plugin` world:
`aether:sdk@0.1`. It contains one interface per area, so a dependent imports only what it uses:

```wit
package aether:sdk@0.1;

interface permissions {
    check: func(p: player, node: string) -> bool;
    require: func(p: player, node: string) -> bool;   // check + "no permission" reply
    level: func(p: player) -> u8;
    has-level: func(p: player, level: u8) -> bool;
    groups: func(p: player) -> list<string>;
    in-group: func(p: player, group: string) -> bool;
    meta: func(p: player, key: string) -> option<string>;
    grant: func(subject: subject, node: string, value: bool) -> result<_, string>; // cap permissions.admin
    revoke: func(subject: subject, node: string) -> result<_, string>;
    add-group: func(subject: subject, group: string) -> result<_, string>;
    remove-group: func(subject: subject, group: string) -> result<_, string>;
    create-group: func(group: string, parent: option<string>) -> result<_, string>;
    delete-group: func(group: string) -> result<_, string>;
}

interface commands {
    register: func(spec: command-spec) -> result<_, string>;
    parse: func(name: string, args: list<string>) -> invocation;
    usage: func(name: string) -> string;
}

interface messages {
    send: func(p: player, text: string);
    send-key: func(p: player, key: string, args: list<(string, string)>);
    actionbar: func(p: player, text: string);
    title: func(p: player, title: string, subtitle: option<string>, times: title-times);
    broadcast: func(text: string);
}

interface players {
    online: func() -> list<player>;
    by-name: func(name: string) -> option<player>;
    by-uuid: func(uuid: u64, high: u64) -> option<player>;
    teleport: func(p: player, to: pos, cause: teleport-cause) -> result<_, string>;
    give: func(p: player, item: string, count: u32) -> u64;
}

interface config {
    value: func(key: string) -> option<string>;
    revision: func() -> u64;
}

interface scheduler {
    after: func(ticks: u64) -> timer-id;
    every: func(period: u64) -> timer-id;
    cancel: func(id: timer-id) -> bool;
}

interface cooldowns {
    ready: func(key: string, ticks: u64) -> bool;
    remaining: func(key: string) -> u64;
}

interface confirm {
    request: func(p: player, prompt: string, command: string) -> bool;
}

interface services {                    // optional dependencies
    lookup: func(name: string, version: string) -> bool;
}
```

- **Caller identity is injected by the host.** A guest does not pass its own plugin id: the host
  function reads `StoreData.plugin` (already present in P1) and forwards `(caller, args)` to the
  provider. The SDK keys timers, confirmations and cooldowns by that id, so disabling a dependent
  drops its state, and capability checks are per-caller.
- **`scheduler.after/every` need the dependent's `on-tick` to fire.** The SDK cannot add a
  subscription at runtime; a dependent that uses the scheduler declares `on-tick` in its manifest
  and the SDK's thin client dispatches due timers when the host calls `on-tick`. (Documented
  requirement, checked by the `depends!` macro where possible.)
- **Versioning.** A breaking change to a service bumps `aether:sdk@0.2`; a dependent names the
  version it built against. The host can hold several versions at once, exactly as
  [`plugins.md`](plugins.md) prescribes for plugin package versions. The frozen
  `aether:plugin@0.1.0` world is untouched.

## 5. How dependents declare the dependency

Three coordinated declarations, checked at load:

1. **Manifest.** `plugin.toml` gains `requires = ["sdk@0.1"]` (optionally finer:
   `["sdk.permissions@0.1", "sdk.scheduler@0.1"]`). The host resolves each against the service
   registry **before instantiation / before native `init`**. A missing service is a load error that
   names the plugin, the service and the likely cause:
   `plugin "economy" requires service "sdk@0.1", which is not registered (is the SDK plugin enabled?).
   refusing to load.` This is what makes "disable the SDK → dependents fail with a clear error"
   true, rather than dependents trapping at first call.
2. **Native: `NativePlugin::requires()`** returns the same `ServiceRef`s; the host checks them
   identically.
3. **WASM: the `dependent` world.** A guest that wants SDK services cannot use the host's frozen
   `plugin` world unchanged, because a component can only call interfaces it imports. The thin
   client ships a small `dependent` world that `include`s the host's `plugin` world and adds the SDK
   imports:

   ```wit
   package aether:sdk-client@0.1;
   world dependent {
       include aether:plugin/plugin@0.1.0;   // the frozen exports/imports
       import aether:sdk/permissions@0.1;
       import aether:sdk/commands@0.1;
       // ...only the interfaces the plugin uses
   }
   ```

   The guest generates bindings against `world: "dependent"`; the host linker always provides the
   SDK interfaces (returning an "unavailable" error if no provider is registered), so adding them
   refuses nobody and a non-dependent still loads unchanged. The `depends!("sdk")` macro in the thin
   client expands the manifest `requires` and the world imports from one place, so the two cannot
   drift.

**Optional services** (economy, regions) use the same machinery but are **not** declared as
`requires`: a dependent calls `services.lookup("economy", "0.1")`, gets `false`/`None` when absent,
and degrades. Only hard dependencies go in `requires`.

Capability policy is unchanged: a service method that writes (permissions admin) or sends is gated
by the **caller's** capability, enforced by the host before forwarding, and the SDK re-checks its
own invariants (e.g. `grant` needs `permissions.admin`).

## 6. What the native SDK plugin owns — and does not

**Owns (stateful services):**

| service | owns | delegates to |
|---|---|---|
| `permissions` | facade + `require` auto-reply + admin write path | core CMD-01/02 resolver |
| `commands` | command registry, spec validation, usage text, parse | host command tree (CMD-03/04) |
| `messages` | colour codes, translation tables, key lookup | host player sends |
| `players` | ergonomic wrappers | core player/teleport APIs (CMD-31/32) |
| `config` | typed view + revision polling per dependent | host `config` import |
| `scheduler` | timer wheel, run-later/every/cancel, async (native) | own `on-tick` |
| `cooldowns` | per-dependent, per-key last-tick maps | clock |
| `confirm` | pending confirmations + expiry | `scheduler` |
| `services` | optional-service lookup facade | host registry |

**Does not own:** the permission *model* (core CMD-01/02 owns nodes, groups, inheritance,
snapshots); the command *tree and transport* (host/CMD-03/04 owns parsing primitives and the
Declare Commands packet); the player, world, teleport or economy mechanisms (core CMD-29/30/31/32,
P2). The SDK is a **facade and state holder**, never a second implementation. It holds no authority:
it cannot allow what the core denies, and capability checks still apply to its callers.

## 7. Pattern catalogue

The survey below is unchanged from the first pass — these are the repeated patterns across the 15
stubs — but each helper is now a **method on an SDK service**, not a function in a linked library.
"Cost" assumes the call path in §8. "SDK-owned" means the native plugin, "client" means the thin
crate (bindings only).

### 7.1 Permissions — the owner's example

Used by permissions, regions, claims, teleport, chat, moderation, social, shops, worldedit.

```rust
permissions().require(p, "essentials.home");   // check + auto reply; returns bool
permissions().check(p, "worldguard.region.bypass")
permissions().level(p) / permissions().has_level(p, 2)
permissions().groups(p) / permissions().in_group(p, "staff")
permissions().meta(p, "prefix")
permissions().grant(Subject::Player(uuid), "essentials.fly", true)   // cap permissions.admin
permissions().revoke(...) / permissions().add_group(...) / permissions().create_group(...)
```

Wraps the core CMD-01/02 resolver and admin writes. One service call each; `require` adds a message
only on denial. Never cached in the client — a resolver snapshot swap must be visible immediately.
**Dependency:** CMD-01/02 are not on this branch; until they land the service reports
`supports("permissions.v1") == false` and answers deny/`0` (fail closed). The permission *model*
lands once, in CMD-01/02; the SDK only fronts it.

### 7.2 Commands

Every stub registers commands; WorldEdit and essentials-* register a dozen.

```rust
sdk.commands.register(
    CommandSpec::new("home")
        .perm("essentials.home")
        .usage("/home [name]")
        .arg(Arg::word("name").optional().suggest_static(&["home", "base"]))
        .sub(CommandSpec::new("set").arg(Arg::word("name")))
)?;
```

The builder type (`CommandSpec`) is constructed by pure sugar in the thin client; `register` is the
service call that validates and registers the node with the host tree (name, usage, permission,
**static** completions). Dispatch still enters the dependent's frozen `on-command` export; the thin
client's generated dispatch calls `commands.parse(name, args)` (one call) and then the handler, so
permission and usage stay uniform across plugins. Subcommands are nested specs.

**Dynamic tab-completion is the one helper that cannot fit `@0.1.0`.** The host drives completion
from its tree, so the guest must answer a new callback (`on-complete`) or `command-result` must grow
a `completions` field — both break the frozen exports. Until `aether:plugin@0.2.0`, only static
completions are supported; a plugin needing to complete its own state (e.g. `/home <name>`) shows a
usage hint. (Open question §12.)

### 7.3 Messages

```rust
messages().send(p, "&aWelcome, &f{name}!");
messages().send_key(p, "essentials.home.set", &[("name", n)]);
messages().actionbar(p, "&7Warping…");
messages().title(p, "&eHome", Some("&7Teleported"), TitleTimes::new(5, 40, 10));
messages().broadcast("&6Server restarting");
```

Colour translation, the per-plugin translation tables and locale lookup live in the SDK plugin;
each send is one service call. Formatting is pure and allocates one `String` per message (inherent
to sending text). The SDK documents that broadcasting from `on-tick` is discouraged; the tick budget
bounds it anyway.

### 7.4 Players

```rust
players().online() -> Vec<PlayerRef>        // allocates one Vec, host-built
players().by_name("Steve") / players().by_uuid(uuid)
players().teleport(p, Pos::safe(x, y, z), Cause::Plugin)?
players().give(p, "minecraft:diamond", 8) -> u64   // returns leftover
players().inventory(p) -> InventoryView
```

Wraps core player APIs (CMD-31/32). Safe teleport runs CMD-31's search host-side; the SDK does not
reimplement collision. `online()`/`inventory()` allocate and are documented as such.

### 7.5 Config and data

```rust
let cfg: MyConfig = config().typed()?;          // read once in init
if config().changed() { cfg = config().typed()?; }   // O(1) via revision
store().kv::<Homes>("homes", p.uuid).get("base");
```

`config.revision()` makes `changed()` O(1) so polling per tick allocates nothing. Typed config and
typed KV are sugar in the client (serde over the flat string map); the underlying
`config`/`kv-*` remain P1 host imports scoped to the **dependent's own** namespace.

### 7.6 Scheduler

```rust
let id = sched().after(20, |ctx| { ... });
let id = sched().every(600, |ctx| { ... });
sched().cancel(id);
```

This is the clearest justification for a native plugin: the SDK owns one shared timer wheel, keyed
by (dependent id, timer id), drained once per tick from the SDK's own `on-tick`. An idle wheel is
O(1); a due batch is O(due). A dependent's timers are dropped when it is disabled or unloaded.
**Async** (blocking work delivered back on the tick) is native-only for the *dependent*: the SDK can
schedule the result, but the work itself is the dependent's. Network work belongs to PA3, not PS1.

### 7.7 Events, cooldowns, confirm

```rust
plugin.on_before(Event::BlockBreak, 10, |p, e| { ... Verdict::Allow });
plugin.on_after(Event::PlayerJoin, |p| { ... });

cooldown().ready("home", 60) -> bool;              // one service call
confirm().request(p, "delete your claim?", "claim delete")   // schedules expiry
```

Event handler tables live in the **dependent** (generated by the thin client from the frozen
exports); the host already orders plugins by manifest priority, and SDK-local priority orders
handlers within a plugin. Cooldowns and confirmations are SDK-owned state, keyed by caller id;
`confirm` is sugar over the SDK scheduler. Cost: a before/after dispatch is a table walk with an
`is_empty` fast path; an unregistered event costs nothing because the host never calls it.

### 7.8 Optional services: economy, regions

```rust
if let Some(econ) = services().economy() { econ.withdraw(p, Money::major(10))?; }
if services().lookup("regions", "0.1") { /* use typed handle */ }
```

Not `requires`: absence is `None`/`false`, never a trap (the `supports()` philosophy). Lookup is
done once in `init` and cached; a call is one service call.

## 8. Cost per call

| path | crossings | work |
|---|---|---|
| native dependent → SDK service | 0 | direct `Arc<dyn SdkService>` call on the tick thread; no serialization, no lock on the query path |
| WASM dependent → SDK service | **1** | guest → host WIT import; host reads caller id and calls the native provider in-process; primitives/short strings copied once each way |
| service resolution (`init` / load) | 0 | registry hash lookup, once |
| SDK `on-tick` drain | 0 | one walk of the shared timer wheel; O(1) when idle, O(due) otherwise |
| `config.changed()` | 1 | compares a revision integer returned by the host |
| `cooldowns.ready` | 1 | map lookup + subtraction; no allocation |

No hidden per-tick allocations: dispatch is subscription-driven (the host calls only subscribed
events); helpers take borrowed args and return primitives where possible; the only allocations are
explicit collections (`online`, `inventory`) and message strings. Every public helper carries a
one-line "cost" note, and the hot-path ones (empty dispatch, cooldown miss, `changed()` false, idle
scheduler) get allocation tests where practical. P1's shared per-tick budget still caps the worst
case, so a heavy plugin cannot stall the tick regardless.

## 9. The thin guest client (`crates/aether-plugin-sdk`)

Contents, and nothing more:

- `wit/` — the `aether:sdk@0.1` package and the `dependent` world, vendored from the host's copy so
  they cannot drift.
- `src/lib.rs` — `wit_bindgen::generate!({ world: "dependent", path: "wit" })` with the standard
  `pub use` surface.
- `src/prelude.rs` — ergonomic re-exports: `permissions()`, `commands()`, `messages()`, `players()`,
  `config()`, `scheduler()`, `cooldowns()`, `confirm()`, `services()` — thin newtype handles over the
  generated resources with no behaviour beyond forwarding.
- `depends!("sdk")` / `export_plugin!` — macros that (a) expand the manifest `requires` and the
  world imports together, and (b) seed the frozen `Guest` impl, dispatching to handler tables.

Explicitly **not** in the crate: formatting, parsing, registries, schedulers, cooldowns,
translation, config typing, or any state. Those are the native plugin's, reached over the boundary.

## 10. Testing

- **Native SDK plugin unit tests**: the stateful services (timer wheel, cooldowns, confirmations,
  command parse, colour/translation) run in-process against a mock core (permissions resolver,
  player registry). This is where the bulk of the logic is tested.
- **Service interface tests**: a fixture WASM plugin that calls each service; the host test asserts
  forwarding, caller-id injection, capability refusal and neutral values — extending
  `crates/aether-plugin/tests/plugin_host.rs`.
- **Load-time dependency tests**: a dependent with `requires = ["sdk@0.1"]` fails to load with the
  documented error when the SDK is disabled, and loads when it is enabled; a non-dependent loads
  either way.
- **Frozen-world regression**: `tests/wit_world.rs` keeps pinning the `@0.1.0` export set, and the
  checked-in `hello.wasm` must still load after the SDK imports are added. Extend the pin to the
  host's *import* interface names, so an accidental breaking import change is caught.
- **Allocation/cost tests** for the hot-path helpers.
- Checks per `COMMON.md`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4
  -- -D warnings`, `cargo-locked test` for touched crates.

## 11. Implementation plan (step 2)

| commit | scope |
|---|---|
| **PS1a** (this) | survey + design; no code |
| **PS1b** | native tier (`NativePlugin`, `NativeContext`, service registry, bus wiring); `plugins/sdk` skeleton with the `permissions` + `commands` services; thin client crate (bindings + `dependent` world + `depends!`) |
| **PS1c** | `messages`, `players`, `config` services + thin-client prelude |
| **PS1d** | `scheduler`, `cooldowns`, `confirm` (+ native async); `on-tick` drain |
| **PS1e** | port one WASM example (`plugins/examples/hello`) and one native example to the SDK; line-count before/after; EN+RU docs; STATUS/ROADMAP/KNOWN_ISSUES |

PS1b necessarily introduces the minimal native tier, because the SDK requires it. Missing core APIs
(permissions, teleport, players) are stubbed behind `supports()` in the same commit and filled in
when CMD-01/02, CMD-31/32 and P2 land.

## 12. Open questions for review

1. **Native tier scope.** The owner's decision pulls a minimal in-process native tier into PS1b.
   Confirm this is intended now (vs. a follow-up), and that `tier = "native"` + a build-time
   registry is the right shape for "loaded like any plugin" before the C-ABI tier exists.
2. **Service-call mechanism in the host.** Confirm the forwarding design: host binds the
   `aether:sdk@0.1` interfaces and calls the registered provider through a typed bridge, with
   caller-id injected from `StoreData`. (Alternative: a generic dynamic `services.call`, less typed.)
3. **Dynamic tab-completion** waits for `aether:plugin@0.2.0` (`on-complete`); confirm it is out of
   scope for PS1.
4. **Permissions backend.** Confirm the SDK *fronts* the CMD-01/02 model (fail closed until it
   lands) rather than growing its own resolver in PS1.
5. **Native example in PS1e.** With the minimal native tier in PS1b there is now a native loader, so
   the "one native example" is a real native dependent; confirm.
6. **Async scheduler** native-only, and whether it is PS1d or deferred with PA3.

## 13. Risks

- **A native tier is new surface.** Until now only WASM rules applied (sandbox, epoch budget). A
  native plugin is trusted, in-process code: it can crash the server. The SDK must therefore stay
  thin, hold no locks across calls, and never panic on a bad argument; PS1 treats native plugins as
  trusted-by-install, documented in `plugins.md`.
- **The frozen world** still blocks new guest callbacks; the catalogue stays inside it and dynamic
  completion waits for `@0.2.0`.
- **Dependencies not landed** (permissions/regions/bulk/teleport/player/economy): the services exist
  as `supports()`-gated facades first, testable against a mock, honest about absence.
- **Thinness.** The SDK must not grow a second implementation of the game; anything needing real
  state beyond timers/registry belongs in the host or core.
