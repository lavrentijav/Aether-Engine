//! Unit tests for the pure model and the engine, against in-memory doubles.
//!
//! Every awkward case the task names is exercised here: an offline target, a
//! deterministic tie, a page past the end, a stat with no data, a session that
//! survives a restart and one that survives a crash, a rename, the others
//! permission, and the guarantee that a leaderboard request does not touch the
//! journal.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::engine::{
    CommandRequest, History, HistoryPage, JournalEvent, PermissionCheck, StatsEngine, Store,
    NODE_STATS_OTHERS,
};
use crate::model::{format_ticks, PlayerId, Stat, TICKS_PER_SECOND};

// --- doubles -----------------------------------------------------------

#[derive(Default)]
struct MemStore {
    map: RefCell<HashMap<String, Vec<u8>>>,
}

impl Store for MemStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.map.borrow().get(key).cloned()
    }
    fn put(&self, key: &str, value: &[u8]) {
        self.map
            .borrow_mut()
            .insert(key.to_string(), value.to_vec());
    }
}

/// A history source holding each player's events, newest first, and counting
/// how many pages it was asked for.
#[derive(Default)]
struct MockHistory {
    events: HashMap<PlayerId, Vec<JournalEvent>>,
    calls: RefCell<Vec<PlayerId>>,
}

impl MockHistory {
    fn add(&mut self, id: PlayerId, event: JournalEvent) {
        let list = self.events.entry(id).or_default();
        list.push(event);
        list.sort_by_key(|a| std::cmp::Reverse(a.seq));
    }
    fn calls(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl History for MockHistory {
    fn player_events(&self, id: PlayerId, before: Option<u64>, limit: u32) -> HistoryPage {
        self.calls.borrow_mut().push(id);
        let all = self.events.get(&id).cloned().unwrap_or_default();
        let start = match before {
            Some(cursor) => all.iter().position(|e| e.seq < cursor).unwrap_or(all.len()),
            None => 0,
        };
        let page: Vec<JournalEvent> = all
            .iter()
            .skip(start)
            .take(limit as usize)
            .cloned()
            .collect();
        let next = if start + page.len() < all.len() {
            page.last().map(|e| e.seq)
        } else {
            None
        };
        HistoryPage { events: page, next }
    }
}

struct Perms {
    allowed: Vec<String>,
}

impl Perms {
    fn player() -> Perms {
        Perms { allowed: vec![] }
    }
    fn staff() -> Perms {
        Perms {
            allowed: vec![NODE_STATS_OTHERS.to_string()],
        }
    }
}

impl PermissionCheck for Perms {
    fn has(&self, node: &str) -> bool {
        self.allowed.iter().any(|n| n == node)
    }
}

// --- helpers -----------------------------------------------------------

fn pid(value: u128) -> PlayerId {
    PlayerId::from_u128(value)
}

fn block(seq: u64, from: &str, to: &str) -> JournalEvent {
    JournalEvent {
        seq,
        kind: "block_set".to_string(),
        from_block: Some(from.to_string()),
        to_block: Some(to.to_string()),
    }
}

fn mined(seq: u64) -> JournalEvent {
    block(seq, "minecraft:stone", "minecraft:air")
}

fn placed(seq: u64) -> JournalEvent {
    block(seq, "minecraft:air", "minecraft:stone")
}

fn request<'a>(
    caller: PlayerId,
    name: &'a str,
    args: &'a [String],
    perms: &'a dyn PermissionCheck,
) -> CommandRequest<'a> {
    CommandRequest {
        caller,
        caller_name: name,
        args,
        perms,
    }
}

const STEVE: u128 = 1;
const ALEX: u128 = 2;
const BOB: u128 = 3;

// --- classification ----------------------------------------------------

#[test]
fn a_break_is_a_mine_and_a_place_is_a_place() {
    use crate::model::{classify_block_change, is_air, BlockAction};
    assert_eq!(
        classify_block_change("block_set", Some("minecraft:stone"), Some("minecraft:air")),
        Some(BlockAction::Mined)
    );
    assert_eq!(
        classify_block_change("block_set", Some("minecraft:air"), Some("minecraft:stone")),
        Some(BlockAction::Placed)
    );
    // Water is not air: replacing a block with water is not a player mine.
    assert_eq!(
        classify_block_change(
            "block_set",
            Some("minecraft:stone"),
            Some("minecraft:water[level=0]")
        ),
        None
    );
    // Air to air, and every non-block event, is ignored.
    assert_eq!(
        classify_block_change(
            "block_set",
            Some("minecraft:air"),
            Some("minecraft:void_air")
        ),
        None
    );
    assert_eq!(classify_block_change("item_move", None, None), None);
    assert!(is_air("minecraft:cave_air"));
    assert!(!is_air("minecraft:water[level=0]"));
}

