use super::context::AgentContext;
use super::contract_manager::ContractManager;
use super::exploration::ExplorationManager;
use super::fleet::FleetManager;
use super::join_handles::JoinHandles;
use super::ledger::Ledger;
use crate::broker::CargoBroker;
use crate::models::*;
use crate::survey_manager::SurveyManager;
use chrono::Utc;
use crate::{
    api_client::ApiClient,
    database::DbClient,
    models::{Agent, Ship},
    tasks::LogisticTaskManager,
    universe::Universe,
};
use dashmap::DashMap;
use futures::future::BoxFuture;
use log::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use strum::EnumString;
use tokio::time::MissedTickBehavior;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString)]
pub enum AgentEra {
    StartingSystem1,
    StartingSystem2,
    InterSystem1,
    InterSystem2,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AgentState {
    pub era: AgentEra,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            era: AgentEra::StartingSystem1,
        }
    }
}

#[derive(Clone)]
pub struct AgentController {
    pub ctx: Arc<AgentContext>,
    pub fleet: FleetManager,
    pub contracts: ContractManager,
    pub exploration: ExplorationManager,

    pub task_manager: Arc<LogisticTaskManager>,
}

impl AgentController {
    pub fn agent(&self) -> Agent {
        self.ctx.agent()
    }
    pub fn state(&self) -> AgentState {
        self.fleet.state()
    }
    pub fn ships(&self) -> Vec<(String, Ship, String, String)> {
        self.ctx
            .ships
            .iter()
            .map(|x| {
                let ship_symbol = x.key().clone();
                let ship = x.value().lock().unwrap().clone();
                let job_id = self
                    .fleet
                    .job_assignments_rev
                    .get(&ship_symbol)
                    .map(|x| x.value().clone())
                    .unwrap_or_default();
                let descr = self
                    .ctx
                    .ship_state_description
                    .get(&ship_symbol)
                    .map(|x| x.value().clone())
                    .unwrap_or_default();
                (ship_symbol, ship, job_id, descr)
            })
            .collect()
    }

    // Contract delegation methods
    pub fn get_current_contract_id(&self) -> Option<String> {
        self.contracts.get_current_contract_id()
    }
    pub fn spawn_contract_task(&self) {
        self.contracts.spawn_contract_task();
    }
    pub async fn contract_tick(&self, may_skip: bool) -> super::ContractStatus {
        self.contracts.contract_tick(may_skip).await
    }

