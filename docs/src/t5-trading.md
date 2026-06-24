# T5 Trading

Once the agent is past its home system and onto the jump-gate network, its main
money-maker is **trading the highest-value systems**: it dispatches a refining
freighter to each system likely to be generation **Tier 5** and runs the
logistics planner there.

This chapter covers how a system's value is estimated (the P(T5) model), how
targets are chosen and reached, the `t5_trader` ship behaviour, and how the fleet
buys and retires ships around it.

## Why Tier 5

Systems belong to one of five generation tiers. Tier 5 systems are the rarest and
have the most planets-with-orbiting-stations, which means many markets packed into
one system — ideal for intra-system trading by a high-capacity freighter. We never
observe a system's tier directly, so we estimate it.

## The P(T5) model

`System::p_t5()` (`src/models/system.rs`) returns the posterior probability that a
system is Tier 5, given two counts derived from its waypoints:

- **`p`** — number of `PLANET` waypoints.
- **`o`** — number of those planets that have an orbiting station.

A station "orbits a planet" iff it shares the planet's coordinates: an orbital body
has the same `(x, y)` as its parent, so `o` = count of `ORBITAL_STATION` waypoints
whose `(x, y)` matches a `PLANET`'s. This needs only waypoint `type` + coordinates,
both already loaded for every system — no orbit-linkage data required.

### Formula

Closed-form Bayesian posterior over the 5 tiers. Per planet independently gets a
station with probability `q_t` (so stations ~ `Binomial(p, q_t)`), and planet count
is `p ~ round(N(t+1, σ))`. The binomial coefficient `C(p, o)` is identical across
tiers and cancels.

For each tier `t = 1..5`:

```
term_t = w_t · A_t(p) · q_t^o · (1 - q_t)^(p - o)
```

where `A_t(p) = P(round(N(μ_t, σ)) == p)` is a normal-rounding band (computed via
the normal CDF Φ). The posterior is `P(T5) = term_5 / Σ_t term_t`.

Tier constants (`t = 1..5`):

| tier        | 1    | 2    | 3    | 4    | 5    |
|-------------|------|------|------|------|------|
| `w_t` prior | 100  | 100  | 50   | 20   | 20   |
| `μ_t = t+1` | 2    | 3    | 4    | 5    | 6    |
| `q_t`       | 0.23 | 0.26 | 0.32 | 0.32 | 0.90 |

`σ = 2.0`. The Tier 5 jump in `q_t` (0.32 → 0.90) is what makes a high
station-to-planet ratio such a strong Tier 5 signal.

> Implementation note: Φ uses an Abramowitz & Stegun erf approximation (max abs
> error ~1.5e-7). Verified to produce zero classification changes vs. exact erf
> across all realistic `(p, o)` at the 0.5 threshold. Returns `None` when `p = 0`
> (the band is undefined). This was originally prototyped as a Python script; the
> Rust function is the single source of truth now.

### Threshold

A system is a trade target when `p_t5() >= 0.5`. The cap on how many we actually
work is a fleet concern (see below), not a probability concern.

## Choosing and reaching targets

Targets are ordered by **jump-gate distance from our home gate** — we trade the
nearest high-value systems first, and only systems actually wired into the charted
network are eligible.

- `Universe::reachable_high_t5_systems(from_gate)`
  (`src/universe/pathfinding.rs`) — Dijkstra over the constructed + charted
  jump-gate graph from the home gate; returns every `p_t5 >= 0.5` system whose gate
  is reachable, nearest-first. A high-T5 system with no gate, or not yet wired in,
  is simply omitted — it appears the moment its gate becomes reachable.
- `Universe::is_jumpgate_reachable(from, to)` — same graph, used to gate fleet
  decisions (see below).

Reservations hand each freighter a distinct system:

- `ExplorationManager::get_t5_system_reservation(ship)`
  (`src/agent_controller/exploration.rs`) — reserves the nearest unreserved
  `p_t5 >= 0.5` system. Reservations are persisted (`t5_system_reservations/<callsign>`)
  so a restart keeps assignments. This is independent of the older
  `get_explorer_reservation` (which targets starter systems and is currently
  dormant).

## The `t5_trader` behaviour

`ShipBehaviour::T5Trader` → `run_t5_trader` (`src/ship_scripts/t5_trader.rs`):

