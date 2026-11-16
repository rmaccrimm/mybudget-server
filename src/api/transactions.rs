//! Transactions, transfers and splits (SPEC 8 to 10).
//!
//! `POST` and `PATCH` both work out the transaction the request asks for
//! (a [`Desired`]), check it against every rule a transaction must meet, and
//! only then change anything: create a payee named by `payee_name`, create or
//! delete a transfer's other half, and store the result (SPEC 3.11, 8.6).

use serde_json::Value;

use crate::error::{ApiError, ApiResult, created, no_content, ok};
use crate::fields::{self, optional, parse_object, required};
use crate::json::{Json, Object};
use crate::model::{
    Cleared, FLAG_COLORS, Ids, Plan, READY_TO_ASSIGN, Store, Subtransaction, Transaction, plan, plan_mut,
};

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message)
}

// ---------------------------------------------------------------------------
// Field parsing

/// A nullable string field.
fn nullable_string(field: &str, v: &Json) -> Result<Option<String>, ApiError> {
    match v {
        Json::Null => Ok(None),
        Json::String(s) => Ok(Some(s.clone())),
        _ => Err(bad(format!("{field} must be a string or null"))),
    }
}

/// `payee_name`: a payee name or `null` (SPEC 6.5).
fn payee_name(field: &str, v: &Json) -> Result<Option<String>, ApiError> {
    match v {
        Json::Null => Ok(None),
        _ => fields::payee_name(field, v).map(Some),
    }
}

fn cleared(field: &str, v: &Json) -> Result<Cleared, ApiError> {
    match v {
        Json::String(s) => Cleared::parse(s),
        _ => None,
    }
    .ok_or_else(|| bad(format!("{field} must be \"uncleared\", \"cleared\" or \"reconciled\"")))
}

fn flag_color(field: &str, v: &Json) -> Result<Option<String>, ApiError> {
    match v {
        Json::Null => Ok(None),
        Json::String(s) if FLAG_COLORS.contains(&s.as_str()) => Ok(Some(s.clone())),
        _ => Err(bad(format!("{field} must be null or one of {}", FLAG_COLORS.join(", ")))),
    }
}

/// A category id in a body must name something that exists: Ready to
/// Assign or one of the plan's categories (SPEC 3.4, 4.5).
fn category_id(plan: &Plan, field: &str, v: &Json) -> Result<Option<String>, ApiError> {
    let id = nullable_string(field, v)?;
    if let Some(id) = &id
        && id != READY_TO_ASSIGN
        && !plan.categories.iter().any(|c| &c.id == id)
    {
        return Err(bad(format!("{field} doesn't name one of the plan's categories")));
    }
    Ok(id)
}

/// A subtransaction as given; its category is checked against the account
/// later (SPEC 10.2).
#[derive(Clone)]
struct SubInput {
    amount: i64,
    category_id: Option<String>,
    memo: Option<String>,
}

fn subtransactions(plan: &Plan, field: &str, v: &Json) -> Result<Vec<SubInput>, ApiError> {
    let Json::Array(items) = v else {
        return Err(bad(format!("{field} must be an array")));
    };
    items
        .iter()
        .map(|item| -> Result<SubInput, ApiError> {
            let Json::Object(obj) = item else {
                return Err(bad(format!("each of {field} must be an object")));
            };
            let amount = fields::amount("amount", required(obj, "amount")?)?;
            let category_id = category_id(plan, "category_id", required(obj, "category_id")?)?;
            let memo = optional(obj, "memo", fields::note)?.flatten();
            Ok(SubInput { amount, category_id, memo })
        })
        .collect()
}

/// The fields of SPEC 8.1, each checked whenever it's present (SPEC 3.6).
struct Input {
    account_id: Option<String>,
    date: Option<String>,
    amount: Option<i64>,
    payee_id: Option<Option<String>>,
    payee_name: Option<Option<String>>,
    category_id: Option<Option<String>>,
    memo: Option<Option<String>>,
    cleared: Option<Cleared>,
    approved: Option<bool>,
    flag_color: Option<Option<String>>,
    subtransactions: Option<Vec<SubInput>>,
}

