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
    /// Concurrent TCP connections the accept loop will serve at once. This
    /// bounds idle sockets — one thread is spawned per connection — separately
    /// from the game's player cap. `0` means "auto": see
    /// [`Self::connection_limit`].
    pub max_connections: u32,
    /// Concurrent connections allowed from a single address. `0` disables the
    /// per-address limit; the total cap still applies.
    pub max_connections_per_ip: u32,
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
            max_connections: 0,
            max_connections_per_ip: 8,
            view_radius: 8,
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

impl ServerConfig {
    /// The compression threshold as the wire layer wants it: `None` when
    /// compression is switched off.
    pub fn compression(&self) -> Option<usize> {
        usize::try_from(self.compression_threshold).ok()
    }

    /// The total concurrent-connection cap the accept loop should use.
    ///
    /// An explicit `max_connections` wins; `0` ("auto") resolves to double the
    /// player slots plus a margin, so that a full server can still answer
    /// status pings and take a reconnect before the old socket is noticed dead.
    pub fn connection_limit(&self) -> u32 {
        if self.max_connections == 0 {
            self.max_players.saturating_mul(2).saturating_add(16)
        } else {
            self.max_connections
        }
    }

    /// The per-address connection cap the accept loop should use. `0` in the
    /// config means no per-address limit, so it passes through unchanged.
    pub fn per_ip_connection_limit(&self) -> u32 {
        self.max_connections_per_ip
    }

    /// The chunk radius the client and the streamer actually use.
    ///
    /// Clamped to `1..=12`. The load accepts any integer, and a `0` would make
    /// the streamer send a 3×3 area while the visibility search seeded only the
    /// player's own section — every other section of those columns would be
    /// masked as stone, invisible walls the client can plainly see.
    pub fn clamped_view_radius(&self) -> i32 {
        self.view_radius.clamp(1, 12)
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

/// `[compat.pack]` settings: the per-version resource packs that re-skin only
/// the block stand-ins. See `docs/design/compat-resource-pack.md`.
///
/// The server still only *points* at a pack; a generated pack is produced at
/// build/deploy time by `tools/pack/build.js` and served by the operator (or by
/// whatever hosts `url`). A client only ever sees a carrier block state when
/// its pack status is `successfully_loaded`, so this being off — or a client
/// declining it — leaves the plain compat stand-in in place.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CompatPackConfig {
    /// Offer the generated per-version compat pack. Off by default.
    pub enabled: bool,
    /// Download URL the client fetches the pack from. Empty disables the
    /// offer even when `enabled` is set.
    pub url: String,
    /// SHA-1 of the pack file, lowercase hex. Clients cache on this.
    pub hash: String,
    /// Whether the client must accept the pack to keep playing. `false` by
    /// default: a declined pack falls back to the plain stand-in rather than
    /// disconnecting.
    pub required: bool,
    /// The `pack_format` the configured pack declares. Enforced against the
    /// generated carrier table's own format: a mismatch means the pack is for a
    /// different release, so the compat pack is not offered to that client
    /// (the plain `[resource_pack]`, if any, applies instead). `0` never
    /// matches a real table, so leaving it unset keeps the pack off.
    pub pack_format: u32,
    /// Directory the generated packs live in (used by `tools/pack/build.js`,
    /// not read by the server).
    pub dir: String,
    /// Vanilla client jar the pack textures are read from at build time. Never
    /// shipped or read by the server at run time.
    pub client_jar: String,
}

impl CompatPackConfig {
    /// Whether a compat pack should be offered at all.
    pub fn active(&self) -> bool {
        self.enabled && !self.url.is_empty()
    }

    /// Whether the configured pack may be offered to a release whose generated
    /// carrier table declares `format`. The contract the resource pack rests
    /// on: the pack's `pack_format` must be the release's.
    pub fn offers_format(&self, format: u32) -> bool {
        self.pack_format != 0 && self.pack_format == format
    }

    /// The resource-pack offer to send, in the shape every codec already
    /// knows how to encode.
    pub fn offer(&self) -> ResourcePackConfig {
        ResourcePackConfig {
            url: self.url.clone(),
            hash: self.hash.clone(),
            required: self.required,
        }
    }
}

/// `[compat]` settings: the version-compatibility layer's optional extras.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CompatConfig {
    /// The per-version stand-in resource packs.
    pub pack: CompatPackConfig,
}

