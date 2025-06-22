use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use scylla::{
    client::{session::Session, session_builder::SessionBuilder},
    statement::Statement,
    DeserializeRow, SerializeRow,
};
use std::sync::Arc;

#[derive(Debug, Clone, DeserializeRow, SerializeRow, Serialize, Deserialize)]
pub struct CurrentState {
    pub event_log_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub state_data: String,
    pub last_updated: DateTime<Utc>,
    pub seq_num: i64,
    pub entity_seq_num: i64,
    pub last_snapshot_entity_seq_num: i64,
}

#[derive(Debug, Clone, DeserializeRow, SerializeRow, Serialize, Deserialize)]
pub struct Event {
    pub event_log_id: String,
    pub seq_num: i64,             // Primary ordering mechanism within event log
    pub timestamp: DateTime<Utc>, // When the event occurred
    pub entity_id: String,
    pub event_type: String,
    pub event_data: String,
}

#[derive(Debug, DeserializeRow, SerializeRow)]
pub struct Snapshot {
    pub event_log_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub state_data: String,
    pub last_updated: DateTime<Utc>,
    pub seq_num: i64,
    pub entity_seq_num: i64,
}

#[derive(Debug, DeserializeRow, SerializeRow)]
pub struct EventLog {
    pub event_log_id: String,
    pub last_seq_num: i64,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ScyllaClient {
    session: Arc<Session>,
}

impl ScyllaClient {
    pub async fn new() -> Self {
        let session = SessionBuilder::new()
            .known_node(std::env::var("SCYLLA_URI").expect("SCYLLA_URI env var not set"))
            .build()
            .await
            .expect("Failed to connect to Scylla");

        ScyllaClient {
            session: Arc::new(session),
        }
    }

    pub async fn get_event_log(&self, log_id: &str) -> Option<EventLog> {
        let query = Statement::new("SELECT event_log_id, last_seq_num, last_updated FROM spacetraders.event_logs WHERE event_log_id = ? LIMIT 1");
        let result = self.session.query_unpaged(query, &(log_id,)).await.unwrap();
        let result = result.into_rows_result().unwrap();
        result
            .rows::<EventLog>()
            .unwrap()
            .next()
            .map(|row| row.unwrap())
    }

    pub async fn upsert_event_log(&self, log: &EventLog) {
        let update_query = Statement::new(
            "INSERT INTO spacetraders.event_logs (event_log_id, last_seq_num, last_updated) VALUES (?, ?, ?)",
        );
        self.session.query_unpaged(update_query, log).await.unwrap();
    }

    // Current State Operations
    pub async fn get_entity(&self, event_log_id: &str, entity_id: &str) -> Option<CurrentState> {
        let query = Statement::new("SELECT * FROM spacetraders.current_state WHERE event_log_id = ? AND entity_id = ? LIMIT 1");
        let result = self
            .session
            .query_unpaged(query, &(event_log_id.to_string(), entity_id.to_string()))
            .await
            .unwrap();
        let result = result.into_rows_result().unwrap();
        result
            .rows::<CurrentState>()
            .unwrap()
            .next()
            .map(|row| row.unwrap())
    }

    pub async fn upsert_entity(&self, current_state: &CurrentState) {
        let query = Statement::new("INSERT INTO spacetraders.current_state (event_log_id, entity_id, entity_type, state_data, last_updated, seq_num, entity_seq_num, last_snapshot_entity_seq_num) VALUES (?, ?, ?, ?, ?, ?, ?, ?)");
        self.session
            .query_unpaged(query, current_state)
            .await
            .unwrap();
    }

    // Event Operations - Main table for consecutive event retrieval
    pub async fn insert_event(&self, event: &Event) {
        // Insert into main events table
        let query = Statement::new(
            "INSERT INTO spacetraders.events (event_log_id, seq_num, timestamp, entity_id, event_type, event_data) VALUES (?, ?, ?, ?, ?, ?)",
        );
        self.session.query_unpaged(query, event).await.unwrap();
    }

