# AU1 notes — online mode / hybrid auth

## Test-run deviation (read this first)

`cargo-locked` could not be used for the AU1d policy/login tests: the
machine-wide lock `$CARGO_TARGET_DIR/.cargo-locked` was held continuously for
40+ minutes by another worktree's `cargo test --workspace`, with several other
worktrees queued behind it, and every `flock -w` attempt to acquire it timed out.

Because all shared-target-dir agents were blocked on that lock (so nothing else
was writing the shared target directory), the untouched dependency sources were
touched to force a rebuild and the tests were run with plain `cargo test` against
the shared `CARGO_TARGET_DIR`. Results:

* `cargo test -p aether-server --lib` — **971 passed, 0 failed**
* `cargo test -p aether-api` — **passed**
* `cargo test -p aether-server --lib auth::policy` — 7 passed
* `cargo test -p aether-server --lib auth` (store/crypto/api/policy) — all passed

AU1a–AU1c were run under `cargo-locked` before the contention began.

## Not done (documented, not hidden)

1. **Live client verification.** No premium client (and no running server with a
   real Mojang path) was available, so the encryption handshake and `hasJoined`
   are covered by unit tests + a local mock HTTP server only. The 1.19.x
   salt/signature variants are written from the protocol description.
2. **V1 harness live test** (`MCC offline under _alpha_01 still admitted in
   hybrid`) was not run: it needs a built server, the harness, and a reachable
   profile API.
3. **Skin properties** reach the modern (1.21.11/26.x) Player Info packet only;
   older codecs still send an empty property list.
4. **Operator/whitelist by uuid** is not implemented (name / name@address only).
5. `/aether auth migrate` moves the player-state and inventory blobs; it does not
   move position-keyed block containers, chests or the journal.

See `docs/KNOWN_ISSUES.md` item 17 for the same list from the operator's side.
