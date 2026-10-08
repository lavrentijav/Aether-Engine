//! Mirroring the world history into PostgreSQL.
//!
//! The journal on disk answers the two questions the server asks while it
//! runs. This answers everything else — "who has broken the most diamond ore
//! this week", "show me every action within 50 blocks of spawn last Tuesday",
//! a dashboard — by putting the same events somewhere with a query planner.
//!
//! Everything about the delivery path lives in
//! [`aether_world::journal::sink`]: batched, off the game thread, retried,
//! and free to fall behind because the journal, not this, is the source of
//! truth. What lives here is the schema and the statement.
//!
//! # Idempotence
//!
//! [`EventSink::write`] may be handed the same batch twice after an ambiguous
//! failure, so every insert ends in `ON CONFLICT (seq) DO NOTHING`. The
//! sequence number is the primary key, which makes replaying a batch — or
//! backfilling the whole journal over a live table — safe by construction.
//!
//! # Enabling it
//!
//! Build with `--features postgres` and set a connection string in
//! `aether-server.toml`:
//!
//! ```toml
//! [database]
//! url = "host=localhost user=aether dbname=aether"
//! ```
//!
//! With no `url`, nothing is mirrored and the server runs exactly as before.

// The schema and the row builder are compiled unconditionally so that they
// stay type-checked and unit-tested in a default build, where nothing calls
// them: a mirror whose SQL only compiles under a feature flag is a mirror
// nobody notices breaking.
#![cfg_attr(not(feature = "postgres"), allow(dead_code))]

#[cfg(feature = "postgres")]
use aether_world::journal::EventSink;
use aether_world::journal::{Event, EventBody, Place};

/// The schema. Applied on connect, so a fresh database needs no setup step.
///
/// Column choices worth naming:
/// * `seq` is the primary key, which is what makes a retried batch harmless.
/// * actors and item uids are `uuid` rather than a numeric: they *are* UUIDs,
///   and Postgres indexes and prints them properly.
/// * `cx`/`cz` are stored, not computed, because `x >> 4` is not what
///   Postgres' `>>` does for negative numbers — it has no arithmetic shift —
///   and an index on a subtly wrong expression is worse than no index.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS aether_events (
    seq         BIGINT PRIMARY KEY,
    at_ms       BIGINT NOT NULL,
    at          TIMESTAMPTZ GENERATED ALWAYS AS (to_timestamp(at_ms / 1000.0)) STORED,
    actor       UUID NOT NULL,
    kind        TEXT NOT NULL,
    x           INTEGER,
    y           INTEGER,
    z           INTEGER,
    cx          INTEGER,
    cz          INTEGER,
    block_from  BIGINT,
    block_to    BIGINT,
    item_uid    UUID,
    item_name   TEXT,
    item_count  INTEGER,
    place_from  TEXT,
    place_to    TEXT
);
CREATE INDEX IF NOT EXISTS aether_events_actor_seq ON aether_events (actor, seq DESC);
CREATE INDEX IF NOT EXISTS aether_events_column    ON aether_events (cx, cz, seq DESC);
CREATE INDEX IF NOT EXISTS aether_events_at        ON aether_events (at DESC);
CREATE INDEX IF NOT EXISTS aether_events_item      ON aether_events (item_uid) WHERE item_uid IS NOT NULL;
";

/// Columns and their types, in the order [`row_values`] produces them.
///
/// The types are here because every value is bound as **text** and cast in the
/// statement. Postgres parameters are typed — unlike literals, a text
/// parameter is not silently parsed into a `bigint` column, it is rejected
/// with "error serializing parameter" — so each placeholder carries its cast.
/// One text-shaped row builder plus a cast list is far less code than sixteen
/// `&dyn ToSql` of four different types per event shape, and Postgres parses
/// the text exactly as it would a literal.
const COLUMNS: [(&str, &str); 16] = [
    ("seq", "bigint"),
    ("at_ms", "bigint"),
    ("actor", "uuid"),
    ("kind", "text"),
    ("x", "integer"),
    ("y", "integer"),
    ("z", "integer"),
    ("cx", "integer"),
    ("cz", "integer"),
    ("block_from", "bigint"),
    ("block_to", "bigint"),
    ("item_uid", "uuid"),
    ("item_name", "text"),
    ("item_count", "integer"),
    ("place_from", "text"),
    ("place_to", "text"),
];
/// How many of them.
const NCOLS: usize = COLUMNS.len();

