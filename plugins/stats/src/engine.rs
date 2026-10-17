//! The stats engine: the model wired to a history source, a KV store and the
//! permission checker, with no host or WIT types in sight.
//!
//! The component (`component.rs`) adapts the SDK imports to the [`History`],
//! [`Store`] and [`PermissionCheck`] traits below; the tests adapt in-memory
//! doubles. `/stats`, `/leaderboard` and `/playtime` are all answered from the
//! cached aggregates here, so a request never walks the journal.

use crate::model::{
    classify_block_change, format_ticks, leaderboard, page_count, Aggregates, AggregatesState,
    PlayerId, Playtime, PlaytimeState, Stat, PAGE_SIZE,
};

/// The KV key holding the aggregate rows.
pub const KEY_AGGREGATES: &str = "aggregates";
/// The KV key holding the playtime ledger.
pub const KEY_PLAYTIME: &str = "playtime";

/// The most events one history page returns; PA2 clamps larger requests.
pub const PAGE_LIMIT: u32 = 1000;

/// The permission node a player needs to ask about someone else.
pub const NODE_STATS_OTHERS: &str = "stats.stats.others";

/// One journal event, as much as the stats model needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEvent {
    /// Position in the global order.
    pub seq: u64,
    /// Stable kind name (`block_set`, `item_move`, …).
    pub kind: String,
    /// For a `block_set`: the block that was there.
    pub from_block: Option<String>,
    /// For a `block_set`: the block that replaced it.
    pub to_block: Option<String>,
}

/// One page of history, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryPage {
    /// The events, newest first.
    pub events: Vec<JournalEvent>,
    /// The cursor for the next, older page.
    pub next: Option<u64>,
}

/// A read-only view over the journal (PA2).
pub trait History {
    /// `id`'s events, newest first, paged from `before`.
    fn player_events(&self, id: PlayerId, before: Option<u64>, limit: u32) -> HistoryPage;
}

/// The plugin's own KV namespace (`storage.kv`).
pub trait Store {
    /// Read a value, or `None`.
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// Write a value.
    fn put(&self, key: &str, value: &[u8]);
}

/// The permission checker the engine consults for `stats.stats.others`.
pub trait PermissionCheck {
    /// Whether the caller holds `node`.
    fn has(&self, node: &str) -> bool;
}

/// One command invocation.
pub struct CommandRequest<'a> {
    /// The caller's UUID.
    pub caller: PlayerId,
    /// The caller's current name.
    pub caller_name: &'a str,
    /// The command arguments, already split.
    pub args: &'a [String],
    /// The caller's permissions.
    pub perms: &'a dyn PermissionCheck,
}

/// The cached stats engine.
#[derive(Default)]
pub struct StatsEngine {
    /// Per-player counters and watermarks.
    pub aggregates: Aggregates,
    /// The playtime ledger.
    pub playtime: Playtime,
}

impl StatsEngine {
    /// Load the persisted state, recovering any session left open by a crash.
    ///
    /// A parse failure — an older schema, a truncated write — is treated as an
    /// empty store rather than an error: stats are a best-effort cache, not
    /// player data that may fail to load.
    pub fn load(store: &dyn Store) -> StatsEngine {
        let aggregates = store
            .get(KEY_AGGREGATES)
            .and_then(|bytes| serde_json::from_slice::<AggregatesState>(&bytes).ok())
            .map(Aggregates::from_state)
            .unwrap_or_default();
        let mut playtime = store
            .get(KEY_PLAYTIME)
            .and_then(|bytes| serde_json::from_slice::<PlaytimeState>(&bytes).ok())
            .map(Playtime::from_state)
            .unwrap_or_default();
        playtime.recover();
        StatsEngine {
            aggregates,
            playtime,
        }
    }

    /// Persist the aggregates and the playtime ledger.
    pub fn persist(&self, store: &dyn Store) {
        if let Ok(bytes) = serde_json::to_vec(&self.aggregates.to_state()) {
            store.put(KEY_AGGREGATES, &bytes);
        }
        if let Ok(bytes) = serde_json::to_vec(&self.playtime.to_state()) {
            store.put(KEY_PLAYTIME, &bytes);
        }
    }

