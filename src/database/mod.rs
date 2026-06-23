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
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
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

// A single KPI snapshot from agent_metrics (used to chart the equity curve & fleet size).
pub struct MetricsPoint {
    pub ts: chrono::DateTime<Utc>,
    pub credits: i64,
    pub net_worth: i64,
    pub num_ships: i32,
}

// One row of the cash journal. `amount` is the signed gross cash delta (negative
// = credits out, positive = credits in). `realized_profit` is set on trade_sell
// rows only (proceeds - cost basis of the units sold) and is NOT part of the
// cash sum. Build one of these at every credit-changing call site and pass it to
// DbClient::record_cash_txn — the single choke point that keeps the journal
// complete.
pub struct CashTxn<'a> {
    pub ts: chrono::DateTime<Utc>,
    pub type_: &'a str,
    pub ship_symbol: Option<&'a str>,
    pub reference: Option<&'a str>,
    pub waypoint: Option<&'a str>,
    pub units: Option<i32>,
    pub amount: i64,
    pub realized_profit: Option<i64>,
}


impl DbClient {
    pub async fn new(slice_id: &str) -> DbClient {
        let database_url = std::env::var("POSTGRES_URI").expect("POSTGRES_URI must be set");
        info!("Using schema: {}", slice_id);
        let db = {
            let database_url = format!("{}?options=-c%20search_path%3D{}", database_url, slice_id);
            let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
            // The fleet has outgrown a 5-connection pool (30+ ships + intel probes + a
            // remote-system trader): concurrent DB ops exceeded it and conn() timed out,
            // panicking the whole agent. Give it comfortable headroom.
            Pool::builder(manager).max_size(32).build().unwrap()
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
        db.create_schema(slice_id).await;
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

    // Record a supply/price snapshot per trade good, but only for goods whose
    // values changed since the last stored snapshot. This keeps the hypertable
    // small (markets are re-observed constantly) and yields a clean step chart.
    pub async fn insert_market_trades(&self, market: &WithTimestamp<Market>) {
        let market_symbol = market.data.symbol.to_string();

        // Latest stored snapshot per good for this market.
        #[derive(QueryableByName)]
        struct LatestRow {
            #[diesel(sql_type = Text)]
            symbol: String,
            #[diesel(sql_type = Integer)]
            trade_volume: i32,
            #[diesel(sql_type = Text)]
            supply: String,
            #[diesel(sql_type = Nullable<Text>)]
            activity: Option<String>,
            #[diesel(sql_type = Integer)]
            purchase_price: i32,
            #[diesel(sql_type = Integer)]
            sell_price: i32,
        }
        let latest: Vec<LatestRow> = diesel::sql_query(
            "SELECT DISTINCT ON (symbol) symbol, trade_volume, supply, activity, \
             purchase_price, sell_price FROM market_trades \
             WHERE market_symbol = $1 ORDER BY symbol, \"timestamp\" DESC",
        )
        .bind::<Text, _>(&market_symbol)
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        let latest_map: std::collections::HashMap<String, LatestRow> =
            latest.into_iter().map(|r| (r.symbol.clone(), r)).collect();

        let changed = |trade: &crate::models::MarketTradeGood| {
            let activity = trade.activity.as_ref().map(|a| a.to_string());
            match latest_map.get(&trade.symbol) {
                Some(prev) => {
                    prev.trade_volume != trade.trade_volume as i32
                        || prev.supply != trade.supply.to_string()
                        || prev.activity != activity
                        || prev.purchase_price != trade.purchase_price as i32
                        || prev.sell_price != trade.sell_price as i32
                }
                None => true,
            }
        };

        let inserts = market
            .data
            .trade_goods
            .iter()
            .filter(|trade| changed(trade))
            .map(|trade| {
                let activity = trade.activity.as_ref().map(|a| a.to_string());
                (
                    market_trades::timestamp.eq(market.timestamp),
                    market_trades::market_symbol.eq(market.data.symbol.to_string()),
                    market_trades::symbol.eq(&trade.symbol),
                    market_trades::trade_volume.eq(trade.trade_volume as i32),
                    market_trades::type_.eq(trade._type.to_string()),
                    market_trades::supply.eq(trade.supply.to_string()),
                    market_trades::activity.eq(activity),
                    market_trades::purchase_price.eq(trade.purchase_price as i32),
                    market_trades::sell_price.eq(trade.sell_price as i32),
                )
            })
            .collect::<Vec<_>>();
        if inserts.is_empty() {
            return;
        }
        diesel::insert_into(market_trades::table)
            .values(inserts)
            .on_conflict((
                market_trades::market_symbol,
                market_trades::symbol,
                market_trades::timestamp,
            ))
            .do_nothing()
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    // Supply/price snapshots for a market (ascending), one row per good per change.
    #[allow(clippy::type_complexity)]
    pub async fn market_price_history(
        &self,
        market: &WaypointSymbol,
    ) -> Vec<(
        chrono::DateTime<Utc>,
        String,
        i32,
        String,
        Option<String>,
        i32,
        i32,
    )> {
        market_trades::table
            .filter(market_trades::market_symbol.eq(market.to_string()))
            .select((
                market_trades::timestamp,
                market_trades::symbol,
                market_trades::trade_volume,
                market_trades::supply,
                market_trades::activity,
                market_trades::purchase_price,
                market_trades::sell_price,
            ))
            .order(market_trades::timestamp.asc())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error")
    }

    // Record that a market was observed at this instant, whether or not any
    // values changed. market_trades only stores changes; this is the full record
    // of sample times, used to draw per-observation ticks on the dashboard chart.
    pub async fn insert_market_observation(&self, market: &WithTimestamp<Market>) {
        diesel::insert_into(market_observations::table)
            .values((
                market_observations::timestamp.eq(market.timestamp),
                market_observations::market_symbol.eq(market.data.symbol.to_string()),
            ))
            .on_conflict((
                market_observations::market_symbol,
                market_observations::timestamp,
            ))
            .do_nothing()
            .execute(&mut self.conn().await)
            .await
            .expect("DB Insert error");
    }

    // All times a market was observed (ascending). Applies to every good at the
    // market, since a single observation samples the whole market at once.
    pub async fn market_observation_times(
        &self,
        market: &WaypointSymbol,
    ) -> Vec<chrono::DateTime<Utc>> {
        market_observations::table
            .filter(market_observations::market_symbol.eq(market.to_string()))
            .select(market_observations::timestamp)
            .order(market_observations::timestamp.asc())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error")
    }

    // Our own buy/sell transactions at a market (ascending), read from the cash
    // journal. Type is normalized to PURCHASE/SELL for the dashboard view.
    pub async fn market_transactions(
        &self,
        market: &WaypointSymbol,
    ) -> Vec<(chrono::DateTime<Utc>, String, String, i32, i32, i32)> {
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = diesel::sql_types::Timestamptz)]
            ts: chrono::DateTime<Utc>,
            #[diesel(sql_type = Text)]
            symbol: String,
            #[diesel(sql_type = Text)]
            type_: String,
            #[diesel(sql_type = Integer)]
            units: i32,
            #[diesel(sql_type = Integer)]
            price_per_unit: i32,
            #[diesel(sql_type = Integer)]
            total_price: i32,
        }
        let rows: Vec<Row> = diesel::sql_query(
            "SELECT ts, \
                    reference AS symbol, \
                    CASE WHEN type='trade_sell' THEN 'SELL' ELSE 'PURCHASE' END AS type_, \
                    COALESCE(units, 0) AS units, \
                    CASE WHEN COALESCE(units,0) <> 0 THEN (ABS(amount) / units)::int ELSE 0 END AS price_per_unit, \
                    ABS(amount)::int AS total_price \
             FROM agent_transaction_log \
             WHERE type IN ('trade_buy','trade_sell') AND waypoint = $1 \
             ORDER BY ts ASC",
        )
        .bind::<Text, _>(market.to_string())
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        rows.into_iter()
            .map(|r| {
                (
                    r.ts,
                    r.symbol,
                    r.type_,
                    r.units,
                    r.price_per_unit,
                    r.total_price,
                )
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_agent_metrics(
        &self,
        ts: chrono::DateTime<Utc>,
        credits: i64,
        available_credits: i64,
        reserved_credits: i64,
        cargo_value: i64,
        num_ships: i32,
        net_worth: i64,
    ) {
        diesel::insert_into(agent_metrics::table)
            .values((
                agent_metrics::ts.eq(ts),
                agent_metrics::credits.eq(credits),
                agent_metrics::available_credits.eq(available_credits),
                agent_metrics::reserved_credits.eq(reserved_credits),
                agent_metrics::cargo_value.eq(cargo_value),
                agent_metrics::num_ships.eq(num_ships),
                agent_metrics::net_worth.eq(net_worth),
            ))
            .on_conflict(agent_metrics::ts)
            .do_nothing()
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    // Recent KPI history (ascending by ts) for charting the equity curve and fleet size.
    pub async fn get_metrics_history(&self, limit: i64) -> Vec<MetricsPoint> {
        let mut rows: Vec<(chrono::DateTime<Utc>, i64, i64, i32)> = agent_metrics::table
            .select((
                agent_metrics::ts,
                agent_metrics::credits,
                agent_metrics::net_worth,
                agent_metrics::num_ships,
            ))
            .order(agent_metrics::ts.desc())
            .limit(limit)
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");
        rows.reverse();
        rows.into_iter()
            .map(|(ts, credits, net_worth, num_ships)| MetricsPoint {
                ts,
                credits,
                net_worth,
                num_ships,
            })
            .collect()
    }

    // Ship-purchase cash events (ascending by ts), amounts returned as positive spend.
    pub async fn ship_spend_events(&self) -> Vec<(chrono::DateTime<Utc>, i64)> {
        self.spend_events("ship_purchase").await
    }

    // The single choke point for the cash journal: every credit-changing event
    // is recorded here. Keeping all spend/income paths funneled through this one
    // method is what lets the reconciliation check (record_metrics) assert the
    // journal is complete.
    // Net cash impact per ship: signed sum of every ship-attributed journal entry
    // (trade margin - fuel - jump antimatter - purchase + scrap). Excludes agent-level
    // entries such as contract payouts, which carry no ship_symbol.
    pub async fn net_cash_by_ship(&self) -> Vec<(String, i64)> {
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = Text)]
            ship_symbol: String,
            #[diesel(sql_type = BigInt)]
            net_cash: i64,
        }
        let rows: Vec<Row> = diesel::sql_query(
            "SELECT ship_symbol, SUM(amount)::bigint AS net_cash \
             FROM agent_transaction_log \
             WHERE ship_symbol IS NOT NULL \
             GROUP BY ship_symbol",
        )
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        rows.into_iter().map(|r| (r.ship_symbol, r.net_cash)).collect()
    }

    pub async fn record_cash_txn(&self, t: CashTxn<'_>) {
        diesel::insert_into(agent_transaction_log::table)
            .values((
                agent_transaction_log::ts.eq(t.ts),
                agent_transaction_log::type_.eq(t.type_),
                agent_transaction_log::ship_symbol.eq(t.ship_symbol),
                agent_transaction_log::reference.eq(t.reference),
                agent_transaction_log::waypoint.eq(t.waypoint),
                agent_transaction_log::units.eq(t.units),
                agent_transaction_log::amount.eq(t.amount),
                agent_transaction_log::realized_profit.eq(t.realized_profit),
            ))
            .execute(&mut self.conn().await)
            .await
            .expect("DB Query error");
    }

    // Sum of journal cash deltas in (start, end]. Used by the reconciliation
    // check: this must equal the actual change in credits over the same window,
    // or some credit-moving path isn't going through record_cash_txn.
    pub async fn cash_txn_sum_between(
        &self,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
    ) -> i64 {
        #[derive(QueryableByName)]
        struct SumRet {
            #[diesel(sql_type = BigInt)]
            total: i64,
        }
        let result: SumRet = diesel::sql_query(
            "SELECT COALESCE(SUM(amount), 0)::bigint AS total \
             FROM agent_transaction_log WHERE ts > $1 AND ts <= $2",
        )
        .bind::<diesel::sql_types::Timestamptz, _>(start)
        .bind::<diesel::sql_types::Timestamptz, _>(end)
        .get_result(&mut self.conn().await)
        .await
        .expect("DB Query error");
        result.total
    }

    // Cumulative credits spent on ships (cost basis), used to approximate net worth.
    pub async fn ship_cost_basis(&self) -> i64 {
        #[derive(QueryableByName)]
        struct SumRet {
            #[diesel(sql_type = BigInt)]
            total: i64,
        }
        // amounts are negative (credits out); negate to return positive cost basis
        let result: SumRet = diesel::sql_query(
            "SELECT COALESCE(-SUM(amount), 0)::bigint AS total \
             FROM agent_transaction_log WHERE type = 'ship_purchase'",
        )
        .get_result(&mut self.conn().await)
        .await
        .expect("DB Query error");
        result.total
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

    // Append a snapshot of a construction site's per-material progress.
    // Skips materials whose latest stored `fulfilled` matches the new value, so the
    // log stays a strict change log even when callers snapshot opportunistically.
    pub async fn insert_construction_snapshot(
        &self,
        ts: chrono::DateTime<Utc>,
        waypoint: &WaypointSymbol,
        materials: &[(String, i32, i32)],
    ) {
        if materials.is_empty() {
            return;
        }
        let waypoint_s = waypoint.to_string();
        let mut conn = self.conn().await;
        #[derive(QueryableByName)]
        struct LatestRow {
            #[diesel(sql_type = Text)]
            trade_symbol: String,
            #[diesel(sql_type = Integer)]
            fulfilled: i32,
        }
        // Latest fulfilled per material for dedup.
        let latest: Vec<LatestRow> = diesel::sql_query(
            "SELECT DISTINCT ON (trade_symbol) trade_symbol, fulfilled \
             FROM construction_log WHERE waypoint = $1 \
             ORDER BY trade_symbol, ts DESC",
        )
        .bind::<Text, _>(&waypoint_s)
        .get_results(&mut conn)
        .await
        .expect("DB Query error");
        let mut latest_map: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
        for r in latest {
            latest_map.insert(r.trade_symbol, r.fulfilled);
        }
        let inserts: Vec<_> = materials
            .iter()
            .filter(|(s, f, _)| latest_map.get(s) != Some(f))
            .map(|(s, f, r)| {
                (
                    construction_log::ts.eq(ts),
                    construction_log::waypoint.eq(&waypoint_s),
                    construction_log::trade_symbol.eq(s),
                    construction_log::fulfilled.eq(*f),
                    construction_log::required.eq(*r),
                )
            })
            .collect();
        if inserts.is_empty() {
            return;
        }
        diesel::insert_into(construction_log::table)
            .values(inserts)
            .on_conflict_do_nothing()
            .execute(&mut conn)
            .await
            .expect("DB Insert error");
    }

    // Per-material fulfilment history for a construction site, ascending by ts.
    pub async fn get_construction_history(
        &self,
        waypoint: &WaypointSymbol,
    ) -> Vec<(chrono::DateTime<Utc>, String, i32, i32)> {
        construction_log::table
            .filter(construction_log::waypoint.eq(waypoint.to_string()))
            .select((
                construction_log::ts,
                construction_log::trade_symbol,
                construction_log::fulfilled,
                construction_log::required,
            ))
            .order((
                construction_log::trade_symbol.asc(),
                construction_log::ts.asc(),
            ))
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error")
    }

    // Distinct waypoints that have any construction-progress snapshots.
    pub async fn construction_waypoints(&self) -> Vec<String> {
        #[derive(QueryableByName)]
        struct WpRow {
            #[diesel(sql_type = Text)]
            waypoint: String,
        }
        let rows: Vec<WpRow> = diesel::sql_query(
            "SELECT DISTINCT waypoint FROM construction_log ORDER BY waypoint",
        )
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        rows.into_iter().map(|r| r.waypoint).collect()
    }

    // Net credits spent on the listed trade goods (buys - sells), from our cash
    // journal. Construction haulers shouldn't sell these, so the figure is
    // normally just gross purchases; subtracting sells keeps it honest if mining
    // ever offloads the same symbol.
    pub async fn market_net_spend_by_good(&self, symbols: &[String]) -> Vec<(String, i64)> {
        if symbols.is_empty() {
            return vec![];
        }
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = Text)]
            symbol: String,
            #[diesel(sql_type = BigInt)]
            net_spend: i64,
        }
        // amount is signed cash (buy negative, sell positive); -SUM = net spend.
        let rows: Vec<Row> = diesel::sql_query(
            "SELECT reference AS symbol, (-COALESCE(SUM(amount), 0))::bigint AS net_spend \
             FROM agent_transaction_log \
             WHERE type IN ('trade_buy','trade_sell') AND reference = ANY($1) \
             GROUP BY reference",
        )
        .bind::<diesel::sql_types::Array<Text>, _>(symbols)
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        rows.into_iter().map(|r| (r.symbol, r.net_spend)).collect()
    }

    // Net spend (credits out, positive) per event on any trade whose good is a
    // known construction material. Construction haulers buy these and donate them
    // to gates (no resale), so this is the gate-construction expense line. The
    // journal is our agent only, so no callsign filter is needed.
    pub async fn construction_spend_events(&self) -> Vec<(chrono::DateTime<Utc>, i64)> {
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = diesel::sql_types::Timestamptz)]
            ts: chrono::DateTime<Utc>,
            #[diesel(sql_type = BigInt)]
            amount: i64,
        }
        let rows: Vec<Row> = diesel::sql_query(
            "SELECT ts, (-amount)::bigint AS amount \
             FROM agent_transaction_log \
             WHERE type IN ('trade_buy','trade_sell') \
             AND reference IN (SELECT DISTINCT trade_symbol FROM construction_log) \
             ORDER BY ts ASC",
        )
        .get_results(&mut self.conn().await)
        .await
        .expect("DB Query error");
        rows.into_iter().map(|r| (r.ts, r.amount)).collect()
    }

    // Cumulative-able signed events from the journal for a category, returned as
    // positive spend (credits out flipped positive). Used for the per-category
    // expense lines on the dashboard.
    async fn spend_events(&self, type_: &str) -> Vec<(chrono::DateTime<Utc>, i64)> {
        let rows: Vec<(chrono::DateTime<Utc>, i64)> = agent_transaction_log::table
            .filter(agent_transaction_log::type_.eq(type_))
            .select((agent_transaction_log::ts, agent_transaction_log::amount))
            .order(agent_transaction_log::ts.asc())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");
        rows.into_iter().map(|(ts, amount)| (ts, -amount)).collect()
    }

    // Jump-gate antimatter spend (credits out): the credits charged on each jump.
    pub async fn antimatter_spend_events(&self) -> Vec<(chrono::DateTime<Utc>, i64)> {
        self.spend_events("jump").await
    }

    // Flying-fuel spend events (refuel endpoint, credits out).
    pub async fn fuel_spend_events(&self) -> Vec<(chrono::DateTime<Utc>, i64)> {
        self.spend_events("refuel").await
    }

    // Realized trade profit per sale (proceeds - cost basis), signed.
    pub async fn realized_profit_events(&self) -> Vec<(chrono::DateTime<Utc>, i64)> {
        let rows: Vec<(chrono::DateTime<Utc>, Option<i64>)> = agent_transaction_log::table
            .filter(agent_transaction_log::type_.eq("trade_sell"))
            .select((
                agent_transaction_log::ts,
                agent_transaction_log::realized_profit,
            ))
            .order(agent_transaction_log::ts.asc())
            .load(&mut self.conn().await)
            .await
            .expect("DB Query error");
        rows.into_iter()
            .map(|(ts, p)| (ts, p.unwrap_or(0)))
            .collect()
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

    pub async fn insert_surveys(&self, surveys: &[KeyedSurvey]) {
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
