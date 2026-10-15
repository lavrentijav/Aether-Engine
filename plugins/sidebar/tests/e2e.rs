//! The sidebar plugin driven through the real host trait with a recording
//! backend: what reaches the core is what these tests check.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use aether_plugin::sidebar::{sidebar, PlayerExtra, SidebarBoard};
use aether_plugin::{HostBackend, LogLevel, NativeContext, NativePlugin, Player, ServiceRegistry};

use aether_sidebar_plugin::registration;

/// A backend that answers a fixed player view and records what the plugin
/// pushes.
#[derive(Default)]
struct Mock {
    players: Vec<Player>,
    extra: Option<PlayerExtra>,
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
}

fn setup() -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    setup_cfg(BTreeMap::new())
}

fn setup_cfg(
    config: BTreeMap<String, String>,
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
        ..Mock::default()
    });
    let services = Arc::new(ServiceRegistry::new());
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut plugin = (registration().new)();
    {
        let mut ctx = NativeContext {
            id: "sidebar",
            capabilities: &[],
            config: &config,
            services: &services,
            backend: &backend_dyn,
        };
        plugin.init(&mut ctx).expect("init");
        // The first tick lands on the 4 Hz period (5 ticks).
        plugin.on_tick(&mut ctx, 5);
    }
    (plugin, backend, services)
}

/// A helper to run a tick outside the setup borrow.
fn tick(
    plugin: &mut Box<dyn NativePlugin>,
    backend: &Arc<Mock>,
    services: &Arc<ServiceRegistry>,
    n: u64,
) {
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut ctx = NativeContext {
        id: "sidebar",
        capabilities: &[],
        config: &BTreeMap::new(),
        services,
        backend: &backend_dyn,
    };
    plugin.on_tick(&mut ctx, n);
}

#[test]
fn a_tick_pushes_one_board_with_placeholders_resolved() {
    let (_plugin, backend, _services) = setup();
    let boards = backend.boards.lock().unwrap();
    assert_eq!(boards.len(), 1, "one board for the one online player");
    let (player, board) = &boards[0];
    assert_eq!(*player, 7);
    assert_eq!(board.title, "§6§lAether", "title rendered with § codes");
    assert!(board.lines.iter().any(|l| l.contains("Steve")));
    assert_eq!(
        board.lines[0], "§7Online: §f1§7/§f20",
        "online/max resolved and coloured"
    );
    assert!(board.lines.iter().any(|l| l.contains("42ms")));
}

#[test]
fn the_service_can_register_a_placeholder_and_override_a_line() {
    let (mut plugin, backend, services) = setup();
    backend.boards.lock().unwrap().clear();
    let svc = sidebar(&services).expect("service registered");
    svc.register_placeholder(
        "test".to_string(),
        Arc::new(|_q: &aether_plugin::sidebar::PlaceholderQuery| Some("VALUE".to_string())),
    );
    svc.set_line(7, 0, "%test%".to_string());
    tick(&mut plugin, &backend, &services, 10);
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
    tick(&mut plugin, &backend, &services, 10);
    assert_eq!(*backend.cleared.lock().unwrap(), vec![7]);
    assert!(backend.boards.lock().unwrap().is_empty());

    backend.cleared.lock().unwrap().clear();
    assert!(svc.toggle(7), "second toggle shows");
    tick(&mut plugin, &backend, &services, 15);
    assert_eq!(backend.boards.lock().unwrap().len(), 1);
}

#[test]
fn the_tab_switch_mirrors_the_board_into_the_header_and_footer() {
    let mut config = BTreeMap::new();
    config.insert("tab".to_string(), "true".to_string());
    let (_plugin, backend, _services) = setup_cfg(config);
    let tabs = backend.tabs.lock().unwrap();
    assert_eq!(tabs.len(), 1, "one header/footer for the one player");
    let (player, header, footer) = &tabs[0];
    assert_eq!(*player, 7);
    assert_eq!(header, "§6§lAether", "the title is the header");
    assert!(footer.contains("Steve"), "the lines are the footer");
}

#[test]
fn an_unchanged_board_is_the_same_board_every_tick() {
    // The core's diff is what makes two equal boards cost nothing; the plugin
    // must therefore hand over an equal board on an unchanged tick.
    let (mut plugin, backend, services) = setup();
    let first = backend.boards.lock().unwrap()[0].1.clone();
    backend.boards.lock().unwrap().clear();
    tick(&mut plugin, &backend, &services, 10);
    let second = backend.boards.lock().unwrap()[0].1.clone();
    assert_eq!(first, second);
}
