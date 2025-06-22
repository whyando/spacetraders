pub mod pathfinding;

use crate::api_client::ApiClient;
use crate::api_client::api_models::{self, WaypointDetailed};
use crate::database::DbClient;
use crate::database::db_models;
use crate::database::db_models::NewWaypointDetails;
use crate::models::{
    Construction, Data, Faction, Market, MarketRemoteView, ShipFlightMode, Shipyard,
    ShipyardRemoteView, System, SystemSymbol, Waypoint, WaypointSymbol, WithTimestamp,
};
use crate::models::{SymbolNameDescr, WaypointDetails};
use crate::pathfinding::{Pathfinding, Route};
use crate::{schema::*, util};
use dashmap::DashMap;
use diesel::BelongingToDsl as _;
use diesel::ExpressionMethods as _;
use diesel::GroupedBy as _;
use diesel::QueryDsl as _;
use diesel::SelectableHelper as _;
use diesel_async::RunQueryDsl as _;
use log::*;
use moka::future::Cache;
use std::collections::BTreeMap;
use std::sync::Arc;

use self::pathfinding::WarpEdge;

pub enum WaypointFilter {
    Imports(String),
    Exports(String),
    Exchanges(String),
    // waypoint traits
    Market,
    Shipyard,
    // waypoint types
    GasGiant,
    EngineeredAsteroid,
    JumpGate,
}

#[derive(Debug, Clone)]
pub struct JumpGateInfo {
    pub is_constructed: bool,
    pub connections: Vec<WaypointSymbol>,
}

pub struct NavEdge {
    pub flight_mode: ShipFlightMode,
    pub fuel_cost: i64,
    pub distance: i64,
    pub duration: i64,
}

pub struct Universe {
    api_client: ApiClient,
    db: DbClient,

    systems: DashMap<SystemSymbol, System>,
    constructions: DashMap<WaypointSymbol, Arc<WithTimestamp<Option<Construction>>>>,
    remote_markets: DashMap<WaypointSymbol, MarketRemoteView>,
    remote_shipyards: DashMap<WaypointSymbol, ShipyardRemoteView>,
    markets: DashMap<WaypointSymbol, Arc<WithTimestamp<Market>>>,
    shipyards: DashMap<WaypointSymbol, Arc<WithTimestamp<Shipyard>>>,
    factions: DashMap<String, Faction>,
    jumpgates: DashMap<WaypointSymbol, JumpGateInfo>,

    // cache
    warp_jump_graph: Cache<(), BTreeMap<SystemSymbol, BTreeMap<SystemSymbol, WarpEdge>>>,
}

impl Universe {
    pub async fn new(api_client: &ApiClient, db: &DbClient) -> Self {
        let db = db.clone();
        let systems = load_systems(&db).await;
        let jumpgates = load_jumpgates(&db).await;
        let factions = load_factions(&db, api_client).await;
        let remote_markets = load_remote_markets(&db).await;
        let remote_shipyards = load_remote_shipyards(&db).await;
        let markets = load_markets(&db).await;
        let shipyards = load_shipyards(&db).await;
        Self {
            api_client: api_client.clone(),
            db: db.clone(),

            systems: DashMap::from_iter(systems.into_iter()),
            constructions: DashMap::new(),
            remote_markets: DashMap::from_iter(remote_markets.into_iter()),
            remote_shipyards: DashMap::from_iter(remote_shipyards.into_iter()),
            markets: DashMap::from_iter(markets.into_iter()),
            shipyards: DashMap::from_iter(shipyards.into_iter()),
            factions: DashMap::from_iter(factions.into_iter()),
            jumpgates: DashMap::from_iter(jumpgates.into_iter()),

            warp_jump_graph: Cache::new(1),
        }
    }

    pub fn connections_known(&self, waypoint: &WaypointSymbol) -> bool {
        self.jumpgates.contains_key(waypoint)
    }

    pub fn systems(&self) -> Vec<System> {
        self.systems.iter().map(|x| x.value().clone()).collect()
    }
    pub fn num_systems(&self) -> usize {
        self.systems.len()
    }
    pub fn num_waypoints(&self) -> usize {
        self.systems.iter().map(|s| s.value().waypoints.len()).sum()
    }
    pub fn system(&self, symbol: &SystemSymbol) -> System {
        self.systems
            .get(symbol)
            .expect("System not found")
            .value()
            .clone()
    }
    pub fn waypoint(&self, symbol: &WaypointSymbol) -> Waypoint {
        let system_symbol = symbol.system();
        let system = self.systems.get(&system_symbol).expect("System not found");
        system
            .value()
            .waypoints
            .iter()
            .find(|w| &w.symbol == symbol)
            .expect("Waypoint not found")
            .clone()
    }

