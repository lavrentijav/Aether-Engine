# Plugin SDK — one-call helpers over the raw host API (survey)

Status: 📋 design (PS1a, 2026-10-15). This is step 1 of
[`QUEUE-PS-plugin-sdk.md`](../tasks/QUEUE-PS-plugin-sdk.md). It surveys the repeated patterns in the
command catalogue (`docs/design/commands.md` on `oc/CMD0`), the 15 plugin stubs
(`plugins/*/README.md` on `oc/CMD0`) and the P1 host on this branch, then fixes the shape of the SDK
before any code is written. Step 2 implements it.

Base: `oc/PS1`, built on `oc/P1` (the WASM plugin host is not on `main` yet). The SDK wraps P1's
raw API; the parts of the survey that need core APIs not yet landed (`permissions` CMD-01/02,
`regions` CMD-29, `bulk-edit` CMD-30, `teleport` CMD-31, `player/UI` CMD-32, `economy` P2) are
marked as dependencies and are designed behind `supports()` probes, not assumed present.

## 1. Goal

Plugin authors — native and WASM alike — write the same handful of patterns in every plugin:
look up a permission, answer a command, send a coloured message, read the config, schedule a task,
remember something between ticks. Today each one is hand-rolled against the raw host functions and
the game's own types. The SDK puts each behind **one call** in a thin layer over the raw API, so a
typical plugin is a few lines. The owner's example is permissions:

```rust
if !sdk::perms().require(p, "essentials.home") { return; }
```

One function each for `check`, `require` (with the "no permission" reply), `grant`/`revoke`,
`level`, `has_level`, level/group management — not a hand-rolled store lookup in every plugin.

Non-goals: a second plugin runtime, a stable ABI for the native tier (still "later" in
[`plugins.md`](plugins.md)), hot reload, and reimplementing core mechanisms in the SDK. The SDK is
*thin*: it holds no game state of its own beyond sugar and small registries, and every helper maps
to an existing (or planned) host call with a known cost.

## 2. What the raw API is today (P1)

The host is `crates/aether-plugin`. A plugin is a `wasm32-wasip2` component exporting the world
`aether:plugin/plugin@0.1.0` (`crates/aether-plugin/wit/aether-plugin.wit`). It may call four host
imports, each capability-gated and refused (logged, returning the neutral value) if not granted:

| import under `host` | capability | returns |
|---|---|---|
| `log(level, message)` | `log` | — |
| `kv-get/kv-put/kv-delete/kv-list` | `storage.kv` | `option<list<u8>>` / `list<string>` |
| `config(key)` | `config` | `option<string>` |
| `supports(feature)` | `supports` | `bool` |

Its **exports are frozen** (`tests/wit_world.rs`): `init`/`shutdown`; six `before-*` returning
`decision`; seven `after-*` returning nothing; `on-tick(n)`; `on-command(p, name, args) ->
command-result { handled, lines }`. A `plugin.toml` manifest declares capabilities, event
`subscribe`, `commands`, `priority`, `tick_period` and `deny_on_fault`. The host orders plugins by
priority and calls only subscribed events.

Two consequences shape the whole design:

1. **Adding imports is backward compatible, adding exports is not.** A component's imports must be
   satisfied but not exhausted: the linker may provide a superset, so new host *imports* (and new
   functions on the existing `host` interface) do not refuse components already built against
   `@0.1.0`. New *exports*, or a change to a type used by an export — e.g. adding a `completions`
   field to `command-result` — do. The checked-in `hello.wasm` fixture, compiled against `@0.1.0`,
   stays loadable in the host tests and is our regression guard for this. Anything needing a new
   guest callback or a changed export type goes into a **new package version** (`aether:plugin@0.2.0`),
   per [`plugins.md`](plugins.md).
2. **The SDK guest half must implement exactly the frozen export set.** So the SDK cannot invent
   `on-scheduled` or `on-complete`; it works by dispatching the frozen events to registered
   handlers, and the host drives everything else.

What the server already offers *behind* the host (native side, `crates/aether-server`): the player
registry (`players.rs`: by name/uuid/entity, snapshot, broadcast), per-player inventory, the
`Economy` trait (`economy/mod.rs`), effects/containers (`game/effects.rs`, `game/containers.rs`),
and an internal tick scheduler (`game/scheduler.rs`). What does **not** exist yet and gates parts of
the SDK: permission nodes/groups (CMD-01/02), the region index and bulk-edit and teleport and
player/UI APIs (CMD-29/30/31/32), and the economy service over WIT (P2). The SDK designs a uniform
surface over all of them and degrades safely when a service is absent.

