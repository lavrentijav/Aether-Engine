# PS1 review 1 — plugin SDK

- Reviewer: opencode (strict pass), 2026-10-15
- Scope: branch `oc/PS1` vs base `oc/P1`, `git log oc/P1..HEAD` (9 commits).
- Inputs: `docs/tasks/QUEUE-PS-plugin-sdk.md`, `docs/design/plugin-sdk.md`, and the code of
  `crates/aether-plugin` (host, native tier, sdk bridge, service registry), `crates/aether-plugin-sdk`
  (thin guest client), `plugins/sdk` (native SDK plugin), `plugins/examples/greet`, and the host/
  bridge tests. No code was edited for this review.

Commits reviewed, oldest first:

| commit | subject |
|---|---|
| `0c711e0` | PS1: бриф |
| `2105bca` | PS1a: plugin SDK survey (design doc) |
| `293d621` | PS1a: SDK is a native service plugin with a thin guest client |
| `e0cf009` | PS1b: native plugin tier, SDK service host, thin guest client |
| `c097849` | PS1c: SDK messages, players and config services |
| `96b5d47` | PS1d: SDK scheduler, cooldowns and confirm services |
| `3e3803c` | PS1e: SDK example, end-to-end test and docs |
| `4271e32` | Merge oc/P1 into oc/PS1: SDK on the CMD0 command registry |
| `089d2b4` | PS1-fix: SDK command builder registers on the CMD0 command registry |

## Owner decisions vs as-built

| owner decision | landed | notes |
|---|---|---|
| SDK is a **native** plugin exporting services | yes | `plugins/sdk` (`tier = "native"`), `NativePlugin`, `ServiceRegistry`; `install_native` first in `plugins.rs`. |
| thin guest client | partial | `crates/aether-plugin-sdk` ships only the `dependent` world, raw generated bindings and `requires!`. No `prelude.rs` handles/builder as the design §9 claims (see F6). |
| typed bridge | yes | `crates/aether-plugin/src/sdk.rs` binds `aether:sdk@0.1` and forwards to the registered provider with the caller id. |
| fronts CMD-01/02, permissions fail-closed | yes | `plugins/sdk` `Permissions` denies/`Err` until the model lands; documented in KNOWN_ISSUES §17. |
| sync timer scheduler | yes | `Scheduler` + SDK `on_tick`; async deferred. |
| command builder on the **core command registry** | yes | `Commands::register` → `HostBackend::register_command` → `commands::registry::register_plugin`, dispatched through `commands::fallback` with `permitted(node, op)`. |

## Checks requested

### 1. Native tier loader safety — PASS, with caveats
- `NativeContext` exposes only `allowed()`/`log()`; the trait has neutral defaults, so a plugin
  overrides only what it needs (`crates/aether-plugin/src/native.rs`).
- `PluginHost::add_native` runs `reject_duplicate` + `check_requires` **before** `init`; an `init`
  error refuses the load (`host.rs:632`-ish). Ids are typed by tier (`add` refuses `Native`,
  `add_native` refuses `Wasm`) — covered by `a_wasm_plugin_may_not_be_added_as_native_and_vice_versa`.
- **No panic containment.** The loader does not `catch_unwind`, and `ServiceRegistry` and the SDK
  services use `Mutex`/`RwLock::unwrap()`. A panicking provider unwinds across the host call into
  the server tick. This is *documented* as trusted-by-install ("may panic or block"), so it is an
  accepted limitation, but "no panics across the boundary" is not actually enforced. Informational.

### 2. No locks held across calls — FAIL (minor)
`plugins/sdk/src/lib.rs:164-173`: `Commands::register` holds the `specs` `Mutex` guard while calling
`backend.register_command(...)`, i.e. across the host backend into `commands::registry` (which takes
its own lock). It is the only such case; `ServiceRegistry::get` correctly clones the `Arc`, drops the
read guard, and only then downcasts. No deadlock cycle exists today, but this directly contradicts the
design's "hold no locks across calls" invariant (§13) and should be fixed by registering with the
core **before** taking or after releasing the `specs` guard.

### 3. Capability checks on grant/revoke — PARTIAL
- WASM path: `StoreData::grant`/`revoke` check `self.allowed("permissions.admin")` and fail closed
  (`crates/aether-plugin/src/sdk.rs:400` and `:414`). Good.