    pub fn get_market(
        &self,
        waypoint_symbol: &WaypointSymbol,
    ) -> Option<Arc<WithTimestamp<Market>>> {
        self.markets.get(waypoint_symbol).map(|x| x.value().clone())
    }

    pub async fn save_market(
        &self,
        waypoint_symbol: &WaypointSymbol,
        market: WithTimestamp<Market>,
    ) {
        self.markets
            .insert(waypoint_symbol.clone(), Arc::new(market.clone()));
        self.db.save_market(waypoint_symbol, &market).await;
        self.db.insert_market_trades(&market).await;
        self.db.upsert_market_transactions(&market).await;
    }

    pub fn get_shipyard(
        &self,
        waypoint_symbol: &WaypointSymbol,
    ) -> Option<Arc<WithTimestamp<Shipyard>>> {
        self.shipyards
            .get(waypoint_symbol)
            .map(|x| x.value().clone())
    }

    pub async fn save_shipyard(
        &self,
        waypoint_symbol: &WaypointSymbol,
        shipyard: WithTimestamp<Shipyard>,
    ) {
        self.shipyards
            .insert(waypoint_symbol.clone(), Arc::new(shipyard.clone()));
        self.db.save_shipyard(waypoint_symbol, &shipyard).await;
    }

    // load Optional<Construction> from db, or fetch from api
    // we should only do initial fetch from api once, and rely on other processes to update
    pub async fn load_construction(
        &self,
        symbol: &WaypointSymbol,
    ) -> WithTimestamp<Option<Construction>> {
        match self.db.get_construction(symbol).await {
            Some(site) => site,
            None => {
                let site = self.api_client.get_construction(symbol).await;
                self.db.save_construction(symbol, &site).await;
                site
            }
        }
    }

    pub async fn get_construction(
        &self,
        symbol: &WaypointSymbol,
    ) -> Arc<WithTimestamp<Option<Construction>>> {
        match self.constructions.get(symbol) {
            Some(construction) => construction.clone(),
            None => {
                let construction = self.load_construction(symbol).await;
                let construction = Arc::new(construction);
                self.constructions
                    .insert(symbol.clone(), construction.clone());
                construction
            }
        }
    }

    pub async fn update_construction(&self, construction: &Construction) {
        let symbol = &construction.symbol;
        let construction = WithTimestamp {
            data: Some(construction.clone()),
            timestamp: chrono::Utc::now(),
        };
        self.constructions
            .insert(symbol.clone(), Arc::new(construction.clone()));
        self.db.save_construction(symbol, &construction).await;
    }

    pub fn get_faction(&self, faction: &str) -> Faction {
        self.factions.get(faction).unwrap().clone()
    }

    pub fn get_factions(&self) -> Vec<Faction> {
        self.factions.iter().map(|x| x.value().clone()).collect()
    }

    pub async fn ensure_system_loaded(&self, symbol: &SystemSymbol) {
        if !self.systems.contains_key(symbol) {
            self.load_system(symbol).await;
        }
    }

    // Fetch system info from API, insert to database and cache
    pub async fn load_system(&self, symbol: &SystemSymbol) {
        // 1. Get from API (single system)
        let system: api_models::System = self
            .api_client
            .get::<Data<api_models::System>>(&format!("/systems/{}", symbol))
            .await
            .data;

        // 2. Insert to database. tables: `systems` and `waypoints`
        // Insert system
        let system_insert = db_models::NewSystem {
            symbol: system.symbol.as_str(),
            type_: &system.system_type,
            x: system.x as i32,
            y: system.y as i32,
        };
        let system_id = self.db.insert_system(&system_insert).await;

        // Insert waypoints
        let waypoint_inserts = system
            .waypoints
            .iter()
            .map(|w| db_models::NewWaypoint {
                symbol: w.symbol.as_str(),
                system_id: system_id,
                type_: &w.waypoint_type,
                x: w.x as i32,
                y: w.y as i32,
            })
            .collect::<Vec<_>>();
        let waypoint_ids = self.db.insert_waypoints(&waypoint_inserts).await;

        let waypoint_id_map = std::iter::zip(waypoint_ids, waypoint_inserts)
            .map(|(id, waypoint)| (waypoint.symbol.to_string(), id))
            .collect::<std::collections::HashMap<_, _>>();

        // 3. Finally load to cache
        let system = System {
            symbol: system.symbol.clone(),
            system_type: system.system_type,
            x: system.x,
            y: system.y,
            waypoints: system
                .waypoints
                .into_iter()
                .map(|waypoint| Waypoint {
                    id: waypoint_id_map[waypoint.symbol.as_str()],
                    symbol: waypoint.symbol.clone(),
                    waypoint_type: waypoint.waypoint_type,
                    x: waypoint.x,
                    y: waypoint.y,
                    details: None,
                })
                .collect(),
        };
        self.systems.insert(system.symbol.clone(), system);
    }