## 3. Architecture

```
crates/aether-plugin-sdk
  Host trait (the service surface helpers call)
   ├─ guest:  forwards to the generated WIT imports  (feature "guest")
   └─ native: implemented over aether-server APIs      (feature "native")
  + pure sugar: command builder, colour, cooldowns, confirm, arg parse, typed config/KV
```

- **One crate, two backends.** Helpers are generic over a `Host` trait; only the backend impl (and
  the generated bindings) differ between a WASM guest and a native plugin. Sugar — formatting, arg
  parsing, cooldowns, the command spec builder, typed config — is backend-independent and is the
  bulk of the crate, so it is unit-tested on the host target with a mock `Host`.
- **The `Plugin` builder** gives plugin authors one place to register handlers:

  ```rust
  pub fn plugin() -> Plugin;
  Plugin::on_init(|ctx| { ... })                       // runs in init
        .command("home").perm("essentials.home").arg(...).run(|ctx, a| { ... })
        .on_before(Event::BlockBreak, 0, |p, e| { ... })
        .on_after(Event::PlayerJoin, |p| { ... });
  export_plugin!(MY_PLUGIN);                           // seeds the frozen Guest impl
  ```

  The `export_plugin!` macro expands the frozen `Guest` impl, dispatching each export to the
  handlers registered on that one plugin value. Native plugins register the same value with the
  server instead of through `export_plugin!`.
- **Initialisation order is fixed**: `init` builds the registries; the frozen exports never
  register, only dispatch. This mirrors the host's "registration only in `init`".
- **The SDK owns no lock unless it must.** Guest is single-threaded (one `Store` per plugin,
  called on the tick thread), so the registries live in a `static`/`OnceCell`, no mutex. Native
  handlers are called on the tick thread too; any shared state a native plugin keeps is its own.

Crate layout (proposed):

```
src/lib.rs          re-exports, Plugin builder, export_plugin!
src/host.rs         Host trait, Error, Capability, feature constants
src/guest.rs        WIT-backed Host (feature "guest")
src/native.rs       server-backed Host adapter (feature "native")
src/prelude.rs      the prelude
src/command.rs      CommandSpec/Builder, Arg, parse, usage
src/message.rs      colour codes, Formatted, title/actionbar/broadcast
src/permissions.rs  check/require/grant/revoke/level/groups/meta
src/players.rs      lookup, online, teleport, give, inventory
src/config.rs       typed config + defaults + reload
src/store.rs        typed KV and a simple persistent map
src/scheduler.rs    run-after / repeat / cancel, tick-driven
src/events.rs       handler tables, priority, cancel
src/cooldown.rs     per-key cooldowns
src/confirm.rs      "type again to confirm"
src/services.rs     optional economy/regions handles
```

## 4. WIT additions: host vs sugar

Everything that touches game state or another subsystem crosses the host boundary and therefore
needs a WIT import; pure bookkeeping and formatting do not. The proposed additions, all **additive
imports** to `aether:plugin@0.1.0` (safe as argued in §2):

| interface / function | capability | needs | purpose |
|---|---|---|---|
| `host.clock-now-ticks() -> u64` | `clock` | P1 | cooldowns, scheduler, warmups |
| `host.config-revision() -> u64` | `config` | P1 | cheap change detection for reload |
| `permissions.*` | `permissions` / `permissions.admin` | CMD-01/02 | check, level, groups, meta; grant/revoke |
| `players.*` | `players` | P1 + CMD-31/32 | online/lookup, message/title/actionbar, broadcast, teleport, give/take, inventory |
| `world.*` | `world.read` / `world.write` | P1 (planned) | block reads and queued writes |
| `commands.register(spec)` | `commands` | P1 + CMD-03/04 | declare args, usage, permission, **static** completions |
| `storage.ledger` | `storage.ledger` | P2 | money-grade transactional KV |
| `services.lookup(name, version)` | `services` | P3 | economy/regions cross-plugin handle |
| `economy@0.1`, `regions@0.1` | via `services` | P2 / CMD-29 | typed service calls |

Pure SDK sugar (no WIT change): the command arg parser and usage text, colour/format codes,
translatable-key lookup (given a locale string from the host), cooldowns, confirm prompts, the
scheduler's pending queue, typed config defaults, typed KV serialisation, optional-service
`Option` handling, and all the `require`/`has_level`/`in_group`-style conveniences.

