//! The Aether **claims** plugin: GriefPrevention-style player-owned land.
//!
//! A native plugin (tier N, per `docs/design/plugins.md`) on top of the core
//! region index and protection check (CMD-29a/29b). The core answers "may this
//! block be broken/placed/used here?"; this plugin owns the *policy* — which
//! boxes a player owns, the claim limits and the commands that create and give
//! them up.
//!
//! ```text
//! /claim                    create a claim in the chunk you stand in, or inspect the claim there
//! /claim <radius>           create a square claim around you
//! /abandonclaim             give up the claim you stand in
//! /abandonallclaims         give up every claim (run twice to confirm)
//! /claimslist [player]      list your claims (a moderator may name another player)
//! golden shovel             right-click corner A, then corner B; inspecting an existing claim
//!                           with it just reports the owner
//! ```
//!
//! Claims are full-height columns (the core region's `-64..=383` band), stored
//! one compact JSON record per claim in the plugin KV. On startup every claim is
//! replayed into the core region store, so protection survives a restart. The
//! protection itself is the core's: a claim is written as a region whose owner
//! is a member, with the configured priority (default 10, above a WorldGuard
//! region's default 0). Overlapping a WorldGuard region is therefore allowed
//! and decided by priority; overlapping another **claim** is refused.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player, RegionInfo,
};
use serde::{Deserialize, Serialize};

/// `/claim`.
pub const COMMAND_CLAIM: &str = "claim";
/// `/abandonclaim`.
pub const COMMAND_ABANDON: &str = "abandonclaim";
/// `/abandonallclaims`.
pub const COMMAND_ABANDON_ALL: &str = "abandonallclaims";
/// `/claimslist`.
pub const COMMAND_LIST: &str = "claimslist";

/// Permission for creating and inspecting a claim.
pub const NODE_CLAIM: &str = "griefprevention.claim";
/// Permission for `/abandonclaim`.
pub const NODE_ABANDON: &str = "griefprevention.abandonclaim";
/// Permission for `/abandonallclaims`.
pub const NODE_ABANDON_ALL: &str = "griefprevention.abandonallclaims";
/// Permission for `/claimslist`.
pub const NODE_LIST: &str = "griefprevention.claimslist";
/// Permission to list another player's claims.
pub const NODE_LIST_OTHERS: &str = "griefprevention.claimslist.others";
/// Moderator node that also allows the cross-player listing.
pub const NODE_ADMIN: &str = "griefprevention.admin";

/// The plugin id; it is also the region source stamped by the host.
pub const PLUGIN_ID: &str = "claims";

const GOLDEN_SHOVEL: &str = "minecraft:golden_shovel";
const KEY_PREFIX: &str = "claim:";
const KEY_NEXT_ID: &str = "next-id";
const CONFIRM_WINDOW: Duration = Duration::from_secs(30);

const WORLD_MIN_Y: i32 = -64;
const WORLD_MAX_Y: i32 = 383;

/// The plugin manifest. The protection events are not listed: the core already
/// checks the region index for every break/place/use, and adding a second check
/// here would only duplicate it. Only the block right-click (the golden shovel)
/// and commands need this plugin.
pub fn manifest() -> Manifest {
    Manifest {
        id: PLUGIN_ID.to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether Claims".to_string(),
        priority: 20,
        capabilities: vec![
            "log".to_string(),
            "config".to_string(),
            "supports".to_string(),
            "storage.kv".to_string(),
            "world.read".to_string(),
            "regions".to_string(),
        ],
        subscribe: vec!["before-use-block".to_string(), "on-command".to_string()],
        commands: vec![
            CommandDecl::Detailed {
                name: COMMAND_CLAIM.to_string(),
                node: NODE_CLAIM.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_ABANDON.to_string(),
                node: NODE_ABANDON.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_ABANDON_ALL.to_string(),
                node: NODE_ABANDON_ALL.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_LIST.to_string(),
                node: NODE_LIST.to_string(),
                op: 0,
            },
        ],
        tick_period: 1,
        deny_on_fault: true,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server collects and links in.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(ClaimsPlugin::default()),
    }
}