    pub async fn get_system_waypoints(&self, symbol: &SystemSymbol) -> Vec<WaypointDetailed> {
        let system = self.system(symbol);
        // Collect Vec<Option<_>> to Option<Vec<_>>
        let waypoints: Option<Vec<WaypointDetailed>> = system
            .waypoints
            .iter()
            .map(|w| match &w.details {
                Some(details) => {
                    let mut traits = vec![];
                    if details.is_market {
                        traits.push("MARKETPLACE".to_string());
                    }
                    if details.is_shipyard {
                        traits.push("SHIPYARD".to_string());
                    }
                    if details.is_uncharted {
                        traits.push("UNCHARTED".to_string());
                    }
                    let traits = traits
                        .into_iter()
                        .map(|symbol| SymbolNameDescr {
                            symbol,
                            name: String::new(),
                            description: String::new(),
                        })
                        .collect();
                    Some(WaypointDetailed {
                        system_symbol: symbol.clone(),
                        symbol: w.symbol.clone(),
                        waypoint_type: w.waypoint_type.clone(),
                        x: w.x,
                        y: w.y,
                        traits: traits,
                        is_under_construction: details.is_under_construction,
                        orbitals: vec![],
                        orbits: None,
                        faction: None,
                        modifiers: vec![],
                        chart: None,
                    })
                }
                None => None,
            })
            .collect();
        match waypoints {
            Some(waypoints) => waypoints,
            None => {
                let waypoints: Vec<WaypointDetailed> =
                    self.api_client.get_system_waypoints(symbol).await;
                assert_eq!(waypoints.len(), system.waypoints.len());
                let inserts: Vec<_> = waypoints
                    .iter()
                    .map(|waypoint| {
                        let db_waypoint = system
                            .waypoints
                            .iter()
                            .find(|w| &w.symbol == &waypoint.symbol)
                            .expect("Waypoint not found");
                        NewWaypointDetails {
                            waypoint_id: db_waypoint.id,
                            is_market: waypoint.is_market(),
                            is_shipyard: waypoint.is_shipyard(),
                            is_uncharted: waypoint.is_uncharted(),
                            is_under_construction: waypoint.is_under_construction,
                        }
                    })
                    .collect();
                diesel::insert_into(waypoint_details::table)
                    .values(inserts)
                    .on_conflict(waypoint_details::waypoint_id)
                    .do_nothing()
                    .execute(&mut self.db.conn().await)
                    .await
                    .expect("DB Insert error");
                // load to memory (self.systems)
                let mut s = self.systems.get_mut(symbol).unwrap();
                let s = s.value_mut();
                assert_eq!(s.waypoints.len(), waypoints.len());
                for w in s.waypoints.iter_mut() {
                    let waypoint = waypoints
                        .iter()
                        .find(|w2| &w2.symbol == &w.symbol)
                        .expect("Waypoint not found");
                    w.details = Some(WaypointDetails {
                        is_market: waypoint.is_market(),
                        is_shipyard: waypoint.is_shipyard(),
                        is_uncharted: waypoint.is_uncharted(),
                        is_under_construction: waypoint.is_under_construction,
                    });
                }
                waypoints
            }
        }
    }

    pub async fn get_system_markets(
        &self,
        symbol: &SystemSymbol,
    ) -> Vec<(MarketRemoteView, Option<Arc<WithTimestamp<Market>>>)> {
        let waypoints = self.get_system_waypoints(symbol).await;
        let mut markets = Vec::new();
        for waypoint in &waypoints {
            if waypoint.is_market() {
                let market_remote = self.get_market_remote(&waypoint.symbol).await;
                let market_opt = self.get_market(&waypoint.symbol);
                markets.push((market_remote, market_opt));
            }
        }
        markets
    }

