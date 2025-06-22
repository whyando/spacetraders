use log::*;
use serde::{Deserialize, Serialize};
use st::scylla_client::ScyllaClient;
use std::convert::Infallible;
use warp::{Filter, Rejection, Reply};

#[derive(Debug, Serialize, Deserialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    seq_num: i64,
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    seq_num: i64,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();
    info!("Starting API server...");

    let scylla_client = ScyllaClient::new().await;

    // GET /log/{log_id} - Get event log information
    let get_log = warp::path!("log" / String)
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_log_handler);

    // GET /log/{log_id}/events - Get all events in a log
    let get_events = warp::path!("log" / String / "events")
        .and(warp::get())
        .and(warp::query::<EventsQuery>())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_events_handler);

    // GET /log/{log_id}/entity/{entity_id} - Get current state of an entity
    let get_entity_state = warp::path!("log" / String / "entity" / String)
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_state_handler);

    // GET /log/{log_id}/entity/{entity_id}/current - Get current state of an entity
    let get_entity_current = warp::path!("log" / String / "entity" / String / "current")
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_state_handler);

    // GET /log/{log_id}/entity/{entity_id}/snapshot - Get snapshot at or before target seq_num
    let get_snapshot = warp::path!("log" / String / "entity" / String / "snapshot")
        .and(warp::get())
        .and(warp::query::<SnapshotQuery>())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_snapshot_handler);

    // GET /log/{log_id}/entity/{entity_id}/events - Get events for a specific entity
    let get_entity_events = warp::path!("log" / String / "entity" / String / "events")
        .and(warp::get())
        .and(warp::query::<EventsQuery>())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_events_handler);

    // Health check endpoint
    let health = warp::path("health").and(warp::get()).map(|| "OK");

    // Main API routes
    let api_routes = get_log
        .or(get_events)
        .or(get_entity_events)
        .or(get_entity_state)
        .or(get_entity_current)
        .or(get_snapshot)
        .with(warp::cors().allow_any_origin())
        .with(warp::log("api_server"));

    // Add health separately to avoid logging
    let routes = health.or(api_routes);

    info!("Starting API server...");
    warp::serve(routes).run(([0, 0, 0, 0], 8080)).await;
}

fn with_scylla_client(
    client: ScyllaClient,
) -> impl Filter<Extract = (ScyllaClient,), Error = Infallible> + Clone {
    warp::any().map(move || client.clone())
}

async fn get_log_handler(
    log_id: String,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client.get_event_log(&log_id).await {
        Some(event_log) => Ok(warp::reply::with_status(
            warp::reply::json(&event_log),
            warp::http::StatusCode::OK,
        )),
        None => Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Not found".to_string(),
            }),
            warp::http::StatusCode::NOT_FOUND,
        )),
    }
}

async fn get_events_handler(
    log_id: String,
    query: EventsQuery,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client
        .get_events(&log_id, Some(query.seq_num), 200)
        .await
    {
        Ok(events) => Ok(warp::reply::with_status(
            warp::reply::json(&events),
            warp::http::StatusCode::OK,
        )),
        Err(_) => Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Failed to retrieve events".to_string(),
            }),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn get_entity_events_handler(
    log_id: String,
    entity_id: String,
    query: EventsQuery,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client
        .get_events_by_entity(&log_id, &entity_id, Some(query.seq_num), 200)
        .await
    {
        Ok(events) => Ok(warp::reply::with_status(
            warp::reply::json(&events),
            warp::http::StatusCode::OK,
        )),
        Err(_) => Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Failed to retrieve events".to_string(),
            }),
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

async fn get_entity_state_handler(
    log_id: String,
    entity_id: String,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client.get_entity(&log_id, &entity_id).await {
        Some(current_state) => Ok(warp::reply::with_status(
            warp::reply::json(&current_state),
            warp::http::StatusCode::OK,
        )),
        None => Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "Not found".to_string(),
            }),
            warp::http::StatusCode::NOT_FOUND,
        )),
    }
}

async fn get_snapshot_handler(
    log_id: String,
    entity_id: String,
    query: SnapshotQuery,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client
        .get_snapshot_at_or_before(&log_id, &entity_id, query.seq_num)
        .await
    {
        Some(snapshot) => Ok(warp::reply::with_status(
            warp::reply::json(&snapshot),
            warp::http::StatusCode::OK,
        )),
        None => Ok(warp::reply::with_status(
            warp::reply::json(&ErrorResponse {
                error: "No snapshot found".to_string(),
            }),
            warp::http::StatusCode::NOT_FOUND,
        )),
    }
}
