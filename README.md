# async-proxy

A multi-threaded async reverse proxy built in Rust. Forwards HTTP and tunnels HTTPS, enforces per-IP rate limiting, rewrites headers, and logs every request with structured output.

```
2026-05-02T15:43:50.358Z  INFO → request method="GET" path="/get" status=200 latency="473ms"
2026-05-02T15:43:56.473Z  WARN  rate limited ip=192.168.1.5
2026-05-02T15:43:56.473Z  INFO → request method="GET" path="/get" status=429 latency="0ms"
2026-05-02T15:43:52.221Z  INFO  CONNECT tunnel target=github.com:443 client=127.0.0.1:56012
```

---

## Why I built this

I wanted to understand what actually happens inside a reverse proxy — how async runtimes handle thousands of concurrent connections without threads, how HTTP is modelled as a function from request to response, and how HTTPS passthrough works without decrypting anything. Building it from scratch in Rust forced me to reason about ownership across async task boundaries, shared mutable state without data races, and the difference between a TCP connection and an HTTP request.

---

## Features

- **Async from the ground up** — built on Tokio; each TCP connection gets its own spawned task, no blocking anywhere
- **HTTP forwarding** — proxies plain HTTP requests to any upstream, preserving the full response
- **HTTPS CONNECT tunnel** — handles the `CONNECT` method by opening a raw TCP tunnel, letting the client negotiate TLS directly with the upstream (no decryption)
- **Per-IP rate limiting** — token bucket algorithm, one bucket per client IP stored in a `DashMap`; burst capacity + continuous refill rate, both configurable
- **Header rewriting** — strips hop-by-hop headers (`Proxy-Connection`, `Transfer-Encoding`, etc.) and injects `X-Forwarded-For`, `X-Real-IP`, and `Via`
- **Structured logging** — every request logged with method, path, status code, and latency via `tracing`; rate-limited requests log at WARN level
- **Zero-cost rejection** — rate-limited requests are rejected entirely in memory at `0ms` latency, no upstream I/O

---

## Architecture

```
Client
  │  TCP connection
  ▼
TcpListener::accept().await
  │  tokio::spawn (one task per connection)
  ▼
service_fn(|req| ...)          ← called once per HTTP request
  │
  ├─ Rate limiter               check token bucket for client IP
  │    └─ 429 if empty          rejected in memory, 0ms latency
  │
  ├─ Header rewriter            strip hop-by-hop, inject X-Forwarded-For
  │
  ├─ Request logger             method · path · status · latency
  │
  ├─ HTTP forward  ──────────►  TcpStream::connect(upstream)
  │                             hyper::client::conn::http1
  │
  └─ CONNECT tunnel ─────────►  tokio::io::copy_bidirectional
                                raw bytes, no TLS inspection
```

### Token bucket rate limiter

```
 capacity = 10 tokens
 refill   = 0.5 tokens/sec

  ┌──────────────────────┐
  │ ● ● ● ● ● ● ● ● ● ● │  full bucket → requests allowed
  └──────────────────────┘
        ↓ 10 requests
  ┌──────────────────────┐
  │                      │  empty → 429 Too Many Requests
  └──────────────────────┘
        ↓ wait 20s (refill)
  ┌──────────────────────┐
  │ ● ● ● ● ● ● ● ● ● ● │  full again
  └──────────────────────┘
```

Each IP gets its own independent bucket. `DashMap` gives per-bucket locking — no global mutex contention under concurrent load.

---

## Getting started

**Prerequisites:** Rust 1.75+ ([install](https://rustup.rs))

```bash
git clone https://github.com/YOUR_USERNAME/async-proxy.git
cd async-proxy
cargo build --release
```

---

## Running

```bash
cargo run
# 2026-05-02T15:43:39Z  INFO  async-proxy listening addr=127.0.0.1:8080 burst=10 refill=0.5
```

### Test HTTP forwarding

```bash
curl -x http://127.0.0.1:8080 http://httpbin.org/get
```

### Test header injection

```bash
# X-Forwarded-For is sent upstream — visible in the origin field
curl -x http://127.0.0.1:8080 http://httpbin.org/get
# "origin": "127.0.0.1, <your-public-ip>"
```

### Test HTTPS tunnel

```bash
# curl uses CONNECT automatically for https:// targets
curl -x http://127.0.0.1:8080 https://httpbin.org/get
```

### Test rate limiting

```bash
# First 10 → 200, then 429 until the bucket refills
for i in $(seq 1 15); do
  echo -n "req $i: "
  curl -s -o /dev/null -w "%{http_code}\n" -x http://127.0.0.1:8080 http://httpbin.org/get
done

# Wait for bucket to refill (20s at 0.5 tokens/sec)
sleep 20 && curl -x http://127.0.0.1:8080 http://httpbin.org/get
```

---

## Configuration

All tunable constants are at the top of `src/main.rs`:

| Constant | Default | Description |
|---|---|---|
| `LISTEN_ADDR` | `127.0.0.1:8080` | Address and port to bind |
| `RATE_LIMIT_BURST` | `10` | Max burst requests before throttling |
| `RATE_LIMIT_REFILL` | `0.5` | Tokens refilled per second per IP |

---

## Project structure

```
src/
  main.rs          — Tokio runtime, TCP accept loop, connection dispatch
  proxy.rs         — HTTP forwarding + CONNECT tunnel handler
  middleware.rs    — Header rewriter and request logger
  rate_limiter.rs  — Token bucket, one per IP via DashMap
```

---

## Crates used

| Crate | Purpose |
|---|---|
| [`tokio`](https://docs.rs/tokio) | Async runtime, TCP listener, task spawning, bidirectional copy |
| [`hyper`](https://docs.rs/hyper) | HTTP/1.1 server and client, CONNECT upgrade |
| [`hyper-util`](https://docs.rs/hyper-util) | `TokioIo` adapter between Tokio and Hyper I/O traits |
| [`http-body-util`](https://docs.rs/http-body-util) | Body boxing and composition |
| [`dashmap`](https://docs.rs/dashmap) | Concurrent hash map for per-IP rate limit buckets |
| [`tracing`](https://docs.rs/tracing) | Structured, async-aware logging |
| [`tracing-subscriber`](https://docs.rs/tracing-subscriber) | Log formatting and `RUST_LOG` env filter |
| [`bytes`](https://docs.rs/bytes) | Zero-copy byte buffer for response bodies |
| [`anyhow`](https://docs.rs/anyhow) | Ergonomic error handling in `main` |

---

## License

MIT
