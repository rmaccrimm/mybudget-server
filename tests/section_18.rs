//! SPEC 18: targets and assignment history.
mod common;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use serde_json::{Value, json};

/// Starts the server with `MYBUDGET_TODAY` set to `today`, keeping its data
/// in `dir`.
fn start_today(dir: &Path, today: &str) -> Server {
    start_with(|c| {
        c.env("MYBUDGET_DATA", dir).env("MYBUDGET_TODAY", today);
    })
}

/// 11.8's plan: Checking with 3000.00, and Groceries, Rent and Dining in
/// Everyday, with October's and November's assignments and spending.
struct Plan {
    p: String,
    groceries: String,
    rent: String,
    dining: String,
}

impl Plan {
    fn new(s: &Server) -> Self {
        let p = new_plan(s);
        let path = |rest: &str| format!("/plans/{p}/{rest}");
        let checking = id(&expect(
            post(
                s,
                &path("accounts"),
                r#"{"name": "Checking", "type": "checking", "starting_balance": 3000000, "starting_date": "2026-10-01"}"#,
            ),
            201,
        ));
        let everyday = id(&expect(
            post(s, &path("category_groups"), r#"{"name": "Everyday"}"#),
            201,
        ));
        let [groceries, rent, dining] = ["Groceries", "Rent", "Dining"].map(|name| {
            let body = json!({"group_id": everyday, "name": name}).to_string();
            id(&expect(post(s, &path("categories"), &body), 201))
        });
        let plan = Self {
            p,
            groceries,
            rent,
            dining,
        };
        plan.assign(s, "2026-10", &plan.groceries, 400000);
        plan.assign(s, "2026-10", &plan.rent, 1200000);
        plan.assign(s, "2026-10", &plan.dining, 50000);
        plan.assign(s, "2026-11", &plan.rent, 1200000);
        for (date, category, amount) in [
            ("2026-10-04", &plan.groceries, -120000),
            ("2026-10-11", &plan.dining, -80000),
        ] {
            let body = json!({"account_id": checking, "date": date, "amount": amount,
                              "category_id": category});
            expect(post(s, &plan.path("transactions"), &body.to_string()), 201);
        }
        plan
    }

    fn path(&self, rest: &str) -> String {
        format!("/plans/{}/{rest}", self.p)
    }

    fn assign(&self, s: &Server, month: &str, category: &str, amount: i64) -> Value {
        let body = json!({"assigned": amount}).to_string();
        expect(
            patch(
                s,
                &self.path(&format!("months/{month}/categories/{category}")),
                &body,
            ),
            200,
        )
    }

    fn assignments(&self, s: &Server, month: &str, category: &str) -> Value {
        expect(
            get(
                s,
                &self.path(&format!("months/{month}/categories/{category}/assignments")),
            ),
            200,
        )
    }

    fn set_target(&self, s: &Server, category: &str, target: &Value) -> Resp {
        let body = json!({ "target": target }).to_string();
        patch(s, &self.path(&format!("categories/{category}")), &body)
    }

    fn entry(&self, s: &Server, month: &str, category: &str) -> Value {
        expect(
            get(
                s,
                &self.path(&format!("months/{month}/categories/{category}")),
            ),
            200,
        )
    }
}

#[test]
fn today_must_be_a_date() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    for today in [
        OsStr::new("2026-02-30"),
        OsStr::new("26-10-01"),
        OsStr::new(" 2026-10-01"),
        OsStr::from_bytes(b"2026-10-0\xff"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
            .env("MYBUDGET_ADDR", "127.0.0.1:0")
            .env("MYBUDGET_DATA", &data)
            .env("MYBUDGET_TODAY", today)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{today:?}");
        assert!(out.stdout.is_empty(), "{today:?}");
        assert!(!out.stderr.is_empty(), "{today:?}");
    }
    assert!(!data.exists());

    // Empty means the local date, as does leaving it unset.
    let s = start_with(|c| {
        c.env("MYBUDGET_TODAY", "");
    });
    let p = new_plan(&s);
    let group = id(&expect(
        post(
            &s,
            &format!("/plans/{p}/category_groups"),
            r#"{"name": "G"}"#,
        ),
        201,
    ));
    let body = json!({"group_id": group, "name": "C"}).to_string();
    let c = id(&expect(
        post(&s, &format!("/plans/{p}/categories"), &body),
        201,
    ));
    expect(
        patch(
            &s,
            &format!("/plans/{p}/months/2026-10/categories/{c}"),
            r#"{"assigned": 5}"#,
        ),
        200,
    );
    let made = expect(
        get(
            &s,
            &format!("/plans/{p}/months/2026-10/categories/{c}/assignments"),
        ),
        200,
    );
    let date = made[0]["date"].as_str().unwrap();
    assert_eq!(date.len(), 10, "{date}");
}

#[test]
fn example_18_2_assignments_keep_their_dates() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_today(dir.path(), "2026-10-01");
    let plan = Plan::new(&s);
    drop(s);
    let s = start_today(dir.path(), "2026-11-20");
    assert_eq!(
        plan.assign(&s, "2026-10", &plan.groceries, 350000),
        json!({"category_id": plan.groceries, "assigned": 350000, "activity": -120000,
               "available": 230000, "cash_overspending": 0, "credit_overspending": 0,
               "target": null, "due": null, "underfunded": null})
    );
    // Leaving it as it is makes none.
    plan.assign(&s, "2026-10", &plan.groceries, 350000);
    assert_eq!(
        plan.assignments(&s, "2026-10", &plan.groceries),
        json!([{"date": "2026-10-01", "amount": 400000},
               {"date": "2026-11-20", "amount": -50000}])
    );
    assert_eq!(plan.assignments(&s, "2026-12", &plan.groceries), json!([]));
    err(
        &get(
            &s,
            &plan.path("months/2026-10/categories/ready_to_assign/assignments"),
        ),
        404,
    );
    err(
        &get(
            &s,
            &plan.path(&format!(
                "months/2026-13/categories/{}/assignments",
                plan.groceries
            )),
        ),
        400,
    );
    err(
        &req(
            &s,
            "PATCH",
            &plan.path(&format!(
                "months/2026-10/categories/{}/assignments",
                plan.groceries
            )),
            Some(b"{}"),
        ),
        405,
    );
    // Amounts may add up past a request's limit.
    plan.assign(&s, "2026-12", &plan.rent, 1_000_000_000_000);
    plan.assign(&s, "2026-12", &plan.rent, -1_000_000_000_000);
    assert_eq!(
        plan.assignments(&s, "2026-12", &plan.rent),
        json!([{"date": "2026-11-20", "amount": 1_000_000_000_000_i64},
               {"date": "2026-11-20", "amount": -2_000_000_000_000_i64}])
    );
}