fn parse_input(plan: &Plan, obj: &Object) -> Result<Input, ApiError> {
    let payee_id = optional(obj, "payee_id", nullable_string)?;
    if let Some(Some(id)) = &payee_id
        && !plan.payees.iter().any(|p| &p.id == id)
    {
        return Err(bad("payee_id doesn't name one of the plan's payees"));
    }
    Ok(Input {
        account_id: optional(obj, "account_id", |f, v| fields::string(f, v).map(str::to_owned))?,
        date: optional(obj, "date", fields::date)?,
        amount: optional(obj, "amount", fields::amount)?,
        payee_id,
        payee_name: optional(obj, "payee_name", payee_name)?,
        category_id: optional(obj, "category_id", |f, v| category_id(plan, f, v))?,
        memo: optional(obj, "memo", fields::note)?,
        cleared: optional(obj, "cleared", cleared)?,
        approved: optional(obj, "approved", fields::boolean)?,
        flag_color: optional(obj, "flag_color", flag_color)?,
        subtransactions: optional(obj, "subtransactions", |f, v| subtransactions(plan, f, v))?,
    })
}

// ---------------------------------------------------------------------------
// Rules

/// Who the payee will be (SPEC 6.5). A payee chosen by name is created only
/// once the request is known to succeed.
enum PayeeChoice {
    None,
    Id(String),
    Name(String),
}

/// The transaction a request asks for, before anything is changed.
struct Desired {
    account: usize,
    date: String,
    amount: i64,
    payee: PayeeChoice,
    /// The transaction's category; for a transfer between an on-budget and a
    /// tracking account, the on-budget half's (SPEC 9.3).
    category: Option<String>,
    memo: Option<String>,
    cleared: Cleared,
    approved: bool,
    flag_color: Option<String>,
    subs: Vec<SubInput>,
}

/// The account a payee transfers to, if it's a transfer payee (SPEC 8.3).
fn transfer_target(plan: &Plan, payee: &PayeeChoice) -> Option<usize> {
    let PayeeChoice::Id(id) = payee else { return None };
    let account_id = plan.payees.iter().find(|p| &p.id == id)?.transfer_account_id.as_ref()?;
    plan.accounts.iter().position(|a| &a.id == account_id)
}

/// A category in an on-budget account: required, and a user category or
/// Ready to Assign, never a payment category (SPEC 8.2).
fn check_on_budget_category(plan: &Plan, field: &str, category: Option<&str>) -> Result<(), ApiError> {
    match category {
        None => Err(bad(format!("{field} is required in an on-budget account"))),
        Some(READY_TO_ASSIGN) => Ok(()),
        Some(id) => match plan.categories.iter().find(|c| c.id == id) {
            Some(c) if c.payment_account_id.is_none() => Ok(()),
            Some(_) => Err(bad(format!("{field} can't be a credit card payment category"))),
            None => Err(bad(format!("{field} doesn't name one of the plan's categories"))),
        },
    }
}

/// Checks every rule a transaction must meet (SPEC 8.2, 9.2, 9.3, 10.1,
/// 10.2). Returns the account it transfers to, if it's a transfer.
fn validate(plan: &Plan, d: &Desired) -> Result<Option<usize>, ApiError> {
    let on_budget = plan.accounts[d.account].kind.on_budget();
    let target = transfer_target(plan, &d.payee);
    if let Some(b) = target {
        if b == d.account {
            return Err(bad("an account can't transfer to itself"));
        }
        if !d.subs.is_empty() {
            return Err(bad("a transfer can't be split"));
        }
        if on_budget == plan.accounts[b].kind.on_budget() {
            if d.category.is_some() {
                return Err(bad("category_id must be null on a transfer between these accounts"));
            }
        } else {
            check_on_budget_category(plan, "category_id", d.category.as_deref())?;
        }
    } else if !d.subs.is_empty() {
        if !on_budget {
            return Err(bad("a tracking account's transactions can't be split"));
        }
        if d.category.is_some() {
            return Err(bad("a split's own category_id must be null"));
        }
        let sum: i128 = d.subs.iter().map(|s| i128::from(s.amount)).sum();
        if sum != i128::from(d.amount) {
            return Err(bad("the subtransactions' amounts must add up to the transaction's amount"));
        }
        for s in &d.subs {
            check_on_budget_category(plan, "a subtransaction's category_id", s.category_id.as_deref())?;
        }
    } else if on_budget {
        check_on_budget_category(plan, "category_id", d.category.as_deref())?;
    } else if d.category.is_some() {
        return Err(bad("category_id must be null in a tracking account"));
    }
    Ok(target)
}

