// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The HTTP side: a listener on 127.0.0.1, every request checked for the
//! access key and for a browser origin before rmcp's Streamable HTTP service
//! sees it, on a small tokio runtime of its own thread.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use super::handler::AnabolicMcp;
use super::{McpEvent, Shared};

/// A running server; dropping it stops it.
pub struct McpServer {
    cancel: CancellationToken,
    pub addr: SocketAddr,
}

struct Guard {
    /// The access key is read from here on every request, so a new one
    /// takes effect at once.
    shared: Arc<Shared>,
    /// The origins a page served by this server would have; any other
    /// origin is a web page somewhere trying to reach a local service.
    origins: [String; 2],
}

impl McpServer {
    /// Listens on 127.0.0.1:`port` at once (so a port in use is an error
    /// here) and serves on a thread of its own.
    pub fn start(shared: Arc<Shared>, port: u16) -> Result<McpServer, String> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .map_err(|e| format!("Could not listen on 127.0.0.1:{port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let guard = Arc::new(Guard {
            shared: shared.clone(),
            origins: [
                format!("http://127.0.0.1:{}", addr.port()),
                format!("http://localhost:{}", addr.port()),
            ],
        });
        std::thread::Builder::new()
            .name("anabolic-mcp".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("anabolic-mcp-rt")
                    .enable_all()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        shared.event(McpEvent::Failed(format!("Could not start: {e}")));
                        return;
                    }
                };
                runtime.block_on(serve(shared, listener, guard, stop));
            })
            .map_err(|e| e.to_string())?;
        Ok(McpServer { cancel, addr })
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn serve(
    shared: Arc<Shared>,
    listener: std::net::TcpListener,
    guard: Arc<Guard>,
    stop: CancellationToken,
) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(e) => {
            shared.event(McpEvent::Failed(e.to_string()));
            return;
        }
    };
    if let Ok(addr) = listener.local_addr() {
        shared.event(McpEvent::Listening(addr));
    }
    let for_sessions = shared.clone();
    let service: StreamableHttpService<AnabolicMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(AnabolicMcp::new(for_sessions.clone())),
            Default::default(),
            StreamableHttpServerConfig::default().with_cancellation_token(stop.child_token()),
        );
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(guard, check));
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await;
    if let Err(e) = result {
        shared.event(McpEvent::Failed(e.to_string()));
    }
}

/// Lets a request through only with the access key and without a foreign
/// browser origin.
async fn check(State(guard): State<Arc<Guard>>, request: Request, next: Next) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let ok = origin.to_str().is_ok_and(|o| {
            guard
                .origins
                .iter()
                .any(|allowed| o.eq_ignore_ascii_case(allowed))
        });
        if !ok {
            return (
                StatusCode::FORBIDDEN,
                "This origin may not use Anabolic's MCP server.",
            )
                .into_response();
        }
    }
    let key = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let settings = guard.shared.settings.load();
    // An empty key would match an empty header: no key, no entry.
    if settings.token.is_empty()
        || !key.is_some_and(|key| same(key.as_bytes(), settings.token.as_bytes()))
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "Anabolic's MCP server needs its access key: Authorization: Bearer <key> (the MCP page \
             shows it).",
        )
            .into_response();
    }
    next.run(request).await
}

/// Compares in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_compare_whole() {
        assert!(super::same(b"abc", b"abc"));
        assert!(!super::same(b"abc", b"abd"));
        assert!(!super::same(b"abc", b"ab"));
        assert!(!super::same(b"", b"a"));
    }
}
