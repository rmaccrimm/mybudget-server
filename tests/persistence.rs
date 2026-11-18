//! SPEC 15: persistence.
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn data_dir() -> TempDir {
    tempfile::tempdir().unwrap()
}

/// The names of the files in `dir`, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn new_named_plan(s: &Server, name: &str) -> Value {
    expect(
        post(
            s,
            "/plans",
            &serde_json::json!({ "name": name }).to_string(),
        ),
        201,
    )
}

fn user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn migrate(file: &Path, version: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
        .args(["migrate".as_ref(), file.as_os_str(), version.as_ref()])
        .output()
        .unwrap()
}

/// Everything the API shows of a plan, to compare before and after a restart.
fn snapshot(s: &Server, plan: &str) -> Vec<Value> {
    let mut all = vec![expect(get(s, "/plans"), 200)];
    for path in ["", "/accounts", "/payees", "/categories", "/transactions"] {
        all.push(expect(get(s, &format!("/plans/{plan}{path}")), 200));
    }
    for month in ["2026-09", "2026-10", "2026-11", "2026-12"] {
        all.push(expect(
            get(s, &format!("/plans/{plan}/months/{month}")),
            200,
        ));
    }
    all
}

/// A plan using every kind of data sections 4 to 13 define.
fn fill(s: &Server, plan: &str) {
    let p = |path: &str| format!("/plans/{plan}{path}");
    let checking = id(&expect(
        post(
            s,
            &p("/accounts"),
            r#"{"name": "Checking", "type": "checking", "starting_balance": 300000, "starting_date": "2026-10-01", "note": "main"}"#,
        ),
        201,
    ));
    let visa = expect(
        post(
            s,
            &p("/accounts"),
            r#"{"name": "Visa", "type": "creditCard", "starting_balance": -20000, "starting_date": "2026-10-01"}"#,
        ),
        201,
    );
    let house = id(&expect(
        post(
            s,
            &p("/accounts"),
            r#"{"name": "House", "type": "otherAsset"}"#,
        ),
        201,
    ));
    let spare = id(&expect(
        post(s, &p("/accounts"), r#"{"name": "Spare", "type": "cash"}"#),
        201,
    ));
    expect(
        patch(s, &p(&format!("/accounts/{spare}")), r#"{"closed": true}"#),
        200,
    );
    expect(post(s, &p("/payees"), r#"{"name": "Corner Shop"}"#), 201);
    expect(
        post(s, &p("/payees"), r#"{"name": "Café \u0000 ☕\n"}"#),
        201,
    );
    let bills = id(&expect(
        post(s, &p("/category_groups"), r#"{"name": "Bills"}"#),
        201,
    ));
    let everyday = id(&expect(
        post(s, &p("/category_groups"), r#"{"name": "Everyday"}"#),
        201,
    ));
    let new_category = |group: &str, name: &str| {
        id(&expect(
            post(
                s,
                &p("/categories"),
                &format!(r#"{{"group_id": "{group}", "name": "{name}"}}"#),
            ),
            201,
        ))
    };
    let rent = new_category(&bills, "Rent");
    let groceries = new_category(&everyday, "Groceries");
    let dining = new_category(&everyday, "Dining");
    let gone = new_category(&everyday, "Gone");
    // Moving Rent puts it after Dining.
    expect(
        patch(
            s,
            &p(&format!("/categories/{rent}")),
            &format!(r#"{{"group_id": "{everyday}", "hidden": true}}"#),
        ),
        200,
    );
    expect(
        patch(
            s,
            &p(&format!("/category_groups/{bills}")),
            r#"{"hidden": true}"#,
        ),
        200,
    );
    for (category, month, amount) in [
        (&groceries, "2026-10", 40000),
        (&rent, "2026-11", 120000),
        (&gone, "2026-10", 500),
    ] {
        expect(
            patch(
                s,
                &p(&format!("/months/{month}/categories/{category}")),
                &format!(r#"{{"assigned": {amount}}}"#),
            ),
            200,
        );
    }
    let visa_payment = visa["payment_category_id"].as_str().unwrap();
    expect(
        patch(
            s,
            &p(&format!("/months/2026-10/categories/{visa_payment}")),
            r#"{"assigned": 5000}"#,
        ),
        200,
    );
    let visa_id = id(&visa);
    let visa_payee = visa["transfer_payee_id"].as_str().unwrap();
    let txn = |body: String| id(&expect(post(s, &p("/transactions"), &body), 201));
    txn(format!(
        r#"{{"account_id": "{checking}", "date": "2026-10-04", "amount": -12000, "payee_name": "Corner Shop",
            "category_id": "{groceries}", "memo": "weekly", "flag_color": "red", "cleared": "reconciled"}}"#
    ));
    txn(format!(
        r#"{{"account_id": "{visa_id}", "date": "2026-10-04", "amount": -9000, "payee_name": "Megastore",
            "subtransactions": [{{"amount": -6000, "category_id": "{groceries}"}},
                                {{"amount": -3000, "category_id": "{dining}", "memo": "lunch"}}]}}"#
    ));
    txn(format!(
        r#"{{"account_id": "{checking}", "date": "2026-10-20", "amount": -5000, "payee_id": "{visa_payee}", "approved": false}}"#
    ));
    let house_view = expect(get(s, &p(&format!("/accounts/{house}"))), 200);
    let house_payee = house_view["transfer_payee_id"].as_str().unwrap();
    txn(format!(
        r#"{{"account_id": "{checking}", "date": "2026-09-30", "amount": -1000, "payee_id": "{house_payee}", "category_id": "{dining}"}}"#
    ));
    let early = txn(format!(
        r#"{{"account_id": "{checking}", "date": "2026-10-01", "amount": -100, "category_id": "{gone}"}}"#
    ));
    txn(format!(
        r#"{{"account_id": "{checking}", "date": "2026-11-02", "amount": 70000, "category_id": "ready_to_assign"}}"#
    ));
    // Deleting an early transaction and a category leaves gaps behind.
    assert_eq!(delete(s, &p(&format!("/transactions/{early}"))).status, 204);
    assert_eq!(
        delete(s, &p(&format!("/categories/{gone}?reassign_to={dining}"))).status,
        204
    );
    expect(patch(s, &p(""), r#"{"currency_code": "EUR"}"#), 200);
}

#[test]
fn a_plan_survives_being_killed_and_restarted() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let plan = id(&new_named_plan(&s, "Household"));
    fill(&s, &plan);
    let before = snapshot(&s, &plan);
    drop(s);
    let s = start_on(dir.path());
    assert_eq!(snapshot(&s, &plan), before);
    // Changes after a restart are saved too.
    expect(
        post(&s, &format!("/plans/{plan}/payees"), r#"{"name": "Later"}"#),
        201,
    );
    let before = snapshot(&s, &plan);
    drop(s);
    let s = start_on(dir.path());
    assert_eq!(snapshot(&s, &plan), before);
}

#[test]
fn plans_keep_their_order() {
    let dir = data_dir();
    let s = start_on(dir.path());
    for name in ["Zeta", "Alpha", "Mid"] {
        new_named_plan(&s, name);
    }
    let before = expect(get(&s, "/plans"), 200);
    drop(s);
    let s = start_on(dir.path());
    assert_eq!(expect(get(&s, "/plans"), 200), before);
    new_named_plan(&s, "Beta");
    let names: Vec<String> = expect(get(&s, "/plans"), 200)
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["Zeta", "Alpha", "Mid", "Beta"]);
}

#[test]
fn ids_are_lowercase_version_4_uuids() {
    let s = start();
    let plan = new_named_plan(&s, "Household");
    let id = plan["id"].as_str().unwrap();
    let uuid = uuid::Uuid::parse_str(id).unwrap();
    assert_eq!(uuid.get_version_num(), 4);
    assert_eq!(id, uuid.hyphenated().to_string());
}

#[test]
fn files_are_named_after_plans() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let household = new_named_plan(&s, "Household");
    let twin = new_named_plan(&s, "Household");
    new_named_plan(&s, "Bills/Rent: 2027");
    new_named_plan(&s, " .hidden. ");
    new_named_plan(&s, "...");
    let twin_file = format!("Household ({}).sqlite", &id(&twin)[..8]);
    assert_eq!(
        files(dir.path()),
        [
            "Bills_Rent_ 2027.sqlite",
            &twin_file,
            "Household.sqlite",
            "Plan.sqlite",
            "hidden.sqlite"
        ]
    );
    let file = dir.path().join("Household.sqlite");
    assert_eq!(user_version(&file), 1);

    // A rename that keeps the safe name keeps the file, suffix included.
    expect(
        patch(
            &s,
            &format!("/plans/{}", id(&twin)),
            r#"{"name": "Household."}"#,
        ),
        200,
    );
    assert!(dir.path().join(&twin_file).exists());
    // The plan's own file never clashes, even when only the case changes.
    expect(
        patch(
            &s,
            &format!("/plans/{}", id(&household)),
            r#"{"name": "household"}"#,
        ),
        200,
    );
    assert!(
        files(dir.path()).contains(&"household.sqlite".to_owned()),
        "{:?}",
        files(dir.path())
    );
    expect(
        patch(
            &s,
            &format!("/plans/{}", id(&household)),
            r#"{"name": "Groceries"}"#,
        ),
        200,
    );
    assert!(files(dir.path()).contains(&"Groceries.sqlite".to_owned()));
    // Deleting a plan deletes its file.
    assert_eq!(
        delete(&s, &format!("/plans/{}", id(&household))).status,
        204
    );
    assert!(
        !files(dir.path())
            .iter()
            .any(|f| f.starts_with("Groceries") || f.starts_with("household"))
    );
    // Nothing but plan files is left between requests.
    assert!(
        files(dir.path()).iter().all(|f| f.ends_with(".sqlite")),
        "{:?}",
        files(dir.path())
    );
}

#[test]
fn journal_files_left_beside_a_plan_file_are_gone_once_it_starts() {
    let stale: [(&str, &[u8]); 7] = [
        ("-journal", b""),
        ("-journal", &[0; 512]),
        ("-journal", b"junk"),
        ("-wal", b""),
        ("-shm", b""),
        ("-shm", &[0; 32768]),
        ("-shm", b"junk"),
    ];
    for (suffix, bytes) in stale {
        let dir = data_dir();
        let s = start_on(dir.path());
        let plan = id(&new_named_plan(&s, "Household"));
        let before = snapshot(&s, &plan);
        drop(s);
        fs::write(dir.path().join(format!("Household.sqlite{suffix}")), bytes).unwrap();
        let s = start_on(dir.path());
        assert_eq!(
            files(dir.path()),
            ["Household.sqlite"],
            "{suffix} of {} bytes",
            bytes.len()
        );
        assert_eq!(snapshot(&s, &plan), before);
    }
}

#[test]
fn a_plan_file_in_wal_mode_is_served_and_leaves_no_wal() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let plan = id(&new_named_plan(&s, "Household"));
    drop(s);
    let file = dir.path().join("Household.sqlite");
    // Closing without a checkpoint leaves the change in the WAL only.
    let conn = Connection::open(&file).unwrap();
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute("UPDATE plan SET name = 'Changed'", [])
        .unwrap();
    drop(conn);
    assert!(files(dir.path()).contains(&"Household.sqlite-wal".to_owned()));

    let s = start_on(dir.path());
    assert_eq!(files(dir.path()), ["Household.sqlite"]);
    assert_eq!(
        expect(get(&s, &format!("/plans/{plan}")), 200)["name"],
        "Changed"
    );
    expect(
        post(&s, &format!("/plans/{plan}/payees"), r#"{"name": "Shop"}"#),
        201,
    );
    assert_eq!(files(dir.path()), ["Household.sqlite"]);
    let mode: String = Connection::open(&file)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
}

#[test]
fn files_it_cant_use_are_skipped_and_left_alone() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let kept = new_named_plan(&s, "Kept");
    let copied = new_named_plan(&s, "Copied");
    drop(s);
    fs::copy(
        dir.path().join("Copied.sqlite"),
        dir.path().join("Copy of Copied.sqlite"),
    )
    .unwrap();
    fs::write(dir.path().join("Garbage.sqlite"), "not a database").unwrap();
    fs::write(dir.path().join("Empty.sqlite"), "").unwrap();
    fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    fs::create_dir(dir.path().join("Folder.sqlite")).unwrap();
    let newer = dir.path().join("Newer.sqlite");
    Connection::open(&newer)
        .unwrap()
        .execute_batch("CREATE TABLE t (x); PRAGMA user_version = 99;")
        .unwrap();
    let unrelated = dir.path().join("Unrelated.sqlite");
    Connection::open(&unrelated)
        .unwrap()
        .execute_batch("CREATE TABLE t (x); PRAGMA user_version = 1;")
        .unwrap();
    let before: Vec<(String, Vec<u8>)> = files(dir.path())
        .into_iter()
        .filter(|f| f != "Folder.sqlite")
        .map(|f| {
            let bytes = fs::read(dir.path().join(&f)).unwrap();
            (f, bytes)
        })
        .collect();

    let mut s = start_with(|c| {
        c.env("MYBUDGET_DATA", dir.path()).stderr(Stdio::piped());
    });
    let plans = expect(get(&s, "/plans"), 200);
    assert_eq!(plans, serde_json::json!([kept]));
    // Skipped names still count as taken.
    let garbage = new_named_plan(&s, "Garbage");
    let copied_again = new_named_plan(&s, "Copied");
    assert!(
        dir.path()
            .join(format!("Garbage ({}).sqlite", &id(&garbage)[..8]))
            .exists()
    );
    assert!(
        dir.path()
            .join(format!("Copied ({}).sqlite", &id(&copied_again)[..8]))
            .exists()
    );
    let _ = copied;

    for (name, bytes) in before.iter().filter(|(f, _)| f != "Kept.sqlite") {
        assert_eq!(
            &fs::read(dir.path().join(name)).unwrap(),
            bytes,
            "{name} changed"
        );
    }
    s.child.kill().unwrap();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut s.child.stderr.take().unwrap(), &mut stderr).unwrap();
    for name in [
        "Copied.sqlite",
        "Copy of Copied.sqlite",
        "Garbage.sqlite",
        "Empty.sqlite",
        "Newer.sqlite",
        "Unrelated.sqlite",
    ] {
        assert!(
            stderr.lines().any(|l| l.contains(name)),
            "{name} isn't named in:\n{stderr}"
        );
    }
    assert!(!stderr.contains("notes.txt"));
}

#[test]
fn migrate_changes_only_plan_files_to_known_versions() {
    let dir = data_dir();
    let s = start_on(dir.path());
    new_named_plan(&s, "Household");
    drop(s);
    let file = dir.path().join("Household.sqlite");
    let bytes = fs::read(&file).unwrap();
    let out = migrate(&file, "1");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(&file).unwrap(), bytes);
    for bad in ["0", "2", "-1", "1.0", "one", ""] {
        let out = migrate(&file, bad);
        assert!(!out.status.success(), "{bad}");
        assert!(!out.stderr.is_empty());
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
    let garbage = dir.path().join("garbage.sqlite");
    fs::write(&garbage, "not a database").unwrap();
    let empty = dir.path().join("empty.sqlite");
    Connection::open(&empty)
        .unwrap()
        .execute_batch("CREATE TABLE t (x);")
        .unwrap();
    let newer = dir.path().join("newer.sqlite");
    Connection::open(&newer)
        .unwrap()
        .execute_batch("PRAGMA user_version = 7;")
        .unwrap();
    for file in [&garbage, &empty, &newer, &dir.path().join("missing.sqlite")] {
        let before = fs::read(file).ok();
        assert!(!migrate(file, "1").status.success(), "{}", file.display());
        assert_eq!(fs::read(file).ok(), before);
    }
}

#[test]
fn an_unusable_data_directory_stops_the_server() {
    let dir = data_dir();
    let blocker = dir.path().join("file");
    fs::write(&blocker, "").unwrap();
    for data in [blocker.join("data"), blocker.clone()] {
        let out = Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
            .env("MYBUDGET_ADDR", "127.0.0.1:0")
            .env("MYBUDGET_DATA", &data)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

#[test]
fn the_data_directory_is_created_with_its_parents() {
    let dir = data_dir();
    let data = dir.path().join("a/b/c");
    let s = start_on(&data);
    new_named_plan(&s, "Household");
    assert_eq!(files(&data), ["Household.sqlite"]);
}

#[test]
fn a_change_that_cant_be_saved_returns_500_and_changes_nothing() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let plan = id(&new_named_plan(&s, "Household"));
    let before = snapshot(&s, &plan);
    let read_only =
        |mode| fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).unwrap();
    read_only(0o555);
    err(&post(&s, "/plans", r#"{"name": "Other"}"#), 500);
    err(
        &patch(&s, &format!("/plans/{plan}"), r#"{"name": "Renamed"}"#),
        500,
    );
    err(
        &patch(&s, &format!("/plans/{plan}"), r#"{"currency_code": "EUR"}"#),
        500,
    );
    err(
        &post(
            &s,
            &format!("/plans/{plan}/category_groups"),
            r#"{"name": "Bills"}"#,
        ),
        500,
    );
    err(&delete(&s, &format!("/plans/{plan}")), 500);
    // Errors that come first still do.
    err(
        &post(&s, &format!("/plans/{plan}/category_groups"), r#"{}"#),
        400,
    );
    assert_eq!(snapshot(&s, &plan), before);
    read_only(0o755);
    assert_eq!(files(dir.path()), ["Household.sqlite"]);
    drop(s);
    let s = start_on(dir.path());
    assert_eq!(snapshot(&s, &plan), before);
}

#[test]
fn a_read_only_plan_file_keeps_its_name_and_data() {
    let dir = data_dir();
    let s = start_on(dir.path());
    let plan = id(&new_named_plan(&s, "Household"));
    let before = snapshot(&s, &plan);
    let file = dir.path().join("Household.sqlite");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    // The file can be renamed but not written, so it gets its name back.
    err(
        &patch(&s, &format!("/plans/{plan}"), r#"{"name": "Renamed"}"#),
        500,
    );
    err(
        &post(&s, &format!("/plans/{plan}/payees"), r#"{"name": "Shop"}"#),
        500,
    );
    assert_eq!(snapshot(&s, &plan), before);
    assert_eq!(files(dir.path()), ["Household.sqlite"]);
    // Changing nothing needs no save.
    expect(patch(&s, &format!("/plans/{plan}"), r#"{}"#), 200);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    expect(
        patch(&s, &format!("/plans/{plan}"), r#"{"name": "Renamed"}"#),
        200,
    );
    assert_eq!(files(dir.path()), ["Renamed.sqlite"]);
}
