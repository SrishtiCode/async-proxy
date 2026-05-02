use std::net::SocketAddr;
use std::time::Instant;

use hyper::Request;
use hyper::header::{HeaderName, HeaderValue};
use tracing::info;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",   // non-standard but curl sends it
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Strip hop-by-hop headers and inject X-Forwarded-For + Via.
pub fn rewrite_request<B>(mut req: Request<B>, client_addr: SocketAddr) -> Request<B> {
    let headers = req.headers_mut();

    for name in HOP_BY_HOP {
        headers.remove(*name);
    }

    let client_ip = client_addr.ip().to_string();

    // Build X-Forwarded-For value (append if already present).
    let xff_value = match headers.get("x-forwarded-for") {
        Some(existing) => format!(
            "{}, {}",
            existing.to_str().unwrap_or(""),
            client_ip
        ),
        None => client_ip.clone(),
    };

    // Use typed HeaderName constants so hyper never strips them.
    headers.insert(
        HeaderName::from_static("x-forwarded-for"),
        HeaderValue::from_str(&xff_value).expect("valid header value"),
    );
    headers.insert(
        HeaderName::from_static("x-real-ip"),
        HeaderValue::from_str(&client_ip).expect("valid header value"),
    );
    headers.insert(
        HeaderName::from_static("via"),
        HeaderValue::from_static("1.1 async-proxy"),
    );

    req
}

pub fn log_request(method: &str, path: &str, status: u16, start: Instant) {
    let ms = start.elapsed().as_millis();
    info!(
        method  = method,
        path    = path,
        status  = status,
        latency = format!("{}ms", ms),
        "→ request"
    );
}