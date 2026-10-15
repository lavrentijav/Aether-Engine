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
    Decision, HostBackend, HostConfig, LogLevel, ManifestFile, Player, PluginHost,
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
