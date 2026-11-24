//! Credit cards (SPEC 12), together with months (SPEC 11).

mod common;

use common::*;
use serde_json::{Value, json};

struct Fx {
    s: Server,
    p: String,
}

impl Fx {
    fn new() -> Self {
        let s = start();
        let p = new_plan(&s);
        Fx { s, p }
    }

    fn account(&self, name: &str, kind: &str, starting: i64) -> Value {
        let body = json!({"name": name, "type": kind, "starting_balance": starting, "starting_date": "2026-10-01"});
        expect(
            post(
                &self.s,
                &format!("/plans/{}/accounts", self.p),
                &body.to_string(),
            ),
            201,
        )
    }

    fn category(&self, name: &str) -> String {
        let g = id(&expect(
            post(
                &self.s,
                &format!("/plans/{}/category_groups", self.p),
                r#"{"name": "G"}"#,
            ),
            201,
        ));
        let body = json!({"group_id": g, "name": name});
        id(&expect(
            post(
                &self.s,
                &format!("/plans/{}/categories", self.p),
                &body.to_string(),
            ),
            201,
        ))
    }

    fn assign(&self, month: &str, cat: &str, amount: i64) {
        expect(
            patch(
                &self.s,
                &format!("/plans/{}/months/{month}/categories/{cat}", self.p),
                &format!(r#"{{"assigned": {amount}}}"#),
            ),
            200,
        );
    }

    fn txn(&self, body: Value) -> Value {
        expect(
            post(
                &self.s,
                &format!("/plans/{}/transactions", self.p),
                &body.to_string(),
            ),
            201,
        )
    }

    fn month(&self, month: &str) -> Value {
        expect(
            get(&self.s, &format!("/plans/{}/months/{month}", self.p)),
            200,
        )
    }
}

fn entry(cat: &str, assigned: i64, activity: i64, available: i64, cash: i64, credit: i64) -> Value {
    json!({"category_id": cat, "assigned": assigned, "activity": activity, "available": available,
           "cash_overspending": cash, "credit_overspending": credit,
           "target": null, "due": null, "underfunded": null})
}

#[test]
fn example_12_7() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 1000000));
    let visa = f.account("Visa", "creditCard", -200000);
    let visa_id = id(&visa);
    let pay = visa["payment_category_id"].as_str().unwrap().to_owned();
    let visa_payee = visa["transfer_payee_id"].as_str().unwrap().to_owned();
    let groceries = f.category("Groceries");
    f.assign("2026-10", &groceries, 300000);
    f.assign("2026-10", &pay, 200000);
    for (date, amount) in [("2026-10-05", -100000), ("2026-10-09", -250000)] {
        f.txn(json!({"account_id": visa_id, "date": date, "amount": amount, "category_id": groceries}));
    }
    f.txn(json!({"account_id": checking, "date": "2026-10-20", "amount": -200000, "payee_id": visa_payee}));
    f.txn(json!({"account_id": visa_id, "date": "2026-11-03", "amount": 30000, "category_id": groceries}));

    assert_eq!(
        f.month("2026-10"),
        json!({"month": "2026-10", "income": 1000000, "cash_advances": 0, "assigned": 500000,
               "activity": -350000, "cash_overspending_last_month": 0, "left_over_from_last_month": 0,
               "assigned_in_future": 0, "ready_to_assign": 500000, "underfunded": 0,
               "uncategorized": no_uncategorized(),
               "categories": [entry(&pay, 200000, 100000, 300000, 0, 0),
                              entry(&groceries, 300000, -350000, -50000, 0, 50000)]})
    );
    assert_eq!(
        f.month("2026-11"),
        json!({"month": "2026-11", "income": 0, "cash_advances": 0, "assigned": 0,
               "activity": 30000, "cash_overspending_last_month": 0, "left_over_from_last_month": 500000,
               "assigned_in_future": 0, "ready_to_assign": 500000, "underfunded": 0,
               "uncategorized": no_uncategorized(),
               "categories": [entry(&pay, 0, -30000, 270000, 0, 0),
                              entry(&groceries, 0, 30000, 30000, 0, 0)]})
    );
}

#[test]
fn funding_order_cash_first_and_cash_advances() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 1000000));
    let a = f.account("A", "creditCard", 0);
    let b = f.account("B", "lineOfCredit", 0);
    let (a_id, b_id) = (id(&a), id(&b));
    let pa = a["payment_category_id"].as_str().unwrap().to_owned();
    let pb = b["payment_category_id"].as_str().unwrap().to_owned();
    let c = f.category("Food");
    f.assign("2026-10", &c, 100000);
    // Cash spends 30, B spends 50, A spends 50 and refunds 10 in the month.
    f.txn(
        json!({"account_id": checking, "date": "2026-10-02", "amount": -30000, "category_id": c}),
    );
    f.txn(json!({"account_id": b_id, "date": "2026-10-03", "amount": -50000, "category_id": c}));
    f.txn(json!({"account_id": a_id, "date": "2026-10-04", "amount": -50000, "category_id": c}));
    f.txn(json!({"account_id": a_id, "date": "2026-10-05", "amount": 10000, "category_id": c}));
    // funds = 100 - 30 = 70; spending A 40, B 50; A takes 40, B takes 30.
    let oct = f.month("2026-10");
    assert_eq!(oct["categories"][0], entry(&pa, 0, 40000, 40000, 0, 0));
    assert_eq!(oct["categories"][1], entry(&pb, 0, 30000, 30000, 0, 0));
    assert_eq!(
        oct["categories"][2],
        entry(&c, 100000, -120000, -20000, 0, 20000)
    );

    // A cash advance from B into checking adds to Ready to Assign.
    f.txn(json!({"account_id": checking, "date": "2026-10-06", "amount": 25000, "payee_id": b["transfer_payee_id"]}));
    let oct = f.month("2026-10");
    assert_eq!(oct["cash_advances"], 25000);
    assert_eq!(oct["ready_to_assign"], 1000000 + 25000 - 100000);
    // Transfers between two credit accounts change nothing.
    f.txn(json!({"account_id": a_id, "date": "2026-10-07", "amount": -5000, "payee_id": b["transfer_payee_id"]}));
    assert_eq!(f.month("2026-10"), oct);
}

#[test]
fn negative_funds_are_cash_overspending() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 1000000));
    let a = f.account("A", "creditCard", 0);
    let c = f.category("Food");
    f.assign("2026-10", &c, 10000);
    f.txn(
        json!({"account_id": checking, "date": "2026-10-02", "amount": -30000, "category_id": c}),
    );
    f.txn(json!({"account_id": id(&a), "date": "2026-10-03", "amount": -50000, "category_id": c}));
    let oct = f.month("2026-10");
    assert_eq!(
        oct["categories"][1],
        entry(&c, 10000, -80000, -70000, 20000, 50000)
    );
    assert_eq!(oct["categories"][0]["activity"], 0);
    let nov = f.month("2026-11");
    assert_eq!(nov["cash_overspending_last_month"], 20000);
}
