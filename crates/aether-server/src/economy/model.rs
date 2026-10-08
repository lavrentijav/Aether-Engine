//! The parts of the economy that are arithmetic rather than storage.
//!
//! Kept separate from the database so the rules — what a valid price is, what
//! a purchase costs, when an offer has expired — are tested without one, and
//! so a second backend cannot quietly disagree with the first about them.

// The rules live here and the storage calls them, so a default build — which
// has no storage backend — compiles and tests them without using them. That is
// deliberate: pricing rules that only type-check under a feature flag are rules
// nobody notices breaking.
#![cfg_attr(not(feature = "postgres"), allow(dead_code))]

use aether_world::journal::ActorId;

/// An amount of money, in minor units (hundredths).
///
/// A newtype over `i64` rather than a float, and signed rather than unsigned:
/// the sign is what lets a bug that would have produced a negative balance
/// show up as a rejected transaction instead of as an enormous positive one
/// from an unsigned wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(pub i64);

impl Money {
    /// Nothing.
    pub const ZERO: Money = Money(0);

    /// Whole coins.
    ///
    /// Only tests construct money this way — everything a player types goes
    /// through [`Money::parse`] — but it stays part of the type because a
    /// caller that needs a literal amount should not have to know that a coin
    /// is a hundred of something.
    #[allow(dead_code)]
    pub fn coins(n: i64) -> Money {
        Money(n.saturating_mul(100))
    }

    /// Parse `"12"`, `"12.5"`, `"12.34"`.
    ///
    /// More than two decimal places is refused rather than rounded: a player
    /// who typed `0.005` meant something, and silently making it `0.01` or
    /// `0.00` is a disagreement they will only discover after the trade.
    pub fn parse(s: &str) -> Option<Money> {
        let s = s.trim();
        if s.is_empty() || s.starts_with('-') {
            return None;
        }
        let (whole, frac) = match s.split_once('.') {
            Some((w, f)) => (w, f),
            None => (s, ""),
        };
        if frac.len() > 2 || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if whole.is_empty() && frac.is_empty() {
            return None;
        }
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
        // "1.5" is fifty, not five: pad to two places rather than parsing the
        // digits as they came.
        let cents: i64 = match frac.len() {
            0 => 0,
            1 => frac.parse::<i64>().ok()? * 10,
            _ => frac.parse().ok()?,
        };
        whole.checked_mul(100)?.checked_add(cents).map(Money)
    }

    /// `amount * count`, refusing to overflow.
    pub fn times(self, count: u64) -> Option<Money> {
        i64::try_from(count)
            .ok()
            .and_then(|n| self.0.checked_mul(n))
            .map(Money)
    }

    /// Whether this is a price someone can actually offer.
    pub fn is_valid_price(self) -> bool {
        self > Money::ZERO
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let v = self.0.unsigned_abs();
        write!(f, "{sign}{}.{:02}", v / 100, v % 100)
    }
}

/// One open offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub id: i64,
    pub seller: ActorId,
    /// Seller's name at the time of listing, for display. Stored rather than
    /// looked up because the seller may be offline when someone browses.
    pub seller_name: String,
    pub item: String,
    /// How many are still on offer.
    pub count: u64,
    /// Price for one.
    pub unit_price: Money,
    /// Unix milliseconds.
    pub created_ms: u64,
    /// Unix milliseconds, or `None` for an offer that does not expire.
    pub expires_ms: Option<u64>,
}

impl Listing {
    /// Total cost of taking `count` from this listing.
    pub fn cost_of(&self, count: u64) -> Option<Money> {
        self.unit_price.times(count)
    }

    /// Whether this offer is still open at `now_ms`.
    pub fn is_open(&self, now_ms: u64) -> bool {
        self.count > 0 && self.expires_ms.is_none_or(|e| e > now_ms)
    }

    /// A one-line description for the market window.
    pub fn describe(&self, now_ms: u64) -> String {
        let short = self.item.split_once(':').map_or(&*self.item, |(_, n)| n);
        let left = match self.expires_ms {
            Some(e) if e > now_ms => format!(" ({} left)", crate::economy::model::until(e - now_ms)),
            Some(_) => " (expired)".to_string(),
            None => String::new(),
        };
        format!(
            "#{} {}x {} @ {} each — {}{}",
            self.id, self.count, short, self.unit_price, self.seller_name, left
        )
    }
}

