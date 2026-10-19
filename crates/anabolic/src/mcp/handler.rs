//! The MCP service one client session talks to: its tools (in the tools_*
//! files, one router each), and what the server says about itself.

use std::sync::Arc;
use std::time::Instant;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, Implementation, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, PaginatedRequestParams, ProgressNotificationParam,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ResourceContents,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};
use serde_json::Value;

use super::{McpEvent, Shared};

#[derive(Clone)]
pub struct AnabolicMcp {
    pub(super) shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl AnabolicMcp {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            tool_router: Self::system_tools()
                + Self::process_tools()
                + Self::inspect_tools()
                + Self::network_tools()
                + Self::service_tools()
                + Self::activity_tools()
                + Self::action_tools(),
        }
    }
}

#[cfg(windows)]
const INSTRUCTIONS: &str = "Anabolic is a performance monitoring and diagnostics tool running on this Windows PC. Its tools read what the machine is doing (processes, CPU per core, memory, disks with their partitions and the files being read and written, network connections and DNS lookups, GPUs, temperatures, services, startup programs, frame times and driver latency, a 7-day history), record what programs do step by step, and act on it (end, suspend or reprioritise processes, control services, block programs from the network, turn startup programs off, write memory dumps).

Start with get_system_overview, then list_processes or get_process. Times are local. Rates are per second (_bps = bytes per second).

The user chose on Anabolic's MCP page what you may do; a tool of a kind that is off is refused and says which. Tools marked OPENS THE PROCESS, and actions on a game or an anti-cheat program, are refused the first time with the risks: tell the user in your own words and call again with force=true only if they agree. Say what you are about to change before calling an action, and prefer the least drastic one.";
#[cfg(target_os = "linux")]
const INSTRUCTIONS: &str = "Anabolic is a performance monitoring and diagnostics tool running on this Linux computer. Its tools read what the machine is doing (processes, CPU per core, memory, disks and their partitions, network connections and their traffic, GPUs, temperatures and fans, systemd services, startup programs, interrupt and softirq latency, a 7-day history), record what programs do step by step down to their system calls, and act on it (end, suspend or reprioritise processes, control services, turn startup programs off, write core dumps). Linux reports no frame times system-wide and its firewalls do not block by program, so those tools say they are unavailable.

Start with get_system_overview, then list_processes or get_process. Times are local. Rates are per second (_bps = bytes per second).

The user chose on Anabolic's MCP page what you may do; a tool of a kind that is off is refused and says which. Tools marked OPENS THE PROCESS, and actions on a game or an anti-cheat program, are refused the first time with the risks: tell the user in your own words and call again with force=true only if they agree. Say what you are about to change before calling an action, and prefer the least drastic one.";

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AnabolicMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(
            Implementation::new("anabolic", env!("CARGO_PKG_VERSION"))
                .with_title("Anabolic")
                .with_description(if cfg!(windows) {
                    "Performance monitoring and diagnostics for Windows: processes, performance, \
                     history, network, services and actions on them."
                } else {
                    "Performance monitoring and diagnostics for Linux: processes, performance, \
                     history, network, services and actions on them."
                }),
        )
        .with_instructions(INSTRUCTIONS)
    }

    /// Every call, timed, for the MCP page.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let started = Instant::now();
        let tool = request.name.to_string();
        let client = client_of(&context);
        let call = ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(call).await;
        let (ok, note) = match &result {
            Ok(CallToolResponse::Complete(r)) if r.is_error == Some(true) => (false, text_of(r)),
            Ok(_) => (true, String::new()),
            Err(e) => (false, e.message.to_string()),
        };
        self.shared.event(McpEvent::Call {
            client,
            tool,
            ok,
            took: started.elapsed(),
            note,
        });
        result
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            super::guide::resources(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            super::guide::templates(),
        ))
    }

    /// A resource is its tool's answer, permissions and all.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let Some((tool, arguments)) = super::guide::resolve(&uri) else {
            return Err(ErrorData::resource_not_found(
                format!("Anabolic has no resource at {uri}."),
                None,
            ));
        };
        let call = ToolCallContext::new(
            self,
            CallToolRequestParams::new(tool).with_arguments(arguments),
            context,
        );
        let result = match self.tool_router.call(call).await? {
            CallToolResponse::Complete(result) => result,
            _ => return Err(ErrorData::internal_error("The tool did not answer.", None)),
        };
        if result.is_error == Some(true) {
            return Err(ErrorData::invalid_request(text_of(&result), None));
        }
        let text = result
            .structured_content
            .as_ref()
            .map_or_else(|| text_of(&result), Value::to_string);
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::TextResourceContents {
                uri,
                mime_type: Some("application/json".into()),
                text,
                meta: None,
            }],
        )))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(super::guide::prompts()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        match super::guide::prompt(&request.name, &arguments) {
            Some(result) => Ok(GetPromptResponse::Complete(result)),
            None => Err(ErrorData::invalid_params(
                format!("Anabolic has no prompt named {}.", request.name),
                None,
            )),
        }
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let (name, version) = context.peer.peer_info().map_or_else(
            || ("an MCP client".to_string(), String::new()),
            |info| {
                (
                    info.client_info.name.clone(),
                    info.client_info.version.clone(),
                )
            },
        );
        self.shared.event(McpEvent::Client { name, version });
    }
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The client's name, for approvals and the call log.
pub(super) fn client_of(context: &RequestContext<RoleServer>) -> String {
    context.peer.peer_info().map_or_else(
        || "An MCP client".to_string(),
        |info| info.client_info.name.clone(),
    )
}

