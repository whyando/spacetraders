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
    from_seq_num: Option<i64>,
    limit: Option<i32>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();
    info!("Starting API server...");

    let scylla_client = ScyllaClient::new().await;

    // GET /{log_id} - Get event log information
    let get_log = warp::path!(String)
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_log_handler);

    // GET /{log_id}/events - Get all events in a log
    let get_events = warp::path!(String / "events")
        .and(warp::get())
        .and(warp::query::<EventsQuery>())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_events_handler);

    // GET /{log_id}/events/{entity_id} - Get events for a specific entity
    let get_entity_events = warp::path!(String / "events" / String)
        .and(warp::get())
        .and(warp::query::<EventsQuery>())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_events_handler);

    // GET /{log_id}/entity/{entity_id} - Get current state of an entity
    let get_entity_state = warp::path!(String / "entity" / String)
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_state_handler);

    // Health check endpoint
    let health = warp::path("health").and(warp::get()).map(|| "OK");

    let routes = get_log
        .or(get_events)
        .or(get_entity_events)
        .or(get_entity_state)
        .or(health)
        .with(warp::cors().allow_any_origin())
        .with(warp::log("api_server"));

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
        .get_events(&log_id, query.from_seq_num, query.limit)
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
        .get_events_by_entity(&log_id, &entity_id, query.from_seq_num, query.limit)
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