**Dynamic tab-completion is the one helper that needs a new package version.** The host drives
completion from its command tree, so the guest must answer a new callback (`on-complete`) or the
export type must grow a `completions` field — both break the frozen export set. Until
`aether:plugin@0.2.0`, the SDK supports only **static** completions declared at registration
(enumerations, online-player names, item/material names) via `commands.register`; a plugin that
needs to complete its own dynamic state (e.g. `/home <name>`) must fall back to a usage hint. This
is recorded as an open question in §10.

## 5. Pattern catalogue

Format per group: the helper, what it wraps, its cost, and where it lives. "Shared" means the code
compiles for both backends and the backend impl only differs in the `Host` method it calls.

### 5.1 Permissions

The owner's motivating example, and the most repeated pattern (permissions, regions, claims,
teleport, chat, moderation, social, shops, worldedit all gate commands and events).

```rust
perms().check(p, "essentials.home") -> bool            // raw: permissions.check(player, node)
perms().require(p, "essentials.home") -> bool          // check + auto "no permission" reply
perms().level(p) -> u8                                  // raw: permissions.level(player)
perms().has_level(p, 2) -> bool                         // sugar: level(p) >= 2
perms().groups(p) -> Vec<String>                        // raw
perms().in_group(p, "staff") -> bool                    // sugar over groups()
perms().meta(p, "prefix") -> Option<String>             // raw (prefix/suffix/weight)
perms().grant(Subject::Player(uuid), "node", true)      // raw, cap permissions.admin
perms().revoke(Subject::Player(uuid), "node")           // raw, cap permissions.admin
perms().add_group(Subject::Player(uuid), "staff")       // raw, cap permissions.admin
perms().create_group("staff") / remove_group("staff")   // raw, cap permissions.admin
```

- **Wraps** the core CMD-01/02 resolver (native) / the proposed `permissions` WIT interface (guest).
  Writes are the admin half of CMD-02; they need `permissions.admin` so a dropped-in plugin cannot
  rewrite rights.
- **Cost**: one host call each, no allocation: the node/group is a `&str`, the host looks it up in
  its immutable snapshot and returns a primitive. `require` adds one message send only on denial.
  Never cached in the SDK — the resolver's snapshot swap must be visible immediately.
- **Shared.** `require`/`has_level`/`in_group` are pure sugar; the rest is backend-abstracted.
- **Dependency**: CMD-01/02 are not on this branch. Until they are, the native backend answers
  deny/`0` and reports `supports("permissions.v1") == false`, so plugins fail closed. Whether PS1
  waits for CMD-01/02 or ships a minimal resolver is an open question (§10) — the SDK itself never
  owns the permission model.

### 5.2 Commands

Every one of the 15 stubs registers commands; several register a dozen (WorldEdit, essentials-*).
The repetition is register-with-args + permission + tab-complete + usage + subcommands.

```rust
plugin.command("home")
    .perm("essentials.home")                 // checked before dispatch
    .usage("/home [name]")
    .arg(Arg::word("name").optional().suggest(homes))   // static completions
    .sub("set", |c| c.arg(Arg::word("name")).run(set_home))
    .run(|ctx, a| { let name = a.str("name").unwrap_or("home"); ... });
```

- **Wraps**: `on-command` dispatch (frozen), the manifest's `commands` list (the host routes only
  declared names), and the proposed `commands.register(spec)` import for the tree/completions.
- **Design**: the builder's `CommandSpec` is stored in the SDK registry; the frozen `on-command`
  export looks the name up, checks `.perm`, parses args, and calls the handler. `permission` is
  enforced by the SDK *and* should be declared to the host so the tree shows it and future hosts can
  check it before the guest is entered.
- **Cost**: dispatch does at most one small `Vec` of arg slices over the incoming `args` (which the
  host already allocated); no per-tick cost, commands are rare. Arg parsing is zero-copy (`&str`).
- **Shared**; `commands.register` is guest-only (native registers with the server directly).
- **Subcommands** are just nested specs; `sub()` builds a child table so `/home set x` routes to the
  `set` handler with the remaining args.

### 5.3 Messages

Chat, titles, action bars, broadcasts, prefixes, colour and translation recur in every stub.

