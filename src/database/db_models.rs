use chrono::{DateTime, Utc};
use diesel::{
    Identifiable, Insertable, Queryable, QueryableByName, Selectable, associations::Associations,
};
use serde_json::Value;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::schema::systems)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewSystem<'a> {
    pub symbol: &'a str,
    pub type_: &'a str,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::schema::waypoints)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewWaypoint<'a> {
    pub symbol: &'a str,
    pub system_id: i64,
    pub type_: &'a str,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::schema::waypoint_details)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewWaypointDetails {
    pub waypoint_id: i64,
    pub is_market: bool,
    pub is_shipyard: bool,
    pub is_uncharted: bool,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::schema::jumpgate_connections)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewJumpGateConnections<'a> {
    pub waypoint_symbol: &'a str,
    pub edges: Vec<&'a str>,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone, Identifiable, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = crate::schema::systems)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct System {
    pub id: i64,
    pub symbol: String,
    pub type_: String,
    pub x: i32,
    pub y: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Identifiable, Queryable, QueryableByName, Selectable, Associations)]
#[diesel(belongs_to(System))]
#[diesel(table_name = crate::schema::waypoints)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Waypoint {
    pub id: i64,
    pub symbol: String,
    pub system_id: i64,
    pub type_: String,
    pub x: i32,
    pub y: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Identifiable, Queryable, QueryableByName, Selectable, Associations)]
#[diesel(belongs_to(Waypoint))]
#[diesel(table_name = crate::schema::waypoint_details)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WaypointDetails {
    pub id: i64,
    pub waypoint_id: i64,
    pub is_market: bool,
    pub is_shipyard: bool,
    pub is_uncharted: bool,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = crate::schema::jumpgate_connections)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct JumpGateConnections {
    pub waypoint_symbol: String,
    pub is_under_construction: bool,
    pub edges: Vec<String>,
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = crate::schema::remote_markets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RemoteMarket {
    pub waypoint_symbol: String,
    pub market_data: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = crate::schema::remote_shipyards)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RemoteShipyard {
    pub waypoint_symbol: String,
    pub shipyard_data: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = crate::schema::markets)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Market {
    pub waypoint_symbol: String,
    pub market_data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::shipyards)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Shipyard {
    pub waypoint_symbol: String,
    pub shipyard_data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