/// Operator-tunable limits, read from `[plugins.claims] config`.
#[derive(Clone, Debug)]
struct Settings {
    max_claims: usize,
    max_claim_size: i64,
    max_radius: i32,
    priority: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_claims: 10,
            max_claim_size: 10_000,
            max_radius: 64,
            priority: 10,
        }
    }
}

/// One persisted claim.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Claim {
    id: u64,
    owner: String,
    world: String,
    min_x: i32,
    min_z: i32,
    max_x: i32,
    max_z: i32,
}

impl Claim {
    fn area(&self) -> i64 {
        (i64::from(self.max_x) - i64::from(self.min_x) + 1)
            * (i64::from(self.max_z) - i64::from(self.min_z) + 1)
    }

    fn contains(&self, world: &str, x: i32, z: i32) -> bool {
        same_world(&self.world, world)
            && self.min_x <= x
            && x <= self.max_x
            && self.min_z <= z
            && z <= self.max_z
    }

    fn is_owned_by(&self, name: &str) -> bool {
        self.owner.eq_ignore_ascii_case(name)
    }

    fn region(&self, priority: i32) -> RegionInfo {
        RegionInfo {
            id: self.id,
            source: Some(PLUGIN_ID.to_string()),
            world: self.world.clone(),
            min: [self.min_x, WORLD_MIN_Y, self.min_z],
            max: [self.max_x, WORLD_MAX_Y, self.max_z],
            priority,
            flags: BTreeMap::new(),
            members: Vec::new(),
            owners: vec![self.owner.clone()],
        }
    }

    fn bounds(&self) -> String {
        format!(
            "({}, {})..({}, {})",
            self.min_x, self.min_z, self.max_x, self.max_z
        )
    }
}

/// Exact world-key comparison, ignoring the `minecraft:` namespace and case.
fn same_world(a: &str, b: &str) -> bool {
    fn normal(world: &str) -> &str {
        let world = world.trim();
        match world.get(..10) {
            Some(prefix) if prefix.eq_ignore_ascii_case("minecraft:") => &world[10..],
            _ => world,
        }
    }
    normal(a).eq_ignore_ascii_case(normal(b))
}

/// The claims plugin.
pub struct ClaimsPlugin {
    claims: BTreeMap<u64, Claim>,
    next_id: u64,
    settings: Settings,
    selections: HashMap<u64, [i32; 3]>,
    confirmations: HashMap<u64, Instant>,
}

impl Default for ClaimsPlugin {
    fn default() -> Self {
        ClaimsPlugin {
            claims: BTreeMap::new(),
            next_id: 1,
            settings: Settings::default(),
            selections: HashMap::new(),
            confirmations: HashMap::new(),
        }
    }
}

impl NativePlugin for ClaimsPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        self.settings = Settings {
            max_claims: config_uint(ctx, "max-claims").unwrap_or(self.settings.max_claims),
            max_claim_size: config_int(ctx, "max-claim-size")
                .unwrap_or(self.settings.max_claim_size),
            max_radius: config_int(ctx, "max-radius")
                .map(|v| v.clamp(0, i64::from(i32::MAX)) as i32)
                .unwrap_or(self.settings.max_radius),
            priority: config_int(ctx, "priority")
                .map(|v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
                .unwrap_or(self.settings.priority),
        };

        let mut claims = BTreeMap::new();
        for key in ctx.backend.kv_list(ctx.id, KEY_PREFIX) {
            let Some(bytes) = ctx.backend.kv_get(ctx.id, &key) else {
                continue;
            };
            match serde_json::from_slice::<Claim>(&bytes) {
                Ok(claim) => {
                    claims.insert(claim.id, claim);
                }
                Err(e) => ctx.log(
                    LogLevel::Warn,
                    &format!("ignoring damaged claim `{key}`: {e}"),
                ),
            }
        }

