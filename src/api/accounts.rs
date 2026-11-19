//! Accounts (SPEC 5).

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Serialize;

use crate::AppState;
use crate::error::ApiError;
use crate::fields::{self, Body};
use crate::model::{AccountType, AccountView, ById, Transaction};

pub async fn list(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    Ok(Json(
        plan.accounts
            .keys()
            .map(|id| plan.account_view(id))
            .collect::<Vec<_>>(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, account_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    plan.accounts.by_id(&account_id)?;
    Ok(Json(plan.account_view(&account_id)))
}

pub async fn create(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let name = body.required("name", fields::name)?;
    let kind = body.required("type", fields::variant)?;
    let note = body.nullable("note", fields::note)?.or_null();
    let starting_balance = body
        .optional("starting_balance", fields::amount)?
        .unwrap_or(0);
    // Checked even when the starting balance is 0 and leaves it unused
    // (SPEC 3.6, 5.2).
    let starting_date = body.optional("starting_date", fields::date)?;
    let starting = match starting_date {
        _ if starting_balance == 0 => None,
        Some(date) => Some((starting_balance, date)),
        None => {
            return Err(ApiError::bad_request(
                "starting_date is required when starting_balance isn't 0",
            ));
        }
    };
    let view = store.change_plan(&plan_id, |plan| {
        let id = plan.create_account(name, kind, note, starting);
        plan.account_view(&id)
    })?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn update(
    State(state): State<AppState>,
    Path((plan_id, account_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let account = plan.accounts.by_id(&account_id)?;
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let note = body.nullable("note", fields::note)?;
    let closed = body.optional("closed", fields::boolean)?;
    if body
        .optional("type", fields::variant::<AccountType>)?
        .is_some_and(|kind| kind != account.kind)
    {
        return Err(ApiError::bad_request("an account's type can't change"));
    }
    if closed == Some(true) && !account.closed && plan.balances(&account_id).balance != 0 {
        return Err(ApiError::conflict(
            "an account can only be closed when its balance is 0",
        ));
    }

    let view = store.change_plan(&plan_id, |plan| {
        let account = plan.accounts.get_mut(&account_id).expect("found above");
        note.apply_to(&mut account.note);
        if let Some(closed) = closed {
            account.closed = closed;
        }
        if let Some(name) = name {
            plan.rename_account(&account_id, name);
        }
        plan.account_view(&account_id)
    })?;
    Ok(Json(view))
}

pub async fn delete(
    State(state): State<AppState>,
    Path((plan_id, account_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    plan.accounts.by_id(&account_id)?;
    if plan.account_has_transactions(&account_id) {
        return Err(ApiError::conflict("the account has transactions"));
    }
    store.change_plan(&plan_id, |plan| plan.delete_account(&account_id))?;
    Ok(StatusCode::NO_CONTENT)
}

/// What reconciling returns: the account and the adjustment, if any
/// (SPEC 14.1).
#[derive(Serialize)]
struct Reconciled {
    account: AccountView,
    adjustment: Option<Transaction>,
}

pub async fn reconcile(
    State(state): State<AppState>,
    Path((plan_id, account_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    plan.accounts.by_id(&account_id)?;
    let body = Body::parse(&body)?;
    let balance = body.required("balance", fields::amount)?;
    let date = body.required("date", fields::date)?;
    // Every transaction's amount stays an amount (SPEC 14.2).
    let adjustment = balance
        .checked_sub(plan.balances(&account_id).cleared_balance)
        .filter(|&a| fields::is_amount(a))
        .ok_or_else(|| {
            ApiError::conflict("the adjustment would be outside the range of amounts")
        })?;

    let reconciled = store.change_plan(&plan_id, |plan| {
        let adjustment_id = plan.reconcile(&account_id, adjustment, date);
        Reconciled {
            account: plan.account_view(&account_id),
            adjustment: adjustment_id.map(|id| plan.transactions[&id].clone()),
        }
    })?;
    Ok(Json(reconciled))
}
