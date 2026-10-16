//! The economy's chat commands.
//!
//! ```text
//! /balance [player]              what you (or they) have
//! /pay <player> <amount>         hand money over
//! /sell <count> <price> [dur]    offer what you are holding
//! /market [filters]              browse open offers
//! /buy <id> [count]              take an offer
//! /listings                      your own offers
//! /unlist <id>                   withdraw one
//! ```
//!
//! Every one of these fails loudly when the database is unreachable, which is
//! the opposite of how the rest of the server treats PostgreSQL and is
//! explained in [`super`]. The failure is visible on purpose: a player told
//! "the economy is unavailable" tries again later, while a player told nothing
//! assumes the trade went through.
//!
//! Selling takes the stack out of the seller's inventory at the moment of
//! listing rather than at the moment of sale. Anything else lets one stack be
//! listed on four offers at once, which is a duplication with extra steps.

use aether_world::journal::{now_ms, ActorId, EventBody, Place};

use super::model::{Money, TradeError};
use super::{Economy, MarketFilter};
use crate::commands::query::parse_duration_ms;
use crate::commands::tree::{self, Ctx, Deny, Node, Requirement};
use crate::commands::Reply;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;

/// Register every economy command on the tree. A command on a server with no
/// economy still routes; it answers that the economy is off, exactly as
/// before.
pub fn nodes() -> Vec<Node> {
    vec![
        tree::with_usage(
            tree::greedy(
                "balance",
                &["bal", "money"],
                Requirement::None,
                exec_balance,
            ),
            "[player]",
        ),
        tree::with_usage(
            tree::greedy("pay", &[], Requirement::None, exec_pay),
            "<player> <amount>",
        ),
        tree::with_usage(
            tree::greedy("sell", &[], Requirement::None, exec_sell),
            "<count> <price each> [duration]",
        ),
        tree::with_usage(
            tree::greedy("market", &[], Requirement::None, exec_market),
            "[filters]",
        ),
        tree::with_usage(
            tree::greedy("buy", &[], Requirement::None, exec_buy),
            "<id> [count]",
        ),
        tree::simple("listings", &[], Requirement::None, exec_listings),
        tree::with_usage(
            tree::greedy("unlist", &[], Requirement::None, exec_unlist),
            "<id>",
        ),
        tree::with_usage(
            tree::greedy("econ", &[], Requirement::Fn(require_econ_op), exec_econ),
            "give <player> <amount> [reason]",
        ),
    ]
}

/// The reply every economy command gives when no economy is installed.
fn disabled() -> Reply {
    Reply(vec![
        "the economy is not enabled on this server.".into(),
        "it needs a [database] url and a build with --features postgres.".into(),
    ])
}

/// The `/econ` gate, enforced by the tree. With no economy installed the
/// disabled reply is what the caller must see, so the gate lets the executor
/// deliver it — the check only bites once an economy exists.
pub fn require_econ_op(handle: &PlayerHandle) -> Result<(), Deny> {
    if crate::economy::get().is_none() {
        return Ok(());
    }
    if crate::is_operator(handle) {
        Ok(())
    } else {
        Err(Deny::Message("that command is for operators".into()))
    }
}

fn exec_balance(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    balance(&args, ctx.handle, ctx.registry, econ)
}

fn exec_pay(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    pay(&args, ctx.handle, ctx.registry, econ)
}

fn exec_sell(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    sell(&args, ctx.handle, ctx.world, econ)
}

fn exec_market(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    market(&args, ctx.handle, ctx.registry, ctx.world, econ)
}

fn exec_buy(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    buy(&args, ctx.handle, ctx.registry, ctx.world, econ)
}

fn exec_listings(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    listings(ctx.handle, econ)
}

fn exec_unlist(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    unlist(&args, ctx.handle, ctx.registry, ctx.world, econ)
}

fn exec_econ(ctx: &mut Ctx) -> Reply {
    let Some(econ) = ctx.econ else {
        return disabled();
    };
    let args = ctx.refs();
    admin(&args, ctx.handle, ctx.registry, econ)
}

/// `/econ give <player> <amount> [reason]` — the only way money enters the
/// economy.
///
/// Without a mint every balance is zero forever and nothing can ever be
/// bought, so this is load-bearing rather than a convenience. It is also the
/// one command that can create value out of nothing, which is why it is
/// gated, and why every use lands in `aether_ledger` with its reason.
fn admin(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    econ: &dyn Economy,
) -> Reply {
    match args {
        ["give", name, amount, rest @ ..] => {
            let Some(to) = registry.uuid_of(name) else {
                return Reply(vec![format!("no player named {name} is online")]);
            };
            let Some(amount) = Money::parse(amount) else {
                return Reply(vec![format!("`{amount}` is not an amount")]);
            };
            let reason = if rest.is_empty() {
                format!("granted by {}", handle.name)
            } else {
                rest.join(" ")
            };
            match econ.mint(ActorId(to), amount, &reason) {
                Ok(()) => {
                    if let Some(other) = registry.by_uuid(to) {
                        other.emit(
                            &crate::protocol::ServerEvent::Chat(format!(
                                "you were granted {amount}"
                            )),
                            &NoBlocks,
                        );
                    }
                    Reply(vec![format!("granted {amount} to {name} ({reason})")])
                }
                Err(e) => err(e),
            }
        }
        _ => Reply(vec!["usage: /econ give <player> <amount> [reason]".into()]),
    }
}

