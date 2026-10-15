# AU1 review 1 — online mode and hybrid auth (security)

Reviewer: strict security pass. Branch `oc/AU1`, commit `9b7fd3a`.
Base for the AU1 work: merge-base with `main` is `b796254` ("задача AU1: ...").
Branch state: `git log main..HEAD` = the 7 AU1 commits (AU1a..AU1e + hybrid step 2);
`git rev-list --count HEAD..main` = 29. Note the branch is 29 commits behind
`main`, so a two-dot `git diff main..HEAD` also *reverses* main's later plugin/
RP1 commits; this review reads the real change set, `git diff b796254..HEAD`
(34 files). A merge/rebase is required before landing regardless of the verdict.

Checks run (all on `/srv/dev/build/aether-target`, `CARGO_INCREMENTAL=0`, `-j4`):

* `cargo-locked test -p aether-server --lib` — **978 passed, 0 failed**
* `cargo-locked test -p aether-server --lib auth::` — **35 passed**
* `cargo-locked test -p aether-api` — passed (incl. `keys::no_two_subsystems_claim_the_same_prefix`)
* `cargo fmt --all -- --check` — clean
* `cargo-locked clippy -p aether-server --all-targets -- -D warnings` — clean
* `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok`
* No code was edited.

---

## Findings

### 1. HIGH — Remote pre-auth unbounded allocation in `read_byte_array` (OOM DoS)
`crates/aether-server/src/auth/policy.rs:343-356` (alloc at `:351`).
The Encryption Response is parsed by hand: `read_byte_array` reads a VarInt
length and then `vec![0u8; len as usize]` **before** checking it against the
bytes actually present. `read_packet` caps the whole frame at
`MAX_PACKET_LEN` = 2 MiB (`proto.rs`), but the *inner* length is trusted up to a
5-byte VarInt, i.e. `0x7FFF_FFFF` ≈ 2 GiB. A hostile client sends
`Encryption Response` (id `0x01`) with a byte-array length of `0x7FFF_FFFF`;
the server allocates/zeroes ~2 GiB, then every subsequent `pin.u8()` fails and
the packet is dropped — but the memory spike and memset stall already happened.
It is called twice per handshake (secret + token).
Reachability: any unauthenticated client on a `hybrid` or `online` server. In
hybrid the offer is sent to *any premium name* (e.g. `Notch` on a fresh server,
unlocked) or any locked name, so a scanner can trigger it at will. With the
default `max_connections = max_players*2+16` (56), a handful of sockets is
enough to OOM the process.
Fix: bound `len` by the remaining length of the `PacketIn` (expose a
`remaining()`, or compare against the packet body length) and reject otherwise;
a byte array in this packet is at most 128 bytes for RSA-1024. This is the
single blocking finding.

### 2. MEDIUM — Stale uuid→name index lets one player delete another's lock
`crates/aether-server/src/auth/store.rs:104-113`, `get_by_uuid` at `:93-97`.
`put` "moves" a lock by consulting `get_by_uuid(lock.uuid)` and deleting
`name_key(old.name)` when the name changed, **without checking
`old.uuid == lock.uuid`**. `get_by_uuid` re-resolves the name through the name
index, so once a name is handed to a different uuid, the old uuid's reverse
entry is stale and points at the new owner. Sequence:

1. A (uuid A) locks `alice`; keys `n/alice→A`, `u/A→"alice"`.
2. A renames at Mojang; B (uuid B) becomes `alice` and logs in online.
   `put` writes `n/alice→B`, `u/B→"alice"` but leaves `u/A→"alice"` dangling.
3. A logs in under the new name `bob`. `put` does `get_by_uuid(A)` →
   `u/A="alice"` → `get_by_name("alice")` → **B's lock**; since
   `"alice" != "bob"` it `del(name_key("alice"))` — deleting B's premium lock.

After that `alice` is unlocked and can be joined offline (visual impersonation).
The existing test `a_name_change_moves_the_lock_and_releases_the_old_name` only
covers the same-uuid path and misses this. Fix: only release the old name when
`old.uuid == lock.uuid`, and/or when overwriting `name_key(lower)` with a
different uuid, clear the displaced uuid's `uuid_key`. Add a regression test for
name-handover followed by old-owner rename.

### 3. MEDIUM — No username validation; whitespace/control/unicode spoof a locked name
The claimed name (`codec.read_login_start`) is used verbatim for the whitelist
(`config.may_join`), the operator check, the lock lookup (`store::get_by_name`,
exact `to_ascii_lowercase` match) and the tab list/chat. Nothing enforces the
vanilla `[A-Za-z0-9_]{3,16}` rule. A locked `notch` does **not** match a claimed
`"notch "` (trailing space), `"notch\u{200b}"`, or a homoglyph; the profile
lookup for that string returns no account, so the server admits it offline and
renders a name that resembles the premium one. It does not yield the premium
uuid (offline uuid differs, ops/whitelist compare case-insensitively and also
miss the padded form), but the whole point of the lock is "an operator bound by
name alone is takeable" — a displayed near-identical name defeats that.
Fix: validate and normalize the username shape before whitelist/auth, and reject
names that do not match, as the vanilla server does.

