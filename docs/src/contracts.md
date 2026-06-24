# Contracts

The agent runs one procurement contract at a time as a side income: a static probe
negotiates and accepts it, a logistics ship buys and delivers the goods, and the
contract is fulfilled for payment.

## Lifecycle (`src/agent_controller/contract_manager.rs`)

`contract_tick(may_skip)` drives the whole flow; the single active contract lives in
`AgentContext.contract` (`Arc<Mutex<Option<Contract>>>`, model in
`src/models/contract.rs`):

1. **Negotiate** — a docked **static probe** calls
   `/my/ships/{ship}/negotiate/contract` to obtain a contract (negotiation requires a
   ship present at a waypoint). If no static probe is available, the tick can't
   negotiate.
2. **Accept** — if a contract exists and isn't accepted, accept it (records the
   `on_accepted` payment in the cash journal).
3. **Deliver** — the goods are bought and delivered (see below) until every deliver
   line's `units_fulfilled == units_required`.
4. **Fulfill** — once all lines are met, fulfill it (records `on_fulfilled`), then the
   next tick can negotiate a new one.

`contract_tick` is invoked every ~60s from the controller loop and again immediately
after a contract delivery completes. `may_skip=true` short-circuits when the contract
state hash hasn't changed, avoiding redundant API calls.

### Acceptance gating

Contracts are only taken if they look worthwhile — there's a profit floor (clearly
unprofitable contracts are rejected) and a credit buffer so taking one doesn't
overcommit the balance. Cheapest non-import sources for the required good are
preferred when available.

## Delivery via the logistics planner

Delivery rides the normal [logistics planner](logistics-planner.md):

- When contract tasks are enabled, `generate_task_list` (`src/tasks.rs`) emits a
  `TransportCargo` task — `BuyGoods(good)` at the cheapest source →
  `DeliverContract(good)` at the destination — with a very high value (~50,000) so
  the planner prioritizes it over ordinary trades. (It also suppresses the plain
  trade task for that good.)
- A logistics ship (typically the home **command frigate**) is assigned the task,
  buys at the source, and calls `ship.deliver_contract(...)` at the destination
  (`/my/contracts/{id}/deliver`).

## Config & gotchas

- **`DEBUG_DISABLE_CONTRACT_TASKS=1`** (`CONFIG.disable_contract_tasks`) stops
  contract *delivery tasks* from being generated; `contract_tick` still
  negotiates/accepts/fulfills.
- **Non-market destinations are silently dropped.** The planner requires every task
  waypoint to be an in-system market (see
  [Logistics Planner → Gotchas](logistics-planner.md)). A contract whose delivery
  destination isn't a market — or is cross-system — is filtered out before planning
  and won't be fulfilled. This is a known limitation, not an error.
- **One contract at a time.** No queue; the next is negotiated only after the current
  one is fulfilled.
- Cash flows go through the `agent_transaction_log` (types `contract_accept` /
  `contract_fulfill`), which the controller's cash reconciliation reads.

## Key code references

| concern | location |
|---|---|
| lifecycle / tick | `src/agent_controller/contract_manager.rs` — `contract_tick`, `negotiate_contract`, `accept_contract`, `fulfill_contract` |
| model | `src/models/contract.rs` — `Contract`, terms/deliver/payment |
| delivery task generation | `src/tasks.rs` — `generate_task_list` (contract `TransportCargo`, value ~50k) |
| deliver action | `src/ship_controller.rs` — `deliver_contract`; `src/logistics_planner/` — `Action::DeliverContract` |
| config | `src/config.rs` — `disable_contract_tasks` (`DEBUG_DISABLE_CONTRACT_TASKS`) |
