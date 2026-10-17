//! The pure stats model: identities, journal classification, cached
//! aggregates, leaderboards and the playtime ledger.
//!
//! Everything here is plain Rust with no host, no WIT and no clock of its own
//! — the caller passes the current world tick and the KV bytes in and out.
//! That is what makes the awkward cases (an offline target, a rename, a
//! crash-restart, paging past the end, a tie) cheap to test. `engine.rs`
//! wires it to the SDK services; `component.rs` wires it to the host.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How many world ticks make a second; vanilla's 20 tps.
pub const TICKS_PER_SECOND: u64 = 20;

/// How many rows one leaderboard page shows.
pub const PAGE_SIZE: usize = 10;

/// A player identity: the 128-bit UUID, kept split into two halves because
/// that is how the component model and the history API carry it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct PlayerId {
    /// Low 64 bits.
    pub low: u64,
    /// High 64 bits.
    pub high: u64,
}

impl PlayerId {
    /// Build from the two halves, exactly as they cross the boundary.
    pub const fn new(low: u64, high: u64) -> PlayerId {
        PlayerId { low, high }
    }

    /// Split a whole UUID into halves.
    pub const fn from_u128(uuid: u128) -> PlayerId {
        PlayerId {
            low: uuid as u64,
            high: (uuid >> 64) as u64,
        }
    }

    /// Recombine the halves.
    pub const fn to_u128(self) -> u128 {
        (self.low as u128) | ((self.high as u128) << 64)
    }

    /// The canonical 32-hex-digit spelling, used as the KV key.
    pub fn hex(self) -> String {
        format!("{:032x}", self.to_u128())
    }

    /// Parse a 32-hex-digit id. Anything else is `None`; a malformed key in
    /// the store is skipped, not fatal.
    pub fn parse_hex(text: &str) -> Option<PlayerId> {
        if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u128::from_str_radix(text, 16).ok().map(PlayerId::from_u128)
    }
}

/// The counters a player accumulates. Distance, deaths and kills are not
/// derivable from the journal (see the README): they stay `0` and are still
/// reported, so `/stats` never errors on a stat the engine cannot produce.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Stats {
    /// Blocks broken (a `block_set` whose new block is air).
    pub mined: u64,
    /// Blocks placed (a `block_set` whose old block was air).
    pub placed: u64,
    /// Blocks walked. Always `0`: the journal records block edits, not moves.
    pub distance: u64,
    /// Deaths. Always `0`: not in the journal.
    pub deaths: u64,
    /// Kills. Always `0`: not in the journal.
    pub kills: u64,
}

/// A rankable stat.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stat {
    /// Blocks mined.
    Mined,
    /// Blocks placed.
    Placed,
    /// Distance walked (always zero today).
    Distance,
    /// Deaths (always zero today).
    Deaths,
    /// Kills (always zero today).
    Kills,
    /// Accumulated playtime, in seconds.
    Playtime,
}

impl Stat {
    /// Parse a `/leaderboard <stat>` argument. Aliases keep the common
    /// spellings working (`blocks`, `mined`, `time`, …).
    pub fn parse(text: &str) -> Option<Stat> {
        Some(match text.to_ascii_lowercase().as_str() {
            "mined" | "mine" | "blocks" | "block" | "mined_blocks" => Stat::Mined,
            "placed" | "place" | "placed_blocks" => Stat::Placed,
            "distance" | "walked" | "traveled" | "travelled" => Stat::Distance,
            "deaths" | "death" => Stat::Deaths,
            "kills" | "kill" => Stat::Kills,
            "playtime" | "time" | "online" => Stat::Playtime,
            _ => return None,
        })
    }

    /// The label shown in a leaderboard header.
    pub fn label(self) -> &'static str {
        match self {
            Stat::Mined => "blocks mined",
            Stat::Placed => "blocks placed",
            Stat::Distance => "distance",
            Stat::Deaths => "deaths",
            Stat::Kills => "kills",
            Stat::Playtime => "playtime (seconds)",
        }
    }

    /// This stat's value for a player: the stored counter, or the live
    /// playtime total in seconds.
    pub fn value(self, stats: &Stats, playtime_ticks: u64) -> u64 {
        match self {
            Stat::Mined => stats.mined,
            Stat::Placed => stats.placed,
            Stat::Distance => stats.distance,
            Stat::Deaths => stats.deaths,
            Stat::Kills => stats.kills,
            Stat::Playtime => playtime_ticks / TICKS_PER_SECOND,
        }
    }
}

