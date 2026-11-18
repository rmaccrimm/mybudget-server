//! Transactions, transfers and splits (SPEC 8 to 10).
//!
//! `POST` and `PATCH` both work out the transaction the request asks for
//! (a [`Desired`]), check it against every rule a transaction must meet, and
//! only then change anything: create a payee named by `payee_name`, create or
//! delete a transfer's other half, and store the result (SPEC 3.11, 8.6).

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::AppState;
use crate::calendar;
use crate::error::ApiError;
use crate::fields::{self, Body, nullable};
use crate::json::Json as JsonValue;
use crate::model::{
    Account, AccountType, ById, Cleared, FlagColor, Plan, READY_TO_ASSIGN, Subtransaction,
    Transaction, new_id,
};

// ---------------------------------------------------------------------------
// Field parsing

/// A category id in a body must name something that exists: Ready to
/// Assign or one of the plan's categories (SPEC 3.4, 4.5).
fn category_id(plan: &Plan, field: &str, v: &JsonValue) -> Result<Option<String>, ApiError> {
    let id = nullable(fields::string)(field, v)?;
    if let Some(id) = &id
        && id != READY_TO_ASSIGN
        && !plan.categories.contains_key(id)
    {
        return Err(ApiError::bad_request(format!(
            "{field} doesn't name one of the plan's categories"
        )));
    }
    Ok(id)
}

/// A subtransaction as given; its category is checked against the account
/// later (SPEC 10.2).
struct SubInput {
    amount: i64,
    category_id: Option<String>,
    memo: Option<String>,
}

impl From<&Subtransaction> for SubInput {
    fn from(s: &Subtransaction) -> Self {
        Self {
            amount: s.amount,
            category_id: Some(s.category_id.clone()),
            memo: s.memo.clone(),
        }
    }
}

fn subtransactions(plan: &Plan, field: &str, v: &JsonValue) -> Result<Vec<SubInput>, ApiError> {
    let JsonValue::Array(items) = v else {
        return Err(ApiError::bad_request(format!("{field} must be an array")));
    };
    items
        .iter()
        .map(|item| {
            let sub = Body::object(&format!("each of {field}"), item)?;
            Ok(SubInput {
                amount: sub.required("amount", fields::amount)?,
                category_id: sub.required("category_id", |f, v| category_id(plan, f, v))?,
                memo: sub.optional("memo", fields::note)?.flatten(),
            })
        })
        .collect()
}

/// The fields of SPEC 8.1, each checked whenever it's present (SPEC 3.6).
struct Input {
    account_id: Option<String>,
    date: Option<String>,
    amount: Option<i64>,
    /// Who the request says the payee is, if it says.
    payee: Option<PayeeChoice>,
    category_id: Option<Option<String>>,
    memo: Option<Option<String>>,
    cleared: Option<Cleared>,
    approved: Option<bool>,
    flag_color: Option<Option<FlagColor>>,
    subtransactions: Option<Vec<SubInput>>,
}

impl Input {
    fn parse(plan: &Plan, body: &Body) -> Result<Self, ApiError> {
        let payee_id = body.optional("payee_id", nullable(fields::string))?;
        if let Some(Some(id)) = &payee_id
            && !plan.payees.contains_key(id)
        {
            return Err(ApiError::bad_request(
                "payee_id doesn't name one of the plan's payees",
            ));
        }
        let payee_name = body.optional("payee_name", nullable(fields::payee_name))?;
        Ok(Self {
            account_id: body.optional("account_id", fields::string)?,
            date: body.optional("date", fields::date)?,
            amount: body.optional("amount", fields::amount)?,
            payee: PayeeChoice::requested(payee_id, payee_name),
            category_id: body.optional("category_id", |f, v| category_id(plan, f, v))?,
            memo: body.optional("memo", fields::note)?,
            cleared: body.optional("cleared", fields::variant)?,
            approved: body.optional("approved", fields::boolean)?,
            flag_color: body.optional("flag_color", nullable(fields::variant))?,
            subtransactions: body
                .optional("subtransactions", |f, v| subtransactions(plan, f, v))?,
        })
    }
}