        let highest = claims.keys().next_back().copied().unwrap_or(0);
        let stored = ctx
            .backend
            .kv_get(ctx.id, KEY_NEXT_ID)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(1);
        self.next_id = stored.max(highest.saturating_add(1)).max(1);
        self.claims = claims;

        for claim in self.claims.values() {
            ctx.backend
                .region_upsert(ctx.id, &claim.region(self.settings.priority));
        }

        ctx.log(
            LogLevel::Info,
            &format!(
                "claims ready: {} claim(s), limits {}/{} blocks, priority {}",
                self.claims.len(),
                self.settings.max_claims,
                self.settings.max_claim_size,
                self.settings.priority
            ),
        );
        Ok(())
    }

    fn before_use_block(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        item: &str,
        _block: &str,
    ) -> Decision {
        if item != GOLDEN_SHOVEL {
            return Decision::Allow;
        }
        if !permitted(ctx, p, NODE_CLAIM) {
            send(
                ctx,
                p,
                "You do not have permission to claim land.".to_string(),
            );
            return Decision::Allow;
        }
        let Some((world, _, _, _)) = ctx.backend.player_position(ctx.id, p.id) else {
            return Decision::Allow;
        };

        if let Some(claim) = self.claim_at(&world, x, z).cloned() {
            self.selections.remove(&p.id);
            for line in inspect_lines(&claim, p) {
                send(ctx, p, line);
            }
            return Decision::Cancel(String::new());
        }

        if let Some(first) = self.selections.remove(&p.id) {
            if first[0] == x && first[2] == z {
                send(ctx, p, "Claim selection cleared.".to_string());
                return Decision::Cancel(String::new());
            }
            let lines = self.create_claim(
                ctx,
                p,
                &world,
                first[0].min(x),
                first[2].min(z),
                first[0].max(x),
                first[2].max(z),
            );
            for line in lines {
                send(ctx, p, line);
            }
            return Decision::Cancel(String::new());
        }

        self.selections.insert(p.id, [x, y, z]);
        send(
            ctx,
            p,
            format!(
                "First corner set at ({x}, {z}). Right-click the opposite corner with the golden \
                 shovel (the same corner or an existing claim cancels)."
            ),
        );
        Decision::Cancel(String::new())
    }

    fn on_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<aether_plugin::CommandResult> {
        let lines = match name {
            COMMAND_CLAIM => self.claim_command(ctx, p, args),
            COMMAND_ABANDON => self.abandon_command(ctx, p, args),
            COMMAND_ABANDON_ALL => self.abandon_all_command(ctx, p, args),
            COMMAND_LIST => self.list_command(ctx, p, args),
            _ => return None,
        };
        Some(aether_plugin::CommandResult {
            handled: true,
            lines,
        })
    }
}