/// What a single journalled block change means for stats.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockAction {
    /// The block that was there is gone.
    Mined,
    /// A block now stands where there was none.
    Placed,
}

/// Whether a block-state or block name is one of the air blocks. A state name
/// carries properties (`minecraft:water[level=0]`); air never does.
pub fn is_air(name: &str) -> bool {
    let bare = name
        .strip_prefix("minecraft:")
        .unwrap_or(name)
        .split('[')
        .next()
        .unwrap_or(name);
    matches!(bare, "air" | "cave_air" | "void_air")
}

/// Classify one journal event into a stats action, if it is one.
///
/// Only `block_set` counts, and only when exactly one side is air: a break
/// (`stone` → `air`) is a mine, a place (`air` → `stone`) is a place. Anything
/// else — air → air, or a fluid replacing a block — is not a player mine/place
/// and is ignored rather than guessed at.
pub fn classify_block_change(
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Option<BlockAction> {
    if kind != "block_set" {
        return None;
    }
    let from_air = from.map(is_air).unwrap_or(false);
    let to_air = to.map(is_air).unwrap_or(false);
    match (from_air, to_air) {
        (false, true) => Some(BlockAction::Mined),
        (true, false) => Some(BlockAction::Placed),
        _ => None,
    }
}

/// One player's cached aggregate plus the sequence watermark already folded
/// in, so a refresh only reads events newer than the last one.
#[derive(Clone, Debug, Default)]
pub struct PlayerRecord {
    /// The last known name; refreshed on every join.
    pub name: String,
    /// The highest event sequence already counted for this player.
    pub watermark: u64,
    /// The counters.
    pub stats: Stats,
}

/// The aggregates, keyed by UUID, with a name index so a rename maps to the
/// same record (CMD-38).
#[derive(Default)]
pub struct Aggregates {
    rows: BTreeMap<PlayerId, PlayerRecord>,
    by_name: BTreeMap<String, PlayerId>,
}

impl Aggregates {
    /// Note that `id` is (still) called `name`, creating a row if needed.
    ///
    /// The row is keyed by UUID, so a rename changes only the displayed name:
    /// the counters and playtime stay with the same player.
    pub fn touch(&mut self, id: PlayerId, name: &str) {
        if name.is_empty() {
            self.rows.entry(id).or_default();
            return;
        }
        match self.rows.get_mut(&id) {
            Some(record) => {
                if !record.name.is_empty() && !record.name.eq_ignore_ascii_case(name) {
                    self.by_name.remove(&record.name.to_ascii_lowercase());
                }
                record.name = name.to_string();
            }
            None => {
                self.rows.insert(
                    id,
                    PlayerRecord {
                        name: name.to_string(),
                        watermark: 0,
                        stats: Stats::default(),
                    },
                );
            }
        }
        self.by_name.insert(name.to_ascii_lowercase(), id);
    }

    /// Resolve a name to the UUID it is (last) known by, case-insensitively.
    pub fn resolve(&self, name: &str) -> Option<PlayerId> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    /// A player's row, if known.
    pub fn get(&self, id: PlayerId) -> Option<&PlayerRecord> {
        self.rows.get(&id)
    }

    /// A player's row, mutably.
    pub fn get_mut(&mut self, id: PlayerId) -> Option<&mut PlayerRecord> {
        self.rows.get_mut(&id)
    }

    /// Every tracked UUID, so a refresh can walk them without holding a
    /// mutable borrow of the map.
    pub fn ids(&self) -> Vec<PlayerId> {
        self.rows.keys().copied().collect()
    }

    /// How many players are tracked.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing is tracked yet.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Fold one event's effect into a player's counters. Callers pass the
    /// event only if it is newer than the player's watermark.
    pub fn apply(&mut self, id: PlayerId, seq: u64, action: BlockAction) {
        let Some(record) = self.rows.get_mut(&id) else {
            return;
        };
        match action {
            BlockAction::Mined => record.stats.mined = record.stats.mined.saturating_add(1),
            BlockAction::Placed => record.stats.placed = record.stats.placed.saturating_add(1),
        }
        record.watermark = record.watermark.max(seq);
    }

    /// The persisted form.
    pub fn to_state(&self) -> AggregatesState {
        AggregatesState {
            rows: self
                .rows
                .iter()
                .map(|(id, record)| AggregatesRow {
                    id: id.hex(),
                    name: record.name.clone(),
                    watermark: record.watermark,
                    stats: record.stats,
                })
                .collect(),
        }
    }

    /// Rebuild from the persisted form; a malformed row is skipped.
    pub fn from_state(state: AggregatesState) -> Aggregates {
        let mut aggregates = Aggregates::default();
        for row in state.rows {
            let Some(id) = PlayerId::parse_hex(&row.id) else {
                continue;
            };
            aggregates.rows.insert(
                id,
                PlayerRecord {
                    name: row.name.clone(),
                    watermark: row.watermark,
                    stats: row.stats,
                },
            );
            if !row.name.is_empty() {
                aggregates.by_name.insert(row.name.to_ascii_lowercase(), id);
            }
        }
        aggregates
    }
}

/// The persisted aggregate rows.
#[derive(Serialize, Deserialize, Default)]
pub struct AggregatesState {
    /// One row per player.
    pub rows: Vec<AggregatesRow>,
}

/// One persisted aggregate row.
#[derive(Serialize, Deserialize)]
pub struct AggregatesRow {
    /// The UUID, hex.
    pub id: String,
    /// The last known name.
    pub name: String,
    /// The sequence watermark.
    pub watermark: u64,
    /// The counters.
    pub stats: Stats,
}

/// One leaderboard row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaderboardEntry {
    /// 1-based rank across the whole list (not just the page).
    pub rank: usize,
    /// The player.
    pub id: PlayerId,
    /// The display name.
    pub name: String,
    /// The stat value.
    pub value: u64,
}

