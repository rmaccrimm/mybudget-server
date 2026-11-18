//! Months and Ready to Assign (SPEC 11), with credit cards (SPEC 12).
//!
//! Everything is sparse: only months with an assignment, a line or income
//! are stored, and the figures for any other month follow from those.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::calendar::Month;
use crate::model::{Plan, READY_TO_ASSIGN};

/// A category's figures for one month (SPEC 11.3).
#[derive(Serialize)]
pub struct Entry {
    pub category_id: String,
    pub assigned: i64,
    pub activity: i64,
    pub available: i64,
    pub cash_overspending: i64,
    pub credit_overspending: i64,
}

/// A month's figures (SPEC 11.6, 11.7).
#[derive(Serialize)]
pub struct MonthView {
    month: Month,
    income: i64,
    cash_advances: i64,
    assigned: i64,
    activity: i64,
    cash_overspending_last_month: i64,
    left_over_from_last_month: i64,
    assigned_in_future: i64,
    ready_to_assign: i64,
    categories: Vec<Entry>,
}

/// What a category carries into a month from the month before: its
/// `available` if positive, and 0 if not (SPEC 11.4).
fn carryover(available_before: i64) -> i64 {
    available_before.max(0)
}

/// A category's settled figures for a month in which it has any.
#[derive(Default, Clone, Copy)]
struct Cell {
    assigned: i64,
    activity: i64,
    credit_overspending: i64,
}

impl Cell {
    /// `available` = carryover + `assigned` + `activity` (SPEC 11.4).
    fn available(&self, available_before: i64) -> i64 {
        carryover(available_before) + self.assigned + self.activity
    }

    /// The overspending not spent on credit (SPEC 11.5).
    fn cash_overspending(&self, available: i64) -> i64 {
        (-available).max(0) - self.credit_overspending
    }
}

/// A user category's raw figures for a month, before credit funding is
/// worked out (SPEC 12.2).
#[derive(Default)]
struct Raw {
    assigned: i64,
    /// The sum of its lines in cash accounts.
    cash: i64,
    /// net(K) per credit account, keyed by the account's creation order.
    credit: BTreeMap<usize, i64>,
}

/// A month map holding a category's assigned amounts (SPEC 11.2), each put
/// in place by `assigned`.
fn with_assigned<T: Default>(
    plan: &Plan,
    category_id: &str,
    assigned: impl Fn(&mut T) -> &mut i64,
) -> BTreeMap<Month, T> {
    let mut map: BTreeMap<Month, T> = BTreeMap::new();
    for (&m, &a) in plan.assigned.get(category_id).into_iter().flatten() {
        *assigned(map.entry(m).or_default()) = a;
    }
    map
}

/// The plan's months, computed from its assignments and lines.
pub struct Budget<'a> {
    plan: &'a Plan,
    /// Per category id, the months it has an assignment or activity in.
    cells: HashMap<&'a str, BTreeMap<Month, Cell>>,
    /// Income by month (SPEC 11.6).
    income: BTreeMap<Month, i64>,
    /// Cash advances by month (SPEC 12.5).
    cash_advances: BTreeMap<Month, i64>,
}

impl<'a> Budget<'a> {
    pub fn new(plan: &'a Plan) -> Self {
        let mut raw: HashMap<&str, BTreeMap<Month, Raw>> = plan
            .categories
            .values()
            .filter(|c| c.payment_account_id.is_none())
            .map(|c| {
                (
                    c.id.as_str(),
                    with_assigned(plan, &c.id, |r: &mut Raw| &mut r.assigned),
                )
            })
            .collect();

        // Payment categories' activity, by account index and month.
        let mut payment_activity: HashMap<usize, BTreeMap<Month, i64>> = HashMap::new();
        let mut income = BTreeMap::new();
        let mut cash_advances = BTreeMap::new();

        for t in plan.transactions.values() {
            // Accounts are numbered by creation order, the order credit
            // funding follows.
            let (ai, _, account) = plan
                .accounts
                .get_full(&t.account_id)
                .expect("a transaction's account exists");
            let kind = account.kind;
            // Lines (SPEC 11.1): tracking accounts never affect the plan.
            if !kind.on_budget() {
                continue;
            }
            let month = Month::of_date(&t.date);
            // A transfer between a cash and a credit account: the credit
            // half decides what happens (SPEC 12.5).
            if kind.is_credit()
                && let Some(other) = &t.transfer_account_id
                && plan
                    .accounts
                    .get(other)
                    .expect("a transfer's other account exists")
                    .kind
                    .is_cash()
            {
                if t.amount > 0 {
                    *payment_activity
                        .entry(ai)
                        .or_default()
                        .entry(month)
                        .or_insert(0) -= t.amount;
                } else {
                    *cash_advances.entry(month).or_insert(0) -= t.amount;
                }
            }
            let mut line = |category: &str, amount: i64| {
                if category == READY_TO_ASSIGN {
                    // In a credit account it changes nothing (SPEC 12.6).
                    if kind.is_cash() {
                        *income.entry(month).or_insert(0) += amount;
                    }
                } else if let Some(map) = raw.get_mut(category) {
                    let r = map.entry(month).or_default();
                    if kind.is_credit() {
                        *r.credit.entry(ai).or_insert(0) += amount;
                    } else {
                        r.cash += amount;
                    }
                }
            };
            if t.subtransactions.is_empty() {
                if let Some(c) = &t.category_id {
                    line(c, t.amount);
                }
            } else {
                for s in &t.subtransactions {
                    line(&s.category_id, s.amount);
                }
            }
        }

        // Settle each user category month by month, funding its credit
        // spending from what it holds (SPEC 12.2, 12.3), and pass the
        // funded amounts and refunds on to the payment categories (12.4).
        let mut cells: HashMap<&str, BTreeMap<Month, Cell>> = HashMap::new();
        for (&cid, months) in &raw {
            let mut out = BTreeMap::new();
            let mut available = 0i64;
            for (&m, r) in months {
                let spending: i64 = r.credit.values().map(|&n| (-n).max(0)).sum();
                let refunds: i64 = r.credit.values().map(|&n| n.max(0)).sum();
                let funds = carryover(available) + r.assigned + r.cash + refunds;
                let funded = spending.min(funds.max(0));
                let mut left = funded;
                for (&ai, &net) in &r.credit {
                    let take = (-net).max(0).min(left);
                    left -= take;
                    let delta = take - net.max(0);
                    if delta != 0 {
                        *payment_activity
                            .entry(ai)
                            .or_default()
                            .entry(m)
                            .or_insert(0) += delta;
                    }
                }
                let activity = r.cash + r.credit.values().sum::<i64>();
                let cell = Cell {
                    assigned: r.assigned,
                    activity,
                    credit_overspending: spending - funded,
                };
                available = cell.available(available);
                out.insert(m, cell);
            }
            cells.insert(cid, out);
        }

        // Payment categories (SPEC 12.1, 12.4).
        for c in plan.categories.values() {
            let Some(account_id) = &c.payment_account_id else {
                continue;
            };
            let mut map = with_assigned(plan, &c.id, |cell: &mut Cell| &mut cell.assigned);
            let ai = plan
                .accounts
                .get_index_of(account_id)
                .expect("a payment category's account exists");
            for (&m, &a) in payment_activity.get(&ai).into_iter().flatten() {
                map.entry(m).or_default().activity += a;
            }
            cells.insert(c.id.as_str(), map);
        }

        Self {
            plan,
            cells,
            income,
            cash_advances,
        }
    }

