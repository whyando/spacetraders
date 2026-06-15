use crate::api_client::api_models::TransferResponse;
use crate::api_client::ApiClient;
use crate::broker::{CargoBroker, TransferActor};
use crate::database::DbClient;
use crate::models::*;
use crate::survey_manager::SurveyManager;
use crate::universe::Universe;

use super::ledger::Ledger;
use dashmap::DashMap;
use log::*;
use serde_json::json;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub struct AgentContext {
    pub universe: Arc<Universe>,
    pub api_client: ApiClient,
    pub db: DbClient,
    pub callsign: String,

    pub agent: Arc<Mutex<Agent>>,
    pub ships: Arc<DashMap<String, Arc<Mutex<Ship>>>>,
    pub contract: Arc<Mutex<Option<Contract>>>,

    pub ledger: Arc<Ledger>,
    pub survey_manager: Arc<SurveyManager>,
    pub cargo_broker: Arc<CargoBroker>,
    pub ship_state_description: Arc<DashMap<String, String>>,
}

impl AgentContext {
    pub fn agent(&self) -> Agent {
        self.agent.lock().unwrap().clone()
    }

    pub fn starting_system(&self) -> SystemSymbol {
        self.agent.lock().unwrap().headquarters.system()
    }

    pub fn starting_faction(&self) -> String {
        self.agent.lock().unwrap().starting_faction.clone()
    }

    pub fn faction_capital(&self) -> SystemSymbol {
        let faction_symbol = self.starting_faction();
        let faction = self.universe.get_faction(&faction_symbol);
        faction.headquarters.unwrap()
    }

    pub fn update_agent(&self, agent_upd: Agent) {
        let mut agent = self.agent.lock().unwrap();
        *agent = agent_upd;
        self.ledger.set_credits(agent.credits);
    }

    pub fn update_contract(&self, contract: Contract) {
        self.contract.lock().unwrap().replace(contract);
    }

    pub fn set_state_description(&self, ship_symbol: &str, desc: &str) {
        self.ship_state_description
            .insert(ship_symbol.to_string(), desc.to_string());
    }

    fn debug(&self, msg: &str) {
        debug!("[{}] {}", self.callsign, msg);
    }

    pub async fn transfer_cargo(
        &self,
        src_ship_symbol: String,
        dest_ship_symbol: String,
        good: String,
        units: i64,
    ) {
        debug!("agent_context::transfer_cargo");

        self.debug(&format!(
            "Transferring {} -> {} {} {}",
            &src_ship_symbol, &dest_ship_symbol, &units, &good
        ));
        let uri = format!("/my/ships/{}/transfer", &src_ship_symbol);
        let body = json!({
            "shipSymbol": &dest_ship_symbol,
            "tradeSymbol": &good,
            "units": &units,
        });
        let TransferResponse {
            cargo,
            target_cargo,
        } = self
            .api_client
            .post::<Data<TransferResponse>, _>(&uri, &body)
            .await
            .data;
        {
            let src_ship = self.ships.get(&src_ship_symbol).unwrap();
            let dest_ship = self.ships.get(&dest_ship_symbol).unwrap();
            let mut src_ship = src_ship.lock().unwrap();
            let mut dest_ship = dest_ship.lock().unwrap();
            src_ship.cargo = cargo;
            dest_ship.cargo = target_cargo;
        }
        debug!("agent_context::transfer_cargo done");
    }
}

impl TransferActor for Arc<AgentContext> {
    fn _transfer_cargo(
        &self,
        src_ship_symbol: String,
        dest_ship_symbol: String,
        good: String,
        units: i64,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let ctx = self.clone();
        Box::pin(async move {
            ctx.transfer_cargo(src_ship_symbol, dest_ship_symbol, good, units)
                .await;
        })
    }
}