impl ClaimsPlugin {
    fn claim_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_CLAIM) {
            return vec!["You do not have permission to claim land.".to_string()];
        }
        if args.len() > 1 {
            return vec!["Usage: /claim [radius]".to_string()];
        }
        let Some((world, x, _, z)) = position(ctx, p) else {
            return vec!["Could not read your position.".to_string()];
        };
        let (x, z) = (x.floor() as i32, z.floor() as i32);

        let Some(raw) = args.first() else {
            if let Some(claim) = self.claim_at(&world, x, z) {
                return inspect_lines(claim, p);
            }
            let min_x = x.div_euclid(16) * 16;
            let min_z = z.div_euclid(16) * 16;
            return self.create_claim(ctx, p, &world, min_x, min_z, min_x + 15, min_z + 15);
        };

        let radius = match raw.parse::<i32>() {
            Ok(value) => value,
            Err(_) => {
                return vec![format!(
                    "Invalid radius `{raw}`: use a whole number of blocks."
                )]
            }
        };
        if radius < 0 {
            return vec!["Radius must be zero or more.".to_string()];
        }
        if radius > self.settings.max_radius {
            return vec![format!(
                "Radius {radius} is too large: the maximum is {}.",
                self.settings.max_radius
            )];
        }
        let (Some(min_x), Some(max_x), Some(min_z), Some(max_z)) = (
            x.checked_sub(radius),
            x.checked_add(radius),
            z.checked_sub(radius),
            z.checked_add(radius),
        ) else {
            return vec!["That claim would fall outside the world.".to_string()];
        };
        self.create_claim(ctx, p, &world, min_x, min_z, max_x, max_z)
    }

    #[allow(clippy::too_many_arguments)]
    fn create_claim(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        world: &str,
        min_x: i32,
        min_z: i32,
        max_x: i32,
        max_z: i32,
    ) -> Vec<String> {
        let area =
            (i64::from(max_x) - i64::from(min_x) + 1) * (i64::from(max_z) - i64::from(min_z) + 1);
        if area > self.settings.max_claim_size {
            return vec![format!(
                "Claim is too large: {area} blocks is {} over the {} block limit.",
                area - self.settings.max_claim_size,
                self.settings.max_claim_size
            )];
        }
        let owned = self
            .claims
            .values()
            .filter(|claim| claim.is_owned_by(&p.name))
            .count();
        if owned >= self.settings.max_claims {
            return vec![format!(
                "Claim limit reached: {owned} of {} claims.",
                self.settings.max_claims
            )];
        }

        let min = [min_x, WORLD_MIN_Y, min_z];
        let max = [max_x, WORLD_MAX_Y, max_z];
        let overlaps = ctx.backend.regions_overlapping(ctx.id, world, min, max);
        if let Some(other) = overlaps.iter().find(|region| region.is_from(PLUGIN_ID)) {
            let owner = other
                .owners
                .first()
                .map(String::as_str)
                .unwrap_or("another player");
            return vec![format!("That land is already claimed by {owner}.")];
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        ctx.backend
            .kv_put(ctx.id, KEY_NEXT_ID, self.next_id.to_string().as_bytes());
        let claim = Claim {
            id,
            owner: p.name.clone(),
            world: world.to_string(),
            min_x,
            min_z,
            max_x,
            max_z,
        };
        match serde_json::to_vec(&claim) {
            Ok(bytes) => ctx
                .backend
                .kv_put(ctx.id, &format!("{KEY_PREFIX}{id}"), &bytes),
            Err(e) => return vec![format!("Could not save the claim: {e}")],
        }
        ctx.backend
            .region_upsert(ctx.id, &claim.region(self.settings.priority));
        self.claims.insert(id, claim.clone());
        self.confirmations.remove(&p.id);
        vec![format!(
            "Claim #{id} created: {} in {}, {area} blocks.",
            claim.bounds(),
            claim.world
        )]
    }

    fn abandon_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_ABANDON) {
            return vec!["You do not have permission to abandon claims.".to_string()];
        }
        if !args.is_empty() {
            return vec!["Usage: /abandonclaim".to_string()];
        }
        let Some((world, x, _, z)) = position(ctx, p) else {
            return vec!["Could not read your position.".to_string()];
        };
        let (x, z) = (x.floor() as i32, z.floor() as i32);
        let Some(claim) = self.claim_at(&world, x, z) else {
            return vec!["You are not standing in a claim.".to_string()];
        };
        if !claim.is_owned_by(&p.name) {
            return vec![format!(
                "This claim belongs to {}; you cannot abandon it.",
                claim.owner
            )];
        }
        let claim = claim.clone();
        self.remove_claim(ctx, &claim);
        vec![format!(
            "Claim #{} abandoned: {} ({} blocks freed).",
            claim.id,
            claim.bounds(),
            claim.area()
        )]
    }

    fn abandon_all_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_ABANDON_ALL) {
            return vec!["You do not have permission to abandon claims.".to_string()];
        }
        if !args.is_empty() {
            return vec!["Usage: /abandonallclaims".to_string()];
        }
        let ids: Vec<u64> = self
            .claims
            .values()
            .filter(|claim| claim.is_owned_by(&p.name))
            .map(|claim| claim.id)
            .collect();
        if ids.is_empty() {
            self.confirmations.remove(&p.id);
            return vec!["You have no claims to abandon.".to_string()];
        }
        let confirmed = self
            .confirmations
            .get(&p.id)
            .is_some_and(|at| at.elapsed() <= CONFIRM_WINDOW);
        if !confirmed {
            self.confirmations.insert(p.id, Instant::now());
            return vec![format!(
                "This will abandon {} claim(s). Run /abandonallclaims again within {} seconds to \
                 confirm.",
                ids.len(),
                CONFIRM_WINDOW.as_secs()
            )];
        }
        self.confirmations.remove(&p.id);
        for id in &ids {
            if let Some(claim) = self.claims.get(id).cloned() {
                self.remove_claim(ctx, &claim);
            }
        }
        self.selections.remove(&p.id);
        vec![format!("Abandoned {} claim(s).", ids.len())]
    }

    fn list_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_LIST) {
            return vec!["You do not have permission to list claims.".to_string()];
        }
        if args.len() > 1 {
            return vec!["Usage: /claimslist [player]".to_string()];
        }
        let target = match args.first() {
            Some(name) if !name.eq_ignore_ascii_case(&p.name) => {
                if !p.operator
                    && !permitted(ctx, p, NODE_LIST_OTHERS)
                    && !permitted(ctx, p, NODE_ADMIN)
                {
                    return vec![
                        "You do not have permission to list another player's claims.".to_string(),
                    ];
                }
                name.clone()
            }
            Some(name) => name.clone(),
            None => p.name.clone(),
        };

        let claims: Vec<&Claim> = self
            .claims
            .values()
            .filter(|claim| claim.owner.eq_ignore_ascii_case(&target))
            .collect();
        let whose = if target.eq_ignore_ascii_case(&p.name) {
            "You have".to_string()
        } else {
            format!("{target} has")
        };
        if claims.is_empty() {
            return vec![format!("{whose} no claims.")];
        }
        let mut lines = vec![format!("{whose} {} claim(s):", claims.len())];
        for claim in claims {
            lines.push(format!(
                "#{} {} in {} — {} blocks{}",
                claim.id,
                claim.bounds(),
                claim.world,
                claim.area(),
                if claim.is_owned_by(&p.name) {
                    ""
                } else {
                    " (moderator view)"
                }
            ));
        }
        lines
    }

    fn claim_at(&self, world: &str, x: i32, z: i32) -> Option<&Claim> {
        self.claims
            .values()
            .find(|claim| claim.contains(world, x, z))
    }

    fn remove_claim(&mut self, ctx: &mut NativeContext, claim: &Claim) {
        ctx.backend.region_remove(ctx.id, claim.id);
        ctx.backend
            .kv_delete(ctx.id, &format!("{KEY_PREFIX}{}", claim.id));
        self.claims.remove(&claim.id);
    }
}

