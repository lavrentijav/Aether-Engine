# Queue I — status effects (crate aether-server, `game/`)
KNOWN_ISSUES item 11: status effects are not applied; foods restore hunger only.
I1: effect model (id, amplifier, duration, ambient/particles flags) on players and mobs, ticking,
expiry, the protocol packets (Update Mob Effect / Remove Mob Effect) for 1.21.11 and 26.x codecs.
I2: sources as vanilla: golden apple / enchanted golden apple, poisonous potato, spider eye,
pufferfish, rotten flesh (hunger), suspicious stew; effects that matter server-side applied:
regeneration, poison (not below 1 HP), wither, hunger, absorption, resistance, fire resistance,
speed/slowness (movement attribute sent to client). Persist active effects with player state
(bump its format version). Tests per effect.
