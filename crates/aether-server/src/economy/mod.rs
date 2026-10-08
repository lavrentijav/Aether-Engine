//! Balances, direct payments, shop listings and auctions.
//!
//! # Why this one is different
//!
//! Everything else in this server treats PostgreSQL as a *mirror*: derived,
//! allowed to lag, allowed to be missing, because the journal on disk is the
//! truth. The economy is the opposite. Money and listings live **in the
//! database and only there**, inside transactions, because the properties they
//! need are the ones a database exists to provide:
//!
//! * A payment must debit and credit as one act, or it prints or burns money.
//! * Two players buying the same listing at once must not both get it.
//! * A balance must never be readable in a state no transaction produced.
//!
//! None of that is achievable in a KV store of independent puts, and
//! approximating it would produce a currency that quietly inflates. So the
//! rule here is the reverse of everywhere else: **if the database is
//! unreachable, economy commands fail and say so.** A trade that silently did
//! not happen is worse than a trade that visibly could not.
//!
//! # Amounts
//!
//! Money is integer minor units — cents — never floating point. `0.1 + 0.2`
//! is a bug report waiting to be filed, and a currency that loses a hundredth
//! of a coin per transaction is a currency players will find a way to exploit.
//! [`Money`] does the formatting.

// See `model.rs`: the trait and its types are compiled unconditionally so a
// default build still type-checks them, even though only the PostgreSQL
// backend implements them.
#![cfg_attr(not(feature = "postgres"), allow(dead_code))]

pub mod commands;
pub mod model;

#[cfg(feature = "postgres")]
pub mod pg;

pub use model::{Listing, Money, TradeError};

use aether_world::journal::ActorId;

/// What the server can ask of an economy backend.
///
/// A trait so the command layer can be written and tested against the model
/// without a database, and so a second backend — ClickHouse for analytics
/// alongside, a different SQL engine — does not mean rewriting the commands.
pub trait Economy: Send + Sync {
    /// The balance of `who`, creating the account at zero if it is new.
    fn balance(&self, who: ActorId) -> Result<Money, TradeError>;

    /// Move `amount` from `from` to `to`, atomically. Fails if `from` cannot
    /// cover it.
    fn pay(&self, from: ActorId, to: ActorId, amount: Money) -> Result<(), TradeError>;

    /// Credit `who` without debiting anyone — an admin grant, a server reward.
    fn mint(&self, who: ActorId, amount: Money, reason: &str) -> Result<(), TradeError>;

    /// Offer `count` of `item` for `price` *each*. Returns the listing id.
    fn list(
        &self,
        seller: ActorId,
        item: &str,
        count: u64,
        price: Money,
        expires_in_secs: Option<u64>,
    ) -> Result<i64, TradeError>;

    /// Buy `count` from listing `id`, atomically: the buyer's balance, the
    /// seller's balance and the listing's remaining count all move together or
    /// none of them do.
    fn buy(&self, buyer: ActorId, id: i64, count: u64) -> Result<Purchase, TradeError>;

    /// Withdraw a listing, returning what is left of it to the seller.
    fn cancel(&self, seller: ActorId, id: i64) -> Result<Listing, TradeError>;

    /// Open listings, newest first.
    fn market(&self, filter: &MarketFilter) -> Result<Vec<Listing>, TradeError>;

    /// One player's own open listings.
    fn listings_of(&self, seller: ActorId) -> Result<Vec<Listing>, TradeError>;
}

/// What a completed purchase moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purchase {
    pub listing: i64,
    pub seller: ActorId,
    pub item: String,
    pub count: u64,
    /// Total paid, not the unit price.
    pub paid: Money,
}

/// Which listings to show.
#[derive(Debug, Clone, Default)]
pub struct MarketFilter {
    /// Only this item.
    pub item: Option<String>,
    /// Only this seller.
    pub seller: Option<ActorId>,
    /// Only at or below this unit price.
    pub max_price: Option<Money>,
    /// At most this many rows.
    pub limit: usize,
}

/// The server's economy, if one was configured.
///
/// Global and set once at startup rather than threaded through every call
/// site: it is a single process-wide service, like the stash, and every
/// alternative meant widening five signatures to carry an `Option` that is
/// decided before the first player connects.
static ECONOMY: std::sync::OnceLock<Option<Box<dyn Economy>>> = std::sync::OnceLock::new();

/// Install the economy. Only the first call has any effect.
pub fn install(econ: Option<Box<dyn Economy>>) {
    let _ = ECONOMY.set(econ);
}

/// The installed economy, or `None` when the server has none.
pub fn get() -> Option<&'static dyn Economy> {
    ECONOMY.get()?.as_deref()
}