    pub async fn get_system_shipyards(
        &self,
        symbol: &SystemSymbol,
    ) -> Vec<(ShipyardRemoteView, Option<Arc<WithTimestamp<Shipyard>>>)> {
        let waypoints = self.get_system_waypoints(symbol).await;
        let mut shipyards = Vec::new();
        for waypoint in &waypoints {
            if waypoint.is_shipyard() {
                let shipyard_remote = self.get_shipyard_remote(&waypoint.symbol).await;
                let shipyard_opt = self.get_shipyard(&waypoint.symbol);
                shipyards.push((shipyard_remote, shipyard_opt));
            }
        }
        shipyards
    }

    pub async fn get_system_markets_remote(&self, symbol: &SystemSymbol) -> Vec<MarketRemoteView> {
        let waypoints = self.get_system_waypoints(symbol).await;
        let mut markets = Vec::new();
        for waypoint in &waypoints {
            if waypoint.is_market() {
                let market_remote = self.get_market_remote(&waypoint.symbol).await;
                markets.push(market_remote);
            }
        }
        markets
    }

    pub async fn get_system_shipyards_remote(
        &self,
        symbol: &SystemSymbol,
    ) -> Vec<ShipyardRemoteView> {
        let waypoints = self.get_system_waypoints(symbol).await;
        let mut shipyards = Vec::new();
        for waypoint in &waypoints {
            if waypoint.is_shipyard() {
                let shipyard_remote = self.get_shipyard_remote(&waypoint.symbol).await;
                shipyards.push(shipyard_remote);
            }
        }
        shipyards
    }

    pub async fn detailed_waypoint(&self, symbol: &WaypointSymbol) -> WaypointDetailed {
        let system_waypoints = self.get_system_waypoints(&symbol.system()).await;
        system_waypoints
            .into_iter()
            .find(|waypoint| &waypoint.symbol == symbol)
            .unwrap()
    }

    pub async fn get_market_remote(&self, symbol: &WaypointSymbol) -> MarketRemoteView {
        // Layer 1 - check cache
        if let Some(market) = &self.remote_markets.get(symbol) {
            return market.value().clone();
        }
        // Layer 2 - fetch from api and save
        let market = self.api_client.get_market_remote(symbol).await;
        self.db.save_market_remote(symbol, &market).await;
        self.remote_markets.insert(symbol.clone(), market.clone());
        market
    }

    pub async fn get_shipyard_remote(&self, symbol: &WaypointSymbol) -> ShipyardRemoteView {
        // Layer 1 - check cache
        if let Some(shipyard) = &self.remote_shipyards.get(symbol) {
            return shipyard.value().clone();
        }
        // Layer 2 - fetch from api and save
        let shipyard = self.api_client.get_shipyard_remote(symbol).await;
        self.db.save_shipyard_remote(symbol, &shipyard).await;
        self.remote_shipyards
            .insert(symbol.clone(), shipyard.clone());
        shipyard
    }

    pub async fn search_shipyards(
        &self,
        system_symbol: &SystemSymbol,
        ship_model: &str,
    ) -> Vec<(WaypointSymbol, i64)> {
        let waypoints = self.get_system_waypoints(system_symbol).await;
        let mut shipyards = Vec::new();
        for waypoint in waypoints {
            if !waypoint.is_shipyard() {
                continue;
            }
            if let Some(shipyard) = self.get_shipyard(&waypoint.symbol) {
                if let Some(ship) = shipyard
                    .data
                    .ships
                    .iter()
                    .find(|ship| ship.ship_type == ship_model)
                {
                    shipyards.push((waypoint.symbol.clone(), ship.purchase_price));
                }
            }
        }
        shipyards
    }