/// `[plugins]` settings: where WebAssembly plugins live and their limits.
///
/// The table is both the global settings and the per-plugin overrides:
/// `[plugins.hello]` is a [`PluginEntry`] for the plugin whose manifest id is
/// `hello`. Missing per-plugin entries default to enabled with the manifest's
/// own capability requests.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
// The fields below are only read by the `plugins` feature; without it the
// config still parses (so one file serves every build) but nothing consults
// the per-plugin map, which the dead-code lint would otherwise flag.
#[allow(dead_code)]
pub struct PluginsConfig {
    /// Directory to scan for `plugin.toml`. Empty disables plugins entirely.
    pub dir: String,
    /// Per-call time budget in milliseconds (epoch interruption).
    pub budget_ms: u64,
    /// Shared time budget in milliseconds, spent by all plugins across one
    /// tick. Once it is gone no further plugin runs until the next tick.
    pub tick_budget_ms: u64,
    /// Per-plugin memory cap in mebibytes.
    pub memory_mib: u64,
    /// Traps a plugin may take before the host disables it.
    pub max_faults: u32,
    /// Fuel granted to a plugin's instantiation and `init`. Bounds how much
    /// first-time work a plugin may do at load, independently of the per-call
    /// time budget.
    pub init_fuel: u64,
    /// Per-plugin overrides, keyed by manifest id.
    #[serde(flatten)]
    pub plugins: std::collections::BTreeMap<String, PluginEntry>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        PluginsConfig {
            dir: String::new(),
            budget_ms: 5,
            tick_budget_ms: 25,
            memory_mib: 64,
            max_faults: 5,
            init_fuel: 100_000_000,
            plugins: std::collections::BTreeMap::new(),
        }
    }
}

/// `[anti_xray]` settings: the optional anti-xray module.
///
/// Off by default. Ore/x-ray cheats must see nothing useful, so blocks a
/// player has no way to see are not sent as themselves. Two modes:
/// - `engine` (cheap, the default): an ore-like block that is fully enclosed
///   by opaque blocks is sent as the surrounding stone/deepslate/netherrack
///   and revealed again the moment a neighbour changes (break, explosion,
///   fluid) so it is on the client before the player can mine to it.
/// - `culling` (the design in `docs/design/visibility-culling.md`): a whole
///   section the player cannot see, even after turning or moving a little, is
///   sent as solid stone instead of its real blocks.
///
/// The permission node [`crate::ANTIXRAY_BYPASS_PERMISSION`] (currently held
/// by operators) lets a player see everything; so does
/// [`Self::bypass_players`] and `/visibility off`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AntiXrayConfig {
    /// Whether the module runs at all. Off until proven.
    pub enabled: bool,
    /// Which masking strategy to use.
    pub mode: AntiXrayMode,
    /// Block names never sent as themselves while fully enclosed. Names are
    /// accepted with or without the `minecraft:` namespace. Empty means the
    /// built-in ore list.
    pub hidden_blocks: Vec<String>,
    /// The world the module applies to (empty = all).
    ///
    /// The server runs one world per process, so this is the per-world toggle
    /// for a deployment that shares a config across several world
    /// directories; it matches the last component of `[server] world_dir`.
    pub worlds: Vec<String>,
    /// `culling` mode only: how far, in blocks, a section may be from the
    /// player and still be sent for real without being reached by sight —
    /// the "could see it if he turned his head or took a step" sphere.
    pub margin_blocks: i32,
    /// `culling` mode only: what a hidden section is encoded as.
    pub hidden_fill: HiddenFill,
    /// `culling` mode only: how many columns may be re-sent (revealed) per
    /// tick.
    pub reveal_budget_per_tick: u32,
    /// Player names always allowed to bypass, in addition to the operators and
    /// the permission node.
    pub bypass_players: Vec<String>,
    /// Deprecated (pre-AX1, used to live under `[visibility]`): accepted and
    /// ignored, so an old file does not silently lose the key.
    pub ray_refinement: Option<bool>,
    /// Deprecated (pre-AX1): accepted and ignored. Engine mode is the
    /// replacement for the old "buried ore masking" idea.
    pub mask_buried_ores: Option<bool>,
}

impl Default for AntiXrayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: AntiXrayMode::Engine,
            hidden_blocks: Vec::new(),
            worlds: Vec::new(),
            margin_blocks: 5,
            hidden_fill: HiddenFill::Stone,
            reveal_budget_per_tick: 32,
            bypass_players: Vec::new(),
            ray_refinement: None,
            mask_buried_ores: None,
        }
    }
}

