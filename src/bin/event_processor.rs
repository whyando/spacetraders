//! Simple event processor. Process events produced by the agent and insert a condensed form into scylla db.
use chrono::Utc;
use lazy_static::lazy_static;
use log::*;
use rdkafka::consumer::CommitMode;
use rdkafka::consumer::Consumer as _;
use rdkafka::consumer::StreamConsumer;
use rdkafka::message::Message as _;
use regex::Regex;
use st::api_client::api_models::BuyShipResponse;
use st::api_client::api_models::NavigateResponse;
use st::api_client::api_models::OrbitResponse;
use st::api_client::kafka_interceptor::ApiRequest;
use st::config::{KAFKA_CONFIG, KAFKA_TOPIC};
use st::event_log::models::ShipEntity;
use st::event_log::models::ShipEntityUpdate;
use st::models::Data;
use st::models::PaginatedList;
use st::models::Ship;
use st::models::ShipFuel;
use st::models::ShipNav;
use st::models::ShipNavStatus;
use st::scylla_client::CurrentState;
use st::scylla_client::Event;
use st::scylla_client::EventLog;
use st::scylla_client::ScyllaClient;
use st::scylla_client::Snapshot;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();

    let worker = Worker::new().await;

    // Set a group_id and log_id directly for testing purposes
    // let id = Utc::now().timestamp();
    let id = "tst4378-6";
    let group_id = format!("event-processor-test-{}", id);
    let log_id = format!("ships-list-test-{}", id);

    let consumer: StreamConsumer = KAFKA_CONFIG
        .clone()
        .set("group.id", group_id)
        .set("enable.auto.commit", "false")
        .set("auto.offset.reset", "earliest")
        .create()
        .expect("Failed to create Kafka consumer");

    consumer.subscribe(&[*KAFKA_TOPIC]).unwrap();

    info!("Subscribed to topic '{}'", *KAFKA_TOPIC);
    loop {
        let message = consumer.recv().await.unwrap();
        let topic = message.topic();
        let payload = message.payload().unwrap();
        if topic == *KAFKA_TOPIC {
            let api_request: ApiRequest = serde_json::from_slice(&payload).unwrap();
            worker.process_api_request(&log_id, api_request).await;
        } else {
            panic!("Unknown topic: {}", topic);
        }
        consumer
            .commit_message(&message, CommitMode::Async)
            .unwrap();
    }
}

struct Worker {
    scylla: ScyllaClient,
}

impl Worker {
    pub async fn new() -> Self {
        Self {
            scylla: ScyllaClient::new().await,
        }
    }

    pub async fn process_api_request(&self, log_id: &str, req: ApiRequest) {
        info!(
            "Received api request: {} {} {} {}",
            req.request_id, req.status, req.method, req.path
        );

        // 1. use the path to identify the relevant event log id and entity(s)

        let mut ship_updates: BTreeMap<String, Ship> = BTreeMap::new();
        let mut ship_nav_updates: BTreeMap<String, ShipNav> = BTreeMap::new();
        let mut ship_fuel_updates: BTreeMap<String, ShipFuel> = BTreeMap::new();

        // Match on the api request path using specific regex patterns
        let (path, _query_params) = parse_path(&req.path);
        match endpoint(&req.method, &path) {
            Endpoint::GetShipsList => {
                let ships_list: PaginatedList<Ship> =
                    serde_json::from_str(&req.response_body).unwrap();
                for ship in ships_list.data {
                    ship_updates.insert(ship.symbol.clone(), ship);
                }
            }
            Endpoint::GetShip(ship_symbol) => {
                let ship: Data<Ship> = serde_json::from_str(&req.response_body).unwrap();
                ship_updates.insert(ship_symbol, ship.data);
            }
            Endpoint::PostShipNavigate(ship_symbol) => {
                let resp: Data<NavigateResponse> =
                    serde_json::from_str(&req.response_body).unwrap();
                ship_nav_updates.insert(ship_symbol.clone(), resp.data.nav);
                ship_fuel_updates.insert(ship_symbol.clone(), resp.data.fuel);
            }
            Endpoint::PostShipDock(ship_symbol) => {
                let resp: Data<OrbitResponse> = serde_json::from_str(&req.response_body).unwrap();
                ship_nav_updates.insert(ship_symbol, resp.data.nav);
            }
            Endpoint::PostShipOrbit(ship_symbol) => {
                let resp: Data<OrbitResponse> = serde_json::from_str(&req.response_body).unwrap();
                ship_nav_updates.insert(ship_symbol, resp.data.nav);
            }
            Endpoint::PostBuyShip => {
                let resp: Data<BuyShipResponse> = serde_json::from_str(&req.response_body).unwrap();
                ship_updates.insert(resp.data.ship.symbol.clone(), resp.data.ship);
            }
            Endpoint::Other => {}
        }

        if ship_updates.is_empty() && ship_nav_updates.is_empty() && ship_fuel_updates.is_empty() {
            return;
        }

        let uniq_ship_symbols: BTreeSet<&String> = ship_updates
            .keys()
            .chain(ship_nav_updates.keys())
            .chain(ship_fuel_updates.keys())
            .collect();
        for symbol in uniq_ship_symbols {
            self.process_ship_req(
                log_id,
                symbol,
                ship_updates.get(symbol),
                ship_nav_updates.get(symbol),
                ship_fuel_updates.get(symbol),
            )
            .await;
        }
    }

