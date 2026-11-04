// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The server over standard input and output: an MCP client starts
//! `anabolic mcp` (on Windows, `anabolic-cli mcp`) itself and talks to it
//! through pipes, one session for the life of the process. No port and no
//! access key: only the client that started it can reach it.

use std::sync::Arc;

use rmcp::ServiceExt;

use super::handler::AnabolicMcp;
use super::{McpEvent, Shared};

/// Serves until the client closes its end (or `stop` resolves: Ctrl+C in a
/// terminal).
pub fn serve_stdio(
    shared: Arc<Shared>,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("anabolic-mcp-rt")
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start: {e}"))?;
    runtime.block_on(async move {
        let service = AnabolicMcp::new(shared.clone())
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| format!("The client did not start a session: {e}"))?;
        let cancel = service.cancellation_token();
        tokio::spawn(async move {
            stop.await;
            cancel.cancel();
        });
        match service.waiting().await {
            Ok(_) => Ok(()),
            Err(e) => {
                let message = e.to_string();
                shared.event(McpEvent::Failed(message.clone()));
                Err(message)
            }
        }
    })
}