/// Render a `u128` as a UUID string. Actors already are UUIDs; item uids are
/// 128 random bits, which is the same thing.
pub fn uuid_text(v: u128) -> String {
    let b = v.to_be_bytes();
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Render a [`Place`] as a short, greppable string.
///
/// Text rather than a set of typed columns: a place is one of four shapes with
/// different fields, and four nullable column groups would make every query
/// that touches them unreadable. Item provenance is followed by uid, not by
/// place.
pub fn place_text(p: &Place) -> String {
    match p {
        Place::Inventory { owner, slot } => format!("inv:{}:{slot}", uuid_text(owner.0)),
        Place::Ground { x, y, z } => format!("ground:{x},{y},{z}"),
        Place::Container { x, y, z, slot } => format!("container:{x},{y},{z}:{slot}"),
        Place::Nowhere => "nowhere".to_string(),
    }
}

/// One row's worth of values, as nullable strings in `COLUMNS` order.
///
/// Strings throughout, cast by the statement: it keeps one code path for every
/// event shape, and Postgres parses a literal into the column's type just as
/// well as it binds a typed parameter. The alternative — sixteen
/// `&dyn ToSql` of four different types per row — is a great deal of code to
/// express the same thing.
pub fn row_values(e: &Event) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = vec![None; NCOLS];
    v[0] = Some(e.seq.to_string());
    v[1] = Some(e.at_ms.to_string());
    v[2] = Some(uuid_text(e.actor.0));
    if let Some((x, y, z)) = e.position() {
        v[4] = Some(x.to_string());
        v[5] = Some(y.to_string());
        v[6] = Some(z.to_string());
        // Arithmetic shift, computed here where the language has one.
        v[7] = Some((x >> 4).to_string());
        v[8] = Some((z >> 4).to_string());
    }
    match &e.body {
        EventBody::BlockSet { from, to, .. } => {
            v[3] = Some("block_set".into());
            v[9] = Some(from.0.to_string());
            v[10] = Some(to.0.to_string());
        }
        EventBody::ItemMint {
            uid,
            item,
            count,
            to,
        } => {
            v[3] = Some("item_mint".into());
            v[11] = Some(uuid_text(uid.0));
            v[12] = Some(item.clone());
            v[13] = Some(count.to_string());
            v[15] = Some(place_text(to));
        }
        EventBody::ItemMove {
            uid,
            from,
            to,
            count,
        } => {
            v[3] = Some("item_move".into());
            v[11] = Some(uuid_text(uid.0));
            v[13] = Some(count.to_string());
            v[14] = Some(place_text(from));
            v[15] = Some(place_text(to));
        }
        EventBody::ItemDestroy { uid, from } => {
            v[3] = Some("item_destroy".into());
            v[11] = Some(uuid_text(uid.0));
            v[14] = Some(place_text(from));
        }
    }
    v
}

