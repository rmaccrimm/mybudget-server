//! Deleting categories (SPEC 13), on the plan of SPEC 11.8.

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

/// SPEC 11.8's plan.
fn setup() -> Fixture {
    let s = start();
    let p = new_plan(&s);
    let body =
        r#"{"name": "Checking", "type": "checking", "starting_balance": 3000000, "starting_date": "2026-10-01"}"#;
    let checking = id(&expect(post(&s, &format!("/plans/{p}/accounts"), body), 201));
    let g = id(&expect(post(&s, &format!("/plans/{p}/category_groups"), r#"{"name": "Everyday"}"#), 201));
    let cat = |name: &str| {
        let body = json!({"group_id": g, "name": name}).to_string();
        id(&expect(post(&s, &format!("/plans/{p}/categories"), &body), 201))
    };
    let (groceries, rent, dining) = (cat("Groceries"), cat("Rent"), cat("Dining"));
    let f = Fixture { s, p, checking, groceries, rent, dining };
    f.assign("2026-10", &f.groceries, 400000);
    f.assign("2026-10", &f.rent, 1200000);
    f.assign("2026-10", &f.dining, 50000);
    f.assign("2026-11", &f.rent, 1200000);
    f.transaction(json!({"date": "2026-10-04", "amount": -120000, "category_id": f.groceries}));
    f.transaction(json!({"date": "2026-10-11", "amount": -80000, "category_id": f.dining}));
    f
}

impl Fixture {
    fn path(&self, rest: &str) -> String {
        format!("/plans/{}/{rest}", self.p)
    }

    fn assign(&self, month: &str, cat: &str, amount: i64) {
        let body = json!({"assigned": amount}).to_string();
        expect(patch(&self.s, &self.path(&format!("months/{month}/categories/{cat}")), &body), 200);
    }

    fn transaction(&self, mut body: Value) -> Value {
        body["account_id"] = json!(self.checking);
        expect(post(&self.s, &self.path("transactions"), &body.to_string()), 201)
    }

    fn delete(&self, cat: &str, query: &str) -> Resp {
        delete(&self.s, &self.path(&format!("categories/{cat}{query}")))
    }

    fn get(&self, rest: &str) -> Resp {
        get(&self.s, &self.path(rest))
    }

    fn category_ids(&self) -> Vec<String> {
        let groups = expect(self.get("categories"), 200);
        groups.as_array().unwrap().iter().flat_map(|g| g["categories"].as_array().unwrap().iter().map(id)).collect()
    }
}

#[test]
fn in_use_needs_reassign_to() {
    let f = setup();
    err(&f.delete(&f.dining, ""), 409);
    assert_eq!(f.category_ids(), [f.groceries.as_str(), &f.rent, &f.dining]);
}

#[test]
fn reassigns_transactions_and_assigned() {
    let f = setup();
    let split = f.transaction(json!({"date": "2026-10-12", "amount": -10000, "subtransactions": [
        {"amount": -4000, "category_id": f.dining, "memo": "tip"},
        {"amount": -6000, "category_id": f.rent},
    ]}));
    assert_eq!(f.delete(&f.dining, &format!("?reassign_to={}", f.groceries)).status, 204);

    let entry = expect(f.get(&format!("months/2026-10/categories/{}", f.groceries)), 200);
    assert_eq!(
        entry,
        json!({"category_id": f.groceries, "assigned": 450000, "activity": -204000, "available": 246000,
               "cash_overspending": 0, "credit_overspending": 0})
    );
    let split = expect(f.get(&format!("transactions/{}", id(&split))), 200);
    assert_eq!(split["subtransactions"][0], json!({"amount": -4000, "category_id": f.groceries, "memo": "tip"}));
    assert_eq!(split["subtransactions"][1]["category_id"], json!(f.rent));

    let nov = expect(f.get("months/2026-11"), 200);
    assert_eq!(nov["cash_overspending_last_month"], 0);
    assert_eq!(nov["categories"].as_array().unwrap().len(), 2);
    assert_eq!(f.category_ids(), [f.groceries.as_str(), &f.rent]);
}

#[test]
fn deleted_category_is_gone() {
    let f = setup();
    assert_eq!(f.delete(&f.dining, &format!("?reassign_to={}", f.rent)).status, 204);
    err(&f.get(&format!("categories/{}", f.dining)), 404);
    err(&f.get(&format!("months/2026-10/categories/{}", f.dining)), 404);
    err(&f.delete(&f.dining, ""), 404);
    err(&f.delete(&f.groceries, &format!("?reassign_to={}", f.dining)), 400);
    let body = json!({"account_id": f.checking, "date": "2026-10-04", "amount": -1, "category_id": f.dining});
    err(&post(&f.s, &f.path("transactions"), &body.to_string()), 400);
}

#[test]
fn unused_category_without_reassign_drops_assigned() {
    let f = setup();
    assert_eq!(f.delete(&f.rent, "").status, 204);
    let oct = expect(f.get("months/2026-10"), 200);
    assert_eq!(oct["assigned"], 450000);
    assert_eq!(oct["assigned_in_future"], 0);
}

#[test]
fn invalid_reassign_to() {
    let f = setup();
    let visa = r#"{"name": "Visa", "type": "creditCard"}"#;
    let visa = expect(post(&f.s, &f.path("accounts"), visa), 201);
    let payment = visa["payment_category_id"].as_str().unwrap();
    for target in [f.dining.as_str(), "ready_to_assign", payment, "nope"] {
        err(&f.delete(&f.dining, &format!("?reassign_to={target}")), 400);
    }
    // Every value is checked; the last counts (SPEC 3.5).
    err(&f.delete(&f.dining, &format!("?reassign_to=nope&reassign_to={}", f.rent)), 400);
    assert_eq!(f.delete(&f.dining, &format!("?reassign_to={}&reassign_to={}", f.rent, f.groceries)).status, 204);
    let entry = expect(f.get(&format!("months/2026-10/categories/{}", f.groceries)), 200);
    assert_eq!(entry["assigned"], 450000);

    err(&f.delete(payment, ""), 400);
    err(&f.delete("ready_to_assign", ""), 404);
    err(&req(&f.s, "PUT", &f.path(&format!("categories/{}", f.rent)), Some(b"{}")), 405);
}
