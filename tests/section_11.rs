//! Months and Ready to Assign (SPEC 11), for plans with cash accounts only.

mod common;

use common::*;
use serde_json::{Value, json};

struct Fixture {
    s: Server,
    p: String,
    checking: String,
    groceries: String,
    rent: String,
    dining: String,
}

fn setup(starting: i64) -> Fixture {
    let s = start();
    let p = new_plan(&s);
    let checking = id(&expect(
        post(
            &s,
            &format!("/plans/{p}/accounts"),
            &format!(
                r#"{{"name": "Checking", "type": "checking", "starting_balance": {starting}, "starting_date": "2026-10-01"}}"#
            ),
        ),
        201,
    ));
    let g = id(&expect(post(&s, &format!("/plans/{p}/category_groups"), r#"{"name": "Everyday"}"#), 201));
    let cat = |name: &str| {
        id(&expect(
            post(&s, &format!("/plans/{p}/categories"), &format!(r#"{{"group_id": "{g}", "name": "{name}"}}"#)),
            201,
        ))
    };
    let groceries = cat("Groceries");
    let rent = cat("Rent");
    let dining = cat("Dining");
    Fixture { s, p, checking, groceries, rent, dining }
}

impl Fixture {
    fn assign(&self, month: &str, cat: &str, amount: i64) -> Value {
        expect(
            patch(
                &self.s,
                &format!("/plans/{}/months/{month}/categories/{cat}", self.p),
                &format!(r#"{{"assigned": {amount}}}"#),
            ),
            200,
        )
    }

    fn spend(&self, date: &str, cat: &str, amount: i64) -> Value {
        let body = json!({"account_id": self.checking, "date": date, "amount": amount, "category_id": cat});
        expect(post(&self.s, &format!("/plans/{}/transactions", self.p), &body.to_string()), 201)
    }

    fn month(&self, month: &str) -> Value {
        expect(get(&self.s, &format!("/plans/{}/months/{month}", self.p)), 200)
    }
}

fn entry(cat: &str, assigned: i64, activity: i64, available: i64, cash_over: i64) -> Value {
    json!({"category_id": cat, "assigned": assigned, "activity": activity, "available": available,
           "cash_overspending": cash_over, "credit_overspending": 0})
}

#[test]
fn example_11_8() {
    let f = setup(3000000);
    let e = f.assign("2026-10", &f.groceries, 400000);
    assert_eq!(e, entry(&f.groceries, 400000, 0, 400000, 0));
    f.assign("2026-10", &f.rent, 1200000);
    f.assign("2026-10", &f.dining, 50000);
    f.assign("2026-11", &f.rent, 1200000);
    f.spend("2026-10-04", &f.groceries, -120000);
    f.spend("2026-10-11", &f.dining, -80000);

    assert_eq!(
        f.month("2026-10"),
        json!({"month": "2026-10", "income": 3000000, "cash_advances": 0, "assigned": 1650000,
               "activity": -200000, "cash_overspending_last_month": 0, "left_over_from_last_month": 0,
               "assigned_in_future": 1200000, "ready_to_assign": 150000,
               "categories": [entry(&f.groceries, 400000, -120000, 280000, 0),
                              entry(&f.rent, 1200000, 0, 1200000, 0),
                              entry(&f.dining, 50000, -80000, -30000, 30000)]})
    );
    assert_eq!(
        f.month("2026-11"),
        json!({"month": "2026-11", "income": 0, "cash_advances": 0, "assigned": 1200000,
               "activity": 0, "cash_overspending_last_month": 30000, "left_over_from_last_month": 1350000,
               "assigned_in_future": 0, "ready_to_assign": 120000,
               "categories": [entry(&f.groceries, 0, 0, 280000, 0),
                              entry(&f.rent, 1200000, 0, 2400000, 0),
                              entry(&f.dining, 0, 0, 0, 0)]})
    );
    // December onwards: everything rolls over.
    let dec = f.month("2027-03");
    assert_eq!(dec["left_over_from_last_month"], 120000);
    assert_eq!(dec["ready_to_assign"], 120000);
    assert_eq!(dec["cash_overspending_last_month"], 0);
    let g = expect(get(&f.s, &format!("/plans/{}/months/2027-03/categories/{}", f.p, f.rent)), 200);
    assert_eq!(g, entry(&f.rent, 0, 0, 2400000, 0));
    // Before any data.
    let early = f.month("1900-01");
    assert_eq!(early["ready_to_assign"], 0);
    assert_eq!(early["assigned_in_future"], 0);
}

#[test]
fn negative_ready_to_assign_shows_in_the_month_that_runs_out() {
    let f = setup(1000000);
    f.assign("2026-12", &f.rent, 1500000);
    let oct = f.month("2026-10");
    assert_eq!(oct["assigned_in_future"], 1000000);
    assert_eq!(oct["ready_to_assign"], 0);
    let dec = f.month("2026-12");
    assert_eq!(dec["left_over_from_last_month"], 1000000);
    assert_eq!(dec["ready_to_assign"], -500000);
    assert_eq!(dec["assigned_in_future"], 0);
}

#[test]
fn income_only_counts_cash_lines_and_splits() {
    let f = setup(0);
    let p = &f.p;
    let tracking = id(&expect(
        post(
            &f.s,
            &format!("/plans/{p}/accounts"),
            r#"{"name": "House", "type": "otherAsset", "starting_balance": 9000, "starting_date": "2026-10-02"}"#,
        ),
        201,
    ));
    let _ = tracking;
    let body = json!({"account_id": f.checking, "date": "2026-10-06", "amount": 100000,
        "subtransactions": [{"amount": 150000, "category_id": "ready_to_assign"},
                            {"amount": -50000, "category_id": f.groceries}]});
    expect(post(&f.s, &format!("/plans/{p}/transactions"), &body.to_string()), 201);
    let oct = f.month("2026-10");
    assert_eq!(oct["income"], 150000);
    assert_eq!(oct["activity"], -50000);
    assert_eq!(oct["ready_to_assign"], 150000);
    assert_eq!(oct["categories"][0], entry(&f.groceries, 0, -50000, -50000, 50000));
    let nov = f.month("2026-11");
    assert_eq!(nov["cash_overspending_last_month"], 50000);
    assert_eq!(nov["ready_to_assign"], 100000);
}

#[test]
fn scoping_hidden_and_payment_categories() {
    let f = setup(500000);
    let p = &f.p;
    let other = new_plan(&f.s);
    err(&get(&f.s, &format!("/plans/{other}/months/2026-10/categories/{}", f.groceries)), 404);
    err(&patch(&f.s, &format!("/plans/{other}/months/2026-10/categories/{}", f.groceries), r#"{"assigned": 1}"#), 404);

    expect(patch(&f.s, &format!("/plans/{p}/categories/{}", f.rent), r#"{"hidden": true}"#), 200);
    f.assign("2026-10", &f.rent, 100000);
    let oct = f.month("2026-10");
    assert_eq!(oct["categories"].as_array().unwrap().len(), 3);
    assert_eq!(oct["ready_to_assign"], 400000);

    let visa = expect(post(&f.s, &format!("/plans/{p}/accounts"), r#"{"name": "Visa", "type": "creditCard"}"#), 201);
    let pay = visa["payment_category_id"].as_str().unwrap().to_string();
    assert_eq!(f.assign("2026-10", &pay, 70000), entry(&pay, 70000, 0, 70000, 0));
    let oct = f.month("2026-10");
    assert_eq!(oct["categories"][0]["category_id"], pay.as_str());
    assert_eq!(oct["assigned"], 170000);
    assert_eq!(delete(&f.s, &format!("/plans/{p}/accounts/{}", id(&visa))).status, 204);
    let oct = f.month("2026-10");
    assert_eq!(oct["assigned"], 100000);
    assert_eq!(oct["ready_to_assign"], 400000);
}

#[test]
fn errors() {
    let f = setup(0);
    let p = &f.p;
    let g = &f.groceries;
    err(&get(&f.s, &format!("/plans/{p}/months/2026-13")), 400);
    err(&get(&f.s, &format!("/plans/{p}/months/1899-12")), 400);
    err(&get(&f.s, "/plans/nope/months/2026-10"), 404);
    err(&get(&f.s, &format!("/plans/{p}/months/2026-10/categories/ready_to_assign")), 404);
    err(&patch(&f.s, &format!("/plans/{p}/months/2026-10/categories/ready_to_assign"), r#"{"assigned": 1}"#), 404);
    err(&get(&f.s, &format!("/plans/{p}/months/bad/categories/nope")), 404);
    err(&get(&f.s, &format!("/plans/{p}/months/bad/categories/{g}")), 400);
    err(&req(&f.s, "POST", &format!("/plans/{p}/months/2026-10"), Some(b"{}")), 405);
    err(&req(&f.s, "DELETE", &format!("/plans/{p}/months/2026-10/categories/{g}"), None), 405);
    let path = format!("/plans/{p}/months/2026-10/categories/{g}");
    for body in [r#"{"assigned": null}"#, r#"{"assigned": 1.5}"#, r#"{"assigned": "1"}"#, "", "[]"] {
        err(&patch(&f.s, &path, body), 400);
    }
    assert_eq!(expect(patch(&f.s, &path, r#"{"other": 1}"#), 200), entry(g, 0, 0, 0, 0));
    assert_eq!(f.assign("2026-10", g, -5000), entry(g, -5000, 0, -5000, 5000));
    assert_eq!(f.assign("2026-10", g, -0), entry(g, 0, 0, 0, 0));
    let head = req(&f.s, "HEAD", &path, None);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
}
