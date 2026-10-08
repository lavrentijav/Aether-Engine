//! Server configuration (TOML).

use serde::Deserialize;

/// `[server]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Bind address. Defaults to loopback so the offline, unauthenticated
    /// preview server is not exposed to the network unless you opt in.
    pub host: String,
    /// Listen port (vanilla default 25565).
    pub port: u16,
    /// Message-of-the-day shown in the server list.
    pub motd: String,
    /// Reported max player slots.
    pub max_players: u32,
    /// The largest chunk radius streamed to any player (a `(2r+1)²` grid).
    ///
    /// Each player gets the smaller of this and their client's own view
    /// distance (plus one ring, which the client needs to draw its edge): a
    /// client set to 12 is never sent 64. Read through
    /// [`ServerConfig::radius_for`], which keeps it within `2..=`
    /// [`MAX_VIEW_RADIUS`].
    pub view_radius: i32,
    /// How many chunk columns may stay in memory at once; past it, the least
    /// recently used of those no player needs go first, then the least
    /// recently used of the rest. `0` sets no limit: columns still leave
    /// memory once no player is near them.
    ///
    /// A column is a few to a few tens of kilobytes depending on the terrain;
    /// the startup log prints an estimate for `view_radius`.
    pub max_resident_columns: usize,
    /// Body size, in bytes, at or above which a packet is deflated.
    ///
    /// Negative disables compression entirely, matching vanilla's
    /// `network-compression-threshold`. This matters far more than it looks:
    /// a chunk column is mostly runs of identical light bytes, which deflate
    /// to almost nothing.
    pub compression_threshold: i32,
    /// `survival` or `creative`.
    ///
    /// Parsed at load rather than kept as a string, so a typo is a startup
    /// error and not a server that silently runs the wrong mode.
    #[serde(deserialize_with = "de_game_mode")]
    pub game_mode: crate::protocol::GameMode,
    /// Directory holding the game's `data/minecraft/worldgen/`.
    ///
    /// The operator's own copy, read at run time and never vendored into this
    /// repository. Empty means the engine's built-in noise terrain; a copy
    /// that is set but cannot be read stops the server at startup.
    pub worldgen_data: String,
    /// The game's `reports/biome_parameters/minecraft/overworld.json`.
    ///
    /// Separate from [`Self::worldgen_data`] because it is not in the data
    /// pack: the overworld's biome table is code in the game, and exists on
    /// disk only in a `--reports` dump. Optional: the generator carries a
    /// port of that table, and uses the report only when one is given.
    #[serde(default)]
    pub biome_data: String,
    /// World seed for the terrain generator.
    pub seed: u64,
    /// Directory holding the persistent world. Created on first run.
    pub world_dir: String,
    /// Seconds between autosaves. Edits are flushed to disk on this tick.
    pub autosave_secs: u64,
    /// Seconds the game tick may stall before the server exits so a
    /// supervisor can restart it; a warning is logged long before. 0 only
    /// warns.
    pub watchdog_secs: u64,
    /// Seconds between status lines in the log (players, tick rate, send
    /// backlog). 0 disables them.
    pub status_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 25565,
            motd: "Aether Engine — 1.8.9 demo (full-bright noise terrain)".to_string(),
            max_players: 20,
            view_radius: 8,
            max_resident_columns: 0,
            compression_threshold: 256,
            game_mode: crate::protocol::GameMode::Creative,
            worldgen_data: String::new(),
            biome_data: String::new(),
            seed: 42,
            world_dir: "world".to_string(),
            autosave_secs: 30,
            watchdog_secs: 60,
            status_secs: 300,
        }
    }
}

/// The largest view radius the server accepts. Vanilla clients draw at most
/// 32; this leaves room for the ones that draw further.
pub const MAX_VIEW_RADIUS: i32 = 128;

impl ServerConfig {
    /// The configured view radius, within `2..=MAX_VIEW_RADIUS`.
    pub fn view_radius(&self) -> i32 {
        self.view_radius.clamp(2, MAX_VIEW_RADIUS)
    }

    /// The radius to stream to a client whose own view distance is `client`
    /// (`None` until it says): never more than the server's, and one ring
    /// past the client's, which it needs in order to draw its outermost one.
    pub fn radius_for(&self, client: Option<u8>) -> i32 {
        let server = self.view_radius();
        match client {
            Some(c) => (c as i32 + 1).clamp(2, server),
            None => server,
        }
    }

