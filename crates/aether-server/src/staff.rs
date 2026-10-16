//! Staff accountability: vanish, spectate and per-admin anti-xray control.
//!
//! The *policy* — who may vanish, who may see the vanished, which x-ray level
//! an admin asked for, and how long it lasts — belongs to the native `staff`
//! plugin (`plugins/staff`). This module is the *mechanism* the plugin drives
//! through [`aether_plugin::HostBackend`]: per-player state, the visibility
//! predicate every player-list/entity path consults, the spectate snapshot and
//! the anti-xray override.
//!
//! Keeping the authority here and not in the plugin is deliberate. Another
//! player's client decides whether to spawn a vanished staff member, and the
//! render path decides whether to mask an ore; both are core decisions that
//! cannot wait for a plugin call, so the core holds the answer once the plugin
//! has set it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::players::{PlayerHandle, PosLook, SharedRegistry};
use crate::protocol::{BlockSource, GameMode, ServerEvent};
use crate::session::DemoWorld;

/// A per-player anti-xray override, set by `/spectate xray`.
///
/// `Default` follows the server config and the explicit bypass rules; every
/// other value is an explicit choice for this one admin, so it can turn
/// anti-xray *on* for a spectator who would otherwise bypass it by an explicit
/// grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AntiXrayLevel {
    /// Follow the `[anti_xray]` config and the bypass rules.
    #[default]
    Default,
    /// Show everything (bypass).
    Off,
    /// Force engine mode: only fully buried ores are masked.
    Engine,
    /// Force culling mode.
    Culling,
    /// Force culling mode with this margin, in blocks; a larger margin is a
    /// weaker hide (more sections sent for real).
    Margin(i32),
}

impl AntiXrayLevel {
    /// Parse the argument of `/spectate xray`: `off`, `engine`, `culling` or a
    /// non-negative margin in blocks.
    pub fn parse(s: &str) -> Option<AntiXrayLevel> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0off" => Some(AntiXrayLevel::Off),
            "engine" => Some(AntiXrayLevel::Engine),
            "culling" | "on" => Some(AntiXrayLevel::Culling),
            "default" | "auto" => Some(AntiXrayLevel::Default),
            other => other
                .parse::<i32>()
                .ok()
                .filter(|n| (0..=64).contains(n))
                .map(AntiXrayLevel::Margin),
        }
    }

    /// The argument spelling, for the reply and the journal.
    pub fn as_str(&self) -> String {
        match self {
            AntiXrayLevel::Default => "default".to_string(),
            AntiXrayLevel::Off => "off".to_string(),
            AntiXrayLevel::Engine => "engine".to_string(),
            AntiXrayLevel::Culling => "culling".to_string(),
            AntiXrayLevel::Margin(n) => n.to_string(),
        }
    }
}

/// The exact per-player state saved when an admin enters spectate and restored
/// on `/spectate off`.
#[derive(Clone)]
pub struct SpectateSnapshot {
    /// Where the admin stood, and how they looked.
    pub pos: PosLook,
    /// The game mode to return to.
    pub mode: GameMode,
    /// The whole inventory, cloned.
    pub inventory: crate::inventory::Inventory,
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub xp_total: i32,
    pub effects: crate::game::effects::Effects,
    pub absorption: f32,
    /// Whether the admin was vanished before spectating, so `/spectate off`
    /// restores that rather than always unvanishing.
    pub vanished: bool,
    /// Whether the admin could see the vanished before spectating.
    pub sees_vanished: bool,
    /// The anti-xray override in force before spectating, so `/spectate off`
    /// reverts it (the brief's "reverts when leaving spectate") instead of
    /// leaving it stuck until the next relog.
    pub xray: AntiXrayLevel,
}

/// Per-player staff state, stored on the [`PlayerHandle`].
///
/// The booleans are atomics so the render path (`visible_to`) reads them
/// without taking a lock; only the rare snapshot and anti-xray override use a
/// mutex.
#[derive(Default)]
pub struct StaffState {
    vanished: AtomicBool,
    sees_vanished: AtomicBool,
    spectating: AtomicBool,
    xray: Mutex<AntiXrayLevel>,
    snapshot: Mutex<Option<SpectateSnapshot>>,
}

