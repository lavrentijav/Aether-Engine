//! Behaviour-preserving stand-ins (task J4).
//!
//! A substitute is not only a different picture. Some items *do* something,
//! and a client on an older version is handed a stand-in that does something
//! else: a client without spears sees a **trident**, whose release throws a
//! trident, and a client without wind charges sees a **snowball**, whose use
//! throws a snowball. The picture is the stand-in; the *action* must be the
//! real item's, decided by what the server holds, never by what the client
//! was shown.
//!
//! [`Kind`] classifies an engine item by what it does. The item layer
//! ([`super::items`]) decides what the client *sees*; this decides what the
//! server *does* when the client acts on its stand-in. [`Outcome`] is the
//! answer for one client input: which action to run, and whether the client
//! has to be corrected (a spear's release makes the client predict a thrown
//! trident that must be cancelled by resending the slot).
//!
//! The stand-in *choice* lives in the generated rules
//! (`tools/gen/compat/items.json`): `*_spear` → `trident` where the trident
//! exists, and — because the rule chains — `*_sword` on 1.8.9, which predates
//! the trident; `mace` → `iron_axe`; `wind_charge` → `snowball`.

// `Input`/`Action` are the general table the queue asks for; the mace and
// wind-charge users are declarative until their own mechanics land, so the
// unused variants stay rather than being deleted and re-added.
#![allow(dead_code)]

use aether_world::registry::items::ItemTypeId;

/// What a real engine item does that a plain item does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No special action: the ordinary melee and use rules apply.
    Plain,
    /// A 1.21.11 spear: left click jabs, hold right click charges and lunges.
    Spear,
    /// A mace: a swing while falling smashes for extra damage.
    Mace,
    /// A wind charge: use throws a wind-charge projectile.
    WindCharge,
}

/// A client input that could mean an item action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Left click — attack.
    Swing,
    /// The right-click press (start using).
    UseStart,
    /// The right-click release.
    UseRelease,
}

/// What the server should run for an item on an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The item's ordinary behaviour (a plain sword's swing, a bow's draw).
    Vanilla,
    /// A spear's short thrust, on a left click.
    SpearJab,
    /// A spear's charged lunge, on the release of a held right click.
    SpearCharge,
    /// A mace's falling smash.
    MaceSmash,
    /// Throw a wind-charge projectile.
    WindChargeThrow,
    /// Nothing this input does for this item.
    None,
}

/// The result of routing one client input on the real item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// The action the server runs.
    pub action: Action,
    /// Whether the client has to be corrected afterwards: it predicted a
    /// projectile or an animation for the stand-in that the real action did
    /// not perform, so the slot is resent. A spear's release predicts a
    /// trident throw; resending the slot cancels it and keeps the spear in
    /// hand.
    pub resend_slot: bool,
}

impl Outcome {
    /// The ordinary outcome: run the item's own behaviour, correct nothing.
    const VANILLA: Outcome = Outcome {
        action: Action::Vanilla,
        resend_slot: false,
    };
}

/// Classify an engine item by what it does.
pub fn kind(item: ItemTypeId) -> Kind {
    let Some(name) = item.vanilla_name() else {
        return Kind::Plain;
    };
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    if bare.ends_with("_spear") {
        Kind::Spear
    } else if bare == "mace" {
        Kind::Mace
    } else if bare == "wind_charge" {
        Kind::WindCharge
    } else {
        Kind::Plain
    }
}

/// Whether `item` is one of the 1.21.11 spears.
pub fn is_spear(item: ItemTypeId) -> bool {
    kind(item) == Kind::Spear
}

/// The real action a client input runs on the real item.
///
/// A spear jabs on a swing, and both the press and the release of the right
/// button run its charge — the release is the lunge itself, never a trident
/// throw. A wind charge throws on use. A mace's smash is resolved by the
/// combat code from the player's fall, so its swing is an ordinary attack
/// here and only the fact of a smash is named.
pub fn action(kind: Kind, input: Input) -> Action {
    match (kind, input) {
        (Kind::Spear, Input::Swing) => Action::SpearJab,
        (Kind::Spear, Input::UseStart) => Action::SpearCharge,
        (Kind::Spear, Input::UseRelease) => Action::SpearCharge,
        (Kind::Mace, Input::Swing) => Action::MaceSmash,
        (Kind::WindCharge, Input::UseStart) => Action::WindChargeThrow,
        _ => Action::Vanilla,
    }
}

/// Route one client input on the real item to an [`Outcome`].
pub fn route(item: ItemTypeId, input: Input) -> Outcome {
    let action = action(kind(item), input);
    // The only action whose client prediction differs from what the server
    // does: a spear's release looks like a thrown trident to the client, so
    // the slot must be resent to cancel it. Everything else either matches
    // the client's expectation or has no prediction at all.
    Outcome {
        action,
        resend_slot: action == Action::SpearCharge,
    }
}

/// Whether releasing `item` must never spawn the stand-in's projectile.
///
/// A spear's client predicts a trident throw on release; running a projectile
/// here is exactly the bug this table exists to prevent.
pub fn swallows_release(item: ItemTypeId) -> bool {
    matches!(kind(item), Kind::Spear)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: &str) -> ItemTypeId {
        ItemTypeId::vanilla(n).unwrap_or_else(|| panic!("no {n}"))
    }

    #[test]
    fn the_spears_are_classified() {
        for n in [
            "minecraft:wooden_spear",
            "minecraft:stone_spear",
            "minecraft:copper_spear",
            "minecraft:iron_spear",
            "minecraft:diamond_spear",
            "minecraft:golden_spear",
            "minecraft:netherite_spear",
        ] {
            assert_eq!(kind(id(n)), Kind::Spear, "{n}");
        }
        // A trident is not a spear: it has its own throw, and must keep it.
        assert_eq!(kind(id("minecraft:trident")), Kind::Plain);
        assert_eq!(kind(id("minecraft:iron_sword")), Kind::Plain);
    }

    #[test]
    fn a_spear_jabs_charges_and_never_throws() {
        assert_eq!(action(Kind::Spear, Input::Swing), Action::SpearJab);
        assert_eq!(action(Kind::Spear, Input::UseStart), Action::SpearCharge);
        assert_eq!(action(Kind::Spear, Input::UseRelease), Action::SpearCharge);
        assert!(swallows_release(id("minecraft:iron_spear")));
        assert!(!swallows_release(id("minecraft:trident")));
    }

    #[test]
    fn a_disguised_spear_release_resends_the_slot() {
        // The v767 client sees a trident and predicts a throw on release; the
        // real item is a spear, so the server runs a charge and corrects the
        // client by resending the slot.
        let out = route(id("minecraft:iron_spear"), Input::UseRelease);
        assert_eq!(out.action, Action::SpearCharge);
        assert!(out.resend_slot, "the predicted trident throw is cancelled");
        // A plain item changes nothing.
        let plain = route(id("minecraft:diamond_sword"), Input::UseRelease);
        assert_eq!(plain, Outcome::VANILLA);
    }

    #[test]
    fn the_second_and_third_users_are_classified_too() {
        assert_eq!(kind(id("minecraft:mace")), Kind::Mace);
        assert_eq!(action(Kind::Mace, Input::Swing), Action::MaceSmash);
        assert_eq!(kind(id("minecraft:wind_charge")), Kind::WindCharge);
        assert_eq!(
            action(Kind::WindCharge, Input::UseStart),
            Action::WindChargeThrow
        );
        assert_eq!(action(Kind::Plain, Input::Swing), Action::Vanilla);
    }
}
