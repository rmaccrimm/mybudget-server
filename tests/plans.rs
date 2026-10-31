//! Starts the real binary and talks raw HTTP/1.1 to it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

struct Server {
    child: Child,
    addr: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start() -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
        .env("MYBUDGET_ADDR", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let addr = line.trim().strip_prefix("listening on ").unwrap().to_string();
    Server { child, addr }
}

struct Resp {
    status: u16,
    headers: String,
    body: String,
}

impl Resp {
    fn json(&self) -> serde_json::Value {
        assert!(self.headers.to_ascii_lowercase().contains("content-type: application/json"), "{}", self.headers);
        serde_json::from_str(&self.body).unwrap()
    }
}

fn req(s: &Server, method: &str, path: &str, body: Option<&[u8]>) -> Resp {
    let mut stream = TcpStream::connect(&s.addr).unwrap();
    let body = body.unwrap_or(b"");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = headers[9..12].parse().unwrap();
    Resp { status, headers: headers.to_string(), body: body.to_string() }
}

fn err(r: &Resp, status: u16) {
    assert_eq!(r.status, status, "{}", r.body);
    assert!(!r.json()["error"].as_str().unwrap().is_empty());
}

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
    let deep = format!(r#"{{"name": "a", "x": {}1{}}}"#, "[".repeat(63), "]".repeat(63));
    assert_eq!(req(&s, "POST", "/plans", Some(deep.as_bytes())).status, 201);
    let ok = br#"{"name": "a", "x": [1e300, -0, 100000000000000000000000, "\\ud800", {"y": null}]}"#;
    assert_eq!(req(&s, "POST", "/plans", Some(ok)).status, 201);

    let just_under = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat((1 << 20) - 22));
    assert_eq!(just_under.len(), 1 << 20);
    assert_eq!(req(&s, "POST", "/plans", Some(just_under.as_bytes())).status, 201);
    let over = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat(1 << 20));
    err(&req(&s, "POST", "/plans", Some(over.as_bytes())), 413);
    let huge = format!(r#"{{"name": "a", "x": "{}"}}"#, "a".repeat(5 << 20));
    err(&req(&s, "POST", "/plans", Some(huge.as_bytes())), 413);
    err(&req(&s, "PATCH", "/plans/nope", Some(huge.as_bytes())), 413);
    assert_eq!(req(&s, "GET", "/plans", None).json().as_array().unwrap().len(), 3);
}

#[test]
fn plans_crud_and_errors() {
    let s = start();
    let r = req(&s, "GET", "/health", None);
    assert_eq!((r.status, r.body.as_str()), (200, "ok"));
    err(&req(&s, "GET", "/nope", None), 404);
    err(&req(&s, "DELETE", "/plans", None), 405);
    err(&req(&s, "PUT", "/health", None), 405);

    let r = req(&s, "POST", "/plans", Some(br#"{"name": "Household", "extra": 1}"#));
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
    assert_eq!(req(&s, "POST", "/plans", Some(format!(r#"{{"name": "{long}"}}"#).as_bytes())).status, 201);
    err(&req(&s, "POST", "/plans", Some(format!(r#"{{"name": "{long}x"}}"#).as_bytes())), 400);
    // Paired surrogates are fine.
    assert_eq!(req(&s, "POST", "/plans", Some(format!(r#"{{"name": "{}ud83d{}ude00"}}"#, '\\', '\\').as_bytes())).status, 201);

    let list = req(&s, "GET", "/plans", None).json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["id"], id.as_str());

    let path = format!("/plans/{id}");
    let r = req(&s, "PATCH", &path, Some(br#"{"currency_code": "EUR"}"#));
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["currency_code"], "EUR");
    assert_eq!(r.json()["name"], "Household");
    assert_eq!(req(&s, "PATCH", &path, Some(b"{}")).json()["currency_code"], "EUR");
    err(&req(&s, "PATCH", &path, Some(br#"{"name": "New", "currency_code": "x"}"#)), 400);
    assert_eq!(req(&s, "GET", &path, None).json()["name"], "Household");
    err(&req(&s, "POST", &path, None), 405);
    err(&req(&s, "PATCH", "/plans/missing", Some(b"bad")), 404);

    let r = req(&s, "DELETE", &path, None);
    assert_eq!((r.status, r.body.as_str()), (204, ""));
    err(&req(&s, "GET", &path, None), 404);
    err(&req(&s, "DELETE", &path, None), 404);
}
