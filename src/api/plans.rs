//! Plans (SPEC 4).

use serde_json::Value;

use crate::error::{ApiResult, created, no_content, ok};
use crate::fields::{self, optional, parse_object, required};
use crate::model::{Plan, Store, plan, plan_index, plan_mut};

pub fn list(store: &Store) -> ApiResult {
    ok(Value::Array(store.plans.iter().map(Plan::to_json).collect()))
}

pub fn create(store: &mut Store, body: &[u8]) -> ApiResult {
    let obj = parse_object(body)?;
    let name = fields::name("name", required(&obj, "name")?)?;
    let currency_code = optional(&obj, "currency_code", fields::currency)?.unwrap_or_else(|| "USD".into());
    let plan = Plan::new(&mut store.ids, name, currency_code);
    let out = plan.to_json();
    store.plans.push(plan);
    created(out)
}

pub fn get(store: &Store, plan_id: &str) -> ApiResult {
    ok(plan(&store.plans, plan_id)?.to_json())
}

pub fn update(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let name = optional(&obj, "name", fields::name)?;
    let currency_code = optional(&obj, "currency_code", fields::currency)?;
    if let Some(name) = name {
        plan.name = name;
    }
    if let Some(code) = currency_code {
        plan.currency_code = code;
    }
    ok(plan.to_json())
}

pub fn delete(store: &mut Store, plan_id: &str) -> ApiResult {
    let i = plan_index(&store.plans, plan_id)?;
    store.plans.remove(i);
    no_content()
}
