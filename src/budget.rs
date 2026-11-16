//! Months and Ready to Assign (SPEC 11).
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

/// Income counts only in cash accounts (SPEC 11.6); in a credit account
/// `ready_to_assign` doesn't touch the plan (SPEC 8.2).
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

#[derive(Default, Clone, Copy)]
struct Cell {
    assigned: i64,
    activity: i64,
}

/// The plan's months, computed from its assignments and lines.
pub struct Budget<'a> {
    plan: &'a Plan,
    /// Per category id, the months it has an assignment or activity in.
    cells: HashMap<&'a str, BTreeMap<i32, Cell>>,
    /// Income by month (SPEC 11.6).
    income: BTreeMap<i32, i64>,
}

impl<'a> Budget<'a> {
    pub fn new(plan: &'a Plan) -> Self {
        let mut cells: HashMap<&str, BTreeMap<i32, Cell>> = HashMap::new();
        for c in &plan.categories {
            let mut map = BTreeMap::new();
            if let Some(assigned) = plan.assigned.get(&c.id) {
                for (&m, &a) in assigned {
                    map.insert(m, Cell { assigned: a, activity: 0 });
                }
            }
            cells.insert(c.id.as_str(), map);
        }
        let mut income = BTreeMap::new();
        let kinds: HashMap<&str, AccountType> = plan.accounts.iter().map(|a| (a.id.as_str(), a.kind)).collect();
        for t in &plan.transactions {
            let Some(&kind) = kinds.get(t.account_id.as_str()) else { continue };
            // Lines (SPEC 11.1): tracking accounts never affect the plan.
            if !kind.on_budget() {
                continue;
            }
            let month = date_month(&t.date);
            let mut line = |category: &str, amount: i64| {
                if category == READY_TO_ASSIGN {
                    if is_cash(kind) {
                        *income.entry(month).or_insert(0) += amount;
                    }
                } else if let Some(map) = cells.get_mut(category) {
                    map.entry(month).or_default().activity += amount;
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
        Self { plan, cells, income }
    }

    /// A category's entry for `month` (SPEC 11.3 to 11.5).
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
        // Only cash accounts are covered by section 11, so all overspending
        // is cash overspending (SPEC 11.5).
        let credit_overspending = 0;
        Entry {
            assigned: cell.assigned,
            activity: cell.activity,
            available,
            cash_overspending: (-available).max(0) - credit_overspending,
            credit_overspending,
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
                if available < 0 {
                    *out.entry(m).or_insert(0) -= available;
                }
            }
        }
        out
    }

    /// net(M) = income + cash advances - assigned, by month (SPEC 11.7).
    fn net(&self) -> BTreeMap<i32, i64> {
        let mut net = self.income.clone();
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
        let cash_advances = 0i64;

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
