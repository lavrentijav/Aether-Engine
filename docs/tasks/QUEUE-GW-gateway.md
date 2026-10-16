# GW — Aether Gateway (proxy) as a separate module, then a separate process, then Elixir

Read `docs/ROADMAP.md` §"Phase 3 — Network & multi-world", `docs/DESIGN_NOTES.md` §4,
`crates/aether-baseproxy/`, and the client-facing half of `crates/aether-server/`
(`session.rs`, `protocol/**`). This is a **design brief with a first, small, verifiable step** — it
is not a request to rewrite the server.

## Why
Today `aether-server` is one process that does three different jobs at once:
1. **accepts client TCP**, TLS/rate-limit, the version handshake, and the **per-version
   codecs** (`crates/aether-server/src/protocol/`, ~39k lines, v47 … v777);
2. **owns the world** — storage, generation, the tick, gameplay, journal;
3. routes each client straight into that one world.

`crates/aether-baseproxy` is only translation helpers (vanilla chunk section → engine SubChunk,
block-state ids, player box). Because networking and the world are fused, an owner **cannot**:
- run several worlds on separate engines and **move a player between them** without a reconnect;
- **reload one world** (restart/upgrade/regenerate it) while the players on it stay connected;
- scale the client-facing layer (huge, I/O-heavy, one thread per connection) away from the tick;
- rewrite the client-facing layer **in Elixir/OTP** later, which is the stated direction
  (`docs/ROADMAP.md`: "Network Gateway (Elixir/OTP or C++/Rust epoll/io_uring)").

The owner's target: **the proxy is the module that talks to clients and translates the core's
language for each client version; it is separate from the world engine; later it is Elixir.** Then a
player can be moved world→world, or a single world reloaded, **without the client reconnecting**.

## Target shape
- **`aether-gateway`** (new crate; a *module boundary* first, a *process* later): TCP/TLS, DDoS/
  rate-limit, handshake, per-version codecs, packet (de)serialization, and one **`EngineLink`** per
  connected player. It holds no world state beyond what a client session needs.
- **`aether-engine`** (the world half of today's server): storage, generation, tick, gameplay,
  journal, and a **`GatewayLink`** per connected player. It speaks the internal protocol, not the
  Minecraft wire.
- **Internal binary protocol** between them — versioned, transport-agnostic (framed messages, not
  tied to TCP or to either language), covering: player input, chunk/sub-chunk sections (already
  `aether-baseproxy`'s job, reused), block changes, entity/player updates, windows, chat, and
  **control** messages (attach/detach a player, save/drain a world, reload).
- **Seamless hand-off (stateless routing)**: the gateway keeps the client's TCP open and, on a
  boundary crossing / admin move / world reload, **re-points the `EngineLink` to another engine** and
  replays the vanilla "change dimension / respawn / resync" sequence to the client — which is exactly
  what a client already tolerates without reconnecting.

## Phased plan (each phase is independently shippable)
1. **The seam, in one process (no behavior change).** Define the `EngineLink`/`GatewayLink` traits and
   the internal message enum; make today's monolithic server implement the gateway→engine side
   **in-process**. Nothing changes for players; this proves the interface and makes the split later a
   move, not a rewrite. This is the task to start with.
2. **Two processes, one world.** Move the gateway to its own binary talking the internal protocol
   (localhost, then a socket) to the engine. Single world. Behaviour still unchanged, but now the
   boundary is real and testable across a socket.
3. **Multi-world + hot reload.** Several engines, one gateway; route by world; `control` messages to
   drain/reload one engine without dropping its players (they are re-pointed to the reloaded engine).
4. **Elixir gateway.** Reimplement `aether-gateway` in Elixir/OTP against the **frozen** internal
   protocol; the Rust engine is unchanged. (Do not start this until the protocol is stable.)

## Phase 1 (the queueable task) — deliverables
- A new module (e.g. `crates/aether-gateway` pulled into the workspace, or a `gateway` module under
  the server if a crate split is premature) defining:
  - `trait EngineLink` — what the gateway needs from the world: attach/detach a player, push player
    input, receive outbound events (chunk sections, block changes, entities, chat, windows).
  - `enum EngineMessage` / `GatewayMessage` — the internal protocol messages, with a **stable wire
    encoding** (a length-framed, versioned format; reuse the server's existing `proto` var-int
    reader/writer where sensible). Document the format in the module and in a new
    `docs/design/gateway.md` (EN+RU).
  - An **in-process adapter** implementing `EngineLink` over today's `aether-server` world/tick, so
    the current server keeps working as the phase-1 engine.
- No player-visible change: the join, play, and admin flows behave exactly as before.
- Tests: the internal protocol round-trips for every message; the in-process adapter drives a join
  and a chunk send end-to-end (a headless test, as the existing protocol tests do).

## Explicitly out of scope for phase 1
- No new process, no socket, no Elixir.
- No multi-world routing yet.
- No rewrite of the codecs or the world engine.
Keep the diff small and the behaviour identical; later phases build on this seam.

## Open questions to answer in the design doc
- Where exactly the codec layer sits: the per-version **encoders** must stay with the gateway (they
  are client-facing), while the world speaks engine ids — confirm the split point against
  `protocol/mod.rs`'s `BlockSource` boundary, which already abstracts the world for the encoders.
- Message framing and back-pressure: one ordered stream per player, how to bound and pace it (the
  existing per-connection outbox is the model).
- Player identity/state ownership across a hand-off: what the gateway keeps (socket, version, ciphers)
  vs what the engine keeps (inventory, position, effects) and how a detach/attach carries it.

## Done criteria (phase 1)
- The traits + message enum exist, documented, with a stable encoding and tests.
- The current server runs its client path through the in-process `EngineLink` with **no behaviour
  change** (existing test suite green).
- `docs/design/gateway.md` (+ `.ru.md`) sketches the full 4-phase plan and the phase-1 contract.
- fmt / clippy `-D warnings` / `cargo-locked test` clean.