fn err(e: TradeError) -> Reply {
    Reply(vec![e.to_string()])
}

fn balance(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    econ: &dyn Economy,
) -> Reply {
    let (who, label) = match args.first() {
        None => (ActorId(handle.uuid), "you have".to_string()),
        Some(name) => match registry.uuid_of(name) {
            Some(u) => (ActorId(u), format!("{name} has")),
            None => return Reply(vec![format!("no player named {name} is online")]),
        },
    };
    match econ.balance(who) {
        Ok(b) => Reply(vec![format!("{label} {b}")]),
        Err(e) => err(e),
    }
}

fn pay(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    econ: &dyn Economy,
) -> Reply {
    let [name, amount] = args else {
        return Reply(vec!["usage: /pay <player> <amount>".into()]);
    };
    let Some(to) = registry.uuid_of(name) else {
        return Reply(vec![format!("no player named {name} is online")]);
    };
    let Some(amount) = Money::parse(amount) else {
        return Reply(vec![format!("`{amount}` is not an amount")]);
    };
    match econ.pay(ActorId(handle.uuid), ActorId(to), amount) {
        Ok(()) => {
            // Telling the recipient is not a nicety: a payment they cannot see
            // is one they will ask about, and the answer is in a database they
            // cannot query.
            if let Some(other) = registry.by_uuid(to) {
                other.emit(
                    &crate::protocol::ServerEvent::Chat(format!(
                        "{} paid you {amount}",
                        handle.name
                    )),
                    &NoBlocks,
                );
            }
            Reply(vec![format!("paid {amount} to {name}")])
        }
        Err(e) => err(e),
    }
}

/// A block source for events that name no block.
///
/// [`PlayerHandle::emit`] takes one because chunk packets need it; a chat line
/// does not, and threading the world through every notification for the sake
/// of a parameter nobody reads would be worse.
struct NoBlocks;
impl crate::protocol::BlockSource for NoBlocks {
    fn block_at(&self, _x: i32, _y: i32, _z: i32) -> aether_world::BlockStateId {
        aether_world::BlockStateId::AIR
    }
}

fn sell(args: &[&str], handle: &PlayerHandle, world: &DemoWorld, econ: &dyn Economy) -> Reply {
    let (count, price, duration) = match args {
        [c, p] => (c, p, None),
        [c, p, d] => (c, p, Some(*d)),
        _ => {
            return Reply(vec![
                "usage: /sell <count> <price each> [duration]".into(),
                "sells what you are holding, e.g. /sell 64 2.50 2h".into(),
            ])
        }
    };
    let Ok(count) = count.parse::<u64>() else {
        return Reply(vec![format!("`{count}` is not a count")]);
    };
    let Some(price) = Money::parse(price) else {
        return Reply(vec![format!("`{price}` is not a price")]);
    };
    let expires = match duration {
        None => None,
        Some(d) => match parse_duration_ms(d) {
            Some(ms) => Some(ms / 1000),
            None => return Reply(vec![format!("`{d}` is not a duration")]),
        },
    };
    let Some(item) = handle.held_item() else {
        return Reply(vec!["you are not holding anything".into()]);
    };
    let item_name = world
        .item_name(item)
        .unwrap_or_else(|| format!("item#{}", item.0));

    // Checked before anything moves, so a short seller gets a message rather
    // than a partly-emptied inventory that has to be put back.
    let have = handle.inventory().count_of(item);
    if have < count {
        return Reply(vec![format!("you have {have} of {item_name}, not {count}")]);
    }
    // Taken from the inventory *before* the listing exists. The other order
    // leaves a window in which the same stack backs two offers.
    handle.inventory().take(item, count);
    handle.sync_inventory(world);

    match econ.list(ActorId(handle.uuid), item, count, price, expires) {
        Ok(id) => {
            journal_move(
                world,
                ActorId(handle.uuid),
                count,
                Place::Inventory {
                    owner: ActorId(handle.uuid),
                    slot: -1,
                },
                Place::Nowhere,
            );
            Reply(vec![format!(
                "listed {count}x {item_name} at {price} each — #{id}"
            )])
        }
        Err(e) => {
            // The listing failed, so the stack goes back. Without this a
            // database hiccup eats the seller's items.
            handle
                .inventory()
                .give(item, count.min(u8::MAX as u64) as u8);
            handle.sync_inventory(world);
            err(e)
        }
    }
}

/// Record an item leaving or arriving somewhere, so the ledger keeps up with
/// trades as well as with slot edits.
fn journal_move(world: &DemoWorld, actor: ActorId, count: u64, from: Place, to: Place) {
    let _ = world.journal().append(
        actor,
        EventBody::ItemMove {
            // A trade moves a *quantity*, not one tracked stack: the seller may
            // have taken it from several slots. A fresh uid marks the parcel
            // that moved, and the count is what an audit reads.
            uid: aether_world::journal::ledger::mint_uid(),
            from,
            to,
            count: count.min(u8::MAX as u64) as u8,
        },
    );
}