    pub async fn new(
        api_client: &ApiClient,
        db: &DbClient,
        universe: &Arc<Universe>,
        callsign: &str,
    ) -> Self {
        let agent: Arc<Mutex<Agent>> = {
            let agent = api_client.get_agent().await;
            assert_eq!(agent.symbol, callsign);
            Arc::new(Mutex::new(agent))
        };
        let ships: Arc<DashMap<String, Arc<Mutex<Ship>>>> = {
            let ships_vec: Vec<Ship> = api_client.get_all_ships().await;
            let ships = Arc::new(DashMap::new());
            for ship in ships_vec {
                ships.insert(ship.symbol.clone(), Arc::new(Mutex::new(ship)));
            }
            ships
        };
        let contract: Option<Contract> = api_client.get_contract().await;

        let system_symbol = agent.lock().unwrap().headquarters.system();
        universe.ensure_system_loaded(&system_symbol).await;

        let job_assignments: DashMap<String, String> = db
            .get_value(&format!("{}/ship_assignments", callsign))
            .await
            .unwrap_or_default();
        let job_assignments_rev = job_assignments
            .iter()
            .map(|x| {
                let (k, v) = x.pair();
                (v.clone(), k.clone())
            })
            .collect();
        let probe_jumpgate_reservations = db.get_probe_jumpgate_reservations(callsign).await;
        let explorer_reservations = db.get_explorer_reservations(callsign).await;
        let task_manager = LogisticTaskManager::new(universe, db, &system_symbol).await;
        let survey_manager = SurveyManager::new(db).await;

        let initial_credits = {
            let agent = agent.lock().unwrap();
            agent.credits
        };
        let ledger = Ledger::new(initial_credits);
        // Restore in-transit cargo cost basis so a restart doesn't make the next
        // sale of pre-restart cargo read as 100% profit.
        if let Some(snapshot) = db
            .get_value::<crate::agent_controller::ledger::LedgerSnapshot>(&format!(
                "ledger/{}",
                callsign
            ))
            .await
        {
            ledger.restore(snapshot);
        }
        let state: AgentState = db
            .get_value(&format!("{}/state", callsign))
            .await
            .unwrap_or_default();

        let ctx = Arc::new(AgentContext {
            callsign: callsign.to_string(),
            agent,
            ships,
            contract: Arc::new(Mutex::new(contract)),
            api_client: api_client.clone(),
            db: db.clone(),
            universe: universe.clone(),
            cargo_broker: Arc::new(CargoBroker::new()),
            survey_manager: Arc::new(survey_manager),
            ledger: Arc::new(ledger),
            ship_state_description: Arc::new(DashMap::new()),
        });

        let hdls = Arc::new(JoinHandles::new());
        let task_manager = Arc::new(task_manager);

        let fleet = FleetManager::new(
            ctx.clone(),
            Arc::new(Mutex::new(state)),
            Arc::new(job_assignments),
            Arc::new(job_assignments_rev),
            hdls.clone(),
            task_manager.clone(),
        );

        let contracts = ContractManager::new(ctx.clone(), fleet.clone());
        let exploration = ExplorationManager::new(
            ctx.clone(),
            probe_jumpgate_reservations,
            explorer_reservations,
        );

        let agent_controller = Self {
            ctx,
            fleet,
            contracts,
            exploration,
            task_manager,
        };
        agent_controller
            .task_manager
            .set_agent_controller(&agent_controller);
        let credits = agent_controller.ctx.ledger.credits();
        let num_ships = agent_controller.num_ships();
        info!(
            "Loaded agent {} ${} with {} ships",
            callsign, credits, num_ships
        );
        info!(
            "{} effective reserved credits, {} available",
            agent_controller.ctx.ledger.effective_reserved_credits(),
            agent_controller.ctx.ledger.available_credits()
        );
        agent_controller
    }

    pub fn starting_system(&self) -> SystemSymbol {
        self.ctx.starting_system()
    }
    pub fn num_ships(&self) -> usize {
        self.ctx.ships.len()
    }
    pub fn update_agent(&self, agent_upd: Agent) {
        self.ctx.update_agent(agent_upd);
    }
    pub fn update_contract(&self, contract: Contract) {
        self.ctx.update_contract(contract);
    }

