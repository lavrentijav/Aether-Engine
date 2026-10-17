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
//! /trust <player>           build trust in the claim you stand in
//! /containertrust <player>  container trust: opening/using, but no breaking
//! /accesstrust <player>     access trust: entering only
//! /managetrust <player>     build trust plus changing this claim's trust list
//! /untrust <player>|all     remove one trustee, or every trustee of this claim
//! golden shovel             right-click corner A, then corner B; inspecting an existing claim
//!                           with it just reports the owner
//! ```
//!
//! Trust levels are written to the core region as per-player action grants
//! (`use` for container trust, `use` + `build` for build/manage), which the
//! CMD-29b check consults after an explicit flag and before membership. The
//! grant is narrower than membership, so a container trustee can open a chest
//! but still cannot break.
//!
//! Claims are full-height columns (the core region's `-64..=383` band), stored
//! one compact JSON record per claim in the plugin KV. On startup every claim is
//! replayed into the core region store, so protection survives a restart. The
//! protection itself is the core's: a claim is written as a region whose owner
//! is a member, with the configured priority (default 10, above a WorldGuard
//! region's default 0). Overlapping a WorldGuard region is therefore allowed
//! and decided by priority; overlapping another **claim** is refused.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player, RegionInfo,
    RegionWriteError,
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
/// `/trust <player>` (build trust).
pub const COMMAND_TRUST: &str = "trust";
/// `/containertrust <player>`.
pub const COMMAND_CONTAINER_TRUST: &str = "containertrust";
/// The misspelt alias some GriefPrevention builds document.
pub const COMMAND_CONTAINER_TRUST_ALIAS: &str = "containerstrust";
/// `/accesstrust <player>`.
pub const COMMAND_ACCESS_TRUST: &str = "accesstrust";
/// `/managetrust <player>`.
pub const COMMAND_MANAGE_TRUST: &str = "managetrust";
/// `/untrust <player>|all`.
pub const COMMAND_UNTRUST: &str = "untrust";

/// Permission for creating and inspecting a claim.
pub const NODE_CLAIM: &str = "griefprevention.claim";
/// Permission for `/abandonclaim`.
pub const NODE_ABANDON: &str = "griefprevention.abandonclaim";
/// Permission for `/abandonallclaims`.
pub const NODE_ABANDON_ALL: &str = "griefprevention.abandonallclaims";
/// Permission for `/claimslist`.
pub const NODE_LIST: &str = "griefprevention.claimslist";
/// Permission for every `/trust` and `/untrust` command.
pub const NODE_TRUST: &str = "griefprevention.trust";
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

/// Serialises claim-record read-modify-write cycles across plugin instances in
/// the same process, so two `/trust` commands racing on one claim cannot lose
/// one trustee to a stale read.
static TRUST_LOCK: Mutex<()> = Mutex::new(());

/// Claim ids live in a plugin-namespaced high range, so the claims plugin and
/// the WorldGuard-style `regions` plugin cannot collide by both counting from
/// one. The low 32 bits are the persistent serial.
pub const CLAIM_ID_NAMESPACE: u64 = 0x434C_4149_0000_0000;
/// The serial part of a claim id (the low 32 bits).
pub const CLAIM_ID_MASK: u64 = 0x0000_0000_FFFF_FFFF;

/// How many id candidates a single allocation or install tries before giving
/// up. Free ids are normally found on the first attempt.
const ID_ATTEMPTS: usize = 64;

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
        subscribe: vec![
            "before-use-block".to_string(),
            "on-command".to_string(),
            "after-player-leave".to_string(),
        ],
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
            CommandDecl::Detailed {
                name: COMMAND_TRUST.to_string(),
                node: NODE_TRUST.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_CONTAINER_TRUST.to_string(),
                node: NODE_TRUST.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_CONTAINER_TRUST_ALIAS.to_string(),
                node: NODE_TRUST.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_ACCESS_TRUST.to_string(),
                node: NODE_TRUST.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_MANAGE_TRUST.to_string(),
                node: NODE_TRUST.to_string(),
                op: 0,
            },
            CommandDecl::Detailed {
                name: COMMAND_UNTRUST.to_string(),
                node: NODE_TRUST.to_string(),
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
    max_claim_blocks: i64,
    max_radius: i32,
    priority: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_claims: 10,
            max_claim_size: 10_000,
            max_claim_blocks: 100_000,
            max_radius: 64,
            priority: 10,
        }
    }
}

