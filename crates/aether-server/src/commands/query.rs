//! Parsing and evaluating the filter language shared by `/inspect`,
//! `/lookup` and `/rollback`.
//!
//! One vocabulary for every command that asks a question of the history,
//! because an operator who has learned `player:steve time:30m` for one of them
//! has learned it for all of them:
//!
//! ```text
//! player:<name>     only this player's actions
//! time:<duration>   only this recent — 90s, 30m, 2h, 3d
//! block:<name>      only events touching this block, either side of the change
//! action:place      only placements; action:break only removals
//! radius:<n>        only within n blocks of the caller, horizontally
//! ```
//!
//! Unknown keys are an error rather than a silent no-op: a mistyped filter
//! that quietly widened a rollback from one player to everybody would be the
//! worst possible failure mode for this command set.

use aether_world::journal::{Event, EventBody, Filter};
use aether_world::BlockStateId;

/// What a command was asked to look at.
pub struct Query {
    /// The journal-level filter, which the index can use to narrow the scan
    /// before anything is decoded.
    pub filter: Filter,
    /// Restrict to events where this block is either the old or the new state.
    pub block: Option<String>,
    /// Restrict to placements or removals.
    pub action: Option<Action>,
}

/// The two directions a block change can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Something now stands where it did not.
    Place,
    /// Something became air.
    Break,
}

/// Why a filter string was rejected.
#[derive(Debug, PartialEq)]
pub enum ParseError {
    /// A token had no `key:value` shape.
    NotAPair(String),
    /// The key is not one of the documented filters.
    UnknownKey(String),
    /// The value did not fit the key.
    BadValue { key: String, value: String },
    /// `player:` named somebody who is not online.
    NoSuchPlayer(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotAPair(t) => {
                write!(f, "`{t}` is not a filter — filters look like key:value")
            }
            ParseError::UnknownKey(k) => write!(
                f,
                "unknown filter `{k}` — try player, time, block, action, radius"
            ),
            ParseError::BadValue { key, value } => write!(f, "`{value}` is not a valid {key}"),
            ParseError::NoSuchPlayer(n) => write!(f, "no player named {n} is online"),
        }
    }
}

/// Parse a duration like `90s`, `30m`, `2h`, `3d` into milliseconds.
///
/// A bare number means minutes, because that is what an operator types when in
/// a hurry and minutes is the unit they mean.
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let (digits, mult) = match s.as_bytes().last()? {
        b's' => (&s[..s.len() - 1], 1_000u64),
        b'm' => (&s[..s.len() - 1], 60_000),
        b'h' => (&s[..s.len() - 1], 3_600_000),
        b'd' => (&s[..s.len() - 1], 86_400_000),
        _ => (s, 60_000),
    };
    digits.parse::<u64>().ok()?.checked_mul(mult)
}

/// Everything a command needs to resolve `player:` and `radius:`.
pub struct Context<'a> {
    /// Resolve an online player's name to their UUID.
    pub lookup_player: &'a dyn Fn(&str) -> Option<u128>,
    /// Where the caller is standing.
    pub here: (i32, i32, i32),
    /// Now, in Unix milliseconds.
    pub now_ms: u64,
}

impl Query {
    /// Parse `tokens` into a query.
    ///
    /// With no tokens at all the query selects the entire history — which is
    /// right for `/lookup` and dangerous for `/rollback`, so that command
    /// checks [`Query::is_unrestricted`] before acting.
    pub fn parse(tokens: &[&str], ctx: &Context<'_>) -> Result<Query, ParseError> {
        let mut q = Query {
            filter: Filter::everything(),
            block: None,
            action: None,
        };
        for t in tokens {
            let (key, value) = t
                .split_once(':')
                .ok_or_else(|| ParseError::NotAPair((*t).to_owned()))?;
            let bad = || ParseError::BadValue {
                key: key.to_owned(),
                value: value.to_owned(),
            };
            match key {
                "player" | "p" | "user" => {
                    let uuid = (ctx.lookup_player)(value)
                        .ok_or_else(|| ParseError::NoSuchPlayer(value.to_owned()))?;
                    q.filter.actor = Some(aether_world::journal::ActorId(uuid));
                }
                "time" | "t" => {
                    let ms = parse_duration_ms(value).ok_or_else(bad)?;
                    q.filter.since_ms = ctx.now_ms.saturating_sub(ms);
                }
                "radius" | "r" => {
                    let r: u32 = value.parse().map_err(|_| bad())?;
                    q.filter.area = Some((ctx.here.0, ctx.here.1, ctx.here.2, r));
                }
                "block" | "b" => {
                    // Accept both `stone` and `minecraft:stone`: players type
                    // the short form and the registry stores the long one.
                    q.block = Some(if value.contains(':') {
                        value.to_owned()
                    } else {
                        format!("minecraft:{value}")
                    });
                }
                "action" | "a" => {
                    q.action = Some(match value {
                        "place" | "placed" | "+" => Action::Place,
                        "break" | "broke" | "remove" | "-" => Action::Break,
                        _ => return Err(bad()),
                    })
                }
                _ => return Err(ParseError::UnknownKey(key.to_owned())),
            }
        }
        Ok(q)
    }

    /// Whether this query has no restriction at all.
    ///
    /// `/rollback` refuses one: "undo the entire history of the world" is a
    /// real operation, but it should never be one typo away.
    pub fn is_unrestricted(&self) -> bool {
        self.filter.actor.is_none()
            && self.filter.area.is_none()
            && self.filter.since_ms == 0
            && self.filter.since_seq == 0
            && self.block.is_none()
            && self.action.is_none()
    }

    /// Whether `e` passes the whole query.
    ///
    /// `name_of` resolves a block id to its registry name; it is passed in
    /// rather than captured so this stays testable without a world.
    pub fn accepts(&self, e: &Event, name_of: &dyn Fn(BlockStateId) -> String) -> bool {
        if !self.filter_matches(e) {
            return false;
        }
        let EventBody::BlockSet { from, to, .. } = &e.body else {
            // Item events carry no block, so a block/action filter excludes
            // them rather than silently letting them through.
            return self.block.is_none() && self.action.is_none();
        };
        if let Some(want) = &self.block {
            if &name_of(*from) != want && &name_of(*to) != want {
                return false;
            }
        }
        if let Some(a) = self.action {
            let broke = *to == BlockStateId::AIR;
            match a {
                Action::Place if broke => return false,
                Action::Break if !broke => return false,
                _ => {}
            }
        }
        true
    }

    /// The journal's own three conditions.
    ///
    /// Spelled out here because [`Filter::matches`] is private to the journal;
    /// they are trivial, and duplicating them is cheaper than widening that
    /// crate's public surface for one caller.
    fn filter_matches(&self, e: &Event) -> bool {
        if let Some(a) = self.filter.actor {
            if e.actor != a {
                return false;
            }
        }
        if e.seq < self.filter.since_seq || e.at_ms < self.filter.since_ms {
            return false;
        }
        if let Some((cx, _, cz, r)) = self.filter.area {
            let Some((x, _, z)) = e.position() else {
                return false;
            };
            let (dx, dz) = ((x - cx) as i64, (z - cz) as i64);
            if dx * dx + dz * dz > (r as i64) * (r as i64) {
                return false;
            }
        }
        true
    }
}
