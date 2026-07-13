pub mod api_models;

use crate::models::*;
use crate::{api_client::api_models::RegisterResponse, config::CONFIG};
use core::panic;
use log::*;
use reqwest::{self, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::Instant;

const API_MAX_PAGE_SIZE: usize = 20;

tokio::task_local! {
    // When set (via `no_io_section`), naming the section, any HTTP request issued on
    // this task panics. Enforces that pure-compute paths that must read only cached
    // state — the jumpgate/warp graph builders — never fall through to the network.
    // A silent galaxy-wide fetch storm there once held the try_buy_ships lock past its
    // 30s fail-fast timeout and crash-looped the agent (twice); this turns that into an
    // instant, named panic at the offending call. See docs: universe-data / pathfinding.
    static NO_IO_SECTION: &'static str;
}

/// Run `fut` in a "no network I/O" section: any [`ApiClient`] request issued while it
/// runs (on this task) panics, naming `label`. Use to assert that pure-over-cache code
/// never fetches. Not inherited across `tokio::spawn` (task-locals don't propagate).
pub async fn no_io_section<F>(label: &'static str, fut: F) -> F::Output
where
    F: std::future::Future,
{
    NO_IO_SECTION.scope(label, fut).await
}

/// Panic if called inside a [`no_io_section`]. Invoked at the single request funnel so
/// every HTTP verb is covered. No-op outside a section.
fn guard_no_io(method: &Method, path: &str) {
    if let Ok(section) = NO_IO_SECTION.try_with(|s| *s) {
        panic!(
            "API {method} {path} attempted inside no-I/O section '{section}' — this path \
             must read only cached state (see api_client::no_io_section)"
        );
    }
}

#[derive(Clone)]
pub struct ApiClient {
    base_url: String,
    client: reqwest::Client,
    agent_token: Arc<RwLock<Option<String>>>,
    next_request_ts: Arc<Mutex<Option<Instant>>>,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    // Test-only client that skips CONFIG (which requires SPACETRADERS_API_URL) and is
    // never dialed. For offline tests of code that must not do I/O — the no_io_section
    // guard fires before any request would reach the (invalid) base_url.
    #[cfg(test)]
    pub(crate) fn for_test() -> ApiClient {
        ApiClient {
            client: reqwest::Client::new(),
            base_url: "http://test.invalid".to_string(),
            agent_token: Arc::new(RwLock::new(None)),
            next_request_ts: Arc::new(Mutex::new(None)),
        }
    }

    pub fn new() -> ApiClient {
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
            agent_token: Arc::new(RwLock::new(None)),
            next_request_ts: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_agent_token(&self, token: &str) {
        let mut agent_token = self.agent_token.write().unwrap();
        if agent_token.is_some() {
            panic!("Cannot set agent token while agent token is already set");
        }
        *agent_token = Some(token.to_string());
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

    // List a system's waypoints carrying a given trait (e.g. "MARKETPLACE",
    // "SHIPYARD"). The server filters on the *real* trait, so this matches waypoints
    // that are still UNCHARTED — unlike get_system_waypoints, whose objects hide the
    // trait behind UNCHARTED until a ship charts the waypoint. The returned objects
    // still show only the UNCHARTED trait; rely on membership in this list (not on the
    // per-object traits) to learn a waypoint is a market/shipyard.
    pub async fn get_system_waypoints_with_trait(
        &self,
        system_symbol: &SystemSymbol,
        trait_symbol: &str,
    ) -> Vec<api_models::WaypointDetailed> {
        self.get_all_pages(&format!(
            "/systems/{}/waypoints?traits={}",
            system_symbol, trait_symbol
        ))
        .await
    }

    // Returns None if the waypoint's market isn't accessible — i.e. it's still
    // UNCHARTED with no ship of ours present (API error 4001, HTTP 400). Callers that
    // discovered a market via a trait filter can hold is_market=true for such a
    // waypoint before it's ever been charted, so this must not panic.
    pub async fn get_market_remote(&self, symbol: &WaypointSymbol) -> Option<MarketRemoteView> {
        let path = format!("/systems/{}/waypoints/{}/market", symbol.system(), symbol);
        let (code, body): (StatusCode, Result<Data<MarketRemoteView>, String>) =
            self.request(Method::GET, &path, None::<&()>).await;
        match code {
            StatusCode::OK => Some(body.unwrap().data),
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => None,
            _ => panic!("Request failed: {} GET {}", code.as_u16(), path),
        }
    }

    // None if the shipyard isn't accessible (uncharted, no ship present); see
    // get_market_remote.
    pub async fn get_shipyard_remote(&self, symbol: &WaypointSymbol) -> Option<ShipyardRemoteView> {
        let path = format!("/systems/{}/waypoints/{}/shipyard", symbol.system(), symbol);
        let (code, body): (StatusCode, Result<Data<ShipyardRemoteView>, String>) =
            self.request(Method::GET, &path, None::<&()>).await;
        match code {
            StatusCode::OK => Some(body.unwrap().data),
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => None,
            _ => panic!("Request failed: {} GET {}", code.as_u16(), path),
        }
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
        // The path may already carry a query string (e.g. a `?traits=` filter), in
        // which case the pagination params must be appended with `&`, not `?`.
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut page = 1;
        let mut vec = Vec::new();
        loop {
            let response: PaginatedList<T> = self
                .get(&format!(
                    "{}{}page={}&limit={}",
                    path, sep, page, API_MAX_PAGE_SIZE
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

    // Chart the ship's current waypoint. Returns None on failure (e.g. the waypoint
    // was already charted by another agent) rather than panicking, so a charting
    // sweep can't crash the ship script.
    pub async fn chart_waypoint(
        &self,
        ship_symbol: &str,
    ) -> Option<api_models::ChartWaypointResponse> {
        let path = format!("/my/ships/{}/chart", ship_symbol);
        let (status, body): (
            StatusCode,
            Result<Data<api_models::ChartWaypointResponse>, String>,
        ) = self.request(Method::POST, &path, Some(&json!({}))).await;
        match body {
            Ok(data) => Some(data.data),
            Err(body) => {
                warn!(
                    "Chart {} failed ({}): {}",
                    ship_symbol,
                    status.as_u16(),
                    body
                );
                None
            }
        }
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

    pub async fn get_string(&self, path: &str) -> String {
        let (status, body_result) = self.request_string(Method::GET, path, None::<&()>).await;
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

    pub async fn post_string<U>(&self, path: &str, json_body: &U) -> String
    where
        U: Serialize,
    {
        let (status, body_result) = self
            .request_string(Method::POST, path, Some(json_body))
            .await;
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

    pub async fn patch_string<U>(&self, path: &str, json_body: &U) -> String
    where
        U: Serialize,
    {
        let (status, body_result) = self
            .request_string(Method::PATCH, path, Some(json_body))
            .await;
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

    pub async fn request_string_with_status<U>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<&U>,
    ) -> (StatusCode, Result<String, String>)
    where
        U: Serialize,
    {
        self.request_string(method, path, json_body).await
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
        let (status, result) = self.request_string(method, path, json_body).await;
        match result {
            Ok(content) => {
                let content: T = serde_json::from_str(&content)
                    .map_err(|e| {
                        error!("Unable to parse response as json: {}\nbody: {}", e, content);
                        panic!("Deserialisation failed");
                    })
                    .unwrap();
                (status, Ok(content))
            }
            Err(body) => (status, Err(body)),
        }
    }

    pub async fn request_string<U>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<&U>,
    ) -> (StatusCode, Result<String, String>)
    where
        U: Serialize,
    {
        guard_no_io(&method, path);
        self.wait_rate_limit().await;
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.client.request(method.clone(), &url);
        if let Some(body) = json_body {
            request = request.json(body);
        }
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

        if status.is_success() {
            (status, Ok(response_body))
        } else {
            (status, Err(response_body))
        }
    }
}

#[cfg(test)]
mod no_io_tests {
    use super::*;

    // Inside a no_io_section, the request funnel's guard panics (naming the section) —
    // before any network work. This is what makes a builder-does-I/O regression fail
    // loudly at the offending call instead of as a downstream lock-timeout.
    #[tokio::test]
    #[should_panic(expected = "no-I/O section 'build_jumpgate_graph'")]
    async fn guard_panics_inside_section() {
        no_io_section("build_jumpgate_graph", async {
            guard_no_io(
                &Method::GET,
                "/systems/X1-HN29/waypoints/X1-HN29-I60/construction",
            );
        })
        .await;
    }

    // Outside any section the guard is a no-op (normal requests proceed).
    #[test]
    fn guard_noop_outside_section() {
        guard_no_io(&Method::GET, "/my/ships");
    }

    // Sections don't leak across sibling tasks / after the scope ends.
    #[tokio::test]
    async fn guard_scope_is_bounded() {
        no_io_section("warp_jump_graph", async {}).await;
        guard_no_io(&Method::POST, "/my/ships"); // must not panic after scope exits
    }
}
