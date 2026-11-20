//! Uncategorized transactions (SPEC 16), and the earlier clauses that now
//! allow them (SPEC 8.2, 9.3, 9.5, 10.2, 10.4).

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

    fn path(&self, rest: &str) -> String {
        format!("/plans/{}/{rest}", self.p)
    }

    fn account(&self, name: &str, kind: &str, starting: i64) -> Value {
        let body = json!({"name": name, "type": kind, "starting_balance": starting, "starting_date": "2026-10-01"});
        expect(
            post(&self.s, &self.path("accounts"), &body.to_string()),
            201,
        )
    }

    fn category(&self, name: &str) -> String {
        let g = id(&expect(
            post(&self.s, &self.path("category_groups"), r#"{"name": "G"}"#),
            201,
        ));
        let body = json!({"group_id": g, "name": name});
        id(&expect(
            post(&self.s, &self.path("categories"), &body.to_string()),
            201,
        ))
    }

    fn assign(&self, month: &str, cat: &str, amount: i64) {
        expect(
            patch(
                &self.s,
                &self.path(&format!("months/{month}/categories/{cat}")),
                &format!(r#"{{"assigned": {amount}}}"#),
            ),
            200,
        );
    }

    fn txn(&self, body: Value) -> Value {
        expect(
            post(&self.s, &self.path("transactions"), &body.to_string()),
            201,
        )
    }

    fn edit(&self, txn: &str, body: Value) -> Resp {
        patch(
            &self.s,
            &self.path(&format!("transactions/{txn}")),
            &body.to_string(),
        )
    }

    fn month(&self, month: &str) -> Value {
        expect(get(&self.s, &self.path(&format!("months/{month}"))), 200)
    }

    fn uncategorized_ids(&self, flag: &str) -> Vec<String> {
        let list = expect(
            get(
                &self.s,
                &self.path(&format!("transactions?uncategorized={flag}")),
            ),
            200,
        );
        list.as_array().unwrap().iter().map(id).collect()
    }
}

fn entry(
    cat: Option<&str>,
    assigned: i64,
    activity: i64,
    available: i64,
    cash: i64,
    credit: i64,
) -> Value {
    json!({"category_id": cat, "assigned": assigned, "activity": activity, "available": available,
           "cash_overspending": cash, "credit_overspending": credit})
}

#[test]
fn example_16_5() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 1000000));
    let visa = f.account("Visa", "creditCard", 0);
    let pay = visa["payment_category_id"].as_str().unwrap().to_owned();
    let groceries = f.category("Groceries");
    f.assign("2026-10", &groceries, 300000);
    let spend = f.txn(json!({"account_id": checking, "date": "2026-10-10", "amount": -50000}));
    assert_eq!(spend["category_id"], Value::Null);
    f.txn(json!({"account_id": id(&visa), "date": "2026-10-12", "amount": -40000, "category_id": null}));
    f.txn(json!({"account_id": checking, "date": "2026-10-15", "amount": 20000}));

    assert_eq!(
        f.month("2026-10"),
        json!({"month": "2026-10", "income": 1000000, "cash_advances": 0, "assigned": 300000,
               "activity": -70000, "cash_overspending_last_month": 0, "left_over_from_last_month": 0,
               "assigned_in_future": 0, "ready_to_assign": 700000,
               "uncategorized": entry(None, 0, -70000, -70000, 30000, 40000),
               "categories": [entry(Some(&pay), 0, 0, 0, 0, 0),
                              entry(Some(&groceries), 300000, 0, 300000, 0, 0)]})
    );
    assert_eq!(
        f.month("2026-11"),
        json!({"month": "2026-11", "income": 0, "cash_advances": 0, "assigned": 0,
               "activity": 0, "cash_overspending_last_month": 30000, "left_over_from_last_month": 700000,
               "assigned_in_future": 0, "ready_to_assign": 670000,
               "uncategorized": no_uncategorized(),
               "categories": [entry(Some(&pay), 0, 0, 0, 0, 0),
                              entry(Some(&groceries), 0, 0, 300000, 0, 0)]})
    );

    expect(f.edit(&id(&spend), json!({"category_id": groceries})), 200);
    let oct = f.month("2026-10");
    assert_eq!(
        oct["uncategorized"],
        entry(None, 0, -20000, -20000, 0, 20000)
    );
    assert_eq!(
        oct["categories"],
        json!([
            entry(Some(&pay), 0, 20000, 20000, 0, 0),
            entry(Some(&groceries), 300000, -50000, 250000, 0, 0)
        ])
    );
    let nov = f.month("2026-11");
    assert_eq!(nov["cash_overspending_last_month"], 0);
    assert_eq!(nov["ready_to_assign"], 700000);
}

