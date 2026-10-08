//! The economy, in PostgreSQL.
//!
//! Every operation that moves money or stock runs inside one transaction, and
//! the two that can race — buying and cancelling the same listing — take a row
//! lock first with `SELECT ... FOR UPDATE`. That is not belt and braces: at
//! `READ COMMITTED`, which is Postgres' default, two concurrent buyers would
//! both read `count = 1`, both pass the check, and both be sold the same
//! diamond. The lock is what makes the second one wait and then correctly fail.
//!
//! Balances are held as `BIGINT` minor units with a `CHECK (balance >= 0)`.
//! The check is the last line of defence rather than the first — the `UPDATE`
//! already refuses to overdraw — but it means that if a future code path ever
//! forgets, the database refuses the transaction instead of quietly inventing
//! money.

use aether_world::journal::ActorId;
use postgres::{Client, NoTls, Transaction};

use super::model::{price_purchase, validate_offer, Listing, Money, TradeError};
use super::{Economy, MarketFilter, Purchase};
use crate::db::uuid_text;

/// Applied on connect, so a fresh database needs no setup step.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS aether_accounts (
    player   UUID PRIMARY KEY,
    name     TEXT NOT NULL DEFAULT '',
    balance  BIGINT NOT NULL DEFAULT 0 CHECK (balance >= 0),
    updated  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS aether_listings (
    id           BIGSERIAL PRIMARY KEY,
    seller       UUID NOT NULL,
    seller_name  TEXT NOT NULL DEFAULT '',
    item         TEXT NOT NULL,
    count        BIGINT NOT NULL CHECK (count >= 0),
    unit_price   BIGINT NOT NULL CHECK (unit_price > 0),
    created_ms   BIGINT NOT NULL,
    expires_ms   BIGINT,
    cancelled    BOOLEAN NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS aether_listings_open
    ON aether_listings (item, unit_price)
    WHERE count > 0 AND NOT cancelled;
CREATE INDEX IF NOT EXISTS aether_listings_seller ON aether_listings (seller);

CREATE TABLE IF NOT EXISTS aether_ledger (
    id        BIGSERIAL PRIMARY KEY,
    at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind      TEXT NOT NULL,
    payer     UUID,
    payee     UUID,
    amount    BIGINT NOT NULL,
    listing   BIGINT,
    note      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS aether_ledger_payer ON aether_ledger (payer, at DESC);
CREATE INDEX IF NOT EXISTS aether_ledger_payee ON aether_ledger (payee, at DESC);
";

fn fail(e: impl std::fmt::Display) -> TradeError {
    TradeError::Unavailable(e.to_string())
}

/// A `postgres::Error` with its cause.
///
/// The crate's `Display` is the useless "db error"; everything that says what
/// actually went wrong is in the source. A message an operator cannot act on
/// is barely better than no message.
fn db_fail(e: postgres::Error) -> TradeError {
    match std::error::Error::source(&e) {
        Some(cause) => TradeError::Unavailable(format!("{e}: {cause}")),
        None => TradeError::Unavailable(e.to_string()),
    }
}

/// Apply the schema, once per process.
///
/// Postgres' `CREATE TABLE IF NOT EXISTS` is not safe against a concurrent
/// identical `CREATE`: the existence check and the insert into the catalogue
/// are not atomic, so two connections starting together can collide on
/// `pg_type_typname_nsp_index`. Doing it once behind a lock removes the race
/// for this process, and the retry below covers the remaining case of two
/// *processes* starting at the same moment.
fn ensure_schema(c: &mut Client) -> Result<(), TradeError> {
    static DONE: std::sync::Mutex<bool> = std::sync::Mutex::new(false);
    let mut done = DONE.lock().map_err(fail)?;
    if *done {
        return Ok(());
    }
    let mut last = None;
    for _ in 0..3 {
        match c.batch_execute(SCHEMA) {
            Ok(()) => {
                *done = true;
                return Ok(());
            }
            Err(e) => {
                last = Some(db_fail(e));
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    Err(last.unwrap_or_else(|| TradeError::Unavailable("schema not applied".into())))
}

/// A PostgreSQL-backed [`Economy`].
///
/// Holds one connection behind a mutex rather than a pool: economy commands
/// are typed by hand, a handful a minute at most, and a pool would add a
/// dependency and a failure mode to save nothing measurable.
pub struct PgEconomy {
    url: String,
    client: std::sync::Mutex<Option<Client>>,
}

impl PgEconomy {
    /// Connect and apply the schema.
    pub fn connect(url: &str) -> Result<PgEconomy, TradeError> {
        let e = PgEconomy {
            url: url.to_owned(),
            client: std::sync::Mutex::new(None),
        };
        e.with(|_| Ok(()))?;
        Ok(e)
    }

    /// Run `f` with a live client, reconnecting once if the connection has
    /// gone away.
    ///
    /// A dropped connection is normal — a database restart, an idle timeout —
    /// and retrying once turns it into a pause instead of a failed trade. It
    /// retries *once*: a second failure is a real outage, and a command that
    /// kept retrying would hang the player's connection thread.
    fn with<T>(&self, f: impl Fn(&mut Client) -> Result<T, TradeError>) -> Result<T, TradeError> {
        let mut guard = self.client.lock().map_err(fail)?;
        for attempt in 0..2 {
            if guard.is_none() {
                let mut c = Client::connect(&self.url, NoTls).map_err(db_fail)?;
                ensure_schema(&mut c)?;
                *guard = Some(c);
            }
            let client = guard.as_mut().expect("connected above");
            match f(client) {
                Ok(v) => return Ok(v),
                // Only a transport failure is worth reconnecting for. A
                // refused trade is an answer, not an outage, and retrying it
                // would run the whole transaction a second time.
                Err(TradeError::Unavailable(msg)) if attempt == 0 && client.is_closed() => {
                    *guard = None;
                    let _ = msg;
                }
                Err(e) => return Err(e),
            }
        }
        Err(TradeError::Unavailable("connection lost twice".into()))
    }
}

/// Read a balance inside a transaction, creating the account if it is new.
fn balance_locked(tx: &mut Transaction<'_>, who: ActorId) -> Result<Money, TradeError> {
    let id = uuid_text(who.0);
    tx.execute(
        "INSERT INTO aether_accounts (player) VALUES ($1::text::uuid) ON CONFLICT DO NOTHING",
        &[&id],
    )
    .map_err(db_fail)?;
    let row = tx
        .query_one(
            "SELECT balance FROM aether_accounts WHERE player = $1::text::uuid FOR UPDATE",
            &[&id],
        )
        .map_err(db_fail)?;
    Ok(Money(row.get::<_, i64>(0)))
}

/// Move money between two accounts inside an open transaction.
fn transfer(
    tx: &mut Transaction<'_>,
    from: ActorId,
    to: ActorId,
    amount: Money,
    kind: &str,
    listing: Option<i64>,
    note: &str,
) -> Result<(), TradeError> {
    // Lock in a fixed order — lower uuid first — so two payments in opposite
    // directions between the same pair cannot deadlock by each holding what
    // the other wants.
    let (a, b) = if from.0 <= to.0 {
        (from, to)
    } else {
        (to, from)
    };
    balance_locked(tx, a)?;
    balance_locked(tx, b)?;

    let have = balance_locked(tx, from)?;
    if have < amount {
        return Err(TradeError::Insufficient { need: amount, have });
    }
    tx.execute(
        "UPDATE aether_accounts SET balance = balance - $2::text::bigint, updated = now() \
         WHERE player = $1::text::uuid",
        &[&uuid_text(from.0), &amount.0.to_string()],
    )
    .map_err(db_fail)?;
    tx.execute(
        "UPDATE aether_accounts SET balance = balance + $2::text::bigint, updated = now() \
         WHERE player = $1::text::uuid",
        &[&uuid_text(to.0), &amount.0.to_string()],
    )
    .map_err(db_fail)?;
    tx.execute(
        "INSERT INTO aether_ledger (kind, payer, payee, amount, listing, note) \
         VALUES ($1, $2::text::uuid, $3::text::uuid, $4::text::bigint, $5::text::bigint, $6)",
        &[
            &kind,
            &uuid_text(from.0),
            &uuid_text(to.0),
            &amount.0.to_string(),
            &listing.map(|l| l.to_string()),
            &note,
        ],
    )
    .map_err(db_fail)?;
    Ok(())
}

fn listing_from_row(row: &postgres::Row) -> Listing {
    Listing {
        id: row.get::<_, i64>("id"),
        seller: ActorId(uuid_to_u128(row.get::<_, uuid_shim::Uuid>("seller"))),
        seller_name: row.get::<_, String>("seller_name"),
        item: row.get::<_, String>("item"),
        count: row.get::<_, i64>("count").max(0) as u64,
        unit_price: Money(row.get::<_, i64>("unit_price")),
        created_ms: row.get::<_, i64>("created_ms").max(0) as u64,
        expires_ms: row
            .get::<_, Option<i64>>("expires_ms")
            .map(|v| v.max(0) as u64),
    }
}

/// The `postgres` crate returns a `uuid::Uuid` for a `UUID` column only when
/// its `with-uuid` feature is on; without it the column comes back as text
/// through a cast. Selecting `seller::text` keeps this build free of a uuid
/// dependency, and this shim keeps the row reader honest about that.
mod uuid_shim {
    /// A UUID as Postgres handed it to us: text, because the query casts it.
    pub type Uuid = String;
}

fn uuid_to_u128(s: uuid_shim::Uuid) -> u128 {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    u128::from_str_radix(&hex, 16).unwrap_or(0)
}

/// The column list every listing query must produce, in the order
/// [`listing_from_row`] reads.
const LISTING_COLUMNS: &str = "id, seller::text AS seller, seller_name, item, count, \
                               unit_price, created_ms, expires_ms";

impl Economy for PgEconomy {
    fn balance(&self, who: ActorId) -> Result<Money, TradeError> {
        self.with(|c| {
            let mut tx = c.transaction().map_err(db_fail)?;
            let b = balance_locked(&mut tx, who)?;
            tx.commit().map_err(db_fail)?;
            Ok(b)
        })
    }

    fn pay(&self, from: ActorId, to: ActorId, amount: Money) -> Result<(), TradeError> {
        if from == to {
            return Err(TradeError::SelfTrade);
        }
        if !amount.is_valid_price() {
            return Err(TradeError::BadOffer("the amount must be above zero"));
        }
        self.with(|c| {
            let mut tx = c.transaction().map_err(db_fail)?;
            transfer(&mut tx, from, to, amount, "pay", None, "")?;
            tx.commit().map_err(db_fail)?;
            Ok(())
        })
    }

    fn mint(&self, who: ActorId, amount: Money, reason: &str) -> Result<(), TradeError> {
        if !amount.is_valid_price() {
            return Err(TradeError::BadOffer("the amount must be above zero"));
        }
        self.with(|c| {
            let mut tx = c.transaction().map_err(db_fail)?;
            balance_locked(&mut tx, who)?;
            tx.execute(
                "UPDATE aether_accounts SET balance = balance + $2::text::bigint, updated = now() \
                 WHERE player = $1::text::uuid",
                &[&uuid_text(who.0), &amount.0.to_string()],
            )
            .map_err(db_fail)?;
            tx.execute(
                "INSERT INTO aether_ledger (kind, payee, amount, note) \
                 VALUES ('mint', $1::text::uuid, $2::text::bigint, $3)",
                &[&uuid_text(who.0), &amount.0.to_string(), &reason],
            )
            .map_err(db_fail)?;
            tx.commit().map_err(db_fail)?;
            Ok(())
        })
    }

    fn list(
        &self,
        seller: ActorId,
        item: &str,
        count: u64,
        price: Money,
        expires_in_secs: Option<u64>,
    ) -> Result<i64, TradeError> {
        validate_offer(count, price)?;
        let now = aether_world::journal::now_ms();
        let expires = expires_in_secs.map(|s| now + s * 1000);
        self.with(|c| {
            let row = c
                .query_one(
                    "INSERT INTO aether_listings \
                     (seller, item, count, unit_price, created_ms, expires_ms) \
                     VALUES ($1::text::uuid, $2, $3::text::bigint, $4::text::bigint, \
                             $5::text::bigint, $6::text::bigint) RETURNING id",
                    &[
                        &uuid_text(seller.0),
                        &item,
                        &count.to_string(),
                        &price.0.to_string(),
                        &now.to_string(),
                        &expires.map(|e| e.to_string()),
                    ],
                )
                .map_err(db_fail)?;
            Ok(row.get::<_, i64>(0))
        })
    }

    fn buy(&self, buyer: ActorId, id: i64, count: u64) -> Result<Purchase, TradeError> {
        self.with(|c| {
            let mut tx = c.transaction().map_err(db_fail)?;
            // The lock, and the reason this whole method exists as a
            // transaction: without it two buyers both read the same stock.
            let row = tx
                .query_opt(
                    &format!(
                        "SELECT {LISTING_COLUMNS} FROM aether_listings \
                         WHERE id = $1::text::bigint AND NOT cancelled FOR UPDATE"
                    ),
                    &[&id.to_string()],
                )
                .map_err(db_fail)?;
            let listing = match row {
                Some(r) => listing_from_row(&r),
                None => return Err(TradeError::NoSuchListing(id)),
            };
            let balance = balance_locked(&mut tx, buyer)?;
            let cost = price_purchase(
                &listing,
                buyer,
                count,
                balance,
                aether_world::journal::now_ms(),
            )?;

            tx.execute(
                "UPDATE aether_listings SET count = count - $2::text::bigint \
                 WHERE id = $1::text::bigint",
                &[&id.to_string(), &count.to_string()],
            )
            .map_err(db_fail)?;
            transfer(
                &mut tx,
                buyer,
                listing.seller,
                cost,
                "buy",
                Some(id),
                &listing.item,
            )?;
            tx.commit().map_err(db_fail)?;
            Ok(Purchase {
                listing: id,
                seller: listing.seller,
                item: listing.item,
                count,
                paid: cost,
            })
        })
    }

    fn cancel(&self, seller: ActorId, id: i64) -> Result<Listing, TradeError> {
        self.with(|c| {
            let mut tx = c.transaction().map_err(db_fail)?;
            let row = tx
                .query_opt(
                    &format!(
                        "SELECT {LISTING_COLUMNS} FROM aether_listings \
                         WHERE id = $1::text::bigint FOR UPDATE"
                    ),
                    &[&id.to_string()],
                )
                .map_err(db_fail)?;
            let listing = match row {
                Some(r) => listing_from_row(&r),
                None => return Err(TradeError::NoSuchListing(id)),
            };
            if listing.seller != seller {
                return Err(TradeError::NotYours(id));
            }
            let n = tx
                .execute(
                    "UPDATE aether_listings SET cancelled = true \
                     WHERE id = $1::text::bigint AND NOT cancelled",
                    &[&id.to_string()],
                )
                .map_err(db_fail)?;
            if n == 0 {
                return Err(TradeError::ListingClosed(id));
            }
            tx.commit().map_err(db_fail)?;
            Ok(listing)
        })
    }

    fn market(&self, filter: &MarketFilter) -> Result<Vec<Listing>, TradeError> {
        let now = aether_world::journal::now_ms();
        let limit = filter.limit.clamp(1, 500);
        self.with(|c| {
            let rows = c
                .query(
                    &format!(
                        "SELECT {LISTING_COLUMNS} FROM aether_listings \
                         WHERE NOT cancelled AND count > 0 \
                           AND (expires_ms IS NULL OR expires_ms > $1::text::bigint) \
                           AND ($2::text IS NULL OR item = $2::text) \
                           AND ($3::text IS NULL OR seller = $3::text::uuid) \
                           AND ($4::text IS NULL OR unit_price <= $4::text::bigint) \
                         ORDER BY unit_price ASC, id DESC LIMIT $5::text::bigint"
                    ),
                    &[
                        &now.to_string(),
                        &filter.item,
                        &filter.seller.map(|s| uuid_text(s.0)),
                        &filter.max_price.map(|p| p.0.to_string()),
                        &limit.to_string(),
                    ],
                )
                .map_err(db_fail)?;
            Ok(rows.iter().map(listing_from_row).collect())
        })
    }

    fn listings_of(&self, seller: ActorId) -> Result<Vec<Listing>, TradeError> {
        self.with(|c| {
            let rows = c
                .query(
                    &format!(
                        "SELECT {LISTING_COLUMNS} FROM aether_listings \
                         WHERE seller = $1::text::uuid AND NOT cancelled AND count > 0 \
                         ORDER BY id DESC LIMIT 200"
                    ),
                    &[&uuid_text(seller.0)],
                )
                .map_err(db_fail)?;
            Ok(rows.iter().map(listing_from_row).collect())
        })
    }
}

/// Live checks against a real PostgreSQL, ignored unless one is named by
/// `AETHER_DATABASE_URL`.
///
/// ```text
/// cargo test -p aether-server --features postgres -- --ignored economy::pg::live
/// ```
///
/// These exist for the properties that only a database has: that a payment is
/// atomic, and that two buyers racing for the last item do not both get it.
/// No amount of testing the model can establish either.
#[cfg(test)]
mod live_tests {
    use super::*;

    fn econ() -> PgEconomy {
        let url = std::env::var("AETHER_DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .expect("AETHER_DATABASE_URL is not set");
        PgEconomy::connect(&url).expect("connect + apply schema")
    }

    /// A fresh player each run, so tests never inherit a balance.
    fn player(tag: u8) -> ActorId {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        ActorId((t << 8) | tag as u128)
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn a_new_account_starts_empty_and_a_grant_lands() {
        let e = econ();
        let a = player(1);
        assert_eq!(e.balance(a).unwrap(), Money::ZERO);
        e.mint(a, Money::coins(50), "test").unwrap();
        assert_eq!(e.balance(a).unwrap(), Money::coins(50));
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn a_payment_moves_the_whole_amount_and_no_more() {
        let e = econ();
        let (a, b) = (player(2), player(3));
        e.mint(a, Money::coins(10), "test").unwrap();
        e.pay(a, b, Money::coins(4)).unwrap();
        assert_eq!(e.balance(a).unwrap(), Money::coins(6));
        assert_eq!(e.balance(b).unwrap(), Money::coins(4));
        // Conservation: the pair holds exactly what was minted.
        assert_eq!(
            e.balance(a).unwrap().0 + e.balance(b).unwrap().0,
            Money::coins(10).0
        );
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn an_overdraft_is_refused_and_changes_nothing() {
        let e = econ();
        let (a, b) = (player(4), player(5));
        e.mint(a, Money::coins(3), "test").unwrap();
        assert!(matches!(
            e.pay(a, b, Money::coins(5)),
            Err(TradeError::Insufficient { .. })
        ));
        assert_eq!(e.balance(a).unwrap(), Money::coins(3), "unchanged");
        assert_eq!(e.balance(b).unwrap(), Money::ZERO);
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn buying_moves_money_and_stock_together() {
        let e = econ();
        let (seller, buyer) = (player(6), player(7));
        e.mint(buyer, Money::coins(100), "test").unwrap();
        let id = e
            .list(seller, "minecraft:diamond", 10, Money::coins(3), None)
            .unwrap();
        let p = e.buy(buyer, id, 4).unwrap();
        assert_eq!(p.paid, Money::coins(12));
        assert_eq!(e.balance(buyer).unwrap(), Money::coins(88));
        assert_eq!(e.balance(seller).unwrap(), Money::coins(12));
        let left = e.listings_of(seller).unwrap();
        assert_eq!(left.iter().find(|l| l.id == id).unwrap().count, 6);
        e.cancel(seller, id).unwrap();
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn two_buyers_racing_for_the_last_item_do_not_both_get_it() {
        // The reason `buy` is a transaction with `FOR UPDATE`. At READ
        // COMMITTED both threads would otherwise read count = 1, both pass the
        // stock check, and the listing would go to minus one.
        let (seller, b1, b2) = (player(8), player(9), player(10));
        {
            let e = econ();
            e.mint(b1, Money::coins(100), "test").unwrap();
            e.mint(b2, Money::coins(100), "test").unwrap();
        }
        let id = econ()
            .list(seller, "minecraft:beacon", 1, Money::coins(5), None)
            .unwrap();

        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [b1, b2]
            .into_iter()
            .map(|who| {
                let start = std::sync::Arc::clone(&start);
                // A connection each: the race has to be between two
                // transactions, not two calls sharing one client.
                std::thread::spawn(move || {
                    let e = econ();
                    start.wait();
                    e.buy(who, id, 1)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "exactly one buyer, got {results:?}");

        let spent = econ().balance(b1).unwrap().0 + econ().balance(b2).unwrap().0;
        assert_eq!(
            spent,
            Money::coins(200).0 - Money::coins(5).0,
            "exactly one payment was made"
        );
        assert_eq!(econ().balance(seller).unwrap(), Money::coins(5));
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn a_cancelled_listing_cannot_be_bought_or_cancelled_again() {
        let e = econ();
        let (seller, buyer) = (player(11), player(12));
        e.mint(buyer, Money::coins(100), "test").unwrap();
        let id = e
            .list(seller, "minecraft:emerald", 5, Money::coins(1), None)
            .unwrap();
        let back = e.cancel(seller, id).unwrap();
        assert_eq!(back.count, 5, "the seller gets the remainder back");
        assert!(matches!(
            e.buy(buyer, id, 1),
            Err(TradeError::NoSuchListing(_))
        ));
        assert!(matches!(
            e.cancel(seller, id),
            Err(TradeError::ListingClosed(_))
        ));
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn only_the_seller_can_withdraw_a_listing() {
        let e = econ();
        let (seller, other) = (player(13), player(14));
        let id = e
            .list(seller, "minecraft:apple", 1, Money::coins(1), None)
            .unwrap();
        assert!(matches!(e.cancel(other, id), Err(TradeError::NotYours(_))));
        e.cancel(seller, id).unwrap();
    }

    #[test]
    #[ignore = "needs a live PostgreSQL named by AETHER_DATABASE_URL"]
    fn the_market_hides_what_is_closed_and_orders_by_price() {
        let e = econ();
        let seller = player(15);
        let cheap = e
            .list(seller, "minecraft:test_item", 1, Money::coins(1), None)
            .unwrap();
        let dear = e
            .list(seller, "minecraft:test_item", 1, Money::coins(9), None)
            .unwrap();
        let gone = e
            .list(seller, "minecraft:test_item", 1, Money::coins(5), None)
            .unwrap();
        e.cancel(seller, gone).unwrap();

        let rows = e
            .market(&MarketFilter {
                item: Some("minecraft:test_item".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<i64> = rows.iter().map(|l| l.id).collect();
        assert!(ids.contains(&cheap) && ids.contains(&dear));
        assert!(
            !ids.contains(&gone),
            "a cancelled listing is not on the market"
        );
        let cheap_at = ids.iter().position(|i| *i == cheap).unwrap();
        let dear_at = ids.iter().position(|i| *i == dear).unwrap();
        assert!(cheap_at < dear_at, "cheapest first");

        e.cancel(seller, cheap).unwrap();
        e.cancel(seller, dear).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uuid_column_round_trips_through_text() {
        let v = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdefu128;
        assert_eq!(uuid_to_u128(uuid_text(v)), v);
        assert_eq!(uuid_to_u128(uuid_text(0)), 0, "the server actor");
    }

    #[test]
    fn an_unreadable_uuid_reads_as_the_server_rather_than_panicking() {
        // It comes from the database, so it is well-formed in practice — but a
        // panic on a connection thread would drop a player's connection, and
        // the nil actor is the safe wrong answer: it owns nothing and can buy
        // nothing.
        assert_eq!(uuid_to_u128("not-a-uuid".into()), 0);
    }

    #[test]
    fn the_column_list_names_every_field_the_row_reader_asks_for() {
        // These drift apart silently: a missing column is a runtime panic in
        // `row.get` on the first market query, which is a long way from here.
        for field in [
            "id",
            "seller",
            "seller_name",
            "item",
            "count",
            "unit_price",
            "created_ms",
            "expires_ms",
        ] {
            assert!(LISTING_COLUMNS.contains(field), "{field} is not selected");
        }
    }
}