/// The pre-AX1 `[visibility]` table, accepted for backward compatibility.
///
/// Before AX1 this was the whole module and always meant section culling, so a
/// file that still uses it is migrated to `[anti_xray] mode = "culling"` by
/// [`Config::migrate_legacy`], with a warning, rather than silently becoming
/// `engine` (or, worse, nothing).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LegacyVisibilityConfig {
    /// Whether culling ran at all.
    pub enabled: bool,
    /// The margin, in blocks.
    pub margin_blocks: i32,
    /// What a hidden section was encoded as.
    pub hidden_fill: HiddenFill,
    /// Reserved then; ignored now.
    pub ray_refinement: bool,
    /// Reserved then; ignored now.
    pub mask_buried_ores: bool,
    /// Columns re-sent per tick.
    pub reveal_budget_per_tick: u32,
}

impl Default for LegacyVisibilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            margin_blocks: 5,
            hidden_fill: HiddenFill::Stone,
            ray_refinement: false,
            mask_buried_ores: false,
            reveal_budget_per_tick: 32,
        }
    }
}

impl From<LegacyVisibilityConfig> for AntiXrayConfig {
    fn from(l: LegacyVisibilityConfig) -> Self {
        AntiXrayConfig {
            enabled: l.enabled,
            mode: AntiXrayMode::Culling,
            hidden_blocks: Vec::new(),
            worlds: Vec::new(),
            margin_blocks: l.margin_blocks,
            hidden_fill: l.hidden_fill,
            reveal_budget_per_tick: l.reveal_budget_per_tick,
            bypass_players: Vec::new(),
            ray_refinement: Some(l.ray_refinement),
            mask_buried_ores: Some(l.mask_buried_ores),
        }
    }
}

impl AntiXrayConfig {
    /// The effective hidden-block list: the configured names, or the built-in
    /// ore list when none are given.
    pub fn hidden_block_names(&self) -> Vec<String> {
        if self.hidden_blocks.is_empty() {
            default_hidden_blocks()
        } else {
            self.hidden_blocks.clone()
        }
    }

    /// Whether the module applies to a world directory. `worlds` empty means
    /// every world; otherwise the last path component must match, ignoring
    /// case and a trailing slash.
    pub fn applies_to_world(&self, world_dir: &str) -> bool {
        if self.worlds.is_empty() {
            return true;
        }
        let name = world_dir
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(world_dir);
        self.worlds.iter().any(|w| w.eq_ignore_ascii_case(name))
    }
}

/// Which anti-xray strategy the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AntiXrayMode {
    /// Hide enclosed ore-like blocks in otherwise real columns. Cheap: only
    /// ore-candidate blocks do any work.
    #[default]
    Engine,
    /// Hide whole sections the player cannot reach by sight.
    Culling,
}

/// The built-in hidden-block list: every ore, ancient debris and monster
/// spawner. Chests of worldgen structures are deliberately **not** here — the
/// generator keeps no block entity, so hiding them would hide natural loot
/// chests without hiding the container texture a player already knows.
pub fn default_hidden_blocks() -> Vec<String> {
    [
        "minecraft:coal_ore",
        "minecraft:deepslate_coal_ore",
        "minecraft:iron_ore",
        "minecraft:deepslate_iron_ore",
        "minecraft:copper_ore",
        "minecraft:deepslate_copper_ore",
        "minecraft:gold_ore",
        "minecraft:deepslate_gold_ore",
        "minecraft:redstone_ore",
        "minecraft:deepslate_redstone_ore",
        "minecraft:lapis_ore",
        "minecraft:deepslate_lapis_ore",
        "minecraft:diamond_ore",
        "minecraft:deepslate_diamond_ore",
        "minecraft:emerald_ore",
        "minecraft:deepslate_emerald_ore",
        "minecraft:nether_gold_ore",
        "minecraft:nether_quartz_ore",
        "minecraft:ancient_debris",
        "minecraft:spawner",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `[auth]` settings: who may claim a name.
///
/// The server has always run offline: a name is just a claim. In `online`
/// mode every client must complete Mojang's encryption handshake. In `hybrid`
/// offline play stays open to everyone; only a name that has completed online
/// auth **on this server** is locked and must authenticate. See
/// [`crate::auth`].
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Which authentication policy to run under.
    pub mode: AuthMode,
    /// Base URL of the session server (`hasJoined`).
    pub session_server: String,
    /// Base URL of the profile API (name → uuid lookup).
    pub profile_api: String,
    /// How long a Mojang HTTP call may take before it counts as a failure.
    pub lookup_timeout_ms: u64,
    /// What `hybrid` does when the profile API cannot be reached.
    pub on_lookup_failure: LookupFailure,
    /// Login attempts allowed per address per minute. `0` disables the limit.
    ///
    /// Each attempt can cost a Mojang lookup and a connection thread, so this
    /// bounds how fast a scanner can drive outbound calls or park threads.
    pub login_rate_per_min: u32,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::Offline,
            session_server: "https://sessionserver.mojang.com".to_string(),
            profile_api: "https://api.mojang.com".to_string(),
            lookup_timeout_ms: 3000,
            on_lookup_failure: LookupFailure::Offline,
            login_rate_per_min: 10,
        }
    }
}

