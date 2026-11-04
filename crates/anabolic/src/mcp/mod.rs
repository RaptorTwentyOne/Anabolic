// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The MCP server: what Anabolic sees and can do, for AI assistants and
//! other Model Context Protocol clients, over Streamable HTTP on 127.0.0.1.
//! Off until turned on on the MCP page.
//!
//! It runs on a tokio runtime of its own. Reading needs nothing from the UI:
//! snapshots come from the collector's ArcSwap, and services, history,
//! handles and the rest are read on the server's threads. The UI takes part
//! only in what it owns: keeping a trace on while a tool waits for its data,
//! and the leaks it watches.
//!
//! What a client may do is chosen on the MCP page (McpKind). Tools that open
//! a process refuse the first time with the risks in words; the client asks
//! its user and, with their yes, calls again with force=true.

mod guide;
mod handler;
mod json;
mod server;
mod stdio;
mod tools_actions;
mod tools_activity;
mod tools_inspect;
mod tools_network;
mod tools_process;
mod tools_services;
mod tools_space;
mod tools_system;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anabolic_core::Snapshot;
use anabolic_core::collector::Command;
use anabolic_core::growth::Leak;
use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use eframe::egui;

use crate::settings::{McpKind, McpSettings};

pub use handler::{AGREE_END, AGREE_START, AnabolicMcp};
pub use server::McpServer;
pub use stdio::serve_stdio;

/// Everything the server's threads share; built by the UI.
pub struct Shared {
    pub snapshot: Arc<ArcSwap<Snapshot>>,
    pub collector: Sender<Command>,
    /// The MCP settings as the UI last saw them (permissions).
    pub settings: Arc<ArcSwap<McpSettings>>,
    /// Processes whose handles, memory or threads keep growing.
    pub leaks: Arc<ArcSwap<Vec<Leak>>>,
    pub wants: Arc<Wants>,
    pub events: Sender<McpEvent>,
    /// Wakes the UI for a trace or an event.
    pub ctx: egui::Context,
    /// %LOCALAPPDATA%\Anabolic (history, dumps).
    pub data_dir: Option<PathBuf>,
    pub elevated: bool,
    /// Activity recordings made for MCP clients (server threads only).
    pub recordings: Mutex<tools_activity::Recordings>,
    /// The last disk space scan a client asked for, kept a while.
    pub space: Mutex<Option<tools_space::Kept>>,
    /// Serving without a window (`anabolic mcp`, the terminal commands):
    /// whoever started this process may end it at any moment.
    pub headless: bool,
}

impl Shared {
    pub fn event(&self, event: McpEvent) {
        let _ = self.events.send(event);
        crate::repaint(&self.ctx);
    }

    /// Whether clients may do `kind`; the refusal says where to allow it.
    pub fn allowed(&self, kind: McpKind) -> Result<(), String> {
        if self.settings.load().allows(kind) {
            Ok(())
        } else if self.headless {
            // Started by a client or in a container: no page to point at.
            Err(format!(
                "Not allowed: \"{}\" is not among what this server was started to allow. Tell \
                 the user what you wanted to do; they can start it with --allow {} (or ALLOW={} \
                 in a container).",
                kind.label(),
                kind.flag(),
                kind.flag()
            ))
        } else {
            Err(format!(
                "Not allowed: \"{}\" is off on Anabolic's MCP page. Tell the user what you \
                 wanted to do; they can turn it on there.",
                kind.label()
            ))
        }
    }
}

/// What the server tells the MCP page.
#[derive(Clone, Debug)]
pub enum McpEvent {
    Listening(SocketAddr),
    Failed(String),
    Client {
        name: String,
        version: String,
    },
    Call {
        client: String,
        tool: String,
        ok: bool,
        took: Duration,
        /// The error, when it failed.
        note: String,
    },
    /// A client blocked a program from the network or let it back, for the
    /// Network page's list.
    Blocked {
        path: String,
        blocked: bool,
    },
}

/// Traces a tool asked the UI to keep on, until a moment.
#[derive(Default)]
pub struct Wants {
    cores: AtomicU64,
    disks: AtomicU64,
    network: AtomicU64,
    frames: AtomicU64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trace {
    /// Who runs on each core (context switches).
    Cores,
    /// Who reads and writes each disk, and which files.
    Disks,
    /// Every connection and listening port.
    Network,
    /// Frame times of programs that draw, and driver DPC/ISR times.
    Frames,
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

impl Wants {
    fn slot(&self, trace: Trace) -> &AtomicU64 {
        match trace {
            Trace::Cores => &self.cores,
            Trace::Disks => &self.disks,
            Trace::Network => &self.network,
            Trace::Frames => &self.frames,
        }
    }

    /// Keeps `trace` on for `span` from now (never shortens an earlier ask).
    pub fn want(&self, trace: Trace, span: Duration) {
        let until = (epoch().elapsed() + span).as_millis() as u64;
        self.slot(trace).fetch_max(until, Ordering::Relaxed);
    }

    pub fn wanted(&self, trace: Trace) -> bool {
        self.slot(trace).load(Ordering::Relaxed) > epoch().elapsed().as_millis() as u64
    }
}

/// The command that adds the server to Claude Code.
pub fn claude_command(port: u16, token: &str) -> String {
    format!(
        "claude mcp add --transport http anabolic http://127.0.0.1:{port}/mcp --header \
         \"Authorization: Bearer {token}\""
    )
}

/// The program a client starts to have the server without the window
/// (`<program> mcp`): anabolic-cli.exe beside this one on Windows, this
/// program itself on Linux. None when it is not there (a copy from before
/// 0.6, or one built without it).
pub fn stdio_program() -> Option<PathBuf> {
    let exe = anabolic_core::update::this_exe()?;
    #[cfg(windows)]
    let program = exe.with_file_name("anabolic-cli.exe");
    #[cfg(not(windows))]
    let program = exe;
    program.exists().then_some(program)
}

/// The command that adds the windowless server to Claude Code.
pub fn claude_stdio_command(program: &std::path::Path) -> String {
    format!("claude mcp add anabolic -- \"{}\" mcp", program.display())
}

/// The windowless server for clients configured with JSON.
pub fn json_stdio_config(program: &std::path::Path) -> String {
    // A path as JSON writes it (backslashes doubled).
    let command = serde_json::to_string(&program.display().to_string()).unwrap_or_default();
    format!(
        "{{\n  \"mcpServers\": {{\n    \"anabolic\": {{\n      \"command\": {command},\n      \
         \"args\": [\"mcp\"]\n    }}\n  }}\n}}"
    )
}

/// The same server for clients configured with JSON (Claude Desktop,
/// Cursor, VS Code and others that speak Streamable HTTP).
pub fn json_config(port: u16, token: &str) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"anabolic\": {{\n      \"type\": \"http\",\n      \"url\": \
         \"http://127.0.0.1:{port}/mcp\",\n      \"headers\": {{ \"Authorization\": \"Bearer \
         {token}\" }}\n    }}\n  }}\n}}"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_windowless_configuration_is_json_with_the_path_as_it_is() {
        let path = std::path::Path::new(r"C:\Program Files\Anabolic\anabolic-cli.exe");
        let config: serde_json::Value =
            serde_json::from_str(&super::json_stdio_config(path)).unwrap();
        let server = &config["mcpServers"]["anabolic"];
        assert_eq!(server["command"], path.display().to_string());
        assert_eq!(server["args"], serde_json::json!(["mcp"]));
    }
}