    async fn matches_filter(&self, waypoint: &WaypointDetailed, filter: &WaypointFilter) -> bool {
        match filter {
            WaypointFilter::Imports(good) => {
                if !waypoint.is_market() {
                    return false;
                }
                let market = self.get_market_remote(&waypoint.symbol).await;
                market.imports.iter().any(|import| import.symbol == *good)
            }
            WaypointFilter::Exports(good) => {
                if !waypoint.is_market() {
                    return false;
                }
                let market = self.get_market_remote(&waypoint.symbol).await;
                market.exports.iter().any(|export| export.symbol == *good)
            }
            WaypointFilter::Exchanges(good) => {
                if !waypoint.is_market() {
                    return false;
                }
                let market = self.get_market_remote(&waypoint.symbol).await;
                market
                    .exchange
                    .iter()
                    .any(|exchange| exchange.symbol == *good)
            }
            WaypointFilter::Market => waypoint.is_market(),
            WaypointFilter::Shipyard => waypoint.is_shipyard(),
            WaypointFilter::GasGiant => waypoint.is_gas_giant(),
            WaypointFilter::EngineeredAsteroid => waypoint.is_engineered_asteroid(),
            WaypointFilter::JumpGate => waypoint.is_jump_gate(),
        }
    }

    pub async fn search_waypoints(
        &self,
        system_symbol: &SystemSymbol,
        filters: &[WaypointFilter],
    ) -> Vec<WaypointDetailed> {
        let waypoints = self.get_system_waypoints(system_symbol).await;
        let mut filtered = Vec::new();
        for waypoint in waypoints {
            // matches_filter is async
            let mut matches = true;
            for filter in filters {
                if !self.matches_filter(&waypoint, filter).await {
                    matches = false;
                    break;
                }
            }
            if matches {
                filtered.push(waypoint);
            }
        }
        filtered
    }

    pub async fn get_route(
        &self,
        src: &WaypointSymbol,
        dest: &WaypointSymbol,
        speed: i64,
        start_fuel: i64,
        fuel_capacity: i64,
    ) -> Route {
        let system_symbol = src.system();
        assert_eq!(system_symbol, dest.system());
        let waypoints = self.get_system_waypoints(&system_symbol).await;
        let pathfinding = Pathfinding::new(waypoints);
        pathfinding.get_route(src, dest, speed, start_fuel, fuel_capacity)
    }

    pub async fn get_jumpgate_opt(&self, symbol: &SystemSymbol) -> Option<WaypointSymbol> {
        let waypoints = self.get_system_waypoints(symbol).await;
        waypoints
            .into_iter()
            .find(|waypoint| waypoint.is_jump_gate())
            .map(|waypoint| waypoint.symbol)
    }

    pub async fn get_jumpgate(&self, symbol: &SystemSymbol) -> WaypointSymbol {
        self.get_jumpgate_opt(symbol)
            .await
            .expect("No jumpgate found")
    }

    pub async fn first_waypoint(&self, symbol: &SystemSymbol) -> WaypointSymbol {
        self.get_system_waypoints(&symbol).await[0].symbol.clone()
    }

    // Get jumpgate connections for a charted system
    pub async fn get_jumpgate_connections(&self, symbol: &WaypointSymbol) -> JumpGateInfo {
        if let Some(jumpgate_info) = &self.jumpgates.get(symbol) {
            return jumpgate_info.value().clone();
        }

        // Otherwise fetch from API
        let waypoint = self.detailed_waypoint(symbol).await;
        let connections = self.api_client.get_jumpgate_conns(symbol).await;
        let info = JumpGateInfo {
            is_constructed: !waypoint.is_under_construction,
            connections,
        };
        let insert = db_models::NewJumpGateConnections {
            waypoint_symbol: symbol.as_str(),
            is_under_construction: !info.is_constructed,
            edges: info
                .connections
                .iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>(),
        };
        diesel::insert_into(jumpgate_connections::table)
            .values(&insert)
            .on_conflict(jumpgate_connections::waypoint_symbol)
            .do_update()
            .set((
                jumpgate_connections::is_under_construction.eq(&insert.is_under_construction),
                jumpgate_connections::edges.eq(&insert.edges),
            ))
            .execute(&mut self.db.conn().await)
            .await
            .expect("DB Insert error");
        self.jumpgates.insert(symbol.clone(), info.clone());
        info
    }

