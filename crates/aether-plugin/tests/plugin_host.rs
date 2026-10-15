//! Host behaviour, driven by real components.
//!
//! The fixtures are `wasm32-wasip2` components built from the guest crates in
//! `tests/guests/`; see `tests/fixtures/README.md` for how to regenerate them.
//! Everything the host does with a component — subscription filtering, the
//! priority order, cancellation, trap isolation and the per-call time budget —
//! is only testable against a component that actually runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aether_plugin::manifest::Manifest;
use aether_plugin::{
    Decision, HostBackend, HostConfig, LogLevel, ManifestFile, Player, PluginHost, PluginTier,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// An in-memory backend that records every log line, so a test can assert
/// which plugin was called and in what order.
#[derive(Default)]
struct TestBackend {
    logs: Mutex<Vec<(String, String)>>,
    kv: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl TestBackend {
    fn messages(&self) -> Vec<(String, String)> {
        self.logs.lock().unwrap().clone()
    }

    /// Messages whose text ends with `suffix`, in the order they arrived.
    fn ending_with(&self, suffix: &str) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m)| m.ends_with(suffix))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

impl HostBackend for TestBackend {
    fn kv_get(&self, plugin: &str, key: &str) -> Option<Vec<u8>> {
        self.kv
            .lock()
            .unwrap()
            .get(&(plugin.to_string(), key.to_string()))
            .cloned()
    }
    fn kv_put(&self, plugin: &str, key: &str, value: &[u8]) {
        self.kv
            .lock()
            .unwrap()
            .insert((plugin.to_string(), key.to_string()), value.to_vec());
    }
    fn kv_delete(&self, plugin: &str, key: &str) {
        self.kv
            .lock()
            .unwrap()
            .remove(&(plugin.to_string(), key.to_string()));
    }
    fn kv_list(&self, plugin: &str, prefix: &str) -> Vec<String> {
        self.kv
            .lock()
            .unwrap()
            .keys()
            .filter(|(p, k)| p == plugin && k.starts_with(prefix))
            .map(|(_, k)| k.clone())
            .collect()
    }
    fn log(&self, plugin: &str, _level: LogLevel, message: &str) {
        self.logs
            .lock()
            .unwrap()
            .push((plugin.to_string(), message.to_string()));
    }
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, feature: &str) -> bool {
        feature == "test.feature"
    }
}

