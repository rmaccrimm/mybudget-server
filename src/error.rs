//! Error responses (SPEC 3.2, 3.4).

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// An error response: a status and a JSON `{"error": ...}` body.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    allow: Option<&'static str>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), allow: None }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn method_not_allowed(allow: &'static str) -> Self {
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

pub fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).expect("JSON values always serialise");
    (status, [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))], body).into_response()
}

pub type ApiResult = Result<Response, ApiError>;

pub fn ok(value: Value) -> ApiResult {
    Ok(json_response(StatusCode::OK, &value))
}

pub fn created(value: Value) -> ApiResult {
    Ok(json_response(StatusCode::CREATED, &value))
}

pub fn no_content() -> ApiResult {
    Ok(StatusCode::NO_CONTENT.into_response())
}
