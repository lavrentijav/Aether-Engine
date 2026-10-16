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

## Deliberately not done (see KNOWN_ISSUES §21)

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

`cargo fmt --all -- --check`, `cargo clippy -p aether-voice-plugin -p
aether-plugin -p aether-server --all-targets -- -D warnings`, and
`cargo-locked test` for `aether-voice-plugin` (40), `aether-plugin`, and
`aether-server --lib` (1087). All green at the branch tip.
