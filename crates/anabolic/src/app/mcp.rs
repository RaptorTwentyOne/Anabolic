//! The MCP server's life in the app: started and stopped with its setting,
//! fed what only the UI holds (the settings, the leaks it watches, the
//! traces it owns), and what it reports kept for the MCP page. Also where
//! every kernel trace is switched, since a tool may want one with the
//! window closed.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::SystemTime;

use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;

use super::*;
use crate::mcp::{McpEvent, McpServer, Shared, Trace, Wants};
use crate::settings::McpSettings;

/// How many calls the MCP page keeps.
const CALLS_KEPT: usize = 200;

pub enum McpStatus {
    Off,
    Listening(SocketAddr),
    Failed(String),
}

pub struct McpClient {
    pub name: String,
    pub version: String,
    pub since: SystemTime,
}

pub struct McpCall {
    pub at: SystemTime,
    pub client: String,
    pub tool: String,
    pub ok: bool,
    pub took: Duration,
    pub note: String,
}

pub struct McpState {
    pub shared: Arc<Shared>,
    events: Receiver<McpEvent>,
    server: Option<McpServer>,
    /// The port the running server, or the failed start, was for: a failed
    /// start is not tried again until it changes, or "Try again".
    started_for: Option<u16>,
    pub status: McpStatus,
    /// Clients that connected this session, the latest first.
    pub clients: Vec<McpClient>,
    /// Tool calls, the latest first.
    pub calls: VecDeque<McpCall>,
    /// This instance may serve: the first one, or one started with
    /// --mcp-port (a second instance would find the port taken).
    pub serves: bool,
    /// The access key is shown on the MCP page.
    pub show_key: bool,
}

impl McpState {
    pub fn new(
        collector: &Collector,
        ctx: &egui::Context,
        settings: &McpSettings,
        elevated: bool,
        serves: bool,
    ) -> Self {
        let (events_tx, events) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            snapshot: collector.shared(),
            collector: collector.commands(),
            settings: Arc::new(ArcSwap::from_pointee(settings.clone())),
            leaks: Arc::new(ArcSwap::from_pointee(Vec::new())),
            wants: Arc::new(Wants::default()),
            events: events_tx,
            ctx: ctx.clone(),
            data_dir: crate::data_dir(),
            elevated,
            recordings: Mutex::new(Default::default()),
        });
        Self {
            shared,
            events,
            server: None,
            started_for: None,
            status: McpStatus::Off,
            clients: Vec::new(),
            calls: VecDeque::new(),
            serves,
            show_key: false,
        }
    }

    /// Lets the next pass try a start that failed.
    pub fn retry(&mut self) {
        self.started_for = None;
    }

    /// A tool is waiting for this trace's data.
    fn wants(&self, trace: Trace) -> bool {
        self.server.is_some() && self.shared.wants.wanted(trace)
    }
}

impl App {
    /// Starts, stops or moves the server as its setting says, hands it what
    /// it reads from the UI, and takes in what it reports.
    pub(super) fn mcp_logic(&mut self) {
        let settings = &self.settings.mcp;
        let mcp = &mut self.mcp;
        let mut blocked = Vec::new();
        if **mcp.shared.settings.load() != *settings {
            mcp.shared.settings.store(Arc::new(settings.clone()));
        }
        let want = mcp.serves && settings.enabled && !settings.token.is_empty();
        if !want {
            mcp.server = None;
            mcp.started_for = None;
            mcp.status = McpStatus::Off;
        } else if mcp.started_for != Some(settings.port) {
            // The old one goes first (another port, so no wait for it).
            mcp.server = None;
            mcp.started_for = Some(settings.port);
            match McpServer::start(mcp.shared.clone(), settings.port) {
                Ok(server) => {
                    mcp.status = McpStatus::Listening(server.addr);
                    mcp.server = Some(server);
                }
                Err(e) => mcp.status = McpStatus::Failed(e),
            }
        }
        if mcp.server.is_some() {
            mcp.shared
                .leaks
                .store(Arc::new(self.growth.leaks().to_vec()));
        }
        for event in mcp.events.try_iter() {
            match event {
                McpEvent::Listening(addr) => mcp.status = McpStatus::Listening(addr),
                McpEvent::Failed(e) => {
                    mcp.server = None;
                    mcp.status = McpStatus::Failed(e);
                }
                McpEvent::Client { name, version } => {
                    mcp.clients
                        .retain(|c| c.name != name || c.version != version);
                    mcp.clients.insert(
                        0,
                        McpClient {
                            name,
                            version,
                            since: SystemTime::now(),
                        },
                    );
                }
                McpEvent::Call {
                    client,
                    tool,
                    ok,
                    took,
                    note,
                } => {
                    mcp.calls.push_front(McpCall {
                        at: SystemTime::now(),
                        client,
                        tool,
                        ok,
                        took,
                        note,
                    });
                    mcp.calls.truncate(CALLS_KEPT);
                }
                McpEvent::Blocked { path, blocked: on } => blocked.push((path, on)),
            }
        }
        for (path, on) in blocked {
            self.settings
                .blocked
                .retain(|b| !b.eq_ignore_ascii_case(&path));
            if on {
                self.settings.blocked.push(path);
            }
        }
    }

    /// Which kernel traces run: those the page on screen needs, and those
    /// an MCP tool is waiting on, with the window closed too. Each costs
    /// some CPU, so nothing else keeps one on.
    pub(super) fn trace_logic(&mut self) {
        let shown = |app: &App, tab: Tab| !app.hidden && app.settings.tab == tab;
        let want = shown(self, Tab::Network) || self.mcp.wants(Trace::Network);
        if want != self.network_watched {
            self.network_watched = want;
            self.collector.send(Command::NetActivity(want));
        }
        let want = (shown(self, Tab::Performance) && self.perf_view == PerfView::Cpu)
            || self.mcp.wants(Trace::Cores);
        if want != self.cores_traced {
            self.cores_traced = want;
            self.collector.send(Command::TraceCores(want));
        }
        let want = (shown(self, Tab::Performance)
            && matches!(self.perf_view, PerfView::Disk | PerfView::PhysicalDisk(_)))
            || self.mcp.wants(Trace::Disks);
        if want != self.disks_traced {
            self.disks_traced = want;
            self.collector.send(Command::TraceDisks(want));
        }
        // Frames and DPCs: also always when asked to keep watching (a game
        // played with the window closed).
        let want = self.settings.watch_stutters
            || shown(self, Tab::Stutters)
            || self.mcp.wants(Trace::Frames);
        if want != self.stutters_traced {
            self.stutters_traced = want;
            self.collector.send(Command::TraceFrames(want));
            self.collector.send(Command::TraceLatency(want));
        }
    }
}
