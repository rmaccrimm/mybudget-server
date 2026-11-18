mod common;

use common::*;

#[test]
fn body_limits() {
    let s = start();
    for bad in [
        &br#"{"name": "a", "x": "\ud800\ud800"}"#[..],
        br#"{"name": "a", "x": "\ud800\n"}"#,
        br#"{"name": "a", "x": "\ud800"}"#,
        br#"{"name": "a", "x": "\uDFFF\uD800"}"#,
        br#"{"name": "a", "\ud800": 1}"#,
        b"{\"name\": \"a\", \"x\": \"\xff\"}",
        b"\xef\xbb\xbf{\"name\": \"a\"}",
        br#"{"name": "a", "x": 1e400}"#,
    ] {
        err(&req(&s, "POST", "/plans", Some(bad)), 400);
    }
    let deep = format!(
        r#"{{"name": "a", "x": {}1{}}}"#,
        "[".repeat(63),
        "]".repeat(63)
    );
    assert_eq!(req(&s, "POST", "/plans", Some(deep.as_bytes())).status, 201);
    let ok =
        br#"{"name": "a", "x": [1e300, -0, 100000000000000000000000, "\\ud800", {"y": null}]}"#;
    assert_eq!(req(&s, "POST", "/plans", Some(ok)).status, 201);
    // Ignored fields may hold any JSON, including keys serde_json treats specially.
    let magic =
        br#"{"name": "a", "x": {"$serde_json::private::RawValue": "nope"}, "y": {"$serde_json::private::Number": 1}}"#;
    assert_eq!(req(&s, "POST", "/plans", Some(magic)).status, 201);

    let just_under = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat((1 << 20) - 22));
    assert_eq!(just_under.len(), 1 << 20);
    assert_eq!(
        req(&s, "POST", "/plans", Some(just_under.as_bytes())).status,
        201
    );
    let over = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat(1 << 20));
    err(&req(&s, "POST", "/plans", Some(over.as_bytes())), 413);
    let huge = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat(5 << 20));
    err(&req(&s, "POST", "/plans", Some(huge.as_bytes())), 413);
    err(&req(&s, "PATCH", "/plans/nope", Some(huge.as_bytes())), 413);
    assert_eq!(
        req(&s, "GET", "/plans", None)
            .json()
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn routing() {
    let s = start();
    let id = new_plan(&s);
    for path in [
        "/plans/",
        "//plans",
        "/plans//accounts",
        "/health/",
        "/plans/x/months//categories/y",
    ] {
        err(&get(&s, path), 404);
    }
    // 405 before 404, with the methods the path takes.
    for (method, path) in [
        ("PUT", "/plans/nope"),
        ("GET", "/plans/nope/category_groups"),
        ("FOO", "/plans"),
    ] {
        let r = req(&s, method, path, None);
        err(&r, 405);
        assert!(
            r.headers.to_ascii_lowercase().contains("\r\nallow: "),
            "{}",
            r.headers
        );
    }
    let r = req(&s, "DELETE", "/plans", None);
    let allow = r.headers.lines().find_map(|l| {
        l.to_ascii_lowercase()
            .strip_prefix("allow: ")
            .map(str::to_owned)
    });
    let mut allow: Vec<String> = allow
        .unwrap()
        .split(',')
        .map(|m| m.trim().to_owned())
        .collect();
    allow.sort();
    assert_eq!(allow, ["get", "head", "post"]);
    // HEAD is GET without the body.
    let r = req(&s, "HEAD", &format!("/plans/{id}"), None);
    assert_eq!((r.status, r.body.as_str()), (200, ""));
    assert_eq!(req(&s, "HEAD", "/plans/nope", None).status, 404);
    // 413 wins over everything, including the fallbacks.
    let huge = vec![b'x'; 2 << 20];
    err(&req(&s, "POST", "/nope", Some(&huge)), 413);
    err(&req(&s, "PUT", "/plans", Some(&huge)), 413);
    // Query parameters are percent-decoded; unknown ones are ignored.
    let path = format!("/plans/{id}/transactions?since_date=2026%2D01%2D01&x=%ZZ&approved=true");
    assert_eq!(expect(get(&s, &path), 200), serde_json::json!([]));
    err(
        &get(
            &s,
            &format!("/plans/{id}/transactions?since_date=2026-01-01&since_date=2026-02-30"),
        ),
        400,
    );
}

#[test]
fn plans_crud_and_errors() {
    let s = start();
    let r = req(&s, "GET", "/health", None);
    assert_eq!((r.status, r.body.as_str()), (200, "ok"));
    err(&req(&s, "GET", "/nope", None), 404);
    err(&req(&s, "DELETE", "/plans", None), 405);
    err(&req(&s, "PUT", "/health", None), 405);

    let r = req(
        &s,
        "POST",
        "/plans",
        Some(br#"{"name": "Household", "extra": 1}"#),
    );
    assert_eq!(r.status, 201);
    let p = r.json();
    assert_eq!(p["name"], "Household");
    assert_eq!(p["currency_code"], "USD");
    let id = p["id"].as_str().unwrap().to_string();

    for bad in [
        &b""[..],
        b"   ",
        b"[]",
        b"null",
        b"{",
        br#"{"name": "a\ud800b"}"#,
        br#"{"name": "ok", "x": "\udc00"}"#,
        b"{}",
        br#"{"name": null}"#,
        br#"{"name": "   "}"#,
        br#"{"name": 5}"#,
        br#"{"name": "a", "currency_code": "usd"}"#,
        br#"{"name": "a", "currency_code": null}"#,
        br#"{"name": "a", "currency_code": "USDX"}"#,
    ] {
        err(&req(&s, "POST", "/plans", Some(bad)), 400);
    }
    let long: String = "é".repeat(100);
    assert_eq!(
        req(
            &s,
            "POST",
            "/plans",
            Some(format!(r#"{{"name": "{long}"}}"#).as_bytes())
        )
        .status,
        201
    );
    err(
        &req(
            &s,
            "POST",
            "/plans",
            Some(format!(r#"{{"name": "{long}x"}}"#).as_bytes()),
        ),
        400,
    );
    // Paired surrogates are fine.
    assert_eq!(
        req(
            &s,
            "POST",
            "/plans",
            Some(format!(r#"{{"name": "{}ud83d{}ude00"}}"#, '\\', '\\').as_bytes())
        )
        .status,
        201
    );

    let list = req(&s, "GET", "/plans", None).json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["id"], id.as_str());

    let path = format!("/plans/{id}");
    let r = req(&s, "PATCH", &path, Some(br#"{"currency_code": "EUR"}"#));
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["currency_code"], "EUR");
    assert_eq!(r.json()["name"], "Household");
    assert_eq!(
        req(&s, "PATCH", &path, Some(b"{}")).json()["currency_code"],
        "EUR"
    );
    err(
        &req(
            &s,
            "PATCH",
            &path,
            Some(br#"{"name": "New", "currency_code": "x"}"#),
        ),
        400,
    );
    assert_eq!(req(&s, "GET", &path, None).json()["name"], "Household");
    err(&req(&s, "POST", &path, None), 405);
    err(&req(&s, "PATCH", "/plans/missing", Some(b"bad")), 404);

    let r = req(&s, "DELETE", &path, None);
    assert_eq!((r.status, r.body.as_str()), (204, ""));
    err(&req(&s, "GET", &path, None), 404);
    err(&req(&s, "DELETE", &path, None), 404);
}
