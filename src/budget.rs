//! Months and Ready to Assign (SPEC 11), with credit cards (SPEC 12).
//!
//! Months are numbered by index: `year * 12 + month - 1`. Everything is
//! sparse: only months with an assignment, a line or income are stored, and
//! the figures for any other month follow from those.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::fields::parse_month;
use crate::model::{AccountType, Plan, READY_TO_ASSIGN};

pub fn month_index(s: &str) -> Option<i32> {
    parse_month(s).map(|(y, m)| y as i32 * 12 + m as i32 - 1)
}

pub fn month_string(index: i32) -> String {
    format!("{:04}-{:02}", index.div_euclid(12), index.rem_euclid(12) + 1)
}

/// The month index of a stored `YYYY-MM-DD` date.
fn date_month(date: &str) -> i32 {
    month_index(&date[..7]).expect("stored dates are valid")
}

/// Checking, savings and cash accounts. Income counts only in these
/// (SPEC 11.6); in a credit account `ready_to_assign` doesn't touch the
/// plan (SPEC 12.6).
fn is_cash(kind: AccountType) -> bool {
    matches!(kind, AccountType::Checking | AccountType::Savings | AccountType::Cash)
}

/// A category's figures for one month (SPEC 11.3).
#[derive(Default, Clone, Copy)]
pub struct Entry {
    pub assigned: i64,
    pub activity: i64,
    pub available: i64,
    pub cash_overspending: i64,
    pub credit_overspending: i64,
}

/// A category's settled figures for a month in which it has any.
#[derive(Default, Clone, Copy)]
struct Cell {
    assigned: i64,
    activity: i64,
    credit_overspending: i64,
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

/// The plan's months, computed from its assignments and lines.
pub struct Budget<'a> {
    plan: &'a Plan,
    /// Per category id, the months it has an assignment or activity in.
    cells: HashMap<&'a str, BTreeMap<i32, Cell>>,
    /// Income by month (SPEC 11.6).
    income: BTreeMap<i32, i64>,
    /// Cash advances by month (SPEC 12.5).
    cash_advances: BTreeMap<i32, i64>,
}

