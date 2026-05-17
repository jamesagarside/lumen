# Lumen — development guide

Two ways to run Lumen locally: **fast iteration** (recommended for day-to-day work) and **production-like** (Docker, for verifying the container behaves).

## Prerequisites

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.82+ (stable) | `rustup install stable` |
| Node.js | 22+ | Vite requires it |
| Docker | recent | Optional — only for the production-like path |
| Python 3 | any | Optional — for the smoke-test packet generator |
| `cargo-watch` | latest | Optional — daemon auto-reload (`cargo install cargo-watch`) |

## Fast iteration (recommended)

This path skips Docker entirely. The daemon runs natively as a normal `cargo run` process; the UI runs through Vite's dev server with HMR. Vite proxies API and WebSocket calls to the daemon.

**One-time setup:**

```bash
make install   # npm install in ui/
```

**Two terminals.**

Terminal A — the daemon:

```bash
make dev-daemon
# equivalent: cargo run --bin lumen
```

This binds:

- HTTP + WebSocket on `http://localhost:3000` (`/healthz`, `/version`, `/ws/flows`)
- NetFlow v5 ingest on UDP `localhost:2055`

If you have `cargo-watch` installed, the daemon auto-rebuilds on file changes (~2s incremental).

Terminal B — the UI dev server:

```bash
make dev-ui
# equivalent: cd ui && npm run dev
```

Vite serves the UI on `http://localhost:5173` with hot module reload. API calls (`/version`, `/ws/flows`) are proxied to `localhost:3000`.

**Open `http://localhost:5173` in a browser.** You should see the Lumen header showing connection state = `open`.

### Sending test traffic

```bash
make demo
# equivalent: python3 scripts/send_netflow.py 2055
```

Sends one synthetic NetFlow v5 packet containing two flow records. Within a frame you should see them in the UI table and on the graph.

For a continuous stream:

```bash
python3 scripts/send_netflow.py 2055 100  # 100 packets, ~10/sec
make demo-stream                          # ~5 packets/sec, runs forever
```

You can also push flows in directly via HTTP — useful for testing your own collector or piping in data from a non-NetFlow source:

```bash
curl -X POST http://localhost:3000/ingest/flows \
  -H "Content-Type: application/json" \
  -d '[{
    "source": "json_http",
    "src": { "ip": "10.0.0.42", "port": 51234 },
    "dst": { "ip": "1.1.1.1", "port": 443 },
    "protocol": 6,
    "bytes": 2048,
    "packets": 4,
    "start": { "secs_since_epoch": 1700000000, "nanos_since_epoch": 0 },
    "end":   { "secs_since_epoch": 1700000001, "nanos_since_epoch": 0 }
  }]'
```

Set `LUMEN_INGEST_API_KEY=...` to require an `X-Api-Key` header on the endpoint. Unset = open ingestion (fine on localhost or trusted networks).

### What you can iterate on

- **UI changes** — save a `.tsx`/`.ts`/`.css` file in `ui/src/`, browser updates instantly.
- **Daemon changes** — save a `.rs` file in `crates/`, daemon restarts (with `cargo-watch`) or you re-run `make dev-daemon` (without it). Browser auto-reconnects when the daemon comes back.

## Production-like (Docker)

For verifying the container itself works (Dockerfile correctness, env-var defaults, asset embedding, etc.):

```bash
make image     # builds lumen:dev locally — first build ~3min
make up        # docker compose up
```

Open `http://localhost:3000`.

### About the image name

`docker-compose.yml` references `image: lumen:dev` with `pull_policy: never` — the image is built locally only. **It is not published to GHCR or any other registry.** If you try `docker run ghcr.io/jamesagarside/lumen:dev`, it will fail with `denied` because that image doesn't exist yet. Publish-to-GHCR happens once the CI release pipeline is wired up (separate work item).

### Logs

```bash
make logs   # tails the daemon container
```

Container logs are JSON-formatted by default. Pipe through `jq` for readable output:

```bash
docker compose logs -f lumen | jq -r '. | "[\(.level)] \(.fields.message)"'
```

## Validation

Before opening a PR:

```bash
make test    # cargo test --workspace + tsc --noEmit
make lint    # cargo clippy -D warnings + cargo fmt --check + eslint
```

CI runs the same checks plus a Docker build + healthz smoke test on every PR.

## Configuration

All operational config is via environment variables (per `CONTEXT.md` §15):