#[test]
fn uncategorized_inflow_carries_over_and_isnt_income() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 0));
    f.txn(json!({"account_id": checking, "date": "2026-10-03", "amount": 25000}));
    let oct = f.month("2026-10");
    assert_eq!(oct["income"], 0);
    assert_eq!(oct["ready_to_assign"], 0);
    assert_eq!(oct["uncategorized"], entry(None, 0, 25000, 25000, 0, 0));
    assert_eq!(
        f.month("2027-01")["uncategorized"],
        entry(None, 0, 0, 25000, 0, 0)
    );
}

#[test]
fn which_transactions_are_uncategorized() {
    let f = Fx::new();
    let checking = f.account("Checking", "checking", 0);
    let savings = f.account("Savings", "savings", 0);
    let house = f.account("House", "otherAsset", 0);
    let food = f.category("Food");
    let field = |a: &Value, k: &str| a[k].as_str().unwrap().to_owned();
    let checking_id = id(&checking);

    let plain =
        id(&f.txn(json!({"account_id": checking_id, "date": "2026-10-02", "amount": -1000})));
    // A transfer between two on-budget accounts has no category, but isn't
    // uncategorized.
    let between = id(&f.txn(
        json!({"account_id": checking_id, "date": "2026-10-03", "amount": -2000,
                                   "payee_id": field(&savings, "transfer_payee_id")}),
    ));
    // The on-budget half of a transfer to a tracking account can be.
    let out = f.txn(
        json!({"account_id": checking_id, "date": "2026-10-04", "amount": -3000,
                           "payee_id": field(&house, "transfer_payee_id")}),
    );
    let tracking_half = field(&out, "transfer_transaction_id");
    let tracking =
        id(&f.txn(json!({"account_id": id(&house), "date": "2026-10-05", "amount": 4000})));
    let categorized = id(&f.txn(
        json!({"account_id": checking_id, "date": "2026-10-06", "amount": -5000,
                                       "category_id": food}),
    ));

    assert_eq!(f.uncategorized_ids("true"), vec![plain.clone(), id(&out)]);
    let others = f.uncategorized_ids("false");
    for t in [&between, &tracking_half, &tracking, &categorized] {
        assert!(others.contains(t), "{t} in {others:?}");
    }
    assert!(!others.contains(&plain));
    err(&get(&f.s, &f.path("transactions?uncategorized=yes")), 400);
    // Uncategorized's activity: -1000 and -3000; the transfer between
    // on-budget accounts and the tracking account's lines don't count.
    assert_eq!(f.month("2026-10")["uncategorized"]["activity"], -4000);
}

#[test]
fn splits_need_categories_and_unsplitting_leaves_it_uncategorized() {
    let f = Fx::new();
    let checking = id(&f.account("Checking", "checking", 0));
    let food = f.category("Food");
    let body = json!({"account_id": checking, "date": "2026-10-02", "amount": -1000,
                      "subtransactions": [{"amount": -1000, "category_id": null}]});
    err(&post(&f.s, &f.path("transactions"), &body.to_string()), 400);
    let body = json!({"account_id": checking, "date": "2026-10-02", "amount": -1000,
                      "subtransactions": [{"amount": -1000}]});
    err(&post(&f.s, &f.path("transactions"), &body.to_string()), 400);

    let split = id(&f.txn(
        json!({"account_id": checking, "date": "2026-10-02", "amount": -1000,
                                 "subtransactions": [{"amount": -400, "category_id": food},
                                                     {"amount": -600, "category_id": food}]}),
    ));
    let t = expect(f.edit(&split, json!({"subtransactions": []})), 200);
    assert_eq!(t["category_id"], Value::Null);
    assert_eq!(f.uncategorized_ids("true"), vec![split]);
}

#[test]
fn transfer_to_tracking_keeps_or_loses_its_category() {
    let f = Fx::new();
    let checking = f.account("Checking", "checking", 0);
    let house = f.account("House", "otherAsset", 0);
    let food = f.category("Food");
    let house_payee = house["transfer_payee_id"].as_str().unwrap();
    let t = id(&f.txn(json!({"account_id": id(&checking), "date": "2026-10-02", "amount": -1000})));
    // Becoming a transfer to a tracking account keeps its null category.
    let changed = expect(f.edit(&t, json!({"payee_id": house_payee})), 200);
    assert_eq!(changed["category_id"], Value::Null);
    assert_eq!(f.uncategorized_ids("true"), vec![t.clone()]);
    // Leaving the transfer keeps the category it's given, or none (9.5).
    expect(
        f.edit(&t, json!({"payee_id": house_payee, "category_id": food})),
        200,
    );
    let plain = expect(f.edit(&t, json!({"payee_id": null})), 200);
    assert_eq!(plain["category_id"], food);
}
