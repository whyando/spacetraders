//! Lightweight read-only JSON API embedded in the agent.
//!
//! Backed by the live in-memory agent/fleet state plus the TimescaleDB KPI
//! history. Consumed cross-origin by the standalone dashboard SPA.

use crate::agent_controller::AgentController;
use crate::database::DbClient;
use crate::models::{MarketTradeGood, ShipNavStatus, WaypointSymbol};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use log::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    controller: AgentController,
    db: DbClient,
}

pub async fn serve(controller: AgentController, db: DbClient, port: u16) {
    let state = AppState { controller, db };
    // Public read-only API consumed cross-origin by the dashboard SPA (Cloudflare Pages).
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET]);
    let app = Router::new()
        .route("/api/agent", get(api_agent))
        .route("/api/ships", get(api_ships))
        .route("/api/history", get(api_history))
        .route("/api/construction", get(api_construction))
        .route("/api/universe", get(api_universe))
        .route("/api/systems", get(api_systems))
        .route("/api/systems/{system}/markets", get(api_system_markets))
        .route("/api/markets/{waypoint}", get(api_market))
        .layer(cors)
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            info!("Web dashboard listening on http://{}", addr);
            if let Err(e) = axum::serve(listener, app).await {
                error!("Web server stopped: {}", e);
            }
        }
        Err(e) => error!("Failed to bind web server on {}: {}", addr, e),
    }
}

#[derive(Serialize)]
struct AgentSummary {
    callsign: String,
    credits: i64,
    net_worth: i64,
    headquarters: String,
    starting_faction: String,
    num_ships: usize,
    // current lifecycle phase (e.g. StartingSystem1, InterSystem1)
    era: String,
}

async fn api_agent(State(s): State<AppState>) -> Json<AgentSummary> {
    let agent = s.controller.agent();
    let num_ships = s.controller.ships().len();
    let era = format!("{:?}", s.controller.state().era);
    // latest net worth from the KPI series, falling back to liquid credits
    let net_worth = s
        .db
        .get_metrics_history(1)
        .await
        .last()
        .map(|m| m.net_worth)
        .unwrap_or(agent.credits);
    Json(AgentSummary {
        callsign: agent.symbol,
        credits: agent.credits,
        net_worth,
        headquarters: agent.headquarters.to_string(),
        starting_faction: agent.starting_faction,
        num_ships,
        era,
    })
}

#[derive(Serialize)]
struct ShipView {
    symbol: String,
    role: String,
    status: String,
    frame: String,
    // Resolved shipyard purchase type (e.g. SHIP_LIGHT_HAULER). Falls back to
    // the frame symbol if the ship's config doesn't match a known model.
    ship_type: String,
    nav_status: ShipNavStatus,
    system: String,
    waypoint: String,
    // current nav route's destination + arrival (meaningful while IN_TRANSIT)
    destination: String,
    arrival: String,
    fuel_current: i64,
    fuel_capacity: i64,
    cargo_units: i64,
    cargo_capacity: i64,
}

async fn api_ships(State(s): State<AppState>) -> Json<Vec<ShipView>> {
    let mut ships: Vec<ShipView> = s
        .controller
        .ships()
        .into_iter()
        .map(|(symbol, ship, role, descr)| {
            let ship_type = ship.model().unwrap_or_else(|_| ship.frame.symbol.clone());
            ShipView {
                symbol,
                role,
                status: descr,
                frame: ship.frame.name,
                ship_type,
                nav_status: ship.nav.status,
                system: ship.nav.system_symbol.to_string(),
                waypoint: ship.nav.waypoint_symbol.to_string(),
                destination: ship.nav.route.destination.symbol.to_string(),
                arrival: ship.nav.route.arrival.to_rfc3339(),
                fuel_current: ship.fuel.current,
                fuel_capacity: ship.fuel.capacity,
                cargo_units: ship.cargo.units,
                cargo_capacity: ship.cargo.capacity,
            }
        })
        .collect();
    ships.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Json(ships)
}

#[derive(Serialize)]
struct HistoryPoint {
    ts: String,
    credits: i64,
    net_worth: i64,
    num_ships: i32,
    // cumulative credits spent on ships up to this point
    ship_spend: i64,
    // cumulative net credits spent on construction materials up to this point
    construction_spend: i64,
    // (net_worth - baseline) + cumulative construction spend, so jump-gate
    // donations don't appear as losses on the chart
    total_profit: i64,
}

