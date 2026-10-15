# AU1 notes — online mode / hybrid auth

## Revision: hybrid step 2 (follow-up)

The owner changed hybrid step 2: **offline stays open for everyone**, including
names registered at Mojang. Only names that completed online auth *on this
server* are locked. For an unlocked premium name the server now:

1. looks the name up in the profile API;
2. if premium, *offers* encryption (Encryption Request);
   * `hasJoined` ok → online login + lock the name;
   * `hasJoined` fails (cracked client that answered without a session) →
     admit offline **in the same connection**, encryption already on;
   * client drops during auth (vanilla client with a failed session) → the
     next attempt from the same IP + name within 5 minutes is admitted
     offline with no encryption offer (`auth::bounce`);
3. no premium account → offline directly;
4. a lookup failure still follows `on_lookup_failure`.

Unlocked names are never refused for auth reasons. The update is commit
"AU1: hybrid step 2 — offline stays open..." on this branch; the `auth::policy`
tests cover the fail→offline, drop→bounce→offline, locked-still-refused and
another-address-still-offered cases.

## Test-run deviation (read this first)

`cargo-locked` could not be used for the AU1d policy/login tests: the
machine-wide lock `$CARGO_TARGET_DIR/.cargo-locked` was held continuously for
40+ minutes by another worktree's `cargo test --workspace`, with several other
worktrees queued behind it, and every `flock -w` attempt to acquire it timed out.

Because all shared-target-dir agents were blocked on that lock (so nothing else
was writing the shared target directory), the untouched dependency sources were
touched to force a rebuild and the tests were run with plain `cargo test` against
the shared `CARGO_TARGET_DIR`. The same contention recurred for the hybrid-step-2
revision (another worktree held the lock for 11+ minutes running `cargo clippy`;
`flock -w 420` and `flock -w 60` both timed out), so that run was plain too.
Results:

* `cargo test -p aether-server --lib` — **978 passed, 0 failed** (after the
  step-2 revision; 971 before it)
* `cargo test -p aether-api` — **passed**
* `cargo test -p aether-server --lib auth::policy` — 10 passed
* `cargo test -p aether-server --lib auth` (store/crypto/api/bounce/policy) — all passed

AU1a–AU1c were run under `cargo-locked` before the contention began.

## Not done (documented, not hidden)

0. **Hybrid no longer refuses unlocked names.** With the step-2 revision the
   only auth refusal left is a locked name whose owner does not authenticate.
   The `on_lookup_failure = "deny"` setting still refuses when the **lookup**
   itself fails, which is the one remaining way an unlocked name can be turned
   away; the default `"offline"` never does.
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
