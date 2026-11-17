//! Accounts, payees and categories (SPEC 5 to 7).

mod common;

use common::*;
use serde_json::{Value, json};

fn names(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn accounts_create_list_and_balances() {
    let s = start();
    let p = new_plan(&s);
    let accounts = format!("/plans/{p}/accounts");

    let checking = expect(
        post(
            &s,
            &accounts,
            r#"{"name": "Checking", "type": "checking", "starting_balance": 2500000, "starting_date": "2026-10-01"}"#,
        ),
        201,
    );
    let cid = id(&checking);
    let cpayee = checking["transfer_payee_id"].as_str().unwrap().to_string();
    assert_eq!(
        checking,
        json!({"id": cid, "name": "Checking", "type": "checking", "on_budget": true, "closed": false,
               "note": null, "balance": 2500000, "cleared_balance": 2500000, "uncleared_balance": 0,
               "transfer_payee_id": cpayee, "payment_category_id": null})
    );
    assert_eq!(expect(get(&s, &accounts), 200), json!([checking]));
    assert_eq!(expect(get(&s, &format!("{accounts}/{cid}")), 200), checking);

    let payees = expect(get(&s, &format!("/plans/{p}/payees")), 200);
    let sb = payees[1]["id"].as_str().unwrap();
    assert_eq!(
        payees,
        json!([{"id": cpayee, "name": "Transfer : Checking", "transfer_account_id": cid},
               {"id": sb, "name": "Starting Balance", "transfer_account_id": null}])
    );

    let visa = expect(
        post(
            &s,
            &accounts,
            r#"{"name": "Visa", "type": "creditCard", "starting_balance": -350000, "starting_date": "2026-10-01", "note": "n"}"#,
        ),
        201,
    );
    assert_eq!(visa["balance"], -350000);
    assert_eq!(visa["note"], "n");
    let vpay = visa["payment_category_id"].as_str().unwrap().to_string();
    // The existing Starting Balance payee is reused.
    assert_eq!(expect(get(&s, &format!("/plans/{p}/payees")), 200).as_array().unwrap().len(), 3);
    let cats = expect(get(&s, &format!("/plans/{p}/categories")), 200);
    assert_eq!(cats.as_array().unwrap().len(), 1);
    assert_eq!(cats[0]["name"], "Credit Card Payments");
    assert_eq!(cats[0]["internal"], true);
    assert_eq!(
        cats[0]["categories"],
        json!([{"id": vpay, "group_id": cats[0]["id"], "name": "Visa", "hidden": false, "note": null,
                "payment_account_id": id(&visa)}])
    );

    // Tracking accounts are off budget; a zero starting balance makes no transaction.
    let house = expect(post(&s, &accounts, r#"{"name": "House", "type": "otherAsset", "starting_balance": -0}"#), 201);
    assert_eq!((house["on_budget"].clone(), house["balance"].clone()), (json!(false), json!(0)));
    assert_eq!(delete(&s, &format!("{accounts}/{}", id(&house))).status, 204);
    let loan = expect(
        post(
            &s,
            &accounts,
            r#"{"name": "Loan", "type": "otherLiability", "starting_balance": 0, "starting_date": "2026-01-01"}"#,
        ),
        201,
    );
    assert_eq!(loan["payment_category_id"], Value::Null);
    let loc = expect(
        post(
            &s,
            &accounts,
            r#"{"name": "LOC", "type": "lineOfCredit", "starting_balance": 1000000000000, "starting_date": "2999-12-31"}"#,
        ),
        201,
    );
    assert!(loc["payment_category_id"].is_string());
    assert_eq!(loc["balance"], 1_000_000_000_000i64);

    let before = (
        get(&s, &accounts).body,
        get(&s, &format!("/plans/{p}/payees")).body,
        get(&s, &format!("/plans/{p}/categories")).body,
    );
    for bad in [
        r#"{"type": "checking"}"#,
        r#"{"name": "A"}"#,
        r#"{"name": "A", "type": "Checking"}"#,
        r#"{"name": "A", "type": null}"#,
        r#"{"name": "", "type": "checking"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 5}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 5, "starting_date": null}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 0, "starting_date": null}"#,
        r#"{"name": "A", "type": "creditCard", "starting_date": "2026-02-30"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_date": 20260101}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 1.0, "starting_date": "2026-01-01"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 1e3, "starting_date": "2026-01-01"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": "5", "starting_date": "2026-01-01"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": null}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": -1000000000001, "starting_date": "2026-01-01"}"#,
        r#"{"name": "A", "type": "creditCard", "starting_balance": 5, "starting_date": "1899-12-31"}"#,
        r#"{"name": "A", "type": "creditCard", "note": 5}"#,
    ] {
        err(&post(&s, &accounts, bad), 400);
    }
    let long_note = "x".repeat(501);
    err(&post(&s, &accounts, &format!(r#"{{"name": "A", "type": "cash", "note": "{long_note}"}}"#)), 400);
    let after = (
        get(&s, &accounts).body,
        get(&s, &format!("/plans/{p}/payees")).body,
        get(&s, &format!("/plans/{p}/categories")).body,
    );
    assert_eq!(before, after);
}

#[test]
fn accounts_patch_close_and_delete() {
    let s = start();
    let p = new_plan(&s);
    let accounts = format!("/plans/{p}/accounts");
    let visa = expect(
        post(
            &s,
            &accounts,
            r#"{"name": "Visa", "type": "creditCard", "starting_balance": -350000, "starting_date": "2026-10-01"}"#,
        ),
        201,
    );
    let vid = id(&visa);
    let vpath = format!("{accounts}/{vid}");
    let vpay = visa["payment_category_id"].as_str().unwrap().to_string();
    let vpayee = visa["transfer_payee_id"].as_str().unwrap().to_string();

    let renamed = expect(patch(&s, &vpath, r#"{"name": "Visa Gold", "type": "creditCard", "balance": 5}"#), 200);
    let mut expected = visa.clone();
    expected["name"] = json!("Visa Gold");
    assert_eq!(renamed, expected);
    assert_eq!(expect(get(&s, &format!("/plans/{p}/payees/{vpayee}")), 200)["name"], "Transfer : Visa Gold");
    assert_eq!(expect(get(&s, &format!("/plans/{p}/categories/{vpay}")), 200)["name"], "Visa Gold");

    err(&patch(&s, &vpath, r#"{"type": "checking"}"#), 400);
    err(&patch(&s, &vpath, r#"{"type": null}"#), 400);
    err(&patch(&s, &vpath, r#"{"closed": null}"#), 400);
    err(&patch(&s, &vpath, r#"{"name": null}"#), 400);
    // 400 beats 409, and a 409 changes nothing.
    err(&patch(&s, &vpath, r#"{"closed": true, "type": "cash"}"#), 400);
    err(&patch(&s, &vpath, r#"{"closed": true, "name": "Other"}"#), 409);
    assert_eq!(expect(get(&s, &vpath), 200)["name"], "Visa Gold");
    assert_eq!(expect(patch(&s, &vpath, r#"{"note": "card"}"#), 200)["note"], "card");
    assert_eq!(expect(patch(&s, &vpath, r#"{"note": null}"#), 200)["note"], Value::Null);
    assert_eq!(expect(patch(&s, &vpath, "{}"), 200)["name"], "Visa Gold");

    err(&delete(&s, &vpath), 409);

    let cash = expect(post(&s, &accounts, r#"{"name": "Wallet", "type": "cash"}"#), 201);
    let wpath = format!("{accounts}/{}", id(&cash));
    assert_eq!(expect(patch(&s, &wpath, r#"{"closed": true}"#), 200)["closed"], true);
    assert_eq!(expect(patch(&s, &wpath, r#"{"closed": true}"#), 200)["closed"], true);
    assert_eq!(expect(patch(&s, &wpath, r#"{"closed": false}"#), 200)["closed"], false);
    assert_eq!(delete(&s, &wpath).status, 204);
    err(&get(&s, &wpath), 404);

    let mc = expect(post(&s, &accounts, r#"{"name": "MC", "type": "creditCard"}"#), 201);
    let mcpay = mc["payment_category_id"].as_str().unwrap().to_string();
    let cats = expect(get(&s, &format!("/plans/{p}/categories")), 200);
    assert_eq!(names(&cats[0]["categories"]), ["Visa Gold", "MC"]);
    assert_eq!(delete(&s, &format!("{accounts}/{}", id(&mc))).status, 204);
    err(&get(&s, &format!("/plans/{p}/categories/{mcpay}")), 404);
    err(&get(&s, &format!("/plans/{p}/payees/{}", mc["transfer_payee_id"].as_str().unwrap())), 404);
    assert_eq!(
        names(&expect(get(&s, &format!("/plans/{p}/payees")), 200)),
        ["Transfer : Visa Gold", "Starting Balance"]
    );
}

#[test]
fn payees() {
    let s = start();
    let p = new_plan(&s);
    let payees = format!("/plans/{p}/payees");
    let checking = expect(
        post(
            &s,
            &format!("/plans/{p}/accounts"),
            r#"{"name": "Checking", "type": "checking", "starting_balance": 1, "starting_date": "2026-10-01"}"#,
        ),
        201,
    );
    let tp = checking["transfer_payee_id"].as_str().unwrap().to_string();

    let shop = expect(post(&s, &payees, r#"{"name": "Corner Shop"}"#), 201);
    assert_eq!(shop, json!({"id": id(&shop), "name": "Corner Shop", "transfer_account_id": null}));
    err(&post(&s, &payees, r#"{"name": "Corner Shop"}"#), 409);
    err(&post(&s, &payees, r#"{"name": "Starting Balance"}"#), 409);
    assert_eq!(post(&s, &payees, r#"{"name": "corner shop"}"#).status, 201);
    assert_eq!(post(&s, &payees, r#"{"name": "Transfer : Checking"}"#).status, 201);
    assert_eq!(post(&s, &payees, &format!(r#"{{"name": "{}"}}"#, "p".repeat(200))).status, 201);
    err(&post(&s, &payees, &format!(r#"{{"name": "{}"}}"#, "q".repeat(201))), 400);
    err(&post(&s, &payees, r#"{"name": " \t"}"#), 400);

    let spath = format!("{payees}/{}", id(&shop));
    assert_eq!(expect(patch(&s, &spath, r#"{"name": "Corner Shop"}"#), 200)["name"], "Corner Shop");
    err(&patch(&s, &spath, r#"{"name": "corner shop"}"#), 409);
    assert_eq!(expect(patch(&s, &spath, r#"{"name": "Corner Shop Ltd"}"#), 200)["name"], "Corner Shop Ltd");
    // A user payee named "Transfer : Checking" exists, so this conflicts.
    err(&patch(&s, &spath, r#"{"name": "Transfer : Checking"}"#), 409);
    let tpath = format!("{payees}/{tp}");
    err(&patch(&s, &tpath, r#"{"name": "Transfer : Checking"}"#), 400);
    assert_eq!(expect(patch(&s, &tpath, r#"{"x": 1}"#), 200)["name"], "Transfer : Checking");

    err(&delete(&s, &tpath), 400);
    let sb = expect(get(&s, &payees), 200)[1]["id"].as_str().unwrap().to_string();
    err(&delete(&s, &format!("{payees}/{sb}")), 409);
    assert_eq!(delete(&s, &spath).status, 204);
    err(&delete(&s, &spath), 404);
}

#[test]
fn categories_and_groups() {
    let s = start();
    let p = new_plan(&s);
    let other = new_plan(&s);
    let cats = expect(get(&s, &format!("/plans/{p}/categories")), 200);
    let ccp = cats[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        cats,
        json!([{"id": ccp, "name": "Credit Card Payments", "hidden": false, "internal": true, "categories": []}])
    );

    let groups = format!("/plans/{p}/category_groups");
    let bills = expect(post(&s, &groups, r#"{"name": "Bills", "hidden": true}"#), 201);
    let bid = id(&bills);
    assert_eq!(bills, json!({"id": bid, "name": "Bills", "hidden": false, "internal": false, "categories": []}));
    let housing = id(&expect(post(&s, &groups, r#"{"name": "Housing"}"#), 201));
    let foreign = id(&expect(post(&s, &format!("/plans/{other}/category_groups"), r#"{"name": "X"}"#), 201));

    let categories = format!("/plans/{p}/categories");
    let rent = expect(post(&s, &categories, &format!(r#"{{"group_id": "{bid}", "name": "Rent"}}"#)), 201);
    let rid = id(&rent);
    assert_eq!(
        rent,
        json!({"id": rid, "group_id": bid, "name": "Rent", "hidden": false, "note": null, "payment_account_id": null})
    );
    let water =
        id(&expect(post(&s, &categories, &format!(r#"{{"group_id": "{bid}", "name": "Water", "note": "n"}}"#)), 201));
    for bad in [
        format!(r#"{{"group_id": "{ccp}", "name": "X"}}"#),
        format!(r#"{{"group_id": "{foreign}", "name": "X"}}"#),
        r#"{"group_id": "nope", "name": "X"}"#.to_string(),
        r#"{"group_id": null, "name": "X"}"#.to_string(),
        format!(r#"{{"group_id": "{bid}"}}"#),
        format!(r#"{{"group_id": "{bid}", "name": "X", "note": 1}}"#),
    ] {
        err(&post(&s, &categories, &bad), 400);
    }

    let gpath = format!("{groups}/{bid}");
    let hidden = expect(patch(&s, &gpath, r#"{"hidden": true}"#), 200);
    assert_eq!(hidden["hidden"], true);
    assert_eq!(names(&hidden["categories"]), ["Rent", "Water"]);
    err(&patch(&s, &gpath, r#"{"hidden": null}"#), 400);
    let ccp_path = format!("{groups}/{ccp}");
    err(&patch(&s, &ccp_path, r#"{"hidden": false}"#), 400);
    err(&patch(&s, &ccp_path, r#"{"name": "Credit Card Payments"}"#), 400);
    assert_eq!(expect(patch(&s, &ccp_path, "{}"), 200)["internal"], true);
    err(&delete(&s, &ccp_path), 400);
    err(&delete(&s, &gpath), 409);
    err(&get(&s, &format!("/plans/{other}/category_groups/{bid}")), 404);

    let rpath = format!("{categories}/{rid}");
    let moved = expect(patch(&s, &rpath, &format!(r#"{{"group_id": "{housing}", "note": "due on the 1st"}}"#)), 200);
    assert_eq!(
        moved,
        json!({"id": rid, "group_id": housing, "name": "Rent", "hidden": false, "note": "due on the 1st", "payment_account_id": null})
    );
    err(&patch(&s, &rpath, &format!(r#"{{"group_id": "{ccp}"}}"#)), 400);
    err(&patch(&s, &rpath, &format!(r#"{{"group_id": "{foreign}"}}"#)), 400);
    err(&patch(&s, &rpath, r#"{"hidden": null}"#), 400);
    let wpath = format!("{categories}/{water}");
    expect(patch(&s, &wpath, &format!(r#"{{"group_id": "{housing}"}}"#)), 200);
    expect(patch(&s, &rpath, &format!(r#"{{"group_id": "{housing}", "name": "Rent!"}}"#)), 200);
    let list = expect(get(&s, &categories), 200);
    assert_eq!(names(&list), ["Credit Card Payments", "Bills", "Housing"]);
    assert_eq!(names(&list[2]["categories"]), ["Rent!", "Water"]);
    expect(patch(&s, &rpath, &format!(r#"{{"group_id": "{bid}"}}"#)), 200);
    expect(patch(&s, &rpath, &format!(r#"{{"group_id": "{housing}"}}"#)), 200);
    let list = expect(get(&s, &categories), 200);
    assert_eq!(names(&list[2]["categories"]), ["Water", "Rent!"]);
    assert_eq!(delete(&s, &gpath).status, 204);

    let visa = expect(post(&s, &format!("/plans/{p}/accounts"), r#"{"name": "Visa", "type": "creditCard"}"#), 201);
    let vpath = format!("{categories}/{}", visa["payment_category_id"].as_str().unwrap());
    err(&patch(&s, &vpath, r#"{"name": "Visa"}"#), 400);
    err(&patch(&s, &vpath, &format!(r#"{{"group_id": "{ccp}"}}"#)), 400);
    let pay = expect(patch(&s, &vpath, r#"{"hidden": true, "note": "pay it"}"#), 200);
    assert_eq!((pay["hidden"].clone(), pay["note"].clone()), (json!(true), json!("pay it")));

    err(&get(&s, &format!("{categories}/ready_to_assign")), 404);
    err(&patch(&s, &format!("{categories}/ready_to_assign"), "{}"), 404);
    err(&req(&s, "PUT", &rpath, Some(b"{}")), 405);
    err(&get(&s, &groups), 405);
    let head = req(&s, "HEAD", &groups, None);
    assert_eq!((head.status, head.body.as_str()), (405, ""));
    let head = req(&s, "HEAD", &categories, None);
    assert_eq!((head.status, head.body.as_str()), (200, ""));
}

#[test]
fn plan_scoping_and_precedence() {
    let s = start();
    let a = new_plan(&s);
    let b = new_plan(&s);
    let acc = id(&expect(post(&s, &format!("/plans/{a}/accounts"), r#"{"name": "C", "type": "cash"}"#), 201));
    let payee = id(&expect(post(&s, &format!("/plans/{a}/payees"), r#"{"name": "Shop"}"#), 201));
    err(&get(&s, &format!("/plans/{b}/accounts/{acc}")), 404);
    err(&patch(&s, &format!("/plans/{b}/payees/{payee}"), "nope"), 404);
    err(&delete(&s, &format!("/plans/{b}/payees/{payee}")), 404);
    err(&req(&s, "PUT", "/plans/missing/accounts/missing", None), 405);
    err(&post(&s, "/plans/missing/accounts", "nope"), 404);
    err(&get(&s, &format!("/plans/{a}/months")), 404);
    err(&get(&s, &format!("/plans/{a}/accounts/{acc}/extra")), 404);
    err(&get(&s, &format!("/plans/{a}/accounts/")), 404);
    assert_eq!(delete(&s, &format!("/plans/{a}")).status, 204);
    err(&get(&s, &format!("/plans/{a}/accounts")), 404);
    err(&get(&s, &format!("/plans/{a}/accounts/{acc}")), 404);
}
