pub mod db_models;

use crate::models::Construction;
use crate::models::KeyedSurvey;
use crate::schema::*;
use crate::tasks::TaskManagerState;
use crate::{
    logistics_planner::ShipSchedule,
    models::{
        Market, MarketRemoteView, Shipyard, ShipyardRemoteView, SystemSymbol, WaypointSymbol,
        WithTimestamp,
    },
};
use chrono::Utc;
use dashmap::DashMap;
use diesel::ExpressionMethods as _;
use diesel::OptionalExtension as _;
use diesel::QueryDsl as _;
use diesel::QueryableByName;
use diesel::SelectableHelper as _;
use diesel::sql_types::{Integer, Text};
use diesel::upsert::excluded;
use diesel_async::AsyncPgConnection;
use diesel_async::RunQueryDsl as _;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Object;
use diesel_async::pooled_connection::deadpool::Pool;
use log::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
pub struct DbClient {
    db: Pool<AsyncPgConnection>,
}

impl DbClient {
    pub async fn new(slice_id: &str) -> DbClient {
        let database_url = std::env::var("POSTGRES_URI").expect("POSTGRES_URI must be set");
        info!("Using schema: {}", slice_id);
        let db = {
            let database_url = format!("{}?options=-c%20search_path%3D{}", database_url, slice_id);
            let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
            Pool::builder(manager).max_size(5).build().unwrap()
        };
        // Check the connection
        {
            let mut conn = db.get().await.unwrap();
            #[derive(QueryableByName)]
            struct Ret {
                #[diesel(sql_type = Integer)]
                value: i32,
            }
            let result: Vec<Ret> = diesel::sql_query("SELECT 1 as value")
                .load(&mut conn)
                .await
                .unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].value, 1);

            #[derive(QueryableByName)]
            struct SearchPathRet {
                #[diesel(sql_type = Text)]
                search_path: String,
            }