| Env var | Default | Purpose |
| --- | --- | --- |
| `LUMEN_HTTP_LISTEN` | `0.0.0.0:3000` | HTTP + WebSocket listen address |
| `LUMEN_NETFLOW_V5_LISTEN` | `0.0.0.0:2055` | NetFlow v5 / v9 / IPFIX UDP listen (`off` to disable) |
| `LUMEN_SYSLOG_LISTEN` | (off) | Syslog UDP listen for iptables-LOG style messages (UniFi UDM, OPNsense, pfSense). Set to e.g. `0.0.0.0:5514`. |
| `LUMEN_UI_ASSETS_DIR` | (none) | Static UI directory; set in container, unset in dev |
| `LUMEN_LOG_FORMAT` | auto (json in container, compact in TTY) | `json` or `compact` |
| `RUST_LOG` | `info,lumen_daemon=info` | tracing filter; useful: `lumen=debug` |
| `LUMEN_DATA_DIR` | `./data` | Where the topology DB lives (and future plugin permissions, etc.) |
| `LUMEN_EDGE_MAX_AGE_SECS` | `300` | Stale-edge eviction threshold |
| `LUMEN_EVICTION_INTERVAL_SECS` | `30` | How often the eviction sweep runs |
| `LUMEN_INGEST_API_KEY` | (unset) | Optional shared secret required by `POST /ingest/flows`. Unset = open ingestion. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | (unset) | If set, traces are exported via OTLP/gRPC. The standard `OTEL_*` env vars (service name, headers, etc.) are also honored. |
| `OTEL_SERVICE_NAME` | `lumen` | Service name attached to exported spans. |
| `LUMEN_INITIAL_ADMIN_EMAIL` | (unset) | First-run admin bootstrap. Required alongside `LUMEN_INITIAL_ADMIN_PASSWORD` on the first start, otherwise nobody can log in. |
| `LUMEN_INITIAL_ADMIN_PASSWORD` | (unset) | First-run admin password. After the admin exists this is ignored. |

User-facing settings (device labels, plugin config, role assignments, etc.) live in the topology DB, not env vars. See `CONTEXT.md` §11 and §15.

## Branch and PR conventions

- **Feature branches:** `feat/issue-N-short-name`
- **Stacked PRs:** when a branch builds on another, set the PR base to the parent branch — GitHub auto-rebases when the parent merges. Stack like `main ← scaffolding ← netflow-v5 ← live-state-engine`.
- **One issue per PR.** Reference it in the PR body with `Closes #N`.
- **Commit messages:** imperative, scoped to the change, body explains *why* not *what*. No `Co-Authored-By` lines (per project policy).

## Project layout

```text
.
├── CONTEXT.md           Canonical design spec (always current)
├── DEVELOPMENT.md       This file
├── Cargo.toml           Workspace root
├── crates/
│   ├── lumen-core/      Domain types (Flow, Device, Interface, …)
│   ├── lumen-daemon/    The binary: HTTP server, ingest, plugins, persistence
│   └── lumen-plugin-sdk/ Rust SDK for plugin authors (placeholder until ABI is pinned)
├── ui/                  SolidJS + Vite + Tailwind v4
├── scripts/             Local dev helpers (smoke-test packet generator, etc.)
├── plugins/             Mounted at runtime — `.wasm` files go here (empty in dev)
├── Dockerfile           Multi-stage: node UI → rust daemon → debian-slim runtime
├── docker-compose.yml   Local "production-like" orchestration
├── Makefile             Shortcuts for common commands (`make help`)
└── .github/workflows/   CI
```

## Troubleshooting

**`docker run ghcr.io/jamesagarside/lumen:dev` fails with `denied`**
That image isn't published. Use `make image && make up` instead, or use the fast iteration path.

**`address already in use` when starting the daemon**
Another process is on :3000 or :2055. Override: `LUMEN_HTTP_LISTEN=127.0.0.1:3001 LUMEN_NETFLOW_V5_LISTEN=127.0.0.1:2056 make dev-daemon`.

**Vite shows `WebSocket connection failed` in the browser console**
The daemon isn't running, or it's on a non-default port. Check Terminal A.

**`cargo-watch` not auto-rebuilding**
Run `cargo install cargo-watch` once. Then `make dev-daemon` picks it up automatically.

**Browser shows "0 flows" forever even after `make demo`**
Confirm the daemon log shows `datagram parsed ... flows: 2` (set `RUST_LOG=lumen=debug`). If yes, the issue is the WebSocket — check the browser network tab for a 101 upgrade.
