//! The staff plugin: vanish, admin spectate and per-admin anti-xray control.
//!
//! A native plugin (tier N, per `docs/design/plugins.md`) on the host API. It
//! owns the *policy* — who toggles, what a staff member may do, which x-ray
//! level, and how long a vanish survives a relog — and drives the *mechanism*
//! through the `staff_*` host hooks, which the core owns because another
//! player's client and the render path need the answer without a plugin call
//! (see `crates/aether-server/src/staff.rs`).
//!
//! ```text
//! /vanish [player]                 toggle vanish (self, or a target)
//! /spectate [player]               spectate (optionally on a player)
//! /spectate off                    restore the exact previous state
//! /spectate xray off|engine|culling|<level>   per-admin anti-xray control
//! ```
//!
//! Permission nodes `aether.staff.vanish`, `aether.staff.vanish.see` and
//! `aether.staff.spectate` are declared now; until the core permission store
//! (CMD-01) lands the gate is the operator flag, the same stand-in the core's
//! own commands use.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{LogLevel, NativeContext, NativePlugin, NativeRegistration, Player};

/// Permission node for `/vanish`.
pub const NODE_VANISH: &str = "aether.staff.vanish";
/// Permission node that lets a player see summoned/vanish-hidden staff.
pub const NODE_VANISH_SEE: &str = "aether.staff.vanish.see";
/// Permission node for `/spectate`.
pub const NODE_SPECTATE: &str = "aether.staff.spectate";

/// The plugin manifest. It subscribes to the tick (the vanish indicator) and
/// to join/leave (persistence across a relog). No hard service dependency, so
/// it loads with the SDK disabled.
pub fn manifest() -> Manifest {
    Manifest {
        id: "staff".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether Staff".to_string(),
        priority: 0,
        capabilities: Vec::new(),
        subscribe: vec![
            "on-tick".to_string(),
            "after-player-join".to_string(),
            "after-player-leave".to_string(),
        ],
        commands: vec![
            CommandDecl::Detailed {
                name: "vanish".to_string(),
                node: NODE_VANISH.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: "spectate".to_string(),
                node: NODE_SPECTATE.to_string(),
                op: 0,
            },
        ],
        tick_period: 20,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server adds.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(StaffPlugin::default()),
    }
}

/// The persistence key holding the vanished names, one per line.
const KV_VANISHED: &str = "vanished";

/// State shared between the plugin and its tick.
#[derive(Default)]
struct State {
    /// Names of vanished players, for persistence across a relog.
    vanished_names: Mutex<HashSet<String>>,
    /// Entity ids of currently vanished players, for the indicator tick.
    vanished_ids: Mutex<HashSet<u64>>,
    /// Whether a vanish survives a relog (`persist` config, default true).
    persist: bool,
}

/// The staff plugin.
#[derive(Default)]
pub struct StaffPlugin {
    state: Arc<State>,
}

