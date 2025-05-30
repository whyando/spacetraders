use crate::agent_controller::AgentController;
use crate::api_client::api_models::WaypointDetailed;
use crate::config::CONFIG;
use crate::database::DbClient;
use crate::logistics_planner::{
    self, Action, LogisticShip, PlannerConstraints, ScheduledAction, ShipSchedule, Task,
    TaskActions,
};
use crate::models::MarketSupply::*;
use crate::models::MarketType::*;
use crate::models::*;
use crate::models::{LogisticsScriptConfig, MarketActivity::*};
use crate::universe::{Universe, WaypointFilter};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use log::*;
use std::cmp::min;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

fn is_task_allowed(task: &Task, config: &LogisticsScriptConfig) -> bool {
    if let Some(waypoint_allowlist) = &config.waypoint_allowlist {
        match &task.actions {
            TaskActions::VisitLocation { waypoint, .. } => {
                if !waypoint_allowlist.contains(&waypoint) {
                    return false;
                }
            }
            TaskActions::TransportCargo { src, dest, .. } => {
                if !waypoint_allowlist.contains(&src) || !waypoint_allowlist.contains(&dest) {
                    return false;
                }
            }
        }
    }
    match &task.actions {
        TaskActions::VisitLocation { action, .. } => match action {
            Action::RefreshMarket => config.allow_market_refresh,
            Action::RefreshShipyard => config.allow_market_refresh,
            Action::TryBuyShips => config.allow_shipbuying,
            _ => true,
        },
        TaskActions::TransportCargo { dest_action, .. } => match dest_action {
            Action::DeliverConstruction(_, _) => config.allow_construction,
            _ => true,
        },
    }
}

#[derive(Clone)]
pub struct LogisticTaskManager {
    start_system: SystemSymbol,
    agent_controller: Arc<RwLock<Option<AgentController>>>,
    universe: Arc<Universe>,
    db_client: DbClient,

    // task_id -> (task, ship_symbol, timestamp)
    in_progress_tasks: Arc<DashMap<String, (Task, String, DateTime<Utc>)>>,
    take_tasks_mutex_guard: Arc<tokio::sync::Mutex<()>>,
}