/// The player's world and position, if the host knows it.
fn position(ctx: &NativeContext, p: &Player) -> Option<(String, f64, f64, f64)> {
    ctx.backend.player_position(ctx.id, p.id)
}

fn permitted(ctx: &NativeContext, p: &Player, node: &str) -> bool {
    p.operator || ctx.backend.permission_check(ctx.id, p.id, node, 0)
}

fn send(ctx: &NativeContext, p: &Player, line: String) {
    ctx.backend.send_message(ctx.id, p.id, &line);
}

fn inspect_lines(claim: &Claim, p: &Player) -> Vec<String> {
    let owner = if claim.is_owned_by(&p.name) {
        "you".to_string()
    } else {
        claim.owner.clone()
    };
    vec![
        format!("Claim #{} owned by {owner}.", claim.id),
        format!(
            "Bounds {} in {}, {} blocks.",
            claim.bounds(),
            claim.world,
            claim.area()
        ),
    ]
}

fn config_int(ctx: &NativeContext, key: &str) -> Option<i64> {
    ctx.config
        .get(key)
        .cloned()
        .or_else(|| ctx.backend.config(ctx.id, key))
        .and_then(|value| value.trim().parse::<i64>().ok())
}

fn config_uint(ctx: &NativeContext, key: &str) -> Option<usize> {
    config_int(ctx, key).and_then(|value| usize::try_from(value).ok())
}
