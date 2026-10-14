# Queue E — 1.8.9 tab list and old-codec gaps (crate aether-server, `protocol/v47`)
E1 Player List Item (0x38) for protocol 47: add/remove players with binary UUID + GameProfile
properties (offline: no skin properties), sent on join/leave so players are tab-listed
(KNOWN_ISSUES item 5). Unit test the exact byte encoding against the wiki.vg 1.8 layout.
E2 `KNOWN_ISSUES` item 7 (1.17 chunk encoding rejected): find and fix the section block-count
encoding in the v755/v756 codecs; add a byte-level test of one section.