// ---------------------------------------------------------------------------
// Changes

fn resolve_payee(plan: &mut Plan, ids: &mut Ids, payee: PayeeChoice) -> Option<String> {
    match payee {
        PayeeChoice::None => None,
        PayeeChoice::Id(id) => Some(id),
        PayeeChoice::Name(name) => Some(plan.payee_by_name(ids, &name)),
    }
}

fn account_on_budget(plan: &Plan, account_id: &str) -> bool {
    plan.accounts.iter().find(|a| a.id == account_id).is_some_and(|a| a.kind.on_budget())
}

/// Creates the other half of the transfer at `ti` into account `b`, after
/// it (SPEC 9.1). Returns its index.
fn create_other_half(plan: &mut Plan, ids: &mut Ids, ti: usize, b: usize) -> usize {
    let t = &plan.transactions[ti];
    let source_payee = plan.accounts.iter().find(|a| a.id == t.account_id).map(|a| a.transfer_payee_id.clone());
    let other = &plan.accounts[b];
    let half = Transaction {
        id: ids.next(),
        account_id: other.id.clone(),
        date: t.date.clone(),
        amount: -t.amount,
        payee_id: source_payee,
        category_id: None,
        memo: t.memo.clone(),
        cleared: Cleared::default_for(other.kind),
        approved: true,
        flag_color: None,
        transfer_account_id: Some(t.account_id.clone()),
        transfer_transaction_id: Some(t.id.clone()),
        subtransactions: Vec::new(),
    };
    let t = &mut plan.transactions[ti];
    t.transfer_account_id = Some(half.account_id.clone());
    t.transfer_transaction_id = Some(half.id.clone());
    plan.transactions.push(half);
    plan.transactions.len() - 1
}

/// Puts the category on the right half: on a transfer between an on-budget
/// and a tracking account, the on-budget half has it (SPEC 9.3).
fn place_category(plan: &mut Plan, ti: usize, partner: Option<usize>, category: Option<String>) {
    let Some(pi) = partner else {
        plan.transactions[ti].category_id = category;
        return;
    };
    let t_on = account_on_budget(plan, &plan.transactions[ti].account_id);
    let p_on = account_on_budget(plan, &plan.transactions[pi].account_id);
    let (t_cat, p_cat) = match (t_on, p_on) {
        (true, false) => (category, None),
        (false, true) => (None, category),
        _ => (None, None),
    };
    plan.transactions[ti].category_id = t_cat;
    plan.transactions[pi].category_id = p_cat;
}

fn into_subs(subs: Vec<SubInput>) -> Vec<Subtransaction> {
    subs.into_iter()
        .map(|s| Subtransaction { amount: s.amount, category_id: s.category_id.unwrap_or_default(), memo: s.memo })
        .collect()
}

// ---------------------------------------------------------------------------
// Handlers

