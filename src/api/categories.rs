//! Category groups and categories (SPEC 7).

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
use crate::model::{ById, Category, Group, Plan, new_id};

/// A body's `group_id` must be one of the plan's own groups (SPEC 7.5, 7.6).
fn own_group<'a>(plan: &'a Plan, id: &str) -> Result<&'a Group, ApiError> {
    plan.groups
        .get(id)
        .filter(|g| !g.internal)
        .ok_or_else(|| ApiError::bad_request("group_id must be one of the plan's own category groups"))
}

pub async fn list(State(state): State<AppState>, Path(plan_id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    Ok(Json(plan.groups.values().map(|g| plan.group_view(g)).collect::<Vec<_>>()))
}

// --- groups ----------------------------------------------------------------

pub async fn create_group(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan_mut(&plan_id)?;
    let body = Body::parse(&body)?;
    let name = body.required("name", fields::name)?;
    let group = Group { id: new_id(), name, hidden: false, internal: false };
    plan.groups.insert(group.id.clone(), group.clone());
    Ok((StatusCode::CREATED, Json(plan.group_view(&group))))
}

pub async fn get_group(
    State(state): State<AppState>,
    Path((plan_id, group_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    Ok(Json(plan.group_view(plan.groups.by_id(&group_id)?)))
}

pub async fn update_group(
    State(state): State<AppState>,
    Path((plan_id, group_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan_mut(&plan_id)?;
    let mut group = plan.groups.by_id(&group_id)?.clone();
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let hidden = body.optional("hidden", fields::boolean)?;
    if group.internal && (name.is_some() || hidden.is_some()) {
        return Err(ApiError::bad_request("the Credit Card Payments group can't be changed"));
    }
    if let Some(name) = name {
        group.name = name;
    }
    if let Some(hidden) = hidden {
        group.hidden = hidden;
    }
    let view = plan.group_view(&group);
    plan.groups.insert(group_id, group);
    Ok(Json(view))
}

pub async fn delete_group(
    State(state): State<AppState>,
    Path((plan_id, group_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan_mut(&plan_id)?;
    if plan.groups.by_id(&group_id)?.internal {
        return Err(ApiError::bad_request("the Credit Card Payments group can't be deleted"));
    }
    if plan.group_categories(&group_id).next().is_some() {
        return Err(ApiError::conflict("the group has categories"));
    }
    plan.groups.shift_remove(&group_id);
    Ok(StatusCode::NO_CONTENT)
}

// --- categories --------------------------------------------------------------

pub async fn create(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan_mut(&plan_id)?;
    let body = Body::parse(&body)?;
    let group = own_group(plan, &body.required("group_id", fields::string)?)?;
    let name = body.required("name", fields::name)?;
    let note = body.optional("note", fields::note)?.flatten();
    let category =
        Category { id: new_id(), group_id: group.id.clone(), name, hidden: false, note, payment_account_id: None };
    plan.categories.insert(category.id.clone(), category.clone());
    Ok((StatusCode::CREATED, Json(category)))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, category_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    Ok(Json(store.plan(&plan_id)?.categories.by_id(&category_id)?.clone()))
}

pub async fn update(
    State(state): State<AppState>,
    Path((plan_id, category_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan_mut(&plan_id)?;
    let mut category = plan.categories.by_id(&category_id)?.clone();
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let note = body.optional("note", fields::note)?;
    let hidden = body.optional("hidden", fields::boolean)?;
    let group_id = body.optional("group_id", fields::string)?;
    if category.payment_account_id.is_some() && (name.is_some() || group_id.is_some()) {
        return Err(ApiError::bad_request("a payment category's name and group follow its account"));
    }
    // Naming the category's own group doesn't move it.
    let moved = match group_id {
        Some(id) if id != category.group_id => {
            category.group_id = own_group(plan, &id)?.id.clone();
            true
        }
        _ => false,
    };
    if let Some(name) = name {
        category.name = name;
    }
    if let Some(note) = note {
        category.note = note;
    }
    if let Some(hidden) = hidden {
        category.hidden = hidden;
    }

    // A moved category goes to the end of its new group (SPEC 7.6): the end
    // of the plan's categories, since a group lists them in their order.
    if moved {
        plan.categories.shift_remove(&category_id);
    }
    plan.categories.insert(category_id, category.clone());
    Ok(Json(category))
}
