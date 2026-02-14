# ── Irrigation System ─────────────────────────────────────────────
# Rust workspace: hub (Pi 5) + node (Pi Zero W)
# ──────────────────────────────────────────────────────────────────

# Cross-compilation targets
TARGET_HUB   := aarch64-unknown-linux-gnu
TARGET_NODE  := arm-unknown-linux-gnueabihf

# Remote hosts (override via env or make args)
HUB_HOST  ?= pi5.local
NODE_HOST ?= pizero.local
REMOTE_USER ?= pi

# ── Development (native) ─────────────────────────────────────────

.PHONY: build check test clippy fmt fmt-check clean doc

## Build the entire workspace (debug)
build:
	cargo build --workspace

## Build the entire workspace (release)
release:
	cargo build --workspace --release

## Type-check without producing binaries
check:
	cargo check --workspace

## Run all tests
test:
	cargo test --workspace

## Run clippy lints (deny warnings)
clippy:
	cargo clippy --workspace -- -D warnings

## Check formatting (CI-friendly, no mutation)
fmt-check:
	cargo fmt --all -- --check

## Auto-format all code
fmt:
	cargo fmt --all

## Remove build artifacts
clean:
	cargo clean

## Generate and open docs
doc:
	cargo doc --workspace --no-deps --open

# ── Individual crates ─────────────────────────────────────────────

.PHONY: build-hub build-node run-hub run-node test-hub test-node

## Build hub crate only (debug)
build-hub:
	cargo build -p irrigation-hub

## Build node crate only (debug)
build-node:
	cargo build -p irrigation-node

## Run hub locally
run-hub:
	cargo run -p irrigation-hub

## Run node locally
run-node:
	cargo run -p irrigation-node

## Test hub crate only
test-hub:
	cargo test -p irrigation-hub

## Test node crate only
test-node:
	cargo test -p irrigation-node

# ── Cross-compilation ────────────────────────────────────────────

.PHONY: cross-hub cross-node cross-all

## Cross-compile hub for Pi 5 (aarch64)
cross-hub:
	cross build -p irrigation-hub --release --target $(TARGET_HUB)

## Cross-compile node for Pi Zero W (armv6 / armhf)
cross-node:
	cross build -p irrigation-node --release --target $(TARGET_NODE)

## Cross-compile everything
cross-all: cross-hub cross-node

# ── Deploy ────────────────────────────────────────────────────────

.PHONY: deploy-hub deploy-node deploy-all

## Deploy hub binary to Pi 5 via scp
deploy-hub: cross-hub
	scp target/$(TARGET_HUB)/release/irrigation-hub $(REMOTE_USER)@$(HUB_HOST):~/irrigation-hub

## Deploy node binary to Pi Zero via scp
deploy-node: cross-node
	scp target/$(TARGET_NODE)/release/irrigation-node $(REMOTE_USER)@$(NODE_HOST):~/irrigation-node

## Deploy both
deploy-all: deploy-hub deploy-node

# ── Docker (local dev) ───────────────────────────────────────────

.PHONY: docker-up docker-down docker-logs

## Spin up the full dev stack (mqtt + hub + nodes)
docker-up:
	docker compose up --build

## Tear down the dev stack
docker-down:
	docker compose down

## Follow logs from all containers
docker-logs:
	docker compose logs -f

# ── CI / pre-commit ──────────────────────────────────────────────

.PHONY: ci lint

## Full lint pass (clippy + fmt check)
lint: clippy fmt-check

## CI pipeline: fmt, clippy, test
ci: fmt-check clippy test

# ── Help ──────────────────────────────────────────────────────────

.PHONY: help
help:
	@echo ""
	@echo "  Irrigation System — Make Targets"
	@echo "  ─────────────────────────────────"
	@echo ""
	@echo "  Development:"
	@echo "    build        Build workspace (debug)"
	@echo "    release      Build workspace (release)"
	@echo "    check        Type-check only"
	@echo "    test         Run all tests"
	@echo "    clippy       Lint with clippy (-D warnings)"
	@echo "    fmt          Auto-format code"
	@echo "    fmt-check    Check formatting (no changes)"
	@echo "    clean        Remove build artifacts"
	@echo "    doc          Generate & open docs"
	@echo ""
	@echo "  Crates:"
	@echo "    build-hub    Build hub crate"
	@echo "    build-node   Build node crate"
	@echo "    run-hub      Run hub locally"
	@echo "    run-node     Run node locally"
	@echo "    test-hub     Test hub crate"
	@echo "    test-node    Test node crate"
	@echo ""
	@echo "  Cross-compilation (requires 'cross'):"
	@echo "    cross-hub    Build hub for Pi 5 ($(TARGET_HUB))"
	@echo "    cross-node   Build node for Pi Zero ($(TARGET_NODE))"
	@echo "    cross-all    Build both"
	@echo ""
	@echo "  Deploy (scp to remote hosts):"
	@echo "    deploy-hub   Deploy hub to $(HUB_HOST)"
	@echo "    deploy-node  Deploy node to $(NODE_HOST)"
	@echo "    deploy-all   Deploy both"
	@echo ""
	@echo "  Docker (local dev):"
	@echo "    docker-up    Build & start dev stack (mqtt + hub + nodes)"
	@echo "    docker-down  Tear down dev stack"
	@echo "    docker-logs  Follow container logs"
	@echo ""
	@echo "  CI:"
	@echo "    lint          clippy + fmt-check"
	@echo "    ci            fmt-check + clippy + test"
	@echo ""
	@echo "  Override remote hosts:"
	@echo "    make deploy-hub HUB_HOST=192.168.1.50"
	@echo "    make deploy-node NODE_HOST=192.168.1.51 REMOTE_USER=admin"
	@echo ""
