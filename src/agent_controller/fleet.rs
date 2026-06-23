use super::context::AgentContext;
use super::join_handles::JoinHandles;
use super::AgentController;
use crate::api_client::api_models::{BuyShipResponse, WaypointDetailed};
use crate::config::CONFIG;
use crate::models::{ShipNavStatus::*, *};
use crate::ship_config::ship_config_starter_system;
use crate::universe::WaypointFilter;
use crate::{ship_controller::ShipController, ship_scripts, tasks::LogisticTaskManager};
use dashmap::DashMap;
use futures::future::BoxFuture;
use log::*;
use serde_json::json;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use super::agent_controller::{AgentEra, AgentState};

#[derive(Clone, Debug)]
enum BuyShipResult {
    Bought(String),
    FailedNeverPurchase,
    FailedLowCredits,
    FailedNoShipyards,
    FailedNoPurchaser(Option<WaypointSymbol>),
}

#[derive(Clone)]
pub struct FleetManager {
    pub(super) ctx: Arc<AgentContext>,
    pub(super) state: Arc<Mutex<AgentState>>,
    ship_config: Arc<Mutex<Vec<ShipConfig>>>,
    pub(super) job_assignments: Arc<DashMap<String, String>>,
    pub(super) job_assignments_rev: Arc<DashMap<String, String>>,
    pub(super) hdls: Arc<JoinHandles>,
    task_manager: Arc<LogisticTaskManager>,
    try_buy_ships_mutex_guard: Arc<tokio::sync::Mutex<()>>,
}

