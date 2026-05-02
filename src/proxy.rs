/// Core proxy handler.
///
/// For plain HTTP:  rewrite headers → forward via hyper Client → return response.
/// For CONNECT:     tunnel raw TCP bytes between client and upstream (HTTPS passthrough).
use std::{net::SocketAddr, sync::Arc, time::Instant};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    header,
    upgrade::Upgraded,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tracing::{error, info, warn};

use crate::middleware::{log_request, rewrite_request};
use crate::rate_limiter::RateLimiter;

pub type BoxResponse = Response<BoxBody<Bytes, hyper::Error>>;

// ── Small response builders ───────────────────────────────────────────────────

fn empty_body() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full_body(text: &'static str) -> BoxBody<Bytes, hyper::Error> {
    Full::new(Bytes::from(text))
        .map_err(|never| match never {})
        .boxed()
}

fn response(status: StatusCode) -> BoxResponse {
    Response::builder()
        .status(status)
        .body(empty_body())
        .unwrap()
}

fn response_with_body(status: StatusCode, body: &'static str) -> BoxResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(full_body(body))
        .unwrap()
}

// ── Main handler ──────────────────────────────────────────────────────────────

pub async fn handle(
    req: Request<Incoming>,
    client_addr: SocketAddr,
    rate_limiter: Arc<RateLimiter>,
) -> Result<BoxResponse, hyper::Error> {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    // ── Rate limiting ─────────────────────────────────────────────────────────
    if !rate_limiter.check(client_addr.ip()) {
        warn!(ip = %client_addr.ip(), "rate limited");
        log_request(method.as_str(), &path, 429, start);
        return Ok(response_with_body(
            StatusCode::TOO_MANY_REQUESTS,
            "429 Too Many Requests — slow down\n",
        ));
    }

    // ── CONNECT — HTTPS tunnel ────────────────────────────────────────────────
    if method == Method::CONNECT {
        return handle_connect(req, client_addr, start).await;
    }

    // ── Plain HTTP forward ────────────────────────────────────────────────────
    let req = rewrite_request(req, client_addr);

    // Determine upstream host:port from the request URI.
    let host = match req.uri().host() {
        Some(h) => h.to_owned(),
        None => {
            log_request(method.as_str(), &path, 400, start);
            return Ok(response_with_body(StatusCode::BAD_REQUEST, "Missing host in URI\n"));
        }
    };
    let port = req.uri().port_u16().unwrap_or(80);
    let addr = format!("{host}:{port}");

    // Open a fresh TCP connection to the upstream.
    let stream = match TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            error!(%addr, err = %e, "upstream connect failed");
            log_request(method.as_str(), &path, 502, start);
            return Ok(response_with_body(StatusCode::BAD_GATEWAY, "502 Bad Gateway\n"));
        }
    };

    // Send the request over HTTP/1.1.
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;

    // Drive the connection in a background task.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            error!(err = %e, "upstream connection error");
        }
    });

    let upstream_resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => {
            error!(%addr, err = %e, "upstream request failed");
            log_request(method.as_str(), &path, 502, start);
            return Ok(response_with_body(StatusCode::BAD_GATEWAY, "502 Bad Gateway\n"));
        }
    };

    let status = upstream_resp.status().as_u16();
    log_request(method.as_str(), &path, status, start);

    // Box the response body so it matches our return type.
    let (parts, body) = upstream_resp.into_parts();
    let boxed = body.map_err(|e| e).boxed();
    Ok(Response::from_parts(parts, boxed))
}

// ── CONNECT tunnel ────────────────────────────────────────────────────────────

async fn handle_connect(
    req: Request<Incoming>,
    client_addr: SocketAddr,
    start: Instant,
) -> Result<BoxResponse, hyper::Error> {
    let authority = req.uri().authority().map(|a| a.to_string());
    let addr = match authority {
        Some(a) => a,
        None => {
            log_request("CONNECT", req.uri().path(), 400, start);
            return Ok(response_with_body(StatusCode::BAD_REQUEST, "Bad CONNECT target\n"));
        }
    };

    info!(target = %addr, client = %client_addr, "CONNECT tunnel");

    // Connect to upstream before we 200 the client — fail fast.
    let upstream = match TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            error!(%addr, err = %e, "CONNECT upstream failed");
            log_request("CONNECT", &addr, 502, start);
            return Ok(response_with_body(StatusCode::BAD_GATEWAY, "502 Bad Gateway\n"));
        }
    };

    // Tell the client the tunnel is ready, then upgrade the connection.
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                if let Err(e) = tunnel(upgraded, upstream).await {
                    error!(err = %e, "tunnel error");
                }
            }
            Err(e) => error!(err = %e, "upgrade error"),
        }
    });

    log_request("CONNECT", &addr, 200, start);
    Ok(response(StatusCode::OK))
}

/// Bidirectional byte copy between upgraded client connection and upstream TCP.
async fn tunnel(upgraded: Upgraded, mut upstream: TcpStream) -> std::io::Result<()> {
    let mut client_io = TokioIo::new(upgraded);
    tokio::io::copy_bidirectional(&mut client_io, &mut upstream).await?;
    Ok(())
}