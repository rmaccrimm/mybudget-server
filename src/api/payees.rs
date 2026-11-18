//! Payees (SPEC 6).

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
use crate::model::{ById, Payee, Plan, new_id};

/// Payees that aren't transfer payees have unique names (SPEC 6.2).
fn check_name_free(plan: &Plan, name: &str, except: Option<&str>) -> Result<(), ApiError> {
    match plan.find_payee_by_name(name) {
        Some(p) if Some(p.id.as_str()) != except => Err(ApiError::conflict("a payee with that name already exists")),
        _ => Ok(()),
    }
}

pub async fn list(State(state): State<AppState>, Path(plan_id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    Ok(Json(plan.payees.values().cloned().collect::<Vec<_>>()))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, payee_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    Ok(Json(store.plan(&plan_id)?.payees.by_id(&payee_id)?.clone()))
}

pub async fn create(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let name = body.required("name", fields::payee_name)?;
    check_name_free(plan, &name, None)?;
    let payee = Payee { id: new_id(), name, transfer_account_id: None };
    store.change_plan(&plan_id, |plan| plan.payees.insert(payee.id.clone(), payee.clone()))?;
    Ok((StatusCode::CREATED, Json(payee)))
}

pub async fn update(
    State(state): State<AppState>,
    Path((plan_id, payee_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let mut payee = plan.payees.by_id(&payee_id)?.clone();
    let body = Body::parse(&body)?;
    if let Some(name) = body.optional("name", fields::payee_name)? {
        if payee.is_transfer() {
            return Err(ApiError::bad_request("a transfer payee's name follows its account"));
        }
        check_name_free(plan, &name, Some(&payee_id))?;
        payee.name = name;
    }
    store.change_plan(&plan_id, |plan| plan.payees.insert(payee_id, payee.clone()))?;
    Ok(Json(payee))
}

pub async fn delete(
    State(state): State<AppState>,
    Path((plan_id, payee_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    if plan.payees.by_id(&payee_id)?.is_transfer() {
        return Err(ApiError::bad_request("a transfer payee can't be deleted"));
    }
    if plan.payee_in_use(&payee_id) {
        return Err(ApiError::conflict("the payee is used by transactions"));
    }
    store.change_plan(&plan_id, |plan| plan.payees.shift_remove(&payee_id))?;
    Ok(StatusCode::NO_CONTENT)
}
