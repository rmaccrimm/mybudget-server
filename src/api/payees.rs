//! Payees (SPEC 6).

use serde_json::Value;

use crate::error::{ApiError, ApiResult, created, no_content, ok};
use crate::fields::{self, optional, parse_object, required};
use crate::model::{Payee, Plan, Store, plan, plan_mut};

/// Payees that aren't transfer payees have unique names (SPEC 6.2).
fn check_name_free(plan: &Plan, name: &str, except: Option<&str>) -> Result<(), ApiError> {
    match plan.find_payee_by_name(name) {
        Some(p) if Some(p.id.as_str()) != except => Err(ApiError::conflict("a payee with that name already exists")),
        _ => Ok(()),
    }
}

pub fn list(store: &Store, plan_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    ok(Value::Array(plan.payees.iter().map(Payee::to_json).collect()))
}

pub fn get(store: &Store, plan_id: &str, payee_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.payee_index(payee_id)?;
    ok(plan.payees[i].to_json())
}

pub fn create(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let name = fields::payee_name("name", required(&obj, "name")?)?;
    check_name_free(plan, &name, None)?;
    let payee = Payee { id: store.ids.next(), name, transfer_account_id: None };
    let out = payee.to_json();
    plan.payees.push(payee);
    created(out)
}

pub fn update(store: &mut Store, plan_id: &str, payee_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.payee_index(payee_id)?;
    let obj = parse_object(body)?;
    let name = optional(&obj, "name", fields::payee_name)?;
    if let Some(name) = name {
        if plan.payees[i].is_transfer() {
            return Err(ApiError::bad_request("a transfer payee's name follows its account"));
        }
        check_name_free(plan, &name, Some(payee_id))?;
        plan.payees[i].name = name;
    }
    ok(plan.payees[i].to_json())
}

pub fn delete(store: &mut Store, plan_id: &str, payee_id: &str) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.payee_index(payee_id)?;
    if plan.payees[i].is_transfer() {
        return Err(ApiError::bad_request("a transfer payee can't be deleted"));
    }
    if plan.payee_in_use(payee_id) {
        return Err(ApiError::conflict("the payee is used by transactions"));
    }
    plan.payees.remove(i);
    no_content()
}