/// "2h", "13m", "45s" — the coarsest unit that is still informative.
pub fn until(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Why a trade did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeError {
    /// The economy's storage could not be reached or refused the statement.
    ///
    /// Deliberately fatal to the command rather than degraded: see the module
    /// docs on why the economy does not fall back to anything.
    Unavailable(String),
    /// Not enough money.
    Insufficient { need: Money, have: Money },
    /// No listing with that id.
    NoSuchListing(i64),
    /// The listing has been withdrawn, bought out or has expired.
    ListingClosed(i64),
    /// Asked for more than the listing still holds.
    NotEnoughStock { asked: u64, left: u64 },
    /// The listing belongs to somebody else.
    NotYours(i64),
    /// A price of zero or a count of zero.
    BadOffer(&'static str),
    /// Buying your own listing, or paying yourself.
    SelfTrade,
}

impl std::fmt::Display for TradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradeError::Unavailable(e) => write!(f, "the economy is unavailable: {e}"),
            TradeError::Insufficient { need, have } => {
                write!(f, "you need {need} and have {have}")
            }
            TradeError::NoSuchListing(id) => write!(f, "there is no listing #{id}"),
            TradeError::ListingClosed(id) => write!(f, "listing #{id} is closed"),
            TradeError::NotEnoughStock { asked, left } => {
                write!(f, "only {left} left, you asked for {asked}")
            }
            TradeError::NotYours(id) => write!(f, "listing #{id} is not yours"),
            TradeError::BadOffer(why) => write!(f, "bad offer: {why}"),
            TradeError::SelfTrade => write!(f, "you cannot trade with yourself"),
        }
    }
}

impl std::error::Error for TradeError {}

/// Check an offer before it reaches the database.
///
/// Here rather than in SQL so the message a player sees explains what they got
/// wrong, and so the rule is the same whichever backend is in use.
pub fn validate_offer(count: u64, price: Money) -> Result<(), TradeError> {
    if count == 0 {
        return Err(TradeError::BadOffer("nothing to sell"));
    }
    if !price.is_valid_price() {
        return Err(TradeError::BadOffer("the price must be above zero"));
    }
    if price.times(count).is_none() {
        return Err(TradeError::BadOffer("that many at that price overflows"));
    }
    Ok(())
}

