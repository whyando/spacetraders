use super::context::AgentContext;
use super::fleet::FleetManager;
use crate::api_client::api_models::ContractActionResponse;
use crate::models::*;
use log::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

pub enum ContractStatus {
    CouldNotNegotiate,
    WillNotFulfill(&'static str),
    RequiresLogisticsTask(WaypointSymbol, WaypointSymbol, MarketTradeGood, i64),
    Skipped,
}

#[derive(Clone)]
pub struct ContractManager {
    ctx: Arc<AgentContext>,
    fleet: FleetManager,
    contract_tick_mutex_guard: Arc<tokio::sync::Mutex<u64>>,
}

impl ContractManager {
    pub fn new(
        ctx: Arc<AgentContext>,
        fleet: FleetManager,
    ) -> Self {
        Self {
            ctx,
            fleet,
            contract_tick_mutex_guard: Arc::new(tokio::sync::Mutex::new(0)),
        }
    }

    fn debug(&self, msg: &str) {
        debug!("[{}] {}", self.ctx.callsign, msg);
    }

    pub fn spawn_contract_task(&self) {
        let self_clone = self.clone();
        let hdl = tokio::spawn(async move {
            self_clone.contract_tick(true).await;
        });
        self.fleet.hdls.push("contract_tick", hdl);
    }

    pub fn get_current_contract_id(&self) -> Option<String> {
        let contract = self.ctx.contract.lock().unwrap();
        contract.as_ref().map(|c| c.id.clone())
    }

    pub fn get_current_contract(&self) -> Option<Contract> {
        let contract = self.ctx.contract.lock().unwrap();
        contract.clone()
    }

    pub fn contract_hash(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let contract = self.ctx.contract.lock().unwrap();
        let mut hasher = std::hash::DefaultHasher::new();
        contract.hash(&mut hasher);
        hasher.finish()
    }

    async fn contract_inner(&self, path: &str) {
        let contract_id = self.get_current_contract_id().expect("no contract");

        self.debug(&format!("{} contract {}", path, contract_id));
        let uri = format!("/my/contracts/{}/{}", contract_id, path);
        let body = json!({});
        let ContractActionResponse { agent, contract } = self
            .ctx
            .api_client
            .post::<Data<ContractActionResponse>, _>(&uri, &body)
            .await
            .data;

        assert_eq!(contract.id, contract_id);
        let (txn_type, amount) = match path {
            "accept" => {
                assert!(contract.accepted);
                ("contract_accept", contract.terms.payment.on_accepted)
            }
            "fulfill" => {
                assert!(contract.fulfilled);
                ("contract_fulfill", contract.terms.payment.on_fulfilled)
            }
            _ => panic!("invalid contract action: {}", path),
        };

        // The on_fulfilled payout is earned by the ships that delivered the goods, so
        // attribute it to them (split by delivered units) instead of leaving it
        // unattributed at the agent level — otherwise contract-sourcing ships look like
        // they only ever lose money (they eat the trade_buy cost with no offset). The
        // on_accepted signing bonus isn't delivery-earned, so it stays agent-level.
        // The split rows sum exactly to `amount`, keeping the cash journal reconciled.
        let deliveries = if txn_type == "contract_fulfill" {
            self.ctx
                .db
                .contract_delivery_units_by_ship(&contract_id)
                .await
        } else {
            Vec::new()
        };
        let shares = split_payment_by_units(&deliveries, amount);
        if !shares.is_empty() {
            for ((ship, units), share) in deliveries.iter().zip(shares) {
                self.ctx
                    .db
                    .record_cash_txn(crate::database::CashTxn {
                        ts: chrono::Utc::now(),
                        type_: txn_type,
                        ship_symbol: Some(ship),
                        reference: Some(&contract_id),
                        waypoint: None,
                        units: Some(*units as i32),
                        amount: share,
                        realized_profit: Some(share),
                    })
                    .await;
            }
        } else {
            // accept bonus, or a contract whose deliveries predate this attribution
            // (e.g. in flight at deploy): keep the original agent-level row.
            self.ctx
                .db
                .record_cash_txn(crate::database::CashTxn {
                    ts: chrono::Utc::now(),
                    type_: txn_type,
                    ship_symbol: None,
                    reference: Some(&contract_id),
                    waypoint: None,
                    units: None,
                    amount,
                    realized_profit: None,
                })
                .await;
        }

        self.ctx.update_contract(contract);
        self.ctx.update_agent(agent);
    }

    pub async fn accept_contract(&self) {
        self.contract_inner("accept").await;
    }

    pub async fn fulfill_contract(&self) {
        self.contract_inner("fulfill").await;
    }

    pub async fn negotiate_contract(&self, ship_symbol: &str) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct Response {
            contract: Contract,
        }
        self.debug(&format!("Negotiating contract with {}", ship_symbol));
        let uri = format!("/my/ships/{}/negotiate/contract", ship_symbol);
        let body = json!({});
        let Response { contract } = self
            .ctx
            .api_client
            .post::<Data<Response>, _>(&uri, &body)
            .await
            .data;
        self.ctx.update_contract(contract);
    }

