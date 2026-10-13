# T02 — Enforce `max_players` and cap concurrent connections

## Context
Aether Engine (`/srv/dev/projects/Aether-Engine`) is a Minecraft server in Rust. Crate
`crates/aether-server`. Two defects:

1. `server.max_players` (config, default 20) is only *displayed* in the server-list ping
   (`session.rs` ~line 126). Login never checks it: any number of players can join.
2. `main.rs` accept loop (~line 175) spawns one OS thread per TCP connection with **no limit**.
   Anyone can open thousands of sockets (each sits up to the 30 s read timeout) and exhaust
   threads/memory.

## Where to work
```
git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/T02 -b oc/T02
cd /srv/dev/projects/Aether-Engine-wt/T02
```
Build only in the worktree, `-j4` max.

## Wanted behaviour
1. **Player cap at login.** In `crates/aether-server/src/session.rs`, right after the existing
   whitelist check (`if !cfg.may_join(&name) { ... return protocol::kick_login(...) }`, ~line 161),
   refuse the login when the number of players already in the registry
   (`registry.snapshot().len()` or a cheaper count if the registry has one) is
   `>= cfg.server.max_players`, with `protocol::kick_login(s, "The server is full.")` and an
   `info` log line in the same style as the whitelist one. **Operators bypass the cap**
   (`cfg.is_operator(&name, ip)`).
2. **Connection cap in the accept loop** (`main.rs`):
   - New config fields in `ServerConfig` (`config.rs`), with serde defaults, documented in the
     example config text inside `config.rs` and in `aether-server.example.toml` at the repo root:
     - `max_connections: u32` — default `max_players * 2 + 16` computed when 0/absent
       (choose: field default 0 = "auto");
     - `max_connections_per_ip: u32` — default 8.
   - Track live connections with an RAII guard (e.g. `Arc<AtomicUsize>` total + a
     `Mutex<HashMap<IpAddr, u32>>` per IP), decremented in `Drop` so every exit path releases it.
   - Over a limit: close the socket immediately (drop it) without spawning a thread, and log at
     most one `warn` per second (rate-limit the log, don't spam).
3. Keep loopback behaviour and everything else unchanged.

## Tests (unit, in the crate)
- the guard increments and decrements (including on early return / drop);
- per-IP limit refuses the (limit+1)-th connection from one IP but not from another;
- `max_connections` auto default = `max_players * 2 + 16`;
- config parsing of the two new fields (follow existing tests in `config.rs`).

## Done when (in the worktree)
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -j4 -- -D warnings`
- `cargo test -p aether-server -j4`

Commit on `oc/T02` (e.g. `server: enforce max_players at login; cap total and per-IP connections`).
Do not push or merge, no sudo, no edits outside the worktree. Match the surrounding code style
(comments explain *why*, like the existing ones).

## Report back
Branch, commit hash, check results, any design choice you had to make.