/// A GriefPrevention trust level.
///
/// The levels are hierarchical: build includes container and access, container
/// includes access, and manage includes build plus the right to change trust.
/// Each claim stores one level per player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// May enter the claim.
    Access,
    /// May enter, open containers and use interactive blocks.
    Container,
    /// May enter, use and break/place blocks.
    Build,
    /// Build trust plus the right to manage the claim's trust list.
    Manage,
}

impl TrustLevel {
    /// The name shown to players and stored in JSON.
    pub const fn label(self) -> &'static str {
        match self {
            TrustLevel::Access => "access",
            TrustLevel::Container => "container",
            TrustLevel::Build => "build",
            TrustLevel::Manage => "manage",
        }
    }

    /// The core action flags this level grants.
    const fn actions(self) -> &'static [&'static str] {
        match self {
            TrustLevel::Access => &["entry"],
            TrustLevel::Container => &["entry", "use"],
            TrustLevel::Build | TrustLevel::Manage => &["entry", "use", "build"],
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
    /// Trusted players and their level. Missing in records written by CMD-35a,
    /// so it defaults to an empty map on load.
    #[serde(default)]
    trusted: BTreeMap<String, TrustLevel>,
}

impl Claim {
    fn area(&self) -> i64 {
        let width = i64::from(self.max_x) - i64::from(self.min_x) + 1;
        let depth = i64::from(self.max_z) - i64::from(self.min_z) + 1;
        // Persisted claims are bounded by `create_claim`, but a hand-edited or
        // damaged record must not panic the tick.
        width.saturating_mul(depth).max(0)
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

    /// The trust level `name` holds, matched case-insensitively.
    fn trust_of(&self, name: &str) -> Option<TrustLevel> {
        self.trusted
            .iter()
            .find(|(trusted, _)| trusted.eq_ignore_ascii_case(name.trim()))
            .map(|(_, level)| *level)
    }

    /// Give `name` `level`, replacing any level they already had.
    fn set_trust(&mut self, name: &str, level: TrustLevel) {
        if let Some(key) = self
            .trusted
            .keys()
            .find(|trusted| trusted.eq_ignore_ascii_case(name))
            .cloned()
        {
            self.trusted.insert(key, level);
        } else {
            self.trusted.insert(name.to_string(), level);
        }
    }

    /// Remove every trust level from `name`. Returns whether anything changed.
    fn remove_trust(&mut self, name: &str) -> bool {
        let Some(key) = self
            .trusted
            .keys()
            .find(|trusted| trusted.eq_ignore_ascii_case(name))
            .cloned()
        else {
            return false;
        };
        self.trusted.remove(&key);
        true
    }

    /// Whether `name` may change this claim's trust list.
    fn may_manage_trust(&self, name: &str) -> bool {
        self.is_owned_by(name) || self.trust_of(name) == Some(TrustLevel::Manage)
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
            trusted: self
                .trusted
                .iter()
                .map(|(name, level)| {
                    (
                        name.clone(),
                        level
                            .actions()
                            .iter()
                            .map(|flag| (*flag).to_string())
                            .collect(),
                    )
                })
                .collect(),
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
        self.settings = load_settings(ctx);

        // Read every persisted claim first, then install them one at a time so
        // an id reassignment (or a corrupted record) is handled in isolation.
        let mut pending = Vec::new();
        for key in ctx.backend.kv_list(ctx.id, KEY_PREFIX) {
            let Some(bytes) = ctx.backend.kv_get(ctx.id, &key) else {
                continue;
            };
            match serde_json::from_slice::<Claim>(&bytes) {
                Ok(claim) => pending.push(claim),
                Err(e) => ctx.log(
                    LogLevel::Warn,
                    &format!("ignoring damaged claim `{key}`: {e}"),
                ),
            }
        }
        pending.sort_by_key(|claim| claim.id);

        let highest = pending
            .iter()
            .map(|claim| claim.id & CLAIM_ID_MASK)
            .max()
            .unwrap_or(0);
        let stored = ctx
            .backend
            .kv_get(ctx.id, KEY_NEXT_ID)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(1);
        self.next_id = stored.max(highest.saturating_add(1)).max(1);
        self.claims.clear();

        for claim in pending {
            let old_id = claim.id;
            let Some(installed) = self.install_existing(ctx, claim) else {
                continue;
            };
            if installed.id != old_id {
                // The old id collided with another plugin's region (or came
                // from the pre-namespace format): move the record to the new id
                // so the next restart replays without a reassignment.
                ctx.backend
                    .kv_delete(ctx.id, &format!("{KEY_PREFIX}{old_id}"));
                if let Err(e) = save_claim(ctx, &installed) {
                    ctx.log(
                        LogLevel::Error,
                        &format!("claim #{} could not be re-saved: {e}", installed.id),
                    );
                }
            }
            self.claims.insert(installed.id, installed);
        }

        ctx.log(
            LogLevel::Info,
            &format!(
                "claims ready: {} claim(s), limits {} claims, {}/claim, {} blocks total, priority {}",
                self.claims.len(),
                self.settings.max_claims,
                self.settings.max_claim_size,
                self.settings.max_claim_blocks,
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

    fn after_player_leave(&mut self, _ctx: &mut NativeContext, p: &Player) {
        // Both maps are keyed by the session entity id. Drop the pending corner
        // and any armed `/abandonallclaims` confirmation so a later session that
        // reuses the id cannot inherit them.
        self.selections.remove(&p.id);
        self.confirmations.remove(&p.id);
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
            COMMAND_TRUST => self.trust_command(ctx, p, args, TrustLevel::Build),
            COMMAND_CONTAINER_TRUST | COMMAND_CONTAINER_TRUST_ALIAS => {
                self.trust_command(ctx, p, args, TrustLevel::Container)
            }
            COMMAND_ACCESS_TRUST => self.trust_command(ctx, p, args, TrustLevel::Access),
            COMMAND_MANAGE_TRUST => self.trust_command(ctx, p, args, TrustLevel::Manage),
            COMMAND_UNTRUST => self.untrust_command(ctx, p, args),
            _ => return None,
        };
        Some(aether_plugin::CommandResult {
            handled: true,
            lines,
        })
    }
}

impl ClaimsPlugin {
    /// The next claim id in the plugin's namespaced range.
    ///
    /// The serial comes from the host's process-wide allocator, which is atomic
    /// across plugin instances and threads, so two instances that would each
    /// start counting at one cannot choose the same id. `next_id` is the local
    /// floor and is persisted for diagnostics; the allocator never goes below
    /// it, so a restart does not reuse a serial. A candidate another plugin
    /// already owns is skipped by the caller's retry when the fresh insert is
    /// refused.
    fn allocate_id(&mut self, ctx: &mut NativeContext) -> Result<u64, String> {
        let floor = {
            let serial = self.next_id & CLAIM_ID_MASK;
            if serial == 0 {
                1
            } else {
                serial
            }
        };
        let serial = ctx.backend.region_allocate_serial(ctx.id, floor) & CLAIM_ID_MASK;
        let serial = serial.max(1);
        self.next_id = if serial >= CLAIM_ID_MASK {
            1
        } else {
            serial + 1
        };
        ctx.backend
            .kv_put(ctx.id, KEY_NEXT_ID, self.next_id.to_string().as_bytes());
        Ok(CLAIM_ID_NAMESPACE | serial)
    }

    /// Install a persisted claim, reassigning its id when it is not namespaced
    /// or the id already belongs to another plugin. Returns the claim with the
    /// id actually installed, or `None` when the record cannot be installed
    /// (already logged).
    fn install_existing(&mut self, ctx: &mut NativeContext, claim: Claim) -> Option<Claim> {
        let mut claim = claim;
        let mut needs_new_id = claim.id & !CLAIM_ID_MASK != CLAIM_ID_NAMESPACE;
        for _ in 0..ID_ATTEMPTS {
            if needs_new_id {
                match self.allocate_id(ctx) {
                    Ok(id) => claim.id = id,
                    Err(e) => {
                        ctx.log(
                            LogLevel::Error,
                            &format!("claim `{}` could not get an id: {e}", claim.bounds()),
                        );
                        return None;
                    }
                }
            }
            // Startup replay is idempotent: it may replace the plugin's own
            // region with the same id, but it still may not take a core or
            // foreign id. A fresh `/claim` uses `region_insert` instead, which
            // refuses every occupied id.
            match ctx
                .backend
                .region_upsert(ctx.id, &claim.region(self.settings.priority))
            {
                Ok(()) => return Some(claim),
                Err(
                    RegionWriteError::OwnedByCore
                    | RegionWriteError::OwnedByOther(_)
                    | RegionWriteError::IdTaken,
                ) => {
                    needs_new_id = true;
                }
                Err(RegionWriteError::Overlap) => {
                    ctx.log(
                        LogLevel::Error,
                        &format!(
                            "claim #{} ({}) overlaps another claim; not installed",
                            claim.id,
                            claim.bounds()
                        ),
                    );
                    return None;
                }
                Err(RegionWriteError::Unavailable) => {
                    ctx.log(
                        LogLevel::Error,
                        "claim region store is unavailable; claims not installed",
                    );
                    return None;
                }
            }
        }
        ctx.log(
            LogLevel::Error,
            &format!(
                "claim #{} ({}) could not be installed after {ID_ATTEMPTS} attempts",
                claim.id,
                claim.bounds()
            ),
        );
        None
    }

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
        let width = i64::from(max_x) - i64::from(min_x) + 1;
        let depth = i64::from(max_z) - i64::from(min_z) + 1;
        let Some(area) = width.checked_mul(depth) else {
            return vec!["Claim is too large.".to_string()];
        };
        if area > self.settings.max_claim_size {
            return vec![format!(
                "Claim is too large: {area} blocks is {} over the {} block limit.",
                area - self.settings.max_claim_size,
                self.settings.max_claim_size
            )];
        }
        let mut owned = 0usize;
        let mut owned_area = 0i64;
        for claim in self
            .claims
            .values()
            .filter(|claim| claim.is_owned_by(&p.name))
        {
            owned += 1;
            owned_area = owned_area.saturating_add(claim.area());
        }
        if owned >= self.settings.max_claims {
            return vec![format!(
                "Claim limit reached: {owned} of {} claims.",
                self.settings.max_claims
            )];
        }
        let total = owned_area.saturating_add(area);
        if total > self.settings.max_claim_blocks {
            return vec![format!(
                "Claim is too large: {area} blocks would put you {} over your {} block claim \
                 allowance ({owned_area} blocks already claimed).",
                total - self.settings.max_claim_blocks,
                self.settings.max_claim_blocks
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

        // The overlap check above is only for the friendly owner message. The
        // insert below is atomic in the host store, so two competing claims
        // cannot both win; an id that belongs to another plugin is retried with
        // the next namespaced serial.
        let mut claim = Claim {
            id: 0,
            owner: p.name.clone(),
            world: world.to_string(),
            min_x,
            min_z,
            max_x,
            max_z,
            trusted: BTreeMap::new(),
        };
        let mut installed = false;
        for _ in 0..ID_ATTEMPTS {
            match self.allocate_id(ctx) {
                Ok(id) => claim.id = id,
                Err(e) => return vec![e],
            }
            match ctx
                .backend
                .region_insert(ctx.id, &claim.region(self.settings.priority))
            {
                Ok(()) => {
                    installed = true;
                    break;
                }
                Err(RegionWriteError::Overlap) => {
                    return vec!["That land is already claimed.".to_string()]
                }
                Err(
                    RegionWriteError::IdTaken
                    | RegionWriteError::OwnedByCore
                    | RegionWriteError::OwnedByOther(_),
                ) => {
                    // The id is occupied; the host allocator hands out the next
                    // serial on the retry, so the claim is never persisted under
                    // an id that protects nothing.
                    continue;
                }
                Err(RegionWriteError::Unavailable) => {
                    return vec!["Claim protection is unavailable on this host.".to_string()]
                }
            }
        }
        if !installed {
            return vec!["Could not allocate a claim id; try again.".to_string()];
        }
        if let Err(e) = save_claim(ctx, &claim) {
            // Never keep a protection region with no persisted claim behind it.
            ctx.backend.region_remove(ctx.id, claim.id);
            return vec![format!("Could not save the claim: {e}")];
        }
        self.claims.insert(claim.id, claim.clone());
        self.confirmations.remove(&p.id);
        vec![format!(
            "Claim #{} created: {} in {}, {area} blocks.",
            claim.id,
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

    /// `/trust`, `/containertrust`, `/accesstrust`, `/managetrust`.
    fn trust_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
        level: TrustLevel,
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_TRUST) {
            return vec!["You do not have permission to manage claim trust.".to_string()];
        }
        if args.len() != 1 || args[0].trim().is_empty() {
            return vec![format!("Usage: /{} <player>", trust_command_name(level))];
        }
        let target = args[0].trim();
        let Some((world, x, _, z)) = position(ctx, p) else {
            return vec!["Could not read your position.".to_string()];
        };
        let (x, z) = (x.floor() as i32, z.floor() as i32);
        let Some(found) = self.claim_at(&world, x, z).cloned() else {
            return vec!["You are not standing in a claim.".to_string()];
        };
        if !found.may_manage_trust(&p.name) {
            return vec!["Only the claim's owner or a manager can change trust.".to_string()];
        }
        if found.is_owned_by(target) {
            return vec!["The claim owner already has full access.".to_string()];
        }

        // Re-read the record under the process-wide trust lock so two plugins
        // (or two command threads) editing the same claim cannot lose an
        // update to a stale in-memory copy.
        let _guard = TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let claim = load_claim(ctx, found.id).unwrap_or(found);
        if !claim.may_manage_trust(&p.name) {
            return vec!["Only the claim's owner or a manager can change trust.".to_string()];
        }
        if claim.is_owned_by(target) {
            return vec!["The claim owner already has full access.".to_string()];
        }
        if claim.trust_of(target) == Some(level) {
            return vec![format!(
                "{target} already has {} trust in claim #{}.",
                level.label(),
                claim.id
            )];
        }

        let mut updated = claim.clone();
        updated.set_trust(target, level);
        match self.publish_claim(ctx, &claim, updated) {
            Ok(()) => vec![format!(
                "{target} now has {} trust in claim #{}.",
                level.label(),
                claim.id
            )],
            Err(e) => vec![e],
        }
    }

    /// `/untrust <player>` or `/untrust all`.
    fn untrust_command(
        &mut self,
        ctx: &mut NativeContext,
        p: &Player,
        args: &[String],
    ) -> Vec<String> {
        if !permitted(ctx, p, NODE_TRUST) {
            return vec!["You do not have permission to manage claim trust.".to_string()];
        }
        if args.len() != 1 || args[0].trim().is_empty() {
            return vec!["Usage: /untrust <player>|all".to_string()];
        }
        let target = args[0].trim();
        let Some((world, x, _, z)) = position(ctx, p) else {
            return vec!["Could not read your position.".to_string()];
        };
        let (x, z) = (x.floor() as i32, z.floor() as i32);
        let Some(found) = self.claim_at(&world, x, z).cloned() else {
            return vec!["You are not standing in a claim.".to_string()];
        };
        if !found.may_manage_trust(&p.name) {
            return vec!["Only the claim's owner or a manager can change trust.".to_string()];
        }

        let _guard = TRUST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let claim = load_claim(ctx, found.id).unwrap_or(found);
        if !claim.may_manage_trust(&p.name) {
            return vec!["Only the claim's owner or a manager can change trust.".to_string()];
        }

        if target.eq_ignore_ascii_case("all") {
            if claim.trusted.is_empty() {
                return vec!["No players are trusted in this claim.".to_string()];
            }
            let removed = claim.trusted.len();
            let mut updated = claim.clone();
            updated.trusted.clear();
            return match self.publish_claim(ctx, &claim, updated) {
                Ok(()) => vec![format!(
                    "Removed all trust from {removed} player(s) in claim #{}.",
                    claim.id
                )],
                Err(e) => vec![e],
            };
        }

        if claim.trust_of(target).is_none() {
            return vec![format!(
                "{target} does not have trust in claim #{}.",
                claim.id
            )];
        }
        let mut updated = claim.clone();
        updated.remove_trust(target);
        match self.publish_claim(ctx, &claim, updated) {
            Ok(()) => vec![format!(
                "Removed all trust from {target} in claim #{}.",
                claim.id
            )],
            Err(e) => vec![e],
        }
    }

    /// Publish a changed claim to the core region index first, then persist it.
    /// A failed save rolls the region back so a claim is never protected by a
    /// state the KV does not hold.
    fn publish_claim(
        &mut self,
        ctx: &mut NativeContext,
        old: &Claim,
        updated: Claim,
    ) -> Result<(), String> {
        if let Err(e) = ctx
            .backend
            .region_upsert(ctx.id, &updated.region(self.settings.priority))
        {
            return Err(format!("Could not update the claim protection: {e}"));
        }
        if let Err(e) = save_claim(ctx, &updated) {
            let _ = ctx
                .backend
                .region_upsert(ctx.id, &old.region(self.settings.priority));
            return Err(format!("Could not save the claim trust: {e}"));
        }
        self.claims.insert(updated.id, updated);
        Ok(())
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

fn trust_command_name(level: TrustLevel) -> &'static str {
    match level {
        TrustLevel::Access => COMMAND_ACCESS_TRUST,
        TrustLevel::Container => COMMAND_CONTAINER_TRUST,
        TrustLevel::Build => COMMAND_TRUST,
        TrustLevel::Manage => COMMAND_MANAGE_TRUST,
    }
}

fn inspect_lines(claim: &Claim, p: &Player) -> Vec<String> {
    let owner = if claim.is_owned_by(&p.name) {
        "you".to_string()
    } else {
        claim.owner.clone()
    };
    let mut lines = vec![
        format!("Claim #{} owned by {owner}.", claim.id),
        format!(
            "Bounds {} in {}, {} blocks.",
            claim.bounds(),
            claim.world,
            claim.area()
        ),
    ];
    if !claim.trusted.is_empty() {
        let trusted = claim
            .trusted
            .iter()
            .map(|(name, level)| format!("{name} ({})", level.label()))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Trusted: {trusted}."));
    }
    lines
}

/// The persisted record for `id`, if the KV still has a readable one.
fn load_claim(ctx: &NativeContext, id: u64) -> Option<Claim> {
    let bytes = ctx.backend.kv_get(ctx.id, &format!("{KEY_PREFIX}{id}"))?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist one claim under its current id.
fn save_claim(ctx: &NativeContext, claim: &Claim) -> Result<(), String> {
    let bytes = serde_json::to_vec(claim).map_err(|e| e.to_string())?;
    ctx.backend
        .kv_put(ctx.id, &format!("{KEY_PREFIX}{}", claim.id), &bytes);
    Ok(())
}

/// An operator config value: the native `[plugins.claims] config` map first,
/// then the backend's `config` import.
fn config_value(ctx: &NativeContext, key: &str) -> Option<String> {
    ctx.config
        .get(key)
        .cloned()
        .or_else(|| ctx.backend.config(ctx.id, key))
}

/// The first configured key from `keys`, with the key name that matched.
fn config_first(ctx: &NativeContext, keys: &[&str]) -> Option<(String, String)> {
    keys.iter()
        .find_map(|key| config_value(ctx, key).map(|value| ((*key).to_string(), value)))
}

/// Read the settings, warning loudly on every value that is not usable instead
/// of silently substituting the default.
fn load_settings(ctx: &mut NativeContext) -> Settings {
    let mut settings = Settings::default();

    if let Some(raw) = config_value(ctx, "max-claims") {
        match raw.trim().parse::<usize>() {
            Ok(value) => settings.max_claims = value,
            Err(_) => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `max-claims` value `{raw}`: expected a non-negative whole number; \
                     using {}",
                    settings.max_claims
                ),
            ),
        }
    }

    if let Some(raw) = config_value(ctx, "max-claim-size") {
        match raw.trim().parse::<i64>() {
            Ok(value) if value > 0 => settings.max_claim_size = value,
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `max-claim-size` value `{raw}`: expected a positive whole number; \
                     using {}",
                    settings.max_claim_size
                ),
            ),
        }
    }

    if let Some((key, raw)) = config_first(ctx, &["max-claim-blocks", "max-accrued-blocks"]) {
        match raw.trim().parse::<i64>() {
            Ok(value) if value > 0 => settings.max_claim_blocks = value,
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `{key}` value `{raw}`: expected a positive whole number; using {}",
                    settings.max_claim_blocks
                ),
            ),
        }
    }

    if let Some(raw) = config_value(ctx, "max-radius") {
        match raw.trim().parse::<i64>() {
            Ok(value) if value >= 0 => {
                if value > i64::from(i32::MAX) {
                    ctx.log(
                        LogLevel::Warn,
                        &format!(
                            "`max-radius` value `{raw}` is out of range; using {}",
                            i32::MAX
                        ),
                    );
                    settings.max_radius = i32::MAX;
                } else {
                    settings.max_radius = value as i32;
                }
            }
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `max-radius` value `{raw}`: expected a non-negative whole number; \
                     using {}",
                    settings.max_radius
                ),
            ),
        }
    }

    if let Some(raw) = config_value(ctx, "priority") {
        match raw.trim().parse::<i64>() {
            Ok(value) if (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&value) => {
                settings.priority = value as i32;
            }
            _ => ctx.log(
                LogLevel::Warn,
                &format!(
                    "invalid `priority` value `{raw}`: expected a 32-bit whole number; using {}",
                    settings.priority
                ),
            ),
        }
    }

    // A square claim larger than `max-claim-size` can never be created, so a
    // larger configured radius only produces a confusing area error later.
    // Clamp it to the largest radius that can still fit the size limit.
    let side = (settings.max_claim_size.max(1) as f64).sqrt() as i64;
    let largest_useful = ((side - 1) / 2).clamp(0, i64::from(i32::MAX)) as i32;
    if settings.max_radius > largest_useful {
        settings.max_radius = largest_useful;
    }

    settings
}