pub fn create(store: &mut Store, plan_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let obj = parse_object(body)?;
    let input = parse_input(plan, &obj)?;
    let account_id = fields::string("account_id", required(&obj, "account_id")?)?;
    let date = input.date.ok_or_else(|| bad("date is required"))?;
    let amount = input.amount.ok_or_else(|| bad("amount is required"))?;
    let account = plan
        .accounts
        .iter()
        .position(|a| a.id == account_id)
        .ok_or_else(|| bad("account_id doesn't name one of the plan's accounts"))?;
    let payee = match (input.payee_id, input.payee_name) {
        (Some(Some(id)), _) => PayeeChoice::Id(id),
        (_, Some(Some(name))) => PayeeChoice::Name(name),
        _ => PayeeChoice::None,
    };
    let d = Desired {
        account,
        date,
        amount,
        payee,
        category: input.category_id.flatten(),
        memo: input.memo.flatten(),
        cleared: input.cleared.unwrap_or(Cleared::default_for(plan.accounts[account].kind)),
        approved: input.approved.unwrap_or(true),
        flag_color: input.flag_color.flatten(),
        subs: input.subtransactions.unwrap_or_default(),
    };
    let target = validate(plan, &d)?;

    let payee_id = resolve_payee(plan, &mut store.ids, d.payee);
    plan.transactions.push(Transaction {
        id: store.ids.next(),
        account_id: plan.accounts[account].id.clone(),
        date: d.date,
        amount: d.amount,
        payee_id,
        category_id: None,
        memo: d.memo,
        cleared: d.cleared,
        approved: d.approved,
        flag_color: d.flag_color,
        transfer_account_id: None,
        transfer_transaction_id: None,
        subtransactions: into_subs(d.subs),
    });
    let ti = plan.transactions.len() - 1;
    let partner = target.map(|b| create_other_half(plan, &mut store.ids, ti, b));
    place_category(plan, ti, partner, d.category);
    created(plan.transactions[ti].to_json())
}

pub fn get(store: &Store, plan_id: &str, transaction_id: &str) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let i = plan.transaction_index(transaction_id)?;
    ok(plan.transactions[i].to_json())
}

/// The category a `PATCH` that leaves out `category_id` ends up with
/// (SPEC 8.6): the one it had, unless the changed transaction can't have
/// one.
fn kept_category(plan: &Plan, t: &Transaction, account: usize, target: Option<usize>, split: bool) -> Option<String> {
    if split {
        return None;
    }
    let on_budget = plan.accounts[account].kind.on_budget();
    match target {
        Some(b) if b != account => {
            if on_budget == plan.accounts[b].kind.on_budget() {
                None
            } else if on_budget {
                t.category_id.clone()
            } else {
                // A tracking half's category is on its on-budget half.
                t.transfer_transaction_id
                    .as_ref()
                    .and_then(|id| plan.transactions.iter().find(|x| &x.id == id))
                    .and_then(|x| x.category_id.clone())
            }
        }
        _ => t.category_id.clone(),
    }
}

pub fn update(store: &mut Store, plan_id: &str, transaction_id: &str, body: &[u8]) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let ti = plan.transaction_index(transaction_id)?;
    let obj = parse_object(body)?;
    let input = parse_input(plan, &obj)?;
    let t = plan.transactions[ti].clone();
    if input.account_id.as_ref().is_some_and(|a| *a != t.account_id) {
        return Err(bad("a transaction's account can't change"));
    }
    let account = plan
        .accounts
        .iter()
        .position(|a| a.id == t.account_id)
        .expect("a transaction's account exists");
    let payee = match (input.payee_id, input.payee_name) {
        (Some(Some(id)), _) => PayeeChoice::Id(id),
        (_, Some(Some(name))) => PayeeChoice::Name(name),
        (Some(None), _) => PayeeChoice::None,
        (None, _) => t.payee_id.clone().map_or(PayeeChoice::None, PayeeChoice::Id),
    };
    let subs = match input.subtransactions {
        Some(subs) => subs,
        None => t
            .subtransactions
            .iter()
            .map(|s| SubInput { amount: s.amount, category_id: Some(s.category_id.clone()), memo: s.memo.clone() })
            .collect(),
    };
    let category = match input.category_id {
        Some(category) => category,
        None => kept_category(plan, &t, account, transfer_target(plan, &payee), !subs.is_empty()),
    };
    let d = Desired {
        account,
        date: input.date.unwrap_or_else(|| t.date.clone()),
        amount: input.amount.unwrap_or(t.amount),
        payee,
        category,
        memo: input.memo.unwrap_or_else(|| t.memo.clone()),
        cleared: input.cleared.unwrap_or(t.cleared),
        approved: input.approved.unwrap_or(t.approved),
        flag_color: input.flag_color.unwrap_or_else(|| t.flag_color.clone()),
        subs,
    };
    let target = validate(plan, &d)?;

    // The other half stays only if it's still in the account transferred to
    // (SPEC 9.5).
    let target_account = target.map(|b| plan.accounts[b].id.clone());
    let keep_partner = t.transfer_account_id.is_some() && t.transfer_account_id == target_account;
    if !keep_partner && let Some(pid) = &t.transfer_transaction_id {
        plan.transactions.retain(|x| &x.id != pid);
    }
    let payee_id = resolve_payee(plan, &mut store.ids, d.payee);
    let ti = plan.transaction_index(transaction_id)?;
    let x = &mut plan.transactions[ti];
    x.date = d.date;
    x.amount = d.amount;
    x.payee_id = payee_id;
    x.memo = d.memo;
    x.cleared = d.cleared;
    x.approved = d.approved;
    x.flag_color = d.flag_color;
    x.subtransactions = into_subs(d.subs);
    if !keep_partner {
        x.transfer_account_id = None;
        x.transfer_transaction_id = None;
    }
    let partner = if keep_partner {
        // Date and amount follow on the other half (SPEC 9.4).
        let pid = t.transfer_transaction_id.as_deref().unwrap_or_default();
        let (date, amount) = (x.date.clone(), x.amount);
        let pi = plan.transaction_index(pid)?;
        plan.transactions[pi].date = date;
        plan.transactions[pi].amount = -amount;
        Some(pi)
    } else {
        target.map(|b| create_other_half(plan, &mut store.ids, ti, b))
    };
    place_category(plan, ti, partner, d.category);
    ok(plan.transactions[ti].to_json())
}