impl NativePlugin for StaffPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Register the commands in the core registry. They go through the
        // core's single gate, so they are dispatched and permission-checked
        // exactly like a core command; `spectate` is not a core name (the
        // staff plugin owns it), `vanish` is new.
        ctx.backend
            .register_command(ctx.id, "vanish", NODE_VANISH, 0)?;
        ctx.backend
            .register_command(ctx.id, "spectate", NODE_SPECTATE, 0)?;

        // `persist = false` clears vanish on logout.
        let persist = ctx
            .config
            .get("persist")
            .map(|v| !matches!(v.as_str(), "false" | "0" | "no" | "off"))
            .unwrap_or(true);
        let mut fresh = State {
            persist,
            ..State::default()
        };
        if let Some(bytes) = ctx.backend.kv_get(ctx.id, KV_VANISHED) {
            if let Ok(text) = String::from_utf8(bytes) {
                fresh.vanished_names = Mutex::new(
                    text.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(|l| l.to_string())
                        .collect(),
                );
            }
        }
        self.state = Arc::new(fresh);

        ctx.log(
            LogLevel::Info,
            &format!(
                "staff ready: {} persisted vanish(es), persist={}",
                self.state.vanished_names.lock().unwrap().len(),
                self.state.persist
            ),
        );
        Ok(())
    }

    fn after_player_join(&mut self, ctx: &mut NativeContext, p: &Player) {
        // A returning name that was vanished comes back vanished.
        let was_vanished = self.state.vanished_names.lock().unwrap().contains(&p.name);
        if was_vanished {
            ctx.backend.staff_set_vanished(ctx.id, p.id, true);
            self.state.vanished_ids.lock().unwrap().insert(p.id);
        }
        // An operator may see the vanished (the stand-in for
        // `aether.staff.vanish.see`).
        ctx.backend
            .staff_set_sees_vanished(ctx.id, p.id, p.operator);
    }

    fn after_player_leave(&mut self, ctx: &mut NativeContext, p: &Player) {
        self.state.vanished_ids.lock().unwrap().remove(&p.id);
        // A non-persistent vanish is dropped on logout; the name is kept only
        // while `persist` is on.
        if !self.state.persist && self.state.vanished_names.lock().unwrap().remove(&p.name) {
            self.save_vanished(ctx);
        }
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, _n: u64) {
        // The indicator: a vanished staff member sees a status line above the
        // hotbar, so they cannot forget they are hidden.
        let vanished = self.state.vanished_ids.lock().unwrap().clone();
        if vanished.is_empty() {
            return;
        }
        for p in ctx.backend.online_players(ctx.id) {
            if !vanished.contains(&p.id) {
                continue;
            }
            let spectating = ctx.backend.staff_is_spectating(ctx.id, p.id);
            let text = if spectating {
                "Spectating — vanished"
            } else {
                "Vanished"
            };
            ctx.backend.staff_actionbar(ctx.id, p.id, text);
        }
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<aether_plugin::CommandResult> {
        match name {
            "vanish" => Some(aether_plugin::CommandResult {
                handled: true,
                lines: self.vanish(ctx, p, args),
            }),
            "spectate" => Some(aether_plugin::CommandResult {
                handled: true,
                lines: self.spectate(ctx, p, args),
            }),
            _ => None,
        }
    }
}

impl StaffPlugin {
    /// `/vanish [player]`.
    fn vanish(&self, ctx: &mut NativeContext, caller: &Player, args: &[String]) -> Vec<String> {
        if !caller.operator {
            return vec!["You do not have permission to vanish.".to_string()];
        }
        let target = match args.first() {
            Some(name) => match self.find(ctx, name) {
                Some(t) => t,
                None => return vec![format!("{name} is not online")],
            },
            None => caller.clone(),
        };
        let now = !ctx.backend.staff_is_vanished(ctx.id, target.id);
        ctx.backend.staff_set_vanished(ctx.id, target.id, now);
        {
            let mut names = self.state.vanished_names.lock().unwrap();
            if now {
                names.insert(target.name.clone());
            } else {
                names.remove(&target.name);
            }
        }
        {
            let mut ids = self.state.vanished_ids.lock().unwrap();
            if now {
                ids.insert(target.id);
            } else {
                ids.remove(&target.id);
            }
        }
        self.save_vanished(ctx);
        self.journal(
            ctx,
            caller,
            if now { "vanish.on" } else { "vanish.off" },
            &target.name,
        );
        let who = if target.id == caller.id {
            "You are".to_string()
        } else {
            format!("{} is", target.name)
        };
        vec![format!(
            "{who} now {}.",
            if now { "vanished" } else { "visible" }
        )]
    }