    async fn process_ship_req(
        &self,
        log_id: &str,
        ship_symbol: &str,
        ship_update: Option<&Ship>,
        ship_nav_update: Option<&ShipNav>,
        ship_fuel_update: Option<&ShipFuel>,
    ) {
        assert!(ship_update.is_some() || ship_nav_update.is_some() || ship_fuel_update.is_some());
        let current_state = self.scylla.get_entity(log_id, ship_symbol).await;
        let ship_entity_prev: Option<ShipEntity> = current_state
            .as_ref()
            .map(|state| serde_json::from_str(&state.state_data).unwrap());

        // Get the latest ship entity
        let ship_entity: ShipEntity = match ship_update {
            Some(ship) => {
                assert!(ship_nav_update.is_none() && ship_fuel_update.is_none());
                to_ship_entity(ship)
            }
            None => {
                assert!(ship_nav_update.is_some() || ship_fuel_update.is_some());
                let mut ship_entity = match &ship_entity_prev {
                    Some(ship_entity_prev) => ship_entity_prev.clone(),
                    None => {
                        warn!("No previous ship entity found in scylla for {}. Skipping partial ship update.", ship_symbol);
                        return;
                    }
                };
                if let Some(ship_nav_update) = ship_nav_update {
                    apply_ship_nav(&mut ship_entity, ship_nav_update);
                }
                if let Some(ship_fuel_update) = ship_fuel_update {
                    apply_ship_fuel(&mut ship_entity, ship_fuel_update);
                }
                ship_entity
            }
        };

        // Compare the previous and new ship entities to determine if anything has changed
        if ship_entity_prev.as_ref() == Some(&ship_entity) {
            return;
        }
        let prev = ship_entity_prev.unwrap_or_default();
        let update = get_ship_entity_update(&prev, &ship_entity);
        debug!("Ship {} entity update: {:?}", ship_symbol, update);

        self.update_entity(
            log_id,
            current_state,
            ship_symbol,
            "ship",
            &serde_json::to_string(&ship_entity).unwrap(),
            &serde_json::to_string(&update).unwrap(),
        )
        .await;
    }

