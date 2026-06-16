//! Lightweight read-only JSON API embedded in the agent.
//!
//! Backed by the live in-memory agent/fleet state plus the TimescaleDB KPI
//! history. Consumed cross-origin by the standalone dashboard SPA.

use crate::agent_controller::AgentController;
use crate::database::{DbClient, GoodProfit};
use crate::models::{ShipNavStatus, WaypointSymbol};
use axum::{Json, Router, extract::State, routing::get};
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
        .route("/api/profit", get(api_profit))
        .route("/api/construction", get(api_construction))
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
}

async fn api_agent(State(s): State<AppState>) -> Json<AgentSummary> {
    let agent = s.controller.agent();
    let num_ships = s.controller.ships().len();
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
    })
}

#[derive(Serialize)]
struct ShipView {
    symbol: String,
    role: String,
    status: String,
    frame: String,
    nav_status: ShipNavStatus,
    system: String,
    waypoint: String,
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
        .map(|(symbol, ship, role, descr)| ShipView {
            symbol,
            role,
            status: descr,
            frame: ship.frame.name,
            nav_status: ship.nav.status,
            system: ship.nav.system_symbol.to_string(),
            waypoint: ship.nav.waypoint_symbol.to_string(),
            fuel_current: ship.fuel.current,
            fuel_capacity: ship.fuel.capacity,
            cargo_units: ship.cargo.units,
            cargo_capacity: ship.cargo.capacity,
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
    // cumulative all-time profit (net_worth at this point minus the first observed net_worth)
    total_profit: i64,
}

async fn api_history(State(s): State<AppState>) -> Json<Vec<HistoryPoint>> {
    let metrics = s.db.get_metrics_history(5000).await;
    let spend = s.db.ship_spend_events().await;
    let baseline = metrics.first().map(|m| m.net_worth).unwrap_or(0);
    // Walk both ascending series together, accumulating ship spend onto the metrics timeline.
    let mut spend_idx = 0;
    let mut cumulative_spend = 0i64;
    let points = metrics
        .into_iter()
        .map(|m| {
            while spend_idx < spend.len() && spend[spend_idx].0 <= m.ts {
                cumulative_spend += spend[spend_idx].1;
                spend_idx += 1;
            }
            HistoryPoint {
                ts: m.ts.to_rfc3339(),
                credits: m.credits,
                net_worth: m.net_worth,
                num_ships: m.num_ships,
                ship_spend: cumulative_spend,
                total_profit: m.net_worth - baseline,
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
    let waypoints = s.db.construction_waypoints().await;
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
struct ProfitResponse {
    // sum of per-good profit (realised market trading profit)
    total: i64,
    by_good: Vec<GoodProfit>,
}

async fn api_profit(State(s): State<AppState>) -> Json<ProfitResponse> {
    let by_good = s.db.profit_by_good().await;
    let total = by_good.iter().map(|g| g.profit).sum();
    Json(ProfitResponse { total, by_good })
}
