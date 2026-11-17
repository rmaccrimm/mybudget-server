//! MyBudget HTTP API.
//!
//! axum answers the first two errors of SPEC 3.4 itself: a path that isn't
//! routed gets the JSON 404 from the fallback, and a routed path with
//! another method gets the JSON 405, with `Allow`, from the method-not-allowed
//! fallback. Handlers then look up the path's objects (404) before reading
//! the body (400) and checking conflicts (409).

use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    Router,
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;

mod api;
mod budget;
mod calendar;
mod error;
mod fields;
mod json;
mod model;

use api::{accounts, categories, months, payees, plans, transactions};
use error::ApiError;
use model::Store;

/// The state handlers share: the store, behind one lock.
#[derive(Clone, Default)]
pub struct AppState(Arc<Mutex<Store>>);

impl AppState {
    /// Locks the store for the rest of the request, so requests behave as if
    /// handled one at a time (SPEC 3.11).
    fn store(&self) -> MutexGuard<'_, Store> {
        // A handler that panicked may have left a change half-applied, so
        // the data can't be trusted after that.
        self.0.lock().expect("no handler panicked while holding the store")
    }
}

/// Builds the application router.
pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/plans", get(plans::list).post(plans::create))
        .route("/plans/{plan}", get(plans::get).patch(plans::update).delete(plans::delete))
        .route("/plans/{plan}/accounts", get(accounts::list).post(accounts::create))
        .route("/plans/{plan}/accounts/{account}", get(accounts::get).patch(accounts::update).delete(accounts::delete))
        .route("/plans/{plan}/payees", get(payees::list).post(payees::create))
        .route("/plans/{plan}/payees/{payee}", get(payees::get).patch(payees::update).delete(payees::delete))
        .route("/plans/{plan}/categories", get(categories::list).post(categories::create))
        .route(
            "/plans/{plan}/categories/{category}",
            get(categories::get).patch(categories::update).delete(categories::delete),
        )
        .route("/plans/{plan}/category_groups", post(categories::create_group))
        .route(
            "/plans/{plan}/category_groups/{group}",
            get(categories::get_group).patch(categories::update_group).delete(categories::delete_group),
        )
        .route("/plans/{plan}/transactions", get(transactions::list).post(transactions::create))
        .route(
            "/plans/{plan}/transactions/{transaction}",
            get(transactions::get).patch(transactions::update).delete(transactions::delete),
        )
        .route("/plans/{plan}/months/{month}", get(months::get))
        .route(
            "/plans/{plan}/months/{month}/categories/{category}",
            get(months::get_category).patch(months::update_category),
        )
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(read_body))
        .with_state(AppState::default())
}

async fn health() -> &'static str {
    "ok"
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

/// Bodies over this many bytes get 413 (SPEC 3.13).
const MAX_BODY: usize = 1 << 20;
/// Past this many bytes, stop reading an oversized body.
const DRAIN_CAP: usize = 64 << 20;

/// Reads the whole body before any route or fallback runs, and answers 413
/// to one over [`MAX_BODY`] whatever else is wrong with the request
/// (SPEC 3.13).
///
/// Hand-rolled because axum's `DefaultBodyLimit` stops reading at the limit
/// and the client still writing the body gets a reset connection instead of
/// the 413. This keeps reading (up to [`DRAIN_CAP`]) and throws the excess
/// away.
async fn read_body(request: Request, next: Next) -> Response {
    let (parts, mut body) = request.into_parts();
    let mut buf = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        if let Ok(data) = frame.into_data() {
            total += data.len();
            if total <= MAX_BODY {
                buf.extend_from_slice(&data);
            } else if total > DRAIN_CAP {
                break;
            }
        }
    }
    if total > MAX_BODY {
        return ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    next.run(Request::from_parts(parts, Body::from(buf))).await
}
