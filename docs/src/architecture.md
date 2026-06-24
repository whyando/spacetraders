# Agent architecture

How this bot is organized and the logic it follows.

## Lifecycle / eras

The agent advances through `AgentEra` (`agent_controller/agent_controller.rs`), checked each
controller tick by `check_era_advance` (`agent_controller/fleet.rs`):

| Era | Entered when | Fleet (`generate_ship_config`) |
|-----|--------------|--------------------------------|
| `StartingSystem1` | initial | starter economy (command frigate trades, inner probes) |
| `StartingSystem2` | available credits ≥ 800k | starter economy + outer probes/siphons/mining |
| `InterSystem1` | home jump gate is **complete** (`is_jumpgate_finished`) | starter economy **+ 20 jumpgate-charting probes** |
| `InterSystem2` | (unimplemented — `panic!`, unreached) | — |

`ERA_OVERRIDE=<era>` forces an era (manual testing). Era is persisted in the DB
(`<callsign>/state`).

## Galaxy bootstrap + readiness gate

The agent only ever loaded systems lazily, so charting (which needs the wider galaxy) couldn't
work. Fixed in `universe/mod.rs`:

- `spawn_galaxy_load` (called from `bin/main.rs`) runs a **one-time background** load of every
  system via `GET /systems`, bulk-inserts systems + waypoints, repopulates the cache. Guarded by a
  `galaxy_loaded` marker in `generic_lookup`, so it runs once per reset; later restarts load from
  the DB.
- `systems_ready` (a `tokio::sync::watch`) flips true when loaded. Full-galaxy consumers
  (`jumpgate_graph`, `_warp_jump_graph`) call `await_systems_loaded()` first, so charting waits for
  the load while the home economy keeps running.

## Jump-gate graph (`universe/pathfinding.rs`)

- **Lazy**: nodes come from the in-memory system list (coords); it does **not** fetch per-waypoint
  details for the whole galaxy (that was a galaxy-wide API storm). Only **charted** gates
  (`self.jumpgates`) carry connection/construction data; not-yet-charted gates are treated as
  presumed-constructed frontier nodes.
- **Cached + invalidated**: memoized in a `moka` cache (returns an `Arc`); `get_jumpgate_connections`
  calls `jumpgate_graph.invalidate(())` whenever a gate's connections change, so a newly-charted
  gate immediately widens the frontier for all probes. `get_with` coalesces concurrent rebuilds.
- Under-construction gates are excluded as both source and destination, so they're never routed
  through or to (you can't jump to/from a gate under construction).

## Charting probes (`ship_scripts/probe_exploration.rs`)

`ShipBehaviour::JumpgateProbe` → `run_jumpgate_probe`, a small state machine:

- **Init**: reserve the nearest uncharted, unreserved, reachable gate
  (`ExplorationManager::get_probe_jumpgate_reservation`, dijkstra over the graph). Then
  **remotely check the target's construction** (`get_construction` — safe without a ship present):
  if it's still under construction, `mark_jumpgate_under_construction` (records it excluded) and
  pick another target — a probe can't jump to an under-construction gate.
- **Exploring**: path to the target via charted gates, jump hop-by-hop, then
  `get_jumpgate_connections` (requires the ship to be present) to chart it. Loop.
- **Idle**: when no target is available, stage at the home gate and re-poll every 60s (the frontier
  grows as others chart), rather than terminating.

## Construction status

A gate's "constructed" state is derived from its **construction site** (`get_construction` →
`is_complete`), *not* the `waypoint_details.is_under_construction` flag, which goes stale after a
gate finishes building and never refreshes. `get_jumpgate_connections` re-derives `is_constructed`
this way and re-fetches a gate cached as not-constructed (self-heals on completion).

## Web API + dashboard

`web/mod.rs` serves a read-only JSON API (`WEB_PORT`, default 8080), consumed cross-origin by the
`spacetraders-dashboard` SPA. Endpoints: `/api/agent` (incl. `era`), `/api/ships` (incl. nav
destination + ETA), `/api/history`, `/api/construction`, `/api/systems`,
`/api/systems/{system}/markets`, `/api/markets/{waypoint}`, `/api/universe` (galaxy map). Dashboard
tabs: Overview · Ships · Markets · Construction · Map.

## Deploy

Version bump is required to roll new code (image `pullPolicy: IfNotPresent` keeps a cached tag):

1. Bump `Cargo.toml` + `helm/spacetraders/Chart.yaml` (`cargo update -p st --precise <v>`).
2. `./publish.sh` (builds + pushes `registry.whyando.com/whyando/spacetraders:<v>`).
3. `helm upgrade tst-4381 ./helm/spacetraders --kube-context=jpa-dev -n spacetraders --reuse-values --set image.tag=<v>`.

The live release is `tst-4381` (namespace `spacetraders`, callsign `TST-4382`, schema
`tst4382_{RESET_DATE}`). A single ship-script panic propagates through `join_handles` and crashes
the whole agent (the entire process exits and Kubernetes restarts the pod).

For deeper dives into specific subsystems, see the [Logistics Planner](logistics-planner.md),
[Pathfinding](pathfinding.md), [Gate Construction](gate-construction.md), and
[T5 Trading](t5-trading.md) chapters.
