//! The sidebar plugin driven through the real host trait with a recording
//! backend and a stub placeholders service: what reaches the core is what these
//! tests check.
//!
//! The placeholder *runtime* (built-ins, caching) is the SDK plugin's, tested
//! there; these tests use a small stub that only substitutes the tokens the
//! sidebar's own composition is asserted on.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use aether_plugin::placeholders::{self, PlaceholderFn, PlaceholdersService, PlayerExtra};
use aether_plugin::sidebar::{sidebar, SidebarBoard};
use aether_plugin::{HostBackend, LogLevel, NativeContext, NativePlugin, Player, ServiceRegistry};

use aether_sidebar_plugin::registration;

/// A stub placeholders service: substitutes a few built-ins and any pushed
/// value, leaving everything else literal.
#[derive(Default)]
struct MockPlaceholders {
    values: Mutex<HashMap<String, String>>,
}

impl PlaceholdersService for MockPlaceholders {
    fn register(&self, _caller: &str, _name: String, _provider: PlaceholderFn) -> bool {
        false
    }
    fn register_name(&self, _caller: &str, _name: String) -> bool {
        false
    }
    fn set_value(&self, _caller: &str, name: String, _player: u64, value: String) {
        self.values.lock().unwrap().insert(name, value);
    }
    fn resolve(&self, _caller: &str, p: &Player, text: &str) -> String {
        let mut out = text.to_string();
        for (name, value) in self.values.lock().unwrap().iter() {
            out = out.replace(&format!("%{name}%"), value);
        }
        out.replace("%player%", &p.name)
            .replace("%online%", "1")
            .replace("%max%", "20")
            .replace("%ping%", "42")
            .replace("%world%", "minecraft:overworld")
    }
}

/// A backend that answers a fixed player view and records what the plugin
/// pushes.
#[derive(Default)]
struct Mock {
    players: Vec<Player>,
    extra: Option<PlayerExtra>,
    /// Whether the vanilla `/scoreboard` claims the sidebar slot (for `yield`).
    vanilla_claimed: bool,
    boards: Mutex<Vec<(u64, SidebarBoard)>>,
    cleared: Mutex<Vec<u64>>,
    tabs: Mutex<Vec<(u64, String, String)>>,
}

impl HostBackend for Mock {
    fn kv_get(&self, _plugin: &str, _key: &str) -> Option<Vec<u8>> {
        None
    }
    fn kv_put(&self, _plugin: &str, _key: &str, _value: &[u8]) {}
    fn kv_delete(&self, _plugin: &str, _key: &str) {}
    fn kv_list(&self, _plugin: &str, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
    fn log(&self, _plugin: &str, _level: LogLevel, _message: &str) {}
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        false
    }
    fn online_players(&self, _plugin: &str) -> Vec<Player> {
        self.players.clone()
    }
    fn player_extra(&self, _plugin: &str, _player: u64) -> Option<PlayerExtra> {
        self.extra.clone()
    }
    fn set_sidebar(&self, _plugin: &str, player: u64, board: &SidebarBoard) {
        self.boards.lock().unwrap().push((player, board.clone()));
    }
    fn clear_sidebar(&self, _plugin: &str, player: u64) {
        self.cleared.lock().unwrap().push(player);
    }
    fn set_tab_header_footer(&self, _plugin: &str, player: u64, header: &str, footer: &str) {
        self.tabs
            .lock()
            .unwrap()
            .push((player, header.to_string(), footer.to_string()));
    }
    fn vanilla_sidebar_claimed(&self, _plugin: &str, _player: u64) -> bool {
        self.vanilla_claimed
    }
}

fn setup() -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    setup_cfg(BTreeMap::new())
}

fn setup_cfg(
    config: BTreeMap<String, String>,
) -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    setup_with(config, false)
}

fn setup_with(
    config: BTreeMap<String, String>,
    vanilla_claimed: bool,
) -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    let backend = Arc::new(Mock {
        players: vec![Player {
            id: 7,
            name: "Steve".to_string(),
            operator: false,
        }],
        extra: Some(PlayerExtra {
            ping: 42,
            world: "minecraft:overworld".to_string(),
            x: 10.0,
            y: 64.0,
            z: -3.0,
            health: 20.0,
            level: 5,
            display_name: "Steve".to_string(),
        }),
        vanilla_claimed,
        ..Mock::default()
    });
    let services = Arc::new(ServiceRegistry::new());
    // The sidebar requires the shared placeholders service; register a stub.
    placeholders::register_placeholders(&services, Arc::new(MockPlaceholders::default()))
        .expect("register placeholders");
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut plugin = (registration().new)();
    let mut ctx = NativeContext {
        id: "sidebar",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    plugin.init(&mut ctx).expect("init");
    // Drive the first second (20 ticks); at 4 Hz the accumulator fires at
    // ticks 5, 10, 15 and 20.
    run_ticks(&mut plugin, &backend, &services, 1, 20);
    (plugin, backend, services)
}

