//! `plugin.toml` — what a plugin declares about itself.
//!
//! The manifest is read before the component is ever instantiated: it chooses
//! the entry file, says which capabilities the plugin wants, which events it
//! subscribes to, and where it sorts against the other plugins.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The API version this host speaks. A manifest naming another one is refused
/// rather than run against an interface it was not written for.
pub const API_VERSION: &str = "0.1";

/// Which plugin tier a manifest describes.
///
/// `wasm` (the default) is a WebAssembly component loaded from `entry`;
/// `native` is in-process Rust compiled into the server, loaded from the
/// build-time registry instead. The two share the manifest, the event bus and
/// the enable/priority/`requires` rules; only the instantiation differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginTier {
    /// A `wasm32-wasip2` component (the default).
    #[default]
    Wasm,
    /// In-process Rust, trusted-by-install.
    Native,
}

/// Event names as they appear in `subscribe`. The manifest spells them the
/// same as the WIT exports so a plugin author has one vocabulary.
pub const EVENT_BEFORE_BLOCK_BREAK: &str = "before-block-break";
pub const EVENT_BEFORE_BLOCK_PLACE: &str = "before-block-place";
/// A native-only event: right-click on a block, with the cell coordinates.
/// The frozen WIT world has no such export, so only native plugins receive it
/// (the regions plugin's internal wand is the first user).
pub const EVENT_BEFORE_BLOCK_INTERACT: &str = "before-block-interact";
pub const EVENT_BEFORE_USE_ITEM: &str = "before-use-item";
pub const EVENT_BEFORE_ATTACK: &str = "before-attack";
pub const EVENT_BEFORE_CHAT: &str = "before-chat";
pub const EVENT_BEFORE_CONTAINER_CLICK: &str = "before-container-click";
pub const EVENT_AFTER_PLAYER_JOIN: &str = "after-player-join";
pub const EVENT_AFTER_PLAYER_LEAVE: &str = "after-player-leave";
pub const EVENT_AFTER_BLOCK_BREAK: &str = "after-block-break";
pub const EVENT_AFTER_ENTITY_DEATH: &str = "after-entity-death";
pub const EVENT_AFTER_DAMAGE: &str = "after-damage";
pub const EVENT_AFTER_CRAFT: &str = "after-craft";
pub const EVENT_AFTER_ENCHANT: &str = "after-enchant";
pub const EVENT_ON_TICK: &str = "on-tick";
pub const EVENT_ON_COMMAND: &str = "on-command";
/// A native-only event: a plugin-channel (custom payload) message from a
/// player. The frozen WIT world has no such callback, so only native plugins
/// can subscribe.
pub const EVENT_ON_PLUGIN_MESSAGE: &str = "on-plugin-message";

/// A parsed, validated `plugin.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Stable plugin identifier: the KV namespace, the log tag and the key
    /// under `[plugins.<id>]`.
    pub id: String,
    /// The plugin's own version, for the operator's benefit.
    #[serde(default)]
    pub version: String,
    /// The plugin API version this component was built against.
    #[serde(default = "default_api")]
    pub api: String,
    /// The `.wasm` component, relative to the manifest's directory. Ignored for
    /// a `native` plugin, which is compiled in.
    #[serde(default)]
    pub entry: String,
    /// Which tier this manifest describes. Defaults to `wasm`.
    #[serde(default)]
    pub tier: PluginTier,
    /// Services this plugin requires at load: `name@version`, e.g.
    /// `sdk.permissions@0.1`. A missing service refuses the load with a clear
    /// error rather than letting the plugin half-run.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Human-readable name for logs; the id is used when absent.
    #[serde(default)]
    pub name: String,
    /// Ordering among plugins for the same event: higher runs first. Ties are
    /// broken by id, so the order is total and reproducible.
    #[serde(default)]
    pub priority: i32,
    /// Capabilities the plugin asks the operator to grant.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Events the plugin wants to receive. Anything absent here is never
    /// called, so it costs nothing.
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Commands the plugin answers in `on-command`. A plugin may only add
    /// **new** names: the core owns the vanilla names (and its own), so a
    /// declaration that collides is refused when the server registers it.
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    /// How often `on-tick` fires, in ticks. Only meaningful when subscribed;
    /// `0` is treated as every tick.
    #[serde(default = "default_tick_period")]
    pub tick_period: u64,
    /// Whether a fault in a `before-*` handler counts as a **veto** instead
    /// of as `allow`. Off by default: a broken plugin must not silently change
    /// gameplay. A plugin whose job is to protect something — a region guard,
    /// an anti-grief rule — sets this so that a trap, an overrun or a spent
    /// tick budget fails closed rather than letting the action through.
    #[serde(default)]
    pub deny_on_fault: bool,
}