/// The authentication policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Anyone may join under any name, as before. No Mojang traffic.
    #[default]
    Offline,
    /// Every client must complete the encryption handshake and `hasJoined`.
    Online,
    /// Locked names require online auth; everyone else may join offline.
    Hybrid,
}

impl std::str::FromStr for AuthMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "offline" => Ok(AuthMode::Offline),
            "online" => Ok(AuthMode::Online),
            "hybrid" => Ok(AuthMode::Hybrid),
            other => Err(format!(
                "`{other}` is not an auth mode — use offline, online or hybrid"
            )),
        }
    }
}

/// What `hybrid` does when the profile API is unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LookupFailure {
    /// Admit the player offline, as if the name were not premium. Favours
    /// availability; someone whose name *is* locked may still be refused by
    /// the lock itself.
    #[default]
    Offline,
    /// Refuse the login. Favours not letting a premium name be squatted while
    /// the API is down.
    Deny,
}

/// `[plugins.<id>]`: what the operator decides about one plugin.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
// See `PluginsConfig`: parsed in every build, consulted only with `plugins`.
#[allow(dead_code)]
pub struct PluginEntry {
    /// `false` skips the plugin. `None` (the field absent) means enabled.
    pub enabled: Option<bool>,
    /// Capabilities the operator grants. Empty means "grant what the manifest
    /// asked for".
    pub capabilities: Vec<String>,
    /// Values the plugin reads through `host.config` in `init`.
    pub config: std::collections::BTreeMap<String, String>,
}

impl PluginEntry {
    /// Whether the plugin should load; enabled unless explicitly disabled.
    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// What a hidden section is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HiddenFill {
    /// Solid stone, the safe default: collision stays, no void to fall into.
    #[default]
    Stone,
    /// Air. Only safe for a client that is flying or otherwise cannot fall
    /// through the floor; the design reserves it for spectators.
    Air,
}

/// Whole server config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server settings.
    pub server: ServerConfig,
    /// Optional resource pack offered on join.
    pub resource_pack: ResourcePackConfig,
    /// The compat layer's optional extras: the per-version stand-in packs.
    pub compat: CompatConfig,
    /// Optional anti-xray (buried-ore masking or section visibility culling).
    pub anti_xray: AntiXrayConfig,
    /// The pre-AX1 `[visibility]` table, if the file still has one. Migrated
    /// into `anti_xray` by [`Self::migrate_legacy`]; never read directly.
    #[serde(default, rename = "visibility")]
    legacy_visibility: Option<LegacyVisibilityConfig>,
    /// Authentication policy.
    pub auth: AuthConfig,
    /// Optional history mirror.
    pub database: DatabaseConfig,
    /// Chat and command logging. See [`ChatConfig`].
    pub chat: ChatConfig,
    /// WebAssembly plugins: the directory and the per-plugin overrides.
    pub plugins: PluginsConfig,
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
    /// Apply pre-AX1 backward compatibility and return human-readable warnings.
    ///
    /// AX1 renamed `[visibility]` to `[anti_xray]`. The old table always meant
    /// section culling, so a file that still has one is migrated to
    /// `[anti_xray] mode = "culling"` with its old knobs instead of quietly
    /// falling back to the new `engine` default or (with `enabled = true`)
    /// doing nothing. If the file sets both, `[anti_xray]` wins. The old
    /// `ray_refinement`/`mask_buried_ores` keys are accepted and reported as
    /// ignored.
    pub fn migrate_legacy(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(legacy) = self.legacy_visibility.take() {
            let replaced = self.anti_xray == AntiXrayConfig::default();
            if replaced {
                self.anti_xray = legacy.into();
                warnings.push(
                    "`[visibility]` is deprecated: migrated to `[anti_xray] mode = \"culling\"`"
                        .to_string(),
                );
            } else {
                warnings.push(
                    "`[visibility]` is deprecated and both it and `[anti_xray]` are set: \
                     kept `[anti_xray]`, ignored `[visibility]`"
                        .to_string(),
                );
            }
        }
        if self.anti_xray.ray_refinement.is_some() {
            warnings.push(
                "`ray_refinement` is deprecated and ignored (the section graph is the robust \
                 part; see docs/design/visibility-culling.md)"
                    .to_string(),
            );
        }
        if self.anti_xray.mask_buried_ores.is_some() {
            warnings.push(
                "`mask_buried_ores` is deprecated: use `[anti_xray] mode = \"engine\"` (the \
                 default) instead"
                    .to_string(),
            );
        }
        warnings
    }

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

