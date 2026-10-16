# voice plugin — Simple Voice Chat + Plasmo Voice bridge (VC1)

Status: 🚧 implemented (VC1a/b/c, 2026-10-16). Native plugin (tier **N**,
`plugins/voice`), compiled into `aether-server` behind the `plugins` feature.
The protocol layers are pinned against the mods' own source:

- **Simple Voice Chat** (henkelmax), branch **`1.21.11`** (`COMPATIBILITY_VERSION`
  18). The server must announce the *same* compatibility version the client
  sends in `voicechat:request_secret`, or the client shows an "incompatible
  version" notice and never connects.
- **Plasmo Voice** (plasmoapp), v2.x. Plugin channel `plasmo:voice/v2`.

The mods' servers are reimplemented natively: no Bukkit, no Paper, no
third-party server-side plugin.

## What works

- **Both handshakes.** SVC `request_secret → secret`, `update_state`, group
  create/join/leave, and per-player state broadcast; PV `PlayerInfoRequest →
  PlayerInfo → Connection`, then config and player-list after the first UDP
  ping. The server drives the keepalives both mods need (SVC `KeepAlive`, PV
  `Ping`) and answers PV `SourceInfoRequest` with a `SourceInfoPacket`.
- **Two UDP servers** (opt-in; default ports `:24454` SVC, `:60606` PV), each
  on its own thread, decoding to one internal `VoiceFrame`. `AES-GCM` (SVC) and
  `AES-CBC/PKCS5` (PV) are implemented here and checked against NIST vectors.
- **Mutual hearing.** `frame::route` decides who hears a frame by proximity
  (per-frame distance, whispers at half) or by group; the Opus payload is
  re-wrapped, **never re-encoded**, so an SVC speaker reaches a PV listener and
  vice versa byte-for-byte.
- **Admin commands** `/voice join|leave|spy|record` under the permission node
  `aether.voice.admin` (operators always pass). `/voice join` bypasses a group
  password; `/voice spy <player|group|all|off>` copies matching frames to the
  admin's own voice client.
- **Recording** off by default; `/voice record start|stop <player|group|all>`
  writes one Ogg/Opus file per speaker per session under `record_dir`, with
  who/when/where/group in the `OpusTags` comments. A separate writer thread and
  a periodic retention walk keep the tick free of filesystem work.

## Config (`[plugins.voice]`)

The two UDP listeners are **opt-in** — nothing binds until you enable one.

| key | default | meaning |
|---|---|---|
| `svc_enabled` / `svc_port` | `false` / `24454` | the Simple Voice Chat UDP server |
| `pv_enabled` / `pv_port` | `false` / `60606` | the Plasmo Voice UDP server |
| `distance` / `whisper` | `48` / `24` | proximity metres |
| `groups_enabled` | `true` | offer SVC groups |
| `admin_permission` | `aether.voice.admin` | the node `/voice` checks |
| `record_dir` | `voice-recordings` | where recordings go |
| `notify_recorded` | `true` | tell a player they are recorded |
| `retention_days` / `max_disk_mb` | `0` / `0` | `0` keeps everything |

## Tests

`cargo-locked test -p aether-voice-plugin` — the protocol round-trips, the AES
and MD5 vectors, the checksummed Ogg writer, an end-to-end bridge test that
carries Opus SVC→PV and PV→SVC, and server-process tests that pin the control
packets (PV `PlayerInfoRequest`/`Connection`/`Ping`/`SourceInfo`), the
per-sender rate limit and the inbound-queue cap.

## Limits

See [KNOWN_ISSUES.md §21](../../docs/KNOWN_ISSUES.md). In short: SVC groups are
in memory and non-persistent; PV RSA key exchange and end-to-end PV audio
encryption are not implemented (PV audio is forwarded as sent); custom-payload
transport is wired for protocols 764–775 (26.2/26.3 not yet); Opus is a
pass-through with no transcoding needed by design.

