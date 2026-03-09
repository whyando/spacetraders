use crate::models::{SystemSymbol, WaypointSymbol};

#[derive(Debug, Clone)]
pub struct Waypoint {
    pub id: i64,
    pub symbol: WaypointSymbol,
    pub waypoint_type: String,
    pub x: i64,
    pub y: i64,
    pub details: Option<WaypointDetails>,
}

#[derive(Debug, Clone)]
pub struct WaypointDetails {
    pub is_market: bool,
    pub is_shipyard: bool,
    pub is_uncharted: bool,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone)]
pub struct System {
    pub symbol: SystemSymbol,
    pub system_type: String,
    pub x: i64,
    pub y: i64,
    pub waypoints: Vec<Waypoint>,
}

impl System {
    pub fn is_starter_system(&self) -> bool {
        self.waypoints
            .iter()
            .any(|w| w.waypoint_type == "ENGINEERED_ASTEROID")
    }
}

impl Waypoint {
    pub fn is_market(&self) -> bool {
        if let Some(details) = &self.details
            && details.is_market {
                return true;
            }
        // All jumpgates are also markets
        if self.waypoint_type == "JUMP_GATE" {
            return true;
        }
        false
    }
}