    /// The compression threshold as the wire layer wants it: `None` when
    /// compression is switched off.
    pub fn compression(&self) -> Option<usize> {
        usize::try_from(self.compression_threshold).ok()
    }
}

/// `[resource_pack]` settings.
///
/// Offering a pack is how a server makes blocks it has no vanilla equivalent
/// for look right on an older client, alongside the codec's block
/// substitution. Disabled unless `url` is set; the server only *points* at a
/// pack, it never hosts one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ResourcePackConfig {
    /// Download URL. Empty disables the offer entirely.
    pub url: String,
    /// SHA-1 of the pack file, lowercase hex. Clients cache on this.
    pub hash: String,
    /// Whether the client must accept the pack to keep playing.
    pub required: bool,
}

impl ResourcePackConfig {
    /// Whether a pack should be offered at all.
    pub fn enabled(&self) -> bool {
        !self.url.is_empty()
    }
}

/// Whole server config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server settings.
    pub server: ServerConfig,
    /// Optional resource pack offered on join.
    pub resource_pack: ResourcePackConfig,
    /// Optional history mirror.
    pub database: DatabaseConfig,
    /// Who may run administrative commands: `name`, or `name@address` to
    /// require that the player also connects from that IP address.
    ///
    /// Names rather than UUIDs because this server is offline-mode and has no
    /// UUIDs worth trusting — an offline UUID is derived from the name. A bare
    /// name is therefore a claim anyone can make; bind it to an address on any
    /// server reachable from outside. See [`Self::is_operator`].
    pub operators: Vec<String>,
    /// Names allowed to join at all. Empty lets anyone in.
    pub whitelist: Vec<String>,
    /// The generated-column cache. See [`crate::gencache`].
    #[serde(default)]
    pub cache: CacheSection,
}

impl Config {
    /// Whether `name`, connecting from `ip`, is an operator. Names compare
    /// case-insensitively; an entry with an address also needs that address.
    pub fn is_operator(&self, name: &str, ip: Option<std::net::IpAddr>) -> bool {
        self.operators
            .iter()
            .any(|entry| match entry.split_once('@') {
                Some((n, addr)) => {
                    n.eq_ignore_ascii_case(name)
                        && ip.is_some_and(|ip| addr.trim().parse::<std::net::IpAddr>() == Ok(ip))
                }
                None => entry.eq_ignore_ascii_case(name),
            })
    }

    /// Whether `name` may join.
    pub fn may_join(&self, name: &str) -> bool {
        self.whitelist.is_empty() || self.whitelist.iter().any(|w| w.eq_ignore_ascii_case(name))
    }

