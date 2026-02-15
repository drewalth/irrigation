# ── Irrigation System ─────────────────────────────────────────────
# Rust workspace: hub (Pi 5) + node (Pi Zero W)
# ──────────────────────────────────────────────────────────────────

.DEFAULT_GOAL := setup

# Cross-compilation targets
TARGET_HUB   := aarch64-unknown-linux-gnu
TARGET_NODE  := arm-unknown-linux-gnueabihf

# Remote hosts (override via env or make args)
HUB_HOST  ?= pi5.local
NODE_HOST ?= pizero.local
REMOTE_USER ?= pi

# Web UI source directory
UI_DIR := crates/hub/src/ui

# sqlx compile-time database
SQLX_DB     := crates/hub/irrigation.db
SQLX_MIGRATION := crates/hub/migrations/0001_init.sql

# Minimum required Node.js version (major.minor.patch)
NODE_MIN_MAJOR := 22
NODE_MIN_MINOR := 12
NODE_MIN_PATCH := 0

# ── Web UI ───────────────────────────────────────────────────────

.PHONY: build-ui

## Build the web UI (requires Node.js / npm)
build-ui:
	cd $(UI_DIR) && npm ci && npm run build

# ── Setup (dev environment) ──────────────────────────────────────

.PHONY: setup _check-tools _check-node-version _setup-ui _setup-db

## Prepare the development environment
setup: _check-tools _check-node-version _setup-ui _setup-db
	@echo ""
	@echo "  ✓ Setup complete. You can now run:"
	@echo "      make build       — debug build"
	@echo "      make run-hub     — run hub locally"
	@echo ""

## Verify required CLI tools are available
_check-tools:
	@echo "── Checking required tools ──"
	@command -v node   >/dev/null 2>&1 || { echo "  ✗ node not found. Install via nvm: https://github.com/nvm-sh/nvm"; exit 1; }
	@echo "  node   $(shell node --version 2>/dev/null || echo 'missing')"
	@command -v npm    >/dev/null 2>&1 || { echo "  ✗ npm not found (comes with node)"; exit 1; }
	@echo "  npm    $(shell npm --version 2>/dev/null || echo 'missing')"
	@command -v cargo  >/dev/null 2>&1 || { echo "  ✗ cargo not found. Install via rustup: https://rustup.rs"; exit 1; }
	@echo "  cargo  $(shell cargo --version 2>/dev/null | cut -d' ' -f2 || echo 'missing')"
	@command -v sqlite3 >/dev/null 2>&1 || { echo "  ✗ sqlite3 not found. Install via your package manager."; exit 1; }
	@echo "  sqlite3 $(shell sqlite3 --version 2>/dev/null | cut -d' ' -f1 || echo 'missing')"

## Validate Node.js version satisfies >=22.12.0
_check-node-version:
	@echo "── Checking Node.js version ──"
	@NODE_VER=$$(node --version | sed 's/^v//'); \
	MAJOR=$$(echo "$$NODE_VER" | cut -d. -f1); \
	MINOR=$$(echo "$$NODE_VER" | cut -d. -f2); \
	PATCH=$$(echo "$$NODE_VER" | cut -d. -f3); \
	OK=0; \
	if [ "$$MAJOR" -gt $(NODE_MIN_MAJOR) ]; then OK=1; \
	elif [ "$$MAJOR" -eq $(NODE_MIN_MAJOR) ] && [ "$$MINOR" -gt $(NODE_MIN_MINOR) ]; then OK=1; \
	elif [ "$$MAJOR" -eq $(NODE_MIN_MAJOR) ] && [ "$$MINOR" -eq $(NODE_MIN_MINOR) ] && [ "$$PATCH" -ge $(NODE_MIN_PATCH) ]; then OK=1; \
	fi; \
	if [ "$$OK" -ne 1 ]; then \
		echo "  ✗ Node.js $$NODE_VER is too old (need >=$(NODE_MIN_MAJOR).$(NODE_MIN_MINOR).$(NODE_MIN_PATCH))"; \
		echo "    Run: nvm install   (uses .nvmrc → Node $(NODE_MIN_MAJOR))"; \
		echo "    Then: nvm use"; \
		exit 1; \
	fi; \
	echo "  Node.js $$NODE_VER — OK"

## Install UI dependencies
_setup-ui:
	@echo "── Installing UI dependencies ──"
	cd $(UI_DIR) && npm ci

## Create the compile-time SQLite DB for sqlx macros (if missing)
_setup-db:
	@echo "── Initializing sqlx compile-time DB ──"
	@if [ -f $(SQLX_DB) ]; then \
		echo "  $(SQLX_DB) already exists — skipping"; \
	else \
		sqlite3 $(SQLX_DB) < $(SQLX_MIGRATION); \
		echo "  Created $(SQLX_DB)"; \
	fi

