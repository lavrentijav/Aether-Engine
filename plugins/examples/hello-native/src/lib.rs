//! The reference **native** Aether plugin: the in-process twin of
//! `plugins/examples/hello`.
//!
//! A native plugin is a normal Rust crate in this workspace (`plugins/<name>/`)
//! that implements [`NativePlugin`] and hands the host a [`NativeRegistration`]
//! at build time. The host links it into the server, so it runs on the same
//! event bus, with the same ordering, enable/disable and service rules as a
//! WASM plugin — but with **no boundary**: no serialization, no sandbox, no
//! epoch budget. It is trusted code the operator chooses to compile in.
//!
//! This example does the same three small things as the WASM `hello` so the two
//! tiers can be compared side by side: it answers `/hello-native`, cancels
//! breaking bedrock, and logs joins and leaves. Everything else is the neutral
//! default (a `before-*` returns `Allow`; an `after-*` does nothing).
//!
//! See [`docs/plugins/README.md`] for the step-by-step guide, and
//! `docs/design/plugins.md` for the tier model. To use it, add the crate to the
//! server's `plugins` feature and call
//! `aether_hello_native_plugin::registration()` from `install_native`.
//!
//! [`docs/plugins/README.md`]: ../../../../docs/plugins/README.md

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::{
    CommandResult, Decision, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

/// The command name this plugin owns. The core owns the vanilla names, so a
/// plugin may only add new ones; `hello-native` is ours.
pub const COMMAND: &str = "hello-native";

/// The plugin manifest.
///
/// A native plugin has no `plugin.toml` at runtime — the manifest *is* this
/// Rust value, collected from [`registration`] at the server's build time. It
/// declares exactly what a WASM `plugin.toml` would: the id (the KV namespace
/// and log tag), priority, event subscriptions and commands.
pub fn manifest() -> Manifest {
    Manifest {
        id: "hello-native".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        // Native plugins are compiled in; `entry` is ignored and stays empty.
        entry: String::new(),
        name: "Hello (native)".to_string(),
        // Higher runs first, ties broken by id, across both tiers.
        priority: 10,
        // Native plugins reach the host directly (and are trusted); this asks
        // for nothing. A service method still checks its *caller's* grant.
        capabilities: Vec::new(),
        subscribe: vec![
            "before-block-break".to_string(),
            "after-player-join".to_string(),
            "after-player-leave".to_string(),
            "on-command".to_string(),
        ],
        commands: vec![CommandDecl::Plain(COMMAND.to_string())],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server collects and links in, the native
/// analogue of dropping a `.wasm` file into `[plugins] dir`.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(HelloNative),
    }
}

/// The plugin itself. It holds no state; a real one keeps whatever it needs in
/// fields (native plugins may mutate themselves directly, with no boundary).
#[derive(Default)]
pub struct HelloNative;

impl NativePlugin for HelloNative {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        // Registration (services, load-time state) happens only here. Errors
        // refuse the load; the host logs them and no other plugin is affected.
        ctx.log(LogLevel::Info, "hello-native plugin ready");
        Ok(())
    }

    fn shutdown(&mut self, ctx: &mut NativeContext) {
        ctx.log(LogLevel::Info, "hello-native plugin stopping");
    }

    fn before_block_break(
        &mut self,
        _ctx: &mut NativeContext,
        _p: &Player,
        _x: i32,
        _y: i32,
        _z: i32,
        block: &str,
    ) -> Decision {
        // Bedrock is the one block every player expects to be unbreakable.
        if block.ends_with("bedrock") {
            Decision::Cancel("Bedrock is indestructible.".to_string())
        } else {
            Decision::Allow
        }
    }

    fn after_player_join(&mut self, ctx: &mut NativeContext, p: &Player) {
        ctx.log(LogLevel::Info, &format!("{} joined the world", p.name));
    }

    fn after_player_leave(&mut self, ctx: &mut NativeContext, p: &Player) {
        ctx.log(LogLevel::Info, &format!("{} left the world", p.name));
    }

    fn on_command(
        &mut self,
        _ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        if name != COMMAND {
            return None;
        }
        let who = args.first().map(String::as_str).unwrap_or(&p.name);
        Some(CommandResult {
            handled: true,
            lines: vec![format!("Hello, {who}!")],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use aether_plugin::{HostBackend, HostConfig, ManifestFile, PluginHost};

    /// A backend with no world: enough to record logs and answer `supports`.
    #[derive(Default)]
    struct TestBackend {
        logs: Mutex<Vec<(String, String)>>,
        kv: Mutex<HashMap<(String, String), Vec<u8>>>,
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

    fn player() -> Player {
        Player {
            id: 7,
            name: "Steve".to_string(),
            operator: false,
        }
    }

    /// The example loads and runs through the same host bus a WASM plugin uses.
    #[test]
    fn the_reference_plugin_loads_and_runs() {
        let backend = Arc::new(TestBackend::default());
        let mut host = PluginHost::new(HostConfig::default(), backend.clone()).unwrap();
        let file = ManifestFile {
            dir: PathBuf::new(),
            manifest: manifest(),
        };
        host.add_native(&file, &[], &Default::default(), (registration().new)())
            .unwrap();
        assert_eq!(host.ids(), vec!["hello-native".to_string()]);

        // The veto and the neutral default both go through the event bus.
        assert!(matches!(
            host.before_block_break(&player(), 0, 64, 0, "minecraft:bedrock"),
            Decision::Cancel(_)
        ));
        assert!(matches!(
            host.before_block_break(&player(), 0, 64, 0, "minecraft:stone"),
            Decision::Allow
        ));

        // `/hello-native` is answered; an unknown command falls through.
        let result = host.on_command(&player(), COMMAND, &[]).unwrap();
        assert_eq!(result.lines, vec!["Hello, Steve!".to_string()]);
        assert!(host.on_command(&player(), "nope", &[]).is_none());

        let logs = backend.logs.lock().unwrap().clone();
        assert!(
            logs.iter()
                .any(|(id, m)| id == "hello-native" && m.contains("plugin ready")),
            "{logs:?}"
        );
    }
}
