# CLAUDE.md

Guidance for working in this repo (a Rust SpaceTraders trading agent).

## What this is

An autonomous agent that plays SpaceTraders: it mines/trades to fund building its
home jump gate, then charts the gate network and trades the highest-value systems.
Single binary (`src/bin/main.rs`), Postgres-backed, deployed to Kubernetes via Helm.

## Build, run, deploy

- **Check/build**: `cargo check` (fast), `cargo build`.
- **Local run**: needs a `.env` (Postgres URI, agent token, schema, callsign). DB is
  reached via `kubectl port-forward` (see `dev.sh`). Scope to one ship with
  `JOB_ID_FILTER=^t5_trader/1$`. Don't run locally against a schema a deployed agent
  is already driving — they share the token/DB and will conflict.
- **Deploy** (see [docs Architecture → Deploy](docs/src/architecture.md)):
  1. Bump `Cargo.toml` + `helm/spacetraders/Chart.yaml` (`cargo update -p st --precise <v>`).
  2. `./publish.sh` (builds + pushes the image; tag = Cargo version).
  3. `helm upgrade tst-4381 ./helm/spacetraders --kube-context=jpa-dev -n spacetraders --reuse-values --set image.tag=<v>`.
  - A new version is required to roll code (`pullPolicy: IfNotPresent`). Per-agent
    values (incl. secrets, `SCRAP_UNASSIGNED`, etc.) live in the separate
    `~/jpa-dev-deployment` repo, not here.

## Gotchas

- **A panic in any ship script crashes the whole agent** (it propagates through
  `join_handles` and the process exits → pod restarts). Avoid panics on recoverable
  conditions; prefer logging + a graceful exit/return.
- Watch out for stale cached waypoint/market data (`get_system_waypoints` only
  refetches when details are missing — see Universe & Market Data).

## Knowledge base — keep it in sync

Project docs are an mdBook under `docs/` (`mdbook serve docs --open`). **When you
change a subsystem, update its chapter** (`docs/src/`). The code-reference table at
the bottom of each chapter maps to the relevant `file:symbol`. Rough file → chapter map:

| if you touch… | review chapter |
|---|---|
| `agent_controller/{agent_controller,fleet}.rs`, `bin/main.rs`, `ledger.rs`, `join_handles.rs` | `eras-lifecycle.md` |
| `universe/mod.rs`, `database/mod.rs`, `models/market.rs`, schema | `universe-data.md` |
| `pathfinding.rs`, `universe/pathfinding.rs` | `pathfinding.md` |
| `logistics_planner/`, `tasks.rs`, `ship_scripts/logistics.rs` | `logistics-planner.md` |
| `ship_scripts/{probe,probe_exploration}.rs`, `agent_controller/exploration.rs` | `exploration.md` |
| `ship_scripts/{mining,siphon}.rs`, `survey_manager.rs`, `broker.rs` | `mining-siphon.md` |
| `ship_scripts/construction.rs`, `is_jumpgate_finished` | `gate-construction.md` |
| `agent_controller/contract_manager.rs`, `models/contract.rs` | `contracts.md` |
| `models/system.rs` (`p_t5`), `ship_scripts/t5_trader.rs`, t5 bits of `fleet.rs`/`pathfinding.rs` | `t5-trading.md` |

If a change spans subsystems or adds a new one, update `docs/src/SUMMARY.md` too.
Generated `docs/book/` is gitignored.
