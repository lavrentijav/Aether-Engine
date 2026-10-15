//! Placeholder resolution: the `%name%` tokens a line may contain.
//!
//! Built-in tokens (`%player%`, `%online%`, …) are answered from the host's
//! view of the player. Everything else — `%economy_balance%`,
//! `%rank_prefix%` — is registered by another plugin through the `sidebar`
//! service, which is how the economy, permissions and stats plugins put their
//! own values on the board without the sidebar plugin knowing about them.
//!
//! Resolution is plain and allocation-light: one pass over the line, a token
//! at a time. Values are resolved afresh on each update; the sidebar's own
//! update rate (four times a second by default) is the cache, so a line is not
//! re-resolved between redraws.

use std::collections::HashMap;

use aether_plugin::sidebar::{PlaceholderFn, PlaceholderQuery};

/// The registered providers, by token name (without the `%`).
#[derive(Default)]
pub struct Placeholders {
    providers: HashMap<String, PlaceholderFn>,
}

impl Placeholders {
    /// Register (or replace) `name`. Returns whether an earlier provider was
    /// replaced.
    pub fn register(&mut self, name: String, provider: PlaceholderFn) -> bool {
        self.providers.insert(name, provider).is_some()
    }

    /// The registered token names, for diagnostics.
    pub fn names(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Resolve every `%token%` in `line`. An unknown token is left literal, so
    /// a typo shows up on screen instead of silently vanishing.
    pub fn resolve(&self, line: &str, q: &PlaceholderQuery) -> String {
        if !line.contains('%') {
            return line.to_string();
        }
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find('%') {
                Some(end) => {
                    let token = &after[..end];
                    if let Some(value) = self.value(token, q) {
                        out.push_str(&value);
                    } else {
                        // Keep the literal, `%` included.
                        out.push('%');
                        out.push_str(token);
                        out.push('%');
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    // An unmatched `%` is literal.
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// One token's value: a registered provider first, then a built-in.
    fn value(&self, token: &str, q: &PlaceholderQuery) -> Option<String> {
        if let Some(provider) = self.providers.get(token) {
            if let Some(v) = provider(q) {
                return Some(v);
            }
        }
        builtin(token, q)
    }
}

/// The built-in placeholders, answered from the host's per-player view.
fn builtin(token: &str, q: &PlaceholderQuery) -> Option<String> {
    let extra = q.extra.as_ref();
    Some(match token {
        "player" | "name" => q.name.clone(),
        "displayname" | "display_name" => extra
            .map(|e| e.display_name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| q.name.clone()),
        "online" => q.online.to_string(),
        "max" | "max_players" => q.max_players.to_string(),
        "tps" => format!("{:.1}", q.tps),
        "ping" => extra.map(|e| e.ping.to_string())?,
        "world" => extra.map(|e| e.world.clone())?,
        "x" => extra.map(|e| e.x.floor() as i64)?.to_string(),
        "y" => extra.map(|e| e.y.floor() as i64)?.to_string(),
        "z" => extra.map(|e| e.z.floor() as i64)?.to_string(),
        "health" => extra.map(|e| format!("{:.1}", e.health))?,
        "level" => extra.map(|e| e.level.to_string())?,
        "time" => clock(q.time),
        _ => return None,
    })
}

/// Translate `&`-style colour codes (`&a`, `&l`) into the `§` codes the
/// client's text renderer expects. A lone `&` is left alone.
pub fn colorize(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.peek() {
                Some(next) if is_color_code(*next) => {
                    out.push('\u{00a7}');
                    out.push(*next);
                    chars.next();
                }
                _ => out.push('&'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `c` opens a legacy formatting code.
fn is_color_code(c: char) -> bool {
    matches!(
        c.to_ascii_lowercase(),
        '0'..='9' | 'a'..='f' | 'k' | 'l' | 'm' | 'n' | 'o' | 'r'
    )
}

/// Format a world time in ticks as `HH:MM`, the way a wall clock would show it
/// (Minecraft's day starts at 06:00).
fn clock(time: i64) -> String {
    let time = time.rem_euclid(24_000);
    let hours = (time / 1000 + 6) % 24;
    let minutes = (time % 1000) * 60 / 1000;
    format!("{hours:02}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> PlaceholderQuery {
        PlaceholderQuery {
            player: 7,
            name: "Steve".to_string(),
            extra: Some(aether_plugin::sidebar::PlayerExtra {
                ping: 42,
                world: "minecraft:overworld".to_string(),
                x: 10.4,
                y: 64.0,
                z: -3.9,
                health: 19.5,
                level: 5,
                display_name: "Steve".to_string(),
            }),
            online: 3,
            max_players: 20,
            tps: 19.9,
            time: 6000,
        }
    }

    #[test]
    fn builtins_resolve_from_the_player_view() {
        let p = Placeholders::default();
        let q = query();
        assert_eq!(p.resolve("%player%", &q), "Steve");
        assert_eq!(p.resolve("%online%/%max%", &q), "3/20");
        assert_eq!(p.resolve("%ping%", &q), "42");
        assert_eq!(p.resolve("%x% %y% %z%", &q), "10 64 -4");
        assert_eq!(p.resolve("%tps%", &q), "19.9");
    }

    #[test]
    fn an_unknown_token_is_left_literal() {
        let p = Placeholders::default();
        assert_eq!(p.resolve("hi %nope%", &query()), "hi %nope%");
        // An unmatched `%` is literal too.
        assert_eq!(p.resolve("100%", &query()), "100%");
    }

    #[test]
    fn a_registered_placeholder_overrides_a_builtin() {
        let mut p = Placeholders::default();
        p.register(
            "player".to_string(),
            std::sync::Arc::new(|q: &PlaceholderQuery| Some(format!("[{}]", q.name))),
        );
        assert_eq!(p.resolve("%player%", &query()), "[Steve]");
    }

    #[test]
    fn a_registered_provider_that_declines_falls_back_to_the_builtin() {
        let mut p = Placeholders::default();
        p.register("online".to_string(), std::sync::Arc::new(|_| None));
        assert_eq!(p.resolve("%online%", &query()), "3");
    }

    #[test]
    fn ampersand_codes_become_section_codes() {
        assert_eq!(colorize("&6Hello &lWorld"), "§6Hello §lWorld");
        assert_eq!(colorize("a&Qb"), "a&Qb", "a lone & is literal");
        assert_eq!(colorize("100&"), "100&");
    }

    #[test]
    fn time_is_a_wall_clock() {
        // 0 ticks is 06:00, 6000 is noon.
        assert_eq!(clock(0), "06:00");
        assert_eq!(clock(6000), "12:00");
        assert_eq!(clock(18000), "00:00");
    }
}