    // Delegation methods for backward compatibility
    pub fn probed_waypoints(&self) -> Vec<(String, Vec<WaypointSymbol>)> {
        self.fleet.probed_waypoints()
    }
    pub fn statically_probed_waypoints(&self) -> Vec<(String, WaypointSymbol)> {
        self.fleet.statically_probed_waypoints()
    }
    pub async fn try_buy_ships(
        &self,
        purchaser: Option<String>,
    ) -> (Vec<String>, Option<WaypointSymbol>) {
        self.fleet.try_buy_ships(purchaser).await
    }
    pub fn spawn_run_ship(&self, ship_symbol: String) -> BoxFuture<'_, ()> {
        self.fleet.spawn_run_ship(self, ship_symbol)
    }

    pub async fn run(&self) {
        let ctx = self.ctx.clone();
        self.fleet.hdls.push(
            "cargo broker",
            tokio::spawn(async move {
                let broker = ctx.cargo_broker.clone();
                broker.run(Box::new(ctx)).await;
            }),
        );
        let self_clone = self.clone();
        self.fleet.hdls.push(
            "agent startup",
            tokio::spawn(async move {
                self_clone.run_agent().await;
            }),
        );
        let self_clone = self.clone();
        self.fleet.hdls.push(
            "controller loop",
            tokio::spawn(async move {
                self_clone.controller_loop().await;
            }),
        );
        let web_controller = self.clone();
        let web_db = self.ctx.db.clone();
        let web_port = std::env::var("WEB_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8080);
        self.fleet.hdls.push(
            "web server",
            tokio::spawn(async move {
                crate::web::serve(web_controller, web_db, web_port).await;
            }),
        );
        self.fleet.hdls.join().await;
    }

    async fn run_agent(&self) {
        let (_bought, _tasks) = self.fleet.try_buy_ships(None).await;
        for ship in self.ctx.ships.iter() {
            let ship_symbol = ship.key().clone();
            self.fleet.spawn_run_ship(self, ship_symbol).await;
        }
    }

    async fn controller_loop(&self) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            self.controller_tick().await;
        }
    }

    async fn controller_tick(&self) {
        debug!("controller_tick");
        self.record_metrics().await;
        self.fleet.check_era_advance().await;
        let (bought, _shipyard_task_waypoint) = self.fleet.try_buy_ships(None).await;
        for ship_symbol in bought {
            debug!("Controller tick bought ship {}", ship_symbol);
            self.fleet.spawn_run_ship(self, ship_symbol).await;
        }
        self.contract_tick(true).await;
    }

    // Append a KPI snapshot for time-series analysis (equity curve, fleet size).
    async fn record_metrics(&self) {
        let credits = self.ctx.ledger.credits();
        // Reconciliation: the cash journal must account for every credit moved.
        // Compare the actual change in credits since this process's baseline tick
        // against the sum of journal cash txns over the same window. A persistent,
        // growing gap means some credit-moving path isn't going through
        // record_cash_txn (exactly the blind spot that hid past credit drains).
        let now = Utc::now();
        let (base_ts, base_credits) = self.ctx.ledger.recon_baseline(now, credits);
        if now > base_ts {
            let journal_delta = self.ctx.db.cash_txn_sum_between(base_ts, now).await;
            let actual_delta = credits - base_credits;
            let gap = actual_delta - journal_delta;
            if gap.abs() > 10_000 {
                warn!(
                    "Cash reconciliation gap: {} credits unaccounted since {} \
                     (actual Δ {}, journal Δ {}). A credit-moving path may be \
                     bypassing record_cash_txn.",
                    gap, base_ts, actual_delta, journal_delta
                );
            } else {
                debug!("Cash reconciliation OK (gap {} credits)", gap);
            }
        }
        let cargo_value = self.ctx.ledger.cargo_value();
        // net worth ~= liquid credits + in-transit cargo + ship cost basis
        let net_worth = credits + cargo_value + self.ctx.db.ship_cost_basis().await;
        self.ctx
            .db
            .insert_agent_metrics(
                Utc::now(),
                credits,
                self.ctx.ledger.available_credits(),
                self.ctx.ledger.effective_reserved_credits(),
                cargo_value,
                self.num_ships() as i32,
                net_worth,
            )
            .await;
        // Persist cargo cost basis each tick so it survives a restart (cheap:
        // a small map, written once per controller tick rather than per trade).
        self.ctx
            .db
            .set_value(
                &format!("ledger/{}", self.ctx.callsign),
                &self.ctx.ledger.snapshot(),
            )
            .await;
    }

    // Exploration delegation methods
    pub async fn get_probe_jumpgate_reservation(
        &self,
        ship_symbol: &str,
        ship_loc: &WaypointSymbol,
    ) -> Option<WaypointSymbol> {
        self.exploration
            .get_probe_jumpgate_reservation(ship_symbol, ship_loc)
            .await
    }
    pub async fn clear_probe_jumpgate_reservation(&self, ship_symbol: &str) {
        self.exploration
            .clear_probe_jumpgate_reservation(ship_symbol)
            .await;
    }
    pub async fn get_explorer_reservation(
        &self,
        ship_symbol: &str,
        ship_loc: &SystemSymbol,
    ) -> Option<SystemSymbol> {
        self.exploration
            .get_explorer_reservation(ship_symbol, ship_loc)
            .await
    }
}