    pub async fn contract_tick(&self, may_skip: bool) -> ContractStatus {
        let mut hash = self.contract_tick_mutex_guard.lock().await;
        let current_hash = self.contract_hash();

        if may_skip && *hash == current_hash {
            return ContractStatus::Skipped;
        }
        *hash = current_hash;

        loop {
            let contract = self.get_current_contract();

            match contract {
                Some(contract) if !contract.fulfilled => {
                    let deliver = &contract.terms.deliver[0];
                    if !contract.accepted {
                        self.accept_contract().await;
                        continue;
                    }
                    if deliver.units_fulfilled == deliver.units_required {
                        self.fulfill_contract().await;
                    } else {
                        let system_symbol = deliver.destination_symbol.system();

                        let good = &deliver.trade_symbol;
                        let markets =
                            self.ctx.universe.get_system_markets(&system_symbol).await;

                        let non_import_trade_exists =
                            markets.iter().any(|(market_remote, _market_opt)| {
                                if market_remote.exports.iter().any(|g| g.symbol == *good)
                                    || market_remote.exchange.iter().any(|g| g.symbol == *good)
                                {
                                    return true;
                                }
                                false
                            });

                        let trades = markets
                            .iter()
                            .filter_map(|(_, market_opt)| match market_opt {
                                Some(market) => {
                                    let market_symbol = market.data.symbol.clone();
                                    let trade = market
                                        .data
                                        .trade_goods
                                        .iter()
                                        .find(|g| g.symbol == *good);
                                    trade.map(|trade| (market_symbol, trade))
                                }
                                None => None,
                            })
                            .collect::<Vec<_>>();
                        let buy_trade_good = trades
                            .iter()
                            .filter(|(_, trade)| {
                                if non_import_trade_exists {
                                    trade._type != MarketType::Import
                                } else {
                                    true
                                }
                            })
                            .min_by_key(|(_, trade)| trade.purchase_price);

                        return match buy_trade_good {
                            Some((market_symbol, trade)) => {
                                debug!(
                                    "contract: {}/{} {} @ {}",
                                    deliver.units_fulfilled,
                                    deliver.units_required,
                                    trade.symbol,
                                    deliver.destination_symbol
                                );
                                debug!(
                                    "contract buy_trade_good: {} {:?}",
                                    market_symbol, trade
                                );
                                let estimated_cost =
                                    trade.purchase_price * deliver.units_required;
                                let reward = contract.terms.payment.on_fulfilled
                                    + contract.terms.payment.on_accepted;
                                let profit = reward - estimated_cost;
                                debug!(
                                    "contract cost: ${}, reward: ${}, profit: ${}",
                                    estimated_cost, reward, profit
                                );

                                let available_credits =
                                    self.ctx.ledger.available_credits() + 100_000;
                                if available_credits < estimated_cost {
                                    return ContractStatus::WillNotFulfill(
                                        "not enough credits",
                                    );
                                }

                                if profit <= -50_000 {
                                    ContractStatus::WillNotFulfill("profit is too low")
                                } else {
                                    let missing =
                                        deliver.units_required - deliver.units_fulfilled;
                                    ContractStatus::RequiresLogisticsTask(
                                        market_symbol.clone(),
                                        deliver.destination_symbol.clone(),
                                        (*trade).clone(),
                                        missing,
                                    )
                                }
                            }
                            None => {
                                debug!("contract: no buy location for {}", good);
                                ContractStatus::WillNotFulfill("no buy location")
                            }
                        };
                    }
                }
                _ => {
                    let static_probes = self.fleet.statically_probed_waypoints();
                    debug!("static_probes: {:?}", static_probes);

                    match static_probes.first() {
                        Some((ship_symbol, _waypoint)) => {
                            self.negotiate_contract(ship_symbol).await;
                        }
                        None => {
                            return ContractStatus::WillNotFulfill("no static probe");
                        }
                    }
                }
            }
        }
    }
}

// Split a contract payout across delivering ships in proportion to units delivered.
// Returns one share per entry of `deliveries` (same order); the last ship absorbs the
// integer-division remainder so the shares sum to `amount` exactly (the cash journal
// must reconcile to the real credit inflow). Returns empty if there are no recorded
// deliveries, signalling the caller to fall back to an agent-level row.
fn split_payment_by_units(deliveries: &[(String, i64)], amount: i64) -> Vec<i64> {
    let total: i64 = deliveries.iter().map(|(_, u)| u).sum();
    if total <= 0 {
        return Vec::new();
    }
    let n = deliveries.len();
    let mut remaining = amount;
    let mut shares = Vec::with_capacity(n);
    for (i, (_, units)) in deliveries.iter().enumerate() {
        let share = if i == n - 1 {
            remaining
        } else {
            amount * units / total
        };
        remaining -= share;
        shares.push(share);
    }
    shares
}

#[cfg(test)]
mod tests {
    use super::split_payment_by_units;

    fn d(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(s, u)| (s.to_string(), *u)).collect()
    }

    #[test]
    fn splits_proportionally_and_sums_exactly() {
        let deliveries = d(&[("A", 30), ("B", 10)]);
        let shares = split_payment_by_units(&deliveries, 1000);
        assert_eq!(shares, vec![750, 250]);
        assert_eq!(shares.iter().sum::<i64>(), 1000);
    }

    #[test]
    fn remainder_goes_to_last_ship_and_total_is_preserved() {
        // 1000 / 3 doesn't divide evenly; rows must still sum to 1000.
        let deliveries = d(&[("A", 1), ("B", 1), ("C", 1)]);
        let shares = split_payment_by_units(&deliveries, 1000);
        assert_eq!(shares, vec![333, 333, 334]);
        assert_eq!(shares.iter().sum::<i64>(), 1000);
    }

    #[test]
    fn single_ship_gets_everything() {
        let shares = split_payment_by_units(&d(&[("A", 5)]), 777);
        assert_eq!(shares, vec![777]);
    }

    #[test]
    fn no_deliveries_returns_empty() {
        assert!(split_payment_by_units(&[], 1000).is_empty());
        assert!(split_payment_by_units(&d(&[("A", 0)]), 1000).is_empty());
    }
}
