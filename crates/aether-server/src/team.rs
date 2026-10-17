//! Scoreboard teams: the model, persistence and the packets that show them.
//!
//! A team is vanilla's `PlayerTeam`: a name, a display name, a colour, member
//! names, friendly-fire/see-invisible switches, nametag and death-message
//! visibility, a collision rule and a prefix/suffix applied to every member's
//! name. The command bodies live in [`crate::commands::team_tag`]; everything
//! here is the one model they and the chat/combat paths share.
//!
//! Teams are stored under [`aether_api::keys::TEAM`], one JSON blob per team
//! keyed by the team name, so `/team` changes survive a restart. There is no
//! process-wide list: every operation reads the current state from the world
//! it is given. That keeps two worlds (and two tests) from sharing teams, and
//! makes the store the single source of truth.
//!
//! Member names are scoreboard names, exactly as vanilla's `PlayerTeam`
//! stores them: a player's profile name, or any literal name passed to
//! `/team join`. Membership therefore survives a logout with no player-save
//! migration.

use std::collections::BTreeSet;

use aether_world::KvBackend;
use serde::{Deserialize, Serialize};

use crate::players::SharedRegistry;
use crate::protocol::scoreboard::{ScoreboardOp, TeamChange, TeamInfo};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

/// The leading bytes of every persisted team key (`TM` + name).
const PREFIX: &[u8] = aether_api::keys::TEAM;

/// One of the sixteen chat colours, or "reset".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
    Reset,
}

impl Color {
    /// Parse a `ChatFormatting` name the way `ColorArgument` does: lower-case
    /// it, drop every non-letter, and match the enum name.
    pub fn parse(s: &str) -> Option<Color> {
        let cleaned: String = s
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Some(match cleaned.as_str() {
            "black" => Color::Black,
            "darkblue" => Color::DarkBlue,
            "darkgreen" => Color::DarkGreen,
            "darkaqua" => Color::DarkAqua,
            "darkred" => Color::DarkRed,
            "darkpurple" => Color::DarkPurple,
            "gold" => Color::Gold,
            "gray" => Color::Gray,
            "darkgray" => Color::DarkGray,
            "blue" => Color::Blue,
            "green" => Color::Green,
            "aqua" => Color::Aqua,
            "red" => Color::Red,
            "lightpurple" => Color::LightPurple,
            "yellow" => Color::Yellow,
            "white" => Color::White,
            "reset" => Color::Reset,
            _ => return None,
        })
    }

    /// The `ChatFormatting` serialized name, used in command feedback.
    pub fn name(self) -> &'static str {
        match self {
            Color::Black => "black",
            Color::DarkBlue => "dark_blue",
            Color::DarkGreen => "dark_green",
            Color::DarkAqua => "dark_aqua",
            Color::DarkRed => "dark_red",
            Color::DarkPurple => "dark_purple",
            Color::Gold => "gold",
            Color::Gray => "gray",
            Color::DarkGray => "dark_gray",
            Color::Blue => "blue",
            Color::Green => "green",
            Color::Aqua => "aqua",
            Color::Red => "red",
            Color::LightPurple => "light_purple",
            Color::Yellow => "yellow",
            Color::White => "white",
            Color::Reset => "reset",
        }
    }

    /// The legacy `§` colour code, or `None` for reset.
    pub fn legacy(self) -> Option<char> {
        Some(match self {
            Color::Black => '0',
            Color::DarkBlue => '1',
            Color::DarkGreen => '2',
            Color::DarkAqua => '3',
            Color::DarkRed => '4',
            Color::DarkPurple => '5',
            Color::Gold => '6',
            Color::Gray => '7',
            Color::DarkGray => '8',
            Color::Blue => '9',
            Color::Green => 'a',
            Color::Aqua => 'b',
            Color::Red => 'c',
            Color::LightPurple => 'd',
            Color::Yellow => 'e',
            Color::White => 'f',
            Color::Reset => return None,
        })
    }

    /// The `ChatFormatting` enum id the team packet carries (reset is 21).
    pub fn id(self) -> i32 {
        match self {
            Color::Black => 0,
            Color::DarkBlue => 1,
            Color::DarkGreen => 2,
            Color::DarkAqua => 3,
            Color::DarkRed => 4,
            Color::DarkPurple => 5,
            Color::Gold => 6,
            Color::Gray => 7,
            Color::DarkGray => 8,
            Color::Blue => 9,
            Color::Green => 10,
            Color::Aqua => 11,
            Color::Red => 12,
            Color::LightPurple => 13,
            Color::Yellow => 14,
            Color::White => 15,
            Color::Reset => 21,
        }
    }
}