async fn api_history(State(s): State<AppState>) -> Json<Vec<HistoryPoint>> {
    let metrics = s.db.get_metrics_history(5000).await;
    let ship_spend_events = s.db.ship_spend_events().await;
    let construction_events = s.db.construction_spend_events().await;
    let baseline = metrics.first().map(|m| m.net_worth).unwrap_or(0);
    // Walk all ascending series together, accumulating onto the metrics timeline.
    let mut ship_idx = 0;
    let mut cumulative_ship_spend = 0i64;
    let mut cons_idx = 0;
    let mut cumulative_construction_spend = 0i64;
    let points = metrics
        .into_iter()
        .map(|m| {
            while ship_idx < ship_spend_events.len() && ship_spend_events[ship_idx].0 <= m.ts {
                cumulative_ship_spend += ship_spend_events[ship_idx].1;
                ship_idx += 1;
            }
            while cons_idx < construction_events.len() && construction_events[cons_idx].0 <= m.ts {
                cumulative_construction_spend += construction_events[cons_idx].1;
                cons_idx += 1;
            }
            HistoryPoint {
                ts: m.ts.to_rfc3339(),
                credits: m.credits,
                net_worth: m.net_worth,
                num_ships: m.num_ships,
                ship_spend: cumulative_ship_spend,
                construction_spend: cumulative_construction_spend,
                total_profit: (m.net_worth - baseline) + cumulative_construction_spend,
            }
        })
        .collect();
    Json(points)
}

#[derive(Serialize)]
struct ConstructionPoint {
    ts: String,
    fulfilled: i32,
}

#[derive(Serialize)]
struct ConstructionMaterialView {
    trade_symbol: String,
    fulfilled: i32,
    required: i32,
    // cumulative net credits spent at markets purchasing this good
    spend: i64,
    history: Vec<ConstructionPoint>,
}

#[derive(Serialize)]
struct ConstructionSiteView {
    waypoint: String,
    is_complete: bool,
    total_spend: i64,
    materials: Vec<ConstructionMaterialView>,
}

async fn api_construction(State(s): State<AppState>) -> Json<Vec<ConstructionSiteView>> {
    // Only surface the jump-gate construction in our headquarters system.
    let hq_system = s.controller.agent().headquarters.system();
    let hq_gate = s.controller.ctx.universe.get_jumpgate_opt(&hq_system).await;
    let waypoints: Vec<String> = s
        .db
        .construction_waypoints()
        .await
        .into_iter()
        .filter(|wp| hq_gate.as_ref().map(|g| g.to_string()) == Some(wp.clone()))
        .collect();
    let mut sites = Vec::with_capacity(waypoints.len());
    let mut all_symbols: BTreeSet<String> = BTreeSet::new();

    // group history rows per (waypoint, material) and remember required (latest wins)
    struct Grouped {
        history: BTreeMap<String, Vec<ConstructionPoint>>,
        required: HashMap<String, i32>,
    }
    let mut grouped: HashMap<String, Grouped> = HashMap::new();
    for wp in &waypoints {
        let wp_symbol = match WaypointSymbol::parse(wp) {
            Ok(w) => w,
            Err(_) => {
                warn!("Skipping invalid waypoint in construction_log: {}", wp);
                continue;
            }
        };
        let rows = s.db.get_construction_history(&wp_symbol).await;
        let mut g = Grouped {
            history: BTreeMap::new(),
            required: HashMap::new(),
        };
        for (ts, symbol, fulfilled, required) in rows {
            all_symbols.insert(symbol.clone());
            g.history
                .entry(symbol.clone())
                .or_default()
                .push(ConstructionPoint {
                    ts: ts.to_rfc3339(),
                    fulfilled,
                });
            g.required.insert(symbol, required);
        }
        grouped.insert(wp.clone(), g);
    }

    // one-shot spend lookup across all materials ever seen
    let symbols: Vec<String> = all_symbols.into_iter().collect();
    let spend_rows = s.db.market_net_spend_by_good(&symbols).await;
    let spend_map: HashMap<String, i64> = spend_rows.into_iter().collect();

    // live status (is_complete + latest fulfilled) from the cache, falling back
    // to the log's tail if the universe hasn't loaded the site this run
    for wp in waypoints {
        let Some(g) = grouped.remove(&wp) else {
            continue;
        };
        let wp_symbol = WaypointSymbol::parse(&wp).unwrap();
        let live = s.controller.ctx.universe.get_construction(&wp_symbol).await;
        let is_complete = live.data.as_ref().map(|c| c.is_complete).unwrap_or(false);
        let live_materials: HashMap<String, i32> = live
            .data
            .as_ref()
            .map(|c| {
                c.materials
                    .iter()
                    .map(|m| (m.trade_symbol.clone(), m.fulfilled as i32))
                    .collect()
            })
            .unwrap_or_default();

        let mut materials: Vec<ConstructionMaterialView> = g
            .history
            .into_iter()
            .map(|(trade_symbol, history)| {
                let required = g.required.get(&trade_symbol).copied().unwrap_or(0);
                let fulfilled = live_materials
                    .get(&trade_symbol)
                    .copied()
                    .or_else(|| history.last().map(|p| p.fulfilled))
                    .unwrap_or(0);
                let spend = spend_map.get(&trade_symbol).copied().unwrap_or(0);
                ConstructionMaterialView {
                    trade_symbol,
                    fulfilled,
                    required,
                    spend,
                    history,
                }
            })
            .collect();
        materials.sort_by(|a, b| a.trade_symbol.cmp(&b.trade_symbol));
        let total_spend = materials.iter().map(|m| m.spend).sum();
        sites.push(ConstructionSiteView {
            waypoint: wp,
            is_complete,
            total_spend,
            materials,
        });
    }
    Json(sites)
}