// ---------------------------------------------------------------------------
// Rules

/// Who the payee will be (SPEC 6.5). A payee chosen by name is created only
/// once the request is known to succeed.
enum PayeeChoice {
    NoPayee,
    Id(String),
    Name(String),
}

impl PayeeChoice {
    /// The payee a body asks for, or `None` if it leaves the payee as it is.
    /// A `payee_id` wins over a `payee_name`; `payee_id: null` asks for no
    /// payee, and `payee_name: null` alone asks for nothing.
    fn requested(
        payee_id: Option<Option<String>>,
        payee_name: Option<Option<String>>,
    ) -> Option<Self> {
        match (payee_id, payee_name) {
            (Some(Some(id)), _) => Some(Self::Id(id)),
            (_, Some(Some(name))) => Some(Self::Name(name)),
            (Some(None), _) => Some(Self::NoPayee),
            (None, _) => None,
        }
    }

    fn from_id(payee_id: Option<String>) -> Self {
        payee_id.map_or(Self::NoPayee, Self::Id)
    }
}

/// The transaction a request asks for in its account, before anything is
/// changed.
struct Desired {
    date: String,
    amount: i64,
    payee: PayeeChoice,
    /// The transaction's category; for a transfer between an on-budget and a
    /// tracking account, the on-budget half's (SPEC 9.3).
    category: Option<String>,
    memo: Option<String>,
    cleared: Cleared,
    approved: bool,
    flag_color: Option<FlagColor>,
    subs: Vec<SubInput>,
}

/// Which half of a transfer carries its category.
#[derive(Clone, Copy)]
enum CategorySide {
    This,
    Other,
    Neither,
}

/// On a transfer between an on-budget and a tracking account, the on-budget
/// half carries the category; any other transfer has none (SPEC 9.3).
fn category_side(this: AccountType, other: AccountType) -> CategorySide {
    match (this.on_budget(), other.on_budget()) {
        (true, false) => CategorySide::This,
        (false, true) => CategorySide::Other,
        _ => CategorySide::Neither,
    }
}

impl CategorySide {
    /// The categories of this half and the other.
    fn split(self, category: Option<String>) -> (Option<String>, Option<String>) {
        match self {
            Self::This => (category, None),
            Self::Other => (None, category),
            Self::Neither => (None, None),
        }
    }
}

/// The account a payee transfers to, if it's a transfer payee (SPEC 8.3).
fn transfer_target<'a>(plan: &'a Plan, payee: &PayeeChoice) -> Option<&'a Account> {
    let PayeeChoice::Id(id) = payee else {
        return None;
    };
    let account_id = plan.payees.get(id)?.transfer_account_id.as_ref()?;
    plan.accounts.get(account_id)
}

/// A category in an on-budget account: required, and a user category or
/// Ready to Assign, never a payment category (SPEC 8.2).
fn check_on_budget_category(
    plan: &Plan,
    field: &str,
    category: Option<&str>,
) -> Result<(), ApiError> {
    match category {
        None => Err(ApiError::bad_request(format!(
            "{field} is required in an on-budget account"
        ))),
        Some(READY_TO_ASSIGN) => Ok(()),
        Some(id) => match plan.categories.get(id) {
            Some(c) if c.payment_account_id.is_none() => Ok(()),
            Some(_) => Err(ApiError::bad_request(format!(
                "{field} can't be a credit card payment category"
            ))),
            None => Err(ApiError::bad_request(format!(
                "{field} doesn't name one of the plan's categories"
            ))),
        },
    }
}

/// Where a valid transfer goes.
struct Transfer<'a> {
    to: &'a Account,
    side: CategorySide,
}