/// A team's nametag/death-message visibility rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Always,
    Never,
    HideForOtherTeams,
    HideForOwnTeam,
}

impl Visibility {
    pub fn parse(s: &str) -> Option<Visibility> {
        Some(match s {
            "always" => Visibility::Always,
            "never" => Visibility::Never,
            "hideForOtherTeams" => Visibility::HideForOtherTeams,
            "hideForOwnTeam" => Visibility::HideForOwnTeam,
            _ => return None,
        })
    }

    pub fn id(self) -> i32 {
        match self {
            Visibility::Always => 0,
            Visibility::Never => 1,
            Visibility::HideForOtherTeams => 2,
            Visibility::HideForOwnTeam => 3,
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Visibility::Always => "Always",
            Visibility::Never => "Never",
            Visibility::HideForOtherTeams => "Hide for other teams",
            Visibility::HideForOwnTeam => "Hide for own team",
        }
    }
}

/// A team's collision rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollisionRule {
    Always,
    Never,
    PushOtherTeams,
    PushOwnTeam,
}

impl CollisionRule {
    pub fn parse(s: &str) -> Option<CollisionRule> {
        Some(match s {
            "always" => CollisionRule::Always,
            "never" => CollisionRule::Never,
            "pushOtherTeams" => CollisionRule::PushOtherTeams,
            "pushOwnTeam" => CollisionRule::PushOwnTeam,
            _ => return None,
        })
    }

    pub fn id(self) -> i32 {
        match self {
            CollisionRule::Always => 0,
            CollisionRule::Never => 1,
            CollisionRule::PushOtherTeams => 2,
            CollisionRule::PushOwnTeam => 3,
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            CollisionRule::Always => "Always",
            CollisionRule::Never => "Never",
            CollisionRule::PushOtherTeams => "Push other teams",
            CollisionRule::PushOwnTeam => "Push own team",
        }
    }
}

/// One scoreboard team.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    /// Display name, already rendered as plain text with legacy `§` styling.
    pub display_name: String,
    pub color: Color,
    pub friendly_fire: bool,
    pub see_friendly_invisibles: bool,
    pub nametag_visibility: Visibility,
    pub death_message_visibility: Visibility,
    pub collision_rule: CollisionRule,
    /// Member-name prefix/suffix, plain text with legacy styling.
    pub prefix: String,
    pub suffix: String,
    pub members: BTreeSet<String>,
}

impl Team {
    /// A fresh team, with vanilla's defaults.
    pub fn new(name: impl Into<String>) -> Team {
        let name = name.into();
        Team {
            display_name: name.clone(),
            name,
            color: Color::Reset,
            friendly_fire: true,
            see_friendly_invisibles: true,
            nametag_visibility: Visibility::Always,
            death_message_visibility: Visibility::Always,
            collision_rule: CollisionRule::Always,
            prefix: String::new(),
            suffix: String::new(),
            members: BTreeSet::new(),
        }
    }

    /// `[displayName]` with the team colour, vanilla's `getFormattedDisplayName`.
    pub fn formatted_display_name(&self) -> String {
        format!("{}{}", self.color_prefix(), self.bracketed())
    }

    /// The bracketed display name without the colour (for already-coloured
    /// contexts and tests).
    pub fn bracketed(&self) -> String {
        format!("[{}]", self.display_name)
    }

    /// A member's name as the team renders it: prefix, name, suffix, colour.
    pub fn format_player_name(&self, member: &str) -> String {
        let colour = self.color_prefix();
        if colour.is_empty() {
            format!("{}{}{}", self.prefix, member, self.suffix)
        } else {
            format!("{}{}{}{}§r", colour, self.prefix, member, self.suffix)
        }
    }

    fn color_prefix(&self) -> String {
        self.color
            .legacy()
            .map(|c| format!("§{c}"))
            .unwrap_or_default()
    }

    /// The wire form of this team, with no membership change.
    pub fn info(&self) -> TeamInfo {
        TeamInfo {
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            color: self.color.id(),
            friendly_fire: self.friendly_fire,
            see_friendly_invisibles: self.see_friendly_invisibles,
            nametag_visibility: self.nametag_visibility.id(),
            death_message_visibility: self.death_message_visibility.id(),
            collision_rule: self.collision_rule.id(),
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
        }
    }
}

fn key(name: &str) -> Vec<u8> {
    // An eight-byte length keeps every key at least ten bytes long: a
    // two-byte prefix plus a name could otherwise total nine bytes, the exact
    // shape of a sub-chunk key (see `aether_api::keys`).
    let mut k = Vec::with_capacity(PREFIX.len() + 8 + name.len());
    k.extend_from_slice(PREFIX);
    k.extend_from_slice(&(name.len() as u64).to_be_bytes());
    k.extend_from_slice(name.as_bytes());
    k
}

