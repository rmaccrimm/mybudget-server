//! MyBudget HTTP API.
//!
//! Routing is done by hand in a single fallback handler so that the error
//! precedence of SPEC 3.4 (405, then 404, then 400, then 409) is exact.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

/// Builds the application router.
pub fn app() -> Router {
    let state = Arc::new(Mutex::new(Store::new()));
    Router::new().fallback(dispatch).with_state(state)
}

type Shared = Arc<Mutex<Store>>;

// ---------------------------------------------------------------------------
// Responses

/// An error response: status plus a JSON `{"error": ...}` body (SPEC 3.2).
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    allow: Option<&'static str>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), allow: None }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }
    fn method_not_allowed(allow: &'static str) -> Self {
        Self { status: StatusCode::METHOD_NOT_ALLOWED, message: "method not allowed".into(), allow: Some(allow) }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = json_response(self.status, &json!({ "error": self.message }));
        if let Some(allow) = self.allow {
            resp.headers_mut().insert(header::ALLOW, HeaderValue::from_static(allow));
        }
        resp
    }
}

fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).expect("JSON values always serialise");
    (status, [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))], body).into_response()
}

type ApiResult = Result<Response, ApiError>;

fn ok(value: Value) -> ApiResult {
    Ok(json_response(StatusCode::OK, &value))
}
fn created(value: Value) -> ApiResult {
    Ok(json_response(StatusCode::CREATED, &value))
}
fn no_content() -> ApiResult {
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Request bodies

/// Parses a POST/PATCH body as a JSON object (SPEC 3.1, 3.4).
fn parse_object(body: &[u8]) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(ApiError::bad_request("body must be a JSON object")),
        Err(e) => Err(ApiError::bad_request(format!("invalid JSON: {e}"))),
    }
}

/// A name per SPEC 3.9: 1..=max scalar values, not all whitespace.
fn validate_name(field: &str, value: &Value, max: usize) -> Result<String, ApiError> {
    let Value::String(s) = value else {
        return Err(ApiError::bad_request(format!("{field} must be a string")));
    };
    let len = s.chars().count();
    if len == 0 || len > max {
        return Err(ApiError::bad_request(format!("{field} must have 1 to {max} characters")));
    }
    if s.chars().all(char::is_whitespace) {
        return Err(ApiError::bad_request(format!("{field} must not be blank")));
    }
    Ok(s.clone())
}

fn validate_currency(value: &Value) -> Result<String, ApiError> {
    match value {
        Value::String(s) if s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase()) => Ok(s.clone()),
        _ => Err(ApiError::bad_request("currency_code must be three uppercase ASCII letters")),
    }
}

// ---------------------------------------------------------------------------
// Store

struct Plan {
    id: String,
    name: String,
    currency_code: String,
}

impl Plan {
    fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "currency_code": self.currency_code })
    }
}

struct Store {
    id_prefix: String,
    next_id: u64,
    /// Oldest first.
    plans: Vec<Plan>,
}

impl Store {
    fn new() -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        Self { id_prefix: format!("{:x}", nanos as u64), next_id: 1, plans: Vec::new() }
    }

    /// Ids are unique across the server and never reused (SPEC 3.5).
    fn new_id(&mut self) -> String {
        let id = format!("{}-{}", self.id_prefix, self.next_id);
        self.next_id += 1;
        id
    }

    fn plan_index(&self, id: &str) -> Result<usize, ApiError> {
        self.plans.iter().position(|p| p.id == id).ok_or_else(ApiError::not_found)
    }
}

// ---------------------------------------------------------------------------
// Routing

async fn dispatch(State(state): State<Shared>, method: Method, uri: Uri, body: Bytes) -> Response {
    match route(&state, &method, uri.path(), &body) {
        Ok(resp) | Err(resp) => resp,
    }
}

fn route(state: &Shared, method: &Method, path: &str, body: &[u8]) -> Result<Response, Response> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    // HEAD is answered like GET; hyper drops the body.
    let method = if method == Method::HEAD { &Method::GET } else { method };

    let result = match segments.as_slice() {
        ["health"] => match *method {
            Method::GET => return Ok("ok".into_response()),
            _ => Err(ApiError::method_not_allowed("GET, HEAD")),
        },
        ["plans"] => match *method {
            Method::GET => list_plans(state),
            Method::POST => create_plan(state, body),
            _ => Err(ApiError::method_not_allowed("GET, HEAD, POST")),
        },
        ["plans", id] if !id.is_empty() => match *method {
            Method::GET => get_plan(state, id),
            Method::PATCH => update_plan(state, id, body),
            Method::DELETE => delete_plan(state, id),
            _ => Err(ApiError::method_not_allowed("GET, HEAD, PATCH, DELETE")),
        },
        _ => Err(ApiError::not_found()),
    };
    result.map_err(IntoResponse::into_response)
}

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, Store> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Plans (SPEC 4)

fn list_plans(state: &Shared) -> ApiResult {
    let store = lock(state);
    ok(Value::Array(store.plans.iter().map(Plan::to_json).collect()))
}

fn create_plan(state: &Shared, body: &[u8]) -> ApiResult {
    let obj = parse_object(body)?;
    let name = match obj.get("name") {
        Some(v) => validate_name("name", v, 100)?,
        None => return Err(ApiError::bad_request("name is required")),
    };
    let currency_code = match obj.get("currency_code") {
        Some(v) => validate_currency(v)?,
        None => "USD".to_string(),
    };
    let mut store = lock(state);
    let plan = Plan { id: store.new_id(), name, currency_code };
    let out = plan.to_json();
    store.plans.push(plan);
    created(out)
}

fn get_plan(state: &Shared, id: &str) -> ApiResult {
    let store = lock(state);
    let i = store.plan_index(id)?;
    ok(store.plans[i].to_json())
}

fn update_plan(state: &Shared, id: &str, body: &[u8]) -> ApiResult {
    let mut store = lock(state);
    let i = store.plan_index(id)?;
    let obj = parse_object(body)?;
    let name = obj.get("name").map(|v| validate_name("name", v, 100)).transpose()?;
    let currency_code = obj.get("currency_code").map(validate_currency).transpose()?;
    let plan = &mut store.plans[i];
    if let Some(name) = name {
        plan.name = name;
    }
    if let Some(code) = currency_code {
        plan.currency_code = code;
    }
    ok(plan.to_json())
}

fn delete_plan(state: &Shared, id: &str) -> ApiResult {
    let mut store = lock(state);
    let i = store.plan_index(id)?;
    store.plans.remove(i);
    no_content()
}