/// Checks every rule a transaction must meet (SPEC 8.2, 9.2, 9.3, 10.1,
/// 10.2) in `account`. Returns where it transfers to, if it's a transfer.
fn validate<'a>(
    plan: &'a Plan,
    account: &Account,
    d: &Desired,
) -> Result<Option<Transfer<'a>>, ApiError> {
    let on_budget = account.kind.on_budget();
    let target = transfer_target(plan, &d.payee);
    if let Some(to) = target {
        if to.id == account.id {
            return Err(ApiError::bad_request("an account can't transfer to itself"));
        }
        if !d.subs.is_empty() {
            return Err(ApiError::bad_request("a transfer can't be split"));
        }
        let side = category_side(account.kind, to.kind);
        match side {
            CategorySide::Neither if d.category.is_some() => {
                return Err(ApiError::bad_request(
                    "category_id must be null on a transfer between these accounts",
                ));
            }
            CategorySide::Neither => {}
            CategorySide::This | CategorySide::Other => {
                check_on_budget_category(plan, "category_id", d.category.as_deref())?;
            }
        }
        return Ok(Some(Transfer { to, side }));
    }
    if !d.subs.is_empty() {
        if !on_budget {
            return Err(ApiError::bad_request(
                "a tracking account's transactions can't be split",
            ));
        }
        if d.category.is_some() {
            return Err(ApiError::bad_request(
                "a split's own category_id must be null",
            ));
        }
        let sum: i128 = d.subs.iter().map(|s| i128::from(s.amount)).sum();
        if sum != i128::from(d.amount) {
            return Err(ApiError::bad_request(
                "the subtransactions' amounts must add up to the transaction's amount",
            ));
        }
        for s in &d.subs {
            check_on_budget_category(
                plan,
                "a subtransaction's category_id",
                s.category_id.as_deref(),
            )?;
        }
    } else if on_budget {
        check_on_budget_category(plan, "category_id", d.category.as_deref())?;
    } else if d.category.is_some() {
        return Err(ApiError::bad_request(
            "category_id must be null in a tracking account",
        ));
    }
    Ok(None)
}

/// The category a `PATCH` that leaves out `category_id` ends up with
/// (SPEC 8.6): the one it had, unless the changed transaction can't have
/// one.
fn kept_category(
    plan: &Plan,
    t: &Transaction,
    account: &Account,
    target: Option<&Account>,
    split: bool,
) -> Option<String> {
    if split {
        return None;
    }
    match target {
        Some(to) if to.id != account.id => match category_side(account.kind, to.kind) {
            CategorySide::Neither => None,
            CategorySide::This => t.category_id.clone(),
            // A tracking half's category is on its on-budget half.
            CategorySide::Other => t
                .transfer_transaction_id
                .as_ref()
                .and_then(|id| plan.transactions.get(id))
                .and_then(|x| x.category_id.clone()),
        },
        _ => t.category_id.clone(),
    }
}

// ---------------------------------------------------------------------------
// Changes

fn resolve_payee(plan: &mut Plan, payee: PayeeChoice) -> Option<String> {
    match payee {
        PayeeChoice::NoPayee => None,
        PayeeChoice::Id(id) => Some(id),
        PayeeChoice::Name(name) => Some(plan.payee_by_name(&name)),
    }
}

/// The other half of the transfer `t` from `from` into `to` (SPEC 9.1),
/// linking `t` to it.
fn other_half(t: &mut Transaction, from: &Account, to: &Account) -> Transaction {
    let half = Transaction {
        id: new_id(),
        account_id: to.id.clone(),
        date: t.date.clone(),
        amount: -t.amount,
        payee_id: Some(from.transfer_payee_id.clone()),
        memo: t.memo.clone(),
        cleared: Cleared::default_for(to.kind),
        approved: true,
        transfer_account_id: Some(t.account_id.clone()),
        transfer_transaction_id: Some(t.id.clone()),
        ..Default::default()
    };
    t.transfer_account_id = Some(half.account_id.clone());
    t.transfer_transaction_id = Some(half.id.clone());
    half
}

impl Desired {
    /// Copies what's asked for into `t`, except the payee, the category and
    /// the transfer links, which depend on more than the request.
    fn apply(&self, t: &mut Transaction) {
        t.date = self.date.clone();
        t.amount = self.amount;
        t.memo = self.memo.clone();
        t.cleared = self.cleared;
        t.approved = self.approved;
        t.flag_color = self.flag_color;
        t.subtransactions = self
            .subs
            .iter()
            .map(|s| Subtransaction {
                amount: s.amount,
                category_id: s
                    .category_id
                    .clone()
                    .expect("validate checked every split line has a category"),
                memo: s.memo.clone(),
            })
            .collect();
    }
}