pub fn delete(store: &mut Store, plan_id: &str, transaction_id: &str) -> ApiResult {
    let plan = plan_mut(&mut store.plans, plan_id)?;
    let i = plan.transaction_index(transaction_id)?;
    // Deleting either half of a transfer deletes both (SPEC 9.6).
    let partner = plan.transactions[i].transfer_transaction_id.clone();
    plan.transactions.retain(|t| t.id != transaction_id && Some(&t.id) != partner.as_ref());
    no_content()
}

// ---------------------------------------------------------------------------
// Listing

fn hex(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

/// Decodes `%XX` escapes and `+` in a query component.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_pairs(query: Option<&str>) -> Vec<(String, String)> {
    query
        .unwrap_or("")
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// Lists transactions by date, oldest first within a date, filtered by the
/// query parameters of SPEC 8.5.
pub fn list(store: &Store, plan_id: &str, query: Option<&str>) -> ApiResult {
    let plan = plan(&store.plans, plan_id)?;
    let (mut account_id, mut payee_id, mut category_id) = (None, None, None);
    let (mut since, mut until, mut approved) = (None, None, None);
    for (key, value) in query_pairs(query) {
        let date = |v: String| {
            fields::parse_date(&v)
                .map(|_| v)
                .ok_or_else(|| bad(format!("{key} must be a date YYYY-MM-DD from 1900-01-01 to 2999-12-31")))
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
                    _ => return Err(bad("approved must be true or false")),
                })
            }
            _ => {}
        }
    }
    let mut list: Vec<&Transaction> = plan
        .transactions
        .iter()
        .filter(|t| account_id.as_ref().is_none_or(|a| &t.account_id == a))
        .filter(|t| payee_id.as_ref().is_none_or(|p| t.payee_id.as_ref() == Some(p)))
        .filter(|t| {
            category_id.as_ref().is_none_or(|c| {
                t.category_id.as_ref() == Some(c) || t.subtransactions.iter().any(|s| &s.category_id == c)
            })
        })
        .filter(|t| since.as_ref().is_none_or(|d| &t.date >= d))
        .filter(|t| until.as_ref().is_none_or(|d| &t.date <= d))
        .filter(|t| approved.is_none_or(|a| t.approved == a))
        .collect();
    // Stable, so creation order holds within a date.
    list.sort_by(|a, b| a.date.cmp(&b.date));
    ok(Value::Array(list.into_iter().map(Transaction::to_json).collect()))
}