#[derive(Serialize)]
struct UniverseSystemNode {
    symbol: String,
    x: i64,
    y: i64,
    #[serde(rename = "type")]
    type_: String,
    has_gate: bool,
    // gate connections are charted (we know where it links to)
    gate_charted: bool,
    // gate is still under construction (can't be jumped to)
    gate_under_construction: bool,
}

#[derive(Serialize)]
struct UniverseMap {
    systems: Vec<UniverseSystemNode>,
    // undirected charted gate-to-gate links, as pairs of system symbols
    edges: Vec<(String, String)>,
    num_systems: usize,
    num_gates: usize,
    num_charted: usize,
}

// Full galaxy map: every known system by coordinate, plus the charted jump-gate
// network, built from in-memory caches (no live API / graph rebuild).
async fn api_universe(State(s): State<AppState>) -> Json<UniverseMap> {
    let u = &s.controller.ctx.universe;
    let systems = u.systems();
    let mut nodes = Vec::with_capacity(systems.len());
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut num_gates = 0;
    let mut num_charted = 0;
    for sys in &systems {
        let gate = sys.waypoints.iter().find(|w| w.waypoint_type == "JUMP_GATE");
        let has_gate = gate.is_some();
        let gate_charted = gate.map(|g| u.connections_known(&g.symbol)).unwrap_or(false);
        let gate_under_construction = gate
            .and_then(|g| g.details.as_ref())
            .map(|d| d.is_under_construction)
            .unwrap_or(false);
        if has_gate {
            num_gates += 1;
        }
        if gate_charted {
            num_charted += 1;
        }
        known.insert(sys.symbol.to_string());
        nodes.push(UniverseSystemNode {
            symbol: sys.symbol.to_string(),
            x: sys.x,
            y: sys.y,
            type_: sys.system_type.clone(),
            has_gate,
            gate_charted,
            gate_under_construction,
        });
    }
    // Undirected, deduped edges between systems with known coordinates.
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut edges = Vec::new();
    for (src, conns) in u.charted_jumpgate_connections() {
        let a = src.system().to_string();
        for dst in conns {
            let b = dst.system().to_string();
            if a == b {
                continue;
            }
            let key = if a < b {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            };
            if !known.contains(&key.0) || !known.contains(&key.1) {
                continue;
            }
            if seen.insert(key.clone()) {
                edges.push(key);
            }
        }
    }
    Json(UniverseMap {
        num_systems: nodes.len(),
        num_gates,
        num_charted,
        systems: nodes,
        edges,
    })
}

#[derive(Serialize)]
struct SystemSummary {
    symbol: String,
    num_markets: usize,
    is_headquarters: bool,
}

// Systems that have at least one known market, HQ first then alphabetical.
async fn api_systems(State(s): State<AppState>) -> Json<Vec<SystemSummary>> {
    let hq = s.controller.agent().headquarters.system().to_string();
    let markets = s.db.get_all_markets().await;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (wp, _) in &markets {
        *counts.entry(wp.system().to_string()).or_default() += 1;
    }
    let mut out: Vec<SystemSummary> = counts
        .into_iter()
        .map(|(symbol, num_markets)| SystemSummary {
            is_headquarters: symbol == hq,
            symbol,
            num_markets,
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_headquarters
            .cmp(&a.is_headquarters)
            .then(a.symbol.cmp(&b.symbol))
    });
    Json(out)
}

#[derive(Serialize)]
struct SystemMarketView {
    waypoint: String,
    imports: Vec<String>,
    exports: Vec<String>,
    exchange: Vec<String>,
    num_goods: usize,
}

