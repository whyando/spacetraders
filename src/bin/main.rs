use log::*;
use reqwest::StatusCode;
use st::agent_controller::AgentController;
use st::api_client::ApiClient;
use st::api_client::kafka_interceptor::KafkaInterceptor;
use st::config::CONFIG;
use st::database::DbClient;
use st::models::Faction;
use st::universe::Universe;
use std::env;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    pretty_env_logger::init_timed();

    let faction = env::var("AGENT_FACTION").unwrap_or("".to_string());
    let callsign = env::var("AGENT_CALLSIGN")
        .expect("AGENT_CALLSIGN env var not set")
        .to_ascii_uppercase();

    info!("Starting agent {} for faction {}", callsign, faction);
    info!("Loaded config: {:?}", *CONFIG);

    let kafka_interceptor = Arc::new(KafkaInterceptor::new().await);
    let api_client = ApiClient::new(vec![kafka_interceptor.clone()]);

    let status = loop {
        let (status_code, status) = api_client.status().await;
        match status_code {
            StatusCode::OK => break status.unwrap(),
            StatusCode::SERVICE_UNAVAILABLE => {
                error!("Failed to get status: {}\nbody: {:?}", status_code, status);
                error!("Assumed maintenance mode, retrying in 1 second");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            _ => {
                error!("Failed to get status: {}\nbody: {:?}", status_code, status);
                panic!("Failed to get status");
            }
        }
    };

    info!("Reset date: {:?}", status.reset_date);
    // Generate a slice ID to be used as postgres schema name
    let slice_id = {
        let pg_schema = std::env::var("POSTGRES_SCHEMA").expect("POSTGRES_SCHEMA must be set");
        let slice_id = pg_schema.replace("{RESET_DATE}", &status.reset_date.replace("-", ""));
        slice_id
    };
    api_client.set_slice_id(&slice_id);

    // Use the reset date on the status response as a unique identifier to partition data between resets
    let db = DbClient::new(&slice_id).await;

    let universe = Arc::new(Universe::new(&api_client, &db).await);

    // Startup Phase: register if not already registered, and load agent token
    let agent_token = match db.get_agent_token(&callsign).await {
        Some(token) => token,
        None => {
            let faction = match faction.as_str() {
                "" => {
                    // Pick a random faction
                    let factions: Vec<Faction> = universe
                        .get_factions()
                        .into_iter()
                        .filter(|f| f.is_recruiting)
                        .collect();
                    use rand::prelude::IndexedRandom as _;
                    let faction = factions.choose(&mut rand::rng()).unwrap();
                    info!("Picked faction {}", faction.symbol);
                    faction.symbol.clone()
                }
                _ => faction.to_string(),
            };
            let token = api_client.register(&faction, &callsign).await;
            db.save_agent_token(&callsign, &token).await;
            token
        }
    };
    log::info!("Setting token {}", agent_token);
    api_client.set_agent_token(&agent_token);

    let agent_hdl = tokio::spawn(async move {
        let agent_controller = AgentController::new(&api_client, &db, &universe, &callsign).await;
        agent_controller.run().await;
    });
    let kafka_interceptor_hdl = tokio::spawn(async move {
        kafka_interceptor.join().await;
    });

    tokio::try_join!(kafka_interceptor_hdl, agent_hdl).unwrap();
}
