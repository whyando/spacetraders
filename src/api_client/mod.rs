pub mod api_models;
pub mod interceptor;
pub mod kafka_interceptor;

use self::interceptor::ApiInterceptor;
use crate::config::CONFIG;
use crate::models::*;
use core::panic;
use log::*;
use reqwest::{self, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, Ordering},
};
use tokio::time::Instant;

const API_MAX_PAGE_SIZE: usize = 20;

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
    req_id: Arc<AtomicU64>,
    agent_token: Arc<RwLock<Option<String>>>,
    slice_id: Arc<RwLock<Option<String>>>,
    next_request_ts: Arc<Mutex<Option<Instant>>>,
    interceptors: Arc<Vec<Arc<dyn ApiInterceptor + 'static>>>,
}

impl ApiClient {
    pub fn new(interceptors: Vec<Arc<dyn ApiInterceptor + 'static>>) -> ApiClient {
        let user_agent = format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        let client = reqwest::ClientBuilder::new()
            .user_agent(user_agent)
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .build()
            .unwrap();
        ApiClient {
            client,
            base_url: CONFIG.api_base_url.to_string(),
            req_id: Arc::new(AtomicU64::new(0)),
            agent_token: Arc::new(RwLock::new(None)),
            slice_id: Arc::new(RwLock::new(None)),
            next_request_ts: Arc::new(Mutex::new(None)),
            interceptors: Arc::new(interceptors),
        }
    }

    pub fn set_agent_token(&self, token: &str) {
        let mut agent_token = self.agent_token.write().unwrap();
        if agent_token.is_some() {
            panic!("Cannot set agent token while agent token is already set");
        }
        *agent_token = Some(token.to_string());
    }

    pub fn set_slice_id(&self, id: &str) {
        let mut slice_id = self.slice_id.write().unwrap();
        if slice_id.is_some() {
            panic!("Cannot set slice id while slice id is already set");
        }
        *slice_id = Some(id.to_string());
    }

    // pub async fn status(&self) -> Status {
    //     self.get("/").await
    // }

    pub async fn status(&self) -> (StatusCode, Result<Status, String>) {
        self.request(Method::GET, "/", None::<&()>).await
    }

    pub fn agent_token(&self) -> Option<String> {
        self.agent_token.read().unwrap().clone()
    }

    pub fn slice_id(&self) -> Option<String> {
        self.slice_id.read().unwrap().clone()
    }

    pub async fn register(&self, faction: &str, callsign: &str) -> String {
        assert!(
            self.agent_token().is_none(),
            "Cannot register while agent token is already set"
        );
        debug!(
            "Registering new agent {} with faction {}",
            callsign, faction
        );
        let req_body = json!({
            "faction": faction,
            "symbol": callsign,
        });

        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct RegisterResponse {
            agent: Agent,
            contract: Contract,
            faction: Faction,
            ships: Vec<Ship>,
            token: String,
        }

        let body: Data<RegisterResponse> = self.post("/register", &req_body).await;
        body.data.token
    }

    pub async fn get_agent(&self) -> Agent {
        let response: Data<Agent> = self.get("/my/agent").await;
        response.data
    }

    pub async fn get_agent_public(&self, callsign: &str) -> Agent {
        let response: Data<Agent> = self.get(&format!("/agents/{}", callsign)).await;
        response.data
    }

    pub async fn get_ship(&self, id: &str) -> Ship {
        let response: Data<Ship> = self.get(&format!("/my/ships/{}", id)).await;
        response.data
    }

    pub async fn get_all_ships(&self) -> Vec<Ship> {
        self.get_all_pages("/my/ships").await
    }

    pub async fn get_contract(&self) -> Option<Contract> {
        self.get_final_paginated_entry("/my/contracts").await
    }

    pub async fn get_system(&self, system_symbol: &SystemSymbol) -> api_models::System {
        let system: Data<api_models::System> =
            self.get(&format!("/systems/{}", system_symbol)).await;
        system.data
    }

    pub async fn get_system_waypoints(
        &self,
        system_symbol: &SystemSymbol,
    ) -> Vec<api_models::WaypointDetailed> {
        self.get_all_pages(&format!("/systems/{}/waypoints", system_symbol))
            .await
    }

    pub async fn get_market_remote(&self, symbol: &WaypointSymbol) -> MarketRemoteView {
        let market: Data<MarketRemoteView> = self
            .get(&format!(
                "/systems/{}/waypoints/{}/market",
                symbol.system(),
                symbol
            ))
            .await;
        market.data
    }

    pub async fn get_shipyard_remote(&self, symbol: &WaypointSymbol) -> ShipyardRemoteView {
        let shipyard: Data<ShipyardRemoteView> = self
            .get(&format!(
                "/systems/{}/waypoints/{}/shipyard",
                symbol.system(),
                symbol
            ))
            .await;
        shipyard.data
    }

    pub async fn get_construction(
        &self,
        symbol: &WaypointSymbol,
    ) -> WithTimestamp<Option<Construction>> {
        let path = format!(
            "/systems/{}/waypoints/{}/construction",
            symbol.system(),
            symbol
        );
        let (code, construction): (StatusCode, Result<Data<Construction>, String>) =
            self.request(Method::GET, &path, None::<&()>).await;
        let construction = match code {
            StatusCode::OK => Some(construction.unwrap().data),
            StatusCode::NOT_FOUND => None,
            _ => panic!("Request failed: {} {} {}", code.as_u16(), Method::GET, path),
        };
        WithTimestamp::<Option<Construction>> {
            timestamp: chrono::Utc::now(),
            data: construction,
        }
    }

