//! Months and category month entries (SPEC 11).

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    response::IntoResponse,
};

use crate::AppState;
use crate::budget::Budget;
use crate::calendar::Month;
use crate::error::ApiError;
use crate::fields::{self, Body};
use crate::model::ById;

/// A month in a path; anything else is 400 (SPEC 3.8).
fn month(s: &str) -> Result<Month, ApiError> {
    s.parse().map_err(|_| ApiError::bad_request("the month must be YYYY-MM from 1900-01 to 2999-12"))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, month_str)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    let m = month(&month_str)?;
    Ok(Json(Budget::new(plan).month(m)))
}

pub async fn get_category(
    State(state): State<AppState>,
    Path((plan_id, month_str, category_id)): Path<(String, String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    plan.categories.by_id(&category_id)?;
    let m = month(&month_str)?;
    Ok(Json(Budget::new(plan).entry(&category_id, m)))
}

pub async fn update_category(
    State(state): State<AppState>,
    Path((plan_id, month_str, category_id)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    store.plan(&plan_id)?.categories.by_id(&category_id)?;
    let m = month(&month_str)?;
    let body = Body::parse(&body)?;
    let assigned = body.optional("assigned", fields::amount)?;
    let entry = store.change_plan(&plan_id, |plan| {
        if let Some(assigned) = assigned {
            plan.assigned.entry(category_id.clone()).or_default().insert(m, assigned);
        }
        Budget::new(plan).entry(&category_id, m)
    })?;
    Ok(Json(entry))
}
