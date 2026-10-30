//! MyBudget HTTP API.

use axum::{Router, routing::get};

/// Builds the application router.
pub fn app() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}
