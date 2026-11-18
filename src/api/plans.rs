//! Plans (SPEC 4).

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::AppState;
use crate::error::ApiError;
use crate::fields::{self, Body};
use crate::model::PlanInfo;

pub async fn list(State(state): State<AppState>) -> Json<Vec<PlanInfo>> {
    let store = state.store();
    Json(store.plans().map(|p| p.info.clone()).collect())
}

pub async fn create(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let body = Body::parse(&body)?;
    let name = body.required("name", fields::name)?;
    let currency_code = body
        .optional("currency_code", fields::currency)?
        .unwrap_or_else(|| "USD".into());
    let plan = store.create_plan(name, currency_code)?;
    Ok((StatusCode::CREATED, Json(plan.info.clone())))
}

pub async fn get(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    Ok(Json(store.plan(&plan_id)?.info.clone()))
}

pub async fn update(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let currency_code = body.optional("currency_code", fields::currency)?;
    let info = store.change_plan(&plan_id, |plan| {
        if let Some(name) = name {
            plan.info.name = name;
        }
        if let Some(code) = currency_code {
            plan.info.currency_code = code;
        }
        plan.info.clone()
    })?;
    Ok(Json(info))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.store().delete_plan(&plan_id)?;
    Ok(StatusCode::NO_CONTENT)
}
