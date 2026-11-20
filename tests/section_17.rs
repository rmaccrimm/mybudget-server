//! Importing OFX files (SPEC 17), and the earlier clauses it changes
//! (SPEC 3.13, 15.9, 16.4).

mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use serde_json::{Value, json};

/// SPEC 17.6's file, in Windows-1252.
const FILE: &str = "OFXHEADER:100\r\nDATA:OFXSGML\r\nVERSION:102\r\nENCODING:USASCII\r\nCHARSET:1252\r\n\r\n\
<OFX>\n\
<SIGNONMSGSRSV1><SONRS><STATUS><CODE>0<SEVERITY>INFO</STATUS></SONRS></SIGNONMSGSRSV1>\n\
<BANKMSGSRSV1><STMTTRNRS><TRNUID>1<STMTRS>\n\
<CURDEF>USD\n\
<BANKACCTFROM><BANKID>021000021<ACCTID>000123456789<ACCTTYPE>CHECKING</BANKACCTFROM>\n\
<BANKTRANLIST><DTSTART>20261001<DTEND>20261031\n\
<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20261005120000.000[-5:EST]<TRNAMT>-120.00\n\
<FITID>202610050001<NAME>CORNER SHOP #42<MEMO>POS PURCHASE</STMTTRN>\n\
<STMTTRN><TRNTYPE>CREDIT<DTPOSTED>20261015<TRNAMT>1800.00<FITID>202610150001\n\
<NAME>ACME PAYROLL</STMTTRN>\n\
<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20261020<TRNAMT>-64.99<FITID>202610200001\n\
<NAME>CITY POWER & LIGHT</STMTTRN>\n\
</BANKTRANLIST>\n\
<LEDGERBAL><BALAMT>4115.01<DTASOF>20261031</LEDGERBAL>\n\
</STMTRS></STMTTRNRS></BANKMSGSRSV1>\n\
</OFX>\n";

fn encode(file: &[u8]) -> String {
    STANDARD.encode(file)
}

struct Fx {
    s: Server,
    p: String,
    checking: String,
}

impl Fx {
    fn new() -> Self {
        let s = start();
        let p = new_plan(&s);
        let body = json!({"name": "Checking", "type": "checking",
                          "starting_balance": 2_500_000, "starting_date": "2026-10-01"});
        let checking = id(&expect(
            post(&s, &format!("/plans/{p}/accounts"), &body.to_string()),
            201,
        ));
        Fx { s, p, checking }
    }

    fn path(&self, rest: &str) -> String {
        format!("/plans/{}/{rest}", self.p)
    }

    fn post(&self, rest: &str, body: &Value) -> Resp {
        post(&self.s, &self.path(rest), &body.to_string())
    }

    fn preview(&self, file: &[u8]) -> Resp {
        self.post("imports/preview", &json!({"file": encode(file)}))
    }

    fn import(&self, file: &[u8], statements: &Value) -> Resp {
        self.post(
            "imports",
            &json!({"file": encode(file), "statements": statements}),
        )
    }

    fn shop(&self) -> Value {
        let body = json!({"account_id": self.checking, "date": "2026-10-04", "amount": -120000,
                          "payee_name": "Corner Shop", "memo": "weekly shop"});
        expect(self.post("transactions", &body), 201)
    }
}

#[test]
fn preview_shows_the_file() {
    let f = Fx::new();
    let preview = expect(f.preview(FILE.as_bytes()), 200);
    assert_eq!(
        preview,
        json!({"statements": [
          {"type": "bank", "bank_id": "021000021", "account_number": "000123456789",
           "account_type": "CHECKING", "currency_code": "USD",
           "start_date": "2026-10-01", "end_date": "2026-10-31",
           "ledger_balance": 4_115_010, "ledger_balance_date": "2026-10-31",
           "account_id": null, "swap_payee_memo": false, "invert_amounts": false,
           "transactions": [
            {"import_id": "202610050001", "type": "DEBIT", "date": "2026-10-05", "amount": -120_000,
             "name": "CORNER SHOP #42", "memo": "POS PURCHASE"},
            {"import_id": "202610150001", "type": "CREDIT", "date": "2026-10-15", "amount": 1_800_000,
             "name": "ACME PAYROLL", "memo": null},
            {"import_id": "202610200001", "type": "DEBIT", "date": "2026-10-20", "amount": -64990,
             "name": "CITY POWER & LIGHT", "memo": null}]}]})
    );
}