    /// Get consecutive events across all entities for a specific event log
    pub async fn get_events(
        &self,
        event_log_id: &str,
        from_seq_num: Option<i64>,
        limit: Option<i32>,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let mut query_str = "SELECT * FROM spacetraders.events WHERE event_log_id = ?".to_string();

        if let Some(_from_seq) = from_seq_num {
            query_str.push_str(" AND seq_num > ?");
        }

        query_str.push_str(" ORDER BY seq_num ASC");

        if let Some(_limit_val) = limit {
            query_str.push_str(" LIMIT ?");
        }

        let query = Statement::new(query_str);

        // Use different query patterns based on parameters
        let result = if let Some(from_seq) = from_seq_num {
            if let Some(limit_val) = limit {
                self.session
                    .query_unpaged(query, (event_log_id.to_string(), from_seq, limit_val))
                    .await?
            } else {
                self.session
                    .query_unpaged(query, (event_log_id.to_string(), from_seq))
                    .await?
            }
        } else {
            if let Some(limit_val) = limit {
                self.session
                    .query_unpaged(query, (event_log_id.to_string(), limit_val))
                    .await?
            } else {
                self.session
                    .query_unpaged(query, (event_log_id.to_string(),))
                    .await?
            }
        };

        let rows = result.into_rows_result()?;
        Ok(rows.rows::<Event>()?.map(|row| row.unwrap()).collect())
    }

    /// Get events for a specific entity within an event log using the materialized view
    pub async fn get_events_by_entity(
        &self,
        event_log_id: &str,
        entity_id: &str,
        from_sequence: Option<i64>,
        limit: Option<i32>,
    ) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let mut query_str = "SELECT * FROM spacetraders.events_by_entity_id WHERE event_log_id = ? AND entity_id = ?".to_string();

        if let Some(_from_seq) = from_sequence {
            query_str.push_str(" AND seq_num > ?");
        }

        query_str.push_str(" ORDER BY entity_id ASC, seq_num ASC");

        if let Some(_limit_val) = limit {
            query_str.push_str(" LIMIT ?");
        }

        let query = Statement::new(query_str);

        // Use different query patterns based on parameters
        let result = if let Some(from_seq) = from_sequence {
            if let Some(limit_val) = limit {
                self.session
                    .query_unpaged(
                        query,
                        (
                            event_log_id.to_string(),
                            entity_id.to_string(),
                            from_seq,
                            limit_val,
                        ),
                    )
                    .await?
            } else {
                self.session
                    .query_unpaged(
                        query,
                        (event_log_id.to_string(), entity_id.to_string(), from_seq),
                    )
                    .await?
            }
        } else {
            if let Some(limit_val) = limit {
                self.session
                    .query_unpaged(
                        query,
                        (event_log_id.to_string(), entity_id.to_string(), limit_val),
                    )
                    .await?
            } else {
                self.session
                    .query_unpaged(query, (event_log_id.to_string(), entity_id.to_string()))
                    .await?
            }
        };

        let rows = result.into_rows_result()?;
        Ok(rows.rows::<Event>()?.map(|row| row.unwrap()).collect())
    }

    // Snapshot Operations
    pub async fn insert_snapshot(&self, snapshot: &Snapshot) {
        let query = Statement::new(
            "INSERT INTO spacetraders.snapshots (event_log_id, entity_id, entity_type, last_updated, seq_num, entity_seq_num, state_data) VALUES (?, ?, ?, ?, ?, ?, ?)",
        );
        self.session.query_unpaged(query, snapshot).await.unwrap();
    }

    pub async fn get_latest_snapshot(
        &self,
        event_log_id: &str,
        entity_id: &str,
    ) -> Option<Snapshot> {
        let query = Statement::new(
            "SELECT * FROM spacetraders.snapshots WHERE event_log_id = ? AND entity_id = ? ORDER BY seq_num DESC LIMIT 1",
        );
        let result = self
            .session
            .query_unpaged(query, &(event_log_id.to_string(), entity_id.to_string()))
            .await
            .unwrap();

        let rows = result.into_rows_result().unwrap();
        rows.rows::<Snapshot>()
            .unwrap()
            .next()
            .map(|row| row.unwrap())
    }

    pub async fn get_snapshots(
        &self,
        event_log_id: &str,
        entity_id: &str,
        limit: Option<i32>,
    ) -> Result<Vec<Snapshot>, Box<dyn std::error::Error>> {
        let mut query_str = "SELECT * FROM spacetraders.snapshots WHERE event_log_id = ? AND entity_id = ? ORDER BY seq_num DESC".to_string();

        if let Some(_limit_val) = limit {
            query_str.push_str(" LIMIT ?");
        }

        let query = Statement::new(query_str);

        let result = if let Some(limit_val) = limit {
            self.session
                .query_unpaged(
                    query,
                    (event_log_id.to_string(), entity_id.to_string(), limit_val),
                )
                .await?
        } else {
            self.session
                .query_unpaged(query, (event_log_id.to_string(), entity_id.to_string()))
                .await?
        };

        let rows = result.into_rows_result()?;
        Ok(rows.rows::<Snapshot>()?.map(|row| row.unwrap()).collect())
    }
}