/// A command a plugin declares in `plugin.toml`.
///
/// Two spellings, both valid, so the simple case stays simple:
///
/// ```toml
/// commands = ["hello"]                       # anyone may run /hello
/// commands = [{ name = "admin", op = 3 }]    # /admin needs op level 3
/// ```
///
/// The permission check happens in the server's command registry before the
/// plugin's `on-command` is entered, not inside the plugin.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CommandDecl {
    /// `/name`, op level 0.
    Plain(String),
    /// `/name` with a permission node and required op level.
    Detailed {
        /// The command name, without the leading slash.
        name: String,
        /// Permission node; empty resolves to `plugin.<id>.<name>`.
        #[serde(default)]
        node: String,
        /// Vanilla op level (0 everyone … 4 owner).
        #[serde(default)]
        op: u8,
    },
}

impl CommandDecl {
    /// The command name, without the leading slash.
    pub fn name(&self) -> &str {
        match self {
            CommandDecl::Plain(n) => n,
            CommandDecl::Detailed { name, .. } => name,
        }
    }

    /// The required op level (clamped to the vanilla 0..=4 range).
    pub fn op(&self) -> u8 {
        match self {
            CommandDecl::Plain(_) => 0,
            CommandDecl::Detailed { op, .. } => (*op).min(4),
        }
    }

    /// The permission node, resolving the default from `plugin_id`.
    pub fn node(&self, plugin_id: &str) -> String {
        match self {
            CommandDecl::Plain(_) => format!("plugin.{plugin_id}.{}", self.name()),
            CommandDecl::Detailed { node, .. } if !node.is_empty() => node.clone(),
            CommandDecl::Detailed { .. } => format!("plugin.{plugin_id}.{}", self.name()),
        }
    }
}

fn default_api() -> String {
    API_VERSION.to_string()
}

fn default_tick_period() -> u64 {
    1
}

/// A manifest together with where it was read from.
#[derive(Debug, Clone)]
pub struct ManifestFile {
    /// The plugin root: the directory holding `plugin.toml`.
    pub dir: PathBuf,
    /// The parsed manifest.
    pub manifest: Manifest,
}

impl Manifest {
    /// Whether the manifest subscribes to `event`.
    pub fn subscribes(&self, event: &str) -> bool {
        self.subscribe.iter().any(|e| e == event)
    }

    /// Whether `name` is one of the plugin's registered commands.
    pub fn serves_command(&self, name: &str) -> bool {
        self.commands.iter().any(|c| c.name() == name)
    }

    /// The declared command `name`, if any.
    pub fn command(&self, name: &str) -> Option<&CommandDecl> {
        self.commands.iter().find(|c| c.name() == name)
    }

    /// The services this plugin requires, parsed from `requires`. An invalid
    /// entry is reported so a manifest typo fails the load loudly.
    pub fn required_services(&self) -> Result<Vec<crate::service::ServiceRef>, String> {
        self.requires
            .iter()
            .map(|s| crate::service::ServiceRef::parse(s))
            .collect()
    }

    /// The display name used in logs.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// The absolute path of the component.
    pub fn entry_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.entry)
    }
}