impl LogisticTaskManager {
    pub async fn new(
        universe: &Arc<Universe>,
        db_client: &DbClient,
        start_system: &SystemSymbol,
    ) -> Self {
        let in_progress_tasks = db_client
            .load_task_manager_state(start_system)
            .await
            .unwrap_or_default();
        Self {
            start_system: start_system.clone(),
            universe: universe.clone(),
            db_client: db_client.clone(),
            agent_controller: Arc::new(RwLock::new(None)),
            in_progress_tasks: Arc::new(in_progress_tasks),
            take_tasks_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn in_progress_tasks(&self) -> Arc<DashMap<String, (Task, String, DateTime<Utc>)>> {
        self.in_progress_tasks.clone()
    }

    pub fn get_assigned_task_status(&self, task_id: &str) -> Option<(Task, String, DateTime<Utc>)> {
        self.in_progress_tasks.get(task_id).map(|v| v.clone())
    }

    pub fn set_agent_controller(&self, ac: &AgentController) {
        let mut agent_controller = self.agent_controller.write().unwrap();
        assert!(agent_controller.is_none());
        *agent_controller = Some(ac.clone());
    }

    fn probe_locations(&self) -> Vec<WaypointSymbol> {
        self.agent_controller()
            .probed_waypoints()
            .into_iter()
            .flat_map(|w| w.1)
            .collect()
    }
    fn agent_controller(&self) -> AgentController {
        self.agent_controller
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone()
    }

    // add trading tasks to the task list, if they don't already exist
    // (this function is not without side effects: it may buy ships)
    pub async fn generate_task_list(
        &self,
        system_symbol: &SystemSymbol,
        capacity_cap: i64,
        buy_ships: bool,
        min_profit: i64,
    ) -> Vec<Task> {
        let now = chrono::Utc::now();
        let waypoints: Vec<WaypointDetailed> =
            self.universe.get_system_waypoints(system_symbol).await;

        let mut tasks = Vec::new();

        let system_prefix = if system_symbol == &self.start_system {
            "".to_string()
        } else {
            format!("{}/", system_symbol)
        };

        // !! one day recalculate ship config here perhaps

        // execute contract actions + generate tasks
        // (todo)

        // execute ship_buy actions + generate tasks
        let (bought, shipyard_task_waypoint) = match buy_ships {
            true => self.agent_controller().try_buy_ships(None).await,
            false => (Vec::new(), None),
        };
        info!(
            "Task Controller buy phase resulted in {} ships bought",
            bought.len()
        );
        for ship_symbol in bought {
            debug!("Task controller bought ship {}", ship_symbol);
            self.agent_controller()._spawn_run_ship(ship_symbol).await;
        }
        if let Some(waypoint) = shipyard_task_waypoint {
            if &waypoint.system() == system_symbol {
                tasks.push(Task {
                    id: format!("{}buyships_{}", system_prefix, waypoint),
                    actions: TaskActions::VisitLocation {
                        waypoint: waypoint.clone(),
                        action: Action::TryBuyShips,
                    },
                    value: 200000,
                });
            }
        }

        // load markets
        let markets = self.universe.get_system_markets(system_symbol).await;
        let shipyards = self.universe.get_system_shipyards(system_symbol).await;

        // unique list of goods
        let mut goods = BTreeSet::new();
        for (_, market_opt) in &markets {
            if let Some(market) = market_opt {
                for good in &market.data.trade_goods {
                    goods.insert(good.symbol.clone());
                }
            }
        }

        // Construction tasks
        let jump_gate = waypoints
            .iter()
            .find(|w| w.is_jump_gate())
            .expect("Star system has no jump gate");

        // Markets deemed critical enough to be the exclusive recipient of certain goods
        let mut good_import_permits = BTreeMap::<&'static str, Vec<WaypointSymbol>>::new();
        // Goods where their flow is more important that prices (bypasses the STRONG MODERATE condition)
        let mut good_req_constant_flow = BTreeSet::<&'static str>::new();
        // Markets where we would like to cap the amount of units we import once we reach a target evolution
        // to prevent overevolution and yo-yo behaviours
        let mut market_capped_import = BTreeMap::<(WaypointSymbol, &'static str), i64>::new();

        let construction = self.universe.get_construction(&jump_gate.symbol).await;
        let mut construction = match &construction.data {
            Some(c) if c.is_complete => None,
            None => None,
            Some(c) => Some(c),
        };
        if CONFIG.no_gate_mode {
            construction = None;
        }

        if let Some(construction) = &construction {
            let fab_mat_markets = self
                .universe
                .search_waypoints(
                    &system_symbol,
                    &[
                        WaypointFilter::Imports("QUARTZ_SAND".to_string()),
                        WaypointFilter::Imports("IRON".to_string()),
                        WaypointFilter::Exports("FAB_MATS".to_string()),
                    ],
                )
                .await;
            assert!(fab_mat_markets.len() >= 1);
            let smeltery_markets = self
                .universe
                .search_waypoints(
                    &system_symbol,
                    &[
                        WaypointFilter::Imports("IRON_ORE".to_string()),
                        WaypointFilter::Imports("COPPER_ORE".to_string()),
                        WaypointFilter::Exports("IRON".to_string()),
                        WaypointFilter::Exports("COPPER".to_string()),
                    ],
                )
                .await;
            assert!(smeltery_markets.len() >= 1);
            let adv_circuit_markets = self
                .universe
                .search_waypoints(
                    &system_symbol,
                    &[
                        WaypointFilter::Imports("ELECTRONICS".to_string()),
                        WaypointFilter::Imports("MICROPROCESSORS".to_string()),
                        WaypointFilter::Exports("ADVANCED_CIRCUITRY".to_string()),
                    ],
                )
                .await;
            assert!(adv_circuit_markets.len() >= 1);

            let electronics_markets = self
                .universe
                .search_waypoints(
                    &system_symbol,
                    &[
                        WaypointFilter::Imports("SILICON_CRYSTALS".to_string()),
                        WaypointFilter::Imports("COPPER".to_string()),
                        WaypointFilter::Exports("ELECTRONICS".to_string()),
                    ],
                )
                .await;
            assert!(electronics_markets.len() >= 1);
            let microprocessor_markets = self
                .universe
                .search_waypoints(
                    &system_symbol,
                    &[
                        WaypointFilter::Imports("SILICON_CRYSTALS".to_string()),
                        WaypointFilter::Imports("COPPER".to_string()),
                        WaypointFilter::Exports("MICROPROCESSORS".to_string()),
                    ],
                )
                .await;
            assert!(microprocessor_markets.len() >= 1);

            let fab_mats = construction
                .materials
                .iter()
                .find(|m| m.trade_symbol == "FAB_MATS")
                .unwrap();
            let adv_circuit = construction
                .materials
                .iter()
                .find(|m| m.trade_symbol == "ADVANCED_CIRCUITRY")
                .unwrap();

            // FAB_MATS
            if fab_mats.fulfilled < fab_mats.required {
                // Clear all imports for the FAB_MAT chain
                good_import_permits.insert("FAB_MATS", vec![]);
                good_import_permits.insert("IRON", vec![]);
                good_import_permits.insert("QUARTZ_SAND", vec![]);
                good_import_permits.insert("IRON_ORE", vec![]);

                for market in &fab_mat_markets {
                    good_import_permits
                        .get_mut("IRON")
                        .unwrap()
                        .push(market.symbol.clone());
                    good_import_permits
                        .get_mut("QUARTZ_SAND")
                        .unwrap()
                        .push(market.symbol.clone());
                }
                for market in &smeltery_markets {
                    good_import_permits
                        .get_mut("IRON_ORE")
                        .unwrap()
                        .push(market.symbol.clone());
                }

                // Buy all supply chain components at constant flow
                // (except FAB_MATS, where we want to minimize cost)
                good_req_constant_flow.insert("IRON_ORE");
                good_req_constant_flow.insert("QUARTZ_SAND");
                good_req_constant_flow.insert("IRON");
                // good_req_constant_flow.insert("FAB_MATS");

                // Extra settings for the iron market:
                // Attempt to massage this market to cap its evolution at 120 trade volume
                // This is because I've observed this specific market over-evolve with an abundance of ore
                // and then proceed to consume more ore than available, leading to a IRON shortage
                for market in &fab_mat_markets {
                    market_capped_import.insert((market.symbol.clone(), "IRON"), 120);
                }
            }

            // ADVANCED_CIRCUITRY
            if adv_circuit.fulfilled < adv_circuit.required {
                // Clear all imports for the ADVANCED_CIRCUITRY chain
                good_import_permits.insert("ADVANCED_CIRCUITRY", vec![]);
                good_import_permits.insert("ELECTRONICS", vec![]);
                good_import_permits.insert("MICROPROCESSORS", vec![]);
                good_import_permits.insert("SILICON_CRYSTALS", vec![]);
                good_import_permits.insert("COPPER", vec![]);
                good_import_permits.insert("COPPER_ORE", vec![]);

                for market in adv_circuit_markets {
                    good_import_permits
                        .get_mut("ELECTRONICS")
                        .unwrap()
                        .push(market.symbol.clone());
                    good_import_permits
                        .get_mut("MICROPROCESSORS")
                        .unwrap()
                        .push(market.symbol.clone());
                }
                for market in electronics_markets {
                    good_import_permits
                        .get_mut("SILICON_CRYSTALS")
                        .unwrap()
                        .push(market.symbol.clone());
                    good_import_permits
                        .get_mut("COPPER")
                        .unwrap()
                        .push(market.symbol.clone());
                }
                for market in microprocessor_markets {
                    good_import_permits
                        .get_mut("SILICON_CRYSTALS")
                        .unwrap()
                        .push(market.symbol.clone());
                    good_import_permits
                        .get_mut("COPPER")
                        .unwrap()
                        .push(market.symbol.clone());
                }
                for market in smeltery_markets {
                    good_import_permits
                        .get_mut("COPPER_ORE")
                        .unwrap()
                        .push(market.symbol.clone());
                }

                // Buy all supply chain components at constant flow
                // (except ADVANCED_CIRCUITRY, where we want to minimize cost)
                good_req_constant_flow.insert("ELECTRONICS");
                good_req_constant_flow.insert("MICROPROCESSORS");
                good_req_constant_flow.insert("SILICON_CRYSTALS");
                good_req_constant_flow.insert("COPPER");
                good_req_constant_flow.insert("COPPER_ORE");
            }
        }

        let probe_locations = self.probe_locations();
        for (market_remote, market_opt) in &markets {
            let is_probed = probe_locations.contains(&market_remote.symbol);
            // Some fuel stop markets only trade fuel, so not worth visiting
            let is_pure_exchange =
                market_remote.exports.is_empty() && market_remote.imports.is_empty();
            if is_probed || is_pure_exchange {
                continue;
            }

            let reward: f64 = match market_opt {
                Some(market) => {
                    let age_minutes =
                        now.signed_duration_since(market.timestamp).num_seconds() as f64 / 60.;
                    match age_minutes {
                        f64::MIN..15. => continue,
                        15.0..30.0 => 1000.,
                        30.0..60.0 => 1000. + ((age_minutes - 30.0) * (4000. / 30.0)),
                        60.0..=f64::MAX => 5000.,
                        _ => panic!("Invalid age_minutes: {}", age_minutes),
                    }
                }
                None => 5000.,
            };
            tasks.push(Task {
                id: format!("{}refreshmarket_{}", system_prefix, market_remote.symbol),
                actions: TaskActions::VisitLocation {
                    waypoint: market_remote.symbol.clone(),
                    action: Action::RefreshMarket,
                },
                value: reward as i64,
            });
        }
        for (shipyard_remote, shipyard_opt) in &shipyards {
            let requires_visit = match shipyard_opt {
                Some(_shipyard) => false,
                None => true,
            };
            let is_probed = probe_locations.contains(&shipyard_remote.symbol);
            if requires_visit && !is_probed {
                tasks.push(Task {
                    id: format!(
                        "{}refreshshipyard_{}",
                        system_prefix, shipyard_remote.symbol
                    ),
                    actions: TaskActions::VisitLocation {
                        waypoint: shipyard_remote.symbol.clone(),
                        action: Action::RefreshShipyard,
                    },
                    value: 5000,
                });
            }
        }

        for good in goods {
            let req_constant_flow = good_req_constant_flow.contains(good.as_str());
            let trades = markets
                .iter()
                .filter_map(|(_, market_opt)| match market_opt {
                    Some(market) => {
                        let market_symbol = market.data.symbol.clone();
                        let trade = market.data.trade_goods.iter().find(|g| g.symbol == good);
                        trade.map(|trade| (market_symbol, trade))
                    }
                    None => None,
                })
                .collect::<Vec<_>>();
            let buy_trade_good = trades
                .iter()
                .filter(|(_, trade)| match trade._type {
                    Import => false,
                    Export => {
                        // Strong markets are where we'll make the most consistent profit
                        if !req_constant_flow && trade.activity == Some(Strong) {
                            trade.supply >= High
                        } else {
                            trade.supply >= Moderate
                        }
                    }
                    Exchange => true,
                })
                .min_by_key(|(_, trade)| trade.purchase_price);
            let sell_trade_good = trades
                .iter()
                .filter(|(market_symbol, trade)| {
                    let key = (market_symbol.clone(), good.as_str());
                    let evo_cap = market_capped_import.get(&key);
                    match evo_cap {
                        Some(evo_cap) => {
                            assert_eq!(
                                trade._type, Import,
                                "Only import trades should have an import evolution cap"
                            );
                            if trade.trade_volume >= *evo_cap {
                                // If we reached the evolution cap, then add an extra requirement to only IMPORT at LIMITED supply
                                // keep the import above scarce, and push limited into low moderate
                                trade.supply <= Limited
                            } else {
                                true
                            }
                        }
                        None => true,
                    }
                })
                .filter(|(_, trade)| match trade._type {
                    Import => trade.supply <= Moderate,
                    Export => false,
                    Exchange => true,
                })
                .filter(|(market, _)| match good_import_permits.get(good.as_str()) {
                    Some(allowlist) => allowlist.contains(&market),
                    None => true,
                })
                .max_by_key(|(_, trade)| trade.sell_price);
            let (buy_trade_good, sell_trade_good) = match (buy_trade_good, sell_trade_good) {
                (Some(buy), Some(sell)) => (buy, sell),
                _ => continue,
            };
            let units = min(
                min(
                    buy_trade_good.1.trade_volume,
                    sell_trade_good.1.trade_volume,
                ),
                capacity_cap,
            );
            let profit =
                (sell_trade_good.1.sell_price - buy_trade_good.1.purchase_price) * (units as i64);
            let can_afford = true; // logistic ships reserve their credits beforehand
            if profit >= min_profit && can_afford {
                debug!(
                    "{}: buy {} @ {} for ${}, sell @ {} for ${}, profit: ${}",
                    good,
                    units,
                    buy_trade_good.0,
                    buy_trade_good.1.purchase_price,
                    sell_trade_good.0,
                    sell_trade_good.1.sell_price,
                    profit
                );
                tasks.push(Task {
                    // full exclusivity seems a bit broad right now, but it's a start
                    id: format!("{}trade_{}", system_prefix, good),
                    actions: TaskActions::TransportCargo {
                        src: buy_trade_good.0.clone(),
                        dest: sell_trade_good.0.clone(),
                        src_action: Action::BuyGoods(good.clone(), units),
                        dest_action: Action::SellGoods(good.clone(), units),
                    },
                    value: profit,
                });
            }
        }
        tasks
    }

    async fn take_tasks_lock(&self) -> tokio::sync::MutexGuard<()> {
        match self.take_tasks_mutex_guard.try_lock() {
            Ok(guard) => guard,
            Err(_e) => {
                debug!("LogisticTaskManager::take_tasks is already running");
                let timeout = tokio::time::Duration::from_secs(20 * 60);
                match tokio::time::timeout(timeout, self.take_tasks_mutex_guard.lock()).await {
                    Ok(guard) => {
                        debug!("LogisticTaskManager::take_tasks lock acquired");
                        guard
                    }
                    Err(_e) => {
                        panic!("LogisticTaskManager::take_tasks lock timeout");
                    }
                }
            }
        }
    }

    // Provide a set of tasks for a single ship
    pub async fn take_tasks(
        &self,
        ship_symbol: &str,
        system_symbol: &SystemSymbol,
        config: &LogisticsScriptConfig,
        cargo_capacity: i64,
        engine_speed: i64,
        fuel_capacity: i64,
        start_waypoint: &WaypointSymbol,
        plan_length: Duration,
    ) -> ShipSchedule {
        let _guard = self.take_tasks_lock().await;
        assert_eq!(&start_waypoint.system(), system_symbol);

        // Cleanup in_progress_tasks for this ship
        self.in_progress_tasks.retain(|_k, v| v.1 != ship_symbol);
        let all_tasks = self
            .generate_task_list(system_symbol, cargo_capacity, true, config.min_profit)
            .await;
        self.agent_controller()
            .ledger
            .reserve_credits(ship_symbol, 5000 * cargo_capacity);

        // Filter out tasks that are already in progress
        // Also filter tasks outlawed by the config for this ship
        let available_tasks = all_tasks
            .into_iter()
            .filter(|task| !self.in_progress_tasks.contains_key(&task.id))
            .filter(|task| is_task_allowed(&task, config))
            .collect::<Vec<_>>();

        let market_waypoints = self
            .universe
            .get_system_waypoints(system_symbol)
            .await
            .into_iter()
            .filter(|w| w.is_market())
            .collect::<Vec<_>>();
        let (duration_matrix, distance_matrix) = self
            .universe
            .full_travel_matrix(&market_waypoints, fuel_capacity, engine_speed)
            .await;
        let logistics_ship = LogisticShip {
            symbol: ship_symbol.to_string(),
            capacity: cargo_capacity,
            speed: engine_speed,
            start_waypoint: start_waypoint.clone(),
            // available_from: Duration::seconds(0), // if we need to account for in-progress task(s)
        };
        let contraints = PlannerConstraints {
            plan_length: plan_length.num_seconds() as i64,
            max_compute_time: Duration::try_seconds(5).unwrap(),
        };
        let available_tasks_clone = available_tasks.clone();
        let (mut task_assignments, schedules) = if config.use_planner {
            tokio::task::spawn_blocking(move || {
                logistics_planner::plan::run_planner(
                    &[logistics_ship],
                    &available_tasks_clone,
                    &market_waypoints
                        .iter()
                        .map(|w| w.symbol.clone())
                        .collect::<Vec<_>>(),
                    &duration_matrix,
                    &distance_matrix,
                    &contraints,
                )
            })
            .await
            .unwrap()
        } else {
            let ship_schedule = ShipSchedule {
                ship: logistics_ship,
                actions: vec![],
            };
            (BTreeMap::new(), vec![ship_schedule])
        };
        assert_eq!(schedules.len(), 1);
        let mut schedule = schedules.into_iter().next().unwrap();

        // If 0 tasks were assigned, instead force assign the highest value task
        if schedule.actions.len() == 0 {
            let mut highest_value_task = None;
            let mut highest_value = 0;
            for task in &available_tasks {
                if task.value > highest_value {
                    highest_value = task.value;
                    highest_value_task = Some(task);
                }
            }
            if let Some(task) = highest_value_task {
                info!(
                    "Forcing assignment of task {} value: {}",
                    task.id, task.value
                );
                // add actions for the task
                match &task.actions {
                    TaskActions::VisitLocation { waypoint, action } => {
                        schedule.actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: waypoint.clone(),
                            action: action.clone(),
                            completes_task_id: Some(task.id.clone()),
                        });
                    }
                    TaskActions::TransportCargo {
                        src,
                        dest,
                        src_action,
                        dest_action,
                    } => {
                        schedule.actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: src.clone(),
                            action: src_action.clone(),
                            completes_task_id: None,
                        });
                        schedule.actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: dest.clone(),
                            action: dest_action.clone(),
                            completes_task_id: Some(task.id.clone()),
                        });
                    }
                };
                task_assignments.insert(task.id.clone(), ship_symbol.to_string());
            }
        }

        for (task_id, ship_symbol) in task_assignments {
            let task = available_tasks.iter().find(|t| t.id == task_id).unwrap();
            debug!("Assigned task {} to ship {}", task_id, ship_symbol);
            self.in_progress_tasks
                .insert(task_id, (task.clone(), ship_symbol.clone(), Utc::now()));
        }
        self.db_client
            .save_task_manager_state(&self.start_system, &self.in_progress_tasks)
            .await;

        schedule
    }

    pub async fn set_task_completed(&self, task_id: &str) {
        self.in_progress_tasks.remove(task_id);
        self.db_client
            .save_task_manager_state(&self.start_system, &self.in_progress_tasks)
            .await;
        debug!("Marking task {} as completed", task_id);
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_logistic_task_manager_state() {
        let in_progress_tasks = DashMap::<String, (Task, String, DateTime<Utc>)>::new();
        let task = Task {
            id: "test".to_string(),
            actions: TaskActions::VisitLocation {
                waypoint: WaypointSymbol::new("X1-S1-A1"),
                action: Action::RefreshMarket,
            },
            value: 20000,
        };
        in_progress_tasks.insert(
            "test".to_string(),
            (task.clone(), "ship".to_string(), Utc::now()),
        );
        let _json = serde_json::to_string(&in_progress_tasks).unwrap();
    }
}
