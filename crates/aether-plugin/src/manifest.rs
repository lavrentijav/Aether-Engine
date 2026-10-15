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

/// Event names as they appear in `subscribe`. The manifest spells them the
/// same as the WIT exports so a plugin author has one vocabulary.
pub const EVENT_BEFORE_BLOCK_BREAK: &str = "before-block-break";
pub const EVENT_BEFORE_BLOCK_PLACE: &str = "before-block-place";
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
    /// The `.wasm` component, relative to the manifest's directory.
    pub entry: String,
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
    /// Command names the plugin answers in `on-command`.
    #[serde(default)]
    pub commands: Vec<String>,
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
        self.commands.iter().any(|c| c == name)
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
    if manifest.entry.is_empty() {
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
commands = ["hello"]
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
    fn the_wrong_api_version_is_refused() {
        let dir = std::env::temp_dir();
        let bad = dir.join("aether-plugin-bad-api.toml");
        std::fs::write(&bad, "id=\"x\"\nentry=\"x.wasm\"\napi=\"9.9\"").unwrap();
        let err = load(&bad).unwrap_err();
        assert!(err.contains("9.9"), "{err}");
        std::fs::remove_file(&bad).ok();
    }
}
