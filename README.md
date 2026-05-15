<p align="center">
  <strong>Lumen</strong><br/>
  <sub>Real-time network visualization for the rest of us.</sub>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square" alt="License"/>
  <img src="https://img.shields.io/badge/status-design--phase-orange?style=flat-square" alt="Status"/>
</p>

---

Lumen is an open-source network visualization tool that turns NetFlow, IPFIX, syslog connection logs, and OpenTelemetry flow data into a live, scrubbable map of your network. Internal devices appear with the names you give them; external services cluster as recognizable brands; edges pulse with current bandwidth. A DAW-style time scrubber lets you replay any moment from the rolling window.

Lumen sits alongside your SIEM, not in place of it. It never persists raw flow records — only aggregated rollups and the topology you care about. Detections come from integrations (UniFi IPS, Suricata, etc.) via a typed plugin schema, surfaced as overlays on the graph.

> **Status: Design phase.** This repo currently contains the locked design specification ([CONTEXT.md](./CONTEXT.md)). Implementation is being scoped into GitHub issues.

## Why Lumen?

- **Real-time, not retrospective.** Traffic appears as it happens; topology is live state, not a query result.
- **Built for homelab → small enterprise.** Not a SIEM, not an enterprise platform. The audience that nobody else makes beautiful tools for.
- **Plugin-first integrations.** WASM-sandboxed plugins for device discovery, DPI enrichment, topology, and detection events. Launch integration: UniFi.
- **Your data stays local.** Single Docker container. No accounts, no cloud, no telemetry phoning home.
- **OpenTelemetry-native.** Speaks OTLP; emits ECS-compatible detection events; exports its own metrics/traces via OTel.

## Family

Lumen is part of the [@jamesagarside](https://github.com/jamesagarside) software family alongside [Helios](https://github.com/jamesagarside/helios). Shared design tokens, shared sensibility, different domains.

## License

[AGPL-3.0](./LICENSE). Use it, extend it, run it for yourself, run it for your team — but if you offer it as a service, contribute your changes back.