/// Run `on_tick` for every tick in `from..=to`, as the server does.
fn run_ticks(
    plugin: &mut Box<dyn NativePlugin>,
    backend: &Arc<Mock>,
    services: &Arc<ServiceRegistry>,
    from: u64,
    to: u64,
) {
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    for n in from..=to {
        let mut ctx = NativeContext {
            id: "sidebar",
            capabilities: &[],
            config: &BTreeMap::new(),
            services,
            backend: &backend_dyn,
        };
        plugin.on_tick(&mut ctx, n);
    }
}

#[test]
fn a_tick_pushes_one_board_with_placeholders_resolved() {
    let (_plugin, backend, _services) = setup();
    let boards = backend.boards.lock().unwrap();
    // 4 Hz over one second: four redraws, all for the one online player.
    assert!(!boards.is_empty());
    assert!(boards.iter().all(|(pid, _)| *pid == 7));
    let board = &boards.last().unwrap().1;
    assert_eq!(board.title, "§6§lAether", "title rendered with § codes");
    assert!(board.lines.iter().any(|l| l.contains("Steve")));
    assert_eq!(
        board.lines[0], "§7Online: §f1§7/§f20",
        "online/max resolved and coloured"
    );
    assert!(board.lines.iter().any(|l| l.contains("42ms")));
}

#[test]
fn the_redraw_rate_is_exact_for_a_frequency_that_does_not_divide_twenty() {
    // 8 Hz: with integer tick division `20/8` would give a period of 2 ticks
    // (10 Hz). The accumulator fires on 5 of every 4 ticks instead, so a
    // second of ticks yields exactly 8 redraws.
    let mut config = BTreeMap::new();
    config.insert("update_hz".to_string(), "8".to_string());
    let (_plugin, backend, _services) = setup_cfg(config);
    assert_eq!(
        backend.boards.lock().unwrap().len(),
        8,
        "8 redraws in the 20 ticks of the first second"
    );
}

#[test]
fn a_line_override_is_resolved_through_the_placeholders_service() {
    let (mut plugin, backend, services) = setup();
    backend.boards.lock().unwrap().clear();
    // A value fed through the shared service, then an overridden line using it.
    placeholders::placeholders(&services)
        .expect("placeholders registered")
        .set_value("test", "test".to_string(), 7, "VALUE".to_string());
    let svc = sidebar(&services).expect("sidebar registered");
    svc.set_line(7, 0, "%test%".to_string());
    run_ticks(&mut plugin, &backend, &services, 21, 25);
    let boards = backend.boards.lock().unwrap();
    let board = &boards.last().unwrap().1;
    assert_eq!(board.lines[0], "VALUE", "override resolved the placeholder");
}

#[test]
fn toggling_hides_and_shows_the_sidebar() {
    let (mut plugin, backend, services) = setup();
    backend.boards.lock().unwrap().clear();
    let svc = sidebar(&services).expect("service registered");
    assert!(!svc.toggle(7), "first toggle hides");
    run_ticks(&mut plugin, &backend, &services, 21, 25);
    assert_eq!(*backend.cleared.lock().unwrap(), vec![7]);
    assert!(backend.boards.lock().unwrap().is_empty());

    backend.cleared.lock().unwrap().clear();
    assert!(svc.toggle(7), "second toggle shows");
    run_ticks(&mut plugin, &backend, &services, 26, 30);
    assert_eq!(backend.boards.lock().unwrap().len(), 1);
}

#[test]
fn the_tab_switch_mirrors_the_board_into_the_header_and_footer() {
    let mut config = BTreeMap::new();
    config.insert("tab".to_string(), "true".to_string());
    let (_plugin, backend, _services) = setup_cfg(config);
    let tabs = backend.tabs.lock().unwrap();
    assert!(!tabs.is_empty(), "a header/footer for the one player");
    let (player, header, footer) = tabs.last().unwrap();
    assert_eq!(*player, 7);
    assert_eq!(header, "§6§lAether", "the title is the header");
    assert!(footer.contains("Steve"), "the lines are the footer");
}