    pub async fn get_jumpgate_conns(&self, symbol: &WaypointSymbol) -> Vec<WaypointSymbol> {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        struct JumpGateResponse {
            symbol: WaypointSymbol,
            connections: Vec<WaypointSymbol>,
        }
        let path = format!(
            "/systems/{}/waypoints/{}/jump-gate",
            symbol.system(),
            symbol
        );
        let JumpGateResponse {
            symbol: _,
            connections,
        } = self.get::<Data<JumpGateResponse>>(&path).await.data;
        connections
    }

    pub async fn get_all_pages<T>(&self, path: &str) -> Vec<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut page = 1;
        let mut vec = Vec::new();
        loop {
            let response: PaginatedList<T> = self
                .get(&format!(
                    "{}?page={}&limit={}",
                    path, page, API_MAX_PAGE_SIZE
                ))
                .await;
            vec.extend(response.data);
            if response.meta.page * API_MAX_PAGE_SIZE >= response.meta.total {
                break;
            }
            page += 1;
        }
        vec
    }

    pub async fn get_final_paginated_entry<T>(&self, path: &str) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        // 1. Get first page with max page size
        let mut response: PaginatedList<T> = self
            .get(&format!("{}?page=1&limit={}", path, API_MAX_PAGE_SIZE))
            .await;
        let num_items = response.meta.total;
        if response.meta.total <= API_MAX_PAGE_SIZE {
            response.data.pop()
        } else {
            // 2. Get final page
            let mut response: PaginatedList<T> = self
                .get(&format!("{}?page={}&limit=1", path, num_items))
                .await;
            assert_eq!(response.meta.total, num_items);
            assert_eq!(response.data.len(), 1);
            response.data.pop()
        }
    }
}

/// Private methods

impl ApiClient {
    pub async fn get<T>(&self, path: &str) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        let (status, body_result) = self.request(Method::GET, path, None::<&()>).await;
        body_result.unwrap_or_else(|body| {
            panic!(
                "Request failed: {} {} {}\nbody: {}",
                status.as_u16(),
                Method::GET,
                path,
                body
            )
        })
    }

    pub async fn post<T, U>(&self, path: &str, json_body: &U) -> T
    where
        T: serde::de::DeserializeOwned,
        U: Serialize,
    {
        let (status, body_result) = self.request(Method::POST, path, Some(json_body)).await;
        body_result.unwrap_or_else(|body| {
            panic!(
                "Request failed: {} {} {}\nbody: {}",
                status.as_u16(),
                Method::POST,
                path,
                body
            )
        })
    }

    pub async fn patch<T, U>(&self, path: &str, json_body: &U) -> T
    where
        T: serde::de::DeserializeOwned,
        U: Serialize,
    {
        let (status, body_result) = self.request(Method::PATCH, path, Some(json_body)).await;
        body_result.unwrap_or_else(|body| {
            panic!(
                "Request failed: {} {} {}\nbody: {}",
                status.as_u16(),
                Method::PATCH,
                path,
                body
            )
        })
    }

    async fn wait_rate_limit(&self) {
        let now = Instant::now();
        let request_instant = {
            let mut next_request_ts = self.next_request_ts.lock().unwrap();
            let request_instant = match *next_request_ts {
                Some(ts) if ts > now => ts,
                _ => now,
            };
            *next_request_ts = Some(request_instant + std::time::Duration::from_millis(501));
            request_instant
        };
        let wait_duration = request_instant
            .checked_duration_since(now)
            .unwrap_or_default();
        if wait_duration >= std::time::Duration::from_secs(10) {
            warn!(
                "Rate limit queue exceeds 10 seconds: {:.3}s",
                wait_duration.as_secs_f64()
            );
        }
        tokio::time::sleep_until(request_instant).await;
    }

    pub async fn request<T, U>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<&U>,
    ) -> (StatusCode, Result<T, String>)
    where
        T: serde::de::DeserializeOwned,
        U: Serialize,
    {
        self.wait_rate_limit().await;
        let req_id = self.req_id.fetch_add(1, Ordering::Relaxed);
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.client.request(method.clone(), &url);
        if let Some(body) = json_body {
            request = request.json(body);
        }
        let request_body = match json_body {
            Some(body) => serde_json::to_string(body).unwrap(),
            None => String::new(),
        };
        // override auth type for /register
        if path == "/register" {
            let account_token = std::env::var("SPACETRADERS_ACCOUNT_TOKEN")
                .expect("SPACETRADERS_ACCOUNT_TOKEN env var must be set to register");
            request = request.header("Authorization", format!("Bearer {}", account_token));
        } else if let Some(token) = self.agent_token() {
            request = request.header("Authorization", format!("Bearer {}", token));
        }
        let response = request.send().await.expect("Failed to send request");
        let status = response.status();
        debug!("{} {} {}", status.as_u16(), method, path);
        let response_body = response.text().await.unwrap();

        // Call after_response interceptor
        for interceptor in self.interceptors.iter() {
            let slice_id = match self.slice_id() {
                Some(slice_id) => slice_id,
                None => {
                    warn!("Slice ID not set, skipping message");
                    continue;
                }
            };
            interceptor.after_response(
                &slice_id,
                req_id,
                &method,
                path,
                status,
                &request_body,
                &response_body,
            );
        }

        if status.is_success() {
            let content: T = serde_json::from_str(&response_body)
                .map_err(|e| {
                    error!(
                        "Unable to parse response as json: {}\nbody: {}",
                        e, response_body
                    );
                    panic!("Deserialisation failed");
                })
                .unwrap();
            (status, Ok(content))
        } else {
            (status, Err(response_body))
        }
    }
}
