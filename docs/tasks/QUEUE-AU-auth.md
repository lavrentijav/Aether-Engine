# AU1 — online mode and hybrid auth (premium names locked to Mojang auth)

Read `docs/tasks/COMMON.md`, the login path for every codec (`protocol/*/login*`, `protocol/modern/version.rs`),
`config.rs`, how player uuids are derived now (offline: `OfflinePlayer:<name>` md5 v3).

## Why
The server only has offline mode. On fi2 random scanner bots (JollyWolf2999 from Hetzner) join freely, and
an operator bound by name alone is takeable. Owner wants a hybrid mode.

## Config
```toml
[auth]
mode = "hybrid"          # "offline" | "online" | "hybrid"   (default "offline" for now, fi2 sets hybrid)
session_server = "https://sessionserver.mojang.com"
profile_api   = "https://api.mojang.com"
lookup_timeout_ms = 3000
on_lookup_failure = "offline"   # hybrid: what to do if the Mojang API is unreachable: "offline" | "deny"
```

## Behaviour
- **online:** vanilla encryption handshake for every protocol we speak (1.8.9 → 26.x): Encryption Request
  (server RSA-1024 keypair generated at start, verify token), Encryption Response, AES/CFB8 on both
  directions, `hasJoined` against the session server (server-id hash = Minecraft's signed SHA-1 hex),
  uuid + name + skin properties from the response go into the profile (and to the tab list/player info).
  Compression after encryption as vanilla. Per-protocol differences (1.19.x signature/salt variants,
  26.2 `online_mode` field in login) handled by the existing version gates.
- **hybrid:**
  1. A name that has **ever completed online auth** on this server is *locked*: stored in the server DB
     (`premium_lock(name_lower → mojang uuid, first_seen, last_seen)`). Locked names must pass online auth;
     an offline attempt under that name is refused with a clear message ("this name is premium, log in with
     the official launcher"). The player keeps the Mojang uuid (inventory, ops, journal all key on it).
  2. A name that is not locked: look it up in the profile API. If a premium account with that name exists →
     require online auth (so nobody can squat a premium name offline first); on success, lock it.
     If no such account → admit offline with the offline uuid.
  3. Lookups cached (positive 1 h, negative 10 min), bounded; API failure → `on_lookup_failure`.
  4. Name change at Mojang: lock is by uuid too; a locked uuid arriving under a new name moves the lock.
  5. An offline-uuid player whose name later becomes premium: admins get a command to migrate data
     (`/aether auth migrate <name>`), not automatic.
- Operators/whitelist entries may name a uuid; a premium-locked op cannot be impersonated offline.
- Logging: each login logs mode used (online/offline/refused + reason).

## Constraints
- Crypto: use RustCrypto crates (`rsa`, `aes`, `cfb8`, `sha1`) — check licenses with cargo deny. HTTP: an
  async client already in the tree if any, else `reqwest` with rustls (no openssl). No blocking on the tick
  thread: the auth runs in the login task.
- Tests: encryption round trip against known vectors; server-id hash vectors (`Notch` →
  `4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48`, `jeb_` → `-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1`,
  `simon` → `88e16a1019277b15d58faf0541e11910eb756f6`); hybrid decision table (locked/unlocked × premium/
  not × online ok/fail × API down) with a mocked session server and profile API; lock persistence;
  offline attempt on a locked name refused. Live test via V1 harness: MCC offline under `_alpha_01` still
  admitted (not premium) in hybrid.
- Docs EN+RU: STATUS, KNOWN_ISSUES, config reference. Commit per step (AU1a config+lock store, AU1b
  encryption, AU1c session server, AU1d hybrid policy, AU1e tests/docs).