1. Reserve a target system (above).
2. **Jump** there over the gate network only — never warp — via
   `goto_waypoint_anywhere`. Safe because the reservation is gate-reachable from
   home and the ship is bought in the (gate-reachable) capital.
3. **Refresh the system's waypoint traits** —
   `Universe::refresh_system_waypoints` re-fetches `/systems/{sys}/waypoints` and
   overwrites our cached details. **This is essential** (see gotcha).
4. Reposition onto the nearest market (the planner indexes the start waypoint into
   the market set and panics on a miss).
5. Trade with a **system-scoped** `LogisticTaskManager` (the shared one only plans
   the starting system, so its market set wouldn't contain this system's markets).

### Gotcha: stale market data

The agent snapshots every gate system's waypoint details once at startup
(`load_gate_waypoints`). A system charted by *other* agents *after* our snapshot
will still look market-less to us — `get_system_waypoints` trusts cached details and
won't refetch. Symptom: a t5_trader arrives and logs "scheduled no tasks to
perform" forever. The step-3 refresh fixes this; without it the freighter idles.

> The freighter has **no sensor array** (only a missile launcher), but it doesn't
> need one: the market traits are public once charted, so a plain re-fetch is
> enough. A sensor scan or per-waypoint chart is only needed to reveal *un*charted
> waypoints.

## Fleet integration

In the `InterSystem1` era, `generate_ship_config`
(`src/agent_controller/fleet.rs`) emits the t5-trading fleet:

- **Lazily scaled**: one `SHIP_REFINING_FREIGHTER` slot per currently-reachable
  high-T5 system, nearest-first, capped at `MAX_T5_TRADERS = 25`. As the network
  grows and more systems become reachable, more freighters are bought — then it
  stops at 25.
- **Bought at the faction capital**: the freighter is sold somewhere in the
  capital; we find the shipyard from its remote `ship_types` (known without a ship
  present) and park a purchaser probe there.
- **Gated on capital reachability**: the whole block only emits once the capital is
  jump-gate-reachable from home (`is_jumpgate_reachable`). Until then a purchaser
  probe couldn't route there (probes jump, never warp) and nothing could be bought.

## Retiring the home fleet

Once the capital is reachable **and** the t5 purchaser probe exists, the
starting-area fleet is obsolete (we trade out of the capital now). The fleet stops
emitting those job slots — home economy, static probes, logistics haulers, the
command ship — so the ships fall unassigned and self-sell via `SCRAP_UNASSIGNED`.
Charting probes and t5 traders are kept.

The purchaser-exists gate avoids a bootstrap deadlock: the home fleet is what buys
the purchaser probe, so it must stick around until the capital pipeline is
self-sufficient. (Mining/siphon/construction already self-retire earlier via
`home_phase_done`.)

## Operational notes

- `SCRAP_UNASSIGNED=1` must be set for the retirement to actually sell ships;
  otherwise they sit idle unassigned. It lives in the agent's deploy values.
- `JOB_ID_FILTER` (regex, default `.*`) scopes which jobs the agent manages
  end-to-end — both which ship scripts run *and* which jobs it will buy. Handy for
  single-ship local runs, e.g. `JOB_ID_FILTER=^t5_trader/1$`.

## Known limitations / future work

- **Charting for credits**: charting uncharted waypoints earns credits, and a
  non-panicking chart primitive exists (`ApiClient::chart_waypoint`, `ship.chart()`)
  but is **not wired into `t5_trader`**. Charting the freighter across distant
  asteroids is slow and is a poor use of an expensive trading ship — it belongs on
  cheap probes.
- **Intel probes** (static price-intel probes per market) were removed — they were
  no-ops in this configuration.

## Key code references

| concern                         | location |
|---------------------------------|----------|
| P(T5) computation               | `src/models/system.rs` — `System::p_t5` |
| reachable targets / reachability| `src/universe/pathfinding.rs` — `reachable_high_t5_systems`, `is_jumpgate_reachable` |
| system trait refresh            | `src/universe/mod.rs` — `refresh_system_waypoints` |
| reservations                    | `src/agent_controller/exploration.rs` — `get_t5_system_reservation` |
| ship behaviour                  | `src/ship_scripts/t5_trader.rs` — `run_t5_trader` |
| fleet emission / retirement     | `src/agent_controller/fleet.rs` — `generate_ship_config` |
| chart primitive (unused)        | `src/api_client/mod.rs` — `chart_waypoint`; `src/ship_controller.rs` — `chart` |
