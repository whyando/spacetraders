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
