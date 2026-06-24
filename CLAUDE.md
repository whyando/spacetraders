# CLAUDE.md

Orientation for working in this repo — a Rust SpaceTraders trading agent.

## What this is

An autonomous agent that plays SpaceTraders: it mines/trades to fund building its
home jump gate, then charts the gate network and trades the highest-value ("T5")
systems. Single binary (`src/bin/main.rs`), Postgres-backed, deployed to Kubernetes
via Helm. It also serves a read-only JSON API that powers a separate dashboard.

## Related repos (all under `~`)

- **`~/spacetraders`** — this repo (the agent).
- **`~/jpa-dev-deployment`** — Helm deploy config and **per-agent values, including
  secrets** (`POSTGRES_URI`, account token, `SCRAP_UNASSIGNED`, image tag). This is
  the canonical place for env/secrets — *not* this repo. The live agent is
  `agents/TST-agent.yaml`. Local-only git repo (no remote).
- **`~/spacetraders-dashboard`** — React + TypeScript + Vite SPA, deployed to
  Cloudflare Pages at <https://spacetraders.whyando.com>. Reads the agent's read-only
  API; `npm run dev` to develop, `VITE_API_BASE` to point at another API.

## Build / run / deploy

- **Check/build/test**: `cargo check` (fast), `cargo build`, `cargo test --lib`.
- **Local run**: needs a `.env` (Postgres URI, agent token, schema, callsign). Reach
  the DB via `kubectl port-forward` (see `dev.sh`). Scope to one ship with
  `JOB_ID_FILTER=^t5_trader/1$` (it gates *buying* too, not just which scripts run).
  Don't run against a schema a deployed agent is already driving — they share the
  token/DB and conflict; scale the live agent to 0 first if testing as the same
  callsign.
- **Deploy** (details in docs → Architecture → Deploy):
  1. Bump `Cargo.toml` + `helm/spacetraders/Chart.yaml` (`cargo update -p st --precise <v>`).
  2. `./publish.sh` (builds + pushes the image; tag = Cargo version).
  3. `helm upgrade tst-4381 ./helm/spacetraders --kube-context=jpa-dev -n spacetraders --reuse-values --set image.tag=<v>`.
  - A new version is required to roll code (`pullPolicy: IfNotPresent`). Secrets/env
    live in `~/jpa-dev-deployment`, applied via `--reuse-values`.

## Live environment & observability

- Release **`tst-4381`**, namespace **`spacetraders`**, kube context **`jpa-dev`**.
  Callsign **`TST-4382`**, schema **`tst4382_{RESET_DATE}`**.
- **Read-only API** (served by the agent, `src/web/mod.rs`, `WEB_PORT` 8080) at
  **<https://api.spacetraders.whyando.com>**, no auth — the quickest way to inspect
  the live agent without kubectl:
  `/api/agent`, `/api/ships` (nav + per-ship net cash), `/api/history`,
  `/api/construction`, `/api/universe`, `/api/systems`,
  `/api/systems/{system}/markets`, `/api/markets/{waypoint}`.
  e.g. `curl -s https://api.spacetraders.whyando.com/api/ships | jq '.[] | select(.symbol=="TST-4382-56")'`
- **Logs**: `kubectl --context=jpa-dev -n spacetraders logs deploy/tst-4381-spacetraders --tail=2000`
  (add `--previous` to read the pre-crash container after a restart).
- The public SpaceTraders API itself (`https://api.spacetraders.io/v2`) serves system
  waypoints/traits without auth — handy for cross-checking what the agent believes vs.
  reality (full schema at `/v2/documentation/json`).

## Gotchas

- **A panic in any ship script crashes the whole agent** — it propagates through
  `join_handles`, the process exits, and Kubernetes restarts the pod. Avoid panics on
  recoverable conditions; prefer logging + a graceful return/exit.
- Cached waypoint/market data can be **stale**: `get_system_waypoints` only refetches
  when details are missing (see Universe & Market Data chapter).

## Keep the knowledge base in sync

Project docs are an mdBook under `docs/` (`mdbook serve docs --open`). **When you
change a subsystem, update its chapter** (`docs/src/`). Each chapter ends with a
`file:symbol` reference table. Document how it works *now* — don't accumulate
fixed-bug history. Rough file → chapter map:

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
| `web/mod.rs` (API), or the dashboard SPA | `architecture.md` (Web API + dashboard) |

A new or cross-cutting subsystem → also update `docs/src/SUMMARY.md`. Generated
`docs/book/` is gitignored.
