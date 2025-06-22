use super::{SystemSymbol, WaypointSymbol};
use crate::models::{self, Symbol, SymbolNameDescr};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

//
// System:
// Includes list of all waypoints but does NOT include waypoint traits
// GET https://api.spacetraders.io/v2/systems/{systemSymbol}
// GET https://api.spacetraders.io/v2/systems
//
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct System {
    pub symbol: SystemSymbol,
    pub sector_symbol: String,
    pub constellation: String,
    pub name: String,
    #[serde(rename = "type")]
    pub system_type: String,
    pub x: i64,
    pub y: i64,
    pub waypoints: Vec<WaypointSimplified>,
    pub factions: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaypointSimplified {
    pub symbol: WaypointSymbol,
    #[serde(rename = "type")]
    pub waypoint_type: String,
    pub x: i64,
    pub y: i64,
    pub orbitals: Vec<Symbol>,
    pub orbits: Option<String>,
}

//
// GET https://api.spacetraders.io/v2/systems/{systemSymbol}/waypoints
//
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaypointDetailed {
    pub symbol: WaypointSymbol,
    #[serde(rename = "type")]
    pub waypoint_type: String,
    pub system_symbol: SystemSymbol,
    pub x: i64,
    pub y: i64,
    pub orbitals: Vec<Symbol>,
    pub orbits: Option<String>,
    pub faction: Option<Symbol>,
    pub traits: Vec<SymbolNameDescr>,
    pub modifiers: Vec<SymbolNameDescr>,
    pub chart: Option<WaypointChart>,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaypointChart {
    pub waypoint_symbol: WaypointSymbol,
    pub submitted_by: String,
    pub submitted_on: DateTime<Utc>,
}

impl WaypointDetailed {
    pub fn is_uncharted(&self) -> bool {
        self.traits.iter().any(|t| t.symbol == "UNCHARTED")
    }
    pub fn is_market(&self) -> bool {
        self.traits.iter().any(|t| t.symbol == "MARKETPLACE")
    }
    pub fn is_shipyard(&self) -> bool {
        self.traits.iter().any(|t| t.symbol == "SHIPYARD")
    }
    pub fn is_jump_gate(&self) -> bool {
        self.waypoint_type == "JUMP_GATE"
    }
    pub fn is_gas_giant(&self) -> bool {
        self.waypoint_type == "GAS_GIANT"
    }
    pub fn is_engineered_asteroid(&self) -> bool {
        self.waypoint_type == "ENGINEERED_ASTEROID"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipPurchaseTransaction {
    pub waypoint_symbol: WaypointSymbol,
    pub ship_type: String,
    pub price: i64,
    pub agent_symbol: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrbitResponse {
    pub nav: models::ShipNav,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResponse {
    pub agent: models::Agent,
    pub cargo: models::ShipCargo,
    pub transaction: models::MarketTransaction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefuelResponse {
    pub agent: models::Agent,
    pub fuel: models::ShipFuel,
    pub transaction: models::MarketTransaction,
    pub cargo: Option<models::ShipCargo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigateResponse {
    pub nav: models::ShipNav,
    pub fuel: models::ShipFuel,
    pub events: Vec<models::ShipConditionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuyShipResponse {
    pub agent: models::Agent,
    pub ship: models::Ship,
    pub transaction: ShipPurchaseTransaction,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        api_client::WaypointSymbol,
        models::{Data, PaginatedList},
    };

    #[test]
    fn test_get_system() {
        // get system response
        let system_json = r#"{"data":{"symbol":"X1-RB8","sectorSymbol":"X1","type":"ORANGE_STAR","x":32975,"y":10980,"waypoints":[{"symbol":"X1-RB8-A1","type":"PLANET","x":-16,"y":20,"orbitals":[{"symbol":"X1-RB8-A2"},{"symbol":"X1-RB8-A3"},{"symbol":"X1-RB8-A4"}]},{"symbol":"X1-RB8-CX5A","type":"ENGINEERED_ASTEROID","x":-25,"y":8,"orbitals":[]},{"symbol":"X1-RB8-B6","type":"FUEL_STATION","x":63,"y":182,"orbitals":[]},{"symbol":"X1-RB8-B7","type":"ASTEROID_BASE","x":-172,"y":301,"orbitals":[]},{"symbol":"X1-RB8-B8","type":"ASTEROID","x":-34,"y":371,"orbitals":[]},{"symbol":"X1-RB8-B9","type":"ASTEROID","x":-179,"y":335,"orbitals":[]},{"symbol":"X1-RB8-B10","type":"ASTEROID","x":-339,"y":106,"orbitals":[]},{"symbol":"X1-RB8-B11","type":"ASTEROID","x":-215,"y":299,"orbitals":[]},{"symbol":"X1-RB8-B12","type":"ASTEROID","x":-381,"y":61,"orbitals":[]},{"symbol":"X1-RB8-B13","type":"ASTEROID","x":-332,"y":8,"orbitals":[]},{"symbol":"X1-RB8-B14","type":"ASTEROID","x":-339,"y":-64,"orbitals":[]},{"symbol":"X1-RB8-B15","type":"ASTEROID","x":-221,"y":-271,"orbitals":[]},{"symbol":"X1-RB8-B16","type":"ASTEROID","x":-319,"y":-108,"orbitals":[]},{"symbol":"X1-RB8-B17","type":"ASTEROID","x":-336,"y":-1,"orbitals":[]},{"symbol":"X1-RB8-B18","type":"ASTEROID","x":-300,"y":-101,"orbitals":[]},{"symbol":"X1-RB8-B19","type":"ASTEROID","x":-178,"y":-264,"orbitals":[]},{"symbol":"X1-RB8-B20","type":"ASTEROID","x":-216,"y":-304,"orbitals":[]},{"symbol":"X1-RB8-B21","type":"ASTEROID","x":-183,"y":-341,"orbitals":[]},{"symbol":"X1-RB8-B22","type":"ASTEROID","x":-359,"y":-134,"orbitals":[]},{"symbol":"X1-RB8-B23","type":"ASTEROID","x":-207,"y":-295,"orbitals":[]},{"symbol":"X1-RB8-B24","type":"ASTEROID","x":-47,"y":-384,"orbitals":[]},{"symbol":"X1-RB8-B25","type":"ASTEROID","x":155,"y":-338,"orbitals":[]},{"symbol":"X1-RB8-B26","type":"ASTEROID","x":-57,"y":-355,"orbitals":[]},{"symbol":"X1-RB8-B27","type":"ASTEROID","x":250,"y":-206,"orbitals":[]},{"symbol":"X1-RB8-B28","type":"ASTEROID","x":288,"y":-148,"orbitals":[]},{"symbol":"X1-RB8-B29","type":"ASTEROID","x":214,"y":-320,"orbitals":[]},{"symbol":"X1-RB8-B30","type":"ASTEROID","x":194,"y":-329,"orbitals":[]},{"symbol":"X1-RB8-B31","type":"ASTEROID","x":361,"y":69,"orbitals":[]},{"symbol":"X1-RB8-B32","type":"ASTEROID","x":327,"y":64,"orbitals":[]},{"symbol":"X1-RB8-B33","type":"ASTEROID","x":173,"y":275,"orbitals":[]},{"symbol":"X1-RB8-B34","type":"ASTEROID","x":147,"y":331,"orbitals":[]},{"symbol":"X1-RB8-B35","type":"ASTEROID","x":254,"y":238,"orbitals":[]},{"symbol":"X1-RB8-B36","type":"ASTEROID","x":283,"y":135,"orbitals":[]},{"symbol":"X1-RB8-B37","type":"ASTEROID","x":101,"y":346,"orbitals":[]},{"symbol":"X1-RB8-B38","type":"ASTEROID","x":62,"y":320,"orbitals":[]},{"symbol":"X1-RB8-B39","type":"ASTEROID","x":-70,"y":314,"orbitals":[]},{"symbol":"X1-RB8-B40","type":"ASTEROID","x":99,"y":329,"orbitals":[]},{"symbol":"X1-RB8-B41","type":"ASTEROID","x":-174,"y":279,"orbitals":[]},{"symbol":"X1-RB8-B42","type":"ASTEROID","x":-142,"y":334,"orbitals":[]},{"symbol":"X1-RB8-B43","type":"ASTEROID","x":-18,"y":382,"orbitals":[]},{"symbol":"X1-RB8-C44","type":"GAS_GIANT","x":-3,"y":-154,"orbitals":[{"symbol":"X1-RB8-C45"}]},{"symbol":"X1-RB8-C46","type":"FUEL_STATION","x":-2,"y":-114,"orbitals":[]},{"symbol":"X1-RB8-D47","type":"PLANET","x":-83,"y":-20,"orbitals":[{"symbol":"X1-RB8-D48"}]},{"symbol":"X1-RB8-E49","type":"PLANET","x":51,"y":20,"orbitals":[{"symbol":"X1-RB8-E50"}]},{"symbol":"X1-RB8-F51","type":"PLANET","x":-53,"y":55,"orbitals":[{"symbol":"X1-RB8-F52"}]},{"symbol":"X1-RB8-G53","type":"PLANET","x":-8,"y":-66,"orbitals":[{"symbol":"X1-RB8-G54"}]},{"symbol":"X1-RB8-H55","type":"PLANET","x":-45,"y":10,"orbitals":[{"symbol":"X1-RB8-H56"},{"symbol":"X1-RB8-H57"},{"symbol":"X1-RB8-H58"}]},{"symbol":"X1-RB8-I59","type":"JUMP_GATE","x":438,"y":107,"orbitals":[]},{"symbol":"X1-RB8-I60","type":"FUEL_STATION","x":224,"y":55,"orbitals":[]},{"symbol":"X1-RB8-J61","type":"FUEL_STATION","x":585,"y":143,"orbitals":[]},{"symbol":"X1-RB8-J62","type":"ASTEROID_BASE","x":699,"y":171,"orbitals":[]},{"symbol":"X1-RB8-J63","type":"ASTEROID","x":-696,"y":215,"orbitals":[]},{"symbol":"X1-RB8-J64","type":"ASTEROID","x":-508,"y":-598,"orbitals":[]},{"symbol":"X1-RB8-J65","type":"ASTEROID","x":-734,"y":-109,"orbitals":[]},{"symbol":"X1-RB8-J66","type":"ASTEROID","x":-405,"y":-637,"orbitals":[]},{"symbol":"X1-RB8-J67","type":"ASTEROID","x":-453,"y":-589,"orbitals":[]},{"symbol":"X1-RB8-J68","type":"ASTEROID","x":-219,"y":-723,"orbitals":[]},{"symbol":"X1-RB8-J69","type":"ASTEROID","x":82,"y":-772,"orbitals":[]},{"symbol":"X1-RB8-J70","type":"ASTEROID","x":181,"y":-763,"orbitals":[]},{"symbol":"X1-RB8-J71","type":"ASTEROID","x":468,"y":-538,"orbitals":[]},{"symbol":"X1-RB8-J72","type":"ASTEROID","x":647,"y":-431,"orbitals":[]},{"symbol":"X1-RB8-J73","type":"ASTEROID","x":497,"y":-545,"orbitals":[]},{"symbol":"X1-RB8-J74","type":"ASTEROID","x":712,"y":173,"orbitals":[]},{"symbol":"X1-RB8-J75","type":"ASTEROID","x":588,"y":470,"orbitals":[]},{"symbol":"X1-RB8-J76","type":"ASTEROID","x":695,"y":-271,"orbitals":[]},{"symbol":"X1-RB8-J77","type":"ASTEROID","x":559,"y":467,"orbitals":[]},{"symbol":"X1-RB8-J78","type":"ASTEROID","x":649,"y":392,"orbitals":[]},{"symbol":"X1-RB8-J79","type":"ASTEROID","x":435,"y":644,"orbitals":[]},{"symbol":"X1-RB8-J80","type":"ASTEROID","x":-13,"y":730,"orbitals":[]},{"symbol":"X1-RB8-J81","type":"ASTEROID","x":273,"y":681,"orbitals":[]},{"symbol":"X1-RB8-J82","type":"ASTEROID","x":-145,"y":768,"orbitals":[]},{"symbol":"X1-RB8-J83","type":"ASTEROID","x":-443,"y":601,"orbitals":[]},{"symbol":"X1-RB8-J84","type":"ASTEROID","x":-440,"y":640,"orbitals":[]},{"symbol":"X1-RB8-J85","type":"ASTEROID","x":-618,"y":481,"orbitals":[]},{"symbol":"X1-RB8-J86","type":"ASTEROID","x":-353,"y":628,"orbitals":[]},{"symbol":"X1-RB8-J87","type":"ASTEROID","x":-517,"y":519,"orbitals":[]},{"symbol":"X1-RB8-J88","type":"ASTEROID","x":-511,"y":544,"orbitals":[]},{"symbol":"X1-RB8-J89","type":"ASTEROID","x":-713,"y":170,"orbitals":[]},{"symbol":"X1-RB8-J90","type":"ASTEROID","x":-590,"y":517,"orbitals":[]},{"symbol":"X1-RB8-J91","type":"ASTEROID","x":-736,"y":-271,"orbitals":[]},{"symbol":"X1-RB8-K92","type":"PLANET","x":95,"y":47,"orbitals":[{"symbol":"X1-RB8-K93"}]},{"symbol":"X1-RB8-A2","type":"MOON","x":-16,"y":20,"orbitals":[],"orbits":"X1-RB8-A1"},{"symbol":"X1-RB8-A3","type":"MOON","x":-16,"y":20,"orbitals":[],"orbits":"X1-RB8-A1"},{"symbol":"X1-RB8-A4","type":"ORBITAL_STATION","x":-16,"y":20,"orbitals":[],"orbits":"X1-RB8-A1"},{"symbol":"X1-RB8-C45","type":"ORBITAL_STATION","x":-3,"y":-154,"orbitals":[],"orbits":"X1-RB8-C44"},{"symbol":"X1-RB8-D48","type":"MOON","x":-83,"y":-20,"orbitals":[],"orbits":"X1-RB8-D47"},{"symbol":"X1-RB8-E50","type":"MOON","x":51,"y":20,"orbitals":[],"orbits":"X1-RB8-E49"},{"symbol":"X1-RB8-F52","type":"ORBITAL_STATION","x":-53,"y":55,"orbitals":[],"orbits":"X1-RB8-F51"},{"symbol":"X1-RB8-G54","type":"MOON","x":-8,"y":-66,"orbitals":[],"orbits":"X1-RB8-G53"},{"symbol":"X1-RB8-H56","type":"MOON","x":-45,"y":10,"orbitals":[],"orbits":"X1-RB8-H55"},{"symbol":"X1-RB8-H57","type":"MOON","x":-45,"y":10,"orbitals":[],"orbits":"X1-RB8-H55"},{"symbol":"X1-RB8-H58","type":"MOON","x":-45,"y":10,"orbitals":[],"orbits":"X1-RB8-H55"},{"symbol":"X1-RB8-K93","type":"MOON","x":95,"y":47,"orbitals":[],"orbits":"X1-RB8-K92"}],"factions":[],"constellation":"Rohini","name":"Rohini VI"}}"#;
        let system: Data<System> = serde_json::from_str(system_json).unwrap();
        assert_eq!(system.data.symbol, SystemSymbol::new("X1-RB8"));
    }

    #[test]
    fn test_get_waypoints() {
        // get waypoints response
        let waypoint_json = r#"{"data":[{"symbol":"X1-RB8-A1","type":"PLANET","systemSymbol":"X1-RB8","x":-16,"y":20,"orbitals":[{"symbol":"X1-RB8-A2"},{"symbol":"X1-RB8-A3"},{"symbol":"X1-RB8-A4"}],"traits":[{"symbol":"FROZEN","name":"Frozen","description":"An ice-covered world with frigid temperatures, providing unique research opportunities and resources such as ice water, ammonia ice, and other frozen compounds."},{"symbol":"SCATTERED_SETTLEMENTS","name":"Scattered Settlements","description":"A collection of dispersed communities, each independent yet connected through trade and communication networks."},{"symbol":"EXPLOSIVE_GASES","name":"Explosive Gases","description":"A volatile environment filled with highly reactive gases, posing a constant risk to those who venture too close and offering opportunities for harvesting valuable materials such as hydrocarbons."},{"symbol":"FOSSILS","name":"Fossils","description":"A waypoint rich in the remains of ancient life, offering a valuable window into the past and the potential for scientific discovery."},{"symbol":"BREATHABLE_ATMOSPHERE","name":"Breathable Atmosphere","description":"A waypoint with a life-sustaining atmosphere, allowing for easy colonization and the flourishing of diverse ecosystems without the need for advanced life support systems."},{"symbol":"MAGMA_SEAS","name":"Magma Seas","description":"A waypoint dominated by molten rock and intense heat, creating inhospitable conditions and requiring specialized technology to navigate and harvest resources."},{"symbol":"MARKETPLACE","name":"Marketplace","description":"A thriving center of commerce where traders from across the galaxy gather to buy, sell, and exchange goods."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-A1","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-CX5A","type":"ENGINEERED_ASTEROID","systemSymbol":"X1-RB8","x":-25,"y":8,"orbitals":[],"traits":[{"symbol":"COMMON_METAL_DEPOSITS","name":"Common Metal Deposits","description":"A waypoint rich in common metal ores like iron, copper, and aluminum, essential for construction and manufacturing."},{"symbol":"STRIPPED","name":"Stripped","description":"A location that has been over-mined or over-harvested, resulting in depleted resources and barren landscapes."},{"symbol":"MARKETPLACE","name":"Marketplace","description":"A thriving center of commerce where traders from across the galaxy gather to buy, sell, and exchange goods."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-CX5A","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B6","type":"FUEL_STATION","systemSymbol":"X1-RB8","x":63,"y":182,"orbitals":[],"traits":[{"symbol":"MARKETPLACE","name":"Marketplace","description":"A thriving center of commerce where traders from across the galaxy gather to buy, sell, and exchange goods."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B6","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B7","type":"ASTEROID_BASE","systemSymbol":"X1-RB8","x":-172,"y":301,"orbitals":[],"traits":[{"symbol":"HOLLOWED_INTERIOR","name":"Hollowed Interior","description":"A location with large hollow spaces beneath its surface, providing unique opportunities for subterranean construction and resource extraction, but also posing risks of structural instability."},{"symbol":"OUTPOST","name":"Outpost","description":"A small, remote settlement providing essential services and a safe haven for travelers passing through."},{"symbol":"MARKETPLACE","name":"Marketplace","description":"A thriving center of commerce where traders from across the galaxy gather to buy, sell, and exchange goods."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B7","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B8","type":"ASTEROID","systemSymbol":"X1-RB8","x":-34,"y":371,"orbitals":[],"traits":[{"symbol":"COMMON_METAL_DEPOSITS","name":"Common Metal Deposits","description":"A waypoint rich in common metal ores like iron, copper, and aluminum, essential for construction and manufacturing."},{"symbol":"HOLLOWED_INTERIOR","name":"Hollowed Interior","description":"A location with large hollow spaces beneath its surface, providing unique opportunities for subterranean construction and resource extraction, but also posing risks of structural instability."},{"symbol":"DEEP_CRATERS","name":"Deep Craters","description":"Marked by deep, expansive craters, potentially formed by ancient meteor impacts. These formations may offer hidden resources but also pose challenges for mobility and construction."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B8","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B9","type":"ASTEROID","systemSymbol":"X1-RB8","x":-179,"y":335,"orbitals":[],"traits":[{"symbol":"MINERAL_DEPOSITS","name":"Mineral Deposits","description":"Abundant mineral resources, attracting mining operations and providing valuable materials such as silicon crystals and quartz sand for various industries."},{"symbol":"EXPLOSIVE_GASES","name":"Explosive Gases","description":"A volatile environment filled with highly reactive gases, posing a constant risk to those who venture too close and offering opportunities for harvesting valuable materials such as hydrocarbons."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B9","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B10","type":"ASTEROID","systemSymbol":"X1-RB8","x":-339,"y":106,"orbitals":[],"traits":[{"symbol":"COMMON_METAL_DEPOSITS","name":"Common Metal Deposits","description":"A waypoint rich in common metal ores like iron, copper, and aluminum, essential for construction and manufacturing."},{"symbol":"DEBRIS_CLUSTER","name":"Debris Cluster","description":"A region filled with hazardous debris and remnants of celestial bodies or man-made objects, requiring advanced navigational capabilities for ships passing through."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B10","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B11","type":"ASTEROID","systemSymbol":"X1-RB8","x":-215,"y":299,"orbitals":[],"traits":[{"symbol":"COMMON_METAL_DEPOSITS","name":"Common Metal Deposits","description":"A waypoint rich in common metal ores like iron, copper, and aluminum, essential for construction and manufacturing."},{"symbol":"RADIOACTIVE","name":"Radioactive","description":"A hazardous location with elevated levels of radiation, requiring specialized equipment and shielding for safe habitation and exploration."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B11","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B12","type":"ASTEROID","systemSymbol":"X1-RB8","x":-381,"y":61,"orbitals":[],"traits":[{"symbol":"MINERAL_DEPOSITS","name":"Mineral Deposits","description":"Abundant mineral resources, attracting mining operations and providing valuable materials such as silicon crystals and quartz sand for various industries."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B12","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}},{"symbol":"X1-RB8-B13","type":"ASTEROID","systemSymbol":"X1-RB8","x":-332,"y":8,"orbitals":[],"traits":[{"symbol":"COMMON_METAL_DEPOSITS","name":"Common Metal Deposits","description":"A waypoint rich in common metal ores like iron, copper, and aluminum, essential for construction and manufacturing."},{"symbol":"EXPLOSIVE_GASES","name":"Explosive Gases","description":"A volatile environment filled with highly reactive gases, posing a constant risk to those who venture too close and offering opportunities for harvesting valuable materials such as hydrocarbons."}],"isUnderConstruction":false,"faction":{"symbol":"UNITED"},"modifiers":[],"chart":{"waypointSymbol":"X1-RB8-B13","submittedBy":"UNITED","submittedOn":"2025-05-18T13:00:59.601Z"}}],"meta":{"total":93,"page":1,"limit":10}}"#;
        let waypoints: PaginatedList<WaypointDetailed> =
            serde_json::from_str(waypoint_json).unwrap();
        assert_eq!(waypoints.data[0].symbol, WaypointSymbol::new("X1-RB8-A1"));
        assert_eq!(waypoints.data.len(), 10);
    }
}
