//! MyBudget HTTP API.
//!
//! Routing is done by hand in a single fallback handler so that the error
//! precedence of SPEC 3.4 (405, then 404, then 400, then 409) is exact.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;

mod api;
mod error;
mod fields;
mod json;
mod model;

use error::{ApiError, ApiResult};
use model::Store;

/// Builds the application router.
pub fn app() -> Router {
    let state = Arc::new(Mutex::new(Store::new()));
    Router::new().fallback(dispatch).with_state(state)
}

type Shared = Arc<Mutex<Store>>;

/// Bodies over this many bytes get 413 (SPEC 3.13).
const MAX_BODY: usize = 1 << 20;
/// Past this many bytes, stop reading an oversized body.
const DRAIN_CAP: usize = 64 << 20;

async fn dispatch(State(state): State<Shared>, request: Request) -> Response {
    let (parts, mut body) = request.into_parts();
    // Read the whole body even past the limit (up to a cap), so a client
    // still writing it gets the 413 rather than a reset connection.
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
    match route(&state, &parts.method, parts.uri.path(), parts.uri.query(), &buf) {
        Ok(resp) => resp,
        Err(err) => err.into_response(),
    }
}

/// A path the API defines, with the ids it names.
enum Route<'a> {
    Health,
    Plans,
    Plan(&'a str),
    Accounts(&'a str),
    Account(&'a str, &'a str),
    Payees(&'a str),
    Payee(&'a str, &'a str),
    Categories(&'a str),
    Category(&'a str, &'a str),
    Groups(&'a str),
    Group(&'a str, &'a str),
    Transactions(&'a str),
    Transaction(&'a str, &'a str),
}

impl<'a> Route<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        let segments: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
        // Ids are never empty, so no defined path has an empty segment.
        if segments.iter().any(|s| s.is_empty()) {
            return None;
        }
        Some(match segments.as_slice() {
            ["health"] => Self::Health,
            ["plans"] => Self::Plans,
            ["plans", p] => Self::Plan(p),
            ["plans", p, "accounts"] => Self::Accounts(p),
            ["plans", p, "accounts", a] => Self::Account(p, a),
            ["plans", p, "payees"] => Self::Payees(p),
            ["plans", p, "payees", id] => Self::Payee(p, id),
            ["plans", p, "categories"] => Self::Categories(p),
            ["plans", p, "categories", c] => Self::Category(p, c),
            ["plans", p, "category_groups"] => Self::Groups(p),
            ["plans", p, "category_groups", g] => Self::Group(p, g),
            ["plans", p, "transactions"] => Self::Transactions(p),
            ["plans", p, "transactions", t] => Self::Transaction(p, t),
            _ => return None,
        })
    }

    /// The methods the path takes, as an `Allow` header. `HEAD` goes
    /// wherever `GET` does (SPEC 3.4).
    fn allow(&self) -> &'static str {
        match self {
            Self::Health => "GET, HEAD",
            Self::Plans | Self::Accounts(_) | Self::Payees(_) | Self::Categories(_) | Self::Transactions(_) => {
                "GET, HEAD, POST"
            }
            Self::Plan(_) | Self::Account(..) | Self::Payee(..) | Self::Group(..) | Self::Transaction(..) => {
                "GET, HEAD, PATCH, DELETE"
            }
            Self::Category(..) => "GET, HEAD, PATCH",
            Self::Groups(_) => "POST",
        }
    }
}

fn route(state: &Shared, method: &Method, path: &str, query: Option<&str>, body: &[u8]) -> ApiResult {
    let route = Route::parse(path).ok_or_else(ApiError::not_found)?;
    let allow = route.allow();
    if !allow.split(", ").any(|m| m == method.as_str()) {
        return Err(ApiError::method_not_allowed(allow));
    }
    // HEAD is answered like GET; hyper drops the body.
    let method = if method == Method::HEAD { "GET" } else { method.as_str() };
    if let Route::Health = route {
        return Ok("ok".into_response());
    }

    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    let store = &mut *guard;
    use api::{accounts, categories, payees, plans, transactions};
    use Route as R;
    match (route, method) {
        (R::Plans, "GET") => plans::list(store),
        (R::Plans, "POST") => plans::create(store, body),
        (R::Plan(p), "GET") => plans::get(store, p),
        (R::Plan(p), "PATCH") => plans::update(store, p, body),
        (R::Plan(p), "DELETE") => plans::delete(store, p),

        (R::Accounts(p), "GET") => accounts::list(store, p),
        (R::Accounts(p), "POST") => accounts::create(store, p, body),
        (R::Account(p, a), "GET") => accounts::get(store, p, a),
        (R::Account(p, a), "PATCH") => accounts::update(store, p, a, body),
        (R::Account(p, a), "DELETE") => accounts::delete(store, p, a),

        (R::Payees(p), "GET") => payees::list(store, p),
        (R::Payees(p), "POST") => payees::create(store, p, body),
        (R::Payee(p, id), "GET") => payees::get(store, p, id),
        (R::Payee(p, id), "PATCH") => payees::update(store, p, id, body),
        (R::Payee(p, id), "DELETE") => payees::delete(store, p, id),

        (R::Categories(p), "GET") => categories::list(store, p),
        (R::Categories(p), "POST") => categories::create(store, p, body),
        (R::Category(p, c), "GET") => categories::get(store, p, c),
        (R::Category(p, c), "PATCH") => categories::update(store, p, c, body),

        (R::Groups(p), "POST") => categories::create_group(store, p, body),
        (R::Group(p, g), "GET") => categories::get_group(store, p, g),
        (R::Group(p, g), "PATCH") => categories::update_group(store, p, g, body),
        (R::Group(p, g), "DELETE") => categories::delete_group(store, p, g),

        (R::Transactions(p), "GET") => transactions::list(store, p, query),
        (R::Transactions(p), "POST") => transactions::create(store, p, body),
        (R::Transaction(p, t), "GET") => transactions::get(store, p, t),
        (R::Transaction(p, t), "PATCH") => transactions::update(store, p, t, body),
        (R::Transaction(p, t), "DELETE") => transactions::delete(store, p, t),

        _ => unreachable!("allow() lists exactly the methods handled here"),
    }
}
