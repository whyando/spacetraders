pub mod pathfinding;

use crate::api_client::ApiClient;
use crate::api_client::api_models::{self, WaypointDetailed};
use crate::database::DbClient;
use crate::database::db_models;
use crate::database::db_models::NewWaypointDetails;
use crate::models::{
    Construction, Data, Faction, Market, MarketRemoteView, Shipyard, ShipyardRemoteView, System,
    SystemSymbol, Waypoint, WaypointSymbol, WithTimestamp,
};
use crate::models::{SymbolNameDescr, WaypointDetails};
use crate::pathfinding::{Pathfinding, Route};
use crate::schema::*;
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

use self::pathfinding::{JumpGate, WarpEdge};

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

pub use pathfinding::NavEdge;

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

    // flips to true once the full galaxy of systems has been loaded into the DB +
    // cache; full-galaxy consumers (jumpgate/warp graphs) await this.
    systems_ready: tokio::sync::watch::Sender<bool>,

    // cache
    warp_jump_graph: Cache<(), BTreeMap<SystemSymbol, BTreeMap<SystemSymbol, WarpEdge>>>,
    // cached jumpgate graph; invalidated whenever a gate's connections change
    jumpgate_graph: Cache<(), Arc<BTreeMap<WaypointSymbol, JumpGate>>>,
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
        // Ready only once both phases are done: systems bulk-loaded AND gate-system
        // waypoint details (construction status) fetched.
        let galaxy_loaded = db.get_value::<bool>("galaxy_loaded").await.unwrap_or(false);
        let gate_waypoints_loaded = db
            .get_value::<bool>("gate_waypoints_loaded")
            .await
            .unwrap_or(false);
        let (systems_ready, _) =
            tokio::sync::watch::channel(galaxy_loaded && gate_waypoints_loaded);
        Self {
            api_client: api_client.clone(),
            db: db.clone(),

            systems: DashMap::from_iter(systems),
            constructions: DashMap::new(),
            remote_markets: DashMap::from_iter(remote_markets),
            remote_shipyards: DashMap::from_iter(remote_shipyards),
            markets: DashMap::from_iter(markets),
            shipyards: DashMap::from_iter(shipyards),
            factions: DashMap::from_iter(factions),
            jumpgates: DashMap::from_iter(jumpgates),
            systems_ready,

            warp_jump_graph: Cache::new(1),
            jumpgate_graph: Cache::new(1),
        }
    }

    // Test seam: build a Universe directly from in-memory caches, bypassing the DB load
    // in `new`. `systems_ready` starts true so full-galaxy consumers don't block. Used to
    // exercise pure-over-cache builders (jumpgate/warp graphs) offline — the DB is
    // `disconnected` and must not be touched, so pre-populate `constructions` for any
    // under-construction gate the build will confirm.
    #[cfg(test)]
    pub(crate) fn from_caches_for_test(
        api_client: ApiClient,
        db: DbClient,
        systems: Vec<(SystemSymbol, System)>,
        jumpgates: Vec<(WaypointSymbol, JumpGateInfo)>,
        constructions: Vec<(WaypointSymbol, Arc<WithTimestamp<Option<Construction>>>)>,
    ) -> Universe {
        let (systems_ready, _) = tokio::sync::watch::channel(true);
        Universe {
            api_client,
            db,
            systems: DashMap::from_iter(systems),
            constructions: DashMap::from_iter(constructions),
            remote_markets: DashMap::new(),
            remote_shipyards: DashMap::new(),
            markets: DashMap::new(),
            shipyards: DashMap::new(),
            factions: DashMap::new(),
            jumpgates: DashMap::from_iter(jumpgates),
            systems_ready,
            warp_jump_graph: Cache::new(1),
            jumpgate_graph: Cache::new(1),
        }
    }

    // Wait until the full galaxy has been loaded (returns immediately if already loaded).
    pub async fn await_systems_loaded(&self) {
        let mut rx = self.systems_ready.subscribe();
        if *rx.borrow_and_update() {
            return;
        }
        while rx.changed().await.is_ok() {
            if *rx.borrow_and_update() {
                return;
            }
        }
    }

    // Kick off the one-time background load: bulk-load every system, then fetch
    // waypoint details for jumpgate systems. Each phase is marker-guarded, so an
    // upgrade that only adds the second phase still runs it.
    pub fn spawn_galaxy_load(self: &Arc<Self>) {
        if *self.systems_ready.borrow() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            if !this
                .db
                .get_value::<bool>("galaxy_loaded")
                .await
                .unwrap_or(false)
            {
                this.load_all_systems().await;
                this.db.set_value("galaxy_loaded", &true).await;
                info!("Galaxy load complete: {} systems", this.num_systems());
            }
            if !this
                .db
                .get_value::<bool>("gate_waypoints_loaded")
                .await
                .unwrap_or(false)
            {
                this.load_gate_waypoints().await;
                this.db.set_value("gate_waypoints_loaded", &true).await;
            }
            let _ = this.systems_ready.send(true);
            info!("Systems ready (galaxy + gate waypoints loaded)");
        });
    }

    // One-time, off-lock warm of construction sites for every gate flagged
    // under-construction, so the jumpgate graph build (which reads cache/DB only, never
    // the API — see construction_cached) has real status without a per-build API sweep.
    // Deliberately NOT gated on `systems_ready`: the lock-holding graph build awaits
    // that barrier, so folding this multi-minute sweep into it would just relocate the
    // try_buy_ships lock-timeout crash it exists to prevent. Marker-guarded so it runs
    // once per DB (and once on upgrade for agents past the gate-waypoint phase).
    pub fn spawn_construction_load(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            if this
                .db
                .get_value::<bool>("gate_construction_loaded")
                .await
                .unwrap_or(false)
            {
                return;
            }
            // Needs gate waypoint details (is_under_construction flags) loaded.
            this.await_systems_loaded().await;
            let gate_systems = this.systems();
            let under_construction: Vec<WaypointSymbol> = gate_systems
                .iter()
                .flat_map(|s| s.waypoints.iter())
                .filter(|w| w.waypoint_type == "JUMP_GATE")
                .filter(|w| {
                    w.details
                        .as_ref()
                        .map(|d| d.is_under_construction)
                        .unwrap_or(false)
                })
                .map(|w| w.symbol.clone())
                .collect();
            info!(
                "Warming construction status for {} under-construction gates...",
                under_construction.len()
            );
            for (i, gate) in under_construction.iter().enumerate() {
                // fetch + persist off any lock; serial, so it respects the rate limit.
                this.get_construction(gate).await;
                if (i + 1) % 250 == 0 {
                    info!("  ...{}/{} gates", i + 1, under_construction.len());
                }
            }
            // Rebuild the memoized graph with the now-warm construction data so gates
            // conservatively excluded during the warm window are re-evaluated.
            this.jumpgate_graph.invalidate(&()).await;
            this.db.set_value("gate_construction_loaded", &true).await;
            info!("Construction status warm complete");
        });
    }

    // Fetch the full paginated systems list and bulk-insert systems + waypoints,
    // then repopulate the in-memory cache from the DB.
    async fn load_all_systems(&self) {
        let start = std::time::Instant::now();
        let systems: Vec<api_models::System> = self.api_client.get_all_pages("/systems").await;
        info!(
            "Fetched {} systems from API in {:.1}s",
            systems.len(),
            start.elapsed().as_secs_f64()
        );

        let system_inserts: Vec<db_models::NewSystem> = systems
            .iter()
            .map(|s| db_models::NewSystem {
                symbol: s.symbol.as_str(),
                type_: &s.system_type,
                x: s.x as i32,
                y: s.y as i32,
            })
            .collect();
        let system_ids = self.db.insert_systems(&system_inserts).await;

        let mut waypoint_inserts: Vec<db_models::NewWaypoint> = Vec::new();
        for (s, &system_id) in systems.iter().zip(system_ids.iter()) {
            for w in &s.waypoints {
                waypoint_inserts.push(db_models::NewWaypoint {
                    symbol: w.symbol.as_str(),
                    system_id,
                    type_: &w.waypoint_type,
                    x: w.x as i32,
                    y: w.y as i32,
                });
            }
        }
        self.db.insert_waypoints(&waypoint_inserts).await;

        // Repopulate the cache from the DB to avoid manual waypoint-id bookkeeping.
        let fresh = load_systems(&self.db).await;
        for (symbol, system) in fresh {
            self.systems.insert(symbol, system);
        }
    }

    // Fetch the waypoint list for every system with a jump gate, so each gate's
    // construction status (and market/shipyard traits) is known up front.
    // get_system_waypoints fetches + persists details when they're missing.
    async fn load_gate_waypoints(&self) {
        let gate_systems: Vec<SystemSymbol> = self
            .systems()
            .into_iter()
            .filter(|s| s.waypoints.iter().any(|w| w.waypoint_type == "JUMP_GATE"))
            .map(|s| s.symbol.clone())
            .collect();
        info!(
            "Loading waypoint details for {} jumpgate systems...",
            gate_systems.len()
        );
        for (i, sym) in gate_systems.iter().enumerate() {
            self.get_system_waypoints(sym).await;
            if (i + 1) % 250 == 0 {
                info!("  ...{}/{} jumpgate systems", i + 1, gate_systems.len());
            }
        }
    }

    pub fn connections_known(&self, waypoint: &WaypointSymbol) -> bool {
        self.jumpgates.contains_key(waypoint)
    }

    // Snapshot of charted jumpgate connections (gate waypoint -> connected gate
    // waypoints), straight from the in-memory cache. Cheap; used by the web map.
    pub fn charted_jumpgate_connections(&self) -> Vec<(WaypointSymbol, Vec<WaypointSymbol>)> {
        self.jumpgates
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().connections.clone()))
            .collect()
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
        self.db.insert_market_observation(&market).await;
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
                // Seed the progress log so freshly-deployed agents have a baseline
                // (the dedup in insert_construction_snapshot handles re-fetches).
                if let Some(c) = &site.data {
                    Self::record_construction_snapshot(&self.db, c, site.timestamp).await;
                }
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

    // Construction status from cache/DB only — never the API. Used on latency-sensitive
    // paths (the jumpgate graph build) that must not block on a network fetch: a
    // galaxy-wide per-gate API sweep there would hold the try_buy_ships lock past its
    // 30s fail-fast timeout. Returns None when the site is not yet cached/persisted;
    // the background construction load (spawn_construction_load) warms it off-lock.
    pub async fn construction_cached(
        &self,
        symbol: &WaypointSymbol,
    ) -> Option<Arc<WithTimestamp<Option<Construction>>>> {
        if let Some(construction) = self.constructions.get(symbol) {
            return Some(construction.clone());
        }
        let site = self.db.get_construction(symbol).await?;
        let construction = Arc::new(site);
        self.constructions
            .insert(symbol.clone(), construction.clone());
        Some(construction)
    }

    pub async fn update_construction(&self, construction: &Construction) {
        let symbol = &construction.symbol;
        let ts = chrono::Utc::now();
        let with_ts = WithTimestamp {
            data: Some(construction.clone()),
            timestamp: ts,
        };
        self.constructions
            .insert(symbol.clone(), Arc::new(with_ts.clone()));
        self.db.save_construction(symbol, &with_ts).await;
        Self::record_construction_snapshot(&self.db, construction, ts).await;
    }

    async fn record_construction_snapshot(
        db: &crate::database::DbClient,
        construction: &Construction,
        ts: chrono::DateTime<chrono::Utc>,
    ) {
        let materials: Vec<(String, i32, i32)> = construction
            .materials
            .iter()
            .map(|m| {
                (
                    m.trade_symbol.clone(),
                    m.fulfilled as i32,
                    m.required as i32,
                )
            })
            .collect();
        db.insert_construction_snapshot(ts, &construction.symbol, &materials)
            .await;
    }

    pub fn get_faction(&self, faction: &str) -> Faction {
        self.factions.get(faction).unwrap().clone()
    }

    pub fn get_factions(&self) -> Vec<Faction> {
        self.factions.iter().map(|x| x.value().clone()).collect()
    }

    // Load a system and warm its per-waypoint detail, market, and shipyard
    // caches. Consumers like FleetManager::generate_ship_config run under the
    // buy-lock and would otherwise do ~30 serial API round-trips on a cold
    // cache, stalling the lock past its timeout. Idempotent: the system load is
    // skipped when already cached, and each warm-up getter checks its own
    // cache/DB first (e.g. after a restart).
    pub async fn ensure_system_loaded(&self, symbol: &SystemSymbol) {
        let start = std::time::Instant::now();
        if !self.systems.contains_key(symbol) {
            self.load_system(symbol).await;
        }
        self.get_system_waypoints(symbol).await;
        self.get_system_markets_remote(symbol).await;
        self.get_system_shipyards_remote(symbol).await;
        info!(
            "Loaded + primed caches for {} in {:.1}s",
            symbol,
            start.elapsed().as_secs_f64()
        );
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
                system_id,
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

    // Force a re-fetch of a system's waypoint traits from the API and ingest them,
    // overwriting our cached details. Unlike get_system_waypoints (which trusts cached
    // details and only fetches when some are missing), this picks up traits that were
    // charted by other agents since we first loaded the system — e.g. markets that were
    // uncharted at startup. Returns the fresh waypoints.
    pub async fn refresh_system_waypoints(&self, symbol: &SystemSymbol) -> Vec<WaypointDetailed> {
        let waypoints = self.api_client.get_system_waypoints(symbol).await;
        self.ingest_scanned_waypoints(&waypoints).await;
        waypoints
    }

    // Learn a system's markets and shipyards even while their waypoints are still
    // UNCHARTED. The waypoints list hides a waypoint's real traits behind UNCHARTED
    // until some ship charts it, so a freshly-reached gate/T5 system has is_market()
    // == false everywhere and the logistics planner sees no markets to trade. A
    // trait-filtered query still matches on the underlying trait, so the server tells
    // us which waypoints are markets/shipyards; we OR those flags in (note_waypoint_traits)
    // so the planner has a market set to plan over. The system must already be loaded.
    // Returns (markets_discovered, shipyards_discovered).
    pub async fn discover_system_markets(&self, symbol: &SystemSymbol) -> (usize, usize) {
        let markets = self
            .api_client
            .get_system_waypoints_with_trait(symbol, "MARKETPLACE")
            .await;
        for w in &markets {
            self.note_waypoint_traits(&w.symbol, true, false).await;
        }
        let shipyards = self
            .api_client
            .get_system_waypoints_with_trait(symbol, "SHIPYARD")
            .await;
        for w in &shipyards {
            self.note_waypoint_traits(&w.symbol, false, true).await;
        }
        (markets.len(), shipyards.len())
    }

    // Whether we currently believe a waypoint is uncharted, from the in-memory cache.
    // Unknown waypoints (no details loaded) report false — we only chart on a positive.
    pub fn is_uncharted(&self, waypoint: &WaypointSymbol) -> bool {
        let Some(s) = self.systems.get(&waypoint.system()) else {
            return false;
        };
        s.value()
            .waypoints
            .iter()
            .find(|w| &w.symbol == waypoint)
            .and_then(|w| w.details.as_ref())
            .map(|d| d.is_uncharted)
            .unwrap_or(false)
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
                        traits,
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
                            .find(|w| w.symbol == waypoint.symbol)
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
                        .find(|w2| w2.symbol == w.symbol)
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

    // Ingest freshly-observed waypoint details (e.g. from a sensor-array scan), updating
    // the in-memory cache and DB. Unlike get_system_waypoints' first-load (do_nothing),
    // this OVERWRITES, so a scan that reveals a previously-uncharted market/shipyard is
    // actually learned (is_market/is_shipyard flip from their stale uncharted values).
    pub async fn ingest_scanned_waypoints(&self, waypoints: &[WaypointDetailed]) {
        let Some(first) = waypoints.first() else {
            return;
        };
        let system_symbol = first.system_symbol.clone();
        let system = self.system(&system_symbol);
        for w in waypoints {
            let Some(db_waypoint) = system.waypoints.iter().find(|x| x.symbol == w.symbol) else {
                continue;
            };
            let details = NewWaypointDetails {
                waypoint_id: db_waypoint.id,
                is_market: w.is_market(),
                is_shipyard: w.is_shipyard(),
                is_uncharted: w.is_uncharted(),
                is_under_construction: w.is_under_construction,
            };
            diesel::insert_into(waypoint_details::table)
                .values(&details)
                .on_conflict(waypoint_details::waypoint_id)
                .do_update()
                .set((
                    waypoint_details::is_market.eq(details.is_market),
                    waypoint_details::is_shipyard.eq(details.is_shipyard),
                    waypoint_details::is_uncharted.eq(details.is_uncharted),
                    waypoint_details::is_under_construction.eq(details.is_under_construction),
                ))
                .execute(&mut self.db.conn().await)
                .await
                .expect("DB Insert error");
        }
        // Refresh the in-memory cache so is_market()/is_shipyard() reflect the scan.
        let mut s = self.systems.get_mut(&system_symbol).unwrap();
        for wp in s.value_mut().waypoints.iter_mut() {
            if let Some(scanned) = waypoints.iter().find(|x| x.symbol == wp.symbol) {
                wp.details = Some(WaypointDetails {
                    is_market: scanned.is_market(),
                    is_shipyard: scanned.is_shipyard(),
                    is_uncharted: scanned.is_uncharted(),
                    is_under_construction: scanned.is_under_construction,
                });
            }
        }
    }

    // Record that a waypoint is a market and/or shipyard — e.g. after successfully
    // refreshing one there. The startup waypoint snapshot can predate a waypoint being
    // charted (faction capitals get charted by NPCs after our load) and never updates, so
    // search_shipyards / intel filters would otherwise never see it. OR-s the flags in;
    // no-op if already known.
    pub async fn note_waypoint_traits(
        &self,
        waypoint: &WaypointSymbol,
        is_market: bool,
        is_shipyard: bool,
    ) {
        let system_symbol = waypoint.system();
        let (wp_id, details) = {
            let Some(mut s) = self.systems.get_mut(&system_symbol) else {
                return;
            };
            let Some(wp) = s
                .value_mut()
                .waypoints
                .iter_mut()
                .find(|w| &w.symbol == waypoint)
            else {
                return;
            };
            let cur = wp.details.clone();
            let (m, y, unch, con) = match &cur {
                Some(d) => (
                    d.is_market,
                    d.is_shipyard,
                    d.is_uncharted,
                    d.is_under_construction,
                ),
                None => (false, false, false, false),
            };
            let new_market = m || is_market;
            let new_shipyard = y || is_shipyard;
            if cur.is_some() && new_market == m && new_shipyard == y {
                return; // already known — avoid a hot-path DB write
            }
            let details = WaypointDetails {
                is_market: new_market,
                is_shipyard: new_shipyard,
                is_uncharted: unch,
                is_under_construction: con,
            };
            wp.details = Some(details.clone());
            (wp.id, details)
        };
        diesel::insert_into(waypoint_details::table)
            .values(NewWaypointDetails {
                waypoint_id: wp_id,
                is_market: details.is_market,
                is_shipyard: details.is_shipyard,
                is_uncharted: details.is_uncharted,
                is_under_construction: details.is_under_construction,
            })
            .on_conflict(waypoint_details::waypoint_id)
            .do_update()
            .set((
                waypoint_details::is_market.eq(details.is_market),
                waypoint_details::is_shipyard.eq(details.is_shipyard),
            ))
            .execute(&mut self.db.conn().await)
            .await
            .expect("DB Insert error");
    }

    pub async fn get_system_markets(
        &self,
        symbol: &SystemSymbol,
    ) -> Vec<(MarketRemoteView, Option<Arc<WithTimestamp<Market>>>)> {
        let waypoints = self.get_system_waypoints(symbol).await;
        let mut markets = Vec::new();
        for waypoint in &waypoints {
            if waypoint.is_market() {
                // Skip markets we can't fetch yet (discovered but still uncharted, no
                // ship present) — they have no remote view to plan trades over. They're
                // handled separately as plain refresh-and-chart targets in the planner.
                let Some(market_remote) = self.get_market_remote(&waypoint.symbol).await else {
                    continue;
                };
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
                // Skip shipyards we can't fetch yet (discovered but uncharted).
                let Some(shipyard_remote) = self.get_shipyard_remote(&waypoint.symbol).await else {
                    continue;
                };
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
            if waypoint.is_market()
                && let Some(market_remote) = self.get_market_remote(&waypoint.symbol).await
            {
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
            if waypoint.is_shipyard()
                && let Some(shipyard_remote) = self.get_shipyard_remote(&waypoint.symbol).await
            {
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

    // None if the market isn't accessible yet — a discovered-but-uncharted market with
    // no ship of ours present. Once charted (or a ship arrives) it resolves to Some.
    pub async fn get_market_remote(&self, symbol: &WaypointSymbol) -> Option<MarketRemoteView> {
        // Layer 1 - check cache
        if let Some(market) = &self.remote_markets.get(symbol) {
            return Some(market.value().clone());
        }
        // A market we know is uncharted has no fetchable remote view — short-circuit so a
        // planning cycle doesn't re-issue a doomed 400 for every uncharted market it holds.
        // It resolves to Some once a ship charts it (is_uncharted flips false).
        if self.is_uncharted(symbol) {
            return None;
        }
        // Layer 2 - fetch from api and save
        let market = self.api_client.get_market_remote(symbol).await?;
        self.db.save_market_remote(symbol, &market).await;
        self.remote_markets.insert(symbol.clone(), market.clone());
        Some(market)
    }

    // None if the shipyard isn't accessible yet (uncharted, no ship present); see
    // get_market_remote.
    pub async fn get_shipyard_remote(&self, symbol: &WaypointSymbol) -> Option<ShipyardRemoteView> {
        // Layer 1 - check cache
        if let Some(shipyard) = &self.remote_shipyards.get(symbol) {
            return Some(shipyard.value().clone());
        }
        // Skip a doomed API call for a shipyard we know is uncharted; see get_market_remote.
        if self.is_uncharted(symbol) {
            return None;
        }
        // Layer 2 - fetch from api and save
        let shipyard = self.api_client.get_shipyard_remote(symbol).await?;
        self.db.save_shipyard_remote(symbol, &shipyard).await;
        self.remote_shipyards
            .insert(symbol.clone(), shipyard.clone());
        Some(shipyard)
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
            if let Some(shipyard) = self.get_shipyard(&waypoint.symbol)
                && let Some(ship) = shipyard
                    .data
                    .ships
                    .iter()
                    .find(|ship| ship.ship_type == ship_model)
            {
                shipyards.push((waypoint.symbol.clone(), ship.purchase_price));
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
                let Some(market) = self.get_market_remote(&waypoint.symbol).await else {
                    return false;
                };
                market.imports.iter().any(|import| import.symbol == *good)
            }
            WaypointFilter::Exports(good) => {
                if !waypoint.is_market() {
                    return false;
                }
                let Some(market) = self.get_market_remote(&waypoint.symbol).await else {
                    return false;
                };
                market.exports.iter().any(|export| export.symbol == *good)
            }
            WaypointFilter::Exchanges(good) => {
                if !waypoint.is_market() {
                    return false;
                }
                let Some(market) = self.get_market_remote(&waypoint.symbol).await else {
                    return false;
                };
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
        self.get_system_waypoints(symbol).await[0].symbol.clone()
    }

    // Get jumpgate connections for a charted system
    pub async fn get_jumpgate_connections(&self, symbol: &WaypointSymbol) -> JumpGateInfo {
        if let Some(jumpgate_info) = self.jumpgates.get(symbol) {
            // Trust the cache only once the gate is constructed; a gate cached while
            // still under construction may since have completed, so re-derive those.
            if jumpgate_info.is_constructed {
                return jumpgate_info.value().clone();
            }
        }

        // Otherwise fetch from API. Construction status comes from the construction
        // site (is_complete) rather than the waypoint flag, which can stay stale
        // after the gate finishes building.
        let construction = self.get_construction(symbol).await;
        let is_constructed = construction
            .data
            .as_ref()
            .map(|c| c.is_complete)
            .unwrap_or(true);
        let connections = self.api_client.get_jumpgate_conns(symbol).await;
        let info = JumpGateInfo {
            is_constructed,
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
        // Connections changed → the cached jumpgate graph is stale.
        self.jumpgate_graph.invalidate(&()).await;
        info
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