impl StaffState {
    /// Whether this player is hidden from other players.
    pub fn vanished(&self) -> bool {
        self.vanished.load(Ordering::Relaxed)
    }

    /// Set the vanished flag (the mechanism only; callers resync visibility).
    pub fn set_vanished(&self, vanished: bool) {
        self.vanished.store(vanished, Ordering::Relaxed);
    }

    /// Whether this player may see vanished players.
    pub fn sees_vanished(&self) -> bool {
        self.sees_vanished.load(Ordering::Relaxed)
    }

    /// Set whether this player may see vanished players.
    pub fn set_sees_vanished(&self, see: bool) {
        self.sees_vanished.store(see, Ordering::Relaxed);
    }

    /// Whether this player is in the spectate mode.
    pub fn spectating(&self) -> bool {
        self.spectating.load(Ordering::Relaxed)
    }

    /// Set the spectating flag.
    pub fn set_spectating(&self, spectating: bool) {
        self.spectating.store(spectating, Ordering::Relaxed);
    }

    /// This player's anti-xray override.
    pub fn xray(&self) -> AntiXrayLevel {
        *self.xray.lock().unwrap()
    }

    /// Replace this player's anti-xray override.
    pub fn set_xray(&self, level: AntiXrayLevel) {
        *self.xray.lock().unwrap() = level;
    }

    /// Take the saved spectate snapshot, leaving none behind.
    pub fn take_snapshot(&self) -> Option<SpectateSnapshot> {
        self.snapshot.lock().unwrap().take()
    }

    /// Store a spectate snapshot.
    pub fn put_snapshot(&self, snapshot: SpectateSnapshot) {
        *self.snapshot.lock().unwrap() = Some(snapshot);
    }

    /// Whether a snapshot is waiting to be restored.
    pub fn has_snapshot(&self) -> bool {
        self.snapshot.lock().unwrap().is_some()
    }
}

/// The vanish visibility rule, as a pure function so it is testable without a
/// live [`PlayerHandle`].
///
/// A target that is not vanished is always visible. A vanished target is
/// visible only to a viewer who may see the vanished, or to another vanished
/// player (so the staff team still sees each other).
pub fn can_see(viewer_sees: bool, viewer_vanished: bool, target_vanished: bool) -> bool {
    !target_vanished || viewer_sees || (viewer_vanished && target_vanished)
}

/// Whether `viewer`'s client should know about `target` as a player.
pub fn visible_to(viewer: &PlayerHandle, target: &PlayerHandle) -> bool {
    if viewer.entity_id == target.entity_id {
        return true;
    }
    can_see(
        viewer.staff().sees_vanished(),
        viewer.staff().vanished(),
        target.staff().vanished(),
    )
}

/// Bring `viewer`'s client in line with whether `target` is visible to it.
///
/// Adding a visible player sends the tab entry before the spawn (a 1.8 client
/// drops a spawn for a UUID it has no entry for); hiding one despawns the
/// entity and removes the tab entry. The caller has already decided the
/// transition, so both directions are unconditional here — the paths that call
/// this (vanish toggles) change the answer only once.
fn sync_viewer(viewer: &PlayerHandle, target: &PlayerHandle, world: &dyn BlockSource) {
    if visible_to(viewer, target) {
        viewer.emit(&ServerEvent::TabListAdd(target), world);
        viewer.emit(&ServerEvent::SpawnPlayer(target), world);
        viewer.emit(&crate::game::tick::equipment_event(target), world);
    } else {
        viewer.emit(&ServerEvent::DespawnEntity(target.entity_id), world);
        viewer.emit(&ServerEvent::TabListRemove(target.uuid), world);
    }
}

/// Apply a vanish toggle: update `target`'s flag and resync every other
/// player's view of them.
pub fn set_vanished(
    target: &PlayerHandle,
    vanished: bool,
    registry: &SharedRegistry,
    world: &dyn BlockSource,
) {
    if target.staff().vanished() == vanished {
        return;
    }
    target.staff().set_vanished(vanished);
    for viewer in registry.snapshot() {
        if viewer.entity_id != target.entity_id {
            sync_viewer(&viewer, target, world);
        }
    }
}