```rust
msg().send(p, "&aWelcome, &f{name}!");                 // raw: players.send-message
msg().send_key(p, "essentials.home.set", &[("name", n)]);  // pure: lookup + format
msg().actionbar(p, "&7Warping…");                      // raw
msg().title(p, "&eHome", "&7Teleported", TitleTimes::new(5, 40, 10));  // raw
msg().broadcast("&6Server restarting");                // raw: players.broadcast
msg().prefix(p) -> String                              // sugar over perms().meta(p,"prefix")
```

- **Wraps** the `players` WIT import for the sends; colour and translation are pure.
- **Colour**: `Colorize` translates `&`/`§` codes to the section-sign form the codec expects and
  strips them for width. It writes into a caller-provided `String` where possible (`write_colored`)
  to avoid a second allocation; the `&str -> String` conversion at the host boundary is the host's
  cost and is unavoidable.
- **Translatable keys**: the SDK keeps a small per-plugin message table (`en_us` built in,
  extra locales loaded from config/data files) and looks a key up with the player's locale obtained
  once per send from the host (`players.locale`). Missing key falls back to the key itself so a
  missing translation is visible, never silent.
- **Cost**: one host call; formatting allocates one `String` per message, which is inherent to
  sending text and is not on the per-tick hot path (unless a plugin broadcasts in `on-tick` — the
  SDK documents that pattern as discouraged and the tick budget already bounds it).
- **Shared** sugar; sends are backend-abstracted.

### 5.4 Players

Lookup and direct manipulation recur in teleport, player, inv, chat, moderation, social, shops, fun.

```rust
players().online() -> Vec<PlayerRef>                    // raw, allocates one Vec
players().by_name("Steve") -> Option<PlayerRef>         // raw
players().by_uuid(uuid) -> Option<PlayerRef>            // raw
players().offline_name(uuid) -> Option<String>          // raw (player store)
players().teleport(p, Pos::safe(x, y, z), Cause::Plugin) -> Result<(), TeleportError>  // CMD-31
players().give(p, "minecraft:diamond", 8) -> u64        // raw: players.give (returns leftover)
players().inventory(p) -> InventoryView                 // raw (read-only borrow/snapshot)
```

- **Wraps** `players` (P1/CMD-32) and `teleport` (CMD-31). `Players::online()` crosses the host
  boundary and allocates the list; every call that returns a collection is documented as such and
  is not called from `on-tick` by well-behaved plugins.
- **Safe teleport** is a single call that runs CMD-31's safe-spot search host-side; the SDK does not
  reimplement collision.
- **Cost**: `by_name`/`by_uuid`/`give`/`teleport` are one call; `online()`/`inventory()` allocate
  (the host builds the list) — unavoidable when the caller asked for a list.
- **Shared**; all are `Host` methods.

### 5.5 Config and data

Typed config, defaults, reload, a per-plugin data dir and a simple KV recur in nearly every stub.

```rust
let cfg: MyConfig = config().typed()?;          // reads all keys in init, typed via serde
config().revision();                            // host.config-revision, cheap
if config().changed() { cfg = config().typed()?; }   // in on-tick, no allocation when unchanged
store().kv::<Homes>("homes", p.uuid).get("base");     // sugar over kv-get/put/delete/list
store().data_dir()                                // per-plugin data dir (P1 config)
```

- **Wraps** `host.config` and `host.kv-*` (both exist in P1); `config-revision` is the only new
  import, needed so `changed()` is O(1) and does not re-read every key per tick. The data dir is a
  host-provided path (config), not a raw filesystem grant — the sandbox stays closed.
- **Typed config** deserialises the flat string map once; `changed()` compares the revision and
  `reload()` re-deserialises, so the common "poll every tick" case allocates nothing.
- **Typed KV** serialises a `T: Serialize` under a namespaced key prefix; keys are the plugin's
  own, the host already namespaces by plugin id. A simple persistent map (`PersistentMap<K, V>`)
  wraps list/get/put/delete for the homes/warps/friends patterns.
- **Cost**: KV reads/writes cross the boundary (bytes); no per-tick hidden work unless the plugin
  chooses to poll storage, which the SDK does not do for it.
- **Shared** sugar over the existing imports.

### 5.6 Scheduler

Run later / repeat / cancel, and "async with the result back on the main tick".

```rust
sched().after(20, |ctx| { ... });          // run once, 20 ticks later
sched().every(600, |ctx| { ... });         // repeat until cancelled
let id = sched().after(40, f); sched().cancel(id);
sched().async_result(|| long_work()).then(|r| { ... });   // native only
```

