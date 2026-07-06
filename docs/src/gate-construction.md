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
  an under-supplied market) unless the **rush** is on (see below). Some materials carry
  a credit buffer so a pricier one doesn't starve the cheaper critical-path one. When
  the hold is full (or nothing more is buyable right now), go to **Delivering**;
  otherwise wait and retry.
- **Delivering** — fly to the gate and `supply_construction(good, units)` for each
  cargo item, then return to **Buying**.
- **Completed** — once the gate is built, idle (and self-scrap; see below).

The exact materials are a property of the server reset (recent resets have used
things like `FAB_MATS` and `ADVANCED_CIRCUITRY`); the code discovers the export
markets for whatever the construction site asks for rather than hard-coding a recipe.
A good may have more than one exporter in the home system (the gate's demand can spawn
a second). The hauler considers *all* of a good's export markets each trip and buys
from whichever currently passes the supply gate (cheapest first), rather than locking
to one. Multiple construction haulers (`NUM_CONSTRUCTION_HAULERS`) run in parallel,
each offset by ship symbol so they prefer different markets when more than one is
buyable instead of contending for the same one.

The hauler sources finished materials directly from export markets and delivers
them. The home mining/siphon economy isn't a direct feeder of the gate — it funds the
agent (credits) and supplies the *upstream* market supply chain that those export
markets draw on. The logistics planner runs with `allow_construction: false` for the
home fleet, leaving gate delivery to the dedicated hauler script.

### The rush (auto-enabled finish)

Normally the supply check keeps buys on the cheap part of the price curve, letting
exports regenerate between trips. The **rush** drops that check and buys the remaining
materials from *any* export regardless of supply, accepting the escalating price to
finish sooner. It turns on in one of two ways:

- **Manually** via `OVERRIDE_CONSTRUCTION_SUPPLY_CHECK=1`.
- **Automatically** once the fleet can afford to finish and still keep a `RUSH_RESERVE`
  (1M) headroom. Each Buying tick estimates the escalating cost to buy out every
  remaining material (`estimate_rush_cost`) and, when
  `available_credits ≥ estimate + RUSH_RESERVE`, latches the rush on fleet-wide via the
  `construction_rush_active` DB key. The latch is sticky — once set it stays set, so
  draining cash mid-rush doesn't flip it back off.

The cost estimate replicates the server's export price curve
(`price = base·(1 + (4·2^(-0.3·s) − 5)·margin/100)`, `s = netSupply/trade_volume`; see
`~/spacetraders-dev`). netSupply isn't observable, so it's inverted from the live
purchase price, then the remaining units are walked in `trade_volume` chunks so the
price escalates as supply drains — cheap for the last shipload, exponential for a deep
rush from an already-drained market. It models a single-market burst, so it errs high
(real buying spreads across markets and lets them regen), which is the safe direction
for an affordability gate. `matches_reference_simulation` pins the constants to a
validated offline sim.

While the rush is active, purchases run with no per-buy credit buffer (0) — including
dropping advanced circuitry's extra buy floor (FAB no longer needs priority when we're
buying everything). The 1M end-state reserve isn't re-checked per buy: the trigger
already required `available ≥ estimate + RUSH_RESERVE` against a high-erring estimate,
so finishing is guaranteed to leave the reserve without stalling the tail.

### Multi-hauler coordination

Two things stop multiple haulers from tripping over each other, and matter most under
rush (where the supply throttle that used to space buys out is gone):

- **Interleaving.** Each hauler starts its material scan at `materials[hauler_index]`
  (mod count), `hauler_index` parsed from its `jump_gate_hauler/N` job id. So haulers
  stagger across materials and fill them in parallel instead of all draining the first
  to 100% first — which would push that one export deepest into its exponential tail.
- **Reservations.** A hauler buys the fleet-wide outstanding gap
  (`required − fulfilled − in-cargo − others' reservations`), not just its own hold, so
  two deciding at once can't both claim the last chunk and overbuy (excess can only be
  dumped at a loss once the gate is satisfied). `fleet_inflight` covers units already in
  a hold; a **reservation** covers the window between deciding and the buy landing. The
  reservation map is **keyed per ship and persisted** (`construction_reservations` DB
  key) so the coordination survives a mid-navigation crash: siblings still see the
  reservation, and on restart the owner reclaims its own entry (`clear_reservation` runs
  at the top of every Buying tick), so it can never become a phantom that permanently
  under-counts the gap. A process-local async mutex serializes the read-modify-write.

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
- **`OVERRIDE_CONSTRUCTION_SUPPLY_CHECK=1`** forces the rush on from the start (buy
  regardless of supply) — useful when local markets are underdeveloped and construction
  would otherwise stall.
- **`construction_rush_active`** (generic_lookup DB key) is the fleet-wide latch for the
  auto-enabled rush; set once the affordability trigger fires and never cleared for the
  reset.

## Key code references

| concern | location |
|---|---|
| completion check | `src/agent_controller/fleet.rs` — `is_jumpgate_finished` |
| construction site fetch + model | `src/universe/mod.rs` (`get_construction`), `src/models/mod.rs` (`Construction`) |
| hauler state machine | `src/ship_scripts/construction.rs` |
| rush trigger + escalating cost estimate | `src/ship_scripts/construction.rs` — `estimate_rush_cost`, `rush_cost_for_good`, `RUSH_RESERVE`, `RUSH_LATCH_KEY` |
| multi-hauler coordination | `src/ship_scripts/construction.rs` — `fleet_inflight`, `reserve_units`/`clear_reservation`/`reservation_gap`, `hauler_index`, `RESERVATIONS_KEY` |
| era progression | `src/agent_controller/fleet.rs` — `check_era_advance`; `src/agent_controller/agent_controller.rs` — `AgentEra` |
| home-phase retirement cue | `src/ship_scripts/mod.rs` — `home_phase_done` |
| home fleet config | `src/ship_config.rs` — `ship_config_starter_system` |
| progress log | `src/schema.rs` — `construction_log`; `/api/construction` |
