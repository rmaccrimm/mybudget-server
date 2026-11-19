use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};

use mybudget_server::{AppState, Store};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;

/// How much of stdin each read takes, to discard it (SPEC 1.4).
const STDIN_CHUNK: usize = 4096;

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
    // The test harness reads this line to find the port (SPEC 1.3). It goes
    // out in one write, since the watch exiting meanwhile leaves stdout
    // unbuffered, and `println!` would then write it piece by piece.
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let line = format!("listening on {addr}\n");
    std::io::stdout()
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
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
        let mut discard = [0; STDIN_CHUNK];
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
/// server mustn't print 1.3's line (SPEC 1.4). The main thread asks this
/// itself, so the answer doesn't hang on how the watch's thread is scheduled.
///
/// Only reading stdin answers for every kind of stdin: macOS's `poll` reports
/// nothing for a named pipe whose writer has closed, and nothing at all about
/// a device such as `/dev/null`. The reads mustn't wait, though, since a quiet
/// pipe or socket mustn't hold startup up, so stdin is non-blocking while this
/// reads it and is then put back as it was. If the watch reads meanwhile, it
/// gets `WouldBlock` and retries, as on any non-blocking stdin. What the reads
/// take is ignored, like all that stdin brings. They stop at end of file, when
/// nothing more is waiting, or once they have taken more than a pipe holds,
/// since a writer is then still sending, so end of file wasn't already there.
/// If stdin's flags can't be changed or a read fails, the watch decides.
fn stdin_eof_is_waiting() -> bool {
    // A pipe holds 64 KiB on macOS, and at most 1 MiB on Linux by default.
    const MORE_THAN_A_PIPE_HOLDS: usize = 1 << 20;
    let stdin = std::io::stdin();
    let Ok(flags) = fcntl_getfl(&stdin) else {
        return false;
    };
    if fcntl_setfl(&stdin, flags | OFlags::NONBLOCK).is_err() {
        return false;
    }
    let mut taken = 0;
    let eof = loop {
        match rustix::io::read(&stdin, &mut [0; STDIN_CHUNK]) {
            Ok(0) => break true,
            Ok(n) if taken + n <= MORE_THAN_A_PIPE_HOLDS => taken += n,
            Err(Errno::INTR) => {}
            // More than a pipe holds, nothing waiting, or a failed read.
            Ok(_) | Err(_) => break false,
        }
    };
    // Should this fail, stdin stays non-blocking, which the watch copes with.
    let _ = fcntl_setfl(&stdin, flags);
    eof
}