/// `[chat]` settings: logging every chat line, private message and command a
/// player types.
///
/// On by default. The server log gets a line per event (`[chat] name text`,
/// `[pm] a -> b: text`, `[cmd] name: /line`) and the same text is journalled as
/// an event so the history API and web admin can show it. Commands that carry
/// a secret are redacted: their arguments become `****` in both places.
///
/// Player text is sanitized of control characters and `§` so it cannot forge a
/// log line or inject an ANSI escape, capped at [`max_len`](Self::max_len)
/// characters, and rate-limited to
/// [`rate_per_min`](Self::rate_per_min) lines per player per minute.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    /// Log public chat and private messages.
    pub log: bool,
    /// Log the command lines players type.
    pub log_commands: bool,
    /// Command names whose arguments are masked, without the leading `/`.
    /// Matched case-insensitively. The default covers the common
    /// authentication commands (`/login`, `/register`, `/l`, `/reg`).
    pub redact: Vec<String>,
    /// The most characters of player text kept in the log and the journal; a
    /// longer line is truncated and marked with `…`. `0` means no limit.
    pub max_len: usize,
    /// The most lines one player may have logged/journalled per minute; `0`
    /// disables the limit. Bounds how fast a chatty or hostile client can
    /// grow the journal, which is appended synchronously on the game thread.
    pub rate_per_min: u32,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            log: true,
            log_commands: true,
            redact: ["login", "register", "l", "reg"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            // Vanilla caps a chat line at 256 characters anyway.
            max_len: 256,
            rate_per_min: 120,
        }
    }
}