// --- aggregation and refresh -------------------------------------------

#[test]
fn refresh_folds_only_new_events() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();

    engine.on_join(pid(STEVE), "Steve", 0, &store);
    for seq in 1..=3 {
        history.add(pid(STEVE), mined(seq));
    }
    history.add(pid(STEVE), placed(4));
    engine.refresh(&history, 100, &store);
    assert_eq!(engine.aggregates.get(pid(STEVE)).unwrap().stats.mined, 3);
    assert_eq!(engine.aggregates.get(pid(STEVE)).unwrap().stats.placed, 1);

    // A second refresh with nothing new does not double count, and stops on
    // the first already-counted event.
    engine.refresh(&history, 200, &store);
    assert_eq!(engine.aggregates.get(pid(STEVE)).unwrap().stats.mined, 3);

    // One more event: only it is folded.
    history.add(pid(STEVE), mined(5));
    engine.refresh(&history, 300, &store);
    assert_eq!(engine.aggregates.get(pid(STEVE)).unwrap().stats.mined, 4);
}

#[test]
fn a_leaderboard_request_does_not_touch_the_journal() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();
    for player in 1..=200u128 {
        engine.on_join(pid(player), &format!("player{player}"), 0, &store);
        history.add(pid(player), mined(1));
    }
    engine.refresh(&history, 100, &store);
    let after_refresh = history.calls();

    let perms = Perms::player();
    let args = vec!["mined".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "player1", &args, &perms),
        100,
    );
    let _ = lines;
    let args2 = vec!["mined".to_string(), "2".to_string()];
    let _ = engine.handle("lb", &request(pid(STEVE), "player1", &args2, &perms), 100);
    // No history call was made by either request: the cache answered.
    assert_eq!(history.calls(), after_refresh);
}

// --- /stats edge cases -------------------------------------------------

#[test]
fn an_offline_target_is_answered_from_the_cache() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();

    engine.on_join(pid(ALEX), "Alex", 0, &store);
    history.add(pid(ALEX), mined(1));
    history.add(pid(ALEX), mined(2));
    engine.refresh(&history, 100, &store);
    engine.on_leave(pid(ALEX), 300, &store);
    assert!(!engine.playtime.online(pid(ALEX)));

    // Staff ask about the offline player; the cached aggregate answers.
    let perms = Perms::staff();
    let args = vec!["Alex".to_string()];
    let lines = engine.handle("stats", &request(pid(STEVE), "Steve", &args, &perms), 400);
    assert!(lines[0].contains("Alex"), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("Blocks mined: 2")),
        "{lines:?}"
    );
    // 300 - 0 ticks at 20 tps = 15 seconds.
    assert!(lines.iter().any(|l| l.contains("15s")), "{lines:?}");
}

#[test]
fn a_stat_with_no_data_is_zero_not_an_error() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    engine.on_join(pid(STEVE), "Steve", 0, &store);
    let history = MockHistory::default();
    engine.refresh(&history, 100, &store);

    let perms = Perms::player();
    let lines = engine.handle("stats", &request(pid(STEVE), "Steve", &[], &perms), 100);
    assert!(
        lines.iter().any(|l| l.contains("Blocks mined: 0")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("Kills: 0")), "{lines:?}");
    // The player is tracked, so the board shows them at zero, not an error.
    let args = vec!["mined".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "Steve", &args, &perms),
        100,
    );
    assert_eq!(
        lines,
        vec![
            "Leaderboard: blocks mined (page 1/1, 10 per page)".to_string(),
            "1. Steve - 0".to_string(),
        ]
    );
    // With nobody tracked at all, the board says so rather than erroring.
    let empty = StatsEngine::load(&MemStore::default());
    let lines = empty.handle(
        "leaderboard",
        &request(pid(STEVE), "Steve", &args, &perms),
        0,
    );
    assert_eq!(lines, vec!["No stats recorded yet.".to_string()]);
}

// --- leaderboard edges -------------------------------------------------