impl<'a> Budget<'a> {
    pub fn new(plan: &'a Plan) -> Self {
        // Accounts by creation order, the order credit funding follows.
        let order: HashMap<&str, usize> =
            plan.accounts.iter().enumerate().map(|(i, a)| (a.id.as_str(), i)).collect();

        let mut raw: HashMap<&str, BTreeMap<i32, Raw>> = HashMap::new();
        for c in plan.categories.iter().filter(|c| c.payment_account_id.is_none()) {
            let mut map: BTreeMap<i32, Raw> = BTreeMap::new();
            if let Some(assigned) = plan.assigned.get(&c.id) {
                for (&m, &a) in assigned {
                    map.entry(m).or_default().assigned = a;
                }
            }
            raw.insert(c.id.as_str(), map);
        }

        // Payment categories' activity, by account index and month.
        let mut payment_activity: HashMap<usize, BTreeMap<i32, i64>> = HashMap::new();
        let mut income = BTreeMap::new();
        let mut cash_advances = BTreeMap::new();

        for t in &plan.transactions {
            let Some(&ai) = order.get(t.account_id.as_str()) else { continue };
            let kind = plan.accounts[ai].kind;
            // Lines (SPEC 11.1): tracking accounts never affect the plan.
            if !kind.on_budget() {
                continue;
            }
            let month = date_month(&t.date);
            // A transfer between a cash and a credit account: the credit
            // half decides what happens (SPEC 12.5).
            if kind.is_credit()
                && let Some(other) = &t.transfer_account_id
                && let Some(&oi) = order.get(other.as_str())
                && is_cash(plan.accounts[oi].kind)
            {
                if t.amount > 0 {
                    *payment_activity.entry(ai).or_default().entry(month).or_insert(0) -= t.amount;
                } else {
                    *cash_advances.entry(month).or_insert(0) -= t.amount;
                }
            }
            let mut line = |category: &str, amount: i64| {
                if category == READY_TO_ASSIGN {
                    // In a credit account it changes nothing (SPEC 12.6).
                    if is_cash(kind) {
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
        let mut cells: HashMap<&str, BTreeMap<i32, Cell>> = HashMap::new();
        for (&cid, months) in &raw {
            let mut out = BTreeMap::new();
            let mut available = 0i64;
            for (&m, r) in months {
                let carryover = available.max(0);
                let spending: i64 = r.credit.values().map(|&n| (-n).max(0)).sum();
                let refunds: i64 = r.credit.values().map(|&n| n.max(0)).sum();
                let funds = carryover + r.assigned + r.cash + refunds;
                let funded = spending.min(funds.max(0));
                let mut left = funded;
                for (&ai, &net) in &r.credit {
                    let take = (-net).max(0).min(left);
                    left -= take;
                    let delta = take - net.max(0);
                    if delta != 0 {
                        *payment_activity.entry(ai).or_default().entry(m).or_insert(0) += delta;
                    }
                }
                let activity = r.cash + r.credit.values().sum::<i64>();
                available = carryover + r.assigned + activity;
                out.insert(m, Cell { assigned: r.assigned, activity, credit_overspending: spending - funded });
            }
            cells.insert(cid, out);
        }

        // Payment categories (SPEC 12.1, 12.4).
        for c in &plan.categories {
            let Some(account_id) = &c.payment_account_id else { continue };
            let mut map: BTreeMap<i32, Cell> = BTreeMap::new();
            if let Some(assigned) = plan.assigned.get(&c.id) {
                for (&m, &a) in assigned {
                    map.entry(m).or_default().assigned = a;
                }
            }
            if let Some(by_month) = order.get(account_id.as_str()).and_then(|ai| payment_activity.get(ai)) {
                for (&m, &a) in by_month {
                    map.entry(m).or_default().activity += a;
                }
            }
            cells.insert(c.id.as_str(), map);
        }

        Self { plan, cells, income, cash_advances }
    }

    /// A category's entry for `month` (SPEC 11.3 to 11.5, 12.1 to 12.3).
    pub fn entry(&self, category_id: &str, month: i32) -> Entry {
        let Some(cells) = self.cells.get(category_id) else { return Entry::default() };
        // `available` after the last month with figures; months between
        // carry it over if positive and start at 0 if not (SPEC 11.4).
        let mut available = 0i64;
        for cell in cells.range(..month).map(|(_, c)| c) {
            available = available.max(0) + cell.assigned + cell.activity;
        }
        let cell = cells.get(&month).copied().unwrap_or_default();
        let available = available.max(0) + cell.assigned + cell.activity;
        Entry {
            assigned: cell.assigned,
            activity: cell.activity,
            available,
            cash_overspending: (-available).max(0) - cell.credit_overspending,
            credit_overspending: cell.credit_overspending,
        }
    }

    pub fn entry_json(&self, category_id: &str, month: i32) -> Value {
        let e = self.entry(category_id, month);
        json!({
            "category_id": category_id,
            "assigned": e.assigned,
            "activity": e.activity,
            "available": e.available,
            "cash_overspending": e.cash_overspending,
            "credit_overspending": e.credit_overspending,
        })
    }

    /// Total cash overspending by month, over every category.
    fn cash_overspending(&self) -> BTreeMap<i32, i64> {
        let mut out = BTreeMap::new();
        for cells in self.cells.values() {
            let mut available = 0i64;
            for (&m, cell) in cells {
                available = available.max(0) + cell.assigned + cell.activity;
                let cash = (-available).max(0) - cell.credit_overspending;
                if cash != 0 {
                    *out.entry(m).or_insert(0) += cash;
                }
            }
        }
        out
    }

    /// net(M) = income + cash advances - assigned, by month (SPEC 11.7).
    fn net(&self) -> BTreeMap<i32, i64> {
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
    pub fn month_json(&self, month: i32) -> Value {
        let income = self.income.get(&month).copied().unwrap_or(0);
        let cash_advances = self.cash_advances.get(&month).copied().unwrap_or(0);

        let mut assigned = 0i64;
        let mut activity = 0i64;
        let mut categories = Vec::new();
        for g in &self.plan.groups {
            for cid in &g.category_ids {
                let Some(c) = self.plan.categories.iter().find(|c| &c.id == cid) else { continue };
                let e = self.entry(cid, month);
                assigned += e.assigned;
                if c.payment_account_id.is_none() {
                    activity += e.activity;
                }
                categories.push(self.entry_json(cid, month));
            }
        }

        let overspending = self.cash_overspending();
        let net = self.net();
        let cash_overspending_last_month = overspending.get(&(month - 1)).copied().unwrap_or(0);
        let left_over_from_last_month = net.range(..month).map(|(_, n)| n).sum::<i64>()
            - overspending.range(..month - 1).map(|(_, o)| o).sum::<i64>();
        let net_m = net.get(&month).copied().unwrap_or(0);
        let pool = left_over_from_last_month + net_m - cash_overspending_last_month;

        let mut running = 0i64;
        let mut shortfall = 0i64;
        for (_, n) in net.range(month + 1..) {
            running += n;
            shortfall = shortfall.max(-running);
        }
        let assigned_in_future = if pool < 0 { 0 } else { pool.min(shortfall) };
        let ready_to_assign = pool - assigned_in_future;

        json!({
            "month": month_string(month),
            "income": income,
            "cash_advances": cash_advances,
            "assigned": assigned,
            "activity": activity,
            "cash_overspending_last_month": cash_overspending_last_month,
            "left_over_from_last_month": left_over_from_last_month,
            "assigned_in_future": assigned_in_future,
            "ready_to_assign": ready_to_assign,
            "categories": categories,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_indices() {
        for s in ["1900-01", "2026-10", "2026-12", "2999-12"] {
            assert_eq!(month_string(month_index(s).unwrap()), s);
        }
        assert_eq!(month_index("2027-01").unwrap() - month_index("2026-12").unwrap(), 1);
    }
}
