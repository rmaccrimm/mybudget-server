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
fn exit_on_stdin_eof() {
    std::thread::spawn(|| {
        // `copy` returns at end of file, or on a read error, which also means
        // the parent can no longer be heard from.
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        std::process::exit(0);
    });
}