/// The multi-row `INSERT` for a batch of `rows` rows.
///
/// Built rather than prepared once because the placeholder count depends on
/// the batch size, and a partial final batch is normal. `ON CONFLICT DO
/// NOTHING` is what makes a retry — and a backfill over a live table —
/// harmless.
pub fn insert_statement(rows: usize) -> String {
    let names: Vec<&str> = COLUMNS.iter().map(|(n, _)| *n).collect();
    let mut sql = format!("INSERT INTO aether_events ({}) VALUES ", names.join(", "));
    for r in 0..rows {
        if r > 0 {
            sql.push(',');
        }
        sql.push('(');
        for (c, (_, ty)) in COLUMNS.iter().enumerate() {
            if c > 0 {
                sql.push(',');
            }
            // Two casts, and the first one is the load-bearing one. With a
            // bare `$1::bigint` Postgres infers the *parameter* as bigint and
            // the client then refuses to send a string for it. `$1::text`
            // pins the parameter's type to text; the second cast is what
            // actually parses it.
            sql.push_str(&format!("${}::text::{ty}", r * NCOLS + c + 1));
        }
        sql.push(')');
    }
    sql.push_str(" ON CONFLICT (seq) DO NOTHING");
    sql
}

#[cfg(feature = "postgres")]
pub use live::PostgresSink;

#[cfg(feature = "postgres")]
mod live {
    use super::*;
    use postgres::{Client, NoTls};

    /// A [`EventSink`] that writes into PostgreSQL.
    pub struct PostgresSink {
        url: String,
        client: Option<Client>,
    }

    impl PostgresSink {
        /// Connect and apply the schema.
        ///
        /// Failing here is reported and does **not** stop the server: a
        /// mirror that cannot be reached is a degraded audit trail, not a
        /// broken world, and refusing to boot over it would be the wrong
        /// trade.
        pub fn connect(url: &str) -> Result<PostgresSink, String> {
            let mut s = PostgresSink {
                url: url.to_owned(),
                client: None,
            };
            s.reconnect()?;
            Ok(s)
        }

        fn reconnect(&mut self) -> Result<(), String> {
            let mut c = Client::connect(&self.url, NoTls).map_err(|e| e.to_string())?;
            c.batch_execute(SCHEMA).map_err(|e| e.to_string())?;
            self.client = Some(c);
            Ok(())
        }
    }

    impl EventSink for PostgresSink {
        fn write(&mut self, batch: &[Event]) -> Result<(), String> {
            if batch.is_empty() {
                return Ok(());
            }
            // A dropped connection surfaces as a write error; reconnecting
            // here rather than failing the batch is what lets the server sit
            // through a database restart without losing the mirror.
            if self.client.is_none() {
                self.reconnect()?;
            }
            let values: Vec<Vec<Option<String>>> = batch.iter().map(row_values).collect();
            let flat: Vec<&(dyn postgres::types::ToSql + Sync)> = values
                .iter()
                .flatten()
                .map(|v| v as &(dyn postgres::types::ToSql + Sync))
                .collect();
            let sql = insert_statement(batch.len());
            let client = self.client.as_mut().expect("reconnected above");
            match client.execute(sql.as_str(), &flat) {
                Ok(_) => Ok(()),
                Err(e) => {
                    // Drop the client so the next attempt reconnects rather
                    // than reusing a socket that may be gone.
                    self.client = None;
                    Err(e.to_string())
                }
            }
        }

        fn describe(&self) -> String {
            "postgres".into()
        }
    }
}

/// Live checks against a real PostgreSQL, ignored unless one is named by
/// `AETHER_DATABASE_URL`.
///
/// ```text
/// cargo test -p aether-server --features postgres -- --ignored db::live_tests
/// ```
///
/// What these prove that the unit tests cannot: that the generated SQL is
/// valid, that the column types accept the strings the sink sends, and that
/// `ON CONFLICT` really makes a replayed batch a no-op rather than an error.
#[cfg(all(test, feature = "postgres"))]
mod live_tests {
    use super::*;
    use aether_world::journal::{ActorId, ItemUid};
    use aether_world::BlockStateId;

    /// A sequence range these tests own, far above anything a world under
    /// test would reach, so they clean up after themselves without touching
    /// anything else in the table.
    const BASE: i64 = 9_000_000_000;

