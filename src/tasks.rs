use crate::agent_controller::{AgentController, ContractStatus};
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
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, RwLock};

fn is_task_allowed(task: &Task, config: &LogisticsScriptConfig) -> bool {
    if let TaskActions::TransportCargo { dest_action, .. } = &task.actions
        && let Action::DeliverContract(_, _) = dest_action {
            // Allow contract delivery to bypass the normal waypoint allowlist
            return true;
        }

    if let Some(waypoint_allowlist) = &config.waypoint_allowlist {
        match &task.actions {
            TaskActions::VisitLocation { waypoint, .. } => {
                if !waypoint_allowlist.contains(waypoint) {
                    return false;
                }
            }
            TaskActions::TransportCargo { src, dest, .. } => {
                if !waypoint_allowlist.contains(src) || !waypoint_allowlist.contains(dest) {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogisticsShip {
    pub system_symbol: SystemSymbol,
    pub config: LogisticsScriptConfig,
    pub cargo_capacity: i64,
    pub engine_speed: i64,
    pub fuel_capacity: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskManagerState {
    in_progress_tasks: DashMap<String, (Task, String, DateTime<Utc>)>,
    ship_tasks: DashMap<String, VecDeque<ScheduledAction>>,
    logistics_ships: DashMap<String, LogisticsShip>,
    planner_run_count: u64,
}

#[derive(Clone)]
pub struct LogisticTaskManager {
    start_system: SystemSymbol,
    agent_controller: Arc<RwLock<Option<AgentController>>>,
    universe: Arc<Universe>,
    db_client: DbClient,
    state: Arc<RwLock<TaskManagerState>>,
    take_tasks_mutex_guard: Arc<tokio::sync::Mutex<()>>,
}

impl LogisticTaskManager {
    pub async fn new(
        universe: &Arc<Universe>,
        db_client: &DbClient,
        start_system: &SystemSymbol,
    ) -> Self {
        let state = db_client
            .load_task_manager_state(start_system)
            .await
            .unwrap_or_else(|| TaskManagerState {
                in_progress_tasks: DashMap::new(),
                ship_tasks: DashMap::new(),
                logistics_ships: DashMap::new(),
                planner_run_count: 0,
            });
        Self {
            start_system: start_system.clone(),
            universe: universe.clone(),
            db_client: db_client.clone(),
            agent_controller: Arc::new(RwLock::new(None)),
            state: Arc::new(RwLock::new(state)),
            take_tasks_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn get_planner_run_count(&self) -> u64 {
        self.state.read().unwrap().planner_run_count
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
        let system_prefix = format!("{}/", system_symbol);

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
            self.agent_controller().spawn_run_ship(ship_symbol).await;
        }
        if let Some(waypoint) = shipyard_task_waypoint
            && waypoint.system() == *system_symbol {
                tasks.push(Task {
                    id: format!("{}buyships_{}", system_prefix, waypoint),
                    actions: TaskActions::VisitLocation {
                        waypoint: waypoint.clone(),
                        action: Action::TryBuyShips,
                    },
                    value: 200000,
                });
            }

        // Contract tasks
        let contract = match self.agent_controller().contract_tick(false).await {
            ContractStatus::RequiresLogisticsTask(src_market, dst_market, trade, units) => {
                Some((src_market, dst_market, trade, units))
            }
            _ => None,
        };
        let contract_good = contract
            .as_ref()
            .map(|(_, _, trade, _)| trade.symbol.clone());

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
                    system_symbol,
                    &[
                        WaypointFilter::Imports("QUARTZ_SAND".to_string()),
                        WaypointFilter::Imports("IRON".to_string()),
                        WaypointFilter::Exports("FAB_MATS".to_string()),
                    ],
                )
                .await;
            assert!(!fab_mat_markets.is_empty());
            let smeltery_markets = self
                .universe
                .search_waypoints(
                    system_symbol,
                    &[
                        WaypointFilter::Imports("IRON_ORE".to_string()),
                        WaypointFilter::Imports("COPPER_ORE".to_string()),
                        WaypointFilter::Exports("IRON".to_string()),
                        WaypointFilter::Exports("COPPER".to_string()),
                    ],
                )
                .await;
            assert!(!smeltery_markets.is_empty());
            let adv_circuit_markets = self
                .universe
                .search_waypoints(
                    system_symbol,
                    &[
                        WaypointFilter::Imports("ELECTRONICS".to_string()),
                        WaypointFilter::Imports("MICROPROCESSORS".to_string()),
                        WaypointFilter::Exports("ADVANCED_CIRCUITRY".to_string()),
                    ],
                )
                .await;
            assert!(!adv_circuit_markets.is_empty());

            let electronics_markets = self
                .universe
                .search_waypoints(
                    system_symbol,
                    &[
                        WaypointFilter::Imports("SILICON_CRYSTALS".to_string()),
                        WaypointFilter::Imports("COPPER".to_string()),
                        WaypointFilter::Exports("ELECTRONICS".to_string()),
                    ],
                )
                .await;
            assert!(!electronics_markets.is_empty());
            let microprocessor_markets = self
                .universe
                .search_waypoints(
                    system_symbol,
                    &[
                        WaypointFilter::Imports("SILICON_CRYSTALS".to_string()),
                        WaypointFilter::Imports("COPPER".to_string()),
                        WaypointFilter::Exports("MICROPROCESSORS".to_string()),
                    ],
                )
                .await;
            assert!(!microprocessor_markets.is_empty());

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
                        f64::MIN..5. => continue,
                        // Very small reward
                        5.0..15. => 1.,
                        // Standard
                        15.0..30.0 => 1000.,
                        30.0..60.0 => 2000.,
                        60.0..=f64::MAX => 4000.,
                        _ => panic!("Invalid age_minutes: {}", age_minutes),
                    }
                }
                None => 4000.,
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
                    value: 1000,
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
                    Some(allowlist) => allowlist.contains(market),
                    None => true,
                })
                .max_by_key(|(_, trade)| trade.sell_price);

            // Contract task
            if !CONFIG.disable_contract_tasks && contract_good.as_ref() == Some(&good) {
                let (src_market, dst_market, trade, missing) = match &contract {
                    Some(contract) => contract,
                    None => continue,
                };
                let units = min(*missing, capacity_cap);
                debug!(
                    "Contract task: buy {} {} @ {} for ${}",
                    units, good, src_market, trade.purchase_price
                );
                tasks.push(Task {
                    id: format!("{}contract_{}", system_prefix, good),
                    actions: TaskActions::TransportCargo {
                        src: src_market.clone(),
                        dest: dst_market.clone(),
                        src_action: Action::BuyGoods(good.clone(), units),
                        dest_action: Action::DeliverContract(good.clone(), units),
                    },
                    value: 50_000, // Very high priority
                });
                continue; // Don't add a trading task for the same good
            }

            // Trading task
            if !CONFIG.disable_trading_tasks {
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
                let profit = (sell_trade_good.1.sell_price - buy_trade_good.1.purchase_price)
                    * (units as i64);
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
        }
        tasks
    }

    async fn take_tasks_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
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

    pub async fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut TaskManagerState),
    {
        let state = {
            let mut state = self.state.write().unwrap();
            f(&mut state);
            (*state).clone()
        };
        self.db_client
            .save_task_manager_state(&self.start_system, &state)
            .await;
        *self.state.write().unwrap() = state;
    }

    fn assert_no_in_progress_tasks(&self, ship_symbol: &str) {
        let state = self.state.read().unwrap();
        assert!(
            !state
                .in_progress_tasks
                .iter()
                .any(|entry| entry.value().1 == ship_symbol),
            "Ship {} already has an in-progress task",
            ship_symbol
        );
    }

    fn assert_no_queued_tasks(&self, ship_symbol: &str) {
        let state = self.state.read().unwrap();
        let ship_tasks = state.ship_tasks.get(ship_symbol);
        assert!(
            ship_tasks.is_none_or(|queue| queue.is_empty()),
            "Ship {} already has scheduled tasks",
            ship_symbol
        );
    }

    pub async fn take_tasks(
        &self,
        ship_symbol: &str,
        start_waypoint: &WaypointSymbol,
    ) -> Option<ScheduledAction> {
        let _guard = self.take_tasks_lock().await;
        let system_symbol = &self.start_system;

        // Assert there are no in progress tasks or scheduled tasks for this ship
        self.assert_no_in_progress_tasks(ship_symbol);
        self.assert_no_queued_tasks(ship_symbol);

        let logistics_ship_config = self
            .state
            .read()
            .unwrap()
            .logistics_ships
            .get(ship_symbol)?
            .value()
            .clone();
        let cargo_capacity = logistics_ship_config.cargo_capacity;
        let config = &logistics_ship_config.config;
        let engine_speed = logistics_ship_config.engine_speed;
        let fuel_capacity = logistics_ship_config.fuel_capacity;

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
            .filter(|task| {
                !self
                    .state
                    .read()
                    .unwrap()
                    .in_progress_tasks
                    .contains_key(&task.id)
            })
            .filter(|task| is_task_allowed(task, config))
            .collect::<Vec<_>>();

        if available_tasks.is_empty() {
            return None;
        }

        // Run planner
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
        };
        let schedules = if config.use_planner {
            let planner_config = config.planner_config.as_ref().unwrap();
            let run_count = self.get_planner_run_count();
            let plan_length = match &planner_config.plan_length {
                PlanLength::Fixed(duration) => *duration,
                PlanLength::Ramping(min, max, ramp_factor) => {
                    // Safety check: if run_count is high enough that min * ramp_factor^run_count would overflow,
                    // just return max duration
                    if run_count >= 10 {
                        *max
                    } else {
                        let duration =
                            min.num_seconds() as f64 * (*ramp_factor).powf(run_count as f64);
                        let duration = duration.min(max.num_seconds() as f64);
                        Duration::try_seconds(duration as i64).unwrap()
                    }
                }
            };
            let contraints = PlannerConstraints {
                plan_length: plan_length.num_seconds(),
                max_compute_time: Duration::try_seconds(5).unwrap(),
            };
            let available_tasks_clone = available_tasks.clone();
            info!(
                "Planning tasks for ship {}, tasks: {}, length: {}s",
                ship_symbol,
                available_tasks_clone.len(),
                plan_length.num_seconds()
            );
            // debug!("Available tasks: {:?}", available_tasks_clone);
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
            vec![ShipSchedule {
                ship: logistics_ship,
                actions: vec![],
            }]
        };
        assert_eq!(schedules.len(), 1);
        let mut actions = schedules.into_iter().next().unwrap().actions;
        info!("Planner returned {} actions", actions.len());

        // If 0 tasks were assigned, instead force assign the highest value task
        if actions.is_empty() {
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
                        actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: waypoint.clone(),
                            action: action.clone(),
                            task_id: task.id.clone(),
                            completes_task: true,
                        });
                    }
                    TaskActions::TransportCargo {
                        src,
                        dest,
                        src_action,
                        dest_action,
                    } => {
                        actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: src.clone(),
                            action: src_action.clone(),
                            task_id: task.id.clone(),
                            completes_task: false,
                        });
                        actions.push(ScheduledAction {
                            timestamp: 0.0,
                            waypoint: dest.clone(),
                            action: dest_action.clone(),
                            task_id: task.id.clone(),
                            completes_task: true,
                        });
                    }
                };
            }
        }

        // Store the task in the ship's queue, and also update in_progress_tasks
        self.update_state(|state| {
            for action in &actions {
                if action.completes_task {
                    let task = available_tasks
                        .iter()
                        .find(|t| t.id == action.task_id)
                        .unwrap();
                    state.in_progress_tasks.insert(
                        action.task_id.clone(),
                        (task.clone(), ship_symbol.to_string(), Utc::now()),
                    );
                    debug!(
                        "[{}]\tAssigned task {} to ship {}",
                        task.value, action.task_id, ship_symbol
                    );
                }
            }
            let queue = VecDeque::from(actions);
            state.ship_tasks.insert(ship_symbol.to_string(), queue);
            state.planner_run_count += 1;
        })
        .await;

        // Return the first action
        self.state
            .read()
            .unwrap()
            .ship_tasks
            .get(ship_symbol)
            .and_then(|queue| queue.front().cloned())
    }

    pub fn get_next_action(&self, ship_symbol: &str) -> Option<ScheduledAction> {
        self.state
            .read()
            .unwrap()
            .ship_tasks
            .get(ship_symbol)
            .and_then(|queue| queue.front().cloned())
    }

    pub async fn complete_action(&self, ship_symbol: &str, action: &ScheduledAction) {
        self.update_state(|state| {
            // 1. Remove action from ship's queue
            let mut ship_tasks = state.ship_tasks.get_mut(ship_symbol).unwrap();
            let front: &ScheduledAction = ship_tasks.front().unwrap();
            assert_eq!(front, action);
            ship_tasks.pop_front();

            // 2. If the action completes a task, remove the task from in_progress_tasks
            if action.completes_task {
                state.in_progress_tasks.remove(&action.task_id);
            }
        })
        .await;
    }

    pub async fn register_ship(
        &self,
        ship_symbol: &str,
        system_symbol: &SystemSymbol,
        config: &LogisticsScriptConfig,
        cargo_capacity: i64,
        engine_speed: i64,
        fuel_capacity: i64,
    ) {
        self.update_state(|state| {
            state.logistics_ships.insert(
                ship_symbol.to_string(),
                LogisticsShip {
                    system_symbol: system_symbol.clone(),
                    config: config.clone(),
                    cargo_capacity,
                    engine_speed,
                    fuel_capacity,
                },
            );
        })
        .await;
    }

    pub async fn get_next_task(
        &self,
        ship_symbol: &str,
        start_waypoint: &WaypointSymbol,
    ) -> Option<ScheduledAction> {
        // First try to get the next action from an existing task
        if let Some(action) = self.get_next_action(ship_symbol) {
            return Some(action);
        }

        // If no existing task, try to take a new one
        self.take_tasks(ship_symbol, start_waypoint).await
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