#[test]
fn examples_18_3_to_18_6_targets() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_today(dir.path(), "2026-11-05");
    let plan = Plan::new(&s);
    let refill = json!({"type": "refill", "amount": 400000, "by_date": 15});
    let view = expect(plan.set_target(&s, &plan.groceries, &refill), 200);
    let refill_target =
        json!({"type": "refill", "amount": 400000, "by_date": 15, "start_month": "2026-11"});
    assert_eq!(view["target"], refill_target);
    assert_eq!(view["name"], "Groceries");

    assert_eq!(
        plan.entry(&s, "2026-11", &plan.groceries),
        json!({"category_id": plan.groceries, "assigned": 0, "activity": 0, "available": 280000,
               "cash_overspending": 0, "credit_overspending": 0,
               "target": refill_target, "due": "2026-11-15", "underfunded": 120000})
    );
    let oct = plan.entry(&s, "2026-10", &plan.groceries);
    assert_eq!(
        (&oct["target"], &oct["due"], &oct["underfunded"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );

    // Resending it keeps its start month; a later target keeps it for
    // November.
    expect(plan.set_target(&s, &plan.groceries, &refill), 200);
    let set_aside = json!({"type": "set_aside", "amount": 300000, "start_month": "2026-12"});
    let view = expect(plan.set_target(&s, &plan.groceries, &set_aside), 200);
    assert_eq!(
        view["target"],
        json!({"type": "set_aside", "amount": 300000, "by_date": null, "start_month": "2026-12"})
    );
    assert_eq!(
        plan.entry(&s, "2026-11", &plan.groceries)["target"],
        refill_target
    );
    let dec = plan.entry(&s, "2026-12", &plan.groceries);
    assert_eq!(
        (&dec["due"], &dec["underfunded"]),
        (&json!("2026-12-31"), &json!(300000))
    );
    let by_31 = json!({"type": "set_aside", "amount": 1, "by_date": 31, "start_month": "2027-02"});
    expect(plan.set_target(&s, &plan.dining, &by_31), 200);
    assert_eq!(plan.entry(&s, "2027-02", &plan.dining)["due"], "2027-02-28");

    // 18.6: Rent's target is met.
    let rent = json!({"type": "set_aside", "amount": 1200000, "by_date": 1});
    expect(plan.set_target(&s, &plan.rent, &rent), 200);
    let nov = expect(get(&s, &plan.path("months/2026-11")), 200);
    assert_eq!(nov["underfunded"], 120000);
    assert_eq!(nov["categories"][1]["due"], "2026-11-01");
    assert_eq!(nov["categories"][1]["underfunded"], 0);
    assert_eq!(nov["uncategorized"]["target"], Value::Null);

    // Removing it removes both of Groceries' targets.
    let view = expect(plan.set_target(&s, &plan.groceries, &Value::Null), 200);
    assert_eq!(view["target"], Value::Null);
    assert_eq!(
        plan.entry(&s, "2026-11", &plan.groceries)["target"],
        Value::Null
    );

    for bad in [
        json!({"type": "set_aside", "amount": 300000, "start_month": "2026-10"}),
        json!({"type": "monthly", "amount": 1}),
        json!({"type": "refill", "amount": 0}),
        json!({"type": "refill"}),
        json!({"amount": 1}),
        json!({"type": "refill", "amount": 1, "by_date": 0}),
        json!({"type": "refill", "amount": 1, "by_date": 32}),
        json!({"type": "refill", "amount": 1, "by_date": 1.0}),
        json!({"type": "refill", "amount": 1, "start_month": null}),
        json!({"type": "refill", "amount": 1, "start_month": "2026-13"}),
        json!([]),
        json!("refill"),
    ] {
        err(&plan.set_target(&s, &plan.groceries, &bad), 400);
    }
}

#[test]
fn targets_change_over_time() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_today(dir.path(), "2026-11-05");
    let plan = Plan::new(&s);
    let target = |amount: i64, start: &str| json!({"type": "set_aside", "amount": amount, "start_month": start});
    expect(plan.set_target(&s, &plan.rent, &target(1, "2026-11")), 200);
    expect(plan.set_target(&s, &plan.rent, &target(2, "2027-01")), 200);
    expect(plan.set_target(&s, &plan.rent, &target(3, "2027-03")), 200);
    // A new target from December removes January's and March's.
    expect(plan.set_target(&s, &plan.rent, &target(4, "2026-12")), 200);
    let amount = |month: &str| plan.entry(&s, month, &plan.rent)["target"]["amount"].clone();
    assert_eq!(amount("2026-11"), 1);
    assert_eq!(amount("2027-04"), 4);
    drop(s);

    // In December, removing it keeps November's.
    let s = start_today(dir.path(), "2026-12-01");
    let amount = |month: &str| plan.entry(&s, month, &plan.rent)["target"]["amount"].clone();
    // November is now before today's month, even for the target it has.
    err(&plan.set_target(&s, &plan.rent, &target(1, "2026-11")), 400);
    err(&plan.set_target(&s, &plan.rent, &target(4, "2026-11")), 400);
    assert_eq!(amount("2026-12"), 4);
    expect(plan.set_target(&s, &plan.rent, &Value::Null), 200);
    assert_eq!(amount("2026-11"), 1);
    assert_eq!(amount("2026-12"), Value::Null);
    // Removing it again changes nothing, and a new one leaves a gap.
    expect(plan.set_target(&s, &plan.rent, &Value::Null), 200);
    expect(plan.set_target(&s, &plan.rent, &target(5, "2027-02")), 200);
    assert_eq!(amount("2026-11"), 1);
    assert_eq!(amount("2027-01"), Value::Null);
    assert_eq!(amount("2027-02"), 5);

    // Payment categories never take one.
    let visa = expect(
        post(
            &s,
            &plan.path("accounts"),
            r#"{"name": "Visa", "type": "creditCard"}"#,
        ),
        201,
    );
    let pay = visa["payment_category_id"].as_str().unwrap();
    err(&plan.set_target(&s, pay, &Value::Null), 400);
    err(&plan.set_target(&s, pay, &target(1, "2027-01")), 400);
}