/// Read and validate a `plugin.toml`.
pub fn load(path: &Path) -> Result<ManifestFile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    if manifest.id.is_empty() {
        return Err(format!("{}: `id` must not be empty", path.display()));
    }
    // A native plugin is compiled in and has no component file; only a WASM
    // manifest must name its `entry`.
    if manifest.tier == PluginTier::Wasm && manifest.entry.is_empty() {
        return Err(format!("{}: `entry` must not be empty", path.display()));
    }
    if !manifest
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "{}: `id` may contain only letters, digits, `_` and `-`",
            path.display()
        ));
    }
    if manifest.api != API_VERSION {
        return Err(format!(
            "{}: plugin API `{}` is not supported by this host (it speaks `{API_VERSION}`)",
            path.display(),
            manifest.api
        ));
    }
    for c in &manifest.commands {
        let name = c.name();
        if name.is_empty() {
            return Err(format!("{}: a command name is empty", path.display()));
        }
        if name.contains('/') || name.contains(char::is_whitespace) {
            return Err(format!(
                "{}: command `{name}` must be a single word without a slash",
                path.display()
            ));
        }
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok(ManifestFile { dir, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
id = "hello"
version = "1.2.3"
api = "0.1"
entry = "hello.wasm"
priority = 10
capabilities = ["log", "storage.kv"]
subscribe = ["on-tick", "before-block-break"]
commands = ["hello", { name = "admin", node = "hello.admin", op = 3 }]
tick_period = 20
"#;

    #[test]
    fn a_manifest_parses_with_the_documented_defaults() {
        let m: Manifest = toml::from_str(GOOD).unwrap();
        assert_eq!(m.id, "hello");
        assert_eq!(m.priority, 10);
        assert!(m.subscribes("on-tick"));
        assert!(!m.subscribes("after-damage"), "unlisted events are off");
        assert!(m.serves_command("hello"));
        assert_eq!(m.label(), "hello", "an absent name falls back to the id");
        // A plain name is op 0 and derives its node; a detailed one carries
        // its own node and op.
        let hello = m.command("hello").unwrap();
        assert_eq!(hello.op(), 0);
        assert_eq!(hello.node("hello"), "plugin.hello.hello");
        let admin = m.command("admin").unwrap();
        assert_eq!(admin.op(), 3);
        assert_eq!(admin.node("hello"), "hello.admin");
        assert!(!m.serves_command("admin2"));

        let minimal: Manifest = toml::from_str("id = \"x\"\nentry = \"x.wasm\"").unwrap();
        assert_eq!(minimal.api, API_VERSION);
        assert_eq!(minimal.tick_period, 1);
        assert!(minimal.subscribe.is_empty());
        assert!(!minimal.deny_on_fault, "faults allow by default");
    }

    #[test]
    fn deny_on_fault_is_opt_in() {
        let m: Manifest = toml::from_str("id=\"g\"\nentry=\"g.wasm\"\ndeny_on_fault=true").unwrap();
        assert!(m.deny_on_fault);
    }

    #[test]
    fn the_tier_defaults_to_wasm_and_native_needs_no_entry() {
        let m: Manifest = toml::from_str(GOOD).unwrap();
        assert_eq!(m.tier, PluginTier::Wasm, "wasm is the default tier");

        // A native manifest names no component file; `entry` may be empty.
        let native: Manifest =
            toml::from_str("id=\"sdk\"\ntier=\"native\"\nrequires=[\"sdk.permissions@0.1\"]")
                .unwrap();
        assert_eq!(native.tier, PluginTier::Native);
        assert!(native.entry.is_empty());
        let refs = native.required_services().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "sdk.permissions");
        assert_eq!(refs[0].version, "0.1");

        // A malformed requirement fails loudly.
        let bad: Manifest =
            toml::from_str("id=\"x\"\nentry=\"x.wasm\"\nrequires=[\"nope\"]").unwrap();
        assert!(bad.required_services().is_err());
    }

    #[test]
    fn a_native_manifest_loads_without_an_entry_file() {
        let dir = std::env::temp_dir();
        let file = dir.join("aether-plugin-native-manifest.toml");
        std::fs::write(&file, "id=\"sdk\"\ntier=\"native\"").unwrap();
        let loaded = load(&file).expect("a native manifest needs no entry");
        assert_eq!(loaded.manifest.tier, PluginTier::Native);
        std::fs::remove_file(&file).ok();

        // A wasm manifest still must name its entry.
        let wasm = dir.join("aether-plugin-wasm-manifest.toml");
        std::fs::write(&wasm, "id=\"hello\"").unwrap();
        assert!(
            load(&wasm).is_err(),
            "a wasm manifest without entry is refused"
        );
        std::fs::remove_file(&wasm).ok();
    }

    #[test]
    fn the_wrong_api_version_is_refused() {
        let dir = std::env::temp_dir();
        let bad = dir.join("aether-plugin-bad-api.toml");
        std::fs::write(&bad, "id=\"x\"\nentry=\"x.wasm\"\napi=\"9.9\"").unwrap();
        let err = load(&bad).unwrap_err();
        assert!(err.contains("9.9"), "{err}");
        std::fs::remove_file(&bad).ok();
    }
}
