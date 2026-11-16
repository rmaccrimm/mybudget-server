//! Months and category month entries (SPEC 11).

use crate::budget::{Budget, month_index};
use crate::error::{ApiError, ApiResult, ok};
use crate::fields::{self, optional, parse_object};
use crate::model::{Store, plan, plan_mut};

/// A month in a path; anything else is 400 (SPEC 3.8).
fn month(s: &str) -> Result<i32, ApiError> {
    month_index(s).ok_or_else(|| ApiError::bad_request("the month must be YYYY-MM from 1900-01 to 2999-12"))
}

pub fn get(store: &Store, plan_id: &str, month_str: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let m = month(month_str)?;
    ok(Budget::new(plan).month_json(m))
}

pub fn get_category(store: &Store, plan_id: &str, month_str: &str, category_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.category_index(category_id)?;
    let m = month(month_str)?;
    ok(Budget::new(plan).entry_json(&plan.categories[i].id, m))
}

pub fn update_category(
    store: &mut Store,
    plan_id: &str,
    month_str: &str,
    category_id: &str,
    body: &[u8],
) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.category_index(category_id)?;
    let m = month(month_str)?;
    let obj = parse_object(body)?;
    let assigned = optional(&obj, "assigned", fields::amount)?;
    let id = plan.categories[i].id.clone();
    if let Some(a) = assigned {
        plan.assigned.entry(id.clone()).or_default().insert(m, a);
    }
    ok(Budget::new(plan).entry_json(&id, m))
}