- **Wraps**: there is no `on-scheduled` export (frozen), so the SDK's scheduler is **tick-driven
  sugar**: it keeps a small ordered queue of `(due_tick, id, FnOnce)` drained by the SDK's
  `on-tick` dispatch, which every plugin using the scheduler must therefore subscribe to. The
  plugin's manifest declares `on-tick`; the SDK cannot add it at runtime. `now-ticks()` (new
  import) is the clock; the SDK caches the last tick seen so `before-*` handlers can also test
  cooldowns.
- **Async** (a blocking result delivered back on the tick) is **native-only**: the native backend
  runs the work on a thread/task and re-queues a closure for the next tick. A WASM guest has no
  threads and its std is sandboxed; async network work belongs to the net capability (PA3) and is
  not part of this SDK step.
- **Cost**: the queue is a `BTreeMap<u64, Vec<...>>`; draining once per tick walks only due
  entries, so an idle scheduler is O(1) and allocates nothing. Handlers are `FnOnce` boxed once at
  registration.
- **Shared** for timers; **native-only** for `async_result`.

### 5.7 Events, cooldowns, confirm

Event hooks with a closure, cancel and priority recur in regions, claims, chat, moderation,
teleport, shops, fun.

```rust
plugin.on_before(Event::BlockBreak, 10, |p, e| {
    if !perms().check(p, "worldedit.build") { return Verdict::Cancel("no build perms") }
    Verdict::Allow
});
plugin.on_after(Event::PlayerJoin, |p| { ... });

cooldown().ready(p, "home", Duration::ticks(60)) -> bool   // sugar over clock
confirm().request(p, "delete your claim?", |p| { ... })     // sugar over scheduler
```

- **Design**: `on_before`/`on_after` register into per-kind ordered tables. The frozen exports
  dispatch to those tables; `on_before` handlers run in **SDK-local priority order** (the host
  already orders plugins by manifest `priority`), and the first `Cancel` short-circuits. `on_after`
  is observe-only. The seven `after-*` and six `before-*` kinds are enum variants, so a handler is
  a closure, not a hand-written export.
- **Cooldowns** are pure: a per-key last-tick map, tested against the cached `now-ticks()`. No host
  call on the query path.
- **Confirm** is pure sugar over the scheduler: it records a pending confirmation with an expiry
  tick and resolves it when the player retypes the command, which is why it is listed with the
  scheduler. It needs `on-tick` like the scheduler.
- **Cost**: the before/after dispatch is a slice walk; an unregistered event costs one empty-table
  check (deduplicated because the *host* already refuses to call unsubscribed events). No per-event
  allocation unless a handler asks for one.
- **Shared.**

### 5.8 Optional services: economy, regions, permissions

Shops/social/moderation need the economy; regions/claims/worldedit need the region check; many need
permissions. The pattern is "use it if installed, degrade if not".

```rust
if let Some(econ) = services().economy() {
    econ.withdraw(p, Money::from_major(10))?;
}
if let Some(regions) = services().regions() {
    regions.check(&RegionQuery::build(p, pos))?;
} else {
    // fall back to a permission node
}
```

- **Wraps** `services.lookup(name, version)` (P3) plus typed `economy@0.1`/`regions@0.1` interfaces.
  A missing service is `None`, **never a trap** (the `supports()` philosophy from `plugins.md`).
- **Cost**: `lookup` is done once in `init` and cached; a service call is one host call.
- **Shared**; the typed handles are generated from the service WIT.

## 6. Cost model — no hidden per-tick allocations

- **Dispatch is subscription-driven.** The host never calls an event a plugin did not subscribe to,
  so the SDK adds no per-tick work for those events. The SDK's per-kind tables are checked with an
  `is_empty` fast path.
- **Helpers take `&str`/borrowed refs** and return primitives where they can; the only allocations
  are where the data itself is a collection (`online()`, `inventory()`) or text (a message), and
  those are documented at the call site.
- **`changed()` is O(1)** via the config revision; **`cooldown().ready()`** is a map lookup plus a
  subtraction; **an idle scheduler drains nothing**.
- **The tick budget still bounds the worst case.** P1's shared per-tick budget (consumed-time, not
  wall-clock) charges every plugin call, so even a plugin that ignores this section cannot stall
  the tick; the SDK's job is to not waste the budget on bookkeeping the plugin did not ask for.
