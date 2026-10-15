# Build the SDK-backed example plugin into a WebAssembly component.
#
# A plugin is a `wasm32-wasip2` artifact, so it is built on its own, not as
# part of the host workspace. Run `rustup target add wasm32-wasip2` once.
#
#   make          build greet.wasm next to plugin.toml
#   make check    only type-check the component
#   make clean    remove build output

CARGO ?= cargo
# Honour the shared target dir when one is configured, as the host build does.
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
WASM := $(TARGET_DIR)/wasm32-wasip2/release/greet.wasm

.PHONY: build check clean

build:
	$(CARGO) build --release --target wasm32-wasip2
	cp $(WASM) greet.wasm

check:
	$(CARGO) check --target wasm32-wasip2

clean:
	$(CARGO) clean
	rm -f greet.wasm