    async fn update_entity(
        &self,
        log_id: &str,
        current_state: Option<CurrentState>,
        entity_id: &str,
        entity_type: &str,
        state_data: &str,
        event_data: &str,
    ) {
        // Update Query 1: get the current seq num for the event log `event_logs` table
        let event_log = self.scylla.get_event_log(log_id).await;
        let next_seq_num = event_log.map(|log| log.last_seq_num).unwrap_or(0) + 1;
        let next_entity_seq_num = current_state
            .as_ref()
            .map(|state| state.entity_seq_num)
            .unwrap_or(0)
            + 1;
        let last_snapshot_entity_seq_num = current_state
            .as_ref()
            .map(|state| state.last_snapshot_entity_seq_num)
            .unwrap_or(0);
        let should_snapshot = next_entity_seq_num - last_snapshot_entity_seq_num >= 20;
        let ts = Utc::now();

        // Update Query 1.1: increment seq num and upsert the event log `event_logs` table
        let event_log = EventLog {
            event_log_id: log_id.to_string(),
            last_seq_num: next_seq_num,
            last_updated: ts,
        };
        self.scylla.upsert_event_log(&event_log).await;

        // Update Query 2: upsert to `current_state` table
        let last_snapshot_entity_seq_num = if should_snapshot {
            next_entity_seq_num
        } else {
            last_snapshot_entity_seq_num
        };
        let state = CurrentState {
            event_log_id: log_id.to_string(),
            entity_id: entity_id.to_string(),
            entity_type: entity_type.to_string(),
            state_data: state_data.to_string(),
            last_updated: ts,
            seq_num: next_seq_num,
            entity_seq_num: next_entity_seq_num,
            last_snapshot_entity_seq_num,
        };
        self.scylla.upsert_entity(&state).await;

        // Update Query 2.1: conditionally, insert the ship entity update into `snapshots` table
        if should_snapshot {
            info!(
                "Snapshotting ship {} at seq num {}",
                entity_id, next_entity_seq_num
            );
            let snapshot = Snapshot {
                event_log_id: log_id.to_string(),
                entity_id: entity_id.to_string(),
                entity_type: entity_type.to_string(),
                state_data: state_data.to_string(),
                last_updated: ts,
                seq_num: next_seq_num,
                entity_seq_num: next_entity_seq_num,
            };
            self.scylla.insert_snapshot(&snapshot).await;
        }

        // Update Query 3: insert the ship entity update into `events` table
        let event = Event {
            event_log_id: log_id.to_string(),
            seq_num: next_seq_num,
            timestamp: ts,
            entity_id: entity_id.to_string(),
            event_type: "ship_update".to_string(),
            event_data: event_data.to_string(),
        };
        self.scylla.insert_event(&event).await;
    }
}

fn parse_path(full_path: &str) -> (String, Vec<(String, String)>) {
    // Split on '?' to separate path from query parameters
    let parts: Vec<&str> = full_path.split('?').collect();
    let path = parts[0].to_string();

    let mut query_params = Vec::new();
    if parts.len() > 1 {
        // Parse query parameters
        let query_string = parts[1];
        for param in query_string.split('&') {
            let key_value: Vec<&str> = param.split('=').collect();
            if key_value.len() == 2 {
                query_params.push((key_value[0].to_string(), key_value[1].to_string()));
            }
        }
    }

    (path, query_params)
}

enum Endpoint {
    GetShipsList,
    GetShip(String),
    PostBuyShip,
    PostShipDock(String),
    PostShipOrbit(String),
    PostShipNavigate(String),
    Other,
}

