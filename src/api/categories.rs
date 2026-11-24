//! Category groups and categories (SPEC 7, 13).

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::AppState;
use crate::calendar::Month;
use crate::error::ApiError;
use crate::fields::{self, Body, Patch};
use crate::json::Json as JsonValue;
use crate::model::{ById, Category, Goal, Group, Plan, Target, new_id};

/// A body's `group_id` must be one of the plan's own groups (SPEC 7.5, 7.6).
fn own_group<'a>(plan: &'a Plan, id: &str) -> Result<&'a Group, ApiError> {
    plan.groups.get(id).filter(|g| !g.internal).ok_or_else(|| {
        ApiError::bad_request("group_id must be one of the plan's own category groups")
    })
}

pub async fn list(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    Ok(Json(
        plan.groups
            .values()
            .map(|g| plan.group_view(g))
            .collect::<Vec<_>>(),
    ))
}

// --- groups ----------------------------------------------------------------

pub async fn create_group(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let name = body.required("name", fields::name)?;
    let group = Group {
        id: new_id(),
        name,
        hidden: false,
        internal: false,
    };
    let view = store.change_plan(&plan_id, |plan| {
        plan.groups.insert(group.id.clone(), group.clone());
        plan.group_view(&group)
    })?;
    Ok((StatusCode::CREATED, Json(view)))
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
    let plan = store.plan(&plan_id)?;
    let mut group = plan.groups.by_id(&group_id)?.clone();
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let hidden = body.optional("hidden", fields::boolean)?;
    if group.internal && (name.is_some() || hidden.is_some()) {
        return Err(ApiError::bad_request(
            "the Credit Card Payments group can't be changed",
        ));
    }
    if let Some(name) = name {
        group.name = name;
    }
    if let Some(hidden) = hidden {
        group.hidden = hidden;
    }
    let view = store.change_plan(&plan_id, |plan| {
        plan.groups.insert(group_id, group.clone());
        plan.group_view(&group)
    })?;
    Ok(Json(view))
}

pub async fn delete_group(
    State(state): State<AppState>,
    Path((plan_id, group_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    if plan.groups.by_id(&group_id)?.internal {
        return Err(ApiError::bad_request(
            "the Credit Card Payments group can't be deleted",
        ));
    }
    if plan.group_categories(&group_id).next().is_some() {
        return Err(ApiError::conflict("the group has categories"));
    }
    store.change_plan(&plan_id, |plan| plan.groups.shift_remove(&group_id))?;
    Ok(StatusCode::NO_CONTENT)
}

// --- categories --------------------------------------------------------------

pub async fn create(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let group = own_group(plan, &body.required("group_id", fields::string)?)?;
    let name = body.required("name", fields::name)?;
    let note = body.nullable("note", fields::note)?.or_null();
    let category = Category {
        id: new_id(),
        group_id: group.id.clone(),
        name,
        hidden: false,
        note,
        payment_account_id: None,
        targets: Vec::new(),
    };
    store.change_plan(&plan_id, |plan| {
        plan.categories
            .insert(category.id.clone(), category.clone())
    })?;
    Ok((StatusCode::CREATED, Json(category.view())))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, category_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    Ok(Json(
        store.plan(&plan_id)?.categories.by_id(&category_id)?.view(),
    ))
}

/// A target in a request (SPEC 18.3). `start_month` defaults to
/// `this_month`, today's, and can't be before it.
fn target(field: &str, v: &JsonValue, this_month: Month) -> Result<Target, ApiError> {
    let target = Body::object(field, v)?;
    let kind = target.required("type", fields::variant)?;
    let amount = target.required("amount", fields::amount)?;
    if amount <= 0 {
        return Err(ApiError::bad_request(
            "a target's amount must be more than 0",
        ));
    }
    let by_date = target.nullable("by_date", fields::day_of_month)?.or_null();
    let start_month = target
        .optional("start_month", fields::month)?
        .unwrap_or(this_month);
    if start_month < this_month {
        return Err(ApiError::bad_request("start_month is before this month"));
    }
    Ok(Target {
        goal: Goal {
            kind,
            amount,
            by_date,
        },
        start_month,
        end_month: None,
    })
}

pub async fn update(
    State(state): State<AppState>,
    Path((plan_id, category_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let mut category = plan.categories.by_id(&category_id)?.clone();
    let body = Body::parse(&body)?;
    let name = body.optional("name", fields::name)?;
    let note = body.nullable("note", fields::note)?;
    let hidden = body.optional("hidden", fields::boolean)?;
    let group_id = body.optional("group_id", fields::string)?;
    let this_month = Month::of_date(state.today());
    let target = body.nullable("target", |field, v| target(field, v, this_month))?;
    // A payment category's name and group follow its account, and it never
    // has a target (SPEC 7.6, 18.3).
    if category.payment_account_id.is_some()
        && (name.is_some() || group_id.is_some() || !matches!(target, Patch::Absent))
    {
        return Err(ApiError::bad_request(
            "a payment category's name, group and target can't be changed",
        ));
    }
    // Naming the category's own group doesn't move it.
    let moved = match group_id {
        Some(id) if id != category.group_id => {
            category.group_id.clone_from(&own_group(plan, &id)?.id);
            true
        }
        _ => false,
    };
    if let Some(name) = name {
        category.name = name;
    }
    note.apply_to(&mut category.note);
    if let Some(hidden) = hidden {
        category.hidden = hidden;
    }
    match target {
        Patch::Absent => {}
        // Today's month and every later month then have no target (18.4).
        Patch::Null => category.end_targets_at(this_month),
        Patch::Set(target) => category.set_target(target),
    }

    let view = category.view();
    store.change_plan(&plan_id, |plan| {
        // A moved category goes to the end of its new group (SPEC 7.6): the
        // end of the plan's categories, since a group lists them in their
        // order.
        if moved {
            plan.categories.shift_remove(&category_id);
        }
        plan.categories.insert(category_id, category)
    })?;
    Ok(Json(view))
}

/// `reassign_to` must name one of the plan's user categories other than the
/// one being deleted (SPEC 13.2). Ready to Assign isn't in the plan's
/// categories, so it fails the lookup.
fn reassign_target(plan: &Plan, deleted_id: &str, id: String) -> Result<String, ApiError> {
    match plan.categories.get(&id) {
        Some(c) if c.payment_account_id.is_none() && c.id != deleted_id => Ok(id),
        _ => Err(ApiError::bad_request(
            "reassign_to must name one of the plan's user categories other than the one being deleted",
        )),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    Path((plan_id, category_id)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    // A payment category goes only with its account (SPEC 13.1).
    if plan
        .categories
        .by_id(&category_id)?
        .payment_account_id
        .is_some()
    {
        return Err(ApiError::bad_request("a payment category can't be deleted"));
    }
    // Every value is checked, and the last counts (SPEC 3.5).
    let mut reassign_to = None;
    for (_, id) in query.into_iter().filter(|(key, _)| key == "reassign_to") {
        reassign_to = Some(reassign_target(plan, &category_id, id)?);
    }
    if reassign_to.is_none() && plan.category_in_use(&category_id) {
        return Err(ApiError::conflict("category is in use"));
    }
    store.change_plan(&plan_id, |plan| {
        plan.delete_category(&category_id, reassign_to.as_deref());
    })?;
    Ok(StatusCode::NO_CONTENT)
}