# ── Development (native) ─────────────────────────────────────────

.PHONY: build check test clippy fmt fmt-check clean doc

## Build the entire workspace (debug)
build: build-ui
	cargo build --workspace

## Build the entire workspace (release)
release: build-ui
	cargo build --workspace --release

## Type-check without producing binaries
check: build-ui
	cargo check --workspace

## Run all tests
test: build-ui
	cargo test --workspace

## Run clippy lints (deny warnings)
clippy: build-ui
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
build-hub: build-ui
	cargo build -p irrigation-hub

## Build node crate only (debug)
build-node:
	cargo build -p irrigation-node

## Run hub locally
run-hub: build-ui
	cargo run -p irrigation-hub

## Run node locally
run-node:
	cargo run -p irrigation-node

## Test hub crate only
test-hub: build-ui
	cargo test -p irrigation-hub

## Test node crate only
test-node:
	cargo test -p irrigation-node

# ── Cross-compilation ────────────────────────────────────────────

.PHONY: cross-hub cross-node cross-all

## Cross-compile hub for Pi 5 (aarch64) with real GPIO
cross-hub: build-ui
	cross build -p irrigation-hub --release --features gpio --target $(TARGET_HUB)

## Cross-compile hub for Pi 5 with GPIO + native TLS
cross-hub-tls: build-ui
	cross build -p irrigation-hub --release --features gpio,tls --target $(TARGET_HUB)

## Cross-compile node for Pi Zero W (armv6 / armhf) — real ADS1115 sensor backend
cross-node:
	cross build -p irrigation-node --release --no-default-features --features adc --target $(TARGET_NODE)

## Cross-compile everything
cross-all: cross-hub cross-node

# ── Deploy ────────────────────────────────────────────────────────

.PHONY: deploy-hub deploy-node deploy-all

## Deploy hub binary + config to Pi 5 via scp
deploy-hub: cross-hub
	scp target/$(TARGET_HUB)/release/irrigation-hub $(REMOTE_USER)@$(HUB_HOST):~/irrigation-hub
	scp config.toml $(REMOTE_USER)@$(HUB_HOST):~/irrigation/config.toml
	scp deploy/irrigation-hub.service $(REMOTE_USER)@$(HUB_HOST):~/irrigation-hub.service

## Deploy node binary + service to Pi Zero via scp
deploy-node: cross-node
	scp target/$(TARGET_NODE)/release/irrigation-node $(REMOTE_USER)@$(NODE_HOST):~/irrigation-node
	scp deploy/irrigation-node.service $(REMOTE_USER)@$(NODE_HOST):~/irrigation-node.service

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

.PHONY: ci lint preflight

## Full lint pass (clippy + fmt check)
lint: clippy fmt-check

## CI pipeline: fmt, clippy, test
ci: fmt-check clippy test

## Preflight: run all CI checks locally (mirrors .github/workflows/ci.yml)
## Single recipe ensures sequential fail-fast execution and builds UI only once.
preflight: _setup-db
	@echo ""
	@echo "══════════════════════════════════════════════════════"
	@echo "  Preflight — running all CI checks locally"
	@echo "══════════════════════════════════════════════════════"
	@echo ""
	@echo "── [1/5] Building UI ──"
	cd $(UI_DIR) && npm ci && npm run build
	@echo ""
	@echo "── [2/5] Checking formatting ──"
	cargo fmt --all -- --check
	@echo ""
	@echo "── [3/5] Running clippy ──"
	cargo clippy --workspace -- -D warnings
	@echo ""
	@echo "── [4/5] Building workspace ──"
	cargo build --workspace
	@echo ""
	@echo "── [5/5] Running tests ──"
	cargo test --workspace
	@echo ""
	@echo "══════════════════════════════════════════════════════"
	@echo "  ✓ Preflight passed — safe to push"
	@echo "══════════════════════════════════════════════════════"
	@echo ""

# ── Help ──────────────────────────────────────────────────────────

.PHONY: help
help:
	@echo ""
	@echo "  Irrigation System — Make Targets"
	@echo "  ─────────────────────────────────"
	@echo ""
	@echo "  Setup:"
	@echo "    setup        Prepare dev environment (tools, node, npm, sqlx db)"
	@echo ""
	@echo "  Development:"
	@echo "    build        Build workspace (debug)"
	@echo "    release      Build workspace (release)"
	@echo "    build-ui     Build web UI (npm ci + vite build)"
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
	@echo "    preflight     Run full CI pipeline locally (use before pushing)"
	@echo "    lint          clippy + fmt-check"
	@echo "    ci            fmt-check + clippy + test"
	@echo ""
	@echo "  Override remote hosts:"
	@echo "    make deploy-hub HUB_HOST=192.168.1.50"
	@echo "    make deploy-node NODE_HOST=192.168.1.51 REMOTE_USER=admin"
	@echo ""
