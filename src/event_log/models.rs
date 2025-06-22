//! Event log entities

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Ship entity
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ShipEntity {
    pub symbol: String,
    pub speed: i64,
    pub waypoint: String,
    pub is_docked: bool,
    pub fuel: i64,
    pub cargo: BTreeMap<String, i64>,
    pub nav_source: String,
    pub nav_arrival_time: i64,
    pub nav_departure_time: i64,
}

/// Event for updating a ship's state.
/// All fields are optional, and only the fields that are provided are to be updated.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ShipEntityUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_docked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuel: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<BTreeMap<String, i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_arrival_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nav_departure_time: Option<i64>,
}

impl std::fmt::Debug for ShipEntityUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fields = Vec::new();

        if let Some(symbol) = &self.symbol {
            fields.push(format!("symbol: {}", symbol));
        }
        if let Some(speed) = &self.speed {
            fields.push(format!("speed: {}", speed));
        }
        if let Some(waypoint) = &self.waypoint {
            fields.push(format!("waypoint: {}", waypoint));
        }
        if let Some(is_docked) = &self.is_docked {
            fields.push(format!("is_docked: {}", is_docked));
        }
        if let Some(fuel) = &self.fuel {
            fields.push(format!("fuel: {}", fuel));
        }
        if let Some(cargo) = &self.cargo {
            fields.push(format!("cargo: {:?}", cargo));
        }
        if let Some(nav_source) = &self.nav_source {
            fields.push(format!("nav_source: {}", nav_source));
        }
        if let Some(nav_arrival_time) = &self.nav_arrival_time {
            fields.push(format!("nav_arrival_time: {}", nav_arrival_time));
        }
        if let Some(nav_departure_time) = &self.nav_departure_time {
            fields.push(format!("nav_departure_time: {}", nav_departure_time));
        }

        write!(f, "ShipEntityUpdate {{ {} }}", fields.join(", "))
    }
}
