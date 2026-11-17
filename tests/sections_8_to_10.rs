//! Transactions, transfers and splits (SPEC 8 to 10), and the earlier
//! clauses they make testable.

mod common;

use common::*;
use serde_json::{Value, json};

struct Fixture {
    s: Server,
    p: String,
    checking: String,
    checking_payee: String,
    savings: String,
    savings_payee: String,
    asset: String,
    asset_payee: String,
    groceries: String,
    household: String,
    visa_payment: String,
}

fn account(s: &Server, p: &str, body: &str) -> Value {
    expect(post(s, &format!("/plans/{p}/accounts"), body), 201)
}

fn setup() -> Fixture {
    let s = start();
    let p = new_plan(&s);
    let c = account(
        &s,
        &p,
        r#"{"name": "Checking", "type": "checking", "starting_balance": 2500000, "starting_date": "2026-10-01"}"#,
    );
    let sv = account(&s, &p, r#"{"name": "Savings", "type": "savings"}"#);
    let a = account(&s, &p, r#"{"name": "House", "type": "otherAsset"}"#);
    let v = account(&s, &p, r#"{"name": "Visa", "type": "creditCard"}"#);
    let g = id(&expect(post(&s, &format!("/plans/{p}/category_groups"), r#"{"name": "Everyday"}"#), 201));
    let cat = |name: &str| {
        id(&expect(
            post(&s, &format!("/plans/{p}/categories"), &format!(r#"{{"group_id": "{g}", "name": "{name}"}}"#)),
            201,
        ))
    };
    let groceries = cat("Groceries");
    let household = cat("Household");
    Fixture {
        checking: id(&c),
        checking_payee: c["transfer_payee_id"].as_str().unwrap().into(),
        savings: id(&sv),
        savings_payee: sv["transfer_payee_id"].as_str().unwrap().into(),
        asset: id(&a),
        asset_payee: a["transfer_payee_id"].as_str().unwrap().into(),
        visa_payment: v["payment_category_id"].as_str().unwrap().into(),
        groceries,
        household,
        s,
        p,
    }
}

impl Fixture {
    fn txns(&self) -> String {
        format!("/plans/{}/transactions", self.p)
    }
    fn txn(&self, id: &str) -> String {
        format!("/plans/{}/transactions/{id}", self.p)
    }
    fn create(&self, body: &str) -> Value {
        expect(post(&self.s, &self.txns(), body), 201)
    }
    fn get(&self, id: &str) -> Value {
        expect(get(&self.s, &self.txn(id)), 200)
    }
    fn patch(&self, id: &str, body: &str) -> Resp {
        patch(&self.s, &self.txn(id), body)
    }
    fn account(&self, id: &str) -> Value {
        expect(get(&self.s, &format!("/plans/{}/accounts/{id}", self.p)), 200)
    }
}

#[test]
fn create_list_and_patch() {
    let f = setup();
    let (c, g) = (&f.checking, &f.groceries);
    let shop = f.create(&format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": -120000, "payee_name": "Corner Shop", "category_id": "{g}", "memo": "weekly shop"}}"#
    ));
    let sid = id(&shop);
    let payee = shop["payee_id"].as_str().unwrap().to_string();
    assert_eq!(
        shop,
        json!({"id": sid, "account_id": c, "date": "2026-10-04", "amount": -120000,
               "payee_id": payee, "category_id": g, "memo": "weekly shop",
               "cleared": "uncleared", "approved": true, "flag_color": null,
               "transfer_account_id": null, "transfer_transaction_id": null, "subtransactions": []})
    );
    let pay = f.create(&format!(
        r#"{{"account_id": "{c}", "date": "2026-10-15", "amount": 1800000, "payee_name": "Employer", "category_id": "ready_to_assign"}}"#
    ));
    // The same payee name is found, not created again.
    let again = f.create(&format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": -1000, "payee_name": "Corner Shop", "category_id": "{g}"}}"#
    ));
    assert_eq!(again["payee_id"], payee.as_str());

    let all = expect(get(&f.s, &f.txns()), 200);
    let ids: Vec<&str> = all.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 4);
    // The starting balance transaction comes first (SPEC 5.3).
    assert_eq!(all[0]["amount"], 2500000);
    assert_eq!(all[0]["category_id"], "ready_to_assign");
    assert_eq!(all[0]["memo"], Value::Null);
    assert_eq!(all[0]["approved"], true);
    assert_eq!(all[0]["cleared"], "cleared");
    assert_eq!(&ids[1..], &[sid.as_str(), again["id"].as_str().unwrap(), pay["id"].as_str().unwrap()]);

    let filtered = expect(
        get(&f.s, &format!("{}?account_id={c}&since_date=2026-10-02&until_date=2026-10-10&approved=true", f.txns())),
        200,
    );
    assert_eq!(filtered.as_array().unwrap().len(), 2);
    assert_eq!(filtered[0], shop);
    err(&get(&f.s, &format!("{}?since_date=2026-02-30", f.txns())), 400);
    err(&get(&f.s, &format!("{}?approved=yes", f.txns())), 400);
    assert_eq!(expect(get(&f.s, &format!("{}?payee_id=nope", f.txns())), 200), json!([]));

    let patched = expect(f.patch(&sid, r#"{"cleared": "cleared", "flag_color": "green"}"#), 200);
    assert_eq!(patched["cleared"], "cleared");
    assert_eq!(patched["flag_color"], "green");
    assert_eq!(patched["category_id"], g.as_str());
    assert_eq!(expect(f.patch(&sid, "{}"), 200), patched);

    // Balances (SPEC 5.4).
    let acct = f.account(c);
    assert_eq!(acct["balance"], 2500000 - 120000 + 1800000 - 1000);
    assert_eq!(acct["cleared_balance"], 2500000 - 120000);
    assert_eq!(acct["uncleared_balance"], 1800000 - 1000);

    // Payee in use (SPEC 6.4), account with transactions (5.8, 5.9).
    err(&delete(&f.s, &format!("/plans/{}/payees/{payee}", f.p)), 409);
    err(&delete(&f.s, &format!("/plans/{}/accounts/{c}", f.p)), 409);
    err(&patch(&f.s, &format!("/plans/{}/accounts/{c}", f.p), r#"{"closed": true}"#), 409);

    assert_eq!(delete(&f.s, &f.txn(&sid)).status, 204);
    err(&get(&f.s, &f.txn(&sid)), 404);
}

#[test]
fn validation() {
    let f = setup();
    let (c, a, g) = (&f.checking, &f.asset, &f.groceries);
    let bad = |body: String| err(&post(&f.s, &f.txns(), &body), 400);
    bad(format!(r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": -1}}"#)); // no category
    bad(format!(r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": -1, "category_id": "{}"}}"#, f.visa_payment));
    bad(format!(r#"{{"account_id": "{a}", "date": "2026-10-04", "amount": -1, "category_id": "{g}"}}"#));
    bad(format!(r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1.0, "category_id": "{g}"}}"#));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1, "category_id": "{g}", "flag_color": "pink"}}"#
    ));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1, "category_id": "{g}", "cleared": null}}"#
    ));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1, "category_id": "{g}", "payee_id": "x", "payee_name": "ok"}}"#
    ));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1, "category_id": "{g}", "payee_name": "   "}}"#
    ));
    bad(r#"{"account_id": "nope", "date": "2026-10-04", "amount": 1, "category_id": "ready_to_assign"}"#.into());
    // A failed request creates no payee (SPEC 3.11).
    bad(format!(r#"{{"account_id": "{c}", "date": "2026-10-04", "amount": 1, "payee_name": "Ghost"}}"#));
    let payees = expect(get(&f.s, &format!("/plans/{}/payees", f.p)), 200);
    assert!(payees.as_array().unwrap().iter().all(|p| p["name"] != "Ghost"));

    // Tracking accounts take no category; cash defaults to cleared.
    let t = f.create(&format!(r#"{{"account_id": "{a}", "date": "2026-10-04", "amount": 5}}"#));
    assert_eq!(t["category_id"], Value::Null);
    let wallet = id(&account(&f.s, &f.p, r#"{"name": "Wallet", "type": "cash"}"#));
    let t =
        f.create(&format!(r#"{{"account_id": "{wallet}", "date": "2026-10-04", "amount": 5, "category_id": "{g}"}}"#));
    assert_eq!(t["cleared"], "cleared");
    err(&f.patch(&id(&t), &format!(r#"{{"account_id": "{c}"}}"#)), 400);
    expect(f.patch(&id(&t), &format!(r#"{{"account_id": "{wallet}"}}"#)), 200);
}

#[test]
fn transfers() {
    let f = setup();
    let (c, sv) = (&f.checking, &f.savings);
    let out = f.create(&format!(
        r#"{{"account_id": "{c}", "date": "2026-10-05", "amount": -500000, "payee_id": "{}"}}"#,
        f.savings_payee
    ));
    let out_id = id(&out);
    let in_id = out["transfer_transaction_id"].as_str().unwrap().to_string();
    assert_eq!(out["transfer_account_id"], sv.as_str());
    assert_eq!(out["category_id"], Value::Null);
    assert_eq!(
        f.get(&in_id),
        json!({"id": in_id, "account_id": sv, "date": "2026-10-05", "amount": 500000,
               "payee_id": f.checking_payee, "category_id": null, "memo": null,
               "cleared": "uncleared", "approved": true, "flag_color": null,
               "transfer_account_id": c, "transfer_transaction_id": out_id, "subtransactions": []})
    );
    // Self-transfer, category on an on-budget transfer, split transfer.
    let bad = |body: String| err(&post(&f.s, &f.txns(), &body), 400);
    bad(format!(r#"{{"account_id": "{c}", "date": "2026-10-05", "amount": 1, "payee_id": "{}"}}"#, f.checking_payee));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-05", "amount": 1, "payee_id": "{}", "category_id": "ready_to_assign"}}"#,
        f.savings_payee
    ));

    // Date and amount follow; memo doesn't.
    expect(f.patch(&in_id, r#"{"amount": 7000, "date": "2026-10-09", "memo": "m"}"#), 200);
    let o = f.get(&out_id);
    assert_eq!(
        (o["amount"].clone(), o["date"].clone(), o["memo"].clone()),
        (json!(-7000), json!("2026-10-09"), Value::Null)
    );

    // Changing to an ordinary payee needs a category, and deletes the other half.
    err(&f.patch(&out_id, r#"{"payee_name": "Bank"}"#), 400);
    assert_eq!(f.get(&in_id)["amount"], 7000);
    let o = expect(f.patch(&out_id, r#"{"payee_name": "Bank", "category_id": "ready_to_assign"}"#), 200);
    assert_eq!(o["transfer_transaction_id"], Value::Null);
    err(&get(&f.s, &f.txn(&in_id)), 404);

    // Ordinary to transfer into a tracking account: on-budget half keeps its category.
    let o = expect(f.patch(&out_id, &format!(r#"{{"payee_id": "{}"}}"#, f.asset_payee)), 200);
    assert_eq!(o["category_id"], "ready_to_assign");
    let half = o["transfer_transaction_id"].as_str().unwrap().to_string();
    let h = f.get(&half);
    assert_eq!(h["account_id"], f.asset.as_str());
    assert_eq!(h["category_id"], Value::Null);
    // The tracking half's category_id sets the on-budget half's.
    let g = &f.groceries;
    let h = expect(f.patch(&half, &format!(r#"{{"category_id": "{g}"}}"#)), 200);
    assert_eq!(h["category_id"], Value::Null);
    assert_eq!(f.get(&out_id)["category_id"], g.as_str());

    // Deleting either half deletes both.
    assert_eq!(delete(&f.s, &f.txn(&half)).status, 204);
    err(&get(&f.s, &f.txn(&out_id)), 404);
}

#[test]
fn splits() {
    let f = setup();
    let (c, g, h) = (&f.checking, &f.groceries, &f.household);
    let split = f.create(&format!(
        r#"{{"account_id": "{c}", "date": "2026-10-06", "amount": -90000, "payee_name": "Megastore",
            "subtransactions": [{{"amount": -60000, "category_id": "{g}"}},
                                {{"amount": -30000, "category_id": "{h}", "memo": "light bulbs"}}]}}"#
    ));
    assert_eq!(split["category_id"], Value::Null);
    assert_eq!(
        split["subtransactions"],
        json!([{"amount": -60000, "category_id": g, "memo": null},
               {"amount": -30000, "category_id": h, "memo": "light bulbs"}])
    );
    let sid = id(&split);
    let by_cat = expect(get(&f.s, &format!("{}?category_id={h}", f.txns())), 200);
    assert_eq!(by_cat.as_array().unwrap().len(), 1);

    let bad = |body: String| err(&post(&f.s, &f.txns(), &body), 400);
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-06", "amount": -1, "subtransactions": [{{"amount": -2, "category_id": "{g}"}}]}}"#
    ));
    bad(format!(
        r#"{{"account_id": "{c}", "date": "2026-10-06", "amount": -1, "category_id": "{g}", "subtransactions": [{{"amount": -1, "category_id": "{g}"}}]}}"#
    ));
    bad(format!(
        r#"{{"account_id": "{}", "date": "2026-10-06", "amount": -1, "subtransactions": [{{"amount": -1, "category_id": "{g}"}}]}}"#,
        f.asset
    ));

    err(&f.patch(&sid, r#"{"amount": -1}"#), 400);
    err(&f.patch(&sid, r#"{"subtransactions": []}"#), 400);
    let t = expect(f.patch(&sid, &format!(r#"{{"subtransactions": [], "category_id": "{g}"}}"#)), 200);
    assert_eq!(t["subtransactions"], json!([]));
    assert_eq!(t["category_id"], g.as_str());
    let t =
        expect(f.patch(&sid, &format!(r#"{{"subtransactions": [{{"amount": -90000, "category_id": "{h}"}}]}}"#)), 200);
    assert_eq!(t["category_id"], Value::Null);
}
