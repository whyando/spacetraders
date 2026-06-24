# Pathfinding

There are two distinct pathfinding layers, and they don't share code:

- **In-system navigation** (`src/pathfinding.rs`) — routing between waypoints
  *inside* one system, fuel- and flight-mode-aware.
- **Inter-system travel** (`src/universe/pathfinding.rs`) — the jump-gate graph and
  the warp+jump graph that move ships *between* systems.

## In-system navigation (`src/pathfinding.rs`)

### Model

A `Pathfinding` holds the system's waypoints plus a precomputed `closest_market`
lookup (nearest market and its CRUISE distance for every non-market waypoint).
`get_route(src, dest, …)` returns a `Route` — a list of hops, each an `Edge`
(distance, travel duration, fuel cost, flight mode) — plus `req_terminal_fuel`.

### Flight modes & the fuel/time trade-off

`edge()` greedily picks the cheapest *fast-enough* mode within the fuel budget:

- **BURN** — fuel `2 × distance`, ~2× faster. Chosen if it fits.
- **CRUISE** — fuel `distance`, slower. Fallback.
- Returns `None` if neither fits → the edge doesn't exist.

Dijkstra minimizes **travel duration**, not fuel; fuel is a hard constraint
expressed by edges existing or not.

### The market-centric fuel model

Ships only refuel at **markets**, so non-market waypoints are "traps" you must be
able to escape:

- From a **market**: edges to all other markets, budgeted with full `fuel_capacity`.
- From a **non-market** (only the start): edges to markets budgeted with the ship's
  *current* `start_fuel`.
- To a **non-market** destination: the last hop reserves `req_escape_fuel` (the
  CRUISE distance from the destination to its closest market), so the ship can still
  leave after arriving.

### Fixed: a hop-reconstruction panic on low-fuel non-market routing

After Dijkstra finds a path, the route is rebuilt hop-by-hop and each hop's edge is
recomputed (`edge(a, b, speed, fuel_max).unwrap()`). This used to panic when routing
**from a low-fuel ship parked on a non-market waypoint to a distant non-market
waypoint** (e.g. a freighter hopping between far-flung asteroids).

Root cause: the "market → non-market dest" edge wasn't guarded to market sources. It
budgets a *full* tank (`fuel_capacity`, since you refuel at the market), but the
condition also fired for the non-market **source**, whose real budget is the current
`start_fuel`. So Dijkstra would route src → dest directly on a full-tank edge, then
reconstruction recomputed that hop with the real `start_fuel`, got `None`, and
unwrapped it — crashing the whole agent (it runs inside a ship-script task). The
legitimate non-market-src → dest case is handled by a separate edge with the correct
`start_fuel` budget.

Fixed by guarding that edge with `x.is_market()`; a low-fuel ship now correctly
refuels at a market before heading to a distant non-market. Covered by the
`route_low_fuel_nonmarket_to_distant_nonmarket` regression test in `pathfinding.rs`.

## Inter-system travel (`src/universe/pathfinding.rs`)

### The jump-gate graph

`build_jumpgate_graph` builds a graph of gate waypoints whose edges are the
traversable jump connections:

- **Nodes**: one gate per system (from the in-memory system list — no galaxy-wide
  per-waypoint fetch).
- **Construction status**: a gate counts as constructed via its *construction site*
  (`get_construction` → `is_complete`), not the `is_under_construction` waypoint flag
  (which goes stale after completion). Under-construction gates are excluded as both
  source and destination — you can't jump to or from them.
- **Edges**: only **charted** gates (`self.jumpgates`) contribute connections; each
  edge has `cooldown = 60 + distance`. A reverse edge is added only when the
  destination isn't itself charted (a charted gate emits its own edges), which avoids
  duplicates while the frontier is half-mapped.

Reachability helpers run Dijkstra over `active_connections`:

- `is_jumpgate_reachable(from, to)` — is one gate reachable from another?
- `reachable_high_t5_systems(from)` — all `p_t5 ≥ 0.5` systems whose gate is
  reachable, nearest-first (see [T5 Trading](t5-trading.md)).

### The warp+jump graph

`warp_jump_graph` is a system-to-system graph combining two edge types, sized for an
explorer-class ship (`EXPLORER_FUEL_CAPACITY`, `EXPLORER_SPEED`):

- **Warp edges**: to nearby systems within warp range, found via a quadtree spatial
  index over all system coordinates.
- **Jump edges**: a system's gate's active connections, which **override** any warp
  edge to the same destination (jumps are faster and free).

This is what the (currently dormant) warp-capable explorer used. The t5 traders
deliberately route over jumps only.

## Executing routes

- **`goto_waypoint`** (`src/ship_controller.rs`) — in-system: get a `Route`, then for
  each hop refuel if needed and `navigate` in the hop's flight mode.
- **`goto_waypoint_anywhere`** (`src/ship_scripts/probe.rs`) — cross-system, **jumps
  only**: if already in the target system, `goto_waypoint`; otherwise Dijkstra over
  the jump-gate graph, go to the start gate, `jump` hop-by-hop, then `goto_waypoint`
  to the final waypoint. If no route exists yet, it sleeps and retries (the frontier
  is still being charted).
- Primitives: `navigate` (in-system), `warp` (cross-system, fueled), `jump`
  (gate-to-gate, cooldown), `refuel`.

## Caching

Both graphs are memoized in `moka` caches keyed on `()`:

- They wait for the one-time galaxy load (`await_systems_loaded`) before building.
- `get_with` coalesces concurrent rebuilds.
- `get_jumpgate_connections` calls `jumpgate_graph.invalidate(())` whenever a gate's
  connections change, so a newly-charted gate immediately widens the frontier for
  every probe.

## Key code references

| concern | location |
|---|---|
| in-system routing | `src/pathfinding.rs` — `Pathfinding`, `get_route`, `edge` |
| jump-gate graph + reachability | `src/universe/pathfinding.rs` — `build_jumpgate_graph`, `is_jumpgate_reachable`, `reachable_high_t5_systems` |
| warp+jump graph | `src/universe/pathfinding.rs` — `warp_jump_graph` |
| travel matrix (planner) | `src/universe/pathfinding.rs` — `full_travel_matrix` |
| in-system execution | `src/ship_controller.rs` — `goto_waypoint`, `navigate`, `warp`, `jump`, `refuel` |
| cross-system execution | `src/ship_scripts/probe.rs` — `goto_waypoint_anywhere` |
| graph caching/invalidation | `src/universe/mod.rs` — `jumpgate_graph`, `get_jumpgate_connections` |
