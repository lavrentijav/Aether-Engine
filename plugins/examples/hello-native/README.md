# hello-native — reference native plugin

The smallest useful **native** (tier N) Aether plugin. It is the in-process twin
of [`plugins/examples/hello`](../hello): it answers `/hello-native`, cancels
breaking bedrock, and logs player joins and leaves.

Read it as the template for a native plugin. The whole tier is:

1. a library crate under `plugins/<name>/` with `aether-plugin` as a dependency;
2. a `NativePlugin` impl (`src/lib.rs`);
3. a `manifest()` describing id, priority, subscriptions and commands;
4. a `registration()` the server links in — the native analogue of dropping a
   `.wasm` file into `[plugins] dir`.

`plugin.toml` is here for comparison only; a native plugin is compiled in, so
the host reads the manifest from Rust.

To wire one into the server: add the crate as an optional dependency behind the
`plugins` feature, then call `registration()` from `install_native` in
`crates/aether-server/src/plugins.rs` (and gate it on `[plugins.<id>] enabled`
like every other plugin, which `install_native_one` already does).

The step-by-step guide is [`docs/plugins/README.md`](../../../docs/plugins/README.md);
the tier model is [`docs/design/plugins.md`](../../../docs/design/plugins.md).

Build and test like any workspace crate:

```sh
cargo test -p aether-hello-native-plugin
```
