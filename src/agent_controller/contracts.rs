use crate::prelude::*;

pub enum ContractStatus {
    // We were unable to negotiate the contract, because no ships were available
    CouldNotNegotiate,
    // We will not fulfill the contract, eg because it's too expensive or unprofitable, or impossible
    WillNotFulfill(&'static str),
    // We have a logistics task to fulfill
    RequiresLogisticsTask(WaypointSymbol, WaypointSymbol, MarketTradeGood, i64),

    // Skipped processing contract tick, because the contract hasn't changed
    Skipped,
}

impl AgentController {
    // Can be used by ships after they have delivered cargo, to hasten the next contract tick
    pub fn spawn_contract_task(&self) {
        let self_clone = self.clone();
        let hdl = tokio::spawn(async move {
            self_clone.contract_tick(true).await;
        });
        self.hdls.push("contract_tick", hdl);
    }

    pub async fn get_current_contract_id(&self) -> Option<String> {
        let contract = self.contract.lock().unwrap();
        contract.as_ref().map(|c| c.id.clone())
    }

    pub async fn get_current_contract(&self) -> Option<Contract> {
        let contract = self.contract.lock().unwrap();
        contract.clone()
    }

    pub fn contract_hash(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let contract = self.contract.lock().unwrap();
        let mut hasher = std::hash::DefaultHasher::new();
        contract.hash(&mut hasher);
        hasher.finish()
    }

    // Negotiate (if able), accept, fulfill or return Delivery Task
    pub async fn contract_tick(&self, may_skip: bool) -> ContractStatus {
        let mut hash = self.contract_tick_mutex_guard.lock().await;
        let current_hash = self.contract_hash();

        // If the contract hasn't changed, we can skip processing contract
        if may_skip && *hash == current_hash {
            return ContractStatus::Skipped;
        }
        *hash = current_hash;

        loop {
            let contract = self.get_current_contract().await;

            match contract {
                Some(contract) if !contract.fulfilled => {
                    let deliver = &contract.terms.deliver[0];
                    if !contract.accepted {
                        // Always accept the contract for the on_accepted credit payment - regardless of whether we intend to fulfill it
                        self.accept_contract().await;
                        continue;
                    }
                    if deliver.units_fulfilled == deliver.units_required {
                        self.fulfill_contract().await;
                    } else {
                        let system_symbol = deliver.destination_symbol.system();

                        // Check trades
                        let good = &deliver.trade_symbol;
                        let markets = self.universe.get_system_markets(&system_symbol).await;

                        // First check if there is a non-import trade for this good
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
                                    let trade =
                                        market.data.trade_goods.iter().find(|g| g.symbol == *good);
                                    trade.map(|trade| (market_symbol, trade))
                                }
                                None => None,
                            })
                            .collect::<Vec<_>>();
                        let buy_trade_good = trades
                            .iter()
                            .filter(|(_, trade)| {
                                // If exchange/export trade exists then filter out import trades
                                // (even if it delays the contract completion until probes reach the market)
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
                                debug!("contract buy_trade_good: {} {:?}", market_symbol, trade);
                                let estimated_cost = trade.purchase_price * deliver.units_required;
                                let reward = contract.terms.payment.on_fulfilled
                                    + contract.terms.payment.on_accepted;
                                let profit = reward - estimated_cost;
                                debug!(
                                    "contract cost: ${}, reward: ${}, profit: ${}",
                                    estimated_cost, reward, profit
                                );

                                // Check if current credits are high enough to cover the cost
                                // The 100k is a bit of an approximation to give us a buffer to use the credits reserved by the logistics ship
                                let available_credits = self.ledger.available_credits() + 100_000;
                                if available_credits < estimated_cost {
                                    return ContractStatus::WillNotFulfill("not enough credits");
                                }

                                if profit <= -50_000 {
                                    ContractStatus::WillNotFulfill("profit is too low")
                                } else {
                                    let missing = deliver.units_required - deliver.units_fulfilled;
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
                    // No active contract: negotiate a new contract
                    let static_probes = self.statically_probed_waypoints();
                    debug!("static_probes: {:?}", static_probes);

                    match static_probes.first() {
                        Some((ship_symbol, _waypoint)) => {
                            self.negotiate_contract(ship_symbol).await;
                        }
                        None => {
                            // Note if we wanted, we could create a logistics task for this situation like we do for buying ships
                            return ContractStatus::WillNotFulfill("no static probe");
                        }
                    }
                }
            }
        }
    }
}
