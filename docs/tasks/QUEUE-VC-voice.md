# CH1 — chat logging + history (core, small)
Every chat message, private message (/msg, /tell, /w), and command typed by players is written to the server log
(`[chat] <name> text`, `[pm] a -> b: text`, `[cmd] name: /line`) and journalled as an event (actor, time, position,
text) so PA2/web admin can show it. Config `[chat] log = true`, `log_commands = true`, redact list for commands with
secrets (`/login`, `/register`, `/l`, `/reg` args masked). Tests; docs EN+RU.

# VC1 — voice chat: Simple Voice Chat + Plasmo Voice, mutual hearing (native plugin `plugins/voice`)
Read `docs/tasks/COMMON.md`, `docs/design/plugins.md`, `docs/design/plugin-sdk.md`.
## Goal
Players with **Simple Voice Chat** (henkelmax, Fabric/Forge/NeoForge/Paper protocol) and players with **Plasmo Voice**
(v2.x) on the same server hear each other. Server implements both server sides natively (no Bukkit):
- SVC: plugin-channel handshake (`voicechat:*` channels), secret exchange, UDP voice server (own port, default 24454),
  encrypted packets (AES), proximity audio, groups (create/join/password/types), player state broadcast.
- Plasmo Voice: its plugin channel handshake, UDP server (own port, default 60606), source/line model, activations,
  proximity, encryption as per its protocol.
- **Bridge**: decode both into one internal frame (Opus payload + sender + position + distance/whisper flags);
  Opus is not re-encoded when forwarding where formats match; otherwise transcode via Opus (pure-Rust or libopus
  binding, justify choice). Proximity distance per config; groups mapped across (SVC group ↔ PV group source).
- Verify protocol details against the mods' public source (versions pinned in notes), with captured-packet fixtures.
## Admin features
- Operators (permission `aether.voice.admin`) can **join any group without the password** and listen silently or speak
  (`/voice join <group>`, `/voice spy <player|group>`, `/voice leave`).
- **Recording**: per player / per group / global, toggled by admins (`/voice record start|stop <player|group|all>`),
  saved as Ogg/Opus per speaker per session under `[voice] record_dir` with metadata (who, when, where, group),
  retention days, max disk. Recording is **off by default**; config `notify_recorded = true` (default) shows players a
  notice/actionbar that they are being recorded (legal requirement in many jurisdictions — keep it on by default and
  document it).
- Web admin hook (later): list/download recordings for operators only.
## Rules
Native plugin on the SDK; UDP servers off the tick thread; rate limits per sender; no secrets in git; tests per
protocol (handshake fixtures, encryption round-trip, bridge SVC→PV and PV→SVC), docs EN+RU, KNOWN_ISSUES.
Split into VC1a (SVC), VC1b (Plasmo), VC1c (bridge + admin + recording) commits on one branch oc/VC1.
