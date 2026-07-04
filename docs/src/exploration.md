# Exploration & Probes

The agent uses cheap `SHIP_PROBE`s for two jobs: **charting the jump-gate network**
(expanding the frontier so new systems become reachable) and **parking on
markets/shipyards** as live price intel and as ship-purchase points.

## Charting probes (`ShipBehaviour::JumpgateProbe`)

`run_jumpgate_probe` (`src/ship_scripts/probe_exploration.rs`) is a small state
machine that maps the gate network gate-by-gate:

- **Init** — reserve a frontier gate to chart next
  (`ExplorationManager::get_probe_jumpgate_reservation`,
  `src/agent_controller/exploration.rs`). Selection is **target-directed**: the
  probes exist to open routes to the systems that matter — high-P(T5) systems and
  faction capitals (all have gates) — so each probe *commits* to one such target
  (`probe_target_systems`, load-balanced across the fleet) and keeps charting
  toward it until that target's gate is charted, then re-commits to the
  least-covered remaining target. Among reachable, uncharted, unreserved gates
  (Dijkstra over the jump-gate graph from the probe), it picks the one minimising
  `g + λ·h`: `g` = jump-cooldown cost to reach it, `h` = Euclidean distance from
  its system to the committed target. Once every important system is charted the
  heuristic vanishes (`h = 0`) and it degrades to the old **nearest-frontier**
  policy, mapping the rest of the network. Then **remotely** check the target's
  construction (`get_construction`, which works without a ship present): if it's
  still under construction, record it as excluded (`mark_jumpgate_under_construction`)
  and pick another — you can't jump to an under-construction gate.
- **Exploring** — route to the target over charted gates, jump hop-by-hop, then
  `get_jumpgate_connections` (requires the ship to be *present*) to chart it. This
  call invalidates the jump-gate graph cache, so the newly-charted gate immediately
  widens the frontier for every probe. Loop.
- **Idle** — when no target is available, stage at the home gate and re-poll every
  60s (other probes keep charting, so the frontier grows) rather than terminating.

The fleet emits a fixed pool of these in the `InterSystem1` era
(`NUM_JUMPGATE_PROBES`, currently 20, in `generate_ship_config`). Reservations live
in a `DashMap` persisted under `probe_jumpgate_reservations/<callsign>` and are
cleared once the target is charted.

> Construction truth comes from the construction *site*, not the
> `is_under_construction` waypoint flag (which goes stale). See
> [Pathfinding](pathfinding.md) and [Gate Construction](gate-construction.md).

## Static intel probes (`ShipBehaviour::Probe`)

`src/ship_scripts/probe.rs` parks a probe at a specific market/shipyard and keeps it
refreshed:

- `goto_waypoint_anywhere` routes there over the jump-gate network (jumps only; see
  [Pathfinding](pathfinding.md)).
- `probe_single_location` parks and periodically `refresh_market`/`refresh_shipyard`s
  — giving the planner live prices and revealing shipyard listings.
  `probe_multiple_locations` roams a small set (less rate-limit-efficient; can't be a
  purchaser).
- **Purchaser role**: a docked static probe at a shipyard is what lets
  `try_buy_ship` actually buy a ship there (see
  [Eras & Lifecycle](eras-lifecycle.md)). This is how the t5-trader purchaser probe
  bootstraps buying freighters in the faction capital, and how contracts get
  negotiated (a static probe negotiates — see [Contracts](contracts.md)).

## Reservations

Both probe kinds and the t5 traders use the same pattern: an in-memory `DashMap`
of `ship → target`, persisted to `generic_lookup` so assignments survive restarts,
with a mutex around the reservation critical section. Charting probes key on gate
waypoints (`probe_jumpgate_reservations`); t5 traders key on systems
(`t5_system_reservations`). The dormant warp explorer uses `explorer_reservations`.
Charting probes additionally keep a `probe_target_systems` map (ship → committed
important system) that drives the target-directed selection above; it's persisted
the same way (`probe_target_systems/<callsign>`) so commitments survive restarts.

## Key code references

| concern | location |
|---|---|
| charting state machine | `src/ship_scripts/probe_exploration.rs` — `run_jumpgate_probe` |
| gate reservation | `src/agent_controller/exploration.rs` — `get_probe_jumpgate_reservation` |
| charting a gate | `src/universe/mod.rs` — `get_jumpgate_connections` (invalidates the graph) |
| static/roaming probes | `src/ship_scripts/probe.rs` — `run`, `probe_single_location`, `goto_waypoint_anywhere` |
| probe fleet emission | `src/agent_controller/fleet.rs` — `generate_ship_config` (`NUM_JUMPGATE_PROBES`) |
