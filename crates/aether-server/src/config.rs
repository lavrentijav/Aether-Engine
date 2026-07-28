//! Server configuration (TOML).

use serde::Deserialize;

/// `[server]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Bind address.
    pub host: String,
    /// Listen port (vanilla default 25565).
    pub port: u16,
    /// Message-of-the-day shown in the server list.
    pub motd: String,
    /// Reported max player slots.
    pub max_players: u32,
    /// Chunk radius sent around spawn (a `(2r+1)²` grid).
    pub view_radius: i32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 25565,
            motd: "Aether Engine — 1.8.9 demo (full-bright flat world)".to_string(),
            max_players: 20,
            view_radius: 5,
        }
    }
}

/// Whole server config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server settings.
    pub server: ServerConfig,
}

/// Sample written when the config file is missing.
pub const SAMPLE: &str = r#"# Aether Engine server configuration (Minecraft 1.8.9 / protocol 47).

[server]
host = "0.0.0.0"
port = 25565
motd = "Aether Engine — 1.8.9 demo (full-bright flat world)"
max_players = 20
view_radius = 5      # chunk radius sent around spawn
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
                let _ = std::fs::write(path, SAMPLE);
                Ok((Config::default(), true))
            }
            Err(e) => Err(format!("reading {path}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_parses() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.server.port, 25565);
        assert_eq!(cfg.server.view_radius, 5);
    }
}
