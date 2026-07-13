# Universe & Market Data

The `Universe` (`src/universe/mod.rs`) is the agent's world model: every system,
waypoint, market, shipyard, jump gate, and construction site. It's read constantly by
the planner and ship scripts, so almost everything is cached behind a consistent
**three-layer pattern**.

## Three-layer cache

Most getters resolve in this order:

1. **In-memory** — `DashMap`s on the `Universe` (`systems`, `remote_markets`,
   `remote_shipyards`, `markets`, `shipyards`, `jumpgates`, `constructions`,
   `factions`) plus two `moka` caches for the computed graphs (see
   [Pathfinding](pathfinding.md)). O(1), no await.
2. **Database** — Postgres (via a 32-connection async pool). Tables persist
   everything so a restart re-hydrates the in-memory layer without re-fetching the
   galaxy.
3. **API** — the SpaceTraders server, authoritative and rate-limited. Hit only on a
   miss; results are written back to layers 2 and 1.

## Galaxy bootstrap

The galaxy is loaded once per reset, in the background, so the home economy can run
while it loads:

- `spawn_galaxy_load` (from `bin/main.rs`): `load_all_systems` bulk-fetches `/systems`
  (waypoint **types + coordinates** only, no traits) and inserts systems + waypoints;
  `load_gate_waypoints` then fetches full waypoint details (traits) for every system
  that has a jump gate.
- Guarded by `galaxy_loaded` / `gate_waypoints_loaded` markers in `generic_lookup`,
  so it runs once and later restarts load from the DB.
- `systems_ready` (a `watch`) flips true when done; full-galaxy consumers
  (`jumpgate_graph`, `warp_jump_graph`) call `await_systems_loaded()` first.
- `spawn_construction_load` (also from `bin/main.rs`): a separate one-time task,
  `gate_construction_loaded`-marker-guarded, that fetches + persists the construction
  site for every gate flagged under-construction, then invalidates `jumpgate_graph`. It
  is deliberately **not** gated on `systems_ready` — the graph build awaits that barrier
  under the `try_buy_ships` lock, so folding this multi-minute sweep into it would
  re-create the lock-timeout crash. The build itself reads construction status cache/DB
  only (`construction_cached`), never the API.

## Waypoint details & the staleness gotcha

`get_system_waypoints` returns cached details and only hits the API when **some**
waypoint in the system has no details yet. That's efficient, but it means a system
whose details were cached early (e.g. at `load_gate_waypoints` time, when its markets
were still uncharted) is **never refetched** and looks market-less forever.

- `refresh_system_waypoints` — force re-fetch and **overwrite** cached details. This
  is what the t5 traders call on arrival to discover markets charted after our
  snapshot. See [T5 Trading → stale traits](t5-trading.md).
- `discover_system_markets` — learn a system's markets/shipyards even while their
  waypoints are still **UNCHARTED**. A trait-filtered query
  (`get_system_waypoints_with_trait`, e.g. `?traits=MARKETPLACE`) matches on the real
  trait server-side, so it returns still-uncharted markets (their per-object traits
  stay hidden — rely on list membership); the flags are OR-ed in via
  `note_waypoint_traits`. This is how a t5 trader bootstraps a never-explored system.
- `ingest_scanned_waypoints` — merge details from a sensor scan.
- `note_waypoint_traits` — after a successful `refresh_market`/`refresh_shipyard`,
  OR-in the proven trait (so a learned market isn't "unlearned" on reload).
- `is_uncharted()` — read the cached uncharted flag for one waypoint (unknown → false).
  `refresh_market` uses it to chart a market on first visit.
- `is_market()` treats every `JUMP_GATE` as a market in addition to the
  `MARKETPLACE` trait.

## Markets: remote vs full

Two views (`src/models/market.rs`):

- **Remote** (`MarketRemoteView`) — imports/exports/exchange lists (and, for
  shipyards, ship types). **No prices.** Available from the API without a ship
  present; `get_market_remote` / `get_shipyard_remote` fetch + cache it.
- **Full** (`Market`) — adds per-good `trade_goods` (supply, prices, trade volume).
  Only obtained by a ship **at** the market via `refresh_market`. `get_market` is a
  cache-only lookup (no API fallback).

Price history is logged to two TimescaleDB hypertables: `market_trades` (a row only
when a good's supply/price *changes* — deduped) and `market_observations` (a row per
sample, even with no change). The planner reads current cached prices with **no
age check** — keeping markets fresh is the probes'/refresh-tasks' job (see
[Logistics Planner](logistics-planner.md)).

## Construction & jump gates

- `get_construction` caches a waypoint's construction site (used to decide gate
  completion — see [Gate Construction](gate-construction.md)); progress is logged to
  `construction_log`.
- `get_jumpgate_connections` caches a gate's charted connections and **invalidates
  the jump-gate graph** so the frontier widens immediately (see
  [Pathfinding](pathfinding.md)).

## Key DB tables

`generic_lookup` (key/JSON singletons: `galaxy_loaded`, reservations, era/ledger
state, …), `systems`, `waypoints`, `waypoint_details`, `jumpgate_connections`,
`remote_markets`, `remote_shipyards`, `markets`, `shipyards`, and the hypertables
`market_trades`, `market_observations`, `construction_log`. Schema:
`spacetraders_schema.sql.template` (applied idempotently at startup with
`CREATE TABLE IF NOT EXISTS`).

## Key code references

| concern | location |
|---|---|
| caches + bootstrap | `src/universe/mod.rs` — `Universe`, `spawn_galaxy_load`, `spawn_construction_load`, `load_all_systems`, `load_gate_waypoints`, `await_systems_loaded`, `construction_cached` |
| waypoint details | `src/universe/mod.rs` — `get_system_waypoints`, `refresh_system_waypoints`, `discover_system_markets`, `ingest_scanned_waypoints`, `note_waypoint_traits`, `is_uncharted` |
| market/shipyard getters | `src/universe/mod.rs` — `get_market_remote`, `get_shipyard_remote`, `get_market` |
| market refresh | `src/ship_controller.rs` — `refresh_market`, `refresh_shipyard` |
| market models | `src/models/market.rs` — `Market`, `MarketRemoteView` |
| persistence | `src/database/mod.rs`; `spacetraders_schema.sql.template` |
