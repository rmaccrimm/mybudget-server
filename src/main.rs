use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::AsFd;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};

use mybudget_server::{AppState, Store};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [] => run(),
        [command, file, version] if command == "migrate" => {
            mybudget_server::migrate(Path::new(file), &version.to_string_lossy())
        }
        _ => Err("usage: mybudget-server [migrate <file> <version>]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Serves the plans. SPEC 1.4's watch on stdin starts first, since it holds
/// while the plans are still loading; `ready` hands it the state once there
/// is one. End of file already waiting when the server would print the line
/// SPEC 1.3 asks for ends it there instead, whether or not the watch has
/// read it yet (SPEC 1.4).
fn run() -> Result<(), String> {
    let ready = Arc::new(OnceLock::new());
    let exit_on_eof = std::env::var_os("MYBUDGET_EXIT_ON_STDIN_EOF").is_some_and(|v| v == "1");
    if exit_on_eof {
        exit_on_stdin_eof(Arc::clone(&ready));
    }
    let store = open_store()?;
    let state = ready.get_or_init(|| AppState::new(store)).clone();
    let listener = bind().map_err(|e| e.to_string())?;
    if exit_on_eof && stdin_eof_is_waiting() {
        state.exit();
    }
    // The test harness reads this line to find the port (SPEC 1.3).
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    println!("listening on {addr}");
    serve(listener, state).map_err(|e| e.to_string())
}

/// The plans to serve: those in the `MYBUDGET_DATA` directory if it's set and
/// not empty, and otherwise none, kept only in memory (SPEC 15.1).
fn open_store() -> Result<Store, String> {
    match std::env::var_os("MYBUDGET_DATA") {
        Some(dir) if !dir.is_empty() => Store::open(Path::new(&dir)),
        _ => Ok(Store::default()),
    }
}

fn bind() -> std::io::Result<TcpListener> {
    let addr: SocketAddr = std::env::var("MYBUDGET_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("MYBUDGET_ADDR must be an ip:port address");
    let listener = TcpListener::bind(addr)?;
    // Tokio needs this to take the listener over.
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[tokio::main]
async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    axum::serve(listener, mybudget_server::app(state)).await
}

/// SPEC 1.4: the app that starts the server as a child process holds the other
/// end of its stdin pipe, so end of file there means the parent has gone and
/// the server must exit with status 0. Whatever is read before then is
/// discarded. A blocking thread is the simplest way to wait on stdin; the
/// process exits outright rather than shutting down gracefully, since an open
/// connection could otherwise keep it alive past the one-second limit. Once
/// the state is `ready`, [`AppState::exit`] lets a save in progress finish;
/// before then nothing is serving, so the server exits at once, and a startup
/// upgrade (SPEC 15.8) cut short is rolled back the next time its file is
/// opened, as after a `SIGKILL`.
///
/// `std::io::copy` won't do here: the parent may hand over a non-blocking pipe
/// or socket, where a read with nothing waiting fails with `WouldBlock`, and
/// that isn't end of file. Such a read is retried after a short pause, well
/// inside the one-second limit; other read errors end the server, since the
/// parent can then no longer be heard from (SPEC 1.4 leaves that case open).
fn exit_on_stdin_eof(ready: Arc<OnceLock<AppState>>) {
    const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut discard = [0; 4096];
        loop {
            match stdin.read(&mut discard) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(RETRY_PAUSE);
                }
                Err(_) => break,
            }
        }
        match ready.get() {
            Some(state) => state.exit(),
            None => std::process::exit(0),
        }
    });
}

/// Whether stdin's end of file is already there, read yet or not, so that the
/// server mustn't print 1.3's line (SPEC 1.4). It never waits, so the answer
/// doesn't hang on how the watch's thread is scheduled, and a quiet or busy
/// stdin doesn't hold startup up.
///
/// `poll` answers for a pipe or socket: `POLLHUP` says its other end is
/// closed, so end of file is next once what's waiting is read; without it,
/// whatever is or isn't waiting, more may come. It can't answer for a regular
/// file, which it always calls readable, or for a device it can't watch, such
/// as `/dev/null` (`POLLNVAL`). A read there returns at once, so one read
/// answers; what it takes is ignored, like all that stdin brings. If `poll`
/// or that read fails, the watch decides.
fn stdin_eof_is_waiting() -> bool {
    // A handle of our own, since the watch keeps `Stdin`'s lock.
    let Ok(stdin) = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
    else {
        return false;
    };
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let revents = loop {
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        match poll(&mut fds, Some(&now)) {
            Ok(_) => break fds[0].revents(),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    };
    if revents.contains(PollFlags::HUP) {
        return true;
    }
    let is_file = stdin.metadata().is_ok_and(|m| m.is_file());
    if !is_file && !revents.contains(PollFlags::NVAL) {
        return false;
    }
    loop {
        match (&stdin).read(&mut [0; 4096]) {
            Ok(n) => return n == 0,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}
