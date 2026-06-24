# Eras & Lifecycle

This chapter ties the moving parts together: how the process starts, how the agent
advances through eras, and the controller loop that buys, assigns, and runs ships.
For the per-era fleet *content* see the subsystem chapters; this is the machinery.

## Startup (`src/bin/main.rs`)

1. Load `.env` (dotenv), init logging, read `AGENT_CALLSIGN` (+ optional faction).
2. Poll `/status` until the server is up; read the `reset_date`.
3. Connect to Postgres with schema `POSTGRES_SCHEMA` (with `{RESET_DATE}` substituted
   — per-reset partitioning); create the schema if needed.
4. Build the `Universe` and `spawn_galaxy_load` (background; see
   [Universe & Market Data](universe-data.md)).
5. Register the agent or load its saved token; set it on the API client.
6. `AgentController::new` hydrates state (agent, ships, contract, reservations,
   ledger, era) from the API + DB, then `run()` spawns the top-level tasks.

`AgentController::run` spawns, via `JoinHandles`: the **cargo broker**, an **agent
startup** task (first `try_buy_ships` + spawn a task per existing ship), the
**controller loop**, and the **web server**.

> **Panic = crash.** `JoinHandles` `unwrap()`s each task result, so a panic in *any*
> ship script propagates up and exits the whole process (Kubernetes then restarts the
> pod). There is no per-ship isolation — this is why ship scripts must avoid panics on
> recoverable conditions. See `src/agent_controller/join_handles.rs`.

## Eras (`src/agent_controller/agent_controller.rs`)

`AgentEra` drives all fleet decisions and is the single source of truth for "what
phase are we in". It's persisted to `generic_lookup` (`<callsign>/state`).

| Era | Entered when |
|---|---|
| `StartingSystem1` | initial |
| `StartingSystem2` | available credits ≥ ~800k |
| `InterSystem1` | home gate complete (`is_jumpgate_finished`) |
| `InterSystem2` | unimplemented (`panic!`, unreached) |

`check_era_advance` (`src/agent_controller/fleet.rs`) runs each tick and advances (it
loops, so several eras can advance at once). `ERA_OVERRIDE=<era>` forces an era for
testing.

## Controller loop

`controller_loop` ticks every 60s (`MissedTickBehavior::Skip`), and each
`controller_tick`:

1. **Records metrics** (`agent_metrics`) and reconciles the cash journal against the
   actual credit delta (warns on a large gap); persists a ledger snapshot.
2. **`check_era_advance`** — maybe advances the era.
3. **`try_buy_ships`** — buys any missing ships for the current config and spawns
   tasks for new ones.
4. **`contract_tick`** — see [Contracts](contracts.md).

## Fleet: config → buy → assign → run

All in `src/agent_controller/fleet.rs`:

- **`generate_ship_config`** returns the desired `ShipConfig` list for the current
  era (id, ship model, `PurchaseCriteria`, `ShipBehaviour`). Re-run each tick via
  `refresh_ship_config`, which also prunes assignments for jobs/ships that no longer
  exist and updates credit reservations.
- **`try_buy_ships` / `try_buy_ship`** — buy unassigned, non-`never_purchase` jobs
  whose id matches `JOB_ID_FILTER`. A buy needs a ship **present at the shipyard** (a
  static probe or designated purchaser); otherwise, if the job allows it, a logistics
  task is created to send one. Serialized by a mutex (panics on a 30s lock timeout).
- **`try_assign_ship`** — match an unassigned ship to the first open job of its model;
  assignments persist in `generic_lookup` (`<callsign>/ship_assignments`).
- **`_spawn_run_ship`** — dispatch a ship to its behaviour's script
  (`Probe`/`Logistics`/`Mining*`/`Siphon*`/`ConstructionHauler`/`JumpgateProbe`/
  `T5Trader`/`Explorer`). If the ship is unassigned and `SCRAP_UNASSIGNED=1`, it runs
  the scrap script instead.

### Key flags

- **`never_purchase`** — slot stays emitted (so a leftover ship stays assigned and its
  script runs, e.g. to self-scrap) but is never bought again.
- **`JOB_ID_FILTER`** (regex, default `.*`) — scopes which jobs the agent manages
  end-to-end: both buying *and* script dispatch. Used for single-ship dev runs.
- **`SCRAP_UNASSIGNED=1`** — unassigned ships self-sell. Used to retire fleets whose
  jobs are no longer emitted (see [T5 Trading](t5-trading.md)).

## The Ledger (`src/agent_controller/ledger.rs`)

Tracks two things: **credit reservations** (so concurrent jobs don't overcommit the
balance — `available_credits()` = credits − effective reserved) and the
**cost basis** of in-transit cargo (weighted average), so a sale after a restart
isn't booked as 100% profit. Snapshotted to `ledger/<callsign>` each tick and
restored at startup.

## Persistence summary

| key (`generic_lookup`) | contents |
|---|---|
| `<callsign>/state` | current era |
| `<callsign>/ship_assignments` | job → ship map |
| `ledger/<callsign>` | reservations + cargo cost basis |
| `*_reservations/<callsign>` | probe / explorer / t5-system reservations |
| `galaxy_loaded`, `gate_waypoints_loaded` | one-time bootstrap markers |

## Key code references

| concern | location |
|---|---|
| startup | `src/bin/main.rs`; `src/agent_controller/agent_controller.rs` — `new`, `run` |
| panic propagation | `src/agent_controller/join_handles.rs` |
| eras | `src/agent_controller/agent_controller.rs` — `AgentEra`; `src/agent_controller/fleet.rs` — `check_era_advance` |
| controller tick | `src/agent_controller/agent_controller.rs` — `controller_loop`, `controller_tick` |
| fleet | `src/agent_controller/fleet.rs` — `generate_ship_config`, `try_buy_ships`, `try_assign_ship`, `_spawn_run_ship` |
| ledger | `src/agent_controller/ledger.rs` |