/// The categories of a transaction and of its other half, if it's a
/// transfer.
fn place_category(
    transfer: Option<&Transfer>,
    category: Option<String>,
) -> (Option<String>, Option<String>) {
    match transfer {
        Some(x) => x.side.split(category),
        None => (category, None),
    }
}

// ---------------------------------------------------------------------------
// Handlers

pub async fn create(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let body = Body::parse(&body)?;
    let input = Input::parse(plan, &body)?;
    let account_id = input
        .account_id
        .ok_or_else(|| fields::missing("account_id"))?;
    let date = input.date.ok_or_else(|| fields::missing("date"))?;
    let amount = input.amount.ok_or_else(|| fields::missing("amount"))?;
    let account = plan.accounts.get(&account_id).ok_or_else(|| {
        ApiError::bad_request("account_id doesn't name one of the plan's accounts")
    })?;
    let d = Desired {
        date,
        amount,
        payee: input.payee.unwrap_or(PayeeChoice::NoPayee),
        category: input.category_id.flatten(),
        memo: input.memo.flatten(),
        cleared: input.cleared.unwrap_or(Cleared::default_for(account.kind)),
        approved: input.approved.unwrap_or(true),
        flag_color: input.flag_color.flatten(),
        subs: input.subtransactions.unwrap_or_default(),
    };
    let transfer = validate(plan, account, &d)?;

    let mut t = Transaction {
        id: new_id(),
        account_id,
        ..Default::default()
    };
    d.apply(&mut t);
    let half = transfer.as_ref().map(|x| other_half(&mut t, account, x.to));
    let (category, other_category) = place_category(transfer.as_ref(), d.category);
    t.category_id = category;

    // Everything is checked: from here on only the save can fail
    // (SPEC 3.11, 15.10).
    let t = store.change_plan(&plan_id, |plan| {
        t.payee_id = resolve_payee(plan, d.payee);
        plan.transactions.insert(t.id.clone(), t.clone());
        if let Some(mut half) = half {
            half.category_id = other_category;
            plan.transactions.insert(half.id.clone(), half);
        }
        t
    })?;
    Ok((StatusCode::CREATED, Json(t)))
}