#[test]
fn ties_break_deterministically_by_name_then_uuid() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();
    // Two players with equal stats, names out of uuid order; and two more
    // with the same name and the same value, to force the uuid tie-break.
    engine.on_join(pid(10), "Zed", 0, &store);
    engine.on_join(pid(20), "Amy", 0, &store);
    engine.on_join(pid(50), "Twin", 0, &store);
    engine.on_join(pid(30), "Twin", 0, &store);
    for player in [10u128, 20, 50, 30] {
        history.add(pid(player), mined(1));
    }
    engine.refresh(&history, 100, &store);

    let perms = Perms::player();
    let args = vec!["mined".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "Steve", &args, &perms),
        100,
    );
    let body: Vec<&str> = lines[1..].iter().map(String::as_str).collect();
    // Every value is equal, so the order is by name (Amy < Twin < Zed), and
    // the two Twins then order by uuid (30 before 50).
    assert_eq!(
        body,
        vec!["1. Amy - 1", "2. Twin - 1", "3. Twin - 1", "4. Zed - 1"]
    );
    // The Twin ordering is by uuid, deterministically: rank 2 is pid 30.
    let entries =
        crate::model::leaderboard(&engine.aggregates, &engine.playtime, Stat::Mined, 100, 1);
    assert_eq!(entries[1].id, pid(30));
    assert_eq!(entries[2].id, pid(50));
}

#[test]
fn paging_past_the_end_is_an_empty_page_not_a_panic() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();
    for player in 1..=15u128 {
        engine.on_join(pid(player), &format!("p{player}"), 0, &store);
        history.add(pid(player), mined(1));
    }
    engine.refresh(&history, 100, &store);

    let perms = Perms::player();
    // 15 rows / 10 per page = 2 pages.
    let args = vec!["mined".to_string(), "2".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "p1", &args, &perms),
        100,
    );
    assert!(lines[0].contains("page 2/2"), "{lines:?}");
    assert_eq!(lines.len(), 1 + 5);

    let args = vec!["mined".to_string(), "3".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "p1", &args, &perms),
        100,
    );
    assert_eq!(
        lines,
        vec!["No entries on page 3; the leaderboard has 2 page(s).".to_string()]
    );

    // Page 0 and a non-number are refused.
    let args = vec!["mined".to_string(), "0".to_string()];
    let lines = engine.handle(
        "leaderboard",
        &request(pid(STEVE), "p1", &args, &perms),
        100,
    );
    assert!(lines[0].contains("positive number"), "{lines:?}");
}

// --- playtime ----------------------------------------------------------

#[test]
fn playtime_survives_a_restart() {
    let store = MemStore::default();
    {
        let mut engine = StatsEngine::load(&store);
        engine.on_join(pid(STEVE), "Steve", 0, &store);
        engine.on_leave(pid(STEVE), 10 * TICKS_PER_SECOND, &store);
    }
    // A fresh engine reads the same store, as after a clean restart.
    let engine = StatsEngine::load(&store);
    let perms = Perms::player();
    let lines = engine.handle("playtime", &request(pid(STEVE), "Steve", &[], &perms), 0);
    assert!(lines[0].contains("10s"), "{lines:?}");
    assert_eq!(engine.playtime.completed_ticks(pid(STEVE)), 200);
}

#[test]
fn a_crash_keeps_the_session_up_to_the_last_checkpoint() {
    let store = MemStore::default();
    {
        let mut engine = StatsEngine::load(&store);
        engine.on_join(pid(STEVE), "Steve", 0, &store);
        // The scheduler heartbeat runs up to tick 100; then the process dies
        // without a leave.
        engine.checkpoint(100, &store);
    }
    // Reload: recovery folds the open session up to the heartbeat. The server
    // was down between 100 and now, but the world tick did not advance, so the
    // downtime is not charged.
    let engine = StatsEngine::load(&store);
    assert!(
        !engine.playtime.online(pid(STEVE)),
        "the stale session is closed"
    );
    let perms = Perms::player();
    let lines = engine.handle("playtime", &request(pid(STEVE), "Steve", &[], &perms), 100);
    assert!(lines[0].contains("5s"), "{lines:?}");
}

#[test]
fn a_session_started_before_the_first_tick_is_pinned_to_it() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    // A join before the plugin has seen a world tick records start 0.
    engine.on_join(pid(STEVE), "Steve", 0, &store);
    // The first `on_tick` pins it; the session is counted from there, not
    // from the epoch.
    engine.rebase_open_sessions(0, 500, &store);
    engine.on_leave(pid(STEVE), 700, &store);
    assert_eq!(engine.playtime.completed_ticks(pid(STEVE)), 200);
}