pub(super) fn ok(value: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::structured(value))
}

/// A tool error the model can read and act on (not a protocol error).
pub(super) fn fail(message: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(
        message.into(),
    )]))
}

/// Runs blocking Windows calls off the runtime's workers.
pub(super) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Tells the client how a long call is going, when it asked to know.
pub(super) async fn progress(
    context: &RequestContext<RoleServer>,
    done: f64,
    total: Option<f64>,
    message: impl Into<String>,
) {
    let Some(token) = context.meta.get_progress_token() else {
        return;
    };
    let mut param = ProgressNotificationParam::new(token, done).with_message(message.into());
    if let Some(total) = total {
        param = param.with_total(total);
    }
    let _ = context.peer.notify_progress(param).await;
}

impl AnabolicMcp {
    /// The latest snapshot. Right after Anabolic starts the first one has
    /// no rates yet (they need two samples): the next is waited for.
    pub(super) async fn snapshot(&self) -> Arc<anabolic_core::Snapshot> {
        for _ in 0..40 {
            let snap = self.shared.snapshot.load_full();
            if snap.seq >= 2 {
                return snap;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        self.shared.snapshot.load_full()
    }

    /// The latest snapshot holding `pid`: a process started a moment ago
    /// is in the next one, which is waited for (a refresh at most).
    pub(super) async fn snapshot_with(&self, pid: u32) -> Arc<anabolic_core::Snapshot> {
        let snap = self.snapshot().await;
        if super::json::find(&snap, pid).is_some() {
            return snap;
        }
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let next = self.shared.snapshot.load_full();
            if next.seq > snap.seq {
                return next;
            }
        }
        self.shared.snapshot.load_full()
    }

    /// None when clients may do `kind`; otherwise the refusal to return.
    pub(super) fn refuse(
        &self,
        kind: crate::settings::McpKind,
    ) -> Option<Result<CallToolResult, ErrorData>> {
        self.shared.allowed(kind).err().map(fail)
    }

    /// Tools that open a process: refused with the risks in words until the
    /// client calls again with force=true, which it should do only after
    /// its user agreed.
    pub(super) fn confirm(
        &self,
        force: bool,
        risks: &str,
    ) -> Option<Result<CallToolResult, ErrorData>> {
        (!force).then(|| {
            fail(format!(
                "Not done yet: this needs the user's agreement. {risks} Tell the user this in \
                 your own words and ask whether to go ahead. Only if they agree, call this tool \
                 again with the same arguments and force=true."
            ))
        })
    }
}
