# V1 live run

- Date: 2026-10-15 05:32:52 UTC
- Target: fi2.walk.boykisser.online:25565, account _alpha_01
- Versions: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2

| scenario | 1.21.11 | 1.21.4 | 1.20.6 | 1.20.4 | 1.20.2 |
| --- | --- | --- | --- | --- | --- |
| join | PASS | PASS | PASS | PASS | PASS |
| inspect | PASS | PASS | PASS | PASS | PASS |
| place-break-rollback | PASS | PASS | PASS | PASS | PASS |
| inventory-persist | PASS | PASS | PASS | PASS | PASS |
| eat-effects | PASS | PASS | PASS | PASS | FAIL |
| container | PASS | PASS | PASS | PASS | PASS |
| potion | PASS | PASS | PASS | PASS | PASS |
| commands | PASS | PASS | PASS | PASS | PASS |
| multi-version-join | PASS | PASS | PASS | PASS | PASS |

## Failures

### FAIL eat-effects 1.20.2 missing expected /Regeneration|Absorption/

```
Minecraft Console Client v26.2 - for MC 1.4.6 to 26.2 - Github.com/MCCTeam
Resolving fi2.walk.boykisser.online...
You chose to run in offline mode.
Using Minecraft version 1.20.2 (protocol v764)
[MCC] Version is supported.
Logging in...
[MCC] Server is in offline mode.
[MCC] Server was successfully joined.
Type '/quit' to leave the server.
You're now under a Health Boost effect (Duration: 1m 51s).
▌_alpha_01 is now in Creative mode
[MCC] Requested GoldenApple x1 in slot #36
[MCC] Changed to slot 1
▌Teleported to 0 200 0
[MCC] Looking Up
[MCC] Used an item
You're now under a Nausea II effect (Duration: 5s).
[MCC] Active effects:
- Nausea II (2s)
- Health Boost (1m 57s)
Effect Nausea II has expired
The current setting is saved as MinecraftClient.ini
```