    /// Whether anyone on the network can reach this server.
    pub fn is_public(&self) -> bool {
        !matches!(self.server.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Operator entries that are a bare name, which anyone can claim on an
    /// offline server.
    pub fn unbound_operators(&self) -> Vec<&str> {
        self.operators
            .iter()
            .filter(|o| !o.contains('@'))
            .map(String::as_str)
            .collect()
    }

    /// Apply the `AETHER_*` environment overrides, so a deployment can keep
    /// machine-specific values out of the file. Blank values are ignored.
    pub fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        let get = |k: &str| {
            var(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let list = |v: String| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        if let Some(v) = get("AETHER_HOST") {
            self.server.host = v;
        }
        if let Some(v) = get("AETHER_PORT") {
            self.server.port = v
                .parse()
                .map_err(|_| format!("AETHER_PORT: not a port: {v}"))?;
        }
        if let Some(v) = get("AETHER_WORLD_DIR") {
            self.server.world_dir = v;
        }
        if let Some(v) = get("AETHER_WORLDGEN_DATA") {
            self.server.worldgen_data = v;
        }
        if let Some(v) = get("AETHER_CACHE_DIR") {
            self.cache.dir = v;
        }
        if let Some(v) = get("AETHER_OPERATORS") {
            self.operators = list(v);
        }
        if let Some(v) = get("AETHER_WHITELIST") {
            self.whitelist = list(v);
        }
        Ok(())
    }
}

/// Parse `game_mode` from its name.
///
/// At load rather than at use, so a typo is a startup error and not a server
/// that silently runs the wrong mode for a week.
fn de_game_mode<'de, D>(d: D) -> Result<crate::protocol::GameMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    String::deserialize(d)?
        .parse()
        .map_err(serde::de::Error::custom)
}

/// `[database]` settings: where to mirror the world history for querying.
///
/// Off unless `url` is set. The mirror is derived from the journal and never
/// authoritative, so a database that is missing, slow or broken degrades the
/// audit trail and nothing else — see [`crate::db`].
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// libpq connection string. Empty disables mirroring.
    ///
    /// Read through [`DatabaseConfig::connection_url`], which prefers the
    /// environment: a connection string carries a password and this file is
    /// in version control.
    pub url: String,
    /// Write as soon as this many events are waiting.
    pub batch_size: usize,
    /// ...or this many milliseconds after the first one arrived.
    pub batch_delay_ms: u64,
    /// How many events may wait before submissions are dropped and counted.
    /// The queue exists so that a database stall never reaches the game
    /// thread; making it deeper buys tolerance for longer stalls, and costs
    /// memory.
    pub queue_depth: usize,
}

/// Environment variable that overrides `[database] url`.
pub const DATABASE_URL_ENV: &str = "AETHER_DATABASE_URL";

impl DatabaseConfig {
    /// The connection string to actually use.
    ///
    /// The environment wins over the file, and an empty value in either place
    /// means "not configured" rather than "connect with defaults" — libpq
    /// would happily interpret an empty string as a connection to the local
    /// socket as the current user, which is not something a server should do
    /// because a setting was left blank.
    pub fn connection_url(&self) -> String {
        match std::env::var(DATABASE_URL_ENV) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => self.url.clone(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            batch_size: 500,
            batch_delay_ms: 1000,
            queue_depth: 100_000,
        }
    }
}

/// `[cache]` settings: the disk cache for unmodified generated columns.
///
/// Purely a speed feature — every entry is reproducible from the seed, so the
/// directory can be deleted at any time and nothing is lost. See
/// [`crate::gencache`] for why that makes it safe to be careless with.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct CacheSection {
    /// Where the cache lives. Empty disables it.
    pub dir: String,
    /// How long an entry stays usable, in hours.
    pub ttl_hours: u64,
    /// How many entries to keep before the oldest are swept.
    pub max_entries: usize,
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            dir: "world-cache".into(),
            ttl_hours: 24,
            max_entries: 20_000,
        }
    }
}

impl CacheSection {
    /// The shape [`crate::gencache`] wants.
    pub fn to_config(&self) -> crate::gencache::CacheConfig {
        crate::gencache::CacheConfig {
            root: self.dir.clone(),
            // A zero here would expire every entry the instant it was
            // written, which reads as "the cache does nothing" rather than
            // "the cache is off"; `dir = ""` is how it is turned off.
            ttl: std::time::Duration::from_secs(self.ttl_hours.max(1) * 3600),
            max_entries: self.max_entries.max(1),
        }
    }
}

/// Sample written when the config file is missing.
pub const SAMPLE: &str = r#"# Aether Engine server configuration.
#
# Most settings can also come from the environment, which wins over this file:
# AETHER_HOST, AETHER_PORT, AETHER_WORLD_DIR, AETHER_WORLDGEN_DATA,
# AETHER_CACHE_DIR, AETHER_OPERATORS, AETHER_WHITELIST, AETHER_DATABASE_URL.

# Who may run administrative commands: "name", or "name@1.2.3.4" to also
# require that address. This server is offline-mode, so a bare name is a claim
# anyone can make: on a server reachable from outside, bind every operator to
# an address. AETHER_OPERATORS (comma-separated) overrides this list.
operators = []
# Names allowed to join; empty lets anyone in. AETHER_WHITELIST overrides it.
whitelist = []