impl ChatConfig {
    /// Whether the arguments of command `name` (without the leading `/`) are
    /// redacted. Case-insensitive, and a leading `/` on `name` is ignored.
    pub fn redacts(&self, name: &str) -> bool {
        let name = name.strip_prefix('/').unwrap_or(name);
        self.redact.iter().any(|r| r.eq_ignore_ascii_case(name))
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
max_connections = 0   # concurrent connections to serve; 0 = max_players*2+16
max_connections_per_ip = 8  # per-address cap; 0 disables it
view_radius = 8       # chunk radius kept loaded around each player
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

[compat.pack]
# The per-version compat packs that re-skin only the block stand-ins. Generated
# by tools/pack/build.js; see docs/design/compat-resource-pack.md. Only a client
# whose pack status is successfully_loaded sees carrier states.
enabled = false
url = ""
hash = ""
required = false
pack_format = 0
dir = "packs/compat"
client_jar = ""

[auth]
# Who may claim a name. "offline" (default): anyone may join under any name.
# "online": every client must complete Mojang's encryption handshake. "hybrid":
# a name that belongs (or has ever belonged) to a premium account is locked to
# it and must pass online auth; everyone else joins offline. Locked names are
# remembered in the world store.
mode = "offline"
session_server = "https://sessionserver.mojang.com"
profile_api = "https://api.mojang.com"
lookup_timeout_ms = 3000     # how long a Mojang HTTP call may take
on_lookup_failure = "offline" # hybrid, API unreachable: "offline" or "deny"
login_rate_per_min = 10      # login attempts per address per minute; 0 disables

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

[chat]
# Log every chat line, private message and command players type. Each is
# written to the server log ([chat] name text, [pm] a -> b: text,
# [cmd] name: /line) and journalled as an event the history API can show.
log = true           # public chat and private messages
log_commands = true  # commands players type
# Command names whose arguments are masked as **** in the log and the journal,
# so a password can never leak through it. Case-insensitive, without the slash.
redact = ["login", "register", "l", "reg"]
# Player text is sanitized of control characters and § before it is logged or
# journalled, so it cannot forge a log line or inject a terminal escape, and is
# cut to this many characters (0 = no limit; a cut line ends with …).
max_len = 256
# The most lines one player may have logged/journalled per minute; 0 disables
# the limit. Bounds how fast a client can grow the journal on the game thread.
rate_per_min = 120

[plugins]
# WebAssembly plugins (a build with the `plugins` feature, the default). Each
# plugin is a directory under `dir` holding plugin.toml and its .wasm
# component. Empty disables plugins. See docs/design/plugins.md.
dir = ""             # e.g. "plugins"
budget_ms = 5        # per-call time budget before a running plugin is interrupted
tick_budget_ms = 25  # shared budget for all plugins across one tick
memory_mib = 64      # per-plugin linear-memory cap
max_faults = 5       # traps before the host disables a plugin
init_fuel = 100000000  # fuel for a plugin's instantiation and init

# Per-plugin overrides are tables named for the plugin's manifest id, e.g.
# [plugins.hello]
# enabled = true
# capabilities = ["log", "config", "supports"]  # DEFAULT DENY: empty grants
#                                                # nothing, even if asked for
# config = { greeting = "hi" }

[cache]
# Unmodified generated columns are cached on disk so the generator does not
# recompute the same terrain on every restart. Pure speed: every entry is
# reproducible from the seed, so deleting this directory loses nothing.
dir = "world-cache"     # empty disables the cache
ttl_hours = 24          # entries older than this are regenerated
max_entries = 20000     # above this the oldest are swept away, hourly

[anti_xray]
# Anti-xray. Off by default until it is proven.
# "engine" (cheap): an ore-like block fully enclosed by opaque blocks is sent
#   as the surrounding stone/deepslate/netherrack, and revealed the moment a
#   neighbour changes (break, explosion, fluid). "culling" (the design in
#   docs/design/visibility-culling.md): a whole section the player cannot see
#   is sent as solid stone. See docs/design/visibility-culling.md.
enabled = false
mode = "engine"          # "engine" (buried ores) or "culling" (sections)
# Blocks never sent as themselves while enclosed. Empty = the built-in list of
# every ore, ancient debris and monster spawner. Names take an optional
# minecraft: namespace.
hidden_blocks = []
worlds = []              # world-dir names this applies to; empty = all
bypass_players = []      # names always allowed to see everything

# culling-mode only:
margin_blocks = 5        # "could see it if he turned or stepped" sphere, in blocks
hidden_fill = "stone"    # "stone" (safe) or "air" (spectators/flying only)
reveal_budget_per_tick = 32  # columns re-sent (revealed) per tick, nearest first
"#;

impl Config {
    /// Load config from `path`, writing a sample and using defaults if absent.
    pub fn load_or_init(path: &str) -> Result<(Config, bool), String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg: Config =
                    toml::from_str(&text).map_err(|e| format!("parsing {path}: {e}"))?;
                for w in cfg.migrate_legacy() {
                    crate::log::warn(&w);
                }
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
    fn the_view_radius_is_clamped_to_the_range_the_streamer_uses() {
        // A raw 0 would make the streamer send a 3x3 area while the visibility
        // search seeded only the player's own section (finding 6).
        let radius = |v: i32| ServerConfig {
            view_radius: v,
            ..ServerConfig::default()
        };
        assert_eq!(radius(0).clamped_view_radius(), 1);
        assert_eq!(radius(-5).clamped_view_radius(), 1);
        assert_eq!(radius(100).clamped_view_radius(), 12);
        assert_eq!(radius(8).clamped_view_radius(), 8);
    }

    #[test]
    fn auth_defaults_to_offline_and_parses_every_mode() {
        let cfg = Config::default();
        assert_eq!(cfg.auth.mode, AuthMode::Offline, "offline is the default");
        assert_eq!(cfg.auth.session_server, "https://sessionserver.mojang.com");
        assert_eq!(cfg.auth.profile_api, "https://api.mojang.com");
        assert_eq!(cfg.auth.lookup_timeout_ms, 3000);
        assert_eq!(cfg.auth.on_lookup_failure, LookupFailure::Offline);

        for (text, want) in [
            ("offline", AuthMode::Offline),
            ("online", AuthMode::Online),
            ("hybrid", AuthMode::Hybrid),
        ] {
            let cfg: Config = toml::from_str(&format!("[auth]\nmode = \"{text}\"\n")).unwrap();
            assert_eq!(cfg.auth.mode, want);
        }
        "offline".parse::<AuthMode>().unwrap();
        assert!("sideways".parse::<AuthMode>().is_err());
    }

    #[test]
    fn the_auth_section_parses_a_hybrid_deny_setup() {
        let cfg: Config = toml::from_str(
            "[auth]\nmode = \"hybrid\"\nsession_server = \"http://localhost:9\"\n\
             profile_api = \"http://localhost:8\"\nlookup_timeout_ms = 250\n\
             on_lookup_failure = \"deny\"\n",
        )
        .unwrap();
        assert_eq!(cfg.auth.mode, AuthMode::Hybrid);
        assert_eq!(cfg.auth.lookup_timeout_ms, 250);
        assert_eq!(cfg.auth.on_lookup_failure, LookupFailure::Deny);
    }

    #[test]
    fn anti_xray_is_off_by_default_and_parses_its_settings() {
        let cfg = Config::default();
        assert!(!cfg.anti_xray.enabled, "the module is opt-in");
        assert_eq!(cfg.anti_xray.mode, AntiXrayMode::Engine);
        assert_eq!(cfg.anti_xray.hidden_fill, HiddenFill::Stone);
        assert_eq!(cfg.anti_xray.margin_blocks, 5);
        assert_eq!(cfg.anti_xray.reveal_budget_per_tick, 32);
        assert!(cfg.anti_xray.hidden_blocks.is_empty());
        assert!(cfg.anti_xray.applies_to_world("world"));

        let cfg: Config = toml::from_str(
            "[anti_xray]\nenabled = true\nmode = \"culling\"\nhidden_fill = \"air\"\n\
             margin_blocks = 2\nworlds = [\"alpha\"]\nbypass_players = [\"root\"]\n",
        )
        .unwrap();
        assert!(cfg.anti_xray.enabled);
        assert_eq!(cfg.anti_xray.mode, AntiXrayMode::Culling);
        assert_eq!(cfg.anti_xray.hidden_fill, HiddenFill::Air);
        assert_eq!(cfg.anti_xray.margin_blocks, 2);
        assert!(cfg.anti_xray.applies_to_world("saves/alpha"));
        assert!(!cfg.anti_xray.applies_to_world("beta"));
        assert_eq!(cfg.anti_xray.bypass_players, vec!["root".to_string()]);
    }

    #[test]
    fn the_hidden_block_list_defaults_to_every_ore_and_debris() {
        let names = AntiXrayConfig::default().hidden_block_names();
        assert!(names.iter().any(|n| n == "minecraft:diamond_ore"));
        assert!(names.iter().any(|n| n == "minecraft:deepslate_diamond_ore"));
        assert!(names.iter().any(|n| n == "minecraft:ancient_debris"));
        assert!(names.iter().any(|n| n == "minecraft:spawner"));
        assert!(
            !names.iter().any(|n| n.contains("chest")),
            "worldgen chests stay visible"
        );

        // An explicit list replaces the built-in one.
        let cfg: Config =
            toml::from_str("[anti_xray]\nhidden_blocks = [\"diamond_ore\"]\n").unwrap();
        assert_eq!(cfg.anti_xray.hidden_block_names(), vec!["diamond_ore"]);
    }

    #[test]
    fn a_pre_ax1_visibility_config_is_migrated_to_culling() {
        // The old `[visibility]` table always meant section culling. A server
        // upgrading must keep hiding, not silently fall back to `engine`.
        let mut cfg: Config = toml::from_str(
            "[visibility]\nenabled = true\nmargin_blocks = 3\nhidden_fill = \"air\"\n\
             mask_buried_ores = false\nray_refinement = false\nreveal_budget_per_tick = 8\n",
        )
        .unwrap();
        let warnings = cfg.migrate_legacy();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("migrated to `[anti_xray]")),
            "a migration warning is expected: {warnings:?}"
        );
        assert!(cfg.anti_xray.enabled, "the old enabled flag is kept");
        assert_eq!(
            cfg.anti_xray.mode,
            AntiXrayMode::Culling,
            "old configs were culling, not the new engine default"
        );
        assert_eq!(cfg.anti_xray.margin_blocks, 3);
        assert_eq!(cfg.anti_xray.hidden_fill, HiddenFill::Air);
        assert_eq!(cfg.anti_xray.reveal_budget_per_tick, 8);
    }

