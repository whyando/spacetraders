mod contract;
mod faction;
mod market;
mod ship;
mod system;
mod waypoint_symbol;

use chrono::{DateTime, Duration, Utc};
pub use contract::*;
pub use faction::*;
pub use market::*;
pub use ship::*;
pub use system::*;
use uuid::Uuid;
pub use waypoint_symbol::*;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub status: String,
    pub version: String,
    pub reset_date: String,
    pub stats: Stats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub agents: i64,
    pub ships: i64,
    pub systems: i64,
    pub waypoints: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    // account_id field is only present for own agent
    pub account_id: Option<String>,
    pub symbol: String,
    pub headquarters: WaypointSymbol,
    pub credits: i64,
    pub starting_faction: String,
    pub ship_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedList<T> {
    pub data: Vec<T>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub page: usize,
    pub limit: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Data<T> {
    pub data: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolNameDescr {
    pub symbol: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithTimestamp<T> {
    pub timestamp: DateTime<Utc>,
    pub data: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanLength {
    // Fixed plan size
    Fixed(Duration),
    // Ramping plan size, from min to max
    Ramping(Duration, Duration, f64), // min, max, ramp_factor
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerConfig {
    pub plan_length: PlanLength,
    pub max_compute_time: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogisticsScriptConfig {
    pub use_planner: bool,
    pub planner_config: Option<PlannerConfig>,
    pub allow_shipbuying: bool,
    pub allow_construction: bool,
    pub allow_market_refresh: bool,
    pub waypoint_allowlist: Option<Vec<WaypointSymbol>>,
    pub min_profit: i64,
}

#[derive(Debug, Clone)]
pub struct ProbeScriptConfig {
    pub waypoints: Vec<WaypointSymbol>,
    pub refresh_market: bool,
}

#[derive(Debug, Clone)]
pub enum ShipBehaviour {
    Probe(ProbeScriptConfig),
    Logistics(LogisticsScriptConfig),
    SiphonDrone,
    SiphonShuttle,
    MiningSurveyor,
    MiningDrone,
    MiningShuttle,
    ConstructionHauler,
    JumpgateProbe,
    Explorer,
    T5Trader,
}

#[derive(Debug, Clone)]
pub struct PurchaseCriteria {
    // this ship is never purchased
    pub never_purchase: bool,
    // require the ship to be bought from a specific system (None = starter system)
    pub system_symbol: Option<SystemSymbol>,
    // allow a logistic task to be created to go to a waypoint
    pub allow_logistic_task: bool,
    // require the ship to be bought from the cheapest shipyard in the system
    // (only relevant when we have multiple shipyards with the same ship
    //  and a purchaser at only a subset)
    pub require_cheapest: bool,
}

impl Default for PurchaseCriteria {
    fn default() -> Self {
        Self {
            never_purchase: false,
            system_symbol: None,
            allow_logistic_task: false,
            require_cheapest: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShipConfig {
    pub id: String,
    pub ship_model: String,
    pub purchase_criteria: PurchaseCriteria,
    pub behaviour: ShipBehaviour,
    // pub era: i64, // purchase/assignment priority
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Construction {
    pub symbol: WaypointSymbol,
    pub materials: Vec<ConstructionMaterial>,
    pub is_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstructionMaterial {
    pub trade_symbol: String,
    pub required: i64,
    pub fulfilled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Survey {
    pub signature: String,
    pub symbol: WaypointSymbol,
    pub deposits: Vec<Symbol>,
    pub expiration: DateTime<Utc>,
    pub size: String,
}

#[derive(Debug, Clone)]
pub struct KeyedSurvey {
    pub uuid: Uuid,
    pub survey: Survey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipConditionEvent {
    pub symbol: String,
    pub component: String,
    pub name: String,
    pub description: String,
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_deserialise_status() {
        let status_json = r#"{"status":"SpaceTraders is currently online and available to play","version":"v2.1.5","resetDate":"2024-01-28","description":"SpaceTraders is a headless API and fleet-management game where players can work together or against each other to trade, explore, expand, and conquer in a dynamic and growing universe. Build your own UI, write automated scripts, or just play the game from the comfort of your terminal. The game is currently in alpha and is under active development.","stats":{"agents":460,"ships":1951,"systems":8498,"waypoints":171701},"leaderboards":{"mostCredits":[{"agentSymbol":"100L-TRADER2","credits":21586291},{"agentSymbol":"CTRI-U-","credits":12881979},{"agentSymbol":"ALKIE","credits":12736024},{"agentSymbol":"OBO6","credits":10391516},{"agentSymbol":"0PTR","credits":10101310},{"agentSymbol":"THE_WITCHERS","credits":7129226},{"agentSymbol":"SAFPLUSPLUS","credits":6705175},{"agentSymbol":"PHANTASM","credits":4492560},{"agentSymbol":"A8RB4R","credits":2580406},{"agentSymbol":"CTRI-C-","credits":2548116},{"agentSymbol":"SIKAYN","credits":2257806},{"agentSymbol":"FLWI","credits":2201693},{"agentSymbol":"SG-1-DEVX3","credits":921819},{"agentSymbol":"A766D26F-1842-","credits":909342},{"agentSymbol":"AFDDD49A286E","credits":692812}],"mostSubmittedCharts":[{"agentSymbol":"CTRI-U-","chartCount":4}]},"serverResets":{"next":"2024-02-11T16:00:00.000Z","frequency":"fortnightly"},"announcements":[{"title":"Server Resets","body":"We will be doing complete server resets frequently during the alpha to deploy fixes, add new features, and balance the game. Resets will typically be conducted on Saturday mornings. Previous access tokens will no longer be valid after the reset and you will need to re-register your agent. Take this as an opportunity to try and make it to the top of the leaderboards!"},{"title":"Support Us","body":"Supporters of SpaceTraders can reserve their agent call sign between resets. Consider donating to support our development: https://donate.stripe.com/28o29m5vxcri6OccMM"},{"title":"Discord","body":"Our Discord community is very active and helpful. Share what you're working on, ask questions, and get help from other players and the developers: https://discord.com/invite/jh6zurdWk5"}],"links":[{"name":"Website","url":"https://spacetraders.io/"},{"name":"Documentation","url":"https://docs.spacetraders.io/"},{"name":"Playground","url":"https://docs.spacetraders.io/playground"},{"name":"API Reference","url":"https://spacetraders.stoplight.io/docs/spacetraders/"},{"name":"OpenAPI Spec - Bundled","url":"https://stoplight.io/api/v1/projects/spacetraders/spacetraders/nodes/reference/SpaceTraders.json?fromExportButton=true&snapshotType=http_service&deref=optimizedBundle"},{"name":"OpenAPI Spec - Source","url":"https://github.com/SpaceTradersAPI/api-docs/blob/main/reference/SpaceTraders.json"},{"name":"Discord","url":"https://discord.com/invite/jh6zurdWk5"},{"name":"Support Us","url":"https://donate.stripe.com/28o29m5vxcri6OccMM"},{"name":"Report Issues","url":"https://github.com/SpaceTradersAPI/api-docs/issues"},{"name":"Wiki","url":"https://github.com/SpaceTradersAPI/api-docs/wiki"},{"name":"Account Portal (Coming Soon)","url":"https://my.spacetraders.io/"},{"name":"Twitter","url":"https://twitter.com/SpaceTradersAPI"}]}"#;
        let status: Status = serde_json::from_str(status_json).unwrap();
        assert_eq!(
            status.status,
            "SpaceTraders is currently online and available to play"
        );
        assert_eq!(status.version, "v2.1.5");
        assert_eq!(status.reset_date, "2024-01-28");
    }

    #[test]
    fn test_deserialise_registration() {
        let registration_json = r#"{"data":{"token":"eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpZGVudGlmaWVyIjoiV0hZQU5ET19URVNUXzEiLCJ2ZXJzaW9uIjoidjIuMS41IiwicmVzZXRfZGF0ZSI6IjIwMjQtMDEtMjgiLCJpYXQiOjE3MDcwNDY2NDksInN1YiI6ImFnZW50LXRva2VuIn0.ez-1ifV4C3kazsfbLaSNkUDwjGRzE-pqpKD1aW_FML-3an1-vr6iBbFbbXARyV8UTBfpim9n3crWRbYR9-Or5HS0qkkO7RpmDSndUuCLpdPfFl8U4m1YcvSGzzJ6fbchiVvAtBk4BSpUvVfRz3JlL54-dUqFn-8mz8msl4eLNuzjxIQOkal3d931QoZ9Kqr2DeJO27i1c37ND38E_KCFIsV2kjFWXCZYnmNQqQ7_jfgT-5_zLa8awSorDQBd0mL-qzmShlSfPobzDaq-gZ9Ec7aYKEMChsTPevL4CTtG2HENIYikC9H5EYnOEmySf0hJ66-SV18Lh36hK6RF8zwtwQ","agent":{"accountId":"cls7fi0omrnrys60cqtfmv24h","symbol":"WHYANDO_TEST_1","headquarters":"X1-TZ26-A1","credits":250000,"startingFaction":"CORSAIRS","shipCount":0},"contract":{"id":"cls7fi0q2rns0s60cgvarxu6v","factionSymbol":"CORSAIRS","type":"PROCUREMENT","terms":{"deadline":"2024-02-11T11:37:29.626Z","payment":{"onAccepted":1391,"onFulfilled":10466},"deliver":[{"tradeSymbol":"ALUMINUM_ORE","destinationSymbol":"X1-TZ26-H51","unitsRequired":46,"unitsFulfilled":0}]},"accepted":false,"fulfilled":false,"expiration":"2024-02-05T11:37:29.626Z","deadlineToAccept":"2024-02-05T11:37:29.626Z"},"faction":{"symbol":"CORSAIRS","name":"Seventh Space Corsairs","description":"The Seventh Space Corsairs are a feared group of pirates and raiders who operate throughout the galaxy, preying on merchant ships and plundering valuable cargo.","headquarters":"X1-ZP15","traits":[{"symbol":"UNPREDICTABLE","name":"Unpredictable","description":"Difficult to predict or anticipate, with a tendency to act in unexpected or chaotic ways."},{"symbol":"BRUTAL","name":"Brutal","description":"Fierce and ruthless, with a willingness to use violence or intimidation to achieve their goals. Often feared or respected by others, but may also be viewed as a threat or enemy by those who oppose their methods."},{"symbol":"FLEETING","name":"Fleeting","description":"Not permanently settled in one place, with a tendency to move frequently or unpredictably. Sometimes difficult to find or track, but may also be able to take advantage of opportunities or evade threats by moving quickly or unexpectedly."},{"symbol":"ADAPTABLE","name":"Adaptable","description":"Quick to adapt to changing circumstances, with the ability to adjust their plans or strategies in response to new information or challenges. Sometimes able to thrive in a wide range of environments or situations, but may also be vulnerable to sudden or unexpected changes."}],"isRecruiting":true},"ship":{"symbol":"WHYANDO_TEST_1-1","nav":{"systemSymbol":"X1-TZ26","waypointSymbol":"X1-TZ26-A1","route":{"departure":{"symbol":"X1-TZ26-A1","type":"PLANET","systemSymbol":"X1-TZ26","x":23,"y":7},"origin":{"symbol":"X1-TZ26-A1","type":"PLANET","systemSymbol":"X1-TZ26","x":23,"y":7},"destination":{"symbol":"X1-TZ26-A1","type":"PLANET","systemSymbol":"X1-TZ26","x":23,"y":7},"arrival":"2024-02-04T11:37:29.703Z","departureTime":"2024-02-04T11:37:29.703Z"},"status":"DOCKED","flightMode":"CRUISE"},"crew":{"current":57,"capacity":80,"required":57,"rotation":"STRICT","morale":100,"wages":0},"fuel":{"current":400,"capacity":400,"consumed":{"amount":0,"timestamp":"2024-02-04T11:37:29.703Z"}},"cooldown":{"shipSymbol":"WHYANDO_TEST_1-1","totalSeconds":0,"remainingSeconds":0},"frame":{"symbol":"FRAME_FRIGATE","name":"Frigate","description":"A medium-sized, multi-purpose spacecraft, often used for combat, transport, or support operations.","moduleSlots":8,"mountingPoints":5,"fuelCapacity":400,"condition":100,"requirements":{"power":8,"crew":25}},"reactor":{"symbol":"REACTOR_FISSION_I","name":"Fission Reactor I","description":"A basic fission power reactor, used to generate electricity from nuclear fission reactions.","condition":100,"powerOutput":31,"requirements":{"crew":8}},"engine":{"symbol":"ENGINE_ION_DRIVE_II","name":"Ion Drive II","description":"An advanced propulsion system that uses ionized particles to generate high-speed, low-thrust acceleration, with improved efficiency and performance.","condition":100,"speed":30,"requirements":{"power":6,"crew":8}},"modules":[{"symbol":"MODULE_CARGO_HOLD_II","name":"Expanded Cargo Hold","description":"An expanded cargo hold module that provides more efficient storage space for a ship's cargo.","capacity":40,"requirements":{"crew":2,"power":2,"slots":2}},{"symbol":"MODULE_CREW_QUARTERS_I","name":"Crew Quarters","description":"A module that provides living space and amenities for the crew.","capacity":40,"requirements":{"crew":2,"power":1,"slots":1}},{"symbol":"MODULE_CREW_QUARTERS_I","name":"Crew Quarters","description":"A module that provides living space and amenities for the crew.","capacity":40,"requirements":{"crew":2,"power":1,"slots":1}},{"symbol":"MODULE_MINERAL_PROCESSOR_I","name":"Mineral Processor","description":"Crushes and processes extracted minerals and ores into their component parts, filters out impurities, and containerizes them into raw storage units.","requirements":{"crew":0,"power":1,"slots":2}},{"symbol":"MODULE_GAS_PROCESSOR_I","name":"Gas Processor","description":"Filters and processes extracted gases into their component parts, filters out impurities, and containerizes them into raw storage units.","requirements":{"crew":0,"power":1,"slots":2}}],"mounts":[{"symbol":"MOUNT_SENSOR_ARRAY_II","name":"Sensor Array II","description":"An advanced sensor array that improves a ship's ability to detect and track other objects in space with greater accuracy and range.","strength":4,"requirements":{"crew":2,"power":2}},{"symbol":"MOUNT_GAS_SIPHON_II","name":"Gas Siphon II","description":"An advanced gas siphon that can extract gas from gas giants and other gas-rich bodies more efficiently and at a higher rate.","strength":20,"requirements":{"crew":2,"power":2}},{"symbol":"MOUNT_MINING_LASER_II","name":"Mining Laser II","description":"An advanced mining laser that is more efficient and effective at extracting valuable minerals from asteroids and other space objects.","strength":5,"requirements":{"crew":2,"power":2}},{"symbol":"MOUNT_SURVEYOR_II","name":"Surveyor II","description":"An advanced survey probe that can be used to gather information about a mineral deposit with greater accuracy.","strength":2,"deposits":["QUARTZ_SAND","SILICON_CRYSTALS","PRECIOUS_STONES","ICE_WATER","AMMONIA_ICE","IRON_ORE","COPPER_ORE","SILVER_ORE","ALUMINUM_ORE","GOLD_ORE","PLATINUM_ORE","DIAMONDS","URANITE_ORE"],"requirements":{"crew":4,"power":3}}],"registration":{"name":"WHYANDO_TEST_1-1","factionSymbol":"CORSAIRS","role":"COMMAND"},"cargo":{"capacity":40,"units":0,"inventory":[]}}}}"#;

        let val: serde_json::Value = serde_json::from_str(registration_json).unwrap();
        let token: String = val["data"]["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9"));

        let agent: Agent = serde_json::from_value(val["data"]["agent"].clone()).unwrap();
        assert_eq!(
            agent.account_id,
            Some("cls7fi0omrnrys60cqtfmv24h".to_string())
        );

        let contract: contract::Contract =
            serde_json::from_value(val["data"]["contract"].clone()).unwrap();
        assert_eq!(contract.id, "cls7fi0q2rns0s60cgvarxu6v");

        let faction: faction::Faction =
            serde_json::from_value(val["data"]["faction"].clone()).unwrap();
        assert_eq!(faction.symbol, "CORSAIRS");

        let ship: ship::Ship = serde_json::from_value(val["data"]["ship"].clone()).unwrap();
        assert_eq!(ship.symbol, "WHYANDO_TEST_1-1");

        let ship_nav: ship::ShipNav = ship.nav;
        assert_eq!(ship_nav.system_symbol, SystemSymbol::new("X1-TZ26"));
    }

    #[test]
    fn test_construction_deserialize() {
        let json = r#"{"data":{"symbol":"X1-HS80-I58","materials":[{"tradeSymbol":"FAB_MATS","required":4000,"fulfilled":0},{"tradeSymbol":"ADVANCED_CIRCUITRY","required":1200,"fulfilled":0},{"tradeSymbol":"QUANTUM_STABILIZERS","required":1,"fulfilled":1}],"isComplete":false}}"#;
        let construction: Data<Construction> = serde_json::from_str(json).unwrap();
        assert_eq!(construction.data.materials.len(), 3);
    }

    #[test]
    fn test_constrution_serialize() {
        let construction_empty: WithTimestamp<Option<Construction>> = WithTimestamp {
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            data: None,
        };
        let construction_incomplete: WithTimestamp<Option<Construction>> = WithTimestamp {
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            data: Some(Construction {
                symbol: WaypointSymbol::new("X1-HS80-I58"),
                materials: vec![ConstructionMaterial {
                    trade_symbol: "FAB_MATS".into(),
                    required: 4000,
                    fulfilled: 0,
                }],
                is_complete: false,
            }),
        };
        let contruction_empty_json = serde_json::to_string(&construction_empty).unwrap();
        let construction_incomplete_json = serde_json::to_string(&construction_incomplete).unwrap();
        assert_eq!(
            contruction_empty_json,
            r#"{"timestamp":"1970-01-01T00:00:00Z","data":null}"#
        );
        assert_eq!(
            construction_incomplete_json,
            r#"{"timestamp":"1970-01-01T00:00:00Z","data":{"symbol":"X1-HS80-I58","materials":[{"tradeSymbol":"FAB_MATS","required":4000,"fulfilled":0}],"isComplete":false}}"#
        );
    }
}
