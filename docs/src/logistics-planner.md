# Logistics Planner

The logistics planner is the agent's core money engine. Given a ship in a system,
it picks a value-maximizing sequence of buy/sell/refresh actions to run within a
time budget, then feeds the ship those actions one at a time. It's used by the
command frigate at home, by the t5 traders in remote systems, and anywhere else a
ship "just trades a system".

- Planner core: `src/logistics_planner/` (`plan.rs`, `mod.rs`, `value_feature.rs`)
- Task generation + per-ship orchestration: `src/tasks.rs` (`LogisticTaskManager`)
- Ship-side execution loop: `src/ship_scripts/logistics.rs`

## The task model

A `Task` (`src/logistics_planner/mod.rs`) has an `id`, a `value` (reward, roughly
credits), and `actions` of one of two shapes:

- **`VisitLocation { waypoint, action }`** — a single stop, e.g. `RefreshMarket`,
  `RefreshShipyard`, `TryBuyShips`.
- **`TransportCargo { src, dest, .. }`** — a buy at `src` paired with a sell/deliver
  at `dest`. The source action is always `BuyGoods`; the destination is `SellGoods`,
  `DeliverContract`, or `DeliverConstruction`. Both reference the same good and
  quantity. This becomes an atomic pickup-delivery job in the solver — both ends are
  served or neither.

The planner returns a `ShipSchedule` of `ScheduledAction`s (waypoint + action +
which task they belong to + whether they complete the task).

## Where tasks come from

`LogisticTaskManager::generate_task_list` (`src/tasks.rs`) builds the candidate
tasks for one system:

- **Trade tasks** — for each good, pair the cheapest viable export/exchange (the
  buy) with the most expensive viable import/exchange (the sell). Units are capped
  by trade volume and cargo capacity; value is `(sell − buy) × units`. Only emitted
  if profit ≥ `config.min_profit`.
- **Refresh-market tasks** — keep price data fresh. The reward scales with
  staleness: data under ~5 min old is skipped, then the reward steps up with age
  (older/unknown markets are worth much more to visit). Pure fuel-stop markets (no
  imports/exports) are skipped. This is what bootstraps a freshly-reached system
  where we have no prices yet.
- **Refresh-shipyard tasks** — visit shipyards whose details we lack.
- **Contract / construction delivery tasks** — high-value `TransportCargo` to a
  contract or construction destination (when enabled by config).

Which of these are generated is gated by `LogisticsScriptConfig` flags
(`allow_market_refresh`, `allow_shipbuying`, `allow_construction`, `min_profit`,
`waypoint_allowlist`).

## The solver

Planning is a **Vehicle Routing Problem** solved with the `vrp-core` crate
(`src/logistics_planner/plan.rs`):

- `translate_problem` maps tasks → VRP jobs: `VisitLocation` → a single job at one
  location; `TransportCargo` → a pickup+delivery multi-job (pickup adds cargo,
  delivery removes it). Each job carries the task's `value`.
- One vehicle per ship, starting at the ship's current waypoint, with the ship's
  cargo capacity as a single-dimension load constraint.
- Travel costs come from a precomputed duration/distance matrix over the system's
  market waypoints (`universe::pathfinding::full_travel_matrix`), which already bakes
  in fuel reachability — unreachable markets simply aren't in the matrix.
- The objective, in priority order: **maximize total task value**, then minimize
  unassigned jobs, then minimize travel time, with cargo capacity as a hard
  constraint (`value_feature.rs` provides the value objective).
- The solve is bounded by `max_compute_time` and a generation cap.

### Plan length (ramping)

`PlannerConfig.plan_length` is either `Fixed` or `Ramping(min, max, factor)`. With
ramping, the time budget grows as `min · factor^run_count` (clamped to `max`): early
plans are short and cheap, later ones are allowed to be longer. `run_count` is
tracked per task manager.

## Per-ship orchestration

`src/ship_scripts/logistics.rs` runs the loop:

1. `register_ship` once (capacity, speed, fuel).
2. `get_next_task(ship, waypoint)` — returns the next queued action, or runs the
   planner to produce a fresh schedule when the queue is empty.
3. `goto_waypoint` + execute the action (`refresh_market`, buy, sell, deliver, etc.)
   then `complete_action`.
4. If the planner yields **nothing**, the ship logs "scheduled no tasks to perform"
   and sleeps 5–10 minutes before retrying. (Seeing this persistently usually means
   the system has no known markets/prices — see [T5 Trading](t5-trading.md) for the
   stale-traits case.)

Planner runs are serialized per manager by a mutex. If the planner returns an empty
schedule but tasks do exist, there's a fallback that assigns the single
highest-value task so the ship always makes progress.

## Per-system vs shared managers

A `LogisticTaskManager` is scoped to **one** `start_system` and only plans tasks for
that system. The home economy uses a shared manager (`ac.task_manager`) for the
starting system; a ship trading a *remote* system must build its own manager scoped
to that system (this is exactly the bug that made early t5 traders panic — see
[T5 Trading](t5-trading.md)).

## Gotchas

- **Start waypoint must be a market.** The planner indexes every task waypoint —
  including the ship's start — into the system's market-waypoint list and `unwrap`s
  the position (`plan.rs`, ~line 36). A non-market start (e.g. a ship parked on a
  jump gate or asteroid) panics. Callers reposition onto a market first.
- **Tasks must reference in-system markets.** Before planning, tasks whose waypoints
  aren't in the system's market set are filtered out (`tasks.rs`), precisely to avoid
  the `unwrap` above (e.g. a contract delivery back to a non-market home waypoint).
- **A planner panic crashes the whole agent** (it runs inside a ship-script task; a
  panic propagates through `join_handles`). Keep the invariants above intact.

## Key code references

| concern | location |
|---|---|
| Task / Action / ShipSchedule types | `src/logistics_planner/mod.rs` |
| VRP translation + solve | `src/logistics_planner/plan.rs` — `translate_problem`, `run_planner` |
| value objective | `src/logistics_planner/value_feature.rs` |
| task generation + rewards | `src/tasks.rs` — `generate_task_list` |
| per-ship planning/assignment | `src/tasks.rs` — `register_ship`, `get_next_task`, `complete_action` |
| execution loop + action dispatch | `src/ship_scripts/logistics.rs` |
| travel-time/distance matrix | `src/universe/pathfinding.rs` — `full_travel_matrix` |
| config | `src/models/mod.rs` — `LogisticsScriptConfig`, `PlannerConfig`, `PlanLength` |