    #[test]
    fn anti_xray_wins_when_both_sections_are_present() {
        let mut cfg: Config = toml::from_str(
            "[anti_xray]\nenabled = true\nmode = \"engine\"\n\n\
             [visibility]\nenabled = false\n",
        )
        .unwrap();
        let warnings = cfg.migrate_legacy();
        assert!(
            warnings.iter().any(|w| w.contains("kept `[anti_xray]`")),
            "{warnings:?}"
        );
        assert_eq!(cfg.anti_xray.mode, AntiXrayMode::Engine);
        assert!(cfg.anti_xray.enabled);
    }

    #[test]
    fn the_deprecated_reserved_keys_are_accepted_and_reported() {
        let mut cfg: Config = toml::from_str(
            "[anti_xray]\nenabled = true\nray_refinement = true\nmask_buried_ores = true\n",
        )
        .unwrap();
        let warnings = cfg.migrate_legacy();
        assert!(
            warnings.iter().any(|w| w.contains("ray_refinement")),
            "ray_refinement must be reported: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("mask_buried_ores")),
            "mask_buried_ores must be reported: {warnings:?}"
        );
        // Still parsed, engine mode still on.
        assert!(cfg.anti_xray.enabled);
        assert_eq!(cfg.anti_xray.mode, AntiXrayMode::Engine);
    }

