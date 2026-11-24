use std::ffi::OsString;
use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use mybudget_server::{AppState, Store};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;

/// How much of stdin each read takes, to discard it (SPEC 1.4).
const STDIN_CHUNK: usize = 4096;

/// How long the watch waits before it reads a non-blocking stdin again once
/// it found nothing there, well inside SPEC 1.4's one second.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

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
/// while the plans are still loading, and has its say before the server
/// prints the line SPEC 1.3 asks for.
fn run() -> Result<(), String> {
    let exit_on_eof = std::env::var_os("MYBUDGET_EXIT_ON_STDIN_EOF").is_some_and(|v| v == "1");
    let watch = exit_on_eof.then(StdinWatch::start);
    let today = mybudget_server::today()?;
    let state = AppState::new(open_store()?, &today);
    let listener = bind().map_err(|e| e.to_string())?;
    if let Some(watch) = watch {
        watch.wait_to_listen(state.clone());
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
/// discarded. A thread of its own reads stdin, from startup on, and is its
/// only reader, so it alone decides, and ends the process outright rather
/// than shutting down gracefully, since an open connection could otherwise
/// keep it alive past the one-second limit.
///
/// End of file already there when the server would print 1.3's line must win
/// over printing it, however the threads are scheduled. So while the server
/// starts, the watch reads stdin non-blocking, which leaves it free to hear
/// when the main thread is about to print: it then reads what's waiting, and
/// either finds end of file and ends the process, or lets the main thread go
/// on. Only reading answers this for every kind of stdin: macOS's `poll`
/// reports nothing for a named pipe whose writer has closed, and nothing at
/// all about a device such as `/dev/null`. Once the server is listening,
/// stdin's flags are as the parent left them, and [`AppState::exit`] lets a
/// save in progress finish. Before then nothing is serving, so the watch
/// exits at once, and a startup upgrade (SPEC 15.8) cut short is rolled back
/// the next time its file is opened, as after a `SIGKILL`.
struct StdinWatch {
    /// Hands the watch the state once the server is about to listen.
    about_to_listen: Sender<AppState>,
    /// Answers once stdin wasn't at end of file then.
    may_listen: Receiver<()>,
}

impl StdinWatch {
    fn start() -> Self {
        let (about_to_listen, listening) = mpsc::channel();
        let (cleared, may_listen) = mpsc::channel();
        std::thread::spawn(move || watch_stdin(&listening, &cleared));
        Self {
            about_to_listen,
            may_listen,
        }
    }

    /// Returns once the server may print 1.3's line; if stdin is already at
    /// end of file, the process ends instead (SPEC 1.4).
    fn wait_to_listen(self, state: AppState) {
        self.about_to_listen
            .send(state)
            .expect("the watch waits for the state");
        self.may_listen
            .recv()
            .expect("the watch answers or ends the process");
    }
}

/// The watch's thread (see [`StdinWatch`]). Should stdin refuse non-blocking
/// mode, the watch can't hear the main thread while a read waits, so it lets
/// it go on at once, and only scheduling then decides whether end of file
/// that's already there wins over printing 1.3's line.
fn watch_stdin(listening: &Receiver<AppState>, cleared: &Sender<()>) {
    let stdin = std::io::stdin();
    let state = match fcntl_getfl(&stdin) {
        Ok(flags) if fcntl_setfl(&stdin, flags | OFlags::NONBLOCK).is_ok() => {
            let state = watch_startup(listening);
            // Should this fail, stdin stays non-blocking, which
            // `discard_until_eof` copes with.
            let _ = fcntl_setfl(&stdin, flags);
            let Some(state) = state else {
                return;
            };
            Some(state)
        }
        _ => None,
    };
    // Fails only if the main thread gave up on starting, and so is ending the
    // process itself.
    let _ = cleared.send(());
    discard_until_eof();
    match state.or_else(|| listening.try_recv().ok()) {
        Some(state) => state.exit(),
        None => std::process::exit(0),
    }
}

/// Reads stdin, which is non-blocking, while the server starts, and ends the
/// process at end of file. Returns the state once the main thread is about to
/// listen and end of file wasn't already there, or `None` if the main thread
/// gave up on starting.
fn watch_startup(listening: &Receiver<AppState>) -> Option<AppState> {
    // A pipe holds 64 KiB on macOS, and at most 1 MiB on Linux by default.
    const MORE_THAN_A_PIPE_HOLDS: usize = 1 << 20;
    let state = loop {
        let pause = match read_stdin() {
            StdinRead::End => std::process::exit(0),
            StdinRead::Data(_) => Duration::ZERO,
            StdinRead::Nothing => RETRY_PAUSE,
        };
        match listening.recv_timeout(pause) {
            Ok(state) => break state,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    };
    // End of file behind what's waiting now is already there too. Taking more
    // than a pipe holds means a writer is still sending, so it isn't.
    let mut taken = 0;
    while taken <= MORE_THAN_A_PIPE_HOLDS {
        match read_stdin() {
            StdinRead::End => std::process::exit(0),
            StdinRead::Data(n) => taken += n,
            StdinRead::Nothing => break,
        }
    }
    Some(state)
}

/// Reads stdin until end of file. A non-blocking stdin with nothing waiting
/// is read again after a pause.
fn discard_until_eof() {
    loop {
        match read_stdin() {
            StdinRead::End => return,
            StdinRead::Data(_) => {}
            StdinRead::Nothing => std::thread::sleep(RETRY_PAUSE),
        }
    }
}

/// What one read of stdin found.
enum StdinRead {
    /// This many bytes, which are ignored (SPEC 1.4).
    Data(usize),
    /// Nothing waiting on a non-blocking stdin, which isn't end of file.
    Nothing,
    End,
}

fn read_stdin() -> StdinRead {
    loop {
        match rustix::io::read(std::io::stdin(), &mut [0; STDIN_CHUNK]) {
            Ok(0) => return StdinRead::End,
            Ok(n) => return StdinRead::Data(n),
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => return StdinRead::Nothing,
            // The parent can no longer be heard from, so this ends the server
            // too (SPEC 1.4 leaves this case open).
            Err(_) => return StdinRead::End,
        }
    }
}