    // Returns a matrix between market waypoints. Assumes we can refuel at any waypoint.
    // Weights are the travel duration in seconds between two waypoints
    // Preferring BURN flight mode, and only CRUISE if the fuel capacity isn't high enough
    pub async fn market_adjacency_edges<'a>(
        &self,
        market_waypoints: &'a [WaypointDetailed],
        ship_max_fuel: i64,
        ship_speed: i64,
    ) -> Vec<BTreeMap<usize, NavEdge>> {
        let mut edges = Vec::new();
        for w1 in market_waypoints.iter() {
            let mut row = BTreeMap::new();
            for (j, w2) in market_waypoints.iter().enumerate() {
                let dist = util::distance(w1, w2);
                let burn_fuel = util::fuel_cost(&ShipFlightMode::Burn, dist);
                let cruise_fuel = util::fuel_cost(&ShipFlightMode::Cruise, dist);
                if burn_fuel <= ship_max_fuel {
                    row.insert(
                        j,
                        NavEdge {
                            flight_mode: ShipFlightMode::Burn,
                            fuel_cost: burn_fuel,
                            distance: dist,
                            duration: util::estimated_travel_duration(
                                &ShipFlightMode::Burn,
                                ship_speed,
                                dist,
                            ),
                        },
                    );
                } else if cruise_fuel <= ship_max_fuel {
                    row.insert(
                        j,
                        NavEdge {
                            flight_mode: ShipFlightMode::Cruise,
                            fuel_cost: cruise_fuel,
                            distance: dist,
                            duration: util::estimated_travel_duration(
                                &ShipFlightMode::Cruise,
                                ship_speed,
                                dist,
                            ),
                        },
                    );
                }
            }
            edges.push(row);
        }
        assert_eq!(edges.len(), market_waypoints.len());
        edges
    }

    pub async fn full_travel_matrix(
        &self,
        market_waypoints: &[WaypointDetailed],
        ship_max_fuel: i64,
        ship_speed: i64,
    ) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
        let mut durations: Vec<Vec<f64>> =
            vec![vec![0.; market_waypoints.len()]; market_waypoints.len()];
        let mut distances: Vec<Vec<f64>> =
            vec![vec![0.; market_waypoints.len()]; market_waypoints.len()];
        let edges = self
            .market_adjacency_edges(market_waypoints, ship_max_fuel, ship_speed)
            .await;
        for i in 0..market_waypoints.len() {
            for j in 0..market_waypoints.len() {
                if i == j {
                    durations[i][j] = 0.;
                    distances[i][j] = 0.;
                } else {
                    if let Some(edge) = edges[i].get(&j) {
                        durations[i][j] = edge.duration as f64;
                        distances[i][j] = edge.distance as f64;
                    } else {
                        durations[i][j] = f64::INFINITY;
                        distances[i][j] = f64::INFINITY;
                    }
                }
            }
        }

        // Fill in the rest of the matrix with floyd warshall
        for k in 0..market_waypoints.len() {
            for i in 0..market_waypoints.len() {
                for j in 0..market_waypoints.len() {
                    durations[i][j] = durations[i][j].min(durations[i][k] + durations[k][j]);
                    distances[i][j] = distances[i][j].min(distances[i][k] + distances[k][j]);
                }
            }
        }
        (durations, distances)
    }
}

