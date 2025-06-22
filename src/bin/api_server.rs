use st::scylla_client::ScyllaClient;
use warp::{Filter, Rejection, Reply};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use log::*;

#[derive(Debug, Serialize, Deserialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();
    info!("Starting API server...");

    let scylla_client = ScyllaClient::new().await;

    // GET /{log_id}/entity/{entity_id} - Get current state of an entity
    let get_entity_state = warp::path!(String / "entity" / String)
        .and(warp::get())
        .and(with_scylla_client(scylla_client.clone()))
        .and_then(get_entity_state_handler);

    // Health check endpoint
    let health = warp::path("health")
        .and(warp::get())
        .map(|| "OK");

    let routes = get_entity_state
        .or(health)
        .with(warp::cors().allow_any_origin())
        .with(warp::log("api_server"));

    info!("Starting API server...");
    warp::serve(routes)
        .run(([0, 0, 0, 0], 8080))
        .await;
}

fn with_scylla_client(
    client: ScyllaClient,
) -> impl Filter<Extract = (ScyllaClient,), Error = Infallible> + Clone {
    warp::any().map(move || client.clone())
}

async fn get_entity_state_handler(
    log_id: String,
    entity_id: String,
    scylla_client: ScyllaClient,
) -> Result<impl Reply, Rejection> {
    match scylla_client.get_entity(&log_id, &entity_id).await {
        Some(current_state) => {
            Ok(warp::reply::with_status(
                warp::reply::json(&current_state),
                warp::http::StatusCode::OK,
            ))
        }
        None => {
            Ok(warp::reply::with_status(
                warp::reply::json(&ErrorResponse {
                    error: "Entity not found".to_string(),
                }),
                warp::http::StatusCode::NOT_FOUND,
            ))
        }
    }
}
