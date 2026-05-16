# Lumen — common dev commands.
# See DEVELOPMENT.md for the full guide.

.PHONY: help dev-daemon dev-ui demo install build test lint fmt fix \
        image up down logs clean

UI_DIR := ui

help:
	@echo "Lumen dev commands:"
	@echo ""
	@echo "  Fast iteration (run in two terminals):"
	@echo "    make dev-daemon       Run the Rust daemon on :3000 + UDP :2055"
	@echo "    make dev-ui           Run the Vite UI dev server on :5173"
	@echo ""
	@echo "  Smoke test:"
	@echo "    make demo             Send a synthetic NetFlow v5 packet"
	@echo ""
	@echo "  Validate:"
	@echo "    make test             cargo test + tsc --noEmit"
	@echo "    make lint             cargo clippy + cargo fmt --check + eslint"
	@echo "    make fix              cargo fmt + cargo clippy --fix"
	@echo ""
	@echo "  Production-like (Docker):"
	@echo "    make image            Build the Docker image"
	@echo "    make up               docker compose up"
	@echo "    make down             docker compose down"
	@echo "    make logs             tail container logs"
	@echo ""
	@echo "  Misc:"
	@echo "    make install          Install UI npm deps"
	@echo "    make build            Release build (rust + ui)"
	@echo "    make clean            Wipe build artifacts"

# ─── Fast iteration ──────────────────────────────────────────────────────────

# Run in terminal A. Auto-reloads with cargo-watch if installed.
dev-daemon:
	@if command -v cargo-watch >/dev/null 2>&1; then \
		cargo watch -q -x 'run --bin lumen'; \
	else \
		echo "tip: cargo install cargo-watch  for auto-reload"; \
		cargo run --bin lumen; \
	fi

# Run in terminal B.
dev-ui:
	cd $(UI_DIR) && npm run dev

# Send a synthetic NetFlow v5 packet to the local daemon.
demo:
	python3 scripts/send_netflow.py 2055

# ─── Validate ────────────────────────────────────────────────────────────────

test:
	cargo test --workspace --all-targets
	cd $(UI_DIR) && npm run typecheck

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check
	cd $(UI_DIR) && npm run lint

fmt:
	cargo fmt --all

fix:
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# ─── Build ───────────────────────────────────────────────────────────────────

install:
	cd $(UI_DIR) && npm install

build:
	cargo build --release
	cd $(UI_DIR) && npm run build

# ─── Docker ──────────────────────────────────────────────────────────────────

image:
	docker compose build

up:
	docker compose up

down:
	docker compose down

logs:
	docker compose logs -f lumen

# ─── Cleanup ─────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf $(UI_DIR)/dist $(UI_DIR)/node_modules