#[test]
fn an_unchanged_board_is_the_same_board_every_tick() {
    // The core's diff is what makes two equal boards cost nothing; the plugin
    // must therefore hand over an equal board on an unchanged tick.
    let (mut plugin, backend, services) = setup();
    let first = backend.boards.lock().unwrap().last().unwrap().1.clone();
    backend.boards.lock().unwrap().clear();
    run_ticks(&mut plugin, &backend, &services, 21, 25);
    let second = backend.boards.lock().unwrap().last().unwrap().1.clone();
    assert_eq!(first, second);
}

#[test]
fn yield_mode_leaves_a_vanilla_sidebar_alone() {
    // The host reports the vanilla `/scoreboard` owns the slot; in `yield`
    // mode the plugin must not draw over it.
    let mut config = BTreeMap::new();
    config.insert("mode".to_string(), "yield".to_string());
    let (_plugin, backend, _services) = setup_with(config, true);
    assert!(
        backend.boards.lock().unwrap().is_empty(),
        "yield must not push a board while vanilla owns the slot"
    );
    assert!(
        backend.cleared.lock().unwrap().contains(&7),
        "the plugin hides its own panel so the vanilla one shows"
    );
}

#[test]
fn default_override_mode_draws_even_when_vanilla_claims_the_slot() {
    let (_plugin, backend, _services) = setup_with(BTreeMap::new(), true);
    assert!(
        !backend.boards.lock().unwrap().is_empty(),
        "override mode ignores the vanilla claim"
    );
}

#[test]
fn reload_is_refused_for_a_non_operator() {
    let (mut plugin, backend, services) = setup();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut ctx = NativeContext {
        id: "sidebar",
        capabilities: &[],
        config: &BTreeMap::new(),
        services: &services,
        backend: &backend_dyn,
    };
    let p = Player {
        id: 7,
        name: "Steve".to_string(),
        operator: false,
    };
    let result = plugin
        .on_command(&mut ctx, &p, "sidebar", &["reload".to_string()])
        .expect("handled");
    assert!(result.handled);
    assert!(
        result.lines.iter().any(|l| l.contains("operator")),
        "a non-operator reload is refused: {:?}",
        result.lines
    );
}

#[test]
fn reload_reads_the_file_off_the_tick_thread_and_applies_on_a_later_tick() {
    // A config file with a distinctive title, read in the background.
    let dir = std::env::temp_dir().join(format!("sb1-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let body = |title: &str| format!("title = \"{title}\"\n\n[[layout]]\nlines = [\"%player%\"]\n");
    std::fs::write(&path, body("&cOld")).unwrap();

    let mut config = BTreeMap::new();
    config.insert("config".to_string(), path.to_string_lossy().into_owned());
    let (mut plugin, backend, services) = setup_cfg(config);
    // The original title was loaded at init; rewrite the file and reload.
    assert_eq!(
        backend.boards.lock().unwrap().last().unwrap().1.title,
        "§cOld"
    );
    backend.boards.lock().unwrap().clear();
    std::fs::write(&path, body("&cReloaded")).unwrap();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();

    // The operator runs `/sidebar reload`; the command returns immediately and
    // the worker thread does the read.
    let p = Player {
        id: 7,
        name: "Steve".to_string(),
        operator: true,
    };
    let mut ctx = NativeContext {
        id: "sidebar",
        capabilities: &[],
        config: &BTreeMap::new(),
        services: &services,
        backend: &backend_dyn,
    };
    let result = plugin
        .on_command(&mut ctx, &p, "sidebar", &["reload".to_string()])
        .expect("handled");
    assert!(result.handled);
    assert!(
        result.lines.iter().any(|l| l.contains("started")),
        "the command returns before the read: {:?}",
        result.lines
    );

    // Ticking applies the pending config once the worker has finished; retry a
    // few times to avoid a fixed sleep.
    let mut applied = false;
    let mut start = 100;
    for _ in 0..200 {
        // A full second of ticks (20) both applies the pending config and
        // triggers the 4 Hz redraw.
        run_ticks(&mut plugin, &backend, &services, start, start + 20);
        start += 21;
        if backend
            .boards
            .lock()
            .unwrap()
            .last()
            .map(|(_, b)| b.title.as_str())
            == Some("§cReloaded")
        {
            applied = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(applied, "the reloaded title reaches the board");
    std::fs::remove_dir_all(&dir).ok();
}
