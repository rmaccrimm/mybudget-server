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
    axum::serve(listener, mybudget_server::app()).await
}
