# AU1 notes — online mode / hybrid auth

## Security review 1 (follow-up) — all findings fixed

`docs/tasks/AU1-review-1.md` returned DO NOT MERGE; every finding is now fixed:

1. **HIGH — pre-auth OOM.** `read_byte_array` bounds the length before
   allocating: `PacketIn::remaining()` is checked and the length capped at 256
   bytes (a shared secret/verify token is at most 128 RSA bytes). A VarInt
   naming `0x7FFFFFFF` now fails without allocating; tested inside the crypto
   parser and end to end against a hostile Encryption Response.
2. **Lock handover.** `store::put` releases a uuid's old name only when that
   name is still its own (`old.uuid == lock.uuid`), and a name handover clears
   the displaced uuid's reverse index. Regression test for handover-then-rename
   (the case that let one player delete another's lock).
3. **Username validation.** `auth::valid_username` enforces vanilla's
   `[A-Za-z0-9_]{3,16}` for every login, offline included, before the whitelist
   and auth; trailing space, zero-width and homoglyph claims are refused.
4. **Corrupt lock fails closed.** `store::check` reports `Corrupt` distinctly;
   an unreadable lock refuses the login (loudly logged) instead of admitting
   offline.
5. **hasJoined failures.** Any transport/status/decode error maps to `ApiDown`
   (a service failure), which follows `on_lookup_failure`; a locked name is
   still refused rather than admitted offline.
7. **Per-IP login rate limit.** `[auth] login_rate_per_min` (default 10, `0`
   disables) through `auth::ratelimit`, checked before the whitelist/auth.

## Merge

The branch was 29 commits behind `main`; `main` is merged in (commit "Merge main
into oc/AU1"). Conflicts were the server `Cargo.toml` (auth deps + optional
`aether-plugin`), `deny.toml` (licence list), `commands/mod.rs` (`/aether auth
migrate` + the plugin fallback dispatcher), `config.rs` (`[auth]` + `[plugins]`),
`KNOWN_ISSUES.md` (auth item 17, plugin item 18) and `Cargo.lock` (regenerated).

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

## Test runs

The review fixes and the merge were checked under `cargo-locked` (forcing the
local build slot: the remote builder `fi2` was mid-toolchain-repair and failed
`ring`'s build script, a builder-environment fault unrelated to this branch):

* `cargo-locked test -p aether-server -j4` — **1004 passed, 0 failed**
* `cargo-locked test -p aether-api -j4` — passed
* `cargo fmt --all -- --check` — clean
* `cargo clippy --workspace --all-targets -D warnings` — clean
* `cargo deny check` — advisories/bans/licenses/sources ok

Earlier (AU1d and the step-2 revision) the machine-wide lock was held for 40+
minutes by another worktree's `cargo test --workspace`, so those interim runs
used plain `cargo test` against the shared target dir; that is noted here for
honesty but superseded by the `cargo-locked` runs above.

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
