# Lumen — Design Context

This document is the canonical reference for what Lumen is, how it's shaped, and why each decision was made. It exists for human contributors and for AI assistants working in this codebase. If a decision below conflicts with code, the code is wrong (or this doc is stale — fix one of them).

Decisions here were locked in a design grilling session and should be treated as durable contracts. Reopening any of them requires explicit justification, not drift.

---

## 1. Identity & positioning

- **Name:** Lumen
- **License:** AGPL-3.0
- **Audience:** Homelab / prosumer (primary), small enterprise (stretch). Not a SIEM. Not an enterprise platform.
- **Sibling project:** [Helios](https://github.com/jamesagarside/helios) — shares design language and design tokens, different domain.
- **One-line positioning:** Real-time network visualization that turns NetFlow/IPFIX/syslog into a live, scrubbable map. Sits alongside a SIEM, never replaces one.
- **The hero screenshot (north star):** An animated force-directed network graph where every external service is a recognizable branded cluster (Netflix, Discord, GitHub, with logos), every internal device has the user's chosen name, edges visibly pulse and flow in the direction of current bytes, and a DAW-style time scrubber sits at the bottom — drag it back 8 minutes and watch the network *replay* that moment.

Every v1 design decision should be measurable against "does this make that screenshot better, or is it a distraction?"

---

## 2. Architecture

### Shape

- **Headless Rust collector daemon + thin SolidJS web UI**, served by the same binary.
- **Distribution:** Docker image primary; single static binary secondary.
- **Bootstrap:** `docker compose up`. Plugins via mounted `./plugins` volume. Operational config via env vars + mounted YAML.

### Why a daemon, not a desktop app

NetFlow only makes sense as a long-lived collector. A desktop app that's only running when the user is looking at it misses the interesting traffic (3am scan, the brief weird burst). A web UI also yields "view from phone, view from laptop, view from anywhere on the LAN" essentially for free.

### Scale targets

| Tier | Hosts | Flows/sec | Notes |
| --- | --- | --- | --- |
| **A — Homelab/prosumer** (bullseye) | 1–500 | <5,000 | Runs comfortably in 1 GB RAM on a Pi 5 / NAS. |
| **B — Small enterprise** (stretch) | 500–10,000 | 5,000–50,000 | 4–16 GB RAM, single VM or k8s pod. Requires LOD/clustering in the graph view. |
| **C — Enterprise** | — | — | **Out of scope.** Multi-site, RBAC-everywhere, sharded collectors, HA — different product. |

---

## 3. Data model

### Retention tiers

| Tier | Storage | Purpose | Lifetime |
| --- | --- | --- | --- |
| **Raw flows** | RAM ring buffer | Real-time UI updates, time scrub within window | `min(15min, 512MB)` defaults; both configurable via env vars |
| **Aggregated rollups** | DuckDB, tiered | Historical query, anomaly baselines | per-tuple `(src,dst,dport,proto)` @ 1s buckets / 1 hour; per-edge `(src,dst)` @ 1m buckets / 7 days; per-host-pair @ 1h buckets / 30 days |
| **Topology** | redb (embedded KV) | Devices, interfaces, user labels, integration context | Persistent. Snapshot on shutdown + every N minutes. |
| **Detection events** | DuckDB (separate table) | Audit trail, scrub-back-to-incident | Indefinite. |

**Lumen never writes a raw flow record to disk.** The user's SIEM is the system of record for raw flows; Lumen keeps a fast operational summary.

### Entity model

- **Node = network interface** (identified by MAC). One MAC = one node in the data layer.
- **Device = group of interfaces** (multi-homed hosts: server with eth0+eth1; laptop wired+wireless = same device, two MACs). Grouping comes from integration data (UniFi knows which MACs belong to one Client) or user declaration.
- **External entities = ASN/brand clusters.** External IPs don't have introspectable interfaces, so the symmetry breaks here cleanly. Branded cluster (Netflix, Cloudflare) when known, ASN cluster when not, individual IPs on demand.

### Identity resolution

- **Internal:** MAC + integration overlay (UniFi-provided name) + user-editable labels (persisted in topology DB). User-editable labels are the moment the tool becomes *theirs*.
- **External:** ASN + curated brand mapping (~500 entries bundled, MaxMind GeoLite2-ASN for ASN data); falls back to ASN org name → reverse DNS → bare IP. Curated brand mapping is community-extensible.
- **DPI:** integrations contribute application-level identity (`{application: "Netflix", category: "streaming"}`) via the `enrich_flow` plugin capability.

---

## 4. Ingestion

Lumen ingests flow data from many sources, normalizes to a common internal `Flow` type, and feeds the live state machine.

| Protocol | v1 | Notes |
| --- | --- | --- |
| NetFlow v5 | ✅ | UDP listener, fixed schema |
| NetFlow v9 | ✅ | UDP listener, template-based |
| IPFIX | ✅ | UDP listener, template-based; shares parser with v9 |
| sFlow | ❌ | Out of v1 (mostly enterprise switch gear) |
| Syslog (UniFi format) | ✅ | UDP/TCP listener |
| Syslog (OPNsense, pfSense, Fortinet, Palo Alto) | follow-up | Community contributions or fast-follow |
| **Open JSON-over-HTTP** | ✅ | Documented endpoint where any collector can POST pre-parsed flows. The escape hatch for niche sources. |
| **OTLP (gRPC + HTTP)** | ✅ | Speak OpenTelemetry Collector's language. Users with an existing OTel pipeline (NetFlow receiver, eBPF flow generator, etc.) can point it at Lumen with no code changes. |

**Lumen does not embed an OTel Collector.** It speaks OTLP natively so users can run their own collector externally if they want one. This avoids bundling a Go runtime and keeps Lumen pure-Rust.

---

## 5. Plugin system

### Runtime

- **WASM via Extism / wasmtime.** Plugins are language-agnostic (Rust, Go, JS, Python, Zig — anything that compiles to WASM).
- **Sandboxed by default.** No filesystem, no network, no secrets unless granted.
- **Manifest-declared permissions.** Plugin ships a manifest declaring what it needs (`network: ["https://unifi.local"]`, `secrets: ["UNIFI_API_KEY"]`); user grants on install, browser-extension-style. Default-deny, no ambient authority.

### Capabilities

| Capability | v1 | Description |
| --- | --- | --- |
| `discover_devices()` | ✅ | Return inventory: MAC, IP, hostname, vendor, role, VLAN |
| `enrich_flow(flow)` | ✅ | Annotate flow with extra context: device, application, category, confidence. DPI lives here. |
| `query_topology()` | ✅ | Describe physical/logical topology (this AP is on this switch) — powers Architecture view |
| `subscribe_detections()` | ✅ | Push stream of detection events into Lumen (UniFi IPS, Suricata, etc.) |
| `consume_detections(event)` | ✅ | Receive detection events for forwarding (Slack/Discord/webhook plugins) |
| `subscribe_events()` | stretch | Generic network events (client connected, AP roam) |
| `actions()` | **never v1** | Plugins that *do* things to the network (block client, isolate VLAN). Security boundary that can't be unshipped — needs separate design phase. |

### Distribution

- **v1:** plain `plugins/*.wasm` directory. Users curl/download from plugin repos themselves.
- **v2 (when ≥5 integrations exist):** registry / marketplace UI. Premature marketplace = empty marketplace.

### Launch plugins shipped by us

- **UniFi** (provider): `discover_devices` + `enrich_flow` (with UniFi DPI) + `query_topology` + `subscribe_detections` (UniFi IPS)
- **Generic webhook** (consumer)
- **Slack** (consumer)
- **Discord** (consumer)

Dogfooding: shipping detection-consumer plugins as official-but-pluggable validates the WASM architecture before external authors stress it.

### ABI commitment

The plugin schema (function signatures, message types, manifest format) becomes a backwards-compatibility contract once published as `v1`. Treat the schema PR as the highest-stakes design review of the project.

---

## 6. Detection events

**Lumen detects nothing itself.** All detections come from provider plugins. This is the cleanest separation between visualization (us) and detection logic (specialist tools).

### Schema

Use **OpenTelemetry semantic conventions (ECS lineage)** — vendor-neutral OTel standard since [the ECS↔OTel merge](https://www.elastic.co/blog/ecs-elastic-common-schema-otel-opentelemetry-merge). Same vocabulary as our OTLP ingestion. Mappable to OCSF for downstream consumers who want it.

```json
{
  "@timestamp": "2026-05-15T14:32:11.123Z",
  "event.kind": "alert",
  "event.category": ["intrusion_detection"],
  "event.type": ["denied"],
  "event.severity": 5,
  "event.action": "blocked",
  "rule.name": "...", "rule.id": "...", "rule.description": "...",
  "source.ip": "...", "source.mac": "...",
  "destination.ip": "...", "destination.port": 443,
  "network.protocol": "tcp", "network.bytes": 4096,
  "host.name": "Sonos Living Room",
  "threat.indicator": {},
  "threat.tactic": {},
  "agent.type": "unifi-ips",
  "url.original": "https://unifi.local/protect/alerts/..."
}
```

### Visual overlay rules

All detection sources render identically — consistency comes from the schema, not from per-source code.

| Severity | Color |
| --- | --- |
| info | neutral |
| low | blue |
| medium | amber |
| high | orange |
| critical | red |

- Affected nodes get a halo + badge.
- Affected edges get a highlight stroke.
- Mark on the time scrubber so you can scrub *to* the moment of detection.
- Sidebar event list, sortable/filterable.
- Click → detail panel with full event + back-link to source system.
- **No toasts. No interrupts.** Passive surfaces only — users who want push notifications subscribe via consumer plugins.

---

## 7. Front-end

| Layer | Choice | Reason |
| --- | --- | --- |
| Framework | **SolidJS** | Fine-grained reactivity, ~7KB runtime, signals map perfectly onto streaming flow updates. No VDOM overhead at high update rates. |
| Graph rendering | **Sigma.js v3** (WebGL) | Mature, focused, custom GLSL programs for animated edges. Right scale for A+B targets. |
| Styling | **Tailwind v4** + CSS custom properties from design tokens package | |
| Design tokens | `@jamesagarside/design-tokens` (separate repo) | Shared with Helios. **Side quest: extract from Helios first as validation, then both projects consume.** Tokens only — no shared components. |

### Wire protocol

- **WebSocket binary** (postcard or similar Rust→TS serializer). At 50k flows/sec aggregated to 10Hz deltas, JSON would be 3× the bytes for no benefit.
- **Snapshot + deltas.** Initial snapshot on connect; delta frames at 10Hz thereafter. Browser maintains a mirror of the graph state.
- **Backpressure handling:** drop frames on slow clients, rebase via fresh snapshot when client catches up. Per-client diff state derived from a stateless `(snapshot_t, snapshot_t+1) → delta` function so the diffing scales independently of client count.
- **Server-Sent Events as future fallback** for hostile-proxy environments. Not v1.

---

## 8. Visualization

### Three views, switchable, with shared state

- **Graph view (default)** — force-directed with semantic anchors: gateway pinned (top), internal devices clustered center, external services arc around the perimeter. The "alive, beautiful, organic" view. Default home.
- **VLAN view** — same nodes regrouped into VLAN/subnet bounded regions. The ops-focused "where do these things sit on my network" view.
- **Architecture view** — hierarchical: gateway → switches → APs → hosts. Only fully realized when an integration provides authoritative topology. Degrades gracefully when topology data is missing (marks unknown placement explicitly).

### View switcher

- **Keyboard primary** (`⌘1` / `⌘2` / `⌘3`) + small persistent toggle in corner.
- Command palette (`⌘K`) is the universal action surface (search devices, switch view, jump to time, run NLQ).
- Cambridge Intelligence aesthetic = minimal chrome, maximum graph.

### Cross-view state

| State | Behavior |
| --- | --- |
| Selection (nodes/edges) | **Carries** across views |
| Scrub time | **Carries** across views |
| Active filters | **Carries** across views |
| Camera position / zoom | **Resets** per view (the spaces are fundamentally different) |
| Per-view expand/collapse | Local to view |

### View-appropriate filtering

Each view filters to entities that make sense (Architecture view doesn't show external services in its tree). A "show all entities" toggle overrides the natural filter as an escape hatch.

### Edges

- **Asymmetric two-line representation:** one line per direction, thickness ∝ bandwidth that direction. Animated dashed motion in flow direction. Shows asymmetry (uploads vs downloads) immediately.
- **Color ∝ application category** when DPI data is available (streaming/sync/security/unknown). Independent of thickness.
- **Merged by default** for parallel edges (same A↔B, different ports/protos); **splayed on selection** to show breakdown.

### Position persistence

- **Persist all positions** to topology DB. Nodes stay where the user dragged them.
- New nodes settle into available space via a *localized* force simulation on their immediate neighborhood — never a global re-layout that disturbs the user's mental map.
- "Re-layout everything" button available for users who want a fresh organic arrangement.

### External service rendering

- Branded clusters with logos (Netflix, Cloudflare, Apple iCloud, GitHub, etc.).
- ASN cluster fallback for unknowns: `AS-12345 (some-hosting.example.com)`.
- "Expand to see individual IPs in this ASN" available on click.

---

## 9. Time as a UI dimension

### v1 scope

- **Live + rewind** within the rolling raw window (15 min default, configurable via env).
- **Historical query** (DuckDB rollups) is a fast-follow, not v1. Rollup pyramid is being written from day one so the data is already there when the historical UI ships.

### Scrubber behavior

- Always visible at bottom of canvas. Parked at "now" by default.
- **Soft scrub** — drag off "now" to rewind, drag back to resume live. No mode toggle. DAW-style.
- **Looping playback at playhead** — scrubbing to T-8min plays a 5-second window centered on T-8min on repeat, so the user sees the *motion* of that moment, not a still frame. The motion is the point.
- Inspector panel always reflects the playhead moment, not real-time.
- When the scrubber crosses out of the raw window (into rollup-only territory in v2), it visually changes appearance to signal "you're now seeing aggregates, not motion."

---

## 10. NLQ (natural language query)

**Opt-in. Off by default.** Behind a config flag. A NetFlow visualizer that *requires* an LLM is a bad NetFlow visualizer.

### Scope ordering

- **v1:** text → visualization manipulation. Structured action vocabulary (`filter_edges`, `highlight_nodes`, `set_time_range`, `run_query`) — LLM emits structured calls, never freeform JS. Killer move: "I described what I want to see and the graph rearranged itself."
- **v2:** agentic investigation (multi-step queries, correlation, narrative findings).
- **Never:** LLM with `actions()` capability access.

### LLM choice

- **BYO API key** for top commercial models (Anthropic, OpenAI, etc.) — ~100 LOC.
- **Local model support** (Ollama / llama.cpp) for the privacy-conscious.
- Both shipped, user picks. **No bundled keys. No proxying through anyone's servers.**

---

## 11. Auth & RBAC

### What we ship

- **Local users** — Argon2id password hashing, bootstrap admin via env var (`PROJECT_VIEW_INITIAL_ADMIN_EMAIL=...`), invite/create more via UI.
- **OIDC** — `openidconnect` crate, configure issuer/client_id/client_secret via env. Group claims map to local roles via configurable mapping. Compatible with Keycloak, Authentik, UniFi Identity, Auth0, Google Workspace, Azure AD, Okta.
- **Reverse-proxy header trust** as opt-in (default disabled), CIDR-restricted allowlist. For users running oauth2-proxy/Authelia in front.
- **Capability-based authz under role-based UI:** ~15 capability constants in code, roles are bundles of capabilities, custom roles editable in v1.

### Default roles

| Role | Capabilities |
| --- | --- |
| **Admin** | All |
| **Operator** | View + edit labels + acknowledge detections + run NLQ |
| **Viewer** | Read-only graph + detections + topology |
| **NOC Display** | View graph + view detections only. Paired with **kiosk mode UI flag** that strips chrome to bare graph + scrubber + alerts sidebar. |

### Audit log

- Populated to DuckDB from day one (mutations only; reads not logged unless someone asks).
- **UI page deferred to v2** — power users can query DuckDB directly meanwhile.

### Out of scope

SAML, LDAP, Kerberos, per-VLAN scoping, password reset flow (admin resets in v1).

---

## 12. Self-observability

- **Structured logging** via `tracing` + `tracing-opentelemetry` layer. JSON output by default in container. Levels honored via `RUST_LOG` env var. OTLP export available for users with an OTel pipeline.
- **OpenMetrics endpoint** at `/metrics`: ingestion rate, RAM usage, plugin health, WS client count, detection event rate.
- Every meaningful operation gets a span. Every error gets context. No `println!` anywhere.

---

## 13. Versioning & releases

- Semver. GitHub Releases. Docker images tagged `:latest` / `:1` / `:1.x` / `:1.x.y`.
- Plugin ABI version separate from app version, advertised in manifest.
- Boring is correct.

---

## 14. Critical sequencing (do these *before* writing v1 code)

1. **Extract `@jamesagarside/design-tokens`** from Helios; migrate Helios to consume it. Validates the abstraction. ~1 weekend.
2. **Pin the WASM plugin ABI.** Once external authors write against it, breakage damages the ecosystem. Treat the schema PR as the highest-stakes design review of the project.
3. **Write the UniFi plugin first.** Before the core daemon is feature-complete. Using a plugin as the launch integration is *only* valid if writing one is humane. Friction here = ABI redesign signal.
4. **Build the empty-state UX before the live UX.** First impression for every new install is "I just ran docker compose up and the graph is empty." That moment needs a real design pass: walkthrough card, "no flows received yet" diagnostic, "configure your exporter" guide.
5. **Validate the onboarding happy-path end-to-end on real hardware.** Container up → UniFi pointed at port 2055 → UniFi plugin installed → controller credentials configured → first device named. If any step has a 5-min pause, it's a v1 blocker.

---

## 15. Operating principles

- **Stream through, don't store.** Lumen is operational visibility; the SIEM is the system of record. Never duplicate.
- **Beautiful at rest, beautiful in motion.** Static screenshots and live demos must both work.
- **Keyboard-first, mouse-friendly.** Power users live in `⌘K`; new users discover via visible affordances.
- **Plugins for everything that's not visualization.** The core does graph + time + UI. Everything else is a plugin.
- **No feature without a screenshot.** If a feature can't be photographed, it shouldn't ship in v1.
