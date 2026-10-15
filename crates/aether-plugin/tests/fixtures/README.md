# Test fixtures

These are `wasm32-wasip2` components the host tests load. They are checked in
so `cargo test` needs no WebAssembly target and no network.

- `hello.wasm` — a copy of the reference plugin built from
  `plugins/examples/hello`. Regenerate with `make` in that directory, then copy
  the result over `hello/hello.wasm`.
- `greet/greet.wasm` — the SDK-backed dependent built from
  `plugins/examples/greet`, used by `tests/sdk_bridge.rs`. Regenerate with
  `make` in that directory, then copy the result over `greet/greet.wasm`.
- `recorder.wasm` — built from `tests/guests/recorder`, a plugin whose
  behaviour (cancel / trap / spin) is chosen by its config. Regenerate with:

  ```sh
  rustup target add wasm32-wasip2
  cd tests/guests/recorder
  cargo build --release --target wasm32-wasip2
  cp "$CARGO_TARGET_DIR/wasm32-wasip2/release/recorder.wasm" ../../fixtures/recorder.wasm
  ```

  (omit `$CARGO_TARGET_DIR/` when it is not set).
