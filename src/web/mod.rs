//! Lightweight read-only web dashboard embedded in the agent.
//!
//! Serves a small JSON API backed by the live in-memory agent/fleet state plus
//! the TimescaleDB KPI history, and a self-contained HTML page that charts it.

use crate::agent_controller::AgentController;
use crate::database::DbClient;
use crate::models::ShipNavStatus;
use axum::{Json, Router, extract::State, response::Html, routing::get};
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
    // Public read-only API consumed cross-origin by the static dashboard (GitHub Pages).
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET]);
    let app = Router::new()
        .route("/", get(index))
        .route("/api/agent", get(api_agent))
        .route("/api/ships", get(api_ships))
        .route("/api/history", get(api_history))
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

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
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
        .map(|(_, _, nw)| *nw)
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
}

async fn api_history(State(s): State<AppState>) -> Json<Vec<HistoryPoint>> {
    let rows = s.db.get_metrics_history(5000).await;
    let points = rows
        .into_iter()
        .map(|(ts, credits, net_worth)| HistoryPoint {
            ts: ts.to_rfc3339(),
            credits,
            net_worth,
        })
        .collect();
    Json(points)
}