// Load all rows from `systems`, `waypoints` and `waypoint_details` tables
async fn load_systems(db: &DbClient) -> BTreeMap<SystemSymbol, System> {
    let query_start = std::time::Instant::now();
    let systems: Vec<db_models::System> = systems::table
        .select(db_models::System::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!("Loaded {} systems in {:.3}s", systems.len(), duration);

    let query_start = std::time::Instant::now();
    let waypoints = db_models::Waypoint::belonging_to(&systems)
        .select(db_models::Waypoint::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!("Loaded {} waypoints in {:.3}s", waypoints.len(), duration);

    let query_start = std::time::Instant::now();
    let waypoint_details = db_models::WaypointDetails::belonging_to(&waypoints)
        .select(db_models::WaypointDetails::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!(
        "Loaded {} waypoint details in {:.3}s",
        waypoint_details.len(),
        duration
    );

    let grouped_details = waypoint_details.grouped_by(&waypoints);
    let waypoints = waypoints
        .into_iter()
        .zip(grouped_details)
        .grouped_by(&systems);

    let mut result = BTreeMap::new();
    let system_iter = std::iter::zip(systems, waypoints);
    for (system, waypoints) in system_iter {
        let waypoints = waypoints
            .into_iter()
            .map(|(waypoint, details)| {
                let details = match details.len() {
                    0 => None,
                    1 => {
                        let details = details.into_iter().next().unwrap();
                        Some(WaypointDetails {
                            is_under_construction: details.is_under_construction,
                            is_market: details.is_market,
                            is_shipyard: details.is_shipyard,
                            is_uncharted: details.is_uncharted,
                        })
                    }
                    _ => panic!("Multiple details for waypoint"),
                };
                Waypoint {
                    id: waypoint.id,
                    symbol: WaypointSymbol::new(&waypoint.symbol),
                    waypoint_type: waypoint.type_,
                    x: waypoint.x as i64,
                    y: waypoint.y as i64,
                    details,
                }
            })
            .collect();
        result.insert(
            SystemSymbol::new(&system.symbol),
            System {
                symbol: SystemSymbol::new(&system.symbol),
                system_type: system.type_,
                x: system.x as i64,
                y: system.y as i64,
                waypoints,
            },
        );
    }
    result
}

// Load all rows from `jumpgate_connections` table
async fn load_jumpgates(db: &DbClient) -> BTreeMap<WaypointSymbol, JumpGateInfo> {
    let query_start = std::time::Instant::now();
    let jumpgates: Vec<db_models::JumpGateConnections> = jumpgate_connections::table
        .select(db_models::JumpGateConnections::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!("Loaded {} jumpgates in {:.3}s", jumpgates.len(), duration);

    let mut result = BTreeMap::new();
    for jumpgate in jumpgates {
        result.insert(
            WaypointSymbol::new(&jumpgate.waypoint_symbol),
            JumpGateInfo {
                is_constructed: !jumpgate.is_under_construction,
                connections: jumpgate
                    .edges
                    .iter()
                    .map(|symbol| WaypointSymbol::new(symbol))
                    .collect(),
            },
        );
    }
    result
}

// Load factions from db, or fetch from api
async fn load_factions(db: &DbClient, api_client: &ApiClient) -> BTreeMap<String, Faction> {
    match db.get_factions().await {
        Some(factions) => factions
            .into_iter()
            .map(|faction| (faction.symbol.clone(), faction))
            .collect(),
        None => {
            // Layer - fetch from api
            let factions: Vec<Faction> = api_client.get_all_pages("/factions").await;
            db.set_factions(&factions).await;
            factions
                .into_iter()
                .map(|faction| (faction.symbol.clone(), faction))
                .collect()
        }
    }
}

// Load all rows from `remote_markets` table
async fn load_remote_markets(db: &DbClient) -> BTreeMap<WaypointSymbol, MarketRemoteView> {
    let query_start = std::time::Instant::now();
    let markets: Vec<db_models::RemoteMarket> = remote_markets::table
        .select(db_models::RemoteMarket::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!(
        "Loaded {} remote markets in {:.3}s",
        markets.len(),
        duration
    );

    let mut result = BTreeMap::new();
    for market in markets {
        let market_data: MarketRemoteView =
            serde_json::from_value(market.market_data).expect("Invalid market data");
        result.insert(market_data.symbol.clone(), market_data);
    }
    result
}

// Load all rows from `remote_shipyards` table
async fn load_remote_shipyards(db: &DbClient) -> BTreeMap<WaypointSymbol, ShipyardRemoteView> {
    let query_start = std::time::Instant::now();
    let shipyards: Vec<db_models::RemoteShipyard> = remote_shipyards::table
        .select(db_models::RemoteShipyard::as_select())
        .load(&mut db.conn().await)
        .await
        .expect("DB Query error");
    let duration = query_start.elapsed().as_millis() as f64 / 1000.0;
    info!(
        "Loaded {} remote shipyards in {:.3}s",
        shipyards.len(),
        duration
    );

    let mut result = BTreeMap::new();
    for shipyard in shipyards {
        let shipyard_data: ShipyardRemoteView =
            serde_json::from_value(shipyard.shipyard_data).expect("Invalid shipyard data");
        result.insert(shipyard_data.symbol.clone(), shipyard_data);
    }
    result
}

async fn load_markets(db: &DbClient) -> Vec<(WaypointSymbol, Arc<WithTimestamp<Market>>)> {
    let markets = db.get_all_markets().await;
    markets
        .into_iter()
        .map(|(symbol, market)| (symbol, Arc::new(market)))
        .collect()
}

async fn load_shipyards(db: &DbClient) -> Vec<(WaypointSymbol, Arc<WithTimestamp<Shipyard>>)> {
    let shipyards = db.get_all_shipyards().await;
    shipyards
        .into_iter()
        .map(|(symbol, shipyard)| (symbol, Arc::new(shipyard)))
        .collect()
}