#[test]
fn playtime_top_lists_by_time_and_others_needs_permission() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    engine.on_join(pid(STEVE), "Steve", 0, &store);
    engine.on_join(pid(ALEX), "Alex", 0, &store);
    engine.on_leave(pid(STEVE), 20 * TICKS_PER_SECOND, &store);
    engine.on_leave(pid(ALEX), 40 * TICKS_PER_SECOND, &store);

    let perms = Perms::player();
    let args = vec!["top".to_string()];
    let lines = engine.handle("playtime", &request(pid(BOB), "Bob", &args, &perms), 0);
    assert!(
        lines.iter().any(|l| l.contains("1. Alex - 40")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("2. Steve - 20")),
        "{lines:?}"
    );

    // A bare player without the node cannot ask about someone else…
    let args = vec!["Alex".to_string()];
    let denied = engine.handle("playtime", &request(pid(BOB), "Bob", &args, &perms), 0);
    assert!(denied[0].contains("permission"), "{denied:?}");
    // …but staff can.
    let staff = Perms::staff();
    let allowed = engine.handle("playtime", &request(pid(BOB), "Bob", &args, &staff), 0);
    assert!(
        allowed[0].contains("Alex") && allowed[0].contains("40s"),
        "{allowed:?}"
    );
}

// --- rename (CMD-38) ---------------------------------------------------

#[test]
fn a_renamed_player_maps_to_the_same_uuid() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    let mut history = MockHistory::default();

    engine.on_join(pid(BOB), "Bob", 0, &store);
    history.add(pid(BOB), mined(1));
    history.add(pid(BOB), mined(2));
    engine.refresh(&history, 100, &store);
    engine.on_leave(pid(BOB), 100, &store);

    // The player comes back with a new name; the UUID is the same.
    engine.on_join(pid(BOB), "Bobby", 1000, &store);
    assert_eq!(engine.aggregates.len(), 1, "one record, keyed by uuid");
    assert_eq!(engine.aggregates.get(pid(BOB)).unwrap().stats.mined, 2);
    // The old name no longer resolves; the new one does.
    assert_eq!(engine.aggregates.resolve("Bob"), None);
    assert_eq!(engine.aggregates.resolve("bobby"), Some(pid(BOB)));

    let perms = Perms::staff();
    let args = vec!["Bobby".to_string()];
    let lines = engine.handle("stats", &request(pid(STEVE), "Steve", &args, &perms), 1000);
    assert!(
        lines.iter().any(|l| l.contains("Blocks mined: 2")),
        "{lines:?}"
    );
}

// --- permissions -------------------------------------------------------

#[test]
fn players_may_query_themselves_but_not_others_without_the_node() {
    let store = MemStore::default();
    let mut engine = StatsEngine::load(&store);
    engine.on_join(pid(STEVE), "Steve", 0, &store);
    engine.on_join(pid(ALEX), "Alex", 0, &store);

    // Self is always allowed, even for a bare player.
    let perms = Perms::player();
    let lines = engine.handle("stats", &request(pid(STEVE), "Steve", &[], &perms), 0);
    assert!(lines[0].contains("Steve"), "{lines:?}");

    // Someone else is refused without `stats.stats.others`.
    let args = vec!["Alex".to_string()];
    let denied = engine.handle("stats", &request(pid(STEVE), "Steve", &args, &perms), 0);
    assert!(denied[0].contains("permission"), "{denied:?}");

    // With it, allowed.
    let staff = Perms::staff();
    let allowed = engine.handle("stats", &request(pid(STEVE), "Steve", &args, &staff), 0);
    assert!(allowed[0].contains("Alex"), "{allowed:?}");
}

// --- formatting --------------------------------------------------------

#[test]
fn durations_drop_leading_zero_units() {
    assert_eq!(format_ticks(0), "0s");
    assert_eq!(format_ticks(TICKS_PER_SECOND * 5), "5s");
    assert_eq!(format_ticks(TICKS_PER_SECOND * 65), "1m 5s");
    assert_eq!(format_ticks(TICKS_PER_SECOND * 3661), "1h 1m 1s");
    assert_eq!(
        format_ticks(TICKS_PER_SECOND * (86_400 + 3600 + 60 + 1)),
        "1d 1h 1m 1s"
    );
}