### 4. LOW — Corrupt or half-written lock fails open (name silently unlocks)
`store.rs:70-73`: `decode` returns `None` for a blob that is not exactly 32
bytes, and `get_by_name` then reports "unlocked" — the login is admitted offline
instead of refused. The existing test `a_corrupt_blob_reads_as_unlocked_not_a_panic`
pins this fail-open behaviour. Separately, `put` writes the name key, then the
uuid key, and on rename does `del` before `put` (non-atomic); a crash in a window
leaves the name with no lock. A security boundary should fail **closed** on an
unreadable lock (refuse + loud log), not silently unlock. Low because remote
input cannot cause it; a disk fault/crash can.

### 5. LOW — `has_joined` transport/5xx errors are conflated with "no session"
`policy.rs:337-338`: only `ApiError::Unreachable` maps to `ApiDown`; any other
error (`Status(5xx)`, `Decode`) becomes `NoSession`. Outcomes stay fail-closed
(online → refused; locked → refused; unlocked premium → offline), so this is not
a bypass, but a transient Mojang outage on a locked login is logged/messaged as
"an offline attempt on a locked name" rather than a service failure, and the
`on_lookup_failure` policy is not consulted for the session server. Consider
mapping 5xx/`Decode` to `ApiDown` for consistent handling and diagnostics.

### 6. LOW/INFO — Ephemeral RSA key and the accepted `RUSTSEC-2023-0071`
`ServerKey::generate` runs once per start, lives only in `SERVER_KEY` memory and
is never persisted — good (nothing to leak on disk, no key reuse). The `rsa`
Marvin timing oracle is explicitly ignored in `deny.toml` with a written
rationale. Here a padding failure and a token mismatch both collapse to the same
`OnlineOutcome::BadResponse` (`policy.rs:317-323`), so there is no obvious
decryption oracle, but the side channel is not eliminated. Acceptable with the
documented note; prefer a blinded decryption if `rsa` exposes one.

### 7. INFO — No rate limit on Mojang traffic or login attempts
Each unlocked premium login performs a profile lookup (cached) and each
premium/locked login performs `hasJoined`; `max_connections` bounds concurrency
but there is no per-IP or global login rate limit, so a bot can drive outbound
Mojang calls and park up to `max_connections` threads on blocking HTTP (bounded
by `lookup_timeout_ms`). The lookup/bounce caches are correctly bounded (4096
each, POSITIVE_TTL 1 h / NEGATIVE_TTL 10 min / TTL 300 s, with eviction). Worth a
limiter before a public deployment; not a compromise by itself.

### 8. INFO — Bounce fallback cannot bypass a lock
`policy.rs:157-169` checks the lock **before** `bounce.bounced`, so the
same-IP+name offline fallback (`:172`) only suppresses the encryption *offer*
for unlocked premium names — which are open offline by design anyway. No security
impact; the `(IP,name)` pre-poisoning caveat is already in `KNOWN_ISSUES.md`.

---

## Verified correct (the parts that had to be right)

* **Server-id hash** (`crypto.rs:119-168`): `SHA1(server_id ‖ secret ‖ pubkey)`
  matches the three wiki.vg vectors (`Notch`/`jeb_`/`simon`), including the
  signed two's-complement rendering and the `BigInteger.toString(16)`
  leading-zero-bit trim (`08 8e… → 88e16a…`).
* **AES/CFB8** (`crypto.rs:74-115`): byte-at-a-time CFB8 matches the `cfb8`
  reference KAT, round-trips across ragged chunk boundaries, and uses the secret
  as both key and IV.
* **Compression/encryption order**: handshake in the uncompressed+unencrypted
  login state, `enable_encryption` after token match (`policy.rs:331`),
  `enable_compression` after admission (`session.rs:195`). `write_frame`
  encrypts the framed bytes / `DecryptReader` decrypts before parsing, so the
  cipher sits outside compression and covers the length prefix; Set Compression
  itself goes out under the old framing.
* **Token/secret checks** happen before the stream switch; a wrong token is
  rejected and `hasJoined` is never called.
* **Per-version gates**: `encryption_request_has_should_authenticate` is `true`
  only for 759/760 (1.19/1.19.2), `false` elsewhere including 1.19.3 and all
  764+; the 26.2 `online_mode` field is `p.online` (`protocol/modern/mod.rs:223`).
* **Lock semantics**: case-insensitive lookup, uuid-move on rename, both indexes
  round-trip, corrupt blob does not panic, `AUTH_LOCK` prefix collides with
  nothing (`keys.rs` test), persistence via `put_meta` into the world store.
* **Policy**: offline attempt on a locked name is refused; locked names never
  consult the bounce cache; `on_lookup_failure` is honoured for lookups.
* **No tick-thread blocking**: `authenticate` runs in `login_and_play` on the
  per-connection OS thread (`lib.rs:304`); the game tick is a different thread.
* **No secrets persisted**: RSA key and AES secret are memory-only; no secret or
  key is logged.
* **Toolchain**: fmt, clippy `-D warnings`, `cargo deny check`, and the locked
  test suites all green as listed above.

---

## Verdict: DO NOT MERGE

Finding 1 is a remotely triggerable, pre-authentication memory-exhaustion DoS
and blocks the merge on its own. Finding 2 is a real lock-integrity defect that
can silently unlock a premium name and has no regression test; finding 3 leaves
the lock's visible guarantee trivially bypassable. The cryptographic core,
version gating, framing order and lock-store basics are otherwise sound, and
findings 4–8 are hardening/documentation. Fix 1 and 2 (with tests) and address 3,
and this should pass a re-review.

Verdict: DO NOT MERGE
