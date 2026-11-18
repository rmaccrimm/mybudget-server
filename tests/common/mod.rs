//! Starts the real binary and talks raw HTTP/1.1 to it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

pub struct Server {
    pub child: Child,
    pub addr: String,
}

/// Kills the server with `SIGKILL`, and waits for it to go.
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn start() -> Server {
    start_with(|_| {})
}

/// Starts the server with `MYBUDGET_DATA` set to `dir`.
pub fn start_on(dir: &std::path::Path) -> Server {
    start_with(|c| {
        c.env("MYBUDGET_DATA", dir);
    })
}

/// Starts the server after letting `configure` adjust its command, such as
/// its environment or stdin.
pub fn start_with(configure: impl FnOnce(&mut Command)) -> Server {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mybudget-server"));
    command
        .env("MYBUDGET_ADDR", "127.0.0.1:0")
        .stdout(Stdio::piped());
    configure(&mut command);
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("listening on ")
        .unwrap()
        .to_string();
    Server { child, addr }
}

pub struct Resp {
    pub status: u16,
    pub headers: String,
    pub body: String,
}

impl Resp {
    pub fn json(&self) -> Value {
        assert!(
            self.headers
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{}",
            self.headers
        );
        serde_json::from_str(&self.body).unwrap()
    }
}

pub fn req(s: &Server, method: &str, path: &str, body: Option<&[u8]>) -> Resp {
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
    Resp {
        status,
        headers: headers.to_string(),
        body: body.to_string(),
    }
}

pub fn err(r: &Resp, status: u16) {
    assert_eq!(r.status, status, "{}", r.body);
    assert!(!r.json()["error"].as_str().unwrap().is_empty());
}

pub fn get(s: &Server, path: &str) -> Resp {
    req(s, "GET", path, None)
}

pub fn post(s: &Server, path: &str, body: &str) -> Resp {
    req(s, "POST", path, Some(body.as_bytes()))
}

pub fn patch(s: &Server, path: &str, body: &str) -> Resp {
    req(s, "PATCH", path, Some(body.as_bytes()))
}

pub fn delete(s: &Server, path: &str) -> Resp {
    req(s, "DELETE", path, None)
}

/// Expects `status` and returns the JSON body.
pub fn expect(r: Resp, status: u16) -> Value {
    assert_eq!(r.status, status, "{}", r.body);
    r.json()
}

pub fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

pub fn new_plan(s: &Server) -> String {
    id(&expect(post(s, "/plans", r#"{"name": "Household"}"#), 201))
}
