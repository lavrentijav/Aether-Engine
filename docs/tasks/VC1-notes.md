# VC1 — voice chat: Simple Voice Chat + Plasmo Voice (notes)

Branch `oc/VC1` from `main`. Split into VC1a (SVC), VC1b (Plasmo), VC1c
(bridge + admin + recording). One native plugin, `plugins/voice`.

## Versions pinned

- **Simple Voice Chat** `henkelmax/simple-voice-chat`, branch **`1.21.11`**
  (`common/src/main/java/de/maxhenkel/voicechat/**`). Compatibility version 18
  (the client sends it in `voicechat:request_secret`; the server echoes the
  same value in `voicechat:secret` or the client refuses). UDP default 24454,
  `max_voice_distance` 48, `whisper_distance` 24, `keep_alive` 1000 ms,
  `mtu_size` 1275, codec `VOIP`. Crypto `AES/GCM/NoPadding`, 16-byte secret,
  12-byte IV, 128-bit tag. Magic byte `0xFF`.
- **Plasmo Voice** `plasmoapp/plasmo-voice` (v2.x). Plugin channel
  `plasmo:voice/v2` (`…/installed`, `…/service`). UDP magic `0x4e9004e9`, UDP
  default is "same as the Minecraft server" in the mod; the plugin default is
  `60606`. Crypto `AES/CBC/PKCS5Padding` (key RSA-wrapped per client), UDP
  packet = `[magic][type][secret UUID][timestamp][body]`.

Protocol details cross-checked against the sources; the fixtures are the
in-tree round-trip tests (bytes in/out) rather than captured pcap files.

## What this slice does

- Protocol libraries (`svc.rs`, `plasmo.rs`), crypto (`crypto.rs`: AES-128-GCM,
  AES-128-CBC, MD5 for the PV name UUIDs — all against published vectors),
  byte codec (`bytes.rs`).
- Native plugin (`lib.rs`): two UDP servers off the tick, the plugin-channel
  handshakes, proximity/group routing through `frame::route`, `/voice` admin
  commands, and Ogg/Opus recording (`record.rs`).
- Core transport: `ClientEvent::PluginMessage` / `ServerEvent::PluginMessage`,
  the `custom_payload` packet id per release, `NativePlugin::on_plugin_message`,
  and `HostBackend` send/uuid/position/disconnect hooks.

## Review round 1 fixes (oc/VC1, merge with main included)

- **PV control packets**: the server sends `PlayerInfoRequest` on join with the
  mod's retry schedule, answers `Connection` with `ip = "0.0.0.0"`, sends the
  periodic UDP `Ping`, and answers `SourceInfoRequest` with a `SourceInfoPacket`.
- **SVC keepalive**: a periodic `KeepAlive` after the connection check, so the
  client's 10 s timeout no longer drops it.
- **Admin**: `/voice join` bypasses a group password; `/voice spy
  <player|group|all|off>` copies matching frames to the admin's own client;
  `/voice record <player>` resolves the real online uuid.
- **Safety**: per-sender token buckets; a capped inbound queue that drops the
  oldest frame; the PV secret index is O(1). Recording and retention moved off
  the tick onto their own threads (a bounded writer channel).
- **Opt-in listeners**: `svc_enabled`/`pv_enabled` default to `false`; the
  misleading `pv_aes_key` key was removed.
- New process tests drive the plugin with a mock host backend and pin the
  control packets, the rate limit and the queue cap.

## Deliberately not done (see KNOWN_ISSUES §25)

- Custom-payload transport is wired for protocols 764–775. **26.2/26.3 are
  `-1`**: `minecraft-data` 3.117.0 has 26.1 (775) but not 26.2/26.3, and the
  ids were not guessed. Wire them by regenerating `gen/` from the release
  reports (`tools/gen/regen.sh`) once the versions are fetched.
- PV RSA key exchange / end-to-end audio encryption: the config is sent
  without an `EncryptionInfo`, so PV audio is forwarded as received.
- SVC groups are non-persistent and unvalidated beyond the read bounds.
- No sequence replay/jitter buffers; Opus passes through untouched (both mods
  use it, so no transcode is required by design).

## Checks

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4 --
-D warnings`, and `cargo-locked test` for `aether-voice-plugin` (51 unit + 4
bridge), `aether-plugin`, and `aether-server --lib` (1151). All green at the
branch tip.

