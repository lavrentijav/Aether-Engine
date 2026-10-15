//! `config.toml` for the sidebar plugin: the layouts, their conditions and
//! their lines.
//!
//! The file is deliberately data-only, so an operator can add a VIP layout or
//! a per-world sidebar without touching the plugin. It is loaded once at
//! `init` (and again on `/sidebar reload`).

use serde::Deserialize;

/// The plugin's configuration.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SidebarConfig {
    /// The objective title. Legacy `§` codes are accepted; the core converts
    /// them to the version's own text form.
    pub title: String,
    /// How many times per second a player's sidebar may update. The default is
    /// the flicker-free 4.
    pub update_hz: u32,
    /// Whether to mirror the same board into the tab-list header/footer.
    pub tab: bool,
    /// What `%time%` shows: the world clock (`world`, default) or the server
    /// wall clock (`server`). `%world_time%` and `%server_time%` always name
    /// one explicitly.
    pub clock: ClockMode,
    /// What to do when the vanilla `/scoreboard` already owns the sidebar slot:
    /// `override` (default) or `yield`.
    pub mode: SidebarMode,
    /// The layouts, tried highest `priority` first.
    pub layout: Vec<Layout>,
}

/// What `%time%` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClockMode {
    /// The in-game world time (`0..24000`), shown as a wall clock.
    #[default]
    World,
    /// The server's own wall clock, in UTC.
    Server,
}

/// The sidebar-vs-vanilla policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SidebarMode {
    /// Show the plugin's sidebar even if `/scoreboard objectives setdisplay`
    /// claimed the slot.
    #[default]
    Override,
    /// Leave the vanilla sidebar alone while it owns the slot.
    Yield,
}

/// One candidate sidebar: a condition and the lines it draws.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Layout {
    /// Which layout wins; the highest passing priority is shown.
    pub priority: i32,
    /// A permission node the player must have. Empty means no check.
    pub permission: String,
    /// A world (dimension) the player must be in. Empty means any.
    pub world: String,
    /// A placeholder whose resolved value must equal `equals`. Empty means no
    /// check.
    pub placeholder: String,
    /// The value `placeholder` must resolve to.
    pub equals: String,
    /// The lines, top first, up to fifteen.
    pub lines: Vec<String>,
}

impl SidebarConfig {
    /// Parse a `config.toml` document.
    pub fn parse(text: &str) -> Result<SidebarConfig, String> {
        let mut cfg: SidebarConfig =
            toml::from_str(text).map_err(|e| format!("sidebar config: {e}"))?;
        cfg.normalise();
        Ok(cfg)
    }

    /// Apply defaults and enforce the invariants the renderer relies on.
    fn normalise(&mut self) {
        if self.title.is_empty() {
            self.title = "&6&lAether".to_string();
        }
        if self.update_hz == 0 {
            self.update_hz = 4;
        }
        self.update_hz = self.update_hz.min(20);
        for l in &mut self.layout {
            l.lines.truncate(crate::MAX_LINES);
        }
    }

    /// The default configuration, used when no file is present.
    pub fn builtin() -> SidebarConfig {
        let mut cfg = SidebarConfig {
            title: "&6&lAether".to_string(),
            update_hz: 4,
            tab: false,
            clock: ClockMode::World,
            mode: SidebarMode::Override,
            layout: vec![Layout {
                priority: 0,
                lines: vec![
                    "&7Online: &f%online%&7/&f%max%".to_string(),
                    "&7Ping: &f%ping%ms".to_string(),
                    "&7World: &f%world%".to_string(),
                    "&7Coords: &f%x% %y% %z%".to_string(),
                    String::new(),
                    "&e%player%".to_string(),
                ],
                ..Layout::default()
            }],
        };
        cfg.normalise();
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_config_gets_the_defaults() {
        let cfg = SidebarConfig::parse("title = \"&cHi\"\nupdate_hz = 8\n").unwrap();
        assert_eq!(cfg.title, "&cHi");
        assert_eq!(cfg.update_hz, 8);
        assert!(cfg.layout.is_empty());
        assert_eq!(cfg.mode, SidebarMode::Override);
    }

    #[test]
    fn a_layout_carries_its_condition_and_lines() {
        let cfg = SidebarConfig::parse(
            r#"
            title = "T"
            [[layout]]
            priority = 10
            permission = "sidebar.vip"
            world = "minecraft:the_end"
            lines = ["a", "b"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.layout.len(), 1);
        assert_eq!(cfg.layout[0].priority, 10);
        assert_eq!(cfg.layout[0].permission, "sidebar.vip");
        assert_eq!(cfg.layout[0].lines, vec!["a", "b"]);
    }

    #[test]
    fn zero_update_rate_becomes_the_default() {
        let cfg = SidebarConfig::parse("update_hz = 0").unwrap();
        assert_eq!(cfg.update_hz, 4);
    }

    #[test]
    fn too_many_lines_are_clipped_to_fifteen() {
        let lines: Vec<String> = (0..40).map(|i| format!("\"{i}\"")).collect();
        let text = format!("[[layout]]\nlines = [{}]", lines.join(","));
        let cfg = SidebarConfig::parse(&text).unwrap();
        assert_eq!(cfg.layout[0].lines.len(), crate::MAX_LINES);
    }

    #[test]
    fn yield_mode_parses() {
        let cfg = SidebarConfig::parse("mode = \"yield\"").unwrap();
        assert_eq!(cfg.mode, SidebarMode::Yield);
    }
}
