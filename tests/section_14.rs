//! Reconciliation (SPEC 14).

mod common;

use common::*;
use serde_json::{Value, json};

/// SPEC 14.3's Checking: a 2500.00 starting balance, a cleared -120.00 and
/// an uncleared -35.00. Returns the plan and the account.
fn checking(s: &Server) -> (String, String) {
    let p = new_plan(s);
    let body = r#"{"name": "Checking", "type": "checking", "starting_balance": 2500000, "starting_date": "2026-10-01"}"#;
    let a = id(&expect(post(s, &format!("/plans/{p}/accounts"), body), 201));
    for (amount, cleared) in [(-120000, "cleared"), (-35000, "uncleared")] {
        let body = json!({"account_id": a, "date": "2026-10-04", "amount": amount,
                          "category_id": "ready_to_assign", "cleared": cleared});
        expect(
            post(s, &format!("/plans/{p}/transactions"), &body.to_string()),
            201,
        );
    }
    (p, a)
}

fn reconcile(s: &Server, p: &str, a: &str, body: &str) -> Resp {
    post(s, &format!("/plans/{p}/accounts/{a}/reconcile"), body)
}

fn cleared_states(s: &Server, p: &str) -> Vec<Value> {
    expect(get(s, &format!("/plans/{p}/transactions")), 200)
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["cleared"].clone())
        .collect()
}

#[test]
fn spec_14_example() {
    let s = start();
    let (p, a) = checking(&s);
    let r = expect(
        reconcile(&s, &p, &a, r#"{"balance": 2375500, "date": "2026-10-31"}"#),
        200,
    );
    assert_eq!(r["account"]["balance"], 2340500);
    assert_eq!(r["account"]["cleared_balance"], 2375500);
    assert_eq!(r["account"]["uncleared_balance"], -35000);
    let adj = &r["adjustment"];
    assert_eq!(adj["amount"], -4500);
    assert_eq!(adj["date"], "2026-10-31");
    assert_eq!(adj["category_id"], "ready_to_assign");
    assert_eq!(adj["cleared"], "reconciled");
    assert_eq!(adj["memo"], Value::Null);
    assert_eq!(adj["subtransactions"], json!([]));
    let payee = expect(
        get(
            &s,
            &format!("/plans/{p}/payees/{}", adj["payee_id"].as_str().unwrap()),
        ),
        200,
    );
    assert_eq!(payee["name"], "Reconciliation Balance Adjustment");
    assert_eq!(
        cleared_states(&s, &p),
        ["reconciled", "reconciled", "uncleared", "reconciled"]
    );
}

#[test]
fn matching_balance_makes_no_adjustment() {
    let s = start();
    let (p, a) = checking(&s);
    let r = expect(
        reconcile(&s, &p, &a, r#"{"balance": 2380000, "date": "2026-10-31"}"#),
        200,
    );
    assert_eq!(r["adjustment"], Value::Null);
    assert_eq!(
        cleared_states(&s, &p),
        ["reconciled", "reconciled", "uncleared"]
    );
}

#[test]
fn tracking_adjustment_has_no_category() {
    let s = start();
    let p = new_plan(&s);
    let a = id(&expect(
        post(
            &s,
            &format!("/plans/{p}/accounts"),
            r#"{"name": "House", "type": "otherAsset"}"#,
        ),
        201,
    ));
    let r = expect(
        reconcile(&s, &p, &a, r#"{"balance": 5000, "date": "2026-10-31"}"#),
        200,
    );
    assert_eq!(r["adjustment"]["category_id"], Value::Null);
    assert_eq!(r["account"]["cleared_balance"], 5000);
}

#[test]
fn errors_change_nothing() {
    let s = start();
    let (p, a) = checking(&s);
    err(
        &reconcile(&s, &p, "nope", r#"{"balance": 0, "date": "2026-10-31"}"#),
        404,
    );
    err(&reconcile(&s, &p, &a, r#"{"date": "2026-10-31"}"#), 400);
    err(
        &reconcile(&s, &p, &a, r#"{"balance": 0, "date": null}"#),
        400,
    );
    err(
        &reconcile(&s, &p, &a, r#"{"balance": 1.5, "date": "2026-10-31"}"#),
        400,
    );
    // 2380000 cleared now; -1e12 is in range but the adjustment isn't.
    err(
        &reconcile(
            &s,
            &p,
            &a,
            r#"{"balance": -1000000000000, "date": "2026-10-31"}"#,
        ),
        409,
    );
    err(
        &req(
            &s,
            "GET",
            &format!("/plans/{p}/accounts/{a}/reconcile"),
            None,
        ),
        405,
    );
    assert_eq!(cleared_states(&s, &p), ["cleared", "cleared", "uncleared"]);
}

#[test]
fn reconciliation_persists() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_on(dir.path());
    let (p, a) = checking(&s);
    expect(
        reconcile(&s, &p, &a, r#"{"balance": 2375500, "date": "2026-10-31"}"#),
        200,
    );
    let before = cleared_states(&s, &p);
    drop(s);
    let s = start_on(dir.path());
    assert_eq!(cleared_states(&s, &p), before);
}