/// Every team in `world`, sorted by name.
pub fn all(world: &DemoWorld) -> Vec<Team> {
    let Ok(rows) = world.journal().backend().scan_prefix(PREFIX) else {
        return Vec::new();
    };
    let mut teams: Vec<Team> = rows
        .into_iter()
        .filter_map(|(_, value)| serde_json::from_slice(&value).ok())
        .collect();
    teams.sort_by(|a, b| a.name.cmp(&b.name));
    teams
}

/// The team called `name`, case-sensitively as vanilla's scoreboard.
pub fn find(world: &DemoWorld, name: &str) -> Option<Team> {
    let blob = world.get_meta(&key(name)).ok().flatten()?;
    serde_json::from_slice(&blob).ok()
}

/// Persist `team`, replacing any team of the same name.
pub fn save(world: &DemoWorld, team: &Team) -> bool {
    match serde_json::to_vec(team) {
        Ok(blob) => world.put_meta(&key(&team.name), &blob).is_ok(),
        Err(_) => false,
    }
}

/// Delete the team called `name`.
pub fn remove(world: &DemoWorld, name: &str) -> bool {
    world.delete_meta(&key(name)).is_ok()
}

/// The team a scoreboard name belongs to, if any.
pub fn team_of(world: &DemoWorld, member: &str) -> Option<Team> {
    all(world).into_iter().find(|t| t.members.contains(member))
}

/// The team name a member belongs to, if any.
pub fn team_name_of(world: &DemoWorld, member: &str) -> Option<String> {
    team_of(world, member).map(|t| t.name)
}

/// Whether two players may damage each other under their teams' friendly-fire
/// rules. `false` only when both are on the *same* team with friendly fire off.
pub fn allows_damage(world: &DemoWorld, attacker: &str, target: &str) -> bool {
    let Some(team) = team_of(world, attacker) else {
        return true;
    };
    if !team.members.contains(target) {
        return true;
    }
    team.friendly_fire
}

/// Send every team to one joining client (add packets with their members).
pub fn send_all(handle: &crate::players::PlayerHandle, world: &DemoWorld) {
    for team in all(world) {
        let op = ScoreboardOp::Team(TeamChange::Add {
            info: team.info(),
            members: team.members.iter().cloned().collect(),
        });
        handle.emit(&ServerEvent::Scoreboard(op), world);
    }
}

/// Send one team change to every connected client.
pub fn broadcast(registry: &SharedRegistry, world: &DemoWorld, change: TeamChange) {
    registry.broadcast(&ServerEvent::Scoreboard(ScoreboardOp::Team(change)), world);
}

/// The add packet for `team`, including its members.
pub fn add_change(team: &Team) -> TeamChange {
    TeamChange::Add {
        info: team.info(),
        members: team.members.iter().cloned().collect(),
    }
}

/// The update packet for `team` (options and prefix/suffix, no membership).
pub fn update_change(team: &Team) -> TeamChange {
    TeamChange::Update { info: team.info() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_the_way_color_argument_does() {
        assert_eq!(Color::parse("dark_blue"), Some(Color::DarkBlue));
        assert_eq!(Color::parse("DARKBLUE"), Some(Color::DarkBlue));
        assert_eq!(Color::parse("dark-blue"), Some(Color::DarkBlue));
        assert_eq!(Color::parse("reset"), Some(Color::Reset));
        assert_eq!(Color::parse("bold"), None);
        assert_eq!(Color::Red.id(), 12);
        assert_eq!(Color::Reset.id(), 21);
    }

    #[test]
    fn a_default_team_formats_like_vanilla() {
        let mut team = Team::new("red");
        assert_eq!(team.formatted_display_name(), "[red]");
        assert_eq!(team.format_player_name("Alice"), "Alice");
        team.color = Color::Red;
        team.prefix = "<".into();
        team.suffix = ">".into();
        assert_eq!(team.formatted_display_name(), "§c[red]");
        assert_eq!(team.format_player_name("Alice"), "§c<Alice>§r");
    }

    #[test]
    fn visibility_and_collision_names_are_the_vanilla_ones() {
        assert_eq!(Visibility::HideForOtherTeams.id(), 2);
        assert_eq!(
            Visibility::HideForOtherTeams.display(),
            "Hide for other teams"
        );
        assert_eq!(CollisionRule::PushOwnTeam.id(), 3);
        assert_eq!(CollisionRule::PushOwnTeam.display(), "Push own team");
    }
}
