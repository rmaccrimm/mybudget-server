//! Accounts (SPEC 5).

use serde_json::Value;

use crate::error::{ApiError, ApiResult, created, no_content, ok};
use crate::fields::{self, optional, parse_object, required};
use crate::json::Json;
use crate::model::{AccountType, Store, plan, plan_mut};

fn account_type(field: &str, v: &Json) -> Result<AccountType, ApiError> {
    match v {
        Json::String(s) => AccountType::parse(s),
        _ => None,
    }
    .ok_or_else(|| {
        ApiError::bad_request(format!(
            "{field} must be one of checking, savings, cash, creditCard, lineOfCredit, otherAsset, otherLiability"
        ))
    })
}

pub fn list(store: &Store, plan_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    ok(Value::Array(plan.accounts.iter().map(|a| plan.account_json(a)).collect()))
}

pub fn get(store: &Store, plan_id: &str, account_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.account_index(account_id)?;
    ok(plan.account_json(&plan.accounts[i]))
}

pub fn create(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let name = fields::name("name", required(&obj, "name")?)?;
    let kind = account_type("type", required(&obj, "type")?)?;
    let note = optional(&obj, "note", fields::note)?.flatten();
    let starting_balance = optional(&obj, "starting_balance", fields::amount)?.unwrap_or(0);
    // Checked even when the starting balance is 0 and leaves it unused
    // (SPEC 3.6, 5.2).
    let starting_date = optional(&obj, "starting_date", fields::date)?;
    let starting = match starting_date {
        _ if starting_balance == 0 => None,
        Some(date) => Some((starting_balance, date)),
        None => return Err(ApiError::bad_request("starting_date is required when starting_balance isn't 0")),
    };
    let i = plan.create_account(&mut store.ids, name, kind, note, starting);
    created(plan.account_json(&plan.accounts[i]))
}

pub fn update(store: &mut Store, plan_id: &str, account_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.account_index(account_id)?;
    let obj = parse_object(body)?;
    let name = optional(&obj, "name", fields::name)?;
    let note = optional(&obj, "note", fields::note)?;
    let closed = optional(&obj, "closed", fields::boolean)?;
    let account = &plan.accounts[i];
    if let Some(t) = obj.get("type")
        && !matches!(t, Json::String(s) if s == account.kind.as_str())
    {
        return Err(ApiError::bad_request("an account's type can't change"));
    }
    if closed == Some(true) && !account.closed && plan.balances(account_id).balance != 0 {
        return Err(ApiError::conflict("an account can only be closed when its balance is 0"));
    }

    if let Some(name) = name {
        plan.rename_account(i, name);
    }
    let account = &mut plan.accounts[i];
    if let Some(note) = note {
        account.note = note;
    }
    if let Some(closed) = closed {
        account.closed = closed;
    }
    ok(plan.account_json(&plan.accounts[i]))
}

pub fn delete(store: &mut Store, plan_id: &str, account_id: &str) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.account_index(account_id)?;
    if plan.account_has_transactions(account_id) {
        return Err(ApiError::conflict("the account has transactions"));
    }
    plan.delete_account(i);
    no_content()
}
