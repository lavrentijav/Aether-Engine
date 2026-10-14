# Deploying `aether-server`

Files in this directory:

- [`aether.service`](aether.service) — a systemd unit for the server.
- [`aether.env.example`](aether.env.example) — the `AETHER_*` environment file the unit
  reads, for machine-specific settings and secrets.

The full configuration reference is the commented
[`aether-server.example.toml`](../aether-server.example.toml); the unit itself is the
authoritative list of steps, repeated here.

## Setup

```sh
# 1. Build the release binary (add --features postgres for the history mirror/economy).
cargo build --release -p aether-server

# 2. A system user and its home.
sudo useradd --system --home /opt/aether --shell /usr/sbin/nologin aether
sudo install -d -o aether -g aether /opt/aether

# 3. The binary and a copy of the config (git ignores aether-server.toml).
sudo install -m 755 target/release/aether-server /opt/aether/
sudo install -o aether -m 640 aether-server.example.toml /opt/aether/aether-server.toml

# 4. Environment: install and edit it (contains secrets, so mode 600).
sudo install -m 600 deploy/aether.env.example /etc/aether.env

# 5. The unit.
sudo install -m 644 deploy/aether.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now aether
```

## Operating

- Logs: `journalctl -u aether -f`.
- Stop: `systemctl stop aether`. The unit sends **SIGINT**, so the server saves its
  players and flushes the world before exiting; `TimeoutStopSec=60` allows for it.
- `Restart=on-failure` restarts after a crash and after the tick watchdog
  (`watchdog_secs`) exits.
- The world and its cache are the only things it writes; `ReadWritePaths=/opt/aether`,
  `ProtectSystem=strict` and `ProtectHome=true` confine it.

## Notes

- The server is **offline mode**: there is no authentication. Before exposing it beyond
  loopback, bind each operator to an address (`AETHER_OPERATORS=name@203.0.113.7`) or set
  a `whitelist`; the server warns at startup when it is exposed without either.
- `AETHER_DATABASE_URL` is the preferred way to supply the PostgreSQL connection string,
  since a connection string contains a password.
- The server refuses to start if `worldgen_data` is set but the data pack is unreadable,
  and if a database is configured for a build without the `postgres` feature.
