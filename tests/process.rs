//! SPEC 1.4: exiting when stdin reaches end of file.
mod common;

use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::*;

fn start_piped(flag: Option<&str>) -> Server {
    start_with(|c| {
        c.stdin(Stdio::piped()).env_remove("MYBUDGET_EXIT_ON_STDIN_EOF");
        if let Some(v) = flag {
            c.env("MYBUDGET_EXIT_ON_STDIN_EOF", v);
        }
    })
}

/// Waits up to `limit` for the server to exit.
fn exit_within(s: &mut Server, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(status) = s.child.try_wait().unwrap() {
            return Some(status);
        }
        sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn exits_with_status_0_when_stdin_closes() {
    let mut s = start_piped(Some("1"));
    let mut stdin = s.child.stdin.take().unwrap();
    stdin.write_all(b"anything\nignored\n").unwrap();
    sleep(Duration::from_millis(200));
    assert_eq!(get(&s, "/health").status, 200);
    drop(stdin);
    let status = exit_within(&mut s, Duration::from_secs(1)).expect("still running");
    assert!(status.success(), "{status}");
}

#[test]
fn exits_with_an_open_connection() {
    let mut s = start_piped(Some("1"));
    let _idle = std::net::TcpStream::connect(&s.addr).unwrap();
    drop(s.child.stdin.take());
    let status = exit_within(&mut s, Duration::from_secs(1)).expect("still running");
    assert!(status.success(), "{status}");
}

#[test]
fn keeps_running_without_the_flag() {
    for flag in [None, Some("0"), Some("true"), Some("")] {
        let mut s = start_piped(flag);
        drop(s.child.stdin.take());
        assert!(exit_within(&mut s, Duration::from_millis(500)).is_none(), "{flag:?}");
        assert_eq!(get(&s, "/health").body, "ok");
    }
}

#[test]
fn keeps_running_with_stdin_null_and_no_flag() {
    let mut s = start_with(|c| {
        c.stdin(Stdio::null()).env_remove("MYBUDGET_EXIT_ON_STDIN_EOF");
    });
    assert!(exit_within(&mut s, Duration::from_millis(500)).is_none());
    assert_eq!(get(&s, "/health").body, "ok");
}

/// Starts the server with the flag set and `stdin`, a non-blocking pipe or
/// socket end, as its standard input. Checks that it outlives a quiet stretch
/// and some input, then exits once `other_end` is dropped.
fn keeps_running_until_eof(stdin: OwnedFd, mut other_end: impl Write) {
    let mut s = start_with(|c| {
        c.stdin(stdin).env("MYBUDGET_EXIT_ON_STDIN_EOF", "1");
    });
    sleep(Duration::from_millis(300));
    assert!(s.child.try_wait().unwrap().is_none(), "exited with nothing waiting");
    other_end.write_all(b"ignored\n").unwrap();
    sleep(Duration::from_millis(300));
    assert!(s.child.try_wait().unwrap().is_none(), "exited after input");
    assert_eq!(get(&s, "/health").body, "ok");
    drop(other_end);
    let status = exit_within(&mut s, Duration::from_secs(1)).expect("still running");
    assert!(status.success(), "{status}");
}

#[test]
fn keeps_running_on_a_quiet_non_blocking_pipe() {
    let (reader, writer) = std::io::pipe().unwrap();
    // std can't make a pipe non-blocking; tokio's pipe receiver sets
    // O_NONBLOCK and hands the descriptor back with it still set.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build().unwrap();
    let _guard = runtime.enter();
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(reader.into()).unwrap().into_nonblocking_fd().unwrap();
    keeps_running_until_eof(reader, writer);
}

#[test]
fn keeps_running_on_a_quiet_non_blocking_socket() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    keeps_running_until_eof(theirs.into(), ours);
}