pub async fn get(
    State(state): State<AppState>,
    Path((plan_id, transaction_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    Ok(Json(
        store
            .plan(&plan_id)?
            .transactions
            .by_id(&transaction_id)?
            .clone(),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    Path((plan_id, transaction_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let plan = store.plan(&plan_id)?;
    let mut t = plan.transactions.by_id(&transaction_id)?.clone();
    let body = Body::parse(&body)?;
    let input = Input::parse(plan, &body)?;
    if input
        .account_id
        .as_ref()
        .is_some_and(|a| *a != t.account_id)
    {
        return Err(ApiError::bad_request(
            "a transaction's account can't change",
        ));
    }
    let account = plan
        .accounts
        .get(&t.account_id)
        .expect("a transaction's account exists");
    let payee = input
        .payee
        .unwrap_or_else(|| PayeeChoice::from_id(t.payee_id.clone()));
    let subs = input
        .subtransactions
        .unwrap_or_else(|| t.subtransactions.iter().map(SubInput::from).collect());
    let category = match input.category_id {
        Some(category) => category,
        None => kept_category(
            plan,
            &t,
            account,
            transfer_target(plan, &payee),
            !subs.is_empty(),
        ),
    };
    let d = Desired {
        date: input.date.unwrap_or_else(|| t.date.clone()),
        amount: input.amount.unwrap_or(t.amount),
        payee,
        category,
        memo: input.memo.unwrap_or_else(|| t.memo.clone()),
        cleared: input.cleared.unwrap_or(t.cleared),
        approved: input.approved.unwrap_or(t.approved),
        flag_color: input.flag_color.unwrap_or(t.flag_color),
        subs,
    };
    let transfer = validate(plan, account, &d)?;

    d.apply(&mut t);
    // The other half stays only if it's still in the account transferred
    // to (SPEC 9.5); otherwise it goes, and a new one comes if needed.
    let keeps_partner = transfer
        .as_ref()
        .is_some_and(|x| t.transfer_account_id.as_ref() == Some(&x.to.id));
    let dropped_partner = if keeps_partner {
        None
    } else {
        t.transfer_account_id = None;
        t.transfer_transaction_id.take()
    };
    let new_half = transfer
        .as_ref()
        .filter(|_| !keeps_partner)
        .map(|x| other_half(&mut t, account, x.to));
    let (category, other_category) = place_category(transfer.as_ref(), d.category);
    t.category_id = category;

    // Everything is checked: from here on only the save can fail
    // (SPEC 3.11, 15.10).
    let t = store.change_plan(&plan_id, |plan| {
        if let Some(id) = dropped_partner {
            plan.transactions.shift_remove(&id);
        }
        t.payee_id = resolve_payee(plan, d.payee);
        if keeps_partner {
            // Date and amount follow on the other half (SPEC 9.4).
            let id = t
                .transfer_transaction_id
                .as_ref()
                .expect("a transfer links its other half");
            let partner = plan
                .transactions
                .get_mut(id)
                .expect("a transfer's other half exists");
            partner.date = t.date.clone();
            partner.amount = -t.amount;
            partner.category_id = other_category;
        } else if let Some(mut half) = new_half {
            half.category_id = other_category;
            plan.transactions.insert(half.id.clone(), half);
        }
        plan.transactions.insert(t.id.clone(), t.clone());
        t
    })?;
    Ok(Json(t))
}

pub async fn delete(
    State(state): State<AppState>,
    Path((plan_id, transaction_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let mut store = state.store();
    let t = store.plan(&plan_id)?.transactions.by_id(&transaction_id)?;
    // Deleting either half of a transfer deletes both (SPEC 9.6).
    let partner = t.transfer_transaction_id.clone();
    store.change_plan(&plan_id, |plan| {
        if let Some(partner) = partner {
            plan.transactions.shift_remove(&partner);
        }
        plan.transactions.shift_remove(&transaction_id);
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Listing

/// Lists transactions by date, oldest first within a date, filtered by the
/// query parameters of SPEC 8.5. A repeated parameter keeps its last value.
pub async fn list(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Result<impl IntoResponse, ApiError> {
    let store = state.store();
    let plan = store.plan(&plan_id)?;
    let (mut account_id, mut payee_id, mut category_id) = (None, None, None);
    let (mut since, mut until, mut approved) = (None, None, None);
    for (key, value) in query {
        let date = |v: String| {
            if calendar::is_date(&v) {
                Ok(v)
            } else {
                Err(fields::invalid_date(&key))
            }
        };
        match key.as_str() {
            "account_id" => account_id = Some(value),
            "payee_id" => payee_id = Some(value),
            "category_id" => category_id = Some(value),
            "since_date" => since = Some(date(value)?),
            "until_date" => until = Some(date(value)?),
            "approved" => {
                approved = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(ApiError::bad_request("approved must be true or false")),
                })
            }
            _ => {}
        }
    }
    let mut list: Vec<Transaction> = plan
        .transactions
        .values()
        .filter(|t| account_id.as_ref().is_none_or(|a| &t.account_id == a))
        .filter(|t| {
            payee_id
                .as_ref()
                .is_none_or(|p| t.payee_id.as_ref() == Some(p))
        })
        .filter(|t| category_id.as_ref().is_none_or(|c| t.has_category(c)))
        .filter(|t| since.as_ref().is_none_or(|d| &t.date >= d))
        .filter(|t| until.as_ref().is_none_or(|d| &t.date <= d))
        .filter(|t| approved.is_none_or(|a| t.approved == a))
        .cloned()
        .collect();
    // Stable, so creation order holds within a date.
    list.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(Json(list))
}