fn market(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    econ: &dyn Economy,
) -> Reply {
    let mut filter = MarketFilter {
        limit: 200,
        ..Default::default()
    };
    for t in args {
        let Some((key, value)) = t.split_once(':') else {
            return Reply(vec![format!("`{t}` is not a filter — try item:diamond")]);
        };
        match key {
            "item" | "i" => {
                let name = if value.contains(':') {
                    value.to_string()
                } else {
                    format!("minecraft:{value}")
                };
                match world.item_id(&name) {
                    Some(id) => filter.item = Some(id),
                    None => return Reply(vec![format!("unknown item `{name}`")]),
                }
            }
            "seller" | "s" => match registry.uuid_of(value) {
                Some(u) => filter.seller = Some(ActorId(u)),
                None => return Reply(vec![format!("no player named {value} is online")]),
            },
            "max" | "under" => match Money::parse(value) {
                Some(p) => filter.max_price = Some(p),
                None => return Reply(vec![format!("`{value}` is not a price")]),
            },
            _ => {
                return Reply(vec![format!(
                    "unknown filter `{key}` — try item, seller, max"
                )])
            }
        }
    }
    match econ.market(&filter) {
        Ok(rows) => {
            let now = now_ms();
            let lines: Vec<String> = rows
                .iter()
                .map(|l| format!("  {}", l.describe(now)))
                .collect();
            let mut s = handle.session();
            s.pager.set("Market", lines);
            Reply(s.pager.render())
        }
        Err(e) => err(e),
    }
}

fn buy(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    econ: &dyn Economy,
) -> Reply {
    let (id, count) = match args {
        [id] => (id, None),
        [id, n] => (id, Some(*n)),
        _ => return Reply(vec!["usage: /buy <id> [count]".into()]),
    };
    let Ok(id) = id.parse::<i64>() else {
        return Reply(vec![format!("`{id}` is not a listing id")]);
    };
    let count = match count {
        None => 1,
        Some(n) => match n.parse::<u64>() {
            Ok(n) => n,
            Err(_) => return Reply(vec![format!("`{n}` is not a count")]),
        },
    };
    match econ.buy(ActorId(handle.uuid), id, count) {
        Ok(p) => {
            let me = ActorId(handle.uuid);
            // The database has already moved the money, so the goods cannot be
            // refused here — a full inventory would leave the buyer poorer
            // with nothing to show. Whatever will not fit falls at their feet
            // and can be picked up once they make room. This is why the seam
            // between the committed transaction and the inventory is
            // survivable rather than merely reported.
            let (_, dropped) =
                crate::ground::give_or_drop(handle, registry, world, p.item, p.count);
            journal_move(
                world,
                me,
                p.count,
                Place::Nowhere,
                Place::Inventory {
                    owner: me,
                    slot: -1,
                },
            );
            let name = world
                .item_name(p.item)
                .unwrap_or_else(|| format!("item#{}", p.item.0));
            let mut lines = vec![format!("bought {}x {} for {}", p.count, name, p.paid)];
            if dropped > 0 {
                lines.push(format!("{dropped} would not fit and fell at your feet"));
            }
            Reply(lines)
        }
        Err(e) => err(e),
    }
}

fn listings(handle: &PlayerHandle, econ: &dyn Economy) -> Reply {
    match econ.listings_of(ActorId(handle.uuid)) {
        Ok(rows) if rows.is_empty() => Reply(vec!["you have no open listings".into()]),
        Ok(rows) => {
            let now = now_ms();
            let lines: Vec<String> = rows
                .iter()
                .map(|l| format!("  {}", l.describe(now)))
                .collect();
            let mut s = handle.session();
            s.pager.set("Your listings", lines);
            Reply(s.pager.render())
        }
        Err(e) => err(e),
    }
}

fn unlist(
    args: &[&str],
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    econ: &dyn Economy,
) -> Reply {
    let [id] = args else {
        return Reply(vec!["usage: /unlist <id>".into()]);
    };
    let Ok(id) = id.parse::<i64>() else {
        return Reply(vec![format!("`{id}` is not a listing id")]);
    };
    match econ.cancel(ActorId(handle.uuid), id) {
        Ok(l) => {
            let me = ActorId(handle.uuid);
            let (_, dropped) =
                crate::ground::give_or_drop(handle, registry, world, l.item, l.count);
            journal_move(
                world,
                me,
                l.count,
                Place::Nowhere,
                Place::Inventory {
                    owner: me,
                    slot: -1,
                },
            );
            let name = world
                .item_name(l.item)
                .unwrap_or_else(|| format!("item#{}", l.item.0));
            let mut lines = vec![format!("withdrew #{id}: {}x {}", l.count, name)];
            if dropped > 0 {
                lines.push(format!("{dropped} would not fit and fell at your feet"));
            }
            Reply(lines)
        }
        Err(e) => err(e),
    }
}