    /// Record a join: resolve the identity, start the session and persist.
    ///
    /// Checkpointing on join is what makes playtime survive a crash: the
    /// session start is on disk before the player has done anything.
    pub fn on_join(&mut self, id: PlayerId, name: &str, now_ticks: u64, store: &dyn Store) {
        self.aggregates.touch(id, name);
        self.playtime.join(id, now_ticks);
        self.playtime.checkpoint(now_ticks);
        self.persist(store);
    }

    /// Record a leave: fold the session into the total and persist.
    pub fn on_leave(&mut self, id: PlayerId, now_ticks: u64, store: &dyn Store) {
        self.playtime.leave(id, now_ticks);
        self.playtime.checkpoint(now_ticks);
        self.persist(store);
    }

    /// Advance the playtime heartbeat and persist, without touching history.
    pub fn checkpoint(&mut self, now_ticks: u64, store: &dyn Store) {
        self.playtime.checkpoint(now_ticks);
        self.persist(store);
    }

    /// Pin sessions that started before the plugin saw a world tick (start
    /// `0`) to the first observed tick; the component calls this on its first
    /// `on_tick`.
    pub fn rebase_open_sessions(&mut self, from_ticks: u64, to_ticks: u64, store: &dyn Store) {
        self.playtime.rebase_sessions(from_ticks, to_ticks);
        self.persist(store);
    }

    /// Fold every event newer than each player's watermark into their
    /// counters, then checkpoint the playtime ledger.
    ///
    /// This runs from the scheduler, never from a command: a request reads the
    /// cache. Each player is read through their own actor index (PA2), so a
    /// busy server costs only the new events, not the whole journal.
    pub fn refresh(&mut self, history: &dyn History, now_ticks: u64, store: &dyn Store) {
        for id in self.aggregates.ids() {
            let watermark = self
                .aggregates
                .get(id)
                .map(|row| row.watermark)
                .unwrap_or(0);
            let mut before = None;
            let mut newest = watermark;
            loop {
                let page = history.player_events(id, before, PAGE_LIMIT);
                if page.events.is_empty() {
                    break;
                }
                let mut done = false;
                for event in &page.events {
                    if event.seq <= watermark {
                        // Pages are newest-first, so the first event already
                        // counted ends the walk.
                        done = true;
                        break;
                    }
                    newest = newest.max(event.seq);
                    if let Some(action) = classify_block_change(
                        &event.kind,
                        event.from_block.as_deref(),
                        event.to_block.as_deref(),
                    ) {
                        self.aggregates.apply(id, event.seq, action);
                    }
                }
                if done {
                    break;
                }
                match page.next {
                    Some(next) if Some(next) != before => before = Some(next),
                    _ => break,
                }
            }
            if let Some(row) = self.aggregates.get_mut(id) {
                row.watermark = row.watermark.max(newest);
            }
        }
        self.checkpoint(now_ticks, store);
    }

    /// Answer one of the plugin's commands. The name is one of `stats`,
    /// `leaderboard`/`lb` or `playtime`; an unknown name is not handled.
    pub fn handle(&self, name: &str, request: &CommandRequest, now_ticks: u64) -> Vec<String> {
        match name {
            "stats" => self.stats_command(request, now_ticks),
            "leaderboard" | "lb" => self.leaderboard_command(request, now_ticks),
            "playtime" => self.playtime_command(request, now_ticks),
            _ => Vec::new(),
        }
    }

    fn stats_command(&self, request: &CommandRequest, now_ticks: u64) -> Vec<String> {
        let (id, name) = match self.resolve_target(request, "stats") {
            Ok(target) => target,
            Err(lines) => return lines,
        };
        let stats = self
            .aggregates
            .get(id)
            .map(|row| row.stats)
            .unwrap_or_default();
        let playtime = self.playtime.total_ticks(id, now_ticks);
        let online = if self.playtime.online(id) {
            " &7(online)"
        } else {
            ""
        };
        vec![
            format!("Stats for {name}{online}:"),
            format!("  Blocks mined: {}", stats.mined),
            format!("  Blocks placed: {}", stats.placed),
            format!("  Distance: {} m", stats.distance),
            format!("  Deaths: {}", stats.deaths),
            format!("  Kills: {}", stats.kills),
            format!("  Playtime: {}", format_ticks(playtime)),
        ]
    }