            let result: Vec<SearchPathRet> = diesel::sql_query("SHOW search_path")
                .load(&mut conn)
                .await
                .unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].search_path, slice_id);
            info!("Successfully connected to database");
        }
        let db = DbClient { db };
        db.create_schema(&slice_id).await;
        db
    }

    async fn create_schema(&self, schema_name: &str) {
        let sql = include_str!("../../spacetraders_schema.sql.template")
            .replace("___SCHEMA___", schema_name);

        let mut conn = self.conn().await;
        use diesel_async::SimpleAsyncConnection as _;
        conn.batch_execute(&sql).await.unwrap();
    }

    pub async fn conn(&self) -> Object<AsyncPgConnection> {
        self.db
            .get()
            .await
            .expect("Timed out waiting for a database connection")
    }

    pub async fn get_value<T>(&self, key: &str) -> Option<T>
    where
        T: Sized + DeserializeOwned,
    {
        debug!("db get: {}", key);
        let value_opt: Option<Value> = generic_lookup::table
            .select(generic_lookup::value)
            .filter(generic_lookup::key.eq(key))
            .first(&mut self.conn().await)
            .await
            .optional()
            .expect("DB Query error");
        value_opt.map(|data| serde_json::from_value(data).unwrap())
    }

    pub async fn set_value<T>(&self, key: &str, value: &T)
    where
        T: Serialize + ?Sized,
    {
        debug!("db set: {}", key);
        let value: Value = serde_json::to_value(value).unwrap();
        diesel::insert_into(generic_lookup::table)
            .values((
                generic_lookup::key.eq(key),
                generic_lookup::value.eq(&value),
            ))
            .on_conflict(generic_lookup::key)
            .do_update()
            .set(generic_lookup::value.eq(&value))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    pub async fn get_agent_token(&self, callsign: &str) -> Option<String> {
        self.get_value(&format!("registrations/{}", callsign)).await
    }

    pub async fn save_agent_token(&self, callsign: &str, token: &str) {
        self.set_value(&format!("registrations/{}", callsign), token)
            .await
    }

    pub async fn get_market_remote(&self, symbol: &WaypointSymbol) -> Option<MarketRemoteView> {
        let market: Option<db_models::RemoteMarket> = remote_markets::table
            .filter(remote_markets::waypoint_symbol.eq(symbol.to_string()))
            .select(db_models::RemoteMarket::as_select())
            .first(&mut self.conn().await)
            .await
            .optional()
            .expect("DB Query error");
        market.map(|m| serde_json::from_value(m.market_data).expect("Invalid market data"))
    }

    pub async fn save_market_remote(&self, symbol: &WaypointSymbol, market: &MarketRemoteView) {
        let market_data = serde_json::to_value(market).expect("Failed to serialize market");
        diesel::insert_into(remote_markets::table)
            .values((
                remote_markets::waypoint_symbol.eq(symbol.to_string()),
                remote_markets::market_data.eq(market_data),
            ))
            .on_conflict(remote_markets::waypoint_symbol)
            .do_update()
            .set((
                remote_markets::market_data.eq(excluded(remote_markets::market_data)),
                remote_markets::updated_at.eq(chrono::Utc::now()),
            ))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Insert error");
    }

    pub async fn get_shipyard_remote(&self, symbol: &WaypointSymbol) -> Option<ShipyardRemoteView> {
        let shipyard: Option<db_models::RemoteShipyard> = remote_shipyards::table
            .filter(remote_shipyards::waypoint_symbol.eq(symbol.to_string()))
            .select(db_models::RemoteShipyard::as_select())
            .first(&mut self.conn().await)
            .await
            .optional()
            .expect("DB Query error");
        shipyard.map(|s| serde_json::from_value(s.shipyard_data).expect("Invalid shipyard data"))
    }

    pub async fn save_shipyard_remote(
        &self,
        symbol: &WaypointSymbol,
        shipyard: &ShipyardRemoteView,
    ) {
        let shipyard_data = serde_json::to_value(shipyard).expect("Failed to serialize shipyard");
        diesel::insert_into(remote_shipyards::table)
            .values((
                remote_shipyards::waypoint_symbol.eq(symbol.to_string()),
                remote_shipyards::shipyard_data.eq(shipyard_data),
            ))
            .on_conflict(remote_shipyards::waypoint_symbol)
            .do_update()
            .set((
                remote_shipyards::shipyard_data.eq(excluded(remote_shipyards::shipyard_data)),
                remote_shipyards::updated_at.eq(chrono::Utc::now()),
            ))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Insert error");
    }

    // Shouldn't be needed, since we shouldn't be loading individual markets
    // pub async fn get_market(&self, symbol: &WaypointSymbol) -> Option<WithTimestamp<Market>> {
    //     let market: Option<db_models::Market> = markets::table
    //         .filter(markets::waypoint_symbol.eq(symbol.to_string()))
    //         .select(db_models::Market::as_select())
    //         .first(&mut self.conn().await)
    //         .await
    //         .optional()
    //         .expect("DB Query error");

    //     market.map(|m| {
    //         let market_data: Market =
    //             serde_json::from_value(m.market_data).expect("Invalid market data");
    //         WithTimestamp {
    //             data: market_data,
    //             timestamp: m.updated_at,
    //         }
    //     })
    // }

    pub async fn save_market(&self, symbol: &WaypointSymbol, market: &WithTimestamp<Market>) {
        let market_data = serde_json::to_value(&market.data).expect("Failed to serialize market");
        diesel::insert_into(markets::table)
            .values((
                markets::waypoint_symbol.eq(symbol.to_string()),
                markets::market_data.eq(market_data),
            ))
            .on_conflict(markets::waypoint_symbol)
            .do_update()
            .set((
                markets::market_data.eq(excluded(markets::market_data)),
                markets::updated_at.eq(chrono::Utc::now()),
            ))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Insert error");
    }

    pub async fn insert_market_trades(&self, _market: &WithTimestamp<Market>) {
        return;
        // let inserts = market
        //     .data
        //     .trade_goods
        //     .iter()
        //     .map(|trade| {
        //         let activity = trade.activity.as_ref().map(|a| a.to_string());
        //         (
        //             market_trades::timestamp.eq(market.timestamp),
        //             market_trades::market_symbol.eq(market.data.symbol.to_string()),
        //             market_trades::symbol.eq(&trade.symbol),
        //             market_trades::trade_volume.eq(trade.trade_volume as i32),
        //             market_trades::type_.eq(trade._type.to_string()),
        //             market_trades::supply.eq(trade.supply.to_string()),
        //             market_trades::activity.eq(activity),
        //             market_trades::purchase_price.eq(trade.purchase_price as i32),
        //             market_trades::sell_price.eq(trade.sell_price as i32),
        //         )
        //     })
        //     .collect::<Vec<_>>();
        // diesel::insert_into(market_trades::table)
        //     .values(&inserts)
        //     .execute(&mut self.conn().await)
        //     .await
        //     .expect("DB Query error");
    }

    pub async fn upsert_market_transactions(&self, market: &WithTimestamp<Market>) {
        let inserts = market
            .data
            .transactions
            .iter()
            .map(|transaction| {
                (
                    market_transaction_log::timestamp.eq(transaction.timestamp),
                    market_transaction_log::market_symbol.eq(market.data.symbol.to_string()),
                    market_transaction_log::symbol.eq(&transaction.trade_symbol),
                    market_transaction_log::ship_symbol.eq(&transaction.ship_symbol),
                    market_transaction_log::type_.eq(&transaction._type),
                    market_transaction_log::units.eq(transaction.units as i32),
                    market_transaction_log::price_per_unit.eq(transaction.price_per_unit as i32),
                    market_transaction_log::total_price.eq(transaction.total_price as i32),
                )
            })
            .collect::<Vec<_>>();
        diesel::insert_into(market_transaction_log::table)
            .values(inserts)
            .on_conflict((
                market_transaction_log::market_symbol,
                market_transaction_log::timestamp,
            ))
            .do_nothing()
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    // Shouldn't be needed, since we shouldn't be loading individual shipyards
    // pub async fn get_shipyard(&self, symbol: &WaypointSymbol) -> Option<WithTimestamp<Shipyard>> {
    //     let shipyard: Option<db_models::Shipyard> = shipyards::table
    //         .filter(shipyards::waypoint_symbol.eq(symbol.to_string()))
    //         .select(db_models::Shipyard::as_select())
    //         .first(&mut self.conn().await)
    //         .await
    //         .optional()
    //         .expect("DB Query error");

    //     shipyard.map(|s| {
    //         let shipyard_data: Shipyard =
    //             serde_json::from_value(s.shipyard_data).expect("Invalid shipyard data");
    //         WithTimestamp {
    //             data: shipyard_data,
    //             timestamp: s.updated_at,
    //         }
    //     })
    // }

    pub async fn save_shipyard(&self, symbol: &WaypointSymbol, shipyard: &WithTimestamp<Shipyard>) {
        let shipyard_data =
            serde_json::to_value(&shipyard.data).expect("Failed to serialize shipyard");
        diesel::insert_into(shipyards::table)
            .values((
                shipyards::waypoint_symbol.eq(symbol.to_string()),
                shipyards::shipyard_data.eq(shipyard_data),
            ))
            .on_conflict(shipyards::waypoint_symbol)
            .do_update()
            .set((
                shipyards::shipyard_data.eq(excluded(shipyards::shipyard_data)),
                shipyards::updated_at.eq(chrono::Utc::now()),
            ))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Insert error");
    }

    pub async fn load_schedule(&self, ship_symbol: &str) -> Option<ShipSchedule> {
        let key = format!("schedules/{}", ship_symbol);
        self.get_value(&key).await
    }
    pub async fn load_schedule_progress(&self, ship_symbol: &str) -> Option<usize> {
        let key = format!("schedule_progress/{}", ship_symbol);
        self.get_value(&key).await
    }
    pub async fn save_schedule(&self, ship_symbol: &str, schedule: &ShipSchedule) {
        let key = format!("schedules/{}", ship_symbol);
        self.set_value(&key, schedule).await
    }
    pub async fn save_schedule_progress(&self, ship_symbol: &str, progress: usize) {
        let key = format!("schedule_progress/{}", ship_symbol);
        self.set_value(&key, &progress).await
    }
    pub async fn update_schedule_progress(&self, ship_symbol: &str, progress: usize) {
        self.save_schedule_progress(ship_symbol, progress).await;
    }

    // type TaskManagerStatus = DashMap<String, (Task, String, DateTime<Utc>)>
    pub async fn save_task_manager_state(
        &self,
        system_symbol: &SystemSymbol,
        state: &TaskManagerState,
    ) {
        let key = format!("task_manager/{}", system_symbol);
        self.set_value(&key, state).await
    }
    pub async fn load_task_manager_state(
        &self,
        system_symbol: &SystemSymbol,
    ) -> Option<TaskManagerState> {
        let key = format!("task_manager/{}", system_symbol);
        self.get_value(&key).await
    }

    pub async fn get_construction(
        &self,
        symbol: &WaypointSymbol,
    ) -> Option<WithTimestamp<Option<Construction>>> {
        let key = format!("construction/{}", symbol);
        self.get_value(&key).await
    }
    pub async fn save_construction(
        &self,
        symbol: &WaypointSymbol,
        construction: &WithTimestamp<Option<Construction>>,
    ) {
        let key = format!("construction/{}", symbol);
        self.set_value(&key, construction).await
    }

    pub async fn get_probe_jumpgate_reservations(
        &self,
        callsign: &str,
    ) -> DashMap<String, WaypointSymbol> {
        let key = format!("probe_jumpgate_reservations/{}", callsign);
        self.get_value(&key).await.unwrap_or_default()
    }

    pub async fn save_probe_jumpgate_reservations(
        &self,
        callsign: &str,
        reservations: &DashMap<String, WaypointSymbol>,
    ) {
        let key = format!("probe_jumpgate_reservations/{}", callsign);
        self.set_value(&key, &reservations).await
    }

    pub async fn get_explorer_reservations(&self, callsign: &str) -> DashMap<String, SystemSymbol> {
        let key = format!("explorer_reservations/{}", callsign);
        self.get_value(&key).await.unwrap_or_default()
    }

    pub async fn save_explorer_reservations(
        &self,
        callsign: &str,
        reservations: &DashMap<String, SystemSymbol>,
    ) {
        let key = format!("explorer_reservations/{}", callsign);
        self.set_value(&key, &reservations).await
    }

    pub async fn get_factions(&self) -> Option<Vec<crate::models::Faction>> {
        self.get_value("factions").await
    }

    pub async fn set_factions(&self, factions: &Vec<crate::models::Faction>) {
        self.set_value("factions", factions).await
    }

    pub async fn insert_surveys(&self, surveys: &Vec<KeyedSurvey>) {
        let now = Utc::now();
        let inserts = surveys
            .iter()
            .map(|survey| {
                (
                    surveys::uuid.eq(&survey.uuid),
                    surveys::survey.eq(serde_json::to_value(&survey.survey).unwrap()),
                    surveys::asteroid_symbol.eq(survey.survey.symbol.to_string()),
                    surveys::inserted_at.eq(now),
                    surveys::expires_at.eq(survey.survey.expiration),
                )
            })
            .collect::<Vec<_>>();
        diesel::insert_into(surveys::table)
            .values(&inserts)
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    pub async fn get_surveys(&self) -> Vec<KeyedSurvey> {
        let surveys: Vec<(Uuid, Value)> = surveys::table
            .select((surveys::uuid, surveys::survey))
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");
        surveys
            .into_iter()
            .map(|(uuid, survey)| KeyedSurvey {
                uuid,
                survey: serde_json::from_value(survey).unwrap(),
            })
            .collect()
    }

    pub async fn remove_survey(&self, uuid: &Uuid) {
        diesel::delete(surveys::table.filter(surveys::uuid.eq(uuid)))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    pub async fn get_systems(&self) -> Vec<db_models::System> {
        systems::table
            .select(db_models::System::as_select())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error")
    }

    pub async fn insert_systems(&self, system_inserts: &[db_models::NewSystem<'_>]) -> Vec<i64> {
        let mut system_ids: Vec<i64> = vec![];
        for chunk in system_inserts.chunks(1000) {
            let ids: Vec<i64> = diesel::insert_into(systems::table)
                .values(chunk)
                .returning(systems::id)
                .on_conflict(systems::symbol)
                .do_update()
                .set((
                    // Use empty ON CONFLICT UPDATE set hack to return id
                    // yes it's a hack, and empty updates have consequences, but it's okay here
                    systems::symbol.eq(excluded(systems::symbol)),
                ))
                .get_results(&mut self.conn().await)
                .await
                .expect("DB Insert error");
            assert_eq!(chunk.len(), ids.len());
            system_ids.extend(ids);
        }
        assert_eq!(system_ids.len(), system_inserts.len());
        system_ids
    }

    pub async fn insert_system(&self, system_insert: &db_models::NewSystem<'_>) -> i64 {
        let ids = self
            .insert_systems(std::slice::from_ref(system_insert))
            .await;
        assert_eq!(ids.len(), 1);
        ids[0]
    }

    pub async fn insert_waypoints(&self, waypoints: &[db_models::NewWaypoint<'_>]) -> Vec<i64> {
        let mut waypoint_ids: Vec<i64> = vec![];
        for chunk in waypoints.chunks(1000) {
            let ids: Vec<i64> = diesel::insert_into(waypoints::table)
                .values(chunk)
                .returning(waypoints::id)
                .on_conflict(waypoints::symbol)
                .do_update()
                .set((
                    // Use empty ON CONFLICT UPDATE set hack to return id
                    // yes it's a hack, and empty updates have consequences, but it's okay here
                    waypoints::symbol.eq(excluded(waypoints::symbol)),
                ))
                .get_results(&mut self.conn().await)
                .await
                .expect("DB Insert error");
            assert_eq!(chunk.len(), ids.len());
            waypoint_ids.extend(ids);
        }
        assert_eq!(waypoint_ids.len(), waypoints.len());
        waypoint_ids
    }

    pub async fn get_all_markets(&self) -> Vec<(WaypointSymbol, WithTimestamp<Market>)> {
        let markets: Vec<db_models::Market> = markets::table
            .select(db_models::Market::as_select())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");

        markets
            .into_iter()
            .map(|m| {
                let market_data: Market =
                    serde_json::from_value(m.market_data).expect("Invalid market data");
                let symbol = WaypointSymbol::new(&m.waypoint_symbol);
                (
                    symbol,
                    WithTimestamp {
                        data: market_data,
                        timestamp: m.updated_at,
                    },
                )
            })
            .collect()
    }

    pub async fn get_all_shipyards(&self) -> Vec<(WaypointSymbol, WithTimestamp<Shipyard>)> {
        let shipyards: Vec<db_models::Shipyard> = shipyards::table
            .select(db_models::Shipyard::as_select())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");

        shipyards
            .into_iter()
            .map(|s| {
                let shipyard_data: Shipyard =
                    serde_json::from_value(s.shipyard_data).expect("Invalid shipyard data");
                let symbol = WaypointSymbol::new(&s.waypoint_symbol);
                (
                    symbol,
                    WithTimestamp {
                        data: shipyard_data,
                        timestamp: s.updated_at,
                    },
                )
            })
            .collect()
    }
}