- Native path: a native dependent resolves `Arc<dyn PermissionsService>` from the registry and calls
  `grant`/`revoke` directly; **no capability gate exists on the trait method**, and the SDK provider
  does not (and cannot) check the caller's capabilities. The design claims the SDK "re-checks its own
  invariants" — it does not. Latent only because permissions currently fail closed.
- Messages: the design (§5) says methods that "write or **send**" are gated by the caller's
  capability, but `messages.send/send_key/actionbar/title/broadcast/prefix` have **no capability
  check**; any dependent with a WASM import can broadcast to every player.

### 4. One boundary crossing per WASM call — PASS
Each guest service import lands on one generated host trait method (`crates/aether-plugin/src/sdk.rs`),
which performs one in-process provider call; the caller id comes from `StoreData.plugin`. The combined
`aether:sdk/services` world is added once (`host.rs` `add`), so no second hop and no double-defined
`aether:plugin/types`. `config` is served directly by the bridge (no registry hop).

### 5. No per-tick allocations — MOSTLY PASS, claim overstated
- Idle tick with zero timers allocates nothing (locks two mutexes, walks an empty map).
- But `Scheduler::advance` (`plugins/sdk/src/lib.rs:496`) linearly scans **all** timers every tick —
  O(total timers), not the design's "timer wheel, O(1) idle / O(due)". It also `clone()`s a `String`
  per due timer (`:505`).
- `Scheduler::poll` (`:564`) drains and rebuilds a `Vec` on every call — an allocation per caller per
  poll.
`Locks`/`Arc`/registry paths in `host.rs` remain allocation-free as claimed.

### 6. Command builder → core registry + permission check — PASS, subcommands missing
- `Commands::register` derives the node (`command_node`) and forwards to
  `HostBackend::register_command`; the server implements it as
  `commands::registry::register_plugin` (`plugins.rs:165-172`). Dispatch enters
  `commands::fallback` → `registry::plugin_command` → `permitted(handle, node, op)` before the
  dependent's `on-command` (`commands/mod.rs:181-196`). A core name is refused at registration
  (ownership rule). Unit test `register_forwards_to_the_core_command_registry` (in `plugins/sdk`)
  asserts both the forwarded `(plugin, name, node, op)` and the core-name refusal. Good.
