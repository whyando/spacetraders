use reqwest::{Method, StatusCode};

/// Trait for intercepting API responses
pub trait ApiInterceptor: Send + Sync {
    /// Called after receiving an API response
    fn after_response(
        &self,
        slice_id: &str,
        req_id: u64,
        method: &Method,
        path: &str,
        status: StatusCode,
        request_body: &str,
        response_body: &str,
    );
}