#[test]
fn import_matches_creates_and_skips_duplicates() {
    let f = Fx::new();
    let shop = f.shop();
    let statements = json!([{"account_id": f.checking}]);
    let result = expect(f.import(FILE.as_bytes(), &statements), 200);
    let entry = &result["statements"][0];
    assert_eq!(entry["account_id"], f.checking.as_str());
    assert_eq!(entry["duplicates"], json!([]));
    let matched = &entry["matched"][0];
    assert_eq!(matched["id"], shop["id"]);
    assert_eq!(matched["cleared"], "cleared");
    assert_eq!(matched["date"], "2026-10-04");
    assert_eq!(
        matched["import"],
        json!({"id": "202610050001", "date": "2026-10-05", "amount": -120_000,
               "payee_name": "CORNER SHOP #42", "memo": "POS PURCHASE", "matched": true})
    );
    let imported = entry["imported"].as_array().unwrap();
    assert_eq!(imported.len(), 2);
    assert_eq!(imported[0]["category_id"], "ready_to_assign");
    assert_eq!(imported[0]["approved"], false);
    assert_eq!(imported[1]["category_id"], Value::Null);
    assert_eq!(imported[1]["import"]["payee_name"], "CITY POWER & LIGHT");
    let account = expect(get(&f.s, &f.path(&format!("accounts/{}", f.checking))), 200);
    assert_eq!(account["balance"], 4_115_010);

    let uncategorized = expect(get(&f.s, &f.path("transactions?uncategorized=true")), 200);
    // The shop transaction was entered without a category.
    assert_eq!(uncategorized.as_array().unwrap().last(), Some(&imported[1]));

    let again = expect(f.import(FILE.as_bytes(), &statements), 200);
    let entry = &again["statements"][0];
    assert_eq!(entry["matched"], json!([]));
    assert_eq!(entry["imported"], json!([]));
    let duplicates: Vec<&Value> = entry["duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| &d["transaction_id"])
        .collect();
    assert_eq!(
        duplicates,
        [&shop["id"], &imported[0]["id"], &imported[1]["id"]]
    );
    assert_eq!(
        entry["duplicates"][0],
        json!({"id": "202610050001", "date": "2026-10-05", "amount": -120_000,
               "payee_name": "CORNER SHOP #42", "memo": "POS PURCHASE",
               "transaction_id": shop["id"]})
    );
    let preview = expect(f.preview(FILE.as_bytes()), 200);
    assert_eq!(preview["statements"][0]["account_id"], f.checking.as_str());

    // SPEC 17.14.
    let shop_id = shop["id"].as_str().unwrap();
    let unmatched = expect(
        f.post(&format!("transactions/{shop_id}/unmatch"), &json!({})),
        200,
    );
    assert_eq!(unmatched["transaction"]["import"], Value::Null);
    assert_eq!(unmatched["transaction"]["cleared"], "cleared");
    let new = &unmatched["imported"];
    assert_eq!(new["memo"], "POS PURCHASE");
    assert_eq!(new["import"]["id"], "202610050001");
    assert_eq!(new["import"]["matched"], false);
    err(
        &f.post(&format!("transactions/{shop_id}/unmatch"), &json!({})),
        409,
    );
}

#[test]
fn a_changed_fitid_is_a_duplicate_of_a_nearby_import() {
    let f = Fx::new();
    let statements = json!([{"account_id": f.checking}]);
    expect(f.import(FILE.as_bytes(), &statements), 200);
    let changed = FILE.replace("202610150001", "OTHER");
    let again = expect(f.import(changed.as_bytes(), &statements), 200);
    let duplicates = again["statements"][0]["duplicates"].as_array().unwrap();
    assert_eq!(duplicates.len(), 3);
    assert_eq!(duplicates[1]["id"], "OTHER");
    assert!(duplicates[1]["transaction_id"].is_string());

    let statements = json!([{"account_id": f.checking, "not_duplicates": ["OTHER"]}]);
    let forced = expect(f.import(changed.as_bytes(), &statements), 200);
    let entry = &forced["statements"][0];
    assert_eq!(entry["duplicates"].as_array().unwrap().len(), 2);
    assert_eq!(entry["imported"][0]["import"]["id"], "OTHER");
}

#[test]
fn a_first_check_duplicate_takes_its_transaction_from_the_second_check() {
    let f = Fx::new();
    let statements = json!([{"account_id": f.checking}]);
    expect(f.import(FILE.as_bytes(), &statements), 200);
    // The payroll is listed again with a new FITID next to the old one: the
    // old one is a duplicate by id, so the new one can't pair with it.
    let extra = "<STMTTRN><DTPOSTED>20261016<TRNAMT>1800.00<FITID>NEW</STMTTRN>\n";
    let file = FILE.replace("</BANKTRANLIST>", &format!("{extra}</BANKTRANLIST>"));
    let again = expect(f.import(file.as_bytes(), &statements), 200);
    let entry = &again["statements"][0];
    assert_eq!(entry["duplicates"].as_array().unwrap().len(), 3);
    assert_eq!(entry["imported"][0]["import"]["id"], "NEW");
}

#[test]
fn a_repeated_id_is_a_duplicate_across_statements() {
    let f = Fx::new();
    let statements = json!([{"account_id": f.checking}]);
    let first = expect(f.import(FILE.as_bytes(), &statements), 200);
    let payroll = first["statements"][0]["imported"][1]["id"].clone();
    // Both statements list the payroll with a changed FITID: the first pairs
    // it with the earlier import, and the second is a duplicate by id.
    let changed = FILE.replace("202610150001", "OTHER");
    let twice = json!([{"account_id": f.checking}, {"account_id": f.checking}]);
    let body = changed.replace(
        "</BANKMSGSRSV1>",
        &format!(
            "{}</BANKMSGSRSV1>",
            &changed
                [changed.find("<STMTTRNRS>").unwrap()..changed.find("</BANKMSGSRSV1>").unwrap()]
        ),
    );
    let again = expect(f.import(body.as_bytes(), &twice), 200);
    let first_dup = &again["statements"][0]["duplicates"][1];
    assert_eq!(first_dup["id"], "OTHER");
    assert_eq!(first_dup["transaction_id"], payroll);
    let second_dup = &again["statements"][1]["duplicates"][1];
    assert_eq!(second_dup["id"], "OTHER");
    assert_eq!(second_dup["transaction_id"], Value::Null);
    assert_eq!(again["statements"][1]["imported"], json!([]));
}

#[test]
fn bad_requests() {
    let f = Fx::new();
    let a = &f.checking;
    for file in ["", "not base64!", "T0ZY SEVB", "T0ZYSEVBREVSOjEwMA"] {
        err(&f.post("imports/preview", &json!({ "file": file })), 400);
    }
    err(&f.post("imports/preview", &json!({})), 400);
    err(&f.preview(b"OFXHEADER:100\n<OFX></OFX>"), 400);
    err(&f.preview(b"<OFX><STMTRS></STMTRS></OFX>"), 400);
    err(
        &f.preview(&FILE.replace("-64.99", "1,234.56").into_bytes()),
        400,
    );
    err(
        &f.preview(&FILE.replace("</SONRS>", "</NOPE>").into_bytes()),
        400,
    );
    for statements in [
        json!([]),
        json!([null, null]),
        json!([{}]),
        json!([{"account_id": "nope"}]),
        json!([{"account_id": a, "swap_payee_memo": 1}]),
        json!([{"account_id": a, "not_duplicates": [1]}]),
        json!({}),
    ] {
        err(&f.import(FILE.as_bytes(), &statements), 400);
    }
    let skipped = expect(f.import(FILE.as_bytes(), &json!([null])), 200);
    assert_eq!(skipped, json!({"statements": [null]}));
    err(&get(&f.s, &f.path("imports")), 405);
    err(&get(&f.s, &f.path("imports/preview")), 405);
    err(&post(&f.s, "/plans/nope/imports", "{}"), 404);
}

#[test]
fn import_bodies_may_be_larger() {
    let f = Fx::new();
    let padding = format!("<X>{}</X>", "y".repeat(4 << 20));
    let big = FILE.replace("<OFX>", &format!("<OFX>{padding}"));
    expect(f.preview(big.as_bytes()), 200);
    err(
        &post(
            &f.s,
            &f.path("transactions"),
            &format!(r#"{{"x": "{}"}}"#, "y".repeat(2 << 20)),
        ),
        413,
    );
}

#[test]
fn utf8_and_xml_files() {
    let f = Fx::new();
    let xml = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?><?OFX OFXHEADER=\"200\"?>\
               <OFX><CCSTMTRS><CCACCTFROM><BANKID>1</BANKID><ACCTID>42</ACCTID></CCACCTFROM>\
               <BANKTRANLIST><STMTTRN><DTPOSTED>20261001</DTPOSTED><TRNAMT>5.</TRNAMT>\
               <PAYEE><NAME>Café &amp; AT&T</NAME></PAYEE></STMTTRN>\
               <STMTTRN><DTPOSTED>20261001<TRNAMT>5</STMTTRN></BANKTRANLIST></CCSTMTRS></OFX>";
    let preview = expect(f.preview(xml.as_bytes()), 200);
    let statement = &preview["statements"][0];
    assert_eq!(statement["type"], "credit_card");
    assert_eq!(statement["bank_id"], Value::Null);
    assert_eq!(statement["account_number"], "42");
    let t = &statement["transactions"];
    assert_eq!(t[0]["name"], "Café & AT&T");
    assert_eq!(t[0]["import_id"], "5000:2026-10-01:1");
    assert_eq!(t[1]["import_id"], "5000:2026-10-01:2");
}

/// SPEC 17.15: imports are kept, and migrating to version 1 drops them.
#[test]
fn imports_persist_and_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let s = start_on(dir.path());
    let p = new_plan(&s);
    let body = json!({"name": "Checking", "type": "checking"});
    let checking = id(&expect(
        post(&s, &format!("/plans/{p}/accounts"), &body.to_string()),
        201,
    ));
    let import = json!({"file": encode(FILE.as_bytes()),
                        "statements": [{"account_id": checking, "invert_amounts": true}]})
    .to_string();
    let imports = format!("/plans/{p}/imports");
    let preview = json!({"file": encode(FILE.as_bytes())}).to_string();
    let previews = format!("{imports}/preview");
    expect(post(&s, &imports, &import), 200);
    let transactions = format!("/plans/{p}/transactions");
    let before = expect(get(&s, &transactions), 200);
    drop(s);

    let s = start_on(dir.path());
    assert_eq!(expect(get(&s, &transactions), 200), before);
    let again = expect(post(&s, &imports, &import), 200);
    let duplicates = again["statements"][0]["duplicates"].as_array().unwrap();
    assert_eq!(duplicates.len(), 3);
    let remembered = expect(post(&s, &previews, &preview), 200);
    assert_eq!(remembered["statements"][0]["invert_amounts"], true);
    drop(s);

    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
        .unwrap();
    for version in ["1", "2"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
            .args(["migrate".as_ref(), file.as_os_str(), version.as_ref()])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
    }
    let s = start_on(dir.path());
    let mut expected = before.clone();
    for t in expected.as_array_mut().unwrap() {
        t["import"] = Value::Null;
    }
    assert_eq!(expect(get(&s, &transactions), 200), expected);
    let forgotten = expect(post(&s, &previews, &preview), 200);
    assert_eq!(forgotten["statements"][0]["account_id"], Value::Null);
    let fresh = expect(post(&s, &imports, &import), 200);
    // None is a duplicate: each matches the transaction it once created.
    let entry = &fresh["statements"][0];
    assert_eq!(entry["duplicates"], json!([]));
    assert_eq!(entry["matched"].as_array().unwrap().len(), 3);
}