/// A recorder fixture with the given id, priority, subscriptions and config.
fn recorder(
    id: &str,
    priority: i32,
    subscribe: &[&str],
    commands: &[&str],
    config: &[(&str, &str)],
) -> (
    ManifestFile,
    Vec<String>,
    std::collections::BTreeMap<String, String>,
) {
    let manifest = Manifest {
        id: id.to_string(),
        version: "0.0.0".to_string(),
        api: "0.1".to_string(),
        entry: "recorder.wasm".to_string(),
        name: String::new(),
        priority,
        capabilities: vec![],
        subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
        commands: commands.iter().map(|s| s.to_string()).collect(),
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Wasm,
        requires: Vec::new(),
    };
    let mut cfg = std::collections::BTreeMap::new();
    // Every recorder acknowledges under the id the test gave it, so two
    // instances can be told apart in the log.
    cfg.insert("id".to_string(), id.to_string());
    for (k, v) in config {
        cfg.insert((*k).to_string(), (*v).to_string());
    }
    let caps = ["log", "config", "supports", "storage.kv"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    (
        ManifestFile {
            dir: fixtures(),
            manifest,
        },
        caps,
        cfg,
    )
}

fn host() -> (PluginHost, Arc<TestBackend>) {
    let backend = Arc::new(TestBackend::default());
    let host = PluginHost::new(HostConfig::default(), backend.clone()).unwrap();
    (host, backend)
}

fn player() -> Player {
    Player {
        id: 7,
        name: "Steve".to_string(),
        operator: false,
    }
}

#[test]
fn a_plugin_loads_and_its_init_runs() {
    let (mut h, backend) = host();
    let m = aether_plugin::manifest::load(&fixtures().join("hello/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    assert_eq!(h.ids(), vec!["hello".to_string()]);
    assert!(
        backend
            .messages()
            .iter()
            .any(|(id, m)| id == "hello" && m.contains("hello plugin ready")),
        "init should have logged: {:?}",
        backend.messages()
    );
}

#[test]
fn a_command_registered_by_a_plugin_is_answered() {
    let (mut h, _backend) = host();
    let m = aether_plugin::manifest::load(&fixtures().join("hello/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    let result = h.on_command(&player(), "hello", &[]).unwrap();
    assert!(result.handled);
    assert_eq!(result.lines, vec!["Hello, Steve!".to_string()]);
    // An unregistered command falls through to the host.
    assert!(h.on_command(&player(), "nope", &[]).is_none());
}

#[test]
fn plugins_run_in_priority_order_and_the_first_cancel_wins() {
    let (mut h, backend) = host();
    let (high, caps, cfg) = recorder(
        "high",
        20,
        &["before-block-break"],
        &[],
        &[("cancel", "before-block-break")],
    );
    let (low, low_caps, low_cfg) = recorder("low", 10, &["before-block-break"], &[], &[]);
    h.add(&high, &caps, &cfg).unwrap();
    h.add(&low, &low_caps, &low_cfg).unwrap();
    assert_eq!(h.ids(), vec!["high".to_string(), "low".to_string()]);

    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    match decision {
        Decision::Cancel(reason) => assert!(reason.contains("high"), "{reason}"),
        Decision::Allow => panic!("high priority should have cancelled"),
    }
    // The cancel stops the walk: `low` is never called.
    assert_eq!(backend.ending_with(":before-block-break"), vec!["high"]);
}

#[test]
fn observe_events_reach_every_subscriber_in_priority_order() {
    let (mut h, backend) = host();
    let (a, caps, cfg) = recorder("a", 5, &["after-player-join"], &[], &[]);
    let (b, b_caps, b_cfg) = recorder("b", 50, &["after-player-join"], &[], &[]);
    h.add(&a, &caps, &cfg).unwrap();
    h.add(&b, &b_caps, &b_cfg).unwrap();
    h.after_player_join(&player());
    assert_eq!(backend.ending_with(":after-player-join"), vec!["b", "a"]);
}

#[test]
fn an_unsubscribed_event_is_never_called() {
    let (mut h, backend) = host();
    let (r, caps, cfg) = recorder("r", 0, &["after-damage"], &[], &[]);
    h.add(&r, &caps, &cfg).unwrap();
    // Not subscribed: even though the plugin implements the export.
    h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    h.on_tick(1);
    assert!(backend.ending_with(":before-block-break").is_empty());
    assert!(backend.ending_with(":on-tick").is_empty());
    // Subscribed: it is called.
    h.after_damage(1, 3.0, "minecraft:fall");
    assert_eq!(backend.ending_with(":after-damage"), vec!["r"]);
}

#[test]
fn a_trapping_plugin_does_not_stop_the_others() {
    let (mut h, backend) = host();
    let (bad, caps, bad_cfg) = recorder(
        "bad",
        20,
        &["before-block-break"],
        &[],
        &[("trap", "before-block-break")],
    );
    let (good, _, good_cfg) = recorder("good", 10, &["before-block-break"], &[], &[]);
    h.add(&bad, &caps, &bad_cfg).unwrap();
    h.add(&good, &caps, &good_cfg).unwrap();

    // A trap is treated as `allow`, and the next plugin still runs.
    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(matches!(decision, Decision::Allow));
    assert_eq!(backend.ending_with(":before-block-break"), vec!["good"]);
    assert!(
        backend
            .messages()
            .iter()
            .any(|(id, m)| id == "bad" && m.contains("trap in `before-block-break`")),
        "the trap must be logged: {:?}",
        backend.messages()
    );
}

#[test]
fn the_shared_tick_budget_bounds_all_plugins_together() {
    // The per-call budget catches one runaway call; without a *shared* budget
    // a hook with many plugins still adds up to several ticks. With it, once
    // the tick's budget is spent no further plugin runs, so the whole hook
    // stays near one budget.
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        budget_ms: 40,
        tick_budget_ms: 40,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    for i in 0..6 {
        let id = format!("spin{i}");
        let (m, caps, cfg) = recorder(&id, 0, &["on-tick"], &[], &[("spin", "on-tick")]);
        h.add(&m, &caps, &cfg).unwrap();
    }
    let start = Instant::now();
    h.on_tick(1);
    let took = start.elapsed();
    assert_eq!(
        backend.ending_with(":on-tick").len(),
        1,
        "only the first plugin should run before the shared budget is spent"
    );
    assert!(
        took < Duration::from_millis(300),
        "the shared budget should keep the whole hook near one tick, took {took:?}"
    );
}

#[test]
fn init_is_bounded_by_the_configured_fuel() {
    // `init_fuel` is operator-tunable; a budget too small for a plugin's
    // first-time work must fail the load rather than run unbounded.
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        init_fuel: 1,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend).unwrap();
    let (m, caps, cfg) = recorder("r", 0, &[], &[], &[]);
    let err = h.add(&m, &caps, &cfg).unwrap_err();
    assert!(
        h.ids().is_empty(),
        "a plugin out of init fuel must not load"
    );
    let _ = err;

    // The default budget loads the same plugin fine.
    let (mut h2, _backend) = host();
    let (m2, caps2, cfg2) = recorder("r", 0, &[], &[], &[]);
    h2.add(&m2, &caps2, &cfg2).unwrap();
    assert_eq!(h2.ids(), vec!["r".to_string()]);
}

#[test]
fn a_plugin_may_not_shadow_a_reserved_command() {
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        reserved_commands: vec!["give".to_string()],
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    // A plugin that registers the operator verb `give` must not be handed it.
    let (m, caps, cfg) = recorder("evil", 0, &["on-command"], &["give"], &[]);
    h.add(&m, &caps, &cfg).unwrap();
    assert!(
        h.on_command(&player(), "give", &[]).is_none(),
        "a reserved built-in name must not reach a plugin"
    );
    assert!(backend.ending_with(":on-command").is_empty());
}

#[test]
fn the_operator_flag_reaches_a_plugin_command() {
    let (mut h, _backend) = host();
    // The recorder answers to its own id.
    let (m, caps, cfg) = recorder("r", 0, &["on-command"], &["r"], &[]);
    h.add(&m, &caps, &cfg).unwrap();
    let mut p = player();
    p.operator = true;
    let result = h.on_command(&p, "r", &[]).unwrap();
    assert_eq!(result.lines, vec!["r:on-command:op=true".to_string()]);
    p.operator = false;
    let result = h.on_command(&p, "r", &[]).unwrap();
    assert_eq!(result.lines, vec!["r:on-command:op=false".to_string()]);
}

#[test]
fn no_epoch_ticker_until_a_plugin_loads() {
    let (mut h, _backend) = host();
    assert!(
        !h.has_epoch_ticker(),
        "a host with no plugins must not spawn the epoch thread"
    );
    let (m, caps, cfg) = recorder("r", 0, &["after-player-join"], &[], &[]);
    h.add(&m, &caps, &cfg).unwrap();
    assert!(
        h.has_epoch_ticker(),
        "loading a plugin starts the epoch clock it is bounded by"
    );
}

#[test]
fn a_duplicate_manifest_id_is_refused() {
    let (mut h, _backend) = host();
    let (first, caps, cfg) = recorder("dup", 0, &["after-player-join"], &[], &[]);
    let (second, second_caps, second_cfg) = recorder("dup", 0, &["after-player-join"], &[], &[]);
    h.add(&first, &caps, &cfg).unwrap();
    let err = h.add(&second, &second_caps, &second_cfg).unwrap_err();
    assert!(err.to_string().contains("already loaded"), "{err}");
    assert_eq!(
        h.ids(),
        vec!["dup".to_string()],
        "only one copy stays loaded"
    );
}

#[test]
fn a_fail_closed_plugin_vetoes_when_it_traps() {
    let (mut h, _backend) = host();
    let (mut m, caps, cfg) = recorder(
        "guard",
        0,
        &["before-block-break"],
        &[],
        &[("trap", "before-block-break")],
    );
    m.manifest.deny_on_fault = true;
    h.add(&m, &caps, &cfg).unwrap();
    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(
        matches!(decision, Decision::Cancel(_)),
        "a fail-closed plugin that traps must veto, not allow"
    );
}

#[test]
fn a_fail_closed_plugin_vetoes_when_the_budget_is_spent() {
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        budget_ms: 40,
        tick_budget_ms: 40,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    // "burn" spins and eats the whole shared tick budget, without vetoing.
    let (burn, caps, burn_cfg) = recorder(
        "burn",
        30,
        &["before-block-break"],
        &[],
        &[("spin", "before-block-break")],
    );
    h.add(&burn, &caps, &burn_cfg).unwrap();
    // "guard" is fail-closed but never gets to run: the budget is gone.
    let (mut guard, guard_caps, guard_cfg) =
        recorder("guard", 10, &["before-block-break"], &[], &[]);
    guard.manifest.deny_on_fault = true;
    h.add(&guard, &guard_caps, &guard_cfg).unwrap();

    // A tick starts the shared budget window.
    h.on_tick(1);
    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(
        matches!(decision, Decision::Cancel(_)),
        "a fail-closed plugin that cannot run must veto, not allow"
    );
}

#[test]
fn idling_between_calls_does_not_spend_the_tick_budget() {
    // The shared budget is time the plugins spend *running*, not wall-clock
    // time since the tick began. A tick is 50 ms and a `before-*` may fire
    // anywhere in it, so if idle time counted, events late in the tick would
    // be skipped (or, fail-closed, spuriously vetoed). Start a tick, idle far
    // past the budget, and check the next `before-*` is still consulted.
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        budget_ms: 5,
        tick_budget_ms: 40,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    let (m, caps, cfg) = recorder("r", 0, &["before-block-break"], &[], &[]);
    h.add(&m, &caps, &cfg).unwrap();

    h.on_tick(1); // starts the tick; no plugin has run, so nothing is spent
    std::thread::sleep(Duration::from_millis(150)); // three times the budget

    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(matches!(decision, Decision::Allow));
    assert_eq!(
        backend.ending_with(":before-block-break"),
        vec!["r"],
        "idling past the tick budget must not consume it: the plugin is still consulted"
    );
}

#[test]
fn idling_does_not_spuriously_veto_a_fail_closed_plugin() {
    // The same wall-clock bug hit a `deny_on_fault` plugin harder: an event
    // late in the tick looked like a spent budget and vetoed an action it had
    // never examined. Idling must leave it consulted and allowing.
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        budget_ms: 5,
        tick_budget_ms: 40,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    let (mut m, caps, cfg) = recorder("guard", 0, &["before-block-break"], &[], &[]);
    m.manifest.deny_on_fault = true;
    h.add(&m, &caps, &cfg).unwrap();

    h.on_tick(1);
    std::thread::sleep(Duration::from_millis(150));

    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(
        matches!(decision, Decision::Allow),
        "no budget was spent, so a fail-closed plugin must not veto"
    );
    assert_eq!(backend.ending_with(":before-block-break"), vec!["guard"]);
}

#[test]
fn a_plugin_that_overruns_its_budget_is_interrupted() {
    let backend = Arc::new(TestBackend::default());
    let config = HostConfig {
        budget_ms: 30,
        ..HostConfig::default()
    };
    let mut h = PluginHost::new(config, backend.clone()).unwrap();
    let (spin, caps, cfg) = recorder("spin", 0, &["on-tick"], &[], &[("spin", "on-tick")]);
    h.add(&spin, &caps, &cfg).unwrap();

    let start = Instant::now();
    h.on_tick(1);
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "the busy loop should have been interrupted, took {took:?}"
    );
    assert!(
        backend
            .messages()
            .iter()
            .any(|(id, m)| id == "spin" && m.contains("trap in `on-tick`")),
        "the overrun must be recorded as a fault: {:?}",
        backend.messages()
    );
}

// --- native tier -------------------------------------------------------

use aether_plugin::{CommandResult, NativeContext, NativePlugin, ServiceRegistry};

/// A native plugin that records every call and cancels breaking bedrock, so a
/// test can see it go through the same bus as a WASM one.
struct TestNative {
    calls: Arc<Mutex<Vec<String>>>,
}

impl NativePlugin for TestNative {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Publish a service named after this plugin, the way the SDK does.
        ctx.services
            .register(&format!("{}.svc", ctx.id), "0.1", Arc::new(41u32))?;
        self.calls.lock().unwrap().push(format!("init:{}", ctx.id));
        Ok(())
    }

    fn shutdown(&mut self, ctx: &mut NativeContext) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("shutdown:{}", ctx.id));
    }

    fn before_block_break(
        &mut self,
        ctx: &mut NativeContext,
        _p: &Player,
        _x: i32,
        _y: i32,
        _z: i32,
        block: &str,
    ) -> Decision {
        self.calls.lock().unwrap().push(format!("break:{}", ctx.id));
        if block.ends_with("bedrock") {
            Decision::Cancel("Bedrock is indestructible.".to_string())
        } else {
            Decision::Allow
        }
    }

    fn on_command(
        &mut self,
        _ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        _args: &[String],
    ) -> Option<CommandResult> {
        if name != "native" {
            return None;
        }
        Some(CommandResult {
            handled: true,
            lines: vec![format!("native hello, {}", p.name)],
        })
    }
}

