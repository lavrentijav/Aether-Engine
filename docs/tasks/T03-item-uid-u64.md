# T03 — Item instance ids: 64-bit, allocated, never reused

## Context
Aether Engine (`/srv/dev/projects/Aether-Engine`), Rust. Every item stack carries an instance id
`ItemUid(pub u128)` (`crates/aether-world/src/journal/event.rs`), minted by
`ledger::mint_uid()` (`crates/aether-world/src/journal/ledger.rs` ~line 242) from two
`RandomState` hashes. Problems: 16 bytes per stack/event where 8 suffice, ids are random (no order,
no "born before" reasoning), and uniqueness is probabilistic.

Design decision (see `docs/storage-v2/README.md`, phase 3, and the journal's own
`K_RESERVED`/`SEQ_BLOCK` reservation in `crates/aether-world/src/journal/mod.rs` — copy that
pattern): **`ItemUid(pub u64)` handed out by a monotonic allocator that reserves blocks.** The
reservation bound is persisted *before* any id from the new block is used, so a crash or restart
can never reuse an id; unused ids after a crash are simply skipped.

Existing saved data does **not** need to be readable after this change (agreed: worlds are wiped).

## Where to work
```
git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/T03 -b oc/T03
cd /srv/dev/projects/Aether-Engine-wt/T03
```
Build only there, `-j4` max.

## Wanted
1. `ItemUid(pub u64)` everywhere (`aether-world`, `aether-server`, `aether-api`). Keep the newtype.
2. New `UidAllocator` in `crates/aether-world/src/journal/ledger.rs` (or a new `uid.rs` next to it):
   - `UidAllocator::open(backend: B) -> Result<Self, StorageError>` reads its bound from key
     `b"U"` (check the key is unused: `grep -rnE "b\"U|b'U'" crates`), starts at the bound
     (or 1 if absent);
   - `next(&self) -> Result<ItemUid, StorageError>`: lock-free `fetch_add` on the hot path; when the
     value reaches the persisted bound, take a mutex and persist `bound = (v / BLOCK + 1) * BLOCK`
     with `BLOCK = 65_536` before returning it (same shape as `Journal::reserve`).
   - Id `0` is never handed out (reserve it as "no id").
3. Process-wide access: `ledger::install_allocator(alloc)` (a `OnceLock`) + `mint_uid()` keeps its
   signature `-> ItemUid` and uses the installed allocator. If none is installed (unit tests), fall
   back to a process-local `AtomicU64` starting at `1 << 62`, documented as the test-only path.
   A storage error in `next()` is a bug-level failure for a running server: `mint_uid` panics with a
   clear message (M-PANIC-ON-BUG); do not silently fall back in that case.
4. The server installs the allocator at startup over the world's KV backend **before** the listener
   accepts connections (find where the world/journal is created in
   `crates/aether-server/src/main.rs` / `session.rs` `DemoWorld`, and how `aether-api` exposes the
   backend — `World::journal().backend()` exists). Add a startup log line with the starting id.
5. Encodings, with format versions bumped (old blobs are refused, not guessed):
   - `crates/aether-world/src/journal/event.rs`: uid fields 16 → 8 bytes, `FORMAT_VERSION` 1 → 2.
     Update the hand-written layout tests there and in `journal/tests.rs`.
   - `crates/aether-server/src/inventory.rs` `encode`/`decode`: uid 16 → 8 bytes,
     `FORMAT_VERSION` 2 → 3; drop the version-1 compatibility branch (old data is not read).
   - `crates/aether-server/src/db.rs` (Postgres mirror text form of uids) and the economy code:
     format the u64 consistently (decimal), adjust tests.
6. Update docs that state "128-bit uid" (`grep -rn "128-bit\|u128" docs crates --include=*.md`
   and doc comments mentioning the uid size).

## Tests to add
- allocator: ids strictly increase; reopen on the same `MemStore` resumes at/after the persisted
  bound and never returns an id ≤ any id returned before; bound is written once per block, not per id;
  concurrent `next()` from 8 threads yields no duplicates (follow
  `concurrent_appends_then_a_reopen_never_reuse_a_sequence_number` in `journal/tests.rs`);
- `0` is never returned;
- inventory and event round-trips with the new widths; a v2 inventory blob / v1 event blob is refused.

## Done when (in the worktree)
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -j4 -- -D warnings`
- `cargo test --workspace -j4`

Commit on `oc/T03` (e.g. `items: 64-bit instance ids from a block-reserving allocator`). No push,
no merge, no sudo, no edits outside the worktree. Match the code's comment style (explain *why*).

## Report back
Branch, commit hash, check results, every place you were unsure about.
