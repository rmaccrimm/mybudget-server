//! SPEC 1.4: exiting when stdin reaches end of file.
mod common;

use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::*;

fn start_piped(flag: Option<&str>) -> Server {
    start_with(|c| {
        c.stdin(Stdio::piped())
            .env_remove("MYBUDGET_EXIT_ON_STDIN_EOF");
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
        assert!(
            exit_within(&mut s, Duration::from_millis(500)).is_none(),
            "{flag:?}"
        );
        assert_eq!(get(&s, "/health").body, "ok");
    }
}

#[test]
fn keeps_running_with_stdin_null_and_no_flag() {
    let mut s = start_with(|c| {
        c.stdin(Stdio::null())
            .env_remove("MYBUDGET_EXIT_ON_STDIN_EOF");
    });
    assert!(exit_within(&mut s, Duration::from_millis(500)).is_none());
    assert_eq!(get(&s, "/health").body, "ok");
}

#[test]
fn exits_while_still_loading_its_data() {
    let dir = tempfile::tempdir().unwrap();
    new_plan(&start_on(dir.path()));
    // Copies of one plan are all read, and all skipped (SPEC 15.7), so
    // loading them takes a while.
    let plan = dir.path().join("Household.sqlite");
    for i in 0..300 {
        std::fs::copy(&plan, dir.path().join(format!("copy {i}.sqlite"))).unwrap();
    }
    let started = Instant::now();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
        .env("MYBUDGET_ADDR", "127.0.0.1:0")
        .env("MYBUDGET_DATA", dir.path())
        .env("MYBUDGET_EXIT_ON_STDIN_EOF", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

/// Runs the server with the flag set and `stdin` already at end of file, and
/// checks that it exits with status 0 without printing 1.3's line.
fn exits_without_listening(stdin: impl Into<Stdio>) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mybudget-server"))
        .env("MYBUDGET_ADDR", "127.0.0.1:0")
        .env_remove("MYBUDGET_DATA")
        .env("MYBUDGET_EXIT_ON_STDIN_EOF", "1")
        .stdin(stdin)
        .stderr(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

/// End of file there from the start always wins over printing the line, even
/// with every core busy, so the server's threads run late.
#[test]
fn never_listens_when_stdin_starts_at_eof() {
    let stop = Arc::new(AtomicBool::new(false));
    let spinners: Vec<_> = (0..std::thread::available_parallelism().map_or(4, usize::from))
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    for _ in 0..30 {
        exits_without_listening(Stdio::null());
        let (reader, writer) = std::io::pipe().unwrap();
        drop(writer);
        exits_without_listening(reader);
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"ignored\n").unwrap();
        drop(writer);
        exits_without_listening(reader);
    }
    stop.store(true, Ordering::Relaxed);
    for spinner in spinners {
        spinner.join().unwrap();
    }
}

/// Starts the server with the flag set and `stdin`, a non-blocking pipe or
/// socket end, as its standard input. Checks that it outlives a quiet stretch
/// and some input, then exits once `other_end` is dropped.
fn keeps_running_until_eof(stdin: OwnedFd, mut other_end: impl Write) {
    let mut s = start_with(|c| {
        c.stdin(stdin).env("MYBUDGET_EXIT_ON_STDIN_EOF", "1");
    });
    sleep(Duration::from_millis(300));
    assert!(
        s.child.try_wait().unwrap().is_none(),
        "exited with nothing waiting"
    );
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(reader.into())
        .unwrap()
        .into_nonblocking_fd()
        .unwrap();
    keeps_running_until_eof(reader, writer);
}

#[test]
fn keeps_running_on_a_quiet_non_blocking_socket() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    keeps_running_until_eof(theirs.into(), ours);
}