fn native_manifest(id: &str, priority: i32, subscribe: &[&str], requires: &[&str]) -> ManifestFile {
    ManifestFile {
        dir: fixtures(),
        manifest: Manifest {
            id: id.to_string(),
            version: "0.0.0".to_string(),
            api: "0.1".to_string(),
            entry: String::new(),
            name: String::new(),
            priority,
            capabilities: vec![],
            subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
            commands: vec!["native".to_string()],
            tick_period: 1,
            deny_on_fault: false,
            tier: PluginTier::Native,
            requires: requires.iter().map(|s| s.to_string()).collect(),
        },
    }
}

fn native(calls: Arc<Mutex<Vec<String>>>) -> Box<dyn NativePlugin> {
    Box::new(TestNative { calls })
}

#[test]
fn a_native_plugin_loads_runs_and_registers_a_service() {
    let (mut h, backend) = host();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let m = native_manifest("native-test", 0, &["before-block-break", "on-command"], &[]);
    h.add_native(&m, &[], &Default::default(), native(Arc::clone(&calls)))
        .unwrap();
    assert_eq!(h.ids(), vec!["native-test".to_string()]);
    assert_eq!(calls.lock().unwrap().as_slice(), ["init:native-test"]);
    // The service it published is resolvable.
    assert_eq!(
        *h.services().get::<u32>("native-test.svc", "0.1").unwrap(),
        41
    );

    // It goes through the same event bus as a WASM plugin.
    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:bedrock");
    assert!(matches!(decision, Decision::Cancel(_)));
    let decision = h.before_block_break(&player(), 0, 64, 0, "minecraft:stone");
    assert!(matches!(decision, Decision::Allow));

    let result = h.on_command(&player(), "native", &[]).unwrap();
    assert_eq!(result.lines, vec!["native hello, Steve".to_string()]);

    h.shutdown();
    let _ = backend;
}