- **Subcommands are not implemented.** `CommandSpec.parent` (`wit-sdk/aether-sdk.wit`,
  `commands.command-spec`) is ignored by `Commands::register` and unknown to the core registry; a spec
  whose `parent` is set registers as a **top-level** `/name`. The design §7.2 ("subcommands are nested
  specs") and the QUEUE's "subcommands" bullet are unmet.

### 7. Dependents fail to load cleanly when SDK disabled — PASS
`PluginHost::check_requires` runs before instantiation / native `init` and returns a named error
("requires service `sdk.commands@0.1`, which is not registered …"). Covered by
`a_dependent_refuses_to_load_without_the_sdk` (WASM `greet`) and
`a_native_dependency_on_a_missing_service_is_refused_clearly` (native). `install_native` loads the SDK
first and honours `[plugins.sdk] enabled = false`. `a_dependency_loads_once_its_provider_is_present`
proves the positive path.

### 8. Tests real — PASS for what exists, incomplete vs design §10
- Real: `plugin_host.rs` and `sdk_bridge.rs` load checked-in `wasm32-wasip2` components
  (`hello.wasm`, `recorder.wasm`, `greet.wasm`); `greet.wasm` calls `commands.register` +
  `messages.send` through the real host linker to a provider, with the caller id asserted. The 19
  `plugins/sdk` unit tests exercise command parse, colour/translation, timers, cooldowns, confirm and
  the core-registry forwarding. `wit_world.rs` still pins the frozen export set.
- Missing vs the design's own testing plan: no capability-refusal test for the SDK grant/revoke, no
  neutral-value/"unavailable" test for the WASM bridge, no allocation/cost tests for the hot paths,
  no import-name pin in `wit_world.rs`, and `sdk_bridge.rs` uses **mock** providers rather than the
  real `plugins/sdk` (so no test drives `SdkPlugin` end-to-end through WASM).

## Additional findings

- **F6 — as-built doc overstates the delivery.** Design §9 lists `src/prelude.rs` with typed handles
  (`permissions()`, `commands()`, …) and a vendored `aether:sdk` package in the thin crate's `wit/`.
  Neither exists: `crates/aether-plugin-sdk` has no handles/builder, and its `wit/` contains only
  `dependent.wit` and reads the host tree by relative path. `KNOWN_ISSUES §17` honestly says "the
  prelude remains for reference", but the design doc (status "✅ implemented") does not. §7.2's
  builder example (`CommandSpec::new(..).perm()..arg()..sub()`) is not implemented;
  `plugins/examples/greet` constructs the WIT record by hand.
- **F7 — `requires!` cannot prevent drift.** The macro emits a bare `REQUIRES` constant that nothing
  reads or checks against `plugin.toml`; keeping the two in sync is manual. The design claims the two
  "cannot drift".
- **F8 — one native example not delivered.** QUEUE step 2 asks for one native and one WASM example
  ported. Only the WASM `greet` exists; the native tier is exercised solely by the in-test
  `TestNative` fixture.
- **F9 — doc bookkeeping.** `KNOWN_ISSUES` numbers the SDK entry §17 (EN) but §16 (RU); minor drift.
- **F10 — config service.** Design lists `config` as a stateful SDK service with "revision polling per
  dependent"; it is implemented as direct host-backed sugar and `revision()` is constant
  (`KNOWN_ISSUES §17` records this). Acceptable, but not the surveyed design.

## Test evidence

- `cargo-locked test -p aether-plugin -p aether-sdk-plugin -p aether-plugin-sdk -j4` → **EXIT 0**,
  52 tests passed (8 manifest/registry + 22 `plugin_host` + 2 `sdk_bridge` + 1 `wit_world` + 19
  `plugins/sdk` unit; doc-tests pass). Run against `089d2b4`; the log included the end-to-end
  `a_wasm_dependent_calls_the_sdk_through_the_host` and both load-refusal tests.
- `cargo test -p aether-sdk-plugin --no-run` → compiles (test profile).
- `cargo check -p aether-server` → finished (dev profile) once the shared-target cache was un-poisoned.
- **Not run:** `cargo-locked test -p aether-server 'plugins::'` could not be completed. The shared
  `CARGO_TARGET_DIR` (`/srv/dev/build/aether-target`) is being actively written by other worktrees
  sharing the same package ids, and `aether-plugin` / `aether-world` fingerprints under the
  server-context hashes flip between the correct PS1 artifacts and stale `oc/P1` / `oc/PV1` ones
  between invocations (`aether_plugin::sdk` unresolved; `EventBody` missing `reason`/`ItemMerge`).
  This is an environment/tooling contamination, not a PS1 code defect: the server-side additions
  (`plugins.rs` `register_command`/`reserved_command_names`/`register_commands`) are exercised by the
  in-file unit tests and by the plugin-crate suite above. The reviewer was unable to obtain a clean
  `cargo-locked` server run in this workspace; re-run on a quiet machine before merge.

## Verdict

The core owner decisions are implemented and behave as designed: a native SDK service plugin, a typed
one-crossing bridge for WASM dependents, fail-closed permissions, a synchronous scheduler, commands
routed through the core registry with the core's permission gate, and clean load refusal when the SDK
is absent. The real component tests pass. The findings above are first-stage gaps and doc/implementa-
tion mismatches, not regressions to shipped behaviour or to the frozen `aether:plugin@0.1.0` world.

Recommended follow-ups before the next stage (none of which require a new package version):
1. Release the `specs` lock before `backend.register_command` (`plugins/sdk` `Commands::register`).
2. Implement `parent`/subcommands, or remove them from the WIT/design until implemented.
3. Gate message sends, and document that native dependents bypass the capability gate (trusted tier).
4. Correct `docs/design/plugin-sdk.md` §7.2/§9/§10 to describe what shipped (no prelude/builder,
   `requires!` is unenforced, scheduler is a scan not a wheel), and align the testing plan.
5. Land a `cargo-locked test -p aether-server` run on a quiet workspace before merging to `main`.

Verdict: MERGE
