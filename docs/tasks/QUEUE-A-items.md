# Queue A — items to ItemTypeId, then history scaling (do in order)

Repo `/srv/dev/projects/Aether-Engine`. Read `docs/storage-v2/README.md` and
`docs/storage-v2/phase1-item-types.md` first. One worktree for the whole queue:
`git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/QA -b oc/QA`.
Build only there, `-j4`. After EACH task: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -j4 -- -D warnings`, `cargo test --workspace -j4` must pass,
then one commit per task (message starts with the task id). No push/merge/sudo, no edits outside the
worktree. If a task is blocked, write why in `docs/tasks/QUEUE-A-notes.md`, commit, go to the next.
Old saved data need not stay readable (bump format versions, refuse old blobs).

## A1 (P1b) — `inventory::Stack.item: String` → `ItemTypeId`
`crates/aether-server/src/inventory.rs` and every user (windows, containers, crafting, ground items,
combat drops, stash, rewards, commands). Game tables keep `&'static str`; resolve names once at
startup to ids (`ItemTypeId::vanilla(name)`, panic with the name if missing — a typo is a bug).
Inventory blob stores `u16` (bump FORMAT_VERSION). Wire encode/decode via
`Version::wire_item` / `engine_item` (modern codecs); old codecs convert via names.
Commands and logs still show names.

## A2 (P1d) — events, ledger, provenance, economy carry `ItemTypeId`
`journal/event.rs` `ItemMint/ItemDerive.item` → `ItemTypeId` (bump FORMAT_VERSION),
`ledger.rs`, `game/provenance.rs` `Entry.item`, economy listings in memory.
External surfaces (Postgres rows, `/audit`, commands) keep names (resolve via the world's registry).

## A3 — `/audit` and history commands must not load the whole journal
`crates/aether-server/src/commands/mod.rs` calls `journal().all_events()` (all history into a Vec:
400k events = 1.6 s and all of it in RAM). Add to `Journal` a streaming/bounded API
(`events_since(seq, limit)` / iterator over the backend prefix scan in chunks, or by actor index)
and make the commands use bounded queries (newest N, by actor, by column). Keep `all_events` only
for tests. Tests: commands work on a journal of 100k events without materialising all of them.

## A4 — mutex poisoning and lock order audit (report + small fixes)
List every `lock().unwrap()` in `crates/aether-server/src` that is held while calling into another
lock-holding module (entities ↔ containers ↔ players). Write the lock order you find into
`docs/storage-v2/lock-order.md`; fix any place that takes them in the opposite order.
