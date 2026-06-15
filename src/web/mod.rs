//! Lightweight read-only JSON API embedded in the agent.
//!
//! Backed by the live in-memory agent/fleet state plus the TimescaleDB KPI
//! history. Consumed cross-origin by the standalone dashboard SPA.

use crate::agent_controller::AgentController;
use crate::database::{DbClient, GoodProfit};
use crate::models::ShipNavStatus;
use axum::{Json, Router, extract::State, routing::get};
use log::*;
use serde::Serialize;
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
}

async fn api_history(State(s): State<AppState>) -> Json<Vec<HistoryPoint>> {
    let metrics = s.db.get_metrics_history(5000).await;
    let spend = s.db.ship_spend_events().await;
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
            }
        })
        .collect();
    Json(points)
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
