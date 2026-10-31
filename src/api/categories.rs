//! Category groups and categories (SPEC 7).

use serde_json::Value;

use crate::error::{ApiError, ApiResult, created, no_content, ok};
use crate::fields::{self, optional, parse_object, required};
use crate::model::{Category, Group, Plan, Store, plan, plan_mut};

/// A body's `group_id` must be one of the plan's own groups (SPEC 7.5, 7.6).
fn own_group(plan: &Plan, id: &str) -> Result<usize, ApiError> {
    plan.own_group_index(id)
        .ok_or_else(|| ApiError::bad_request("group_id must be one of the plan's own category groups"))
}

pub fn list(store: &Store, plan_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    ok(Value::Array(plan.groups.iter().map(|g| plan.group_json(g)).collect()))
}

// --- groups ----------------------------------------------------------------

pub fn create_group(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let name = fields::name("name", required(&obj, "name")?)?;
    let group = Group { id: store.ids.next(), name, hidden: false, internal: false, category_ids: Vec::new() };
    let out = plan.group_json(&group);
    plan.groups.push(group);
    created(out)
}

pub fn get_group(store: &Store, plan_id: &str, group_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.group_index(group_id)?;
    ok(plan.group_json(&plan.groups[i]))
}

pub fn update_group(store: &mut Store, plan_id: &str, group_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.group_index(group_id)?;
    let obj = parse_object(body)?;
    let name = optional(&obj, "name", fields::name)?;
    let hidden = optional(&obj, "hidden", fields::boolean)?;
    let group = &mut plan.groups[i];
    if group.internal && (name.is_some() || hidden.is_some()) {
        return Err(ApiError::bad_request("the Credit Card Payments group can't be changed"));
    }
    if let Some(name) = name {
        group.name = name;
    }
    if let Some(hidden) = hidden {
        group.hidden = hidden;
    }
    ok(plan.group_json(&plan.groups[i]))
}

pub fn delete_group(store: &mut Store, plan_id: &str, group_id: &str) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.group_index(group_id)?;
    let group = &plan.groups[i];
    if group.internal {
        return Err(ApiError::bad_request("the Credit Card Payments group can't be deleted"));
    }
    if !group.category_ids.is_empty() {
        return Err(ApiError::conflict("the group has categories"));
    }
    plan.groups.remove(i);
    no_content()
}

// --- categories --------------------------------------------------------------

pub fn create(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let group = own_group(plan, fields::string("group_id", required(&obj, "group_id")?)?)?;
    let name = fields::name("name", required(&obj, "name")?)?;
    let note = optional(&obj, "note", fields::note)?.flatten();
    let category = Category {
        id: store.ids.next(),
        group_id: plan.groups[group].id.clone(),
        name,
        hidden: false,
        note,
        payment_account_id: None,
    };
    let out = category.to_json();
    plan.groups[group].category_ids.push(category.id.clone());
    plan.categories.push(category);
    created(out)
}

pub fn get(store: &Store, plan_id: &str, category_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.category_index(category_id)?;
    ok(plan.categories[i].to_json())
}

pub fn update(store: &mut Store, plan_id: &str, category_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.category_index(category_id)?;
    let obj = parse_object(body)?;
    let name = optional(&obj, "name", fields::name)?;
    let note = optional(&obj, "note", fields::note)?;
    let hidden = optional(&obj, "hidden", fields::boolean)?;
    let group_id = optional(&obj, "group_id", fields::string)?;
    let category = &plan.categories[i];
    if category.payment_account_id.is_some() && (name.is_some() || group_id.is_some()) {
        return Err(ApiError::bad_request("a payment category's name and group follow its account"));
    }
    // Naming the category's own group doesn't move it.
    let move_to = match group_id {
        Some(id) if id != category.group_id => Some(own_group(plan, id)?),
        _ => None,
    };

    if let Some(to) = move_to {
        plan.move_category(i, to);
    }
    let category = &mut plan.categories[i];
    if let Some(name) = name {
        category.name = name;
    }
    if let Some(note) = note {
        category.note = note;
    }
    if let Some(hidden) = hidden {
        category.hidden = hidden;
    }
    ok(category.to_json())
}
