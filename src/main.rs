use std::io::{ErrorKind, Read};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr: SocketAddr = std::env::var("MYBUDGET_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("MYBUDGET_ADDR must be an ip:port address");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // The test harness reads this line to find the port (SPEC 1.3).
    println!("listening on {}", listener.local_addr()?);
    if std::env::var_os("MYBUDGET_EXIT_ON_STDIN_EOF").is_some_and(|v| v == "1") {
        exit_on_stdin_eof();
    }
    axum::serve(listener, mybudget_server::app()).await
}

/// SPEC 1.4: the app that starts the server as a child process holds the other
/// end of its stdin pipe, so end of file there means the parent has gone and
/// the server must exit with status 0. Whatever is read before then is
/// discarded. A blocking thread is the simplest way to wait on stdin; the
/// process exits outright rather than shutting down gracefully, since an open
/// connection could otherwise keep it alive past the one-second limit.
///
/// `std::io::copy` won't do here: the parent may hand over a non-blocking pipe
/// or socket, where a read with nothing waiting fails with `WouldBlock`, and
/// that isn't end of file. Such a read is retried after a short pause, well
/// inside the one-second limit; other read errors end the server, since the
/// parent can then no longer be heard from (SPEC 1.4 leaves that case open).
fn exit_on_stdin_eof() {
    const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
    std::thread::spawn(|| {
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
        std::process::exit(0);
    });
}