/// Check a purchase against a listing and a balance, returning what it costs.
pub fn price_purchase(
    listing: &Listing,
    buyer: ActorId,
    count: u64,
    balance: Money,
    now_ms: u64,
) -> Result<Money, TradeError> {
    if listing.seller == buyer {
        return Err(TradeError::SelfTrade);
    }
    if !listing.is_open(now_ms) {
        return Err(TradeError::ListingClosed(listing.id));
    }
    if count == 0 {
        return Err(TradeError::BadOffer("nothing to buy"));
    }
    if count > listing.count {
        return Err(TradeError::NotEnoughStock {
            asked: count,
            left: listing.count,
        });
    }
    let cost = listing
        .cost_of(count)
        .ok_or(TradeError::BadOffer("that many at that price overflows"))?;
    if balance < cost {
        return Err(TradeError::Insufficient {
            need: cost,
            have: balance,
        });
    }
    Ok(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(count: u64, price: i64, expires: Option<u64>) -> Listing {
        Listing {
            id: 1,
            seller: ActorId(1),
            seller_name: "alice".into(),
            item: "minecraft:diamond".into(),
            count,
            unit_price: Money(price),
            created_ms: 0,
            expires_ms: expires,
        }
    }

    #[test]
    fn money_parses_the_forms_a_player_types() {
        assert_eq!(Money::parse("12"), Some(Money(1200)));
        assert_eq!(Money::parse("12.34"), Some(Money(1234)));
        assert_eq!(Money::parse("0.05"), Some(Money(5)));
        assert_eq!(Money::parse(" 7 "), Some(Money(700)));
        assert_eq!(Money::parse(".5"), Some(Money(50)));
    }

    #[test]
    fn one_decimal_place_is_tenths_and_not_hundredths() {
        // The classic: "1.5" parsed as 1 coin and 5 cents instead of 50.
        // A tenfold pricing error nobody notices until someone exploits it.
        assert_eq!(Money::parse("1.5"), Some(Money(150)));
        assert_eq!(Money::parse("1.05"), Some(Money(105)));
    }

    #[test]
    fn money_refuses_what_it_cannot_represent_exactly() {
        // Rounding here is a disagreement the player only discovers after the
        // trade has happened.
        assert_eq!(Money::parse("0.005"), None);
        assert_eq!(Money::parse("1.234"), None);
        assert_eq!(Money::parse("-5"), None);
        assert_eq!(Money::parse("abc"), None);
        assert_eq!(Money::parse(""), None);
        assert_eq!(Money::parse("1.2x"), None);
    }

    #[test]
    fn money_prints_back_the_way_it_was_typed() {
        for s in ["12.34", "0.05", "1000.00"] {
            assert_eq!(Money::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(Money::coins(3).to_string(), "3.00");
        assert_eq!(Money(-250).to_string(), "-2.50");
    }

    #[test]
    fn multiplying_refuses_to_overflow_rather_than_wrapping() {
        // A wrap here turns an unaffordable purchase into a free one.
        assert_eq!(Money(i64::MAX).times(2), None);
        assert_eq!(Money(100).times(3), Some(Money(300)));
        assert_eq!(Money(100).times(0), Some(Money::ZERO));
    }

    #[test]
    fn an_offer_must_have_stock_and_a_price() {
        assert!(validate_offer(1, Money(1)).is_ok());
        assert_eq!(
            validate_offer(0, Money(100)),
            Err(TradeError::BadOffer("nothing to sell"))
        );
        assert_eq!(
            validate_offer(5, Money::ZERO),
            Err(TradeError::BadOffer("the price must be above zero"))
        );
        assert!(validate_offer(u64::MAX, Money(1000)).is_err(), "overflow");
    }

    #[test]
    fn a_purchase_costs_the_unit_price_times_the_count() {
        let l = listing(10, 250, None);
        assert_eq!(
            price_purchase(&l, ActorId(2), 4, Money(10_000), 0),
            Ok(Money(1000))
        );
    }

    #[test]
    fn a_purchase_is_refused_for_each_reason_separately() {
        let l = listing(10, 250, None);
        assert_eq!(
            price_purchase(&l, ActorId(1), 1, Money(10_000), 0),
            Err(TradeError::SelfTrade)
        );
        assert_eq!(
            price_purchase(&l, ActorId(2), 99, Money(10_000), 0),
            Err(TradeError::NotEnoughStock {
                asked: 99,
                left: 10
            })
        );
        assert_eq!(
            price_purchase(&l, ActorId(2), 4, Money(999), 0),
            Err(TradeError::Insufficient {
                need: Money(1000),
                have: Money(999)
            })
        );
    }

    #[test]
    fn exactly_enough_money_is_enough() {
        // The boundary a `<` instead of a `<=` gets wrong, and which every
        // player hits eventually by spending their whole balance.
        let l = listing(1, 500, None);
        assert_eq!(
            price_purchase(&l, ActorId(2), 1, Money(500), 0),
            Ok(Money(500))
        );
    }

    #[test]
    fn an_expired_listing_is_closed_and_a_future_one_is_not() {
        let l = listing(10, 100, Some(1_000));
        assert!(l.is_open(999));
        assert!(!l.is_open(1_000), "expiry is exclusive at the instant");
        assert_eq!(
            price_purchase(&l, ActorId(2), 1, Money(10_000), 5_000),
            Err(TradeError::ListingClosed(1))
        );
    }

    #[test]
    fn a_sold_out_listing_is_closed_even_before_it_expires() {
        let l = listing(0, 100, None);
        assert!(!l.is_open(0));
    }

    #[test]
    fn a_listing_describes_itself_with_its_time_left() {
        let l = listing(10, 250, Some(7_200_000));
        let s = l.describe(0);
        assert!(s.contains("#1"), "{s}");
        assert!(s.contains("10x diamond"), "{s}");
        assert!(s.contains("2.50"), "{s}");
        assert!(s.contains("alice"), "{s}");
        assert!(s.contains("2h left"), "{s}");
        assert!(!l.describe(9_000_000).contains("left"), "expired says so");
    }
}
