# Headless client probes

Small [mineflayer](https://github.com/PrismarineJS/mineflayer) scripts that connect a
**real** 1.21.11 client to a locally running `aether-server` and report what the server
actually sent — as opposed to what the server's own tests believe it sent.

They are not a test suite. They are the thing you reach for when a live client behaves
oddly and you want evidence rather than a theory. In their first run they found two
defects nothing else had:

* every block outside a seven-entry table was placed as **stone**, because that was the
  fallback at the end of the item→block chain;
* the server never sent `set_health`, which a vanilla client tolerates and every bot
  library does not — mineflayer will not emit `spawn` without it.

## Running

```sh
npm install mineflayer          # not vendored; this directory is scripts only
node pkt.js                     # which play packets arrive, and does the client spawn
node states.js                  # place state-carrying blocks, report what came back
node states3.js                 # does the state follow yaw / clicked face / neighbours
```

The server must be listening on `127.0.0.1:25565` in offline mode.

`states3.js` searches outward from the spawn for dry, solid ground before it builds —
the spawn is often over ocean, and placing into water reports `water` for everything and
looks like a much worse bug than it is.