[server]
# Loopback by default: this is an offline, unauthenticated preview server.
# Set host = "0.0.0.0" only if you understand it will accept LAN/internet peers.
host = "127.0.0.1"
port = 25565
motd = "Aether Engine — 1.8.9 demo (full-bright noise terrain)"
max_players = 20
view_radius = 8       # largest chunk radius sent to a player (each gets min(this, their own + 1))
max_resident_columns = 0  # columns kept in memory at most, least recently used go first; 0 = no limit
compression_threshold = 256  # deflate packets this size or larger; -1 disables
game_mode = "creative"  # survival: blocks drop and pay; creative: build freely
# The directory holding the game's data pack (it contains data/minecraft/),
# read at run time. When set, the world is vanilla terrain: biomes, surface,
# caves, ores, trees and plants, and the server refuses to start if it cannot
# read it. Empty uses the built-in noise terrain.
# tools/fetch-vanilla-data.sh fetches one and prints this line.
worldgen_data = ""
# Optional: the game's reports/biome_parameters/minecraft/overworld.json.
# The biome table is built in; set this only to override it with a report.
biome_data = ""
seed = 42             # world seed for the terrain generator
world_dir = "world"   # persistent world directory, created on first run
autosave_secs = 30    # how often block edits are flushed to disk
watchdog_secs = 60    # exit (for a supervisor to restart) if the tick stalls this long; 0 only warns
status_secs = 300     # a status line in the log this often; 0 disables

[resource_pack]
# Offered on join so older clients can render blocks their version lacks.
# Leave url empty to offer nothing. The server only points at the pack.
url = ""
hash = ""
required = false

[database]
# Mirror the world history into PostgreSQL for querying (needs
# `--features postgres`). Leave url empty to disable. The mirror is derived
# from the on-disk journal, so losing it never risks the world.
#
# A connection string contains a password, so prefer the AETHER_DATABASE_URL
# environment variable, which overrides this.
url = ""
batch_size = 500        # write as soon as this many events are waiting
batch_delay_ms = 1000   # ...or this long after the first one arrived
queue_depth = 100000    # events that may wait before submissions are dropped

[cache]
# Unmodified generated columns are cached on disk so the generator does not
# recompute the same terrain on every restart. Pure speed: every entry is
# reproducible from the seed, so deleting this directory loses nothing.
dir = "world-cache"     # empty disables the cache
ttl_hours = 24          # entries older than this are regenerated
max_entries = 20000     # above this the oldest are swept away, hourly
"#;

