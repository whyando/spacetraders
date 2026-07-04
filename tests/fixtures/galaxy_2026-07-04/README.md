# Galaxy snapshot — 2026-07-04

Frozen snapshot of the live agent's universe, used by the offline exploration
simulator (`src/sim/`, `cargo run --bin explore_sim`).

- `universe.json` — `/api/universe` dump: 6,996 systems (symbol, x, y, type,
  has_gate, gate_charted) + 6,827 undirected charted gate edges. Source for the
  Scenario 1 (gate-network discovery) gate graph.
- `waypoints_gatesystems.ndjson` — one JSON object per gate-bearing system:
  `{system, x, y, waypoints: [[symbol, type, x, y], ...]}`. 3,101 systems,
  157,307 waypoints. Source for Scenario 2 (charting throughput).
- `waypoints_sample25.ndjson` — first 25 systems, for fast unit tests.

Pulled from Postgres schema `tst4382_20260628` (reset 2026-06-28) via the
`dev.sh` port-forward. Edges reflect the agent's charting state at pull time
(~90% of gates charted); uncharted gates appear as edge-less leaves.