/// Apply a change to who may see the vanished: resync every viewer's view of
/// every currently-vanished player.
pub fn set_sees_vanished(
    viewer_handle: &PlayerHandle,
    see: bool,
    registry: &SharedRegistry,
    world: &dyn BlockSource,
) {
    if viewer_handle.staff().sees_vanished() == see {
        return;
    }
    viewer_handle.staff().set_sees_vanished(see);
    for target in registry.snapshot() {
        if target.entity_id != viewer_handle.entity_id && target.staff().vanished() {
            sync_viewer(viewer_handle, &target, world);
        }
    }
}

/// Enter spectate for `handle`: save the exact previous state, switch to
/// spectator, auto-vanish, and (if `target` is given) teleport to them.
pub fn enter_spectate(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    target: Option<&PlayerHandle>,
) -> Result<(), String> {
    if handle.staff().spectating() {
        return Err("already spectating".to_string());
    }
    let snapshot = {
        let pos = handle.pos();
        let st = handle.game();
        SpectateSnapshot {
            pos,
            mode: st.mode,
            inventory: handle.inventory().clone(),
            health: st.health,
            food: st.food,
            saturation: st.saturation,
            xp_total: st.xp_total,
            effects: st.effects.clone(),
            absorption: st.absorption,
            vanished: handle.staff().vanished(),
            sees_vanished: handle.staff().sees_vanished(),
            xray: handle.staff().xray(),
        }
    };
    handle.staff().put_snapshot(snapshot);
    handle.staff().set_spectating(true);

    // Auto-vanish: a spectator must not be a visible player.
    set_vanished(handle, true, registry, world);

    crate::game::interact::set_mode(handle, world, GameMode::Spectator);

    // Optionally move onto the target's position. The camera is not attached:
    // there is no Set Camera packet in this protocol layer yet, so the
    // spectator flies to and watches the player rather than riding their eyes
    // (documented in `docs/KNOWN_ISSUES.md`).
    if let Some(target) = target {
        let pos = target.pos();
        crate::game::commands::teleport(handle, registry, world, pos.x, pos.y, pos.z);
    }
    Ok(())
}

/// Leave spectate: restore the snapshot exactly and drop the override state.
pub fn leave_spectate(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
) -> Result<(), String> {
    if !handle.staff().spectating() {
        return Err("not spectating".to_string());
    }
    let Some(snapshot) = handle.staff().take_snapshot() else {
        return Err("no spectate snapshot to restore".to_string());
    };

    // Restore server state first, then push it to the client.
    {
        let mut st = handle.game();
        st.mode = snapshot.mode;
        st.health = snapshot.health;
        st.food = snapshot.food;
        st.saturation = snapshot.saturation;
        st.xp_total = snapshot.xp_total;
        st.effects = snapshot.effects.clone();
        st.absorption = snapshot.absorption;
        st.dead = false;
        st.fall_peak = None;
    }
    handle.restore_inventory(snapshot.inventory.clone());
    handle.set_pos(snapshot.pos);

    // `after_spawn` sends position, mode, effects, health, experience and the
    // whole inventory, which is exactly the restore the brief asks for.
    crate::session::after_spawn(handle, registry, world);

    handle.staff().set_spectating(false);
    // Restore the vanish and "see" flags the admin had before spectating.
    set_vanished(handle, snapshot.vanished, registry, world);
    set_sees_vanished(handle, snapshot.sees_vanished, registry, world);
    // Revert the per-admin anti-xray override and re-send the columns it had
    // masked, so leaving spectate restores the server's normal masking for
    // this admin rather than leaving them bypassing (or stuck under a forced
    // level) until the next relog.
    set_xray_level(handle, snapshot.xray, world);
    Ok(())
}

