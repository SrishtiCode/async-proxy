mod middleware;
mod proxy;
mod rate_limiter;

use std::{net::SocketAddr, sync::Arc};

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt};

use proxy::handle;
use rate_limiter::RateLimiter;

const LISTEN_ADDR: &str = "127.0.0.1:8080";

/// Burst capacity: how many requests an IP can fire before being throttled.
/// Set low (20) so it's easy to hit during testing.
/// In production you'd set this to something like 200.
const RATE_LIMIT_BURST: u32 = 10;

/// How fast the bucket refills: 5 tokens/sec = 1 request per 200ms sustained.
const RATE_LIMIT_REFILL: f64 = 0.5;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let addr: SocketAddr = LISTEN_ADDR.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!(
        addr    = %addr,
        burst   = RATE_LIMIT_BURST,
        refill  = RATE_LIMIT_REFILL,
        "async-proxy listening"
    );

    // Arc lets every spawned task share the same limiter without copying.
    let rate_limiter = Arc::new(RateLimiter::new(RATE_LIMIT_BURST, RATE_LIMIT_REFILL));

    loop {
        let (stream, client_addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => { error!(err = %e, "accept failed"); continue; }
        };

        let rl = Arc::clone(&rate_limiter);

        // Each connection gets its own task — fully independent, no blocking.
        tokio::spawn(async move {
            let io  = TokioIo::new(stream);
            let svc = service_fn(move |req| {
                let rl = Arc::clone(&rl);
                async move { handle(req, client_addr, rl).await }
            });

            if let Err(e) = http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(false)
                .serve_connection(io, svc)
                .with_upgrades()   // required for CONNECT / HTTPS tunnel
                .await
            {
                if !e.is_incomplete_message() {
                    error!(client = %client_addr, err = %e, "connection error");
                }
            }
        });
    }
}