/// How many pages a leaderboard has, at least one (an empty board is one
/// empty page).
pub fn page_count(total_rows: usize) -> usize {
    total_rows.div_ceil(PAGE_SIZE).max(1)
}

/// The `page`-th (1-based) page of the leaderboard for `stat`.
///
/// The order is total and reproducible: value descending, then name ascending
/// (case-insensitive, then exact), then UUID ascending. Two players with the
/// same name and value therefore still order by UUID, so the page boundaries
/// never depend on map iteration order. A page past the end is an empty slice,
/// not an error.
pub fn leaderboard(
    aggregates: &Aggregates,
    playtime: &Playtime,
    stat: Stat,
    now_ticks: u64,
    page: usize,
) -> Vec<LeaderboardEntry> {
    if page == 0 {
        return Vec::new();
    }
    let mut rows: Vec<(PlayerId, String, u64)> = aggregates
        .rows
        .iter()
        .map(|(id, record)| {
            let name = if record.name.is_empty() {
                id.hex()
            } else {
                record.name.clone()
            };
            let value = stat.value(&record.stats, playtime.total_ticks(*id, now_ticks));
            (*id, name, value)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    let start = (page - 1) * PAGE_SIZE;
    if start >= rows.len() {
        return Vec::new();
    }
    rows.into_iter()
        .enumerate()
        .skip(start)
        .take(PAGE_SIZE)
        .map(|(index, (id, name, value))| LeaderboardEntry {
            rank: index + 1,
            id,
            name,
            value,
        })
        .collect()
}

/// The playtime ledger.
///
/// Totals are in world ticks. A live session is a start tick; the reported
/// total is `completed + (now - start)` for an online player, and just
/// `completed` for an offline one. The caller checkpoints — advances
/// `saved_at_ticks` — on a schedule and on every join/leave, then persists the
/// ledger, so a crash loses at most the ticks since the last checkpoint and a
/// restart never counts the server's downtime (the world tick does not advance
/// while the process is down).
#[derive(Default, Clone, Debug)]
pub struct Playtime {
    totals_ticks: BTreeMap<PlayerId, u64>,
    sessions_ticks: BTreeMap<PlayerId, u64>,
    saved_at_ticks: u64,
}

impl Playtime {
    /// Start (or restart) a session at `now_ticks`.
    pub fn join(&mut self, id: PlayerId, now_ticks: u64) {
        self.totals_ticks.entry(id).or_default();
        self.sessions_ticks.insert(id, now_ticks);
    }

    /// End a session at `now_ticks`, folding its length into the total.
    pub fn leave(&mut self, id: PlayerId, now_ticks: u64) {
        if let Some(start) = self.sessions_ticks.remove(&id) {
            let elapsed = now_ticks.saturating_sub(start);
            let total = self.totals_ticks.entry(id).or_default();
            *total = total.saturating_add(elapsed);
        }
    }

    /// Record a heartbeat at `now_ticks`. The sessions keep running; the
    /// heartbeat is the point recovery folds them up to if the process dies
    /// without a clean leave.
    pub fn checkpoint(&mut self, now_ticks: u64) {
        self.saved_at_ticks = now_ticks;
    }

    /// Move any open session that started at `from_ticks` to `to_ticks`.
    ///
    /// A session starts at `0` only when the host had not yet reported a world
    /// tick when the player joined (the plugin initialises from the host, and
    /// its first `on_tick` can be up to a period after a join). Pinning it to
    /// the first observed tick keeps playtime from counting the world's age.
    pub fn rebase_sessions(&mut self, from_ticks: u64, to_ticks: u64) {
        for start in self.sessions_ticks.values_mut() {
            if *start == from_ticks {
                *start = to_ticks;
            }
        }
    }

    /// Whether a session is open for `id`.
    pub fn online(&self, id: PlayerId) -> bool {
        self.sessions_ticks.contains_key(&id)
    }

    /// The total playtime for `id` as of `now_ticks`, counting an open session.
    pub fn total_ticks(&self, id: PlayerId, now_ticks: u64) -> u64 {
        let completed = self.totals_ticks.get(&id).copied().unwrap_or(0);
        let live = self
            .sessions_ticks
            .get(&id)
            .map(|start| now_ticks.saturating_sub(*start))
            .unwrap_or(0);
        completed.saturating_add(live)
    }

    /// The total for `id` ignoring any open session (an offline view).
    pub fn completed_ticks(&self, id: PlayerId) -> u64 {
        self.totals_ticks.get(&id).copied().unwrap_or(0)
    }

    /// The heartbeat tick.
    pub fn saved_at_ticks(&self) -> u64 {
        self.saved_at_ticks
    }

    /// Fold every open session into its total up to the last heartbeat, then
    /// close them. Called once at load: a session that was never closed (a
    /// crash) still counts up to the last checkpoint, and the server's
    /// downtime is not charged.
    pub fn recover(&mut self) {
        let at = self.saved_at_ticks;
        for (id, start) in std::mem::take(&mut self.sessions_ticks) {
            let elapsed = at.saturating_sub(start);
            let total = self.totals_ticks.entry(id).or_default();
            *total = total.saturating_add(elapsed);
        }
    }

    /// The persisted form.
    pub fn to_state(&self) -> PlaytimeState {
        PlaytimeState {
            saved_at_ticks: self.saved_at_ticks,
            totals: self
                .totals_ticks
                .iter()
                .map(|(id, ticks)| (id.hex(), *ticks))
                .collect(),
            sessions: self
                .sessions_ticks
                .iter()
                .map(|(id, ticks)| (id.hex(), *ticks))
                .collect(),
        }
    }

    /// Rebuild from the persisted form; malformed rows are skipped.
    pub fn from_state(state: PlaytimeState) -> Playtime {
        let mut playtime = Playtime {
            saved_at_ticks: state.saved_at_ticks,
            ..Playtime::default()
        };
        for (hex, ticks) in state.totals {
            if let Some(id) = PlayerId::parse_hex(&hex) {
                playtime.totals_ticks.insert(id, ticks);
            }
        }
        for (hex, ticks) in state.sessions {
            if let Some(id) = PlayerId::parse_hex(&hex) {
                playtime.sessions_ticks.insert(id, ticks);
            }
        }
        playtime
    }
}

/// The persisted playtime ledger.
#[derive(Serialize, Deserialize, Default)]
pub struct PlaytimeState {
    /// The last checkpoint tick.
    #[serde(default)]
    pub saved_at_ticks: u64,
    /// `(uuid hex, completed ticks)`.
    #[serde(default)]
    pub totals: Vec<(String, u64)>,
    /// `(uuid hex, session start tick)`.
    #[serde(default)]
    pub sessions: Vec<(String, u64)>,
}

/// Format a tick count as `1d 2h 3m 4s`, dropping the leading zero units.
pub fn format_ticks(ticks: u64) -> String {
    let seconds = ticks / TICKS_PER_SECOND;
    let (days, rem) = (seconds / 86_400, seconds % 86_400);
    let (hours, rem) = (rem / 3600, rem % 3600);
    let (minutes, secs) = (rem / 60, rem % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 || !parts.is_empty() {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || !parts.is_empty() {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{secs}s"));
    parts.join(" ")
}