impl FleetManager {
    pub fn new(
        ctx: Arc<AgentContext>,
        state: Arc<Mutex<AgentState>>,
        job_assignments: Arc<DashMap<String, String>>,
        job_assignments_rev: Arc<DashMap<String, String>>,
        hdls: Arc<JoinHandles>,
        task_manager: Arc<LogisticTaskManager>,
    ) -> Self {
        Self {
            ctx,
            state,
            ship_config: Arc::new(Mutex::new(vec![])),
            job_assignments,
            job_assignments_rev,
            hdls,
            task_manager,
            try_buy_ships_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn state(&self) -> AgentState {
        *self.state.lock().unwrap()
    }

    fn debug(&self, msg: &str) {
        debug!("[{}] {}", self.ctx.callsign, msg);
    }

    pub fn get_ship_config(&self) -> Vec<ShipConfig> {
        self.ship_config.lock().unwrap().clone()
    }

    pub fn set_ship_config(&self, config: Vec<ShipConfig>) {
        let mut ship_config = self.ship_config.lock().unwrap();
        *ship_config = config;
    }

    pub fn ship_controller(&self, ship_symbol: &str) -> ShipController {
        let ship = self.ctx.ships.get(ship_symbol).unwrap();
        ShipController::new(&self.ctx, ship.clone())
    }

    pub fn ship_assigned(&self, ship_symbol: &str) -> bool {
        self.job_assignments_rev.contains_key(ship_symbol)
    }

    pub fn job_assigned(&self, job_id: &str) -> bool {
        self.job_assignments.contains_key(job_id)
    }

    pub fn probed_waypoints(&self) -> Vec<(String, Vec<WaypointSymbol>)> {
        let ship_config = self.ship_config.lock().unwrap();
        ship_config
            .iter()
            .filter_map(|job| {
                if let ShipBehaviour::Probe(config) = &job.behaviour
                    && let Some(assignment) = self.job_assignments.get(&job.id)
                {
                    let ship_symbol = assignment.value().clone();
                    return Some((ship_symbol, config.waypoints.clone()));
                }
                None
            })
            .collect()
    }

    pub fn statically_probed_waypoints(&self) -> Vec<(String, WaypointSymbol)> {
        let ship_config = self.ship_config.lock().unwrap();
        let starting_system = self.ctx.starting_system();
        ship_config
            .iter()
            .filter_map(|job| {
                if let ShipBehaviour::Probe(config) = &job.behaviour {
                    let waypoints = &config.waypoints;
                    if waypoints.len() != 1 {
                        return None;
                    }
                    let waypoint_symbol = &waypoints[0];
                    if let Some(assignment) = self.job_assignments.get(&job.id) {
                        let ship = self.ctx.ships.get(assignment.value()).unwrap();
                        let ship = ship.lock().unwrap();
                        if ship.nav.status != InTransit
                            && ship.nav.waypoint_symbol == *waypoint_symbol
                        {
                            return Some((ship.symbol.clone(), waypoint_symbol.clone()));
                        }
                    }
                } else if let ShipBehaviour::ConstructionHauler = &job.behaviour
                    && let Some(assignment) = self.job_assignments.get(&job.id)
                {
                    let ship = self.ctx.ships.get(assignment.value()).unwrap();
                    let ship = ship.lock().unwrap();
                    if ship.nav.status != InTransit
                        && ship.nav.system_symbol != starting_system
                    {
                        return Some((ship.symbol.clone(), ship.nav.waypoint_symbol.clone()));
                    }
                }
                None
            })
            .collect()
    }

    pub fn reserve_credits_for_job(&self, job: &ShipConfig, ship_symbol: &str) {
        match &job.behaviour {
            ShipBehaviour::Logistics(_) => {}
            _ => return,
        }
        let ship = self.ctx.ships.get(ship_symbol).unwrap();
        let ship = ship.lock().unwrap();
        self.ctx
            .ledger
            .reserve_credits(ship_symbol, ship.cargo.capacity * 5000);
    }

    async fn buy_ship(&self, shipyard: &WaypointSymbol, ship_model: &str) -> String {
        self.debug(&format!("Buying {} at {}", &ship_model, &shipyard));
        let uri = "/my/ships";
        let body = json!({
            "shipType": ship_model,
            "waypointSymbol": shipyard,
        });
        let BuyShipResponse {
            agent,
            ship,
            transaction,
        } = self
            .ctx
            .api_client
            .post::<Data<BuyShipResponse>, _>(uri, &body)
            .await
            .data;
        let ship_symbol = ship.symbol.clone();
        self.debug(&format!(
            "Successfully bought ship {} for ${}",
            ship_symbol, transaction.price
        ));
        self.ctx
            .db
            .record_cash_txn(crate::database::CashTxn {
                ts: transaction.timestamp,
                type_: "ship_purchase",
                ship_symbol: Some(&ship_symbol),
                reference: Some(&transaction.ship_type),
                waypoint: Some(&transaction.waypoint_symbol.to_string()),
                units: None,
                amount: -transaction.price,
                realized_profit: None,
            })
            .await;
        self.ctx.update_agent(agent);
        self.ctx
            .ships
            .insert(ship_symbol.clone(), Arc::new(Mutex::new(ship)));
        ship_symbol
    }

    async fn try_buy_ships_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        match self.try_buy_ships_mutex_guard.try_lock() {
            Ok(guard) => guard,
            Err(_e) => {
                debug!("FleetManager::try_buy_ships is already running");
                let timeout = tokio::time::Duration::from_secs(30);
                match tokio::time::timeout(timeout, self.try_buy_ships_mutex_guard.lock()).await {
                    Ok(guard) => {
                        debug!("FleetManager::try_buy_ships lock acquired");
                        guard
                    }
                    Err(_e) => {
                        panic!("FleetManager::try_buy_ships lock timeout");
                    }
                }
            }
        }
    }

    async fn try_buy_ship(&self, purchaser: &Option<String>, job: &ShipConfig) -> BuyShipResult {
        let purchase_criteria = &job.purchase_criteria;
        debug!(
            "try_buy_ship ({:?}): {} {} {:?}",
            purchaser, job.id, job.ship_model, purchase_criteria
        );
        if purchase_criteria.never_purchase {
            return BuyShipResult::FailedNeverPurchase;
        }
        let purchase_system = match &purchase_criteria.system_symbol {
            Some(system_symbol) => system_symbol.clone(),
            None => self.ctx.starting_system(),
        };

        let mut shipyards = self
            .ctx
            .universe
            .search_shipyards(&purchase_system, &job.ship_model)
            .await;
        shipyards.sort_by_key(|x| x.1);

        if shipyards.is_empty() {
            return BuyShipResult::FailedNoShipyards;
        }
        let job_credit_reservation = match &job.behaviour {
            ShipBehaviour::Logistics(_) => {
                SHIP_MODELS[job.ship_model.as_str()].cargo_capacity * 5000
            }
            _ => 0,
        };
        let current_credits = self.ctx.ledger.available_credits();
        let cheapest_shipard = shipyards[0].0.clone();
        let can_afford_cheapest = current_credits >= shipyards[0].1 + job_credit_reservation;
        debug!("try_buy_ship Credits available: {}", current_credits);
        debug!(
            "try_buy_ship Extra credits for job reservation: {}",
            job_credit_reservation
        );

        let static_probes = self.statically_probed_waypoints();
        for (shipyard, cost) in &shipyards {
            if current_credits < cost + job_credit_reservation {
                break;
            }
            let ship_symbol: Option<String> = self
                .ctx
                .ships
                .iter()
                .find(|ship| {
                    let ship = ship.value().lock().unwrap();
                    if ship.nav.waypoint_symbol != *shipyard || ship.nav.status == InTransit {
                        return false;
                    }
                    let is_static_probe = static_probes.iter().any(|(s, _w)| s == &ship.symbol);
                    let is_purchaser = match &purchaser {
                        Some(purchaser) => ship.symbol == *purchaser,
                        None => false,
                    };
                    is_static_probe || is_purchaser
                })
                .map(|ship| ship.key().clone());
            let ship_controller = match &ship_symbol {
                Some(ship_symbol) => self.ship_controller(ship_symbol),
                None => {
                    if purchase_criteria.require_cheapest {
                        break;
                    } else {
                        continue;
                    }
                }
            };
            let bought_ship_symbol = self.buy_ship(shipyard, &job.ship_model).await;
            ship_controller.refresh_shipyard().await;
            let assigned = self.try_assign_ship(&bought_ship_symbol).await;
            assert!(assigned);
            return BuyShipResult::Bought(bought_ship_symbol);
        }
        if !can_afford_cheapest {
            return BuyShipResult::FailedLowCredits;
        }
        if purchase_criteria.allow_logistic_task {
            BuyShipResult::FailedNoPurchaser(Some(cheapest_shipard))
        } else {
            BuyShipResult::FailedNoPurchaser(None)
        }
    }

    pub async fn try_buy_ships(
        &self,
        purchaser: Option<String>,
    ) -> (Vec<String>, Option<WaypointSymbol>) {
        let _guard = self.try_buy_ships_lock().await;

        self.refresh_ship_config().await;

        if CONFIG.scrap_all_ships {
            return (vec![], None);
        }

        let mut purchased_ships = vec![];

        let ship_config = self.get_ship_config();
        // Skip never_purchase jobs here: they're intentionally never bought, and an
        // *unassigned* one (e.g. a retire slot left behind after a ship is scrapped) would
        // otherwise hit the FailedNeverPurchase early-return below and starve every job
        // after it in the list (jumpgate/intel/explorer purchases).
        for job in ship_config
            .iter()
            .filter(|job| !self.job_assigned(&job.id) && !job.purchase_criteria.never_purchase)
        {
            let result = self.try_buy_ship(&purchaser, job).await;
            match result {
                BuyShipResult::Bought(ship_symbol) => {
                    purchased_ships.push(ship_symbol);
                }
                BuyShipResult::FailedNeverPurchase => {
                    debug!("Not buying ship {}: never_purchase", job.ship_model);
                    return (purchased_ships, None);
                }
                BuyShipResult::FailedLowCredits => {
                    debug!("Not buying ship {}: low credits", job.ship_model);
                    return (purchased_ships, None);
                }
                BuyShipResult::FailedNoShipyards => {
                    debug!("Not buying ship {}: no shipyards", job.ship_model);
                    return (purchased_ships, None);
                }
                BuyShipResult::FailedNoPurchaser(waypoint) => {
                    if let Some(waypoint) = waypoint {
                        debug!(
                            "Not buying ship {}: no purchaser. Adding task @ {}",
                            job.ship_model, waypoint
                        );
                        return (purchased_ships, Some(waypoint));
                    }
                    debug!("Not buying ship {}: no purchaser", job.ship_model);
                    return (purchased_ships, None);
                }
            }
        }
        (purchased_ships, None)
    }

    pub async fn try_assign_ship(&self, ship_symbol: &str) -> bool {
        assert!(!self.job_assignments_rev.contains_key(ship_symbol));
        let ship = self.ctx.ships.get(ship_symbol).unwrap();
        let ship_model = { ship.lock().unwrap().model().unwrap() };
        let ship_config = self.get_ship_config();
        let job_opt = ship_config.iter().find(|job| {
            !self.job_assignments.contains_key(&job.id) && job.ship_model == ship_model
        });
        match job_opt {
            Some(job) => {
                self.job_assignments
                    .insert(job.id.clone(), ship_symbol.to_string());
                self.job_assignments_rev
                    .insert(ship_symbol.to_string(), job.id.clone());
                info!(
                    "Assigned {} ({}) to job {}",
                    ship_symbol, ship_model, job.id,
                );
                self.ctx
                    .db
                    .set_value(
                        &format!("{}/ship_assignments", self.ctx.callsign),
                        self.job_assignments.deref(),
                    )
                    .await;
                self.reserve_credits_for_job(job, ship_symbol);
                true
            }
            None => {
                debug!(
                    "No job available for ship {} of model {}",
                    ship_symbol, ship_model
                );
                false
            }
        }
    }

    pub async fn generate_ship_config(&self) -> Vec<ShipConfig> {
        let era = self.state().era;

        if era == AgentEra::InterSystem2 {
            let capital = self.ctx.faction_capital();
            let _waypoints: Vec<WaypointDetailed> =
                self.ctx.universe.get_system_waypoints(&capital).await;
            panic!("Late game not supported");
        }

        let start_system = self.ctx.starting_system();
        let waypoints: Vec<WaypointDetailed> =
            self.ctx.universe.get_system_waypoints(&start_system).await;
        let markets = self
            .ctx
            .universe
            .get_system_markets_remote(&start_system)
            .await;
        let shipyards = self
            .ctx
            .universe
            .get_system_shipyards_remote(&start_system)
            .await;

        let mut ships = vec![];
        let use_nonstatic_probes = true;
        let incl_outer_probes_and_siphons = !matches!(era, AgentEra::StartingSystem1);
        // The home-system build-out fleet (mining + siphon + the dedicated construction
        // hauler) exists only to fund and feed gate construction. While building it out
        // (StartingSystem*) we buy and run it; once past the gate we stop buying. The
        // slots stay emitted as never_purchase, which keeps any leftover ships assigned
        // so their scripts run and self-scrap on the same era cue
        // (ship_scripts::home_phase_done) — covering a restart past the gate, where they'd
        // otherwise load unassigned and never scrap.
        let in_home_phase = matches!(
            era,
            AgentEra::StartingSystem1 | AgentEra::StartingSystem2
        );
        if CONFIG.no_gate_mode {
            panic!("No gate mode not supported");
        }

        ships.append(&mut ship_config_starter_system(
            &waypoints,
            &markets,
            &shipyards,
            use_nonstatic_probes,
            incl_outer_probes_and_siphons,
            in_home_phase,
        ));

        if era == AgentEra::InterSystem1 {
            // Charting phase: fan probes out across the jump-gate network to map
            // the web of connections as quickly as possible. The home economy
            // (starter-system config above) keeps running to fund them.
            const NUM_JUMPGATE_PROBES: i64 = 20;
            for i in 0..NUM_JUMPGATE_PROBES {
                ships.push(ShipConfig {
                    id: format!("jumpgate_probe/{}", i),
                    ship_model: "SHIP_PROBE".to_string(),
                    // Bought in the starting system; mirror starter-probe criteria so a
                    // static probe at a shipyard (or the logistics planner) can purchase.
                    purchase_criteria: PurchaseCriteria {
                        allow_logistic_task: true,
                        require_cheapest: false,
                        ..PurchaseCriteria::default()
                    },
                    behaviour: ShipBehaviour::JumpgateProbe,
                });
            }

            // Remote-system market/shipyard intelligence: station a static probe at every
            // market and shipyard in each configured intel system. Probes self-route across
            // the jump-gate network (probe::goto_waypoint_anywhere) and refresh on arrival,
            // giving live prices + ship listings there. Only deploy once the target's gate is
            // charted — otherwise there's no route and the probes would just wait.
            for system in &CONFIG.intel_systems {
                let Some(gate) = self.ctx.universe.get_jumpgate_opt(system).await else {
                    continue;
                };
                if !self.ctx.universe.connections_known(&gate) {
                    continue;
                }
                let waypoints = self.ctx.universe.get_system_waypoints(system).await;
                for w in waypoints
                    .iter()
                    .filter(|w| w.is_market() || w.is_shipyard())
                {
                    ships.push(ShipConfig {
                        id: format!("intel_probe/{}", w.symbol),
                        ship_model: "SHIP_PROBE".to_string(),
                        purchase_criteria: PurchaseCriteria {
                            allow_logistic_task: true,
                            require_cheapest: false,
                            ..PurchaseCriteria::default()
                        },
                        behaviour: ShipBehaviour::Probe(ProbeScriptConfig {
                            waypoints: vec![w.symbol.clone()],
                            refresh_market: true,
                        }),
                    });
                }
            }

            // Bootstrap a SHIP_EXPLORER (fast + sensor array + warp) from a faction-capital
            // shipyard, then have it survey-scan remote systems. A purchaser probe parks at
            // the shipyard, refreshing it so the agent learns it sells explorers and can
            // transact there (note_waypoint_traits); the explorer is bought once and runs
            // the Survey behaviour against SURVEY_SYSTEM.
            if let Some(shipyard) = &CONFIG.explorer_shipyard {
                ships.push(ShipConfig {
                    id: format!("explorer_purchaser/{}", shipyard),
                    ship_model: "SHIP_PROBE".to_string(),
                    purchase_criteria: PurchaseCriteria {
                        allow_logistic_task: true,
                        require_cheapest: false,
                        ..PurchaseCriteria::default()
                    },
                    behaviour: ShipBehaviour::Probe(ProbeScriptConfig {
                        waypoints: vec![shipyard.clone()],
                        refresh_market: true,
                    }),
                });
                ships.push(ShipConfig {
                    id: "survey_explorer".to_string(),
                    ship_model: "SHIP_EXPLORER".to_string(),
                    purchase_criteria: PurchaseCriteria {
                        system_symbol: Some(shipyard.system()),
                        require_cheapest: false,
                        ..PurchaseCriteria::default()
                    },
                    behaviour: ShipBehaviour::Survey,
                });
            }
        }
        ships
    }

    pub async fn is_jumpgate_finished(&self) -> bool {
        let jump_gate_symbol = {
            let waypoints = self
                .ctx
                .universe
                .search_waypoints(&self.ctx.starting_system(), &[WaypointFilter::JumpGate])
                .await;
            assert!(waypoints.len() == 1);
            waypoints[0].symbol.clone()
        };
        let construction = self.ctx.universe.get_construction(&jump_gate_symbol).await;
        match &construction.data {
            None => true,
            Some(x) => x.is_complete,
        }
    }

    pub async fn refresh_ship_config(&self) {
        let ship_config = self.generate_ship_config().await;
        self.set_ship_config(ship_config.clone());

        let mut keys_to_remove = Vec::new();
        for it in self.job_assignments.iter() {
            let (job_id, ship_symbol) = it.pair();
            let job_exists = ship_config.iter().any(|job| job.id == *job_id);
            let ship_exists = self.ctx.ships.contains_key(ship_symbol);
            if !job_exists {
                warn!(
                    "Unassigning ship {} from non-existant job {}",
                    ship_symbol, job_id
                );
                keys_to_remove.push((job_id.clone(), ship_symbol.clone()));
            }
            if !ship_exists {
                warn!(
                    "Unassigning non-existant ship {} from job {}",
                    ship_symbol, job_id
                );
                keys_to_remove.push((job_id.clone(), ship_symbol.clone()));
            }
        }
        for (job_id, ship_symbol) in keys_to_remove {
            self.job_assignments.remove(&job_id);
            self.job_assignments_rev.remove(&ship_symbol);
        }
        self.ctx
            .db
            .set_value(
                &format!("{}/ship_assignments", self.ctx.callsign),
                self.job_assignments.deref(),
            )
            .await;

        for ship in self.ctx.ships.iter() {
            let ship_symbol = ship.key().clone();
            if !self.ship_assigned(&ship_symbol) {
                self.try_assign_ship(&ship_symbol).await;
            }
        }

        self.ctx.ledger.reserve_credits("FUEL", 10_000);
        if self.is_jumpgate_finished().await {
            self.ctx.ledger.reserve_credits("JUMPGATE_COSTS", 500_000);
        }
        for ship_config in ship_config {
            if let Some(ship_symbol) = &self.job_assignments.get(&ship_config.id) {
                let ship_symbol: &String = ship_symbol.value();
                self.reserve_credits_for_job(&ship_config, ship_symbol);
            }
        }
    }

    pub async fn update_era(&self, era: AgentEra) {
        let state = {
            let mut state = self.state.lock().unwrap();
            state.era = era;
            *state
        };
        self.ctx
            .db
            .set_value(&format!("{}/state", self.ctx.callsign), &state)
            .await;
    }

    pub async fn check_era_advance(&self) {
        if let Some(era_override) = CONFIG.era_override {
            let state = self.state();
            if era_override != state.era {
                info!(
                    "Agent {} advancing to era {:?} (override)",
                    self.ctx.callsign, era_override
                );
                self.update_era(era_override).await;
            }
            return;
        }
        loop {
            let current_era = self.state().era;
            let next_era = match current_era {
                AgentEra::StartingSystem1 => {
                    let credits = self.ctx.ledger.available_credits();
                    if credits >= 800_000 {
                        Some(AgentEra::StartingSystem2)
                    } else {
                        None
                    }
                }
                AgentEra::StartingSystem2 => {
                    // Once the home jump gate is built, start charting the network.
                    if self.is_jumpgate_finished().await {
                        Some(AgentEra::InterSystem1)
                    } else {
                        None
                    }
                }
                AgentEra::InterSystem1 => None,
                AgentEra::InterSystem2 => None,
            };
            match next_era {
                None => break,
                Some(next_era) => {
                    assert_ne!(current_era, next_era);
                    info!(
                        "Agent {} advancing to era {:?}",
                        self.ctx.callsign, next_era
                    );
                    self.update_era(next_era).await;
                }
            }
        }
    }

    pub fn spawn_run_ship<'a>(
        &'a self,
        ac: &'a AgentController,
        ship_symbol: String,
    ) -> BoxFuture<'a, ()> {
        Box::pin(self._spawn_run_ship(ac, ship_symbol))
    }

    async fn _spawn_run_ship(&self, ac: &AgentController, ship_symbol: String) {
        debug!("Spawning task for {}", ship_symbol);

        let job_id_opt = self.job_assignments_rev.get(&ship_symbol);
        let scrap = CONFIG.scrap_all_ships || (job_id_opt.is_none() && CONFIG.scrap_unassigned);
        if scrap {
            let ship_controller = self.ship_controller(&ship_symbol);
            let join_hdl = tokio::spawn(async move {
                ship_scripts::scrap::run(ship_controller).await;
            });
            let name = format!("{}:scrap", ship_symbol);
            self.hdls.push(&name, join_hdl);
            return;
        }

        match job_id_opt {
            Some(job_id) => {
                let ship_config = self.get_ship_config();
                let job_spec = ship_config
                    .iter()
                    .find(|s| s.id == *job_id)
                    .unwrap_or_else(|| panic!("No job found for {}", *job_id));
                if !CONFIG.job_id_filter.is_match(&job_spec.id) {
                    return;
                }
                let ship_controller = self.ship_controller(&ship_symbol);
                let ship = ship_controller.ship();
                if ship.engine.condition.unwrap() < 0.0 {
                    warn!(
                        "Ship {} has engine condition {}",
                        ship_symbol,
                        ship.engine.condition.unwrap()
                    );
                    return;
                }
                if ship.frame.condition.unwrap() < 0.0 {
                    warn!(
                        "Ship {} has frame condition {}",
                        ship_symbol,
                        ship.frame.condition.unwrap()
                    );
                    return;
                }
                if ship.reactor.condition.unwrap() < 0.0 {
                    warn!(
                        "Ship {} has reactor condition {}",
                        ship_symbol,
                        ship.reactor.condition.unwrap()
                    );
                    return;
                }

                let join_hdl = match &job_spec.behaviour {
                    ShipBehaviour::Probe(config) => {
                        let config = config.clone();
                        tokio::spawn(async move {
                            ship_scripts::probe::run(ship_controller, &config).await;
                        })
                    }
                    ShipBehaviour::Logistics(config) => {
                        let task_manager = self.task_manager.clone();
                        let ac = ac.clone();
                        let config = config.clone();
                        tokio::spawn(async move {
                            ship_scripts::logistics::run(
                                ship_controller,
                                task_manager,
                                config,
                                ac,
                            )
                            .await;
                        })
                    }
                    ShipBehaviour::SiphonDrone => {
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::siphon::run_drone(ship_controller, ac).await;
                        })
                    }
                    ShipBehaviour::SiphonShuttle => {
                        let db = self.ctx.db.clone();
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::siphon::run_shuttle(ship_controller, db, ac).await;
                        })
                    }
                    ShipBehaviour::MiningDrone => {
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::mining::run_mining_drone(ship_controller, ac).await;
                        })
                    }
                    ShipBehaviour::MiningShuttle => {
                        let db = self.ctx.db.clone();
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::mining::run_shuttle(ship_controller, db, ac).await;
                        })
                    }
                    ShipBehaviour::MiningSurveyor => {
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::mining::run_surveyor(ship_controller, ac).await;
                        })
                    }
                    ShipBehaviour::ConstructionHauler => {
                        let db = self.ctx.db.clone();
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::construction::run_hauler(ship_controller, db, ac).await;
                        })
                    }
                    ShipBehaviour::JumpgateProbe => {
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::probe_exploration::run_jumpgate_probe(
                                ship_controller,
                                ac,
                            )
                            .await;
                        })
                    }
                    ShipBehaviour::Explorer => {
                        let db = self.ctx.db.clone();
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::exploration::run_explorer(ship_controller, db, ac).await;
                        })
                    }
                    ShipBehaviour::Survey => {
                        let ac = ac.clone();
                        tokio::spawn(async move {
                            ship_scripts::survey::run_scanner(ship_controller, ac).await;
                        })
                    }
                };
                let name = format!("{}:{}", ship_symbol, job_spec.id);
                self.hdls.push(&name, join_hdl);
            }
            None => {
                debug!("Warning. No job assigned to ship {}", ship_symbol);
            }
        }
    }
}