#[test]
fn a_native_dependency_on_a_missing_service_is_refused_clearly() {
    let (mut h, _backend) = host();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let m = native_manifest("needs-sdk", 0, &[], &["sdk.permissions@0.1"]);
    let err = h
        .add_native(&m, &[], &Default::default(), native(calls))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("sdk.permissions@0.1"), "{text}");
    assert!(text.contains("not registered"), "{text}");
    assert!(
        h.ids().is_empty(),
        "a dependent with no provider must not load"
    );
}

#[test]
fn a_dependency_loads_once_its_provider_is_present() {
    let (mut h, _backend) = host();
    // The provider registers `provider.svc@0.1`; a dependent requiring it
    // loads only after the provider does.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = native_manifest("provider", 10, &[], &[]);
    h.add_native(
        &provider,
        &[],
        &Default::default(),
        native(Arc::clone(&calls)),
    )
    .unwrap();

    let dependent = native_manifest("dependent", 0, &[], &["provider.svc@0.1"]);
    h.add_native(
        &dependent,
        &[],
        &Default::default(),
        native(Arc::clone(&calls)),
    )
    .unwrap();
    assert_eq!(
        h.ids(),
        vec!["provider".to_string(), "dependent".to_string()]
    );
}

#[test]
fn a_wasm_plugin_may_not_be_added_as_native_and_vice_versa() {
    let (mut h, _backend) = host();
    let m = aether_plugin::manifest::load(&fixtures().join("hello/plugin.toml")).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let err = h
        .add_native(&m, &["log".to_string()], &Default::default(), native(calls))
        .unwrap_err();
    assert!(err.to_string().contains("not a native plugin"), "{err}");

    // And a native manifest through the WASM path is refused too.
    let native = native_manifest("n", 0, &[], &[]);
    let err = h.add(&native, &[], &Default::default()).unwrap_err();
    assert!(err.to_string().contains("not a WASM component"), "{err}");
}

#[test]
fn the_service_registry_is_shared_with_the_host() {
    let (h, _backend) = host();
    let reg: &Arc<ServiceRegistry> = h.services();
    reg.register("shared", "0.1", Arc::new("value".to_string()))
        .unwrap();
    assert_eq!(*reg.get::<String>("shared", "0.1").unwrap(), "value");
}