    fn leaderboard_command(&self, request: &CommandRequest, now_ticks: u64) -> Vec<String> {
        let Some(stat_arg) = request.args.first() else {
            return vec![
                "Usage: /leaderboard <stat> [page]; stats: mined, placed, distance, deaths, \
                 kills, playtime"
                    .to_string(),
            ];
        };
        let Some(stat) = Stat::parse(stat_arg) else {
            return vec![format!(
                "Unknown stat `{stat_arg}`; try mined, placed, distance, deaths, kills or playtime."
            )];
        };
        let Some(page) = parse_page(request.args.get(1)) else {
            return vec!["The page must be a positive number.".to_string()];
        };
        self.render_leaderboard(stat, page, now_ticks)
    }

    fn playtime_command(&self, request: &CommandRequest, now_ticks: u64) -> Vec<String> {
        match request.args.first().map(String::as_str) {
            Some(top) if top.eq_ignore_ascii_case("top") => {
                let Some(page) = parse_page(request.args.get(1)) else {
                    return vec!["The page must be a positive number.".to_string()];
                };
                self.render_leaderboard(Stat::Playtime, page, now_ticks)
            }
            // No argument, or the caller's own name: the caller's own time,
            // which needs no extra permission.
            None => self.render_playtime(request.caller, request.caller_name, now_ticks),
            Some(name) if name.eq_ignore_ascii_case(request.caller_name) => {
                self.render_playtime(request.caller, request.caller_name, now_ticks)
            }
            Some(name) => {
                if !request.perms.has(NODE_STATS_OTHERS) {
                    return vec![
                        "You do not have permission to view other players' playtime.".to_string(),
                    ];
                }
                match self.aggregates.resolve(name) {
                    Some(id) => {
                        let display = self
                            .aggregates
                            .get(id)
                            .map(|row| row.name.clone())
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| name.to_string());
                        self.render_playtime(id, &display, now_ticks)
                    }
                    None => vec![format!("No playtime recorded for {name}.")],
                }
            }
        }
    }

    fn render_playtime(&self, id: PlayerId, name: &str, now_ticks: u64) -> Vec<String> {
        let ticks = self.playtime.total_ticks(id, now_ticks);
        vec![format!("Playtime for {name}: {}", format_ticks(ticks))]
    }

    fn render_leaderboard(&self, stat: Stat, page: usize, now_ticks: u64) -> Vec<String> {
        if self.aggregates.is_empty() {
            return vec!["No stats recorded yet.".to_string()];
        }
        let pages = page_count(self.aggregates.len());
        if page > pages {
            return vec![format!(
                "No entries on page {page}; the leaderboard has {pages} page(s)."
            )];
        }
        let entries = leaderboard(&self.aggregates, &self.playtime, stat, now_ticks, page);
        let mut lines = vec![format!(
            "Leaderboard: {} (page {page}/{pages}, {} per page)",
            stat.label(),
            PAGE_SIZE
        )];
        for entry in &entries {
            lines.push(format!("{}. {} - {}", entry.rank, entry.name, entry.value));
        }
        lines
    }

    /// Resolve the `/stats [player]` target, applying the others permission.
    fn resolve_target(
        &self,
        request: &CommandRequest,
        what: &str,
    ) -> Result<(PlayerId, String), Vec<String>> {
        match request.args.first() {
            None => Ok((request.caller, request.caller_name.to_string())),
            Some(name) if name.eq_ignore_ascii_case(request.caller_name) => {
                Ok((request.caller, request.caller_name.to_string()))
            }
            Some(name) => {
                if !request.perms.has(NODE_STATS_OTHERS) {
                    return Err(vec![format!(
                        "You do not have permission to view other players' {what}."
                    )]);
                }
                match self.aggregates.resolve(name) {
                    Some(id) => {
                        let display = self
                            .aggregates
                            .get(id)
                            .map(|row| row.name.clone())
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| name.to_string());
                        Ok((id, display))
                    }
                    None => Err(vec![format!("No {what} recorded for {name}.")]),
                }
            }
        }
    }
}

/// Parse a 1-based page argument; absent is page 1, and `0` or a non-number is
/// an error (`None`).
fn parse_page(arg: Option<&String>) -> Option<usize> {
    match arg {
        None => Some(1),
        Some(text) => match text.parse::<usize>() {
            Ok(0) | Err(_) => None,
            Ok(page) => Some(page),
        },
    }
}
