# Mining & Siphon

Two near-identical extract-and-haul pipelines feed the home economy during the
build-out phase: **mining** ore at engineered asteroids and **siphoning** gas at
gas giants. Both use a drone + shuttle split coordinated by an in-place cargo
broker, and both self-retire once the home phase ends.

## Mining (`src/ship_scripts/mining.rs`)

Three roles:

- **MiningSurveyor** (`run_surveyor`) — sits at an engineered asteroid and
  repeatedly `ship.survey()`s. Each survey is pushed into the `SurveyManager`.
- **MiningDrone** (`run_mining_drone`) — pulls the best current survey from the
  manager and `extract_survey`s with it, jettisons waste goods (e.g. `ICE_WATER`,
  `ALUMINUM_ORE`), and hands full cargo to a shuttle via the broker. Waits out
  cooldown before pulling a survey (reduces wasted requests on exhausted surveys).
- **MiningShuttle** (`run_shuttle`, a `SHIP_LIGHT_HAULER`) — a two-state
  (Loading/Selling) machine: receive cargo from drones at the asteroid, then sell
  the valuable goods at the best import markets. State is persisted so it survives
  restarts.

### Surveys (`src/survey_manager.rs`)

`ship.survey()` returns deposits + sizes + expiry; the `SurveyManager` keeps them
in-memory keyed by asteroid and persisted in the `surveys` DB table.
`get_survey` returns the highest-scoring non-expired survey (with a few-minute expiry
safety margin); `survey_score` averages per-deposit scores (valuable ores 1.0, waste
0.0). The API signals exhausted/invalid surveys via error codes, on which the
manager drops them.

## Siphon (`src/ship_scripts/siphon.rs`)

Same shape with two roles: **SiphonDrone** (`run_drone`) repeatedly `ship.siphon()`s
at a gas giant and transfers full cargo; **SiphonShuttle** (`run_shuttle`) loads from
drones and sells gas (`LIQUID_NITROGEN`, `LIQUID_HYDROGEN`, `HYDROCARBON`) at an
exchange. No surveys are involved — siphoning doesn't need them.

## The cargo broker (`src/broker.rs`)

Drones and shuttles meet at the same waypoint and transfer cargo ship-to-ship
without anyone flying off. The broker is an async actor (an `mpsc` channel, spawned
once in `AgentController::run`):

- A drone calls `transfer_cargo(ship, waypoint, goods)`; a shuttle calls
  `receive_cargo(ship, waypoint, capacity)`. Both block (via oneshot channels) until
  matched.
- `try_transfer` FIFO-matches waiting senders/receivers per waypoint and issues the
  actual ship-to-ship transfer API call, moving `min(capacity, units)` at a time.

This decouples the fast-cycling extractors from the slower haulers and keeps drones
mining continuously.

## Lifecycle

Both fleets are bought only `in_home_phase` (`ship_config.rs` —
`ship_config_starter_system`: surveyor + drones + shuttles; siphon drones + shuttle).
Once `home_phase_done(ac)` is true (era past `StartingSystem*`), each script scraps
itself (`ship_scripts::scrap`), and the config slots flip to `never_purchase` so none
are rebought. See [Gate Construction](gate-construction.md) for the shared
retirement cue and [Eras & Lifecycle](eras-lifecycle.md) for the era machinery.

## Key code references

| concern | location |
|---|---|
| mining roles | `src/ship_scripts/mining.rs` — `run_surveyor`, `run_mining_drone`, `run_shuttle` |
| siphon roles | `src/ship_scripts/siphon.rs` — `run_drone`, `run_shuttle` |
| survey store/scoring | `src/survey_manager.rs` — `get_survey`, `survey_score`, `insert_surveys` |
| extract / siphon / survey | `src/ship_controller.rs` — `survey`, `extract_survey`, `siphon` |
| in-place cargo transfer | `src/broker.rs` — `CargoBroker`, `transfer_cargo`, `receive_cargo`, `try_transfer` |
| fleet sizing + retirement | `src/ship_config.rs`; `src/ship_scripts/mod.rs` — `home_phase_done` |