#[test]
fn without_a_set_today_start_months_are_checked_against_the_clock() {
    let s = start_with(|c| {
        c.env_remove("MYBUDGET_TODAY");
    });
    let plan = Plan::new(&s);
    let target = json!({"type": "refill", "amount": 1, "start_month": "1900-01"});
    err(&plan.set_target(&s, &plan.groceries, &target), 400);
    let view = expect(
        plan.set_target(&s, &plan.groceries, &json!({"type": "refill", "amount": 1})),
        200,
    );
    let this_month = chrono::Local::now().format("%Y-%m").to_string();
    assert_eq!(view["target"]["start_month"], this_month);
}

#[test]
fn deleting_a_category_moves_or_drops_its_assignments() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_today(dir.path(), "2026-11-05");
    let plan = Plan::new(&s);
    plan.assign(&s, "2026-10", &plan.groceries, 450000);
    plan.assign(&s, "2026-10", &plan.dining, 0);
    let target = json!({"type": "refill", "amount": 1});
    expect(plan.set_target(&s, &plan.dining, &target), 200);
    expect(plan.set_target(&s, &plan.groceries, &target), 200);
    let status = delete(
        &s,
        &plan.path(&format!(
            "categories/{}?reassign_to={}",
            plan.dining, plan.groceries
        )),
    )
    .status;
    assert_eq!(status, 204);
    assert_eq!(
        plan.assignments(&s, "2026-10", &plan.groceries),
        json!([{"date": "2026-11-05", "amount": 400000},
               {"date": "2026-11-05", "amount": 50000},
               {"date": "2026-11-05", "amount": 50000},
               {"date": "2026-11-05", "amount": -50000}])
    );
    assert_eq!(
        plan.entry(&s, "2026-11", &plan.groceries)["target"]["amount"],
        1
    );
    assert_eq!(
        delete(&s, &plan.path(&format!("categories/{}", plan.rent))).status,
        204
    );
    let oct = expect(get(&s, &plan.path("months/2026-10")), 200);
    assert_eq!(oct["assigned"], 450000);
}