    /// A category's entry for `month` (SPEC 11.3 to 11.5, 12.1 to 12.3).
    pub fn entry(&self, category_id: &str, month: Month) -> Entry {
        let cells = self.cells.get(category_id);
        // Months without figures between those with them just carry
        // `available` over (SPEC 11.4).
        let before = cells
            .into_iter()
            .flat_map(|c| c.range(..month))
            .fold(0, |a, (_, cell)| cell.available(a));
        let cell = cells
            .and_then(|c| c.get(&month))
            .copied()
            .unwrap_or_default();
        let available = cell.available(before);
        Entry {
            category_id: category_id.to_owned(),
            assigned: cell.assigned,
            activity: cell.activity,
            available,
            cash_overspending: cell.cash_overspending(available),
            credit_overspending: cell.credit_overspending,
        }
    }

    /// Total cash overspending by month, over every category.
    fn cash_overspending(&self) -> BTreeMap<Month, i64> {
        let mut out = BTreeMap::new();
        for cells in self.cells.values() {
            let mut available = 0i64;
            for (&m, cell) in cells {
                available = cell.available(available);
                let cash = cell.cash_overspending(available);
                if cash != 0 {
                    *out.entry(m).or_insert(0) += cash;
                }
            }
        }
        out
    }

    /// net(M) = income + cash advances - assigned, by month (SPEC 11.7).
    fn net(&self) -> BTreeMap<Month, i64> {
        let mut net = self.income.clone();
        for (&m, &a) in &self.cash_advances {
            *net.entry(m).or_insert(0) += a;
        }
        for cells in self.cells.values() {
            for (&m, cell) in cells {
                *net.entry(m).or_insert(0) -= cell.assigned;
            }
        }
        net
    }

    /// The month's view (SPEC 11.6, 11.7).
    pub fn month(&self, month: Month) -> MonthView {
        let mut assigned = 0i64;
        let mut activity = 0i64;
        let mut categories = Vec::new();
        for c in self
            .plan
            .groups
            .keys()
            .flat_map(|g| self.plan.group_categories(g))
        {
            let e = self.entry(&c.id, month);
            assigned += e.assigned;
            if c.payment_account_id.is_none() {
                activity += e.activity;
            }
            categories.push(e);
        }

        let overspending = self.cash_overspending();
        let net = self.net();
        let last_month = month.prev();
        let cash_overspending_last_month = overspending.get(&last_month).copied().unwrap_or(0);
        let left_over_from_last_month = net.range(..month).map(|(_, n)| n).sum::<i64>()
            - overspending
                .range(..last_month)
                .map(|(_, o)| o)
                .sum::<i64>();
        let net_m = net.get(&month).copied().unwrap_or(0);
        let pool = left_over_from_last_month + net_m - cash_overspending_last_month;

        let mut running = 0i64;
        let mut shortfall = 0i64;
        for (_, n) in net.range(month.next()..) {
            running += n;
            shortfall = shortfall.max(-running);
        }
        let assigned_in_future = if pool < 0 { 0 } else { pool.min(shortfall) };

        MonthView {
            month,
            income: self.income.get(&month).copied().unwrap_or(0),
            cash_advances: self.cash_advances.get(&month).copied().unwrap_or(0),
            assigned,
            activity,
            cash_overspending_last_month,
            left_over_from_last_month,
            assigned_in_future,
            ready_to_assign: pool - assigned_in_future,
            categories,
        }
    }
}
