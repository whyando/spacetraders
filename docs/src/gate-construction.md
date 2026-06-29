# Gate Construction

The early game is gated — literally. A fresh agent is stuck in its home system until
the home **jump gate** is built, which requires delivering construction materials to
the gate's construction site. The whole "home phase" fleet exists to fund and feed
that, and retires once it's done.

This chapter covers how construction is detected, how materials get delivered, and
how it ties into the era/lifecycle machinery.

## Lifecycle and completion

A gate's construction site is queried via `get_construction` →
`Construction { materials, is_complete }`, where each `ConstructionMaterial` tracks
`trade_symbol`, `required`, and `fulfilled`.

`FleetManager::is_jumpgate_finished` (`src/agent_controller/fleet.rs`) is the single
source of truth for "is the home gate done": it reads the construction site and
returns `is_complete` (treating a missing site as complete). This is the cue that
advances the agent out of the home phase.

> Completion is derived from the construction site, **not** the
> `is_under_construction` waypoint flag — that flag goes stale after a gate finishes
> and never refreshes. (The same care applies to the jump-gate graph; see
> [Pathfinding](pathfinding.md).)

## The ConstructionHauler

`ShipBehaviour::ConstructionHauler` → `src/ship_scripts/construction.rs` is a small
state machine:

- **Buying** — check the construction site; for each material still short
  (`fulfilled + onboard < required`), find the market that exports it and buy up to
  what fits / what's needed. Purchases respect a market **supply check** (don't drain
  an under-supplied market) unless `OVERRIDE_CONSTRUCTION_SUPPLY_CHECK=1`. Some
  materials carry a credit buffer so a pricier one doesn't starve the cheaper
  critical-path one. When the hold is full (or nothing more is buyable right now), go
  to **Delivering**; otherwise wait and retry.
- **Delivering** — fly to the gate and `supply_construction(good, units)` for each
  cargo item, then return to **Buying**.
- **Completed** — once the gate is built, idle (and self-scrap; see below).

The exact materials are a property of the server reset (recent resets have used
things like `FAB_MATS` and `ADVANCED_CIRCUITRY`); the code discovers the export
markets for whatever the construction site asks for rather than hard-coding a recipe.
A good may have more than one exporter in the home system (the gate's demand can spawn
a second); the hauler picks the cheapest known one per good at startup rather than
assuming a unique exporter.

The hauler sources finished materials directly from export markets and delivers
them. The home mining/siphon economy isn't a direct feeder of the gate — it funds the
agent (credits) and supplies the *upstream* market supply chain that those export
markets draw on. The logistics planner runs with `allow_construction: false` for the
home fleet, leaving gate delivery to the dedicated hauler script.

## Eras and the home-phase fleet

Construction lives inside the era machinery (`AgentEra`, advanced by
`check_era_advance`):

| Era | Entered when |
|---|---|
| `StartingSystem1` | initial |
| `StartingSystem2` | available credits ≥ ~800k |
| `InterSystem1` | home gate complete (`is_jumpgate_finished`) |

The **home-phase fleet** — mining surveyor + drones + shuttles, siphon drone +
shuttle, the construction hauler, and starter probes — is bought only while
`in_home_phase` (the `StartingSystem*` eras). See `ship_config.rs`
(`ship_config_starter_system`).

### Retirement: `never_purchase` + self-scrap

Once past the gate, the fleet retires via two coordinated mechanisms keyed on the
same cue:

- **Config side**: the slots are still emitted but flipped to `never_purchase`, so no
  replacements are bought.
- **Script side**: each home-fleet script checks `home_phase_done(ac)` (true once the
  era is past `StartingSystem*`) and, when done, scraps itself
  (`ship_scripts::scrap`).

Using one cue for both sides prevents churn (scrap → re-buy → scrap). This is a
different, earlier retirement than the t5-era teardown of the *remaining* starting-area
fleet — see [T5 Trading → Retiring the home fleet](t5-trading.md).

## Observability & overrides

- **`construction_log`** table (`src/schema.rs`) snapshots `fulfilled`/`required` per
  material over time; surfaced via `/api/construction` on the dashboard.
- **`OVERRIDE_CONSTRUCTION_SUPPLY_CHECK=1`** lets the hauler buy materials even when a
  market's supply is low — useful when local markets are underdeveloped and
  construction would otherwise stall.

## Key code references

| concern | location |
|---|---|
| completion check | `src/agent_controller/fleet.rs` — `is_jumpgate_finished` |
| construction site fetch + model | `src/universe/mod.rs` (`get_construction`), `src/models/mod.rs` (`Construction`) |
| hauler state machine | `src/ship_scripts/construction.rs` |
| era progression | `src/agent_controller/fleet.rs` — `check_era_advance`; `src/agent_controller/agent_controller.rs` — `AgentEra` |
| home-phase retirement cue | `src/ship_scripts/mod.rs` — `home_phase_done` |
| home fleet config | `src/ship_config.rs` — `ship_config_starter_system` |
| progress log | `src/schema.rs` — `construction_log`; `/api/construction` |