/// Set a player's anti-xray override and re-send the columns they currently
/// hold, so the change takes effect at once instead of on the next stream.
pub fn set_xray_level(handle: &PlayerHandle, level: AntiXrayLevel, world: &dyn BlockSource) {
    handle.staff().set_xray(level);
    crate::visibility::resend_held_columns(handle, world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::players::Registry;
    use crate::protocol::codec_for;
    use std::net::{TcpListener, TcpStream};

    /// A throwaway world on a temp store, as the entity tests use.
    fn test_world(seed: u64) -> DemoWorld {
        use aether_api::{FjallStore, World};
        let dir =
            std::env::temp_dir().join(format!("aether-sp1-staff-{}-{seed}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let generator = crate::worldgen::Generator::load("", "", seed).expect("noise generator");
        let revision = generator.revision();
        let cfg = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, seed, revision, &cfg);
        World::new(store, generator)
    }

    /// Register one player on a throwaway socket.
    fn handle(registry: &Registry, eid: i32, name: &str) -> std::sync::Arc<PlayerHandle> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client =
            crate::proto::Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let _peer = listener.accept().unwrap();
        registry
            .join(
                eid,
                0,
                name.into(),
                None,
                Vec::new(),
                codec_for(763).unwrap(),
                &client,
                PosLook {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                },
            )
            .unwrap()
    }

    #[test]
    fn leaving_spectate_reverts_the_anti_xray_override() {
        let world = test_world(31);
        let registry = std::sync::Arc::new(Registry::default());
        let h = handle(&registry, 1, "Steve");
        // Both a bypass and a forced level must revert, not stick until relog.
        for level in [AntiXrayLevel::Off, AntiXrayLevel::Engine] {
            enter_spectate(&h, &registry, &world, None).expect("enter spectate");
            set_xray_level(&h, level, &world);
            assert_eq!(h.staff().xray(), level);
            leave_spectate(&h, &registry, &world).expect("leave spectate");
            assert_eq!(
                h.staff().xray(),
                AntiXrayLevel::Default,
                "the override reverted on leave"
            );
            assert!(!h.staff().spectating());
        }
    }

    #[test]
    fn a_vanished_player_is_not_huntable_and_is_no_spawn_anchor() {
        use crate::game::entities::player_view;
        let registry = std::sync::Arc::new(Registry::default());
        let h = handle(&registry, 1, "Steve");
        // Survival and alive: normally huntable.
        h.game().mode = crate::protocol::GameMode::Survival;
        assert!(player_view(&h).huntable);
        assert!(player_view(&h).survival);
        assert!(!player_view(&h).vanished);
        // Vanished: no longer a mob target and dropped as a spawn anchor, but
        // still `survival` so it collects drops (arrows) as before.
        h.staff().set_vanished(true);
        let view = player_view(&h);
        assert!(view.vanished);
        assert!(!view.huntable, "mobs must not target a vanished player");
        assert!(
            view.survival,
            "a vanished player still picks items up (survival, not huntable)"
        );
        assert!(view.alive, "still alive for proximity, just not a target");
    }

    #[test]
    fn a_vanished_player_is_hidden_unless_seen_or_also_vanished() {
        // Not vanished: always visible.
        assert!(can_see(false, false, false));
        // Vanished, ordinary viewer: hidden.
        assert!(!can_see(false, false, true));
        // Vanished, viewer may see the vanished: shown.
        assert!(can_see(true, false, true));
        // Vanished, viewer is also vanished: the staff team sees each other.
        assert!(can_see(false, true, true));
    }

    #[test]
    fn xray_levels_parse_and_round_trip() {
        assert_eq!(AntiXrayLevel::parse("off"), Some(AntiXrayLevel::Off));
        assert_eq!(AntiXrayLevel::parse("engine"), Some(AntiXrayLevel::Engine));
        assert_eq!(
            AntiXrayLevel::parse("culling"),
            Some(AntiXrayLevel::Culling)
        );
        assert_eq!(
            AntiXrayLevel::parse("default"),
            Some(AntiXrayLevel::Default)
        );
        assert_eq!(AntiXrayLevel::parse("12"), Some(AntiXrayLevel::Margin(12)));
        // Out of range and non-numeric are refused.
        assert_eq!(AntiXrayLevel::parse("999"), None);
        assert_eq!(AntiXrayLevel::parse("nonsense"), None);
        for level in [
            AntiXrayLevel::Default,
            AntiXrayLevel::Off,
            AntiXrayLevel::Engine,
            AntiXrayLevel::Culling,
            AntiXrayLevel::Margin(7),
        ] {
            assert_eq!(AntiXrayLevel::parse(&level.as_str()), Some(level));
        }
    }
}