    /// `/spectate [off|player|xray <level>]`.
    fn spectate(&self, ctx: &mut NativeContext, caller: &Player, args: &[String]) -> Vec<String> {
        if !caller.operator {
            return vec!["You do not have permission to spectate.".to_string()];
        }
        match args.first().map(String::as_str) {
            Some("xray") => self.set_xray(ctx, caller, args.get(1).map(String::as_str)),
            Some("off") => match ctx.backend.staff_leave_spectate(ctx.id, caller.id) {
                Ok(()) => {
                    // The core restored the pre-spectate vanish flag, so the
                    // plugin's tracked set must follow it (else the indicator
                    // would lie and a relog would wrongly re-vanish).
                    self.sync_vanish_from_core(ctx, caller);
                    self.journal(ctx, caller, "spectate.off", "");
                    vec!["Spectate off; your previous state was restored.".to_string()]
                }
                Err(e) => vec![format!("Cannot leave spectate: {e}")],
            },
            Some(name) => {
                let Some(target) = self.find(ctx, name) else {
                    return vec![format!("{name} is not online")];
                };
                let target_id = target.id;
                match ctx
                    .backend
                    .staff_enter_spectate(ctx.id, caller.id, Some(target_id))
                {
                    Ok(()) => {
                        self.note_auto_vanish(ctx, caller);
                        self.journal(ctx, caller, "spectate.enter", name);
                        vec![format!("Spectating {name}. `/spectate off` restores you.")]
                    }
                    Err(e) => vec![format!("Cannot spectate: {e}")],
                }
            }
            None => match ctx.backend.staff_enter_spectate(ctx.id, caller.id, None) {
                Ok(()) => {
                    self.note_auto_vanish(ctx, caller);
                    self.journal(ctx, caller, "spectate.enter", "");
                    vec!["Spectating. `/spectate off` restores you.".to_string()]
                }
                Err(e) => vec![format!("Cannot spectate: {e}")],
            },
        }
    }

    /// `/spectate xray <level>`, only while spectating.
    fn set_xray(
        &self,
        ctx: &mut NativeContext,
        caller: &Player,
        level: Option<&str>,
    ) -> Vec<String> {
        let Some(level) = level else {
            return vec!["/spectate xray off|engine|culling|<level>".to_string()];
        };
        if !ctx.backend.staff_is_spectating(ctx.id, caller.id) {
            return vec!["Anti-xray control is only available while spectating.".to_string()];
        }
        match ctx.backend.staff_set_xray_level(ctx.id, caller.id, level) {
            Ok(()) => {
                self.journal(ctx, caller, "xray", level);
                vec![format!("Anti-xray level set to {level}.")]
            }
            Err(e) => vec![format!("Cannot set anti-xray level: {e}")],
        }
    }

    /// Record the auto-vanish that [`NativePlugin::on_command`]'s spectate
    /// entry performs in the core, so the indicator and persistence track it.
    fn note_auto_vanish(&self, ctx: &mut NativeContext, caller: &Player) {
        self.state
            .vanished_names
            .lock()
            .unwrap()
            .insert(caller.name.clone());
        self.state.vanished_ids.lock().unwrap().insert(caller.id);
        self.save_vanished(ctx);
    }

    /// Re-read the core's vanish flag for `caller` and update the plugin's
    /// tracked set to match (used after `/spectate off`, where the core
    /// restores the pre-spectate value).
    fn sync_vanish_from_core(&self, ctx: &NativeContext, caller: &Player) {
        let vanished = ctx.backend.staff_is_vanished(ctx.id, caller.id);
        {
            let mut names = self.state.vanished_names.lock().unwrap();
            if vanished {
                names.insert(caller.name.clone());
            } else {
                names.remove(&caller.name);
            }
        }
        {
            let mut ids = self.state.vanished_ids.lock().unwrap();
            if vanished {
                ids.insert(caller.id);
            } else {
                ids.remove(&caller.id);
            }
        }
        self.save_vanished(ctx);
    }

    /// Find an online player by name, case-insensitively.
    fn find(&self, ctx: &NativeContext, name: &str) -> Option<Player> {
        ctx.backend
            .online_players(ctx.id)
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Append a staff-accountability event to the world journal.
    fn journal(&self, ctx: &NativeContext, caller: &Player, action: &str, detail: &str) {
        ctx.backend.staff_journal(ctx.id, caller.id, action, detail);
    }

    /// Persist the vanished-name set to the plugin KV.
    fn save_vanished(&self, ctx: &NativeContext) {
        let mut names: Vec<String> = self
            .state
            .vanished_names
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        names.sort();
        ctx.backend
            .kv_put(ctx.id, KV_VANISHED, names.join("\n").as_bytes());
    }
}