    fn url() -> String {
        std::env::var("AETHER_DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .expect("AETHER_DATABASE_URL is not set")
    }

    fn batch() -> Vec<Event> {
        vec![
            Event {
                seq: BASE as u64,
                at_ms: 1_700_000_000_000,
                actor: ActorId(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
                body: EventBody::BlockSet {
                    x: -33,
                    y: 70,
                    z: 17,
                    from: BlockStateId(3),
                    to: BlockStateId(1),
                },
            },
            Event {
                seq: BASE as u64 + 1,
                at_ms: 1_700_000_001_000,
                actor: ActorId::SERVER,
                body: EventBody::ItemMint {
                    uid: ItemUid(0xdead_beef),
                    item: "minecraft:diamond".into(),
                    count: 64,
                    to: Place::Inventory {
                        owner: ActorId(1),
                        slot: 3,
                    },
                },
            },
            Event {
                seq: BASE as u64 + 2,
                at_ms: 1_700_000_002_000,
                actor: ActorId(1),
                body: EventBody::ItemDestroy {
                    uid: ItemUid(0xdead_beef),
                    from: Place::Ground { x: 1, y: 2, z: 3 },
                },
            },
        ]
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn a_batch_lands_and_replaying_it_changes_nothing() {
        use aether_world::journal::EventSink as _;
        let url = url();
        let mut sink = PostgresSink::connect(&url).expect("connect + apply schema");
        let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let clear = |c: &mut postgres::Client| {
            c.execute("DELETE FROM aether_events WHERE seq >= $1", &[&BASE])
                .unwrap();
        };
        let count = |c: &mut postgres::Client| -> i64 {
            c.query_one(
                "SELECT count(*) FROM aether_events WHERE seq >= $1",
                &[&BASE],
            )
            .unwrap()
            .get(0)
        };
        clear(&mut client);

        sink.write(&batch()).expect("first write");
        assert_eq!(count(&mut client), 3, "every row in the batch landed");

        // The property the whole retry path depends on.
        sink.write(&batch()).expect("replay");
        assert_eq!(count(&mut client), 3, "a replay must not duplicate rows");

        let row = client
            .query_one(
                "SELECT kind, cx, cz, block_to FROM aether_events WHERE seq = $1",
                &[&BASE],
            )
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "block_set");
        assert_eq!(row.get::<_, i32>(1), -3, "cx from an arithmetic shift");
        assert_eq!(row.get::<_, i32>(2), 1, "cz");
        assert_eq!(row.get::<_, i64>(3), 1);

        let kinds: Vec<String> = client
            .query(
                "SELECT kind FROM aether_events WHERE seq >= $1 ORDER BY seq",
                &[&BASE],
            )
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(kinds, ["block_set", "item_mint", "item_destroy"]);

        clear(&mut client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::journal::{ActorId, ItemUid};
    use aether_world::BlockStateId;

    fn block_event() -> Event {
        Event {
            seq: 42,
            at_ms: 1_700_000_000_000,
            actor: ActorId(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            body: EventBody::BlockSet {
                x: -33,
                y: 70,
                z: 17,
                from: BlockStateId(3),
                to: BlockStateId(1),
            },
        }
    }

    #[test]
    fn a_u128_renders_as_a_canonical_uuid() {
        assert_eq!(
            uuid_text(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(
            uuid_text(0),
            "00000000-0000-0000-0000-000000000000",
            "the server actor is the nil uuid, not an error"
        );
    }

    #[test]
    fn the_chunk_columns_use_an_arithmetic_shift() {
        // Postgres has no arithmetic right shift, so this must be computed in
        // Rust. -33 >> 4 is -3, not 268435453 — getting it wrong would file
        // every negative-coordinate event under a nonsense column and quietly
        // break every area query in the west and north of the world.
        let v = row_values(&block_event());
        assert_eq!(v[7].as_deref(), Some("-3"), "cx");
        assert_eq!(v[8].as_deref(), Some("1"), "cz");
    }

    #[test]
    fn a_block_event_fills_the_block_columns_and_leaves_the_item_ones_null() {
        let v = row_values(&block_event());
        assert_eq!(v.len(), NCOLS);
        assert_eq!(v[3].as_deref(), Some("block_set"));
        assert_eq!(v[9].as_deref(), Some("3"));
        assert_eq!(v[10].as_deref(), Some("1"));
        assert!(v[11].is_none() && v[12].is_none() && v[13].is_none());
    }

    #[test]
    fn an_item_event_fills_the_item_columns() {
        let e = Event {
            seq: 7,
            at_ms: 1,
            actor: ActorId(1),
            body: EventBody::ItemMint {
                uid: ItemUid(9),
                item: "minecraft:diamond".into(),
                count: 64,
                to: Place::Inventory {
                    owner: ActorId(1),
                    slot: 3,
                },
            },
        };
        let v = row_values(&e);
        assert_eq!(v[3].as_deref(), Some("item_mint"));
        assert_eq!(v[12].as_deref(), Some("minecraft:diamond"));
        assert_eq!(v[13].as_deref(), Some("64"));
        assert!(v[15].as_deref().unwrap().starts_with("inv:"));
        // An inventory slot has no world position, so those stay null rather
        // than defaulting to 0,0,0 and polluting every area query.
        assert!(v[4].is_none() && v[7].is_none());
    }

    #[test]
    fn every_place_shape_renders_distinctly() {
        let rendered: Vec<String> = [
            Place::Inventory {
                owner: ActorId(1),
                slot: 2,
            },
            Place::Ground { x: 1, y: 2, z: 3 },
            Place::Container {
                x: 1,
                y: 2,
                z: 3,
                slot: 4,
            },
            Place::Nowhere,
        ]
        .iter()
        .map(place_text)
        .collect();
        let unique: std::collections::HashSet<&String> = rendered.iter().collect();
        assert_eq!(
            unique.len(),
            4,
            "two places rendered the same: {rendered:?}"
        );
    }

    #[test]
    fn the_statement_numbers_its_placeholders_consecutively_from_one() {
        // An off-by-one here binds every value to the wrong column, and
        // Postgres would accept many of them because they are all text.
        let sql = insert_statement(3);
        assert!(sql.starts_with("INSERT INTO aether_events ("));
        assert!(sql.ends_with("ON CONFLICT (seq) DO NOTHING"));
        for n in 1..=3 * NCOLS {
            assert!(sql.contains(&format!("${n}")), "missing placeholder ${n}");
        }
        assert!(
            !sql.contains(&format!("${}", 3 * NCOLS + 1)),
            "one placeholder too many"
        );
    }

    #[test]
    fn the_column_list_and_the_row_width_agree() {
        // These two drift apart the moment a column is added to one and not
        // the other, and the result is a runtime error on the first insert.
        assert_eq!(row_values(&block_event()).len(), NCOLS);
    }

    #[test]
    fn every_placeholder_carries_its_cast() {
        // Without the casts a text parameter bound to a bigint column fails
        // with "error serializing parameter", which is a runtime error no
        // amount of statement-shape checking would catch.
        let sql = insert_statement(1);
        assert!(sql.contains("$1::text::bigint"), "seq");
        assert!(sql.contains("$3::text::uuid"), "actor");
        assert!(sql.contains("$5::text::integer"), "x");
        assert!(sql.contains("$16::text::text"), "place_to");
        for n in 1..=NCOLS {
            assert!(
                sql.contains(&format!("${n}::text::")),
                "placeholder ${n} is not pinned to text"
            );
        }
    }

    #[test]
    fn a_single_row_batch_is_a_valid_statement() {
        let sql = insert_statement(1);
        assert!(sql.contains("VALUES ($1::text::bigint,"));
        assert!(!sql.contains(",("), "no stray empty tuple");
    }
}
