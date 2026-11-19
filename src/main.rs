use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use mybudget_server::{AppState, Store};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [] => open_store().and_then(|store| serve(store).map_err(|e| e.to_string())),
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

/// The plans to serve: those in the `MYBUDGET_DATA` directory if it's set and
/// not empty, and otherwise none, kept only in memory (SPEC 15.1).
fn open_store() -> Result<Store, String> {
    match std::env::var_os("MYBUDGET_DATA") {
        Some(dir) if !dir.is_empty() => Store::open(Path::new(&dir)),
        _ => Ok(Store::default()),
    }
}

#[tokio::main]
async fn serve(store: Store) -> std::io::Result<()> {
    let addr: SocketAddr = std::env::var("MYBUDGET_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("MYBUDGET_ADDR must be an ip:port address");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // The test harness reads this line to find the port (SPEC 1.3).
    println!("listening on {}", listener.local_addr()?);
    let state = AppState::new(store);
    if std::env::var_os("MYBUDGET_EXIT_ON_STDIN_EOF").is_some_and(|v| v == "1") {
        exit_on_stdin_eof(state.clone());
    }
    axum::serve(listener, mybudget_server::app(state)).await
}

/// SPEC 1.4: the app that starts the server as a child process holds the other
/// end of its stdin pipe, so end of file there means the parent has gone and
/// the server must exit with status 0. Whatever is read before then is
/// discarded. A blocking thread is the simplest way to wait on stdin; the
/// process exits outright ([`AppState::exit`]) rather than shutting down
/// gracefully, since an open connection could otherwise keep it alive past
/// the one-second limit.
///
/// `std::io::copy` won't do here: the parent may hand over a non-blocking pipe
/// or socket, where a read with nothing waiting fails with `WouldBlock`, and
/// that isn't end of file. Such a read is retried after a short pause, well
/// inside the one-second limit; other read errors end the server, since the
/// parent can then no longer be heard from (SPEC 1.4 leaves that case open).
fn exit_on_stdin_eof(state: AppState) {
    const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut discard = [0; 4096];
        loop {
            match stdin.read(&mut discard) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(RETRY_PAUSE),
                Err(_) => break,
            }
        }
        state.exit();
    });
}
