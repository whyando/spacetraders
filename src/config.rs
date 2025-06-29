use lazy_static::lazy_static;
use regex::Regex;

use crate::agent_controller::AgentEra;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_base_url: String,
    pub job_id_filter: Regex,
    pub override_construction_supply_check: bool,
    pub scrap_all_ships: bool,
    pub scrap_unassigned: bool,
    pub no_gate_mode: bool,
    pub disable_trading_tasks: bool,
    pub disable_contract_tasks: bool,
    pub era_override: Option<AgentEra>,
}

lazy_static! {
    pub static ref CONFIG: Config = {
        let api_base_url = std::env::var("SPACETRADERS_API_URL")
            .expect("SPACETRADERS_API_URL env var not set")
            .parse()
            .expect("Invalid SPACETRADERS_API_URL");
        let job_id_filter = match std::env::var("JOB_ID_FILTER") {
            Ok(val) if val.is_empty() => None,
            Ok(val) => Some(val),
            Err(_) => None,
        };
        let job_id_filter = match job_id_filter {
            Some(val) => Regex::new(&val).expect("Invalid JOB_ID_FILTER regex"),
            None => Regex::new(".*").expect("Invalid default regex"),
        };
        let override_construction_supply_check =
            std::env::var("OVERRIDE_CONSTRUCTION_SUPPLY_CHECK")
                .map(|val| val == "1")
                .unwrap_or(false);
        let scrap_all_ships = std::env::var("SCRAP_ALL_SHIPS")
            .map(|val| val == "1")
            .unwrap_or(false);
        let scrap_unassigned = std::env::var("SCRAP_UNASSIGNED")
            .map(|val| val == "1")
            .unwrap_or(false);
        let no_gate_mode = std::env::var("NO_GATE_MODE")
            .map(|val| val == "1")
            .unwrap_or(false);
        let disable_trading_tasks = std::env::var("DEBUG_DISABLE_TRADING_TASKS")
            .map(|val| val == "1")
            .unwrap_or(false);
        let disable_contract_tasks = std::env::var("DEBUG_DISABLE_CONTRACT_TASKS")
            .map(|val| val == "1")
            .unwrap_or(false);
        let era_override = match std::env::var("ERA_OVERRIDE") {
            Ok(val) if val.is_empty() => None,
            Ok(val) => Some(val.parse().expect("Invalid ERA_OVERRIDE")),
            Err(_) => None,
        };
        Config {
            api_base_url,
            job_id_filter,
            override_construction_supply_check,
            scrap_all_ships,
            scrap_unassigned,
            era_override,
            no_gate_mode,
            disable_trading_tasks,
            disable_contract_tasks,
        }
    };
}

// Kafka config
lazy_static! {
    pub static ref KAFKA_TOPIC: &'static str = "api-requests";
    pub static ref KAFKA_CONFIG: rdkafka::ClientConfig = {
        let kafka_url = std::env::var("KAFKA_URL").expect("KAFKA_URL must be set");
        // let kafka_username = std::env::var("KAFKA_USERNAME").expect("KAFKA_USERNAME must be set");
        //let kafka_password = std::env::var("KAFKA_PASSWORD").expect("KAFKA_PASSWORD must be set");
        let mut config = rdkafka::ClientConfig::new();
        // config
        //     .set("bootstrap.servers", kafka_url)
        //     .set("security.protocol", "SASL_PLAINTEXT")
        //     .set("sasl.mechanism", "PLAIN")
        //     // jpa note: use PLAIN for now, seems like SCRAM is broken atm in the rdkafka crate (perhaps since kafka 4.0.0)
        //     // .set("sasl.mechanism", "SCRAM-SHA-256")
        //     .set("sasl.username", kafka_username)
        //     .set("sasl.password", kafka_password);

        // Disable SASL entirely
        config
            .set("bootstrap.servers", kafka_url)
            .set("security.protocol", "PLAINTEXT");
        config
    };
}

lazy_static! {}