fn endpoint(method: &str, path: &str) -> Endpoint {
    lazy_static! {
        static ref SHIP_REGEX: Regex = Regex::new(r"^/my/ship/([^/]+)$").unwrap();
        static ref SHIP_NAVIGATE_REGEX: Regex =
            Regex::new(r"^/my/ships/([^/]+)/navigate$").unwrap();
        static ref SHIP_DOCK_REGEX: Regex = Regex::new(r"^/my/ships/([^/]+)/dock$").unwrap();
        static ref SHIP_ORBIT_REGEX: Regex = Regex::new(r"^/my/ships/([^/]+)/orbit$").unwrap();
    }

    match method {
        "GET" => {
            if path == "/my/ships" {
                Endpoint::GetShipsList
            } else if SHIP_REGEX.is_match(path) {
                let captures = SHIP_REGEX.captures(path).unwrap();
                let ship_symbol = captures.get(1).unwrap().as_str().to_string();
                Endpoint::GetShip(ship_symbol)
            } else {
                Endpoint::Other
            }
        }
        "POST" => {
            if path == "/my/ships" {
                Endpoint::PostBuyShip
            } else if SHIP_NAVIGATE_REGEX.is_match(path) {
                let captures = SHIP_NAVIGATE_REGEX.captures(path).unwrap();
                let ship_symbol = captures.get(1).unwrap().as_str().to_string();
                Endpoint::PostShipNavigate(ship_symbol)
            } else if SHIP_DOCK_REGEX.is_match(path) {
                let captures = SHIP_DOCK_REGEX.captures(path).unwrap();
                let ship_symbol = captures.get(1).unwrap().as_str().to_string();
                Endpoint::PostShipDock(ship_symbol)
            } else if SHIP_ORBIT_REGEX.is_match(path) {
                let captures = SHIP_ORBIT_REGEX.captures(path).unwrap();
                let ship_symbol = captures.get(1).unwrap().as_str().to_string();
                Endpoint::PostShipOrbit(ship_symbol)
            } else {
                Endpoint::Other
            }
        }
        _ => Endpoint::Other,
    }
}

fn to_ship_entity(ship: &Ship) -> ShipEntity {
    let is_docked = ship.nav.status == ShipNavStatus::Docked;
    let nav_source = ship.nav.route.origin.symbol.to_string();
    let nav_arrival_time = ship.nav.route.arrival.timestamp_millis();
    let nav_departure_time = ship.nav.route.departure_time.timestamp_millis();
    let cargo = ship
        .cargo
        .inventory
        .iter()
        .map(|item| (item.symbol.clone(), item.units))
        .collect();
    ShipEntity {
        symbol: ship.symbol.clone(),
        speed: ship.engine.speed,
        waypoint: ship.nav.waypoint_symbol.to_string(),
        is_docked,
        fuel: ship.fuel.current,
        cargo,
        nav_source,
        nav_arrival_time,
        nav_departure_time,
    }
}

fn apply_ship_nav(ship_entity: &mut ShipEntity, nav: &ShipNav) {
    let is_docked = nav.status == ShipNavStatus::Docked;
    let nav_source = nav.route.origin.symbol.to_string();
    let nav_arrival_time = nav.route.arrival.timestamp_millis();
    let nav_departure_time = nav.route.departure_time.timestamp_millis();

    ship_entity.waypoint = nav.waypoint_symbol.to_string();
    ship_entity.is_docked = is_docked;
    ship_entity.nav_source = nav_source;
    ship_entity.nav_arrival_time = nav_arrival_time;
    ship_entity.nav_departure_time = nav_departure_time;
}

fn apply_ship_fuel(ship_entity: &mut ShipEntity, fuel: &ShipFuel) {
    ship_entity.fuel = fuel.current;
}

fn get_ship_entity_update(prev: &ShipEntity, new: &ShipEntity) -> ShipEntityUpdate {
    let mut update = ShipEntityUpdate::default();
    if prev.symbol != new.symbol {
        update.symbol = Some(new.symbol.clone());
    }
    if prev.speed != new.speed {
        update.speed = Some(new.speed);
    }
    if prev.waypoint != new.waypoint {
        update.waypoint = Some(new.waypoint.clone());
    }
    if prev.is_docked != new.is_docked {
        update.is_docked = Some(new.is_docked);
    }
    if prev.fuel != new.fuel {
        update.fuel = Some(new.fuel);
    }
    if prev.cargo != new.cargo {
        update.cargo = Some(new.cargo.clone());
    }
    if prev.nav_source != new.nav_source {
        update.nav_source = Some(new.nav_source.clone());
    }
    if prev.nav_arrival_time != new.nav_arrival_time {
        update.nav_arrival_time = Some(new.nav_arrival_time);
    }
    if prev.nav_departure_time != new.nav_departure_time {
        update.nav_departure_time = Some(new.nav_departure_time);
    }
    update
}