    #[test]
    fn the_compat_pack_is_off_by_default_and_parses_its_settings() {
        let cfg = Config::default();
        assert!(!cfg.compat.pack.active(), "the compat packs are opt-in");
        assert!(
            !cfg.compat.pack.required,
            "a declined pack falls back to the stand-in"
        );

        let cfg: Config = toml::from_str(
            "[compat.pack]\nenabled = true\nurl = \"https://example/pack.zip\"\n\
             hash = \"abc123\"\npack_format = 15\n",
        )
        .unwrap();
        assert!(cfg.compat.pack.active());
        let offer = cfg.compat.pack.offer();
        assert_eq!(offer.url, "https://example/pack.zip");
        assert_eq!(offer.hash, "abc123");
        assert!(!offer.required, "required stays false unless asked for");
        // The format contract: the pack is offered only for a matching format,
        // and an unset format (0) matches nothing.
        assert!(cfg.compat.pack.offers_format(15));
        assert!(!cfg.compat.pack.offers_format(0));
        assert!(!cfg.compat.pack.offers_format(46));
        assert!(!CompatPackConfig::default().offers_format(0));

        // `enabled` alone, with no URL, offers nothing.
        let cfg: Config = toml::from_str("[compat.pack]\nenabled = true\n").unwrap();
        assert!(!cfg.compat.pack.active());
    }

    #[test]
    fn the_connection_cap_defaults_to_double_the_slots_plus_a_margin() {
        let mut server = ServerConfig::default();
        assert_eq!(server.max_connections, 0, "the default is 'auto'");
        assert_eq!(server.connection_limit(), 20 * 2 + 16);
        server.max_players = 3;
        assert_eq!(server.connection_limit(), 3 * 2 + 16, "follows max_players");
        server.max_connections = 100;
        assert_eq!(server.connection_limit(), 100, "an explicit cap wins");
    }

    #[test]
    fn the_connection_caps_are_read_from_the_file() {
        let cfg: Config =
            toml::from_str("[server]\nmax_connections = 64\nmax_connections_per_ip = 2\n").unwrap();
        assert_eq!(cfg.server.max_connections, 64);
        assert_eq!(cfg.server.connection_limit(), 64);
        assert_eq!(cfg.server.per_ip_connection_limit(), 2);
    }

    #[test]
    fn the_per_ip_cap_defaults_to_eight_and_zero_disables_it() {
        assert_eq!(ServerConfig::default().per_ip_connection_limit(), 8);
        let cfg: Config = toml::from_str("[server]\nmax_connections_per_ip = 0\n").unwrap();
        assert_eq!(cfg.server.per_ip_connection_limit(), 0);
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
    fn chat_logging_is_on_by_default_and_redacts_the_auth_commands() {
        let cfg = Config::default();
        assert!(cfg.chat.log, "chat logging is on by default");
        assert!(cfg.chat.log_commands);
        assert_eq!(cfg.chat.max_len, 256, "chat lines are capped by default");
        assert_eq!(
            cfg.chat.rate_per_min, 120,
            "chat is rate-limited by default"
        );
        for name in ["login", "register", "l", "reg", "/login", "LOGIN"] {
            assert!(cfg.chat.redacts(name), "{name} must be redacted");
        }
        for name in ["gamemode", "msg", "lookup", "r"] {
            assert!(!cfg.chat.redacts(name), "{name} must not be redacted");
        }
    }

    #[test]
    fn the_chat_section_parses_and_can_replace_the_redact_list() {
        let cfg: Config = toml::from_str(
            "[chat]\nlog = false\nlog_commands = false\nredact = [\"password\", \"pw\"]\n",
        )
        .unwrap();
        assert!(!cfg.chat.log);
        assert!(!cfg.chat.log_commands);
        assert_eq!(cfg.chat.redact, vec!["password", "pw"]);
        assert!(cfg.chat.redacts("PW"), "matching is case-insensitive");
        assert!(!cfg.chat.redacts("login"), "the default list was replaced");
    }

    #[test]
    fn the_chat_length_and_rate_limits_are_configurable() {
        let cfg: Config = toml::from_str("[chat]\nmax_len = 40\nrate_per_min = 7\n").unwrap();
        assert_eq!(cfg.chat.max_len, 40);
        assert_eq!(cfg.chat.rate_per_min, 7);
        // Zero is the documented "off" value for both.
        let off: Config = toml::from_str("[chat]\nmax_len = 0\nrate_per_min = 0\n").unwrap();
        assert_eq!(off.chat.max_len, 0);
        assert_eq!(off.chat.rate_per_min, 0);
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
}