- A short "cost" line is required in the doc comment of every public helper, and the hot-path ones
  (`check`, `now_ticks`, cooldowns, empty scheduler drain) get an allocation test where practical.

## 7. Versioning and capability policy

- The SDK crate versions independently (`0.1`) but compiles against the host's WIT. A helper whose
  host import is absent is gated by `supports()` and returns a typed "not available" error / the
  neutral value.
- New capabilities are named exactly as the manifest spells them (`storage.kv`, `permissions`,
  `permissions.admin`, `players`, `world.read`, …). A helper whose capability is ungranted is
  refused by the host, which logs it; the SDK returns the neutral value, matching P1.
- New host imports stay in `aether:plugin@0.1.0`; a new guest callback or export type change is a
  `@0.2.0` world. The SDK is written so the guest backend can be regenerated against either.

## 8. Testing

- **Mock `Host`**: the bulk of the SDK (sugar, command parse, colour, cooldowns, scheduler,
  confirm, typed config/KV) is tested on the host target against a mock `Host` that records calls
  and returns canned values. No WASM target, no server.
- **Host-side integration**: every new WIT import gets a host test in `crates/aether-plugin` (like
  the existing `plugin_host.rs`) that loads a fixture plugin and asserts capability refusal,
  neutral values and the documented result.
- **Frozen-world regression**: the checked-in `hello.wasm` must still load after every import
  addition; `tests/wit_world.rs` continues to pin the export set. Consider extending it to pin the
  import interface names as well so an accidental breaking import change is caught.
- **Cost**: an allocation-counting test for the hot-path helpers (empty dispatch, cooldown miss,
  `changed()` false, idle scheduler), so "no hidden per-tick allocations" is enforced, not just
  claimed.
- Checks per `COMMON.md`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4
  -- -D warnings`, `cargo-locked test` for the touched crates.

## 9. Implementation plan (step 2)

Mapped to the queue's commits; each step lands helpers + tests + doc updates, and stops if a
dependency is missing.

| commit | scope |
|---|---|
| **PS1a** (this) | survey + design; no code |
| **PS1b** | crate skeleton, `Host` trait, mock; `command` builder; `permissions` facade |
| **PS1c** | `message`, `players`, `config`, `store` |
| **PS1d** | `scheduler`, `events`, `cooldown`, `confirm` |
| **PS1e** | port `plugins/examples/hello` (WASM) and one native example; line-count before/after; EN+RU docs |

PS1b starts with the pieces that need no new host capability (`command` parsing/builder, permissions
sugar over the planned import) and the mock, so a missing CMD-01/02 only affects the native/WIT
backend, not the testable core. The examples port in PS1e is where the line-count reduction is
measured.

## 10. Open questions for review

1. **Permissions backend.** CMD-01/02 are not on `oc/P1`. Do we (a) make the SDK's permission
   helpers a facade that returns deny/`supports == false` until CMD-01/02 land, or (b) have PS1
   also add the minimal core resolver? Recommendation: (a) — the permission model is core and
   should land once, in CMD-01/02; PS1 provides the facade and the import so plugins are written
   against the final API from day one.
2. **Dynamic tab-completion.** Static declarations cover most stubs, but `/home <name>`,
   `/warp <name>` and `/lp …` want dynamic suggestions. Confirm that this waits for
   `aether:plugin@0.2.0` with an `on-complete` export, rather than growing the frozen
   `command-result`.
3. **Async scheduler scope.** Native-only `async_result` adds threads to native plugins; confirm
   that is in scope for PS1d or deferred with PA3.
4. **Config reload signal.** `config-revision` is the cheapest signal; the alternative is polling a
   boolean on `on-tick`. Confirm the revision import is acceptable (it is additive).
5. **Native example.** There is no native loader yet, so the "one native example" in PS1e will be a
   test-only native plugin driven by the mock `Host` unless a loader lands first. Confirm that is
   an acceptable reading of "port one native example" given P1 only ships WASM.

## 11. Risks

- **The frozen world** is the main constraint: anything that needs a new guest callback or a changed
  export type is blocked on `@0.2.0`. The catalogue is written to stay inside the frozen set.
- **Dependencies not landed** (permissions/regions/bulk/teleport/player/economy) mean several
  helpers exist as gated facades before their host import does; the `supports()` policy keeps them
  honest and testable against a mock in the meantime.
- **Thinness.** The SDK must not grow a second implementation of the game. Every helper is a
  `Host` method plus sugar; anything that starts to need real state belongs in the host or core.
