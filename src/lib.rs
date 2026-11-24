//! `MyBudget` HTTP API.
//!
//! axum answers the first two errors of SPEC 3.4 itself: a path that isn't
//! routed gets the JSON 404 from the fallback, and a routed path with
//! another method gets the JSON 405, with `Allow`, from the method-not-allowed
//! fallback. Handlers then look up the path's objects (404) before reading
//! the body (400) and checking conflicts (409).
#![warn(clippy::all, clippy::pedantic)]
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::similar_names,
    clippy::missing_safety_doc
)]

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
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
mod import;
mod json;
mod model;
mod ofx;
mod persistence;
mod store;

use api::{accounts, categories, imports, months, payees, plans, transactions};
pub use calendar::today;
use error::ApiError;
pub use persistence::migrate;
pub use store::Store;

/// Writes a line to stderr, for whoever runs the server. Unlike `eprintln!`,
/// it doesn't panic if stderr is closed: a parent that doesn't read stderr
/// mustn't stop the server from starting (SPEC 15.7) or answering (15.10).
fn log(line: std::fmt::Arguments) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// How long [`AppState::exit`] waits for a request to finish with the store,
/// well inside SPEC 1.4's one second.
const EXIT_PATIENCE: Duration = Duration::from_millis(500);

/// The state handlers share: the store, behind one lock, and today's date,
/// which is fixed while the server runs (SPEC 18.1).
#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
    today: Arc<str>,
}

impl AppState {
    /// `today` is a date `YYYY-MM-DD`, as [`today`] gives it.
    pub fn new(store: Store, today: &str) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            today: today.into(),
        }
    }

    /// Locks the store for the rest of the request, so requests behave as if
    /// handled one at a time (SPEC 3.11).
    fn store(&self) -> MutexGuard<'_, Store> {
        // A handler that panicked may have left a change half-applied, so
        // the data can't be trusted after that.
        self.store
            .lock()
            .expect("no handler panicked while holding the store")
    }

    /// Today's date (SPEC 18.1).
    fn today(&self) -> &str {
        &self.today
    }

    /// Exits the process with status 0 once no request holds the store, so
    /// that exiting doesn't cut a save short and leave a journal beside its
    /// file (SPEC 15.6), but within [`EXIT_PATIENCE`] even if a request still
    /// holds it, since the server must exit within a second (SPEC 1.4). A
    /// save cut short is no worse than the `SIGKILL` that SPEC 15.5 already
    /// covers: its request got no response, and `SQLite` rolls its
    /// transaction back the next time the file is opened.
    pub fn exit(&self) -> ! {
        let deadline = Instant::now() + EXIT_PATIENCE;
        // Held until the process ends; a poisoned lock is held all the same.
        let _store = loop {
            match self.store.try_lock() {
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                held => break held,
            }
        };
        std::process::exit(0)
    }
}

/// Builds the application router.
pub fn app(state: AppState) -> Router {
    // The import endpoints take larger bodies than the rest (SPEC 3.13), so
    // they get their own body limit, and are merged in after the rest get
    // theirs. Their extractors' own limit, 2 MiB by default, is raised to
    // match.
    let imports = Router::new()
        .route("/plans/{plan}/imports", post(imports::import))
        .route("/plans/{plan}/imports/preview", post(imports::preview))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_IMPORT_BODY))
        .layer(middleware::from_fn_with_state(MAX_IMPORT_BODY, read_body));
    Router::new()
        .route("/health", get(health))
        .route("/plans", get(plans::list).post(plans::create))
        .route(
            "/plans/{plan}",
            get(plans::get).patch(plans::update).delete(plans::delete),
        )
        .route(
            "/plans/{plan}/accounts",
            get(accounts::list).post(accounts::create),
        )
        .route(
            "/plans/{plan}/accounts/{account}",
            get(accounts::get)
                .patch(accounts::update)
                .delete(accounts::delete),
        )
        .route(
            "/plans/{plan}/accounts/{account}/reconcile",
            post(accounts::reconcile),
        )
        .route(
            "/plans/{plan}/payees",
            get(payees::list).post(payees::create),
        )
        .route(
            "/plans/{plan}/payees/{payee}",
            get(payees::get)
                .patch(payees::update)
                .delete(payees::delete),
        )
        .route(
            "/plans/{plan}/categories",
            get(categories::list).post(categories::create),
        )
        .route(
            "/plans/{plan}/categories/{category}",
            get(categories::get)
                .patch(categories::update)
                .delete(categories::delete),
        )
        .route(
            "/plans/{plan}/category_groups",
            post(categories::create_group),
        )
        .route(
            "/plans/{plan}/category_groups/{group}",
            get(categories::get_group)
                .patch(categories::update_group)
                .delete(categories::delete_group),
        )
        .route(
            "/plans/{plan}/transactions",
            get(transactions::list).post(transactions::create),
        )
        .route(
            "/plans/{plan}/transactions/{transaction}",
            get(transactions::get)
                .patch(transactions::update)
                .delete(transactions::delete),
        )
        .route(
            "/plans/{plan}/transactions/{transaction}/unmatch",
            post(transactions::unmatch),
        )
        .route("/plans/{plan}/months/{month}", get(months::get))
        .route(
            "/plans/{plan}/months/{month}/categories/{category}",
            get(months::get_category).patch(months::update_category),
        )
        .route(
            "/plans/{plan}/months/{month}/categories/{category}/assignments",
            get(months::assignments),
        )
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(MAX_BODY, read_body))
        .merge(imports)
        .with_state(state)
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
/// The import endpoints' larger limit (SPEC 3.13, 17.1).
const MAX_IMPORT_BODY: usize = 16 << 20;
/// Past this many bytes, stop reading an oversized body.
const DRAIN_CAP: usize = 64 << 20;

/// Reads the whole body before any route or fallback runs, and answers 413
/// to one over `limit` bytes whatever else is wrong with the request
/// (SPEC 3.13).
///
/// Hand-rolled because axum's `DefaultBodyLimit` stops reading at the limit
/// and the client still writing the body gets a reset connection instead of
/// the 413. This keeps reading (up to [`DRAIN_CAP`]) and throws the excess
/// away.
async fn read_body(State(limit): State<usize>, request: Request, next: Next) -> Response {
    let (parts, mut body) = request.into_parts();
    let mut buf = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        if let Ok(data) = frame.into_data() {
            total += data.len();
            if total <= limit {
                buf.extend_from_slice(&data);
            } else if total > DRAIN_CAP {
                break;
            }
        }
    }
    if total > limit {
        return ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
    }
    next.run(Request::from_parts(parts, Body::from(buf))).await
}