#[test]
fn assignments_and_targets_persist_and_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_today(dir.path(), "2026-10-01");
    let plan = Plan::new(&s);
    drop(s);
    let s = start_today(dir.path(), "2026-11-20");
    plan.assign(&s, "2026-10", &plan.groceries, 350000);
    let target = json!({"type": "refill", "amount": 5, "by_date": 3});
    expect(plan.set_target(&s, &plan.rent, &target), 200);
    let categories = expect(get(&s, &plan.path("categories")), 200);
    let made = plan.assignments(&s, "2026-10", &plan.groceries);
    drop(s);

    let s = start_today(dir.path(), "2026-11-20");
    assert_eq!(expect(get(&s, &plan.path("categories")), 200), categories);
    assert_eq!(plan.assignments(&s, "2026-10", &plan.groceries), made);
    drop(s);

    let file = dir.path().join("Household.sqlite");
    let migrate = |version: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
            .args(["migrate".as_ref(), file.as_os_str(), version.as_ref()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    migrate("2");
    migrate("3");
    let s = start_today(dir.path(), "2026-11-20");
    assert_eq!(
        plan.assignments(&s, "2026-10", &plan.groceries),
        json!([{"date": "2026-10-01", "amount": 350000}])
    );
    assert_eq!(
        plan.assignments(&s, "2026-11", &plan.rent),
        json!([{"date": "2026-11-01", "amount": 1200000}])
    );
    assert_eq!(plan.entry(&s, "2026-11", &plan.rent)["target"], Value::Null);
    assert_eq!(
        plan.entry(&s, "2026-10", &plan.groceries)["assigned"],
        350000
    );
}
