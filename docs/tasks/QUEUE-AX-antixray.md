# AX1 — anti-xray (visibility culling), optional native module

Read `docs/tasks/COMMON.md`, `docs/design/visibility-culling.md` (the design — follow it), `docs/design/plugins.md`.

## Goal
Ore/x-ray cheats must see nothing useful. Off by default, `[anti_xray]` in config.
- Mode `engine` (cheap): hidden ores in fully enclosed positions are sent as the surrounding stone/deepslate/netherrack; revealed on neighbour change (break/explosion/piston) by resending the real block.
- Mode `culling` (design doc): section visibility graph + BFS from the player with a margin; invisible sections sent as stone; re-send on move/turn before the player can see them (no pop-in in normal play).
- Configurable hidden-block list (default: all ores, ancient debris, spawners, chests in worldgen structures off).
- Every protocol; per-world toggle; operator bypass permission `aether.antixray.bypass`.
## Done when
Unit tests for enclosure/BFS; a test client (MCC on fi2 is fine via V1 lane) reading chunk data near ores sees stone; mining reveals neighbours correctly; benchmark: chunk-send cost overhead reported (target <15% in engine mode). Docs: ROADMAP/STATUS/KNOWN_ISSUES EN+RU.
