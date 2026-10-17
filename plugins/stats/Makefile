# Build the stats plugin into a WebAssembly component.
#
# A plugin is a `wasm32-wasip2` artifact, so it is built for the component
# target even though the crate is a workspace member (for host-side tests).
# Run `rustup target add wasm32-wasip2` once.
#
#   make          build stats.wasm next to plugin.toml
#   make check    only type-check the component
#   make clean    remove build output

CARGO ?= cargo
# Honour the shared target dir when one is configured, as the host build does.
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
WASM := $(TARGET_DIR)/wasm32-wasip2/release/stats.wasm

.PHONY: build check clean

build:
	$(CARGO) build --release --target wasm32-wasip2 -p aether-stats-plugin
	cp $(WASM) stats.wasm

check:
	$(CARGO) check --target wasm32-wasip2 -p aether-stats-plugin

clean:
	$(CARGO) clean -p aether-stats-plugin
	rm -f stats.wasm