// Markets in a system, from the latest stored snapshot (no live API fetch).
async fn api_system_markets(
    State(s): State<AppState>,
    Path(system): Path<String>,
) -> Json<Vec<SystemMarketView>> {
    let mut out: Vec<SystemMarketView> = s
        .db
        .get_all_markets()
        .await
        .into_iter()
        .filter(|(wp, _)| wp.system().to_string() == system)
        .map(|(wp, m)| {
            let md = &m.data;
            let syms = |v: &[crate::models::SymbolNameDescr]| {
                v.iter().map(|x| x.symbol.clone()).collect::<Vec<_>>()
            };
            SystemMarketView {
                waypoint: wp.to_string(),
                imports: syms(&md.imports),
                exports: syms(&md.exports),
                exchange: syms(&md.exchange),
                num_goods: md.trade_goods.len(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.waypoint.cmp(&b.waypoint));
    Json(out)
}

#[derive(Serialize)]
struct MarketSnapshotPoint {
    ts: String,
    supply: String,
    activity: Option<String>,
    trade_volume: i32,
    purchase_price: i32,
    sell_price: i32,
}

#[derive(Serialize)]
struct MarketTxnPoint {
    ts: String,
    #[serde(rename = "type")]
    type_: String,
    units: i32,
    price_per_unit: i32,
    total_price: i32,
}

#[derive(Serialize)]
struct MarketGoodView {
    symbol: String,
    // current snapshot fields (None if the good only appears in history/transactions)
    #[serde(rename = "type")]
    type_: Option<String>,
    supply: Option<String>,
    activity: Option<String>,
    trade_volume: Option<i64>,
    purchase_price: Option<i64>,
    sell_price: Option<i64>,
    history: Vec<MarketSnapshotPoint>,
    transactions: Vec<MarketTxnPoint>,
}

#[derive(Serialize)]
struct MarketDetailView {
    waypoint: String,
    system: String,
    goods: Vec<MarketGoodView>,
}

// One market's current goods plus supply/price history and our own transactions,
// grouped per good for charting.
async fn api_market(
    State(s): State<AppState>,
    Path(waypoint): Path<String>,
) -> Json<MarketDetailView> {
    let wp = match WaypointSymbol::parse(&waypoint) {
        Ok(w) => w,
        Err(_) => {
            return Json(MarketDetailView {
                waypoint,
                system: String::new(),
                goods: vec![],
            });
        }
    };
    let system = wp.system().to_string();

    // Current snapshot: in-memory cache, falling back to the stored snapshot.
    let current_market = match s.controller.ctx.universe.get_market(&wp) {
        Some(m) => Some(m.data.clone()),
        None => s
            .db
            .get_all_markets()
            .await
            .into_iter()
            .find(|(w, _)| w == &wp)
            .map(|(_, m)| m.data),
    };
    let current_goods: HashMap<String, MarketTradeGood> = current_market
        .as_ref()
        .map(|m| {
            m.trade_goods
                .iter()
                .map(|g| (g.symbol.clone(), g.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut hist_map: BTreeMap<String, Vec<MarketSnapshotPoint>> = BTreeMap::new();
    for (ts, symbol, trade_volume, supply, activity, purchase_price, sell_price) in
        s.db.market_price_history(&wp).await
    {
        hist_map.entry(symbol).or_default().push(MarketSnapshotPoint {
            ts: ts.to_rfc3339(),
            supply,
            activity,
            trade_volume,
            purchase_price,
            sell_price,
        });
    }

    let mut txn_map: BTreeMap<String, Vec<MarketTxnPoint>> = BTreeMap::new();
    for (ts, symbol, type_, units, price_per_unit, total_price) in
        s.db.market_transactions(&wp).await
    {
        txn_map.entry(symbol).or_default().push(MarketTxnPoint {
            ts: ts.to_rfc3339(),
            type_,
            units,
            price_per_unit,
            total_price,
        });
    }

    // Union of goods seen in the snapshot, the history, and our transactions.
    let mut symbols: BTreeSet<String> = BTreeSet::new();
    symbols.extend(current_goods.keys().cloned());
    symbols.extend(hist_map.keys().cloned());
    symbols.extend(txn_map.keys().cloned());

    let goods = symbols
        .into_iter()
        .map(|symbol| {
            let g = current_goods.get(&symbol);
            let history = hist_map.remove(&symbol).unwrap_or_default();
            let transactions = txn_map.remove(&symbol).unwrap_or_default();
            MarketGoodView {
                type_: g.map(|g| g._type.to_string()),
                supply: g.map(|g| g.supply.to_string()),
                activity: g.and_then(|g| g.activity.as_ref().map(|a| a.to_string())),
                trade_volume: g.map(|g| g.trade_volume),
                purchase_price: g.map(|g| g.purchase_price),
                sell_price: g.map(|g| g.sell_price),
                history,
                transactions,
                symbol,
            }
        })
        .collect();

    Json(MarketDetailView {
        waypoint: wp.to_string(),
        system,
        goods,
    })
}
