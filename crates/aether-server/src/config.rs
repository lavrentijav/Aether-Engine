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
    /// Chunk radius kept loaded around each player (a `(2r+1)²` grid).
    pub view_radius: i32,
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
    /// repository. Empty falls back to the engine's built-in noise terrain, as
    /// does a copy that cannot be read — a world that generates beats a server
    /// that will not start.
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 25565,
            motd: "Aether Engine — 1.8.9 demo (full-bright noise terrain)".to_string(),
            max_players: 20,
            view_radius: 8,
            compression_threshold: 256,
            game_mode: crate::protocol::GameMode::Creative,
            worldgen_data: String::new(),
            biome_data: String::new(),
            seed: 42,
            world_dir: "world".to_string(),
            autosave_secs: 30,
        }
    }
}

impl ServerConfig {
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
    /// Names allowed to run administrative commands.
    ///
    /// Names rather than UUIDs because this server is offline-mode and has no
    /// UUIDs worth trusting; that is also why the list is empty by default.
    /// Anyone who can pick a username can claim one on an offline server, so
    /// filling this in is a decision the operator makes knowingly.
    pub operators: Vec<String>,
    /// The generated-column cache. See [`crate::gencache`].
    #[serde(default)]
    pub cache: CacheSection,
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
pub const SAMPLE: &str = r#"# Aether Engine server configuration (Minecraft 1.8.9 / protocol 47).

# Names allowed to run administrative commands (/econ give). Empty by default:
# this server is offline-mode, so a username is a claim and not an identity.
operators = []

[server]
# Loopback by default: this is an offline, unauthenticated preview server.
# Set host = "0.0.0.0" only if you understand it will accept LAN/internet peers.
host = "127.0.0.1"
port = 25565
motd = "Aether Engine — 1.8.9 demo (full-bright noise terrain)"
max_players = 20
view_radius = 8       # chunk radius kept loaded around each player
compression_threshold = 256  # deflate packets this size or larger; -1 disables
game_mode = "creative"  # survival: blocks drop and pay; creative: build freely
# The game's own data pack (data/minecraft/), read at run time. When set,
# the world is vanilla terrain: biomes, surface, caves, ores, trees and
# plants. Empty uses the built-in noise terrain.
worldgen_data = ""
# Optional: the game's reports/biome_parameters/minecraft/overworld.json.
# The biome table is built in; set this only to override it with a report.
biome_data = ""
seed = 42             # world seed for the terrain generator
world_dir = "world"   # persistent world directory, created on first run
autosave_secs = 30    # how often block edits are flushed to disk

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
# A connection string contains a password and this file is in version control,
# so prefer the AETHER_DATABASE_URL environment variable, which overrides this.
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
    fn mirroring_is_off_unless_it_is_configured() {
        // SAFETY: single-threaded test, and the variable is read only here.
        unsafe { std::env::remove_var(DATABASE_URL_ENV) };
        assert!(DatabaseConfig::default().connection_url().is_empty());
    }

    #[test]
    fn the_checked_in_toml_names_every_setting_the_sample_does() {
        // The two files are deliberately *not* compared byte for byte. The
        // repo's aether-server.toml is a live file an operator edits — the
        // running server binds 0.0.0.0, which the sample must not suggest as a
        // default — so equality would fail forever and stop reporting the
        // thing that actually matters: a setting added to SAMPLE and forgotten
        // in the checked-in file, which a fresh checkout would then silently
        // run without.
        let checked_in = include_str!("../../../aether-server.toml");
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
                "aether-server.toml is missing `{key}`, which SAMPLE documents"
            );
        }
        // Both must still parse into the same shape.
        toml::from_str::<Config>(checked_in).expect("checked-in config must parse");
    }
}