impl Config {
    /// Load config from `path`, writing a sample and using defaults if absent.
    pub fn load_or_init(path: &str) -> Result<(Config, bool), String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg = toml::from_str(&text).map_err(|e| format!("parsing {path}: {e}"))?;
                Ok((cfg, false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Report whether the sample was actually written: a failed write
                // (e.g. a read-only directory) must not claim a file was created.
                let wrote = std::fs::write(path, SAMPLE).is_ok();
                Ok((Config::default(), wrote))
            }
            Err(e) => Err(format!("reading {path}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    /// TOML binds a bare key to the table header above it. `operators` sits
    /// at the root of [`Config`], so writing it at the *bottom* of the file —
    /// which reads naturally and is where it started — makes it
    /// `database.operators`, an unknown key that serde quietly drops. The
    /// list then parses as empty and every administrative command answers
    /// "that command is for operators", including to the operator. Found on a
    /// live server, not here.
    #[test]
    fn the_sample_puts_the_operator_list_where_toml_will_read_it() {
        let cfg: super::Config =
            toml::from_str(&super::SAMPLE.replace("operators = []", r#"operators = ["root"]"#))
                .expect("the sample must parse");
        assert_eq!(cfg.operators, vec!["root".to_string()]);

        // And the shape that caused it must still be wrong, or the test is
        // asserting nothing.
        let broken = format!(
            "{}\noperators = [\"root\"]\n",
            super::SAMPLE.replace("operators = []\n", "")
        );
        let cfg: super::Config = toml::from_str(&broken).expect("parses, wrongly");
        assert!(
            cfg.operators.is_empty(),
            "a key under [database] is not the root list — if this ever becomes \
             an error, the guard above can go"
        );
    }

    use super::*;

    #[test]
    fn sample_parses() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.server.port, 25565);
        assert_eq!(cfg.server.view_radius, 8);
        assert_eq!(cfg.server.max_resident_columns, 0);
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.world_dir, "world");
        assert_eq!(cfg.server.autosave_secs, 30);
    }

    #[test]
    fn the_environment_overrides_the_file_but_an_empty_value_does_not() {
        // An empty override must not read as "connect with defaults": libpq
        // treats an empty connection string as the local socket as the current
        // user, so a blank variable would silently start mirroring somewhere
        // nobody chose.
        let cfg = DatabaseConfig {
            url: "host=from-file".into(),
            ..Default::default()
        };
        // SAFETY: single-threaded test, and the variable is read only here.
        unsafe { std::env::remove_var(DATABASE_URL_ENV) };
        assert_eq!(cfg.connection_url(), "host=from-file");
        // SAFETY: as above.
        unsafe { std::env::set_var(DATABASE_URL_ENV, "   ") };
        assert_eq!(
            cfg.connection_url(),
            "host=from-file",
            "blank is not an override"
        );
        // SAFETY: as above.
        unsafe { std::env::set_var(DATABASE_URL_ENV, "host=from-env") };
        assert_eq!(cfg.connection_url(), "host=from-env");
        // SAFETY: as above.
        unsafe { std::env::remove_var(DATABASE_URL_ENV) };
    }

    #[test]
    fn operators_bound_to_an_address_need_that_address() {
        let cfg = Config {
            operators: vec!["Root".into(), "admin@10.0.0.7".into()],
            ..Default::default()
        };
        let ip = |s: &str| Some(s.parse().unwrap());
        assert!(
            cfg.is_operator("root", None),
            "a bare name is enough for a bare entry"
        );
        assert!(cfg.is_operator("ADMIN", ip("10.0.0.7")));
        assert!(
            !cfg.is_operator("admin", ip("10.0.0.8")),
            "another address is not"
        );
        assert!(!cfg.is_operator("admin", None));
        assert!(!cfg.is_operator("someone", ip("10.0.0.7")));
        assert_eq!(cfg.unbound_operators(), vec!["Root"]);
    }

    #[test]
    fn an_empty_whitelist_lets_everyone_in() {
        let mut cfg = Config::default();
        assert!(cfg.may_join("anyone"));
        cfg.whitelist = vec!["Alice".into()];
        assert!(cfg.may_join("alice"));
        assert!(!cfg.may_join("mallory"));
    }

    #[test]
    fn the_environment_overrides_settings_and_ignores_blanks() {
        let mut cfg = Config::default();
        let env = |k: &str| match k {
            "AETHER_PORT" => Some("25999".to_string()),
            "AETHER_OPERATORS" => Some("a@1.2.3.4, b ,".to_string()),
            "AETHER_WORLD_DIR" => Some("   ".to_string()),
            _ => None,
        };
        cfg.apply_env(env).unwrap();
        assert_eq!(cfg.server.port, 25999);
        assert_eq!(cfg.operators, vec!["a@1.2.3.4", "b"]);
        assert_eq!(cfg.server.world_dir, "world", "blank is not an override");
        assert!(cfg
            .apply_env(|k| (k == "AETHER_PORT").then(|| "x".to_string()))
            .is_err());
    }

    #[test]
    fn mirroring_is_off_unless_it_is_configured() {
        // SAFETY: single-threaded test, and the variable is read only here.
        unsafe { std::env::remove_var(DATABASE_URL_ENV) };
        assert!(DatabaseConfig::default().connection_url().is_empty());
    }

    #[test]
    fn the_checked_in_toml_names_every_setting_the_sample_does() {
        // The example is what an operator copies to aether-server.toml (which
        // is not tracked); a setting added to SAMPLE and forgotten there would
        // be one a fresh deployment silently runs without.
        let checked_in = include_str!("../../../aether-server.example.toml");
        let keys = |src: &str| -> Vec<String> {
            src.lines()
                .map(str::trim)
                .filter(|l| !l.starts_with('#') && l.contains('='))
                .map(|l| l.split('=').next().unwrap().trim().to_owned())
                .collect()
        };
        for key in keys(SAMPLE) {
            assert!(
                keys(checked_in).contains(&key),
                "aether-server.example.toml is missing `{key}`, which SAMPLE documents"
            );
        }
        // Both must still parse into the same shape.
        toml::from_str::<Config>(checked_in).expect("checked-in config must parse");
    }

    #[test]
    fn a_client_gets_the_smaller_radius_plus_its_edge_ring() {
        let mut cfg = Config::default();
        cfg.server.view_radius = 64;
        assert_eq!(cfg.server.radius_for(None), 64, "until the client says");
        assert_eq!(cfg.server.radius_for(Some(12)), 13);
        assert_eq!(
            cfg.server.radius_for(Some(127)),
            64,
            "never past the server's"
        );
        assert_eq!(cfg.server.radius_for(Some(0)), 2);
        cfg.server.view_radius = 1_000;
        assert_eq!(cfg.server.view_radius(), MAX_VIEW_RADIUS);
    }
}
