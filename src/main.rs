use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, sync_channel};
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
/// is one, and the line SPEC 1.3 asks for waits until the watch has read all
/// that was already waiting.
fn run() -> Result<(), String> {
    let ready = Arc::new(OnceLock::new());
    let watch = std::env::var_os("MYBUDGET_EXIT_ON_STDIN_EOF")
        .is_some_and(|v| v == "1")
        .then(|| StdinWatch::start(Arc::clone(&ready)));
    let store = open_store()?;
    let state = ready.get_or_init(|| AppState::new(store)).clone();
    let listener = bind().map_err(|e| e.to_string())?;
    if let Some(watch) = watch {
        watch.wait_until_caught_up();
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
struct StdinWatch {
    /// Gets a message each time the watch finishes a read that didn't end
    /// the server. It holds at most one, so nothing piles up once no one
    /// waits on it.
    read_done: Receiver<()>,
}

impl StdinWatch {
    fn start(ready: Arc<OnceLock<AppState>>) -> Self {
        const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
        let (read_done, waiter) = sync_channel(1);
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
                // A full channel already says a read finished, and once
                // startup is over no one listens.
                let _ = read_done.try_send(());
            }
            match ready.get() {
                Some(state) => state.exit(),
                None => std::process::exit(0),
            }
        });
        Self { read_done: waiter }
    }

    /// Returns once stdin has nothing waiting, so a read would block. Until
    /// then, what is waiting may be end of file (SPEC 1.4: then the server
    /// exits without printing 1.3's line), so each time `poll` says a read
    /// would return at once, this waits for the watch to finish one more
    /// read. End of file there before the call therefore always ends the
    /// process before this returns, however the threads are scheduled. A
    /// quiet pipe or socket, blocking or not, returns at once.
    fn wait_until_caught_up(self) {
        let stdin = std::io::stdin();
        let now = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            match poll(&mut [PollFd::new(&stdin, PollFlags::IN)], Some(&now)) {
                Ok(0) => return,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // `poll` can't say, and the watch still ends the server
                // within the second.
                Err(_) => return,
                Ok(_) => {}
            }
            self.read_done
                .recv()
                .expect("the watch ends the process before it stops sending");
        }
    }
}
