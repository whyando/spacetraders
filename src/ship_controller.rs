use crate::agent_controller::AgentContext;
use crate::api_client::api_models::{
    ExtractResponse, JettisonResponse, NavigateResponse, OrbitResponse, RefuelResponse,
    SiphonResponse, SurveyResponse, TradeResponse, WaypointDetailed, WaypointScanResponse,
};
use crate::models::*;
use crate::models::{ShipCargoItem, ShipCooldown};
use crate::ship_controller::ShipNavStatus::*;
use chrono::{DateTime, Duration, Utc};
use log::*;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::min;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ShipController {
    pub ship_symbol: String,
    ship: Arc<Mutex<Ship>>,
    pub ctx: Arc<AgentContext>,
}

impl ShipController {
    pub fn new(ctx: &Arc<AgentContext>, ship: Arc<Mutex<Ship>>) -> ShipController {
        let symbol = ship.lock().unwrap().symbol.clone();
        ShipController {
            ctx: ctx.clone(),
            ship,
            ship_symbol: symbol,
        }
    }
    pub fn ship(&self) -> Ship {
        self.ship.lock().unwrap().clone()
    }
    pub fn symbol(&self) -> String {
        self.ship_symbol.clone()
    }
    pub fn flight_mode(&self) -> ShipFlightMode {
        let ship = self.ship.lock().unwrap();
        ship.nav.flight_mode.clone()
    }
    pub fn nav_status(&self) -> ShipNavStatus {
        let ship = self.ship.lock().unwrap();
        ship.nav.status.clone()
    }
    pub fn engine_speed(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.engine.speed
    }
    pub fn fuel_capacity(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.fuel.capacity
    }
    pub fn current_fuel(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.fuel.current
    }
    pub fn cargo_capacity(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.cargo.capacity
    }
    pub fn cargo_units(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.cargo.units
    }
    pub fn waypoint(&self) -> WaypointSymbol {
        let ship = self.ship.lock().unwrap();
        ship.nav.waypoint_symbol.clone()
    }
    pub fn system(&self) -> SystemSymbol {
        let ship = self.ship.lock().unwrap();
        ship.nav.system_symbol.clone()
    }
    pub fn cargo_empty(&self) -> bool {
        let ship = self.ship.lock().unwrap();
        ship.cargo.units == 0
    }
    pub fn update_nav_status(&self, status: ShipNavStatus) {
        let mut ship = self.ship.lock().unwrap();
        ship.nav.status = status;
    }
    pub fn update_nav(&self, nav: ShipNav) {
        let mut ship = self.ship.lock().unwrap();
        ship.nav = nav;
    }
    pub fn update_fuel(&self, fuel: ShipFuel) {
        let mut ship = self.ship.lock().unwrap();
        ship.fuel = fuel;
    }
    pub fn update_cargo(&self, cargo: ShipCargo) {
        let mut ship = self.ship.lock().unwrap();
        ship.cargo = cargo;
    }
    pub fn update_cooldown(&self, cooldown: ShipCooldown) {
        let mut ship = self.ship.lock().unwrap();
        ship.cooldown = cooldown;
    }
    pub fn cargo_first_item(&self) -> Option<ShipCargoItem> {
        let ship = self.ship.lock().unwrap();
        ship.cargo.inventory.first().cloned()
    }
    pub fn cargo_good_count(&self, good: &str) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.cargo
            .inventory
            .iter()
            .find(|g| g.symbol == *good)
            .map(|g| g.units)
            .unwrap_or(0)
    }
    pub fn cargo_space_available(&self) -> i64 {
        let ship = self.ship.lock().unwrap();
        ship.cargo.capacity - ship.cargo.units
    }
    pub fn cargo_map(&self) -> std::collections::BTreeMap<String, i64> {
        let ship = self.ship.lock().unwrap();
        ship.cargo
            .inventory
            .iter()
            .map(|g| (g.symbol.clone(), g.units))
            .collect()
    }

    pub fn debug(&self, msg: &str) {
        debug!("[{}] {}", self.ship_symbol, msg);
    }

    pub async fn orbit(&self) {
        if self.nav_status() == InOrbit {
            return;
        }
        let uri = format!("/my/ships/{}/orbit", self.ship_symbol);
        let resp: Data<OrbitResponse> = self.ctx.api_client.post(&uri, &json!({})).await;
        self.update_nav(resp.data.nav);
    }

    pub async fn dock(&self) {
        if self.nav_status() == Docked {
            return;
        }
        let uri = format!("/my/ships/{}/dock", self.ship_symbol);
        let resp: Data<OrbitResponse> = self.ctx.api_client.post(&uri, &json!({})).await;
        self.update_nav(resp.data.nav);
    }

    pub async fn set_flight_mode(&self, mode: ShipFlightMode) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct NavUpdateResponse {
            nav: ShipNav,
            fuel: ShipFuel,
            events: Vec<ShipConditionEvent>,
        }

        if self.flight_mode() == mode {
            return;
        }
        self.debug(&format!("Setting flight mode to {:?}", mode));
        let uri = format!("/my/ships/{}/nav", self.ship_symbol);
        let response: Data<NavUpdateResponse> = self
            .ctx
            .api_client
            .patch(&uri, &json!({ "flightMode": mode }))
            .await;
        let nav = response.data.nav;
        let fuel = response.data.fuel;
        let events = response.data.events;
        self.update_nav(nav);
        self.update_fuel(fuel);
        self.handle_ship_condition_events(&events);
    }

    pub fn is_in_transit(&self) -> bool {
        let arrival_time = self.ship.lock().unwrap().nav.route.arrival;
        let now = chrono::Utc::now();
        arrival_time >= now
    }

    async fn wait_until_timestamp(&self, timestamp: DateTime<Utc>, event: &str) {
        // Multiple sleep calls are necessary due to WSL2 tendency to 'skip time'
        loop {
            let now = Utc::now();
            let wait_time = timestamp - now;
            if wait_time > chrono::Duration::zero() {
                debug!(
                    "Waiting for {}: {:.3}s",
                    event,
                    wait_time.num_milliseconds() as f64 / 1000.0
                );
            } else {
                break;
            }
            tokio::time::sleep(wait_time.to_std().unwrap()).await;
        }
    }

    pub async fn wait_for_transit(&self) {
        let arrival_time = { self.ship.lock().unwrap().nav.route.arrival };
        self.wait_until_timestamp(arrival_time + Duration::seconds(1), "transit")
            .await;
    }
    pub async fn wait_for_cooldown(&self) {
        let cooldown = { self.ship.lock().unwrap().cooldown.clone() };
        if let Some(expiration) = cooldown.expiration {
            self.wait_until_timestamp(expiration + Duration::seconds(1), "cooldown")
                .await;
        }
    }

    async fn trade_good(&self, _type: &str, good: &str, units: i64, adjust_reserved_credits: bool) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        match _type {
            "purchase" => {
                self.debug(&format!("Buying {} units of {}", units, good));
                assert!(
                    units <= self.cargo_capacity(),
                    "Ship can't hold that much cargo"
                );
            }
            "sell" => {
                self.debug(&format!("Selling {} units of {}", units, good));
            }
            _ => panic!("Invalid trade type: {}", _type),
        }
        self.dock().await;
        let uri = format!("/my/ships/{}/{}", self.ship_symbol, _type);
        let body = json!({
            "symbol": good,
            "units": units,
        });
        let TradeResponse {
            cargo,
            agent,
            transaction,
        } = self
            .ctx
            .api_client
            .post::<Data<TradeResponse>, _>(&uri, &body)
            .await
            .data;
        self.update_cargo(cargo);
        self.ctx.update_agent(agent);
        let waypoint = transaction.waypoint_symbol.to_string();
        if _type == "purchase" {
            // Only register basis for trade-flow buys (adjust_reserved_credits);
            // construction/stockpile buys are consumed, not resold.
            if adjust_reserved_credits {
                self.ctx.ledger.register_purchase(
                    &self.ship_symbol,
                    &transaction.trade_symbol,
                    transaction.units,
                    transaction.price_per_unit,
                );
            }
            self.ctx
                .db
                .record_cash_txn(crate::database::CashTxn {
                    ts: transaction.timestamp,
                    type_: "trade_buy",
                    ship_symbol: Some(&self.ship_symbol),
                    reference: Some(&transaction.trade_symbol),
                    waypoint: Some(&waypoint),
                    units: Some(transaction.units as i32),
                    amount: -transaction.total_price,
                    realized_profit: None,
                })
                .await;
        } else {
            // Realized profit on every sale = proceeds - cost basis of the units
            // sold (basis is 0 for mined/siphoned goods, so they're pure profit).
            let realized = self.ctx.ledger.register_sale(
                &self.ship_symbol,
                &transaction.trade_symbol,
                transaction.units,
                transaction.price_per_unit,
            );
            self.ctx
                .db
                .record_cash_txn(crate::database::CashTxn {
                    ts: transaction.timestamp,
                    type_: "trade_sell",
                    ship_symbol: Some(&self.ship_symbol),
                    reference: Some(&transaction.trade_symbol),
                    waypoint: Some(&waypoint),
                    units: Some(transaction.units as i32),
                    amount: transaction.total_price,
                    realized_profit: Some(realized),
                })
                .await;
        }
        self.debug(&format!(
            "{} {} {} for ${} (total ${})",
            transaction._type,
            transaction.units,
            transaction.trade_symbol,
            transaction.price_per_unit,
            transaction.total_price
        ));
    }

    pub async fn buy_goods(&self, good: &str, units: i64, adjust_reserved_credits: bool) {
        self.trade_good("purchase", good, units, adjust_reserved_credits)
            .await;
    }

    pub async fn sell_goods(&self, good: &str, units: i64, adjust_reserved_credits: bool) {
        self.trade_good("sell", good, units, adjust_reserved_credits)
            .await;
    }

    pub async fn sell_all_cargo(&self) {
        self.refresh_market().await;
        let market = self.ctx.universe.get_market(&self.waypoint()).unwrap();
        while let Some(cargo_item) = self.cargo_first_item() {
            let market_good = market
                .data
                .trade_goods
                .iter()
                .find(|g| g.symbol == cargo_item.symbol)
                .unwrap();
            let units = min(market_good.trade_volume, cargo_item.units);
            assert!(units > 0);
            self.sell_goods(&cargo_item.symbol, units, false).await;
            let new_units = self.cargo_good_count(&cargo_item.symbol);
            assert!(new_units == cargo_item.units - units);
        }
        self.refresh_market().await;
    }

    pub async fn jettison_cargo(&self, good: &str, units: i64) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        self.debug(&format!("Jettisoning {} {}", units, good));
        let uri = format!("/my/ships/{}/jettison", self.ship_symbol);
        let body = json!({
            "symbol": good,
            "units": units,
        });
        let JettisonResponse { cargo } = self
            .ctx
            .api_client
            .post::<Data<JettisonResponse>, _>(&uri, &body)
            .await
            .data;
        self.update_cargo(cargo);
    }

    // Fuel is bought in multiples of 100, so refuel as the highest multiple of 100
    // or to full if that wouldn't reach the required_fuel amount
    //
    // If from_cargo is true, refuel from cargo, and we must check after the refuel whether the refuel suceeded
    // Whereas if buying from market, we can safely assume we can obtain the required amount
    pub async fn refuel(&self, required_fuel: i64, from_cargo: bool) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        assert!(
            required_fuel <= self.fuel_capacity(),
            "Ship can't hold that much fuel"
        );
        if self.current_fuel() >= required_fuel {
            return;
        }

        let current = self.current_fuel();
        let capacity = self.fuel_capacity();
        let max_refuel_units = match from_cargo {
            true => 100 * self.cargo_good_count("FUEL"),
            false => i64::MAX,
        };
        if max_refuel_units == 0 {
            self.debug("No fuel in cargo to refuel");
            return;
        }
        let mut units = {
            let missing_fuel = capacity - current;
            // round down to the nearest 100, so we don't buy more than we need
            let units = (missing_fuel / 100) * 100;
            if units + current < required_fuel {
                missing_fuel
            } else {
                units
            }
        };
        units = min(units, max_refuel_units);
        self.dock().await;
        self.debug(&format!(
            "Refueling {} to {}/{}",
            units,
            current + units,
            capacity
        ));
        let uri = format!("/my/ships/{}/refuel", self.ship_symbol);
        let body = json!({
            "units": units,
            "fromCargo": from_cargo,
        });

        let initial_cargo_fuel = self.cargo_good_count("FUEL");
        let RefuelResponse {
            fuel,
            agent,
            cargo,
            transaction,
        } = self
            .ctx
            .api_client
            .post::<Data<RefuelResponse>, _>(&uri, &body)
            .await
            .data;
        // Flying-fuel expense: market refuels cost credits (from_cargo refuels
        // draw on already-bought cargo, so total_price is 0). Logged distinctly
        // from FUEL bought as a trade good, which flows through realized profit.
        if !from_cargo {
            self.ctx
                .db
                .record_cash_txn(crate::database::CashTxn {
                    ts: transaction.timestamp,
                    type_: "refuel",
                    ship_symbol: Some(&self.ship_symbol),
                    reference: Some("FUEL"),
                    waypoint: Some(&transaction.waypoint_symbol.to_string()),
                    units: Some(transaction.units as i32),
                    amount: -transaction.total_price,
                    realized_profit: None,
                })
                .await;
        } else {
            // Consuming pre-bought fuel from cargo: clear any tracked basis.
            self.ctx
                .ledger
                .register_consumption(&self.ship_symbol, "FUEL", (units + 99) / 100);
        }
        self.update_fuel(fuel);
        assert_eq!(cargo.is_some(), from_cargo);
        if let Some(cargo) = cargo {
            self.update_cargo(cargo);
            let expected_cargo_fuel = initial_cargo_fuel - (units + 99) / 100;
            assert_eq!(self.cargo_good_count("FUEL"), expected_cargo_fuel);
        }
        self.ctx.update_agent(agent);
    }

    pub async fn full_load_cargo(&self, good: &str) {
        let cargo_units = self.cargo_good_count(good);
        assert_eq!(cargo_units, self.cargo_units());

        let buy_units = self.cargo_capacity() - cargo_units;
        if buy_units > 0 {
            // Makes assumptions about the TV of the good
            self.buy_goods(good, buy_units, false).await;
            self.refresh_market().await;
        }
    }

    async fn navigate(&self, flight_mode: ShipFlightMode, waypoint: &WaypointSymbol) {
        assert!(!self.is_in_transit(), "Ship is already in transit");
        if self.waypoint() == *waypoint {
            return;
        }
        assert_eq!(self.waypoint().system(), waypoint.system());
        self.set_flight_mode(flight_mode).await;
        self.orbit().await;
        self.debug(&format!("Navigating to waypoint: {}", waypoint));
        let uri = format!("/my/ships/{}/navigate", self.ship_symbol);
        let NavigateResponse { nav, fuel, events } = self
            .ctx
            .api_client
            .post::<Data<NavigateResponse>, _>(&uri, &json!({ "waypointSymbol": waypoint }))
            .await
            .data;
        self.handle_ship_condition_events(&events);
        self.update_nav(nav);
        self.update_fuel(fuel);
        self.wait_for_transit().await;
        self.update_nav_status(InOrbit);
    }

    pub async fn warp(&self, flight_mode: ShipFlightMode, waypoint: &WaypointSymbol) {
        assert!(!self.is_in_transit(), "Ship is already in transit");
        if self.waypoint() == *waypoint {
            return;
        }
        assert_ne!(self.waypoint().system(), waypoint.system());
        self.set_flight_mode(flight_mode).await;
        self.orbit().await;
        self.debug(&format!("Warp to waypoint: {}", waypoint));
        let uri = format!("/my/ships/{}/warp", self.ship_symbol);
        let NavigateResponse { nav, fuel, events } = self
            .ctx
            .api_client
            .post::<Data<NavigateResponse>, _>(&uri, &json!({ "waypointSymbol": waypoint }))
            .await
            .data;
        self.handle_ship_condition_events(&events);
        self.update_nav(nav);
        self.update_fuel(fuel);
        self.wait_for_transit().await;
        self.update_nav_status(InOrbit);
    }

    pub async fn jump(&self, waypoint: &WaypointSymbol) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct JumpResponse {
            nav: ShipNav,
            cooldown: ShipCooldown,
            transaction: MarketTransaction,
            agent: Agent,
        }

        assert!(!self.is_in_transit(), "Ship is in transit");
        self.wait_for_cooldown().await;
        self.orbit().await;
        self.debug(&format!("Jumping to waypoint: {}", waypoint));
        let uri = format!("/my/ships/{}/jump", self.ship_symbol);
        let body = json!({ "waypointSymbol": waypoint });
        let JumpResponse {
            nav,
            cooldown,
            agent,
            transaction,
        } = self
            .ctx
            .api_client
            .post::<Data<JumpResponse>, _>(&uri, &body)
            .await
            .data;
        self.update_nav(nav);
        self.ctx.update_agent(agent);
        self.update_cooldown(cooldown);
        // The jump charges credits at the gate (antimatter cost). Journal it so
        // the cash journal stays complete and the antimatter expense line is real.
        if transaction.total_price != 0 {
            self.ctx
                .db
                .record_cash_txn(crate::database::CashTxn {
                    ts: transaction.timestamp,
                    type_: "jump",
                    ship_symbol: Some(&self.ship_symbol),
                    reference: Some(&transaction.trade_symbol),
                    waypoint: Some(&transaction.waypoint_symbol.to_string()),
                    units: Some(transaction.units as i32),
                    amount: -transaction.total_price,
                    realized_profit: None,
                })
                .await;
        }
    }

    pub async fn goto_waypoint(&self, target: &WaypointSymbol) {
        assert!(!self.is_in_transit(), "Ship is already in transit");
        if self.fuel_capacity() == 0 {
            self.navigate(ShipFlightMode::Cruise, target).await;
            self.debug(&format!("Arrived at waypoint: {}", target));
            return;
        }
        if self.waypoint() == *target {
            return;
        }
        let route = self
            .ctx
            .universe
            .get_route(
                &self.waypoint(),
                target,
                self.engine_speed(),
                self.current_fuel(),
                self.fuel_capacity(),
            )
            .await;
        for (waypoint, edge, a_market, b_market) in route.hops {
            // calculate fuel required before leaving
            let required_fuel = if b_market {
                edge.fuel_cost
            } else {
                assert!(waypoint == *target);
                edge.fuel_cost + route.req_terminal_fuel
            };
            if self.current_fuel() < required_fuel {
                assert!(a_market);
                self.refuel(required_fuel, false).await;
            }
            self.navigate(edge.flight_mode, &waypoint).await;
            self.debug(&format!("Arrived at waypoint: {}", waypoint));
        }
    }

    pub async fn supply_construction(&self, good: &str, units: i64) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct SupplyConstructionResponse {
            cargo: ShipCargo,
            construction: Construction,
        }

        assert!(!self.is_in_transit(), "Ship is in transit");
        self.dock().await;
        self.debug(&format!("Constructing {} units of {}", units, good));
        let uri = format!(
            "/systems/{}/waypoints/{}/construction/supply",
            self.system(),
            self.waypoint()
        );
        let body = json!({
            "shipSymbol": self.ship_symbol,
            "tradeSymbol": good,
            "units": units,
        });
        let SupplyConstructionResponse {
            cargo,
            construction,
        } = self
            .ctx
            .api_client
            .post::<Data<SupplyConstructionResponse>, _>(&uri, &body)
            .await
            .data;
        self.update_cargo(cargo);
        self.ctx.universe.update_construction(&construction).await;
    }

    pub async fn deliver_contract(&self, contract_id: &str, good: &str, units: i64) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct DeliverContractResponse {
            cargo: ShipCargo,
            contract: Contract,
        }

        assert!(!self.is_in_transit(), "Ship is in transit");
        self.dock().await;
        self.debug(&format!("Delivering {} units of {}", units, good));
        let uri = format!("/my/contracts/{}/deliver", contract_id);
        let body = json!({ "shipSymbol": self.ship_symbol, "tradeSymbol": good, "units": units });
        let DeliverContractResponse { cargo, contract } = self
            .ctx
            .api_client
            .post::<Data<DeliverContractResponse>, _>(&uri, &body)
            .await
            .data;
        self.update_cargo(cargo);
        self.ctx.update_contract(contract);

        // Contract delivery isn't a sale, but it's how this ship earns its share of
        // the contract payout (credited at fulfill, split across deliverers by units).
        // Drop the delivered units' cost basis here (otherwise it leaks and inflates
        // net-worth valuation), and journal a memo row carrying the per-ship units and
        // the COGS (as negative realized_profit). amount is 0 — no cash moves until
        // fulfill. See ContractManager::contract_inner for the payout split.
        let basis = self
            .ctx
            .ledger
            .register_consumption(&self.ship_symbol, good, units);
        let wp = self.waypoint().to_string();
        self.ctx
            .db
            .record_cash_txn(crate::database::CashTxn {
                ts: chrono::Utc::now(),
                type_: "contract_deliver",
                ship_symbol: Some(&self.ship_symbol),
                reference: Some(contract_id),
                waypoint: Some(&wp),
                units: Some(units as i32),
                amount: 0,
                realized_profit: Some(-basis),
            })
            .await;
    }

    pub async fn refresh_market(&self) {
        assert!(!self.is_in_transit());
        let waypoint = self.waypoint();
        let system = self.system();
        self.debug(&format!("Refreshing market at waypoint {}", &waypoint));
        let uri = format!("/systems/{}/waypoints/{}/market", &system, &waypoint);
        let response: Data<Market> = self.ctx.api_client.get(&uri).await;
        let market = WithTimestamp::<Market> {
            timestamp: chrono::Utc::now(),
            data: response.data,
        };
        self.ctx.universe.save_market(&waypoint, market).await;
        // Refreshing here proves it's a market; learn the trait in case our cache was stale.
        self.ctx
            .universe
            .note_waypoint_traits(&waypoint, true, false)
            .await;
    }

    pub async fn refresh_shipyard(&self) {
        assert!(!self.is_in_transit());
        let waypoint = self.waypoint();
        let system = self.system();
        self.debug(&format!("Refreshing shipyard at waypoint {}", &waypoint));
        let uri = format!("/systems/{}/waypoints/{}/shipyard", &system, &waypoint);
        let response: Data<Shipyard> = self.ctx.api_client.get(&uri).await;
        let shipyard = WithTimestamp::<Shipyard> {
            timestamp: chrono::Utc::now(),
            data: response.data,
        };
        self.ctx.universe.save_shipyard(&waypoint, shipyard).await;
        // Refreshing here proves it's a shipyard; learn the trait so search_shipyards (and
        // thus remote purchasing) can find it even if our startup snapshot was stale.
        self.ctx
            .universe
            .note_waypoint_traits(&waypoint, false, true)
            .await;
    }

    pub async fn survey(&self) {
        assert!(!self.is_in_transit());
        self.wait_for_cooldown().await;
        self.debug(&format!("Surveying {}", self.waypoint()));
        let uri = format!("/my/ships/{}/survey", self.ship_symbol);
        let SurveyResponse { cooldown, surveys } = self
            .ctx
            .api_client
            .post::<Data<SurveyResponse>, _>(&uri, &json!({}))
            .await
            .data;
        for survey in &surveys {
            let deposits = survey
                .deposits
                .iter()
                .map(|d| d.symbol.clone())
                .collect::<Vec<_>>()
                .join(", ");
            self.debug(&format!("Surveyed {} {}", survey.size, deposits));
        }
        self.update_cooldown(cooldown);
        self.ctx.survey_manager.insert_surveys(surveys).await;
    }

    // Chart the current waypoint if it isn't charted yet (earns credits and reveals
    // its traits). Safe no-op if it's already charted or charting otherwise fails.
    pub async fn chart(&self) {
        assert!(!self.is_in_transit());
        self.orbit().await;
        self.debug(&format!("Charting {}", self.waypoint()));
        if let Some(resp) = self.ctx.api_client.chart_waypoint(&self.ship_symbol).await {
            self.ctx
                .universe
                .ingest_scanned_waypoints(&[resp.waypoint])
                .await;
        }
    }

    // Sensor-array waypoint scan: reveals nearby waypoints' traits (markets/shipyards),
    // bypassing their uncharted state. Requires a MOUNT_SENSOR_ARRAY; triggers a cooldown.
    // Ingests the revealed traits into the universe so the agent learns the markets.
    pub async fn scan_waypoints(&self) -> Vec<WaypointDetailed> {
        assert!(!self.is_in_transit());
        self.wait_for_cooldown().await;
        self.debug(&format!("Scanning waypoints from {}", self.waypoint()));
        let uri = format!("/my/ships/{}/scan/waypoints", self.ship_symbol);
        let WaypointScanResponse {
            cooldown,
            waypoints,
        } = self
            .ctx
            .api_client
            .post::<Data<WaypointScanResponse>, _>(&uri, &json!({}))
            .await
            .data;
        self.update_cooldown(cooldown);
        self.ctx.universe.ingest_scanned_waypoints(&waypoints).await;
        waypoints
    }

    pub async fn transfer_cargo(&self) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        self.orbit().await;
        let cargo = {
            let ship = self.ship.lock().unwrap();
            ship.cargo
                .inventory
                .iter()
                .map(|g| (g.symbol.clone(), g.units))
                .collect()
        };
        self.ctx
            .cargo_broker
            .transfer_cargo(&self.ship_symbol, &self.waypoint(), cargo)
            .await;
    }

    pub async fn receive_cargo(&self) {
        self.orbit().await;
        assert!(!self.is_in_transit(), "Ship is in transit");
        let space = self.cargo_space_available();
        self.ctx
            .cargo_broker
            .receive_cargo(&self.ship_symbol, &self.waypoint(), space)
            .await;
    }

    pub async fn siphon(&self) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        self.orbit().await;
        self.wait_for_cooldown().await;
        self.debug("Siphoning");
        let uri = format!("/my/ships/{}/siphon", self.ship_symbol);
        let body = json!({});
        let SiphonResponse {
            cargo,
            cooldown,
            siphon,
            events,
        } = self
            .ctx
            .api_client
            .post::<Data<SiphonResponse>, _>(&uri, &body)
            .await
            .data;
        let good = siphon._yield.symbol;
        let units = siphon._yield.units;
        self.handle_ship_condition_events(&events);
        self.debug(&format!("Siphoned {} units of {}", units, good));
        self.update_cooldown(cooldown);
        self.update_cargo(cargo);
    }

    pub async fn extract_survey(&self, survey: &KeyedSurvey) {
        assert!(!self.is_in_transit(), "Ship is in transit");
        // self.orbit().await;
        self.wait_for_cooldown().await;
        self.debug(&format!("Extracting survey {}", survey.uuid));
        let uri = format!("/my/ships/{}/extract/survey", self.ship_symbol);
        let req_body = &survey.survey;

        let (code, resp_body): (StatusCode, Result<String, String>) = self
            .ctx
            .api_client
            .request_string(Method::POST, &uri, Some(req_body))
            .await;
        match code {
            StatusCode::CREATED => {
                let response: String = resp_body.unwrap();
                let ExtractResponse {
                    cargo,
                    cooldown,
                    extraction,
                    events,
                } = serde_json::from_str::<Data<ExtractResponse>>(&response)
                    .unwrap()
                    .data;
                self.handle_ship_condition_events(&events);
                self.debug(&format!(
                    "Extracted {} units of {}",
                    extraction._yield.units, extraction._yield.symbol
                ));
                self.update_cooldown(cooldown);
                self.update_cargo(cargo);
            }
            StatusCode::BAD_REQUEST | StatusCode::CONFLICT => {
                let response: Value = serde_json::from_str(&resp_body.unwrap_err()).unwrap();
                // variety of responses we might get here: exhausted, expired, asteroid overmined
                let code = response["error"]["code"].as_i64().unwrap();
                if code == 4221 {
                    // Request failed: 400 {"error":{"message":"Ship survey failed. Target signature is no longer in range or valid.","code":4221}}
                    self.debug(
                        "Extraction failed: Target signature is no longer in range or valid",
                    );
                    self.ctx.survey_manager.remove_survey(survey).await;
                } else if code == 4224 {
                    self.debug("Extraction failed: Survey has been exhausted");
                    self.ctx.survey_manager.remove_survey(survey).await;
                } else {
                    panic!(
                        "Request failed: {} {} {}\nbody: {:?}",
                        code,
                        Method::POST,
                        uri,
                        response
                    );
                }
            }
            _ => panic!(
                "Request failed: {} {} {}\nbody: {:?}",
                code.as_u16(),
                Method::POST,
                uri,
                resp_body
            ),
        };
    }

    pub async fn scrap(&self) {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct ScrapResponse {
            agent: Agent,
            transaction: ScrapTransaction,
        }

        assert!(!self.is_in_transit(), "Ship is in transit");
        self.dock().await;
        self.debug("Scrapping Ship");
        let uri = format!("/my/ships/{}/scrap", self.ship_symbol);
        let ScrapResponse { agent, transaction } = self
            .ctx
            .api_client
            .post::<Data<ScrapResponse>, _>(&uri, &json!({}))
            .await
            .data;
        info!(
            "{} Scrapped ship for ${}",
            self.ship_symbol, transaction.total_price
        );
        self.ctx
            .db
            .record_cash_txn(crate::database::CashTxn {
                ts: transaction.timestamp,
                type_: "scrap",
                ship_symbol: Some(&self.ship_symbol),
                reference: None,
                waypoint: Some(&transaction.waypoint_symbol.to_string()),
                units: None,
                amount: transaction.total_price,
                realized_profit: None,
            })
            .await;
        self.ctx.update_agent(agent);
    }

    pub fn handle_ship_condition_events(&self, events: &Vec<ShipConditionEvent>) {
        for e in events {
            self.debug(&format!(
                "Encountered ship event: {} ({})",
                e.symbol, e.component
            ));
        }
    }

    pub fn set_state_description(&self, desc: &str) {
        self.ctx.set_state_description(&self.ship_symbol, desc);
    }
}
