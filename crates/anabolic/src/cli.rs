// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The terminal commands: `anabolic ps`, `anabolic kill 1234`, `anabolic
//! mcp` and the rest (on Windows through anabolic-cli.exe, see build.rs).
//!
//! Each command reads or acts through one of the MCP server's tools, called
//! in this process as a client would call it: a command and its tool answer
//! alike and refuse alike (Windows' critical processes, Anabolic itself, a
//! game or an anti-cheat program without --force). `--json` prints the
//! tool's answer as it is, for scripts.

mod render;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Map, Value, json};

use crate::headless::{self, Headless};
use crate::mcp::{AGREE_END, AGREE_START, AnabolicMcp};
use crate::settings::{McpKind, McpSettings};
use render::View;

/// The program's name as typed in a terminal.
#[cfg(windows)]
const PROGRAM: &str = "anabolic-cli";
#[cfg(not(windows))]
const PROGRAM: &str = "anabolic";

/// Whether `word` (the first argument) names a terminal command rather than
/// starting the window.
pub fn is_command(word: &str) -> bool {
    COMMANDS.iter().any(|c| c.name == word)
        || matches!(word, "help" | "--help" | "-h" | "version" | "--version")
}

/// Runs the command `args` names (the program's own name left out); the
/// exit code: 0 done, 1 refused or failed, 2 not understood.
pub fn main(args: Vec<String>) -> u8 {
    let Some((name, rest)) = args.split_first() else {
        print!("{}", help());
        return 0;
    };
    match name.as_str() {
        "help" | "--help" | "-h" => {
            match rest
                .first()
                .and_then(|n| COMMANDS.iter().find(|c| c.name == n))
            {
                Some(command) => print!("{}", command.help()),
                None => print!("{}", help()),
            }
            return 0;
        }
        "version" | "--version" => {
            println!("Anabolic {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        _ => {}
    }
    let Some(command) = COMMANDS.iter().find(|c| c.name == name) else {
        eprintln!("{PROGRAM}: there is no command \"{name}\". `{PROGRAM} help` lists them.");
        return 2;
    };
    let args = match Args::parse(rest) {
        Ok(args) => args,
        Err(e) => return usage(command, &e),
    };
    if args.flag("--help") {
        print!("{}", command.help());
        return 0;
    }
    let plan = match (command.plan)(&args) {
        Ok(plan) => plan,
        Err(e) => return usage(command, &e),
    };
    match plan {
        Plan::Serve => serve(&args),
        Plan::Tools => list_tools(&args),
        Plan::Call {
            tool,
            arguments,
            view,
        } => call(&tool, arguments, &view, &args),
    }
}

fn usage(command: &Command, problem: &str) -> u8 {
    eprintln!("{PROGRAM} {}: {problem}\n", command.name);
    eprint!("{}", command.help());
    2
}

/// What a command comes to.
enum Plan {
    /// Call `tool` with `arguments` and show its answer as `view`.
    Call {
        tool: String,
        arguments: Map<String, Value>,
        view: View,
    },
    /// List the tools.
    Tools,
    /// Serve MCP until stopped.
    Serve,
}

struct Command {
    name: &'static str,
    /// The arguments, after the name.
    usage: &'static str,
    about: &'static str,
    /// Options and remarks under the usage line.
    details: &'static str,
    group: Group,
    plan: fn(&Args) -> Result<Plan, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Look,
    Act,
    Other,
}

impl Command {
    fn help(&self) -> String {
        let mut text = format!(
            "{}\n\nUsage: {PROGRAM} {} {}\n",
            self.about, self.name, self.usage
        );
        if !self.details.is_empty() {
            text.push('\n');
            text.push_str(self.details);
            text.push('\n');
        }
        text.push_str("\n  --json    print the answer as JSON, as the tool gives it\n");
        text
    }
}

fn help() -> String {
    let mut text = format!(
        "Anabolic {}: what the machine is doing, from a terminal.\n\nUsage: {PROGRAM} <command> \
         [arguments] [--json]\n",
        env!("CARGO_PKG_VERSION")
    );
    for (group, title) in [
        (Group::Look, "Look"),
        (Group::Act, "Act"),
        (Group::Other, "MCP and the rest"),
    ] {
        text.push_str(&format!("\n{title}:\n"));
        for c in COMMANDS.iter().filter(|c| c.group == group) {
            let line = format!("{} {}", c.name, c.usage);
            text.push_str(&format!("  {line:<34} {}\n", c.about));
        }
    }
    text.push_str(&format!(
        "\n`{PROGRAM} help <command>` says more about one. Commands run with the rights of the \
         terminal: from one\nthat is not elevated, other users' processes show less and acting on \
         them is refused.\n"
    ));
    #[cfg(windows)]
    text.push_str("The window program is anabolic.exe, beside this one.\n");
    text
}

const COMMANDS: &[Command] = &[
    Command {
        name: "overview",
        usage: "[--top N]",
        about: "The machine at a glance",
        details: "  --top N   how many of the busiest processes per resource (default 5)",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.number("--top", "top", &mut args)?;
            Ok(tool("get_system_overview", args, View::Auto))
        },
    },
    Command {
        name: "ps",
        usage: "[FILTER] [--sort KEY] [--limit N] [--all]",
        about: "Processes, the busiest first",
        details: "  FILTER       part of the name, path, command line or user, or a PID\n  \
                  --sort KEY   cpu (the default), memory, private, disk, network, gpu, handles, \
                  threads, name or pid\n  --limit N    how many (default 40)\n  --all        \
                  also those that exited a moment ago",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.word(0, "filter", &mut args);
            a.text("--sort", "sort", &mut args);
            a.number("--limit", "limit", &mut args)?;
            if a.flag("--all") {
                args.insert("include_exited".into(), json!(true));
            }
            Ok(tool(
                "list_processes",
                args,
                View::Table {
                    list: "processes",
                    columns: &[
                        "pid",
                        "name",
                        "cpu_percent",
                        "private_bytes=Memory",
                        "disk_read_bps=Disk read",
                        "disk_write_bps=Disk write",
                        "net_receive_bps=Received",
                        "net_send_bps=Sent",
                        "gpu_percent=GPU %",
                        "user",
                    ],
                },
            ))
        },
    },
    Command {
        name: "tree",
        usage: "[PID] [--depth N]",
        about: "Who started what",
        details: "  PID        the process at the top (default: the whole machine)\n  --depth N  \
                  levels below it (default 6)",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.pid_word(0, "pid", &mut args)?;
            a.number("--depth", "depth", &mut args)?;
            Ok(tool(
                "get_process_tree",
                args,
                View::Tree {
                    list: "tree",
                    columns: &["pid", "name", "cpu_percent", "private_bytes"],
                },
            ))
        },
    },
    Command {
        name: "show",
        usage: "PID",
        about: "One process in full",
        details: "",
        group: Group::Look,
        plan: |a| Ok(tool("get_process", a.pid_only()?, View::Auto)),
    },
    Command {
        name: "why",
        usage: "PID",
        about: "Why a process will not end",
        details: "",
        group: Group::Look,
        plan: |a| Ok(tool("why_process_will_not_end", a.pid_only()?, View::Auto)),
    },
    Command {
        name: "space",
        usage: "[PATH] [--depth N] [--top N] [--files | --types] [--rescan]",
        about: "What fills a drive or folder",
        details: "  PATH       the drive or folder (default: the system drive on Windows, your home \
                  folder on Linux)\n  --depth N  folder levels to show (1 to 4, default 2)\n  \
                  --top N    the largest entries of each folder (default 8)\n  --files    the \
                  largest files instead\n  --types    the file types taking the most room \
                  instead\n  --rescan   read it again (a scan is kept for ten minutes)\n\nAn NTFS \
                  drive is read from its master file table in seconds from an elevated terminal; \
                  otherwise folder by folder.",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.word(0, "path", &mut args);
            a.number("--depth", "depth", &mut args)?;
            a.number("--top", "top", &mut args)?;
            if a.flag("--rescan") {
                args.insert("rescan".into(), json!(true));
            }
            let view = if a.flag("--files") {
                args.insert("largest".into(), json!(40));
                View::Table {
                    list: "largest_files",
                    columns: &["size_bytes=Size", "modified", "name", "path"],
                }
            } else if a.flag("--types") {
                args.insert("types".into(), json!(30));
                View::Table {
                    list: "types",
                    columns: &[
                        "extension",
                        "size_bytes=Size",
                        "share_percent=Share %",
                        "files",
                    ],
                }
            } else {
                View::Tree {
                    list: "tree",
                    columns: &["size_bytes", "share_percent", "name"],
                }
            };
            Ok(tool("get_disk_usage", args, view))
        },
    },
    Command {
        name: "services",
        usage: "[FILTER] [--state STATE] [--limit N]",
        about: "Services",
        details: "  FILTER         part of the name, display name or description\n  --state \
                  STATE  running, stopped or failed\n  --limit N      how many (default 50)",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.word(0, "filter", &mut args);
            a.text("--state", "state", &mut args);
            a.number("--limit", "limit", &mut args)?;
            Ok(tool(
                "list_services",
                args,
                View::Table {
                    list: "services",
                    columns: &["name", "state", "startup", "pid", "display_name"],
                },
            ))
        },
    },
    Command {
        name: "service",
        usage: "NAME | ACTION NAME | startup NAME TYPE",
        about: "One service, or start, stop... it",
        details: "  NAME                   one service in full\n  ACTION NAME            start, \
                  stop, restart, pause or continue it\n      --dependents       stop (and start \
                  again) the services that depend on it too\n  startup NAME TYPE      start it \
                  automatic, delayed, manual or disabled",
        group: Group::Act,
        plan: |a| {
            let words = &a.words;
            match words.as_slice() {
                [name] => Ok(tool(
                    "get_service",
                    obj(json!({ "name": name })),
                    View::Auto,
                )),
                [action, name]
                    if matches!(
                        action.as_str(),
                        "start" | "stop" | "restart" | "pause" | "continue"
                    ) =>
                {
                    Ok(tool(
                        "control_service",
                        obj(json!({
                            "name": name,
                            "action": action,
                            "with_dependents": a.flag("--dependents"),
                        })),
                        View::Auto,
                    ))
                }
                [startup, name, kind] if startup == "startup" => {
                    let startup = match kind.as_str() {
                        "automatic" | "auto" => "automatic",
                        "delayed" | "automatic_delayed" => "automatic_delayed",
                        "manual" => "manual",
                        "disabled" => "disabled",
                        other => {
                            return Err(format!(
                                "\"{other}\" is not a start type: automatic, delayed, manual or \
                                 disabled"
                            ));
                        }
                    };
                    Ok(tool(
                        "set_service_startup",
                        obj(json!({ "name": name, "startup": startup })),
                        View::Auto,
                    ))
                }
                [] => Err("which service?".into()),
                _ => Err("one service name, or an action and a name".into()),
            }
        },
    },
    Command {
        name: "ports",
        usage: "",
        about: "Ports open to others, and who listens",
        details: "",
        group: Group::Look,
        plan: |_| {
            Ok(tool(
                "list_listening_ports",
                Map::new(),
                View::Table {
                    list: "ports",
                    columns: &[
                        "port",
                        "protocol",
                        "address",
                        "reachable_from=Reachable from",
                        "pid",
                        "process",
                    ],
                },
            ))
        },
    },
    Command {
        name: "connections",
        usage: "[FILTER] [--pid PID] [--limit N]",
        about: "Network connections",
        details: "  FILTER     part of a program's name or an address\n  --pid PID  only this \
                  process'",
        group: Group::Look,
        plan: |a| {
            let mut args = Map::new();
            a.word(0, "filter", &mut args);
            a.number("--pid", "pid", &mut args)?;
            a.number("--limit", "limit", &mut args)?;
            Ok(tool(
                "list_connections",
                args,
                View::Table {
                    list: "connections",
                    columns: &[
                        "pid",
                        "process",
                        "protocol",
                        "local_port=Local port",
                        "remote",
                        "remote_port=Port",
                        "state",
                        "remote_name=Name",
                    ],
                },
            ))
        },
    },
    Command {
        name: "startup",
        usage: "",
        about: "Programs that start with the system or at sign-in",
        details: "",
        group: Group::Look,
        plan: |_| {
            Ok(tool(
                "list_startup_items",
                Map::new(),
                View::Table {
                    list: "items",
                    columns: &["name", "enabled=On", "where", "publisher", "command"],
                },
            ))
        },
    },
    Command {
        name: "kill",
        usage: "PID [--tree] [--force]",
        about: "End a process",
        details: "  --tree   with every process it started, and theirs\n  --force  also a game \
                  or an anti-cheat program, after it said what can happen",
        group: Group::Act,
        plan: |a| {
            let mut args = a.pid_only()?;
            args.insert("tree".into(), json!(a.flag("--tree")));
            Ok(tool("end_process", args, View::Auto))
        },
    },
    Command {
        name: "suspend",
        usage: "PID [--force]",
        about: "Pause a process",
        details: "",
        group: Group::Act,
        plan: |a| Ok(tool("suspend_process", a.pid_only()?, View::Auto)),
    },
    Command {
        name: "resume",
        usage: "PID [--force]",
        about: "Let a paused process go on",
        details: "",
        group: Group::Act,
        plan: |a| Ok(tool("resume_process", a.pid_only()?, View::Auto)),
    },
    Command {
        name: "priority",
        usage: "PID LEVEL [--force]",
        about: "Set a process' priority",
        details: "  LEVEL  idle, below_normal, normal, above_normal, high or realtime",
        group: Group::Act,
        plan: |a| {
            let mut args = Map::new();
            a.pid_word(0, "pid", &mut args)?;
            let Some(level) = a.words.get(1) else {
                return Err("which priority?".into());
            };
            args.insert("priority".into(), json!(level));
            Ok(tool("set_priority", args, View::Auto))
        },
    },
    Command {
        name: "mcp",
        usage: "[--http [--port N] [--key KEY]] [--allow KINDS]",
        about: "Serve MCP to an AI assistant, without the window",
        details: "  Without --http it speaks over standard input and output: the client starts \
                  this\n  command itself (\"command\": \"<this program>\", \"args\": \
                  [\"mcp\"]).\n  --http      listen on 127.0.0.1 instead, until Ctrl+C\n  --port N    \
                  the port (default: the MCP page's, 47123)\n  --key KEY   the access key \
                  requests must carry (default: the MCP page's)\n  --allow     what clients may \
                  do: read, trace, inspect, dump, record, end, control,\n              services, \
                  network, startup, or all (default: what the MCP page allows)",
        group: Group::Other,
        plan: |_| Ok(Plan::Serve),
    },
    Command {
        name: "tools",
        usage: "",
        about: "Every tool the MCP server has",
        details: "",
        group: Group::Other,
        plan: |_| Ok(Plan::Tools),
    },
    Command {
        name: "call",
        usage: "TOOL [NAME=VALUE ...]",
        about: "Call one tool, as an MCP client would",
        details: "  NAME=VALUE  an argument: numbers, true and false, and [1,2] are JSON; the \
                  rest is text\n  For example: call get_cpu trace_seconds=3",
        group: Group::Other,
        plan: |a| {
            let Some((name, rest)) = a.words.split_first() else {
                return Err("which tool? `tools` lists them".into());
            };
            let mut args = Map::new();
            for pair in rest {
                let Some((key, value)) = pair.split_once('=') else {
                    return Err(format!("\"{pair}\" is not NAME=VALUE"));
                };
                let value = serde_json::from_str(value).unwrap_or_else(|_| json!(value));
                args.insert(key.into(), value);
            }
            Ok(tool(name, args, View::Json))
        },
    },
];

fn tool(name: &str, arguments: Map<String, Value>, view: View) -> Plan {
    Plan::Call {
        tool: name.into(),
        arguments,
        view,
    }
}

fn obj(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// A command's arguments: words, --flags and --options with a value.
struct Args {
    words: Vec<String>,
    flags: HashSet<String>,
    values: HashMap<String, String>,
}

/// The options that take a value.
const VALUED: &[&str] = &[
    "--sort", "--limit", "--state", "--pid", "--depth", "--top", "--port", "--allow", "--key",
];
/// The options that stand alone.
const FLAGS: &[&str] = &[
    "--json",
    "--force",
    "--tree",
    "--all",
    "--dependents",
    "--http",
    "--files",
    "--types",
    "--rescan",
    "--help",
];

impl Args {
    fn parse(args: &[String]) -> Result<Args, String> {
        let mut parsed = Args {
            words: Vec::new(),
            flags: HashSet::new(),
            values: HashMap::new(),
        };
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if let Some((name, value)) = arg.split_once('=').filter(|(n, _)| n.starts_with("--")) {
                if !VALUED.contains(&name) {
                    return Err(format!("{name} takes no value"));
                }
                parsed.values.insert(name.into(), value.into());
            } else if VALUED.contains(&arg.as_str()) {
                let Some(value) = rest.next() else {
                    return Err(format!("{arg} needs a value"));
                };
                parsed.values.insert(arg.clone(), value.clone());
            } else if FLAGS.contains(&arg.as_str()) || arg == "-h" {
                parsed.flags.insert(if arg == "-h" {
                    "--help".into()
                } else {
                    arg.clone()
                });
            } else if arg.starts_with("--") {
                return Err(format!("there is no option {arg}"));
            } else {
                parsed.words.push(arg.clone());
            }
        }
        Ok(parsed)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn word(&self, i: usize, key: &str, into: &mut Map<String, Value>) {
        if let Some(word) = self.words.get(i) {
            into.insert(key.into(), json!(word));
        }
    }

    fn text(&self, option: &str, key: &str, into: &mut Map<String, Value>) {
        if let Some(value) = self.values.get(option) {
            into.insert(key.into(), json!(value));
        }
    }

    fn number(&self, option: &str, key: &str, into: &mut Map<String, Value>) -> Result<(), String> {
        if let Some(value) = self.values.get(option) {
            let n: u32 = value
                .parse()
                .map_err(|_| format!("{option} takes a number, not \"{value}\""))?;
            into.insert(key.into(), json!(n));
        }
        Ok(())
    }

    fn pid_word(&self, i: usize, key: &str, into: &mut Map<String, Value>) -> Result<(), String> {
        if let Some(word) = self.words.get(i) {
            let pid: u32 = word
                .parse()
                .map_err(|_| format!("\"{word}\" is not a process ID"))?;
            into.insert(key.into(), json!(pid));
        }
        Ok(())
    }

    /// `{"pid": N}` from the first word, which must be there.
    fn pid_only(&self) -> Result<Map<String, Value>, String> {
        let mut args = Map::new();
        self.pid_word(0, "pid", &mut args)?;
        if args.is_empty() {
            return Err("which process? Give its PID".into());
        }
        Ok(args)
    }
}

/// What tools may do when the user runs a command: anything, since the
/// user is the one acting. The refusals that protect the system stay.
fn everything() -> McpSettings {
    McpSettings {
        allowed: McpKind::ALL.to_vec(),
        ..Default::default()
    }
}

/// The client half of an in-process MCP session, and what to stop after.
struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    _server: rmcp::service::RunningService<rmcp::RoleServer, AnabolicMcp>,
    runtime: tokio::runtime::Runtime,
    _anabolic: Headless,
}

impl Session {
    fn open() -> Result<Session, String> {
        let _ = anabolic_core::hardening::harden_process();
        // Half a second between reads: the first answer with rates (which
        // need two reads) comes sooner than with the window's second.
        let anabolic = Headless::start(everything(), Duration::from_millis(500), false);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let shared = anabolic.shared.clone();
        let (client, server) = runtime.block_on(async move {
            let (server_end, client_end) = tokio::io::duplex(1 << 20);
            let server = tokio::spawn(AnabolicMcp::new(shared).serve(server_end));
            let client = ().serve(client_end).await.map_err(|e| e.to_string())?;
            let server = server
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((client, server))
        })?;
        Ok(Session {
            client,
            _server: server,
            runtime,
            _anabolic: anabolic,
        })
    }

    /// The tool's answer, or what it said instead.
    fn call(&self, tool: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let params = CallToolRequestParams::new(tool.to_string()).with_arguments(arguments);
        let result = self
            .runtime
            .block_on(self.client.call_tool(params))
            .map_err(|e| e.to_string())?;
        let text = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join(" ");
        if result.is_error == Some(true) {
            return Err(text);
        }
        Ok(result
            .structured_content
            .or_else(|| serde_json::from_str(&text).ok())
            .unwrap_or(Value::String(text)))
    }
}

fn call(tool: &str, mut arguments: Map<String, Value>, view: &View, args: &Args) -> u8 {
    if args.flag("--force") {
        arguments.insert("force".into(), json!(true));
    }
    let session = match Session::open() {
        Ok(session) => session,
        Err(e) => {
            eprintln!("{PROGRAM}: could not start: {e}");
            return 1;
        }
    };
    let answer = session.call(tool, arguments);
    let code = match answer {
        Ok(value) => {
            if args.flag("--json") || matches!(view, View::Json) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                print!("{}", render::render(&value, view));
            }
            0
        }
        Err(text) => {
            eprintln!("{}", refusal(&text, matches!(view, View::Json)));
            1
        }
    };
    // Threads that read the system need not be waited for.
    drop(session);
    code
}

/// A tool's refusal in a terminal's words: one that waits for agreement
/// says how to give it.
fn refusal(text: &str, raw: bool) -> String {
    let Some(rest) = text.strip_prefix(AGREE_START) else {
        if raw {
            return text.to_string();
        }
        // Messages written for a model name its tools; a terminal has
        // commands for the same.
        let mut text = text.to_string();
        for (tool, command) in [
            ("list_processes", "ps"),
            ("get_process_tree", "tree"),
            ("get_process", "show"),
            ("list_services", "services"),
            ("get_service", "service NAME"),
            ("list_listening_ports", "ports"),
            ("list_connections", "connections"),
            ("list_startup_items", "startup"),
            ("get_system_overview", "overview"),
        ] {
            text = text.replace(tool, &format!("`{PROGRAM} {command}`"));
        }
        return text;
    };
    let risks = rest.split(AGREE_END).next().unwrap_or(rest).trim();
    if raw {
        format!("{risks}\nCall it again with force=true to go ahead.")
    } else {
        format!("{risks}\nRun it again with --force to go ahead.")
    }
}

fn list_tools(args: &Args) -> u8 {
    let session = match Session::open() {
        Ok(session) => session,
        Err(e) => {
            eprintln!("{PROGRAM}: could not start: {e}");
            return 1;
        }
    };
    let tools = match session.runtime.block_on(session.client.list_all_tools()) {
        Ok(tools) => tools,
        Err(e) => {
            eprintln!("{PROGRAM}: {e}");
            return 1;
        }
    };
    if args.flag("--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&tools).unwrap_or_default()
        );
        return 0;
    }
    let width = tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for t in &tools {
        let description = t.description.as_deref().unwrap_or("");
        // The first sentence: the rest is for the model.
        let first = description
            .split_once(". ")
            .map_or(description, |(first, _)| first)
            .trim_end_matches('.');
        println!("{:<width$}  {first}", t.name);
    }
    println!(
        "\n`{PROGRAM} call TOOL NAME=VALUE ...` calls one; `{PROGRAM} tools --json` shows their \
         arguments."
    );
    0
}

/// `mcp`: serves until the client goes (standard input and output) or
/// Ctrl+C (HTTP).
fn serve(args: &Args) -> u8 {
    let saved = headless::saved_mcp_settings();
    let mut settings = McpSettings {
        allowed: saved.allowed.clone(),
        ..Default::default()
    };
    if let Some(list) = args.values.get("--allow") {
        settings.allowed = McpKind::parse_list(list);
        if settings.allowed.is_empty() {
            eprintln!("{PROGRAM} mcp: \"{list}\" names nothing clients could do.");
            return 2;
        }
    }
    let http = args.flag("--http");
    if http {
        settings.token = args
            .values
            .get("--key")
            .cloned()
            .unwrap_or(saved.token.clone());
        if settings.token.is_empty() {
            eprintln!(
                "{PROGRAM} mcp: over HTTP every request needs an access key. Give one with --key, \
                 or turn the server on once on Anabolic's MCP page, which makes one."
            );
            return 2;
        }
    }
    let port = match args.values.get("--port") {
        Some(port) => match port.parse::<u16>() {
            Ok(port) => port,
            Err(_) => {
                eprintln!("{PROGRAM} mcp: --port takes a port number, not \"{port}\"");
                return 2;
            }
        },
        None => saved.port,
    };
    let _ = anabolic_core::hardening::harden_process();
    let anabolic = Headless::start(settings, Duration::from_secs(1), true);
    let result = if http {
        serve_http(&anabolic, port)
    } else {
        crate::mcp::serve_stdio(anabolic.shared.clone(), async {
            let _ = tokio::signal::ctrl_c().await;
        })
    };
    drop(anabolic);
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{PROGRAM} mcp: {e}");
            1
        }
    }
}

fn serve_http(anabolic: &Headless, port: u16) -> Result<(), String> {
    let server = crate::mcp::McpServer::start(anabolic.shared.clone(), port).map_err(|e| {
        format!("{e}. Anabolic's window may be serving there: give another --port.")
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    drop(server);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        Args::parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn options_flags_and_words_come_apart() {
        let a = args(&["chrome", "--sort", "memory", "--limit=5", "--json"]).unwrap();
        assert_eq!(a.words, ["chrome"]);
        assert_eq!(a.values["--sort"], "memory");
        assert_eq!(a.values["--limit"], "5");
        assert!(a.flag("--json"));
        assert!(args(&["--sort"]).is_err());
        assert!(args(&["--nope"]).is_err());
        assert!(args(&["--json=yes"]).is_err());
    }

    #[test]
    fn each_command_comes_to_its_tool() {
        let plan = |name: &str, list: &[&str]| {
            let command = COMMANDS.iter().find(|c| c.name == name).unwrap();
            match (command.plan)(&args(list).unwrap()) {
                Ok(Plan::Call {
                    tool, arguments, ..
                }) => Ok((tool, Value::Object(arguments))),
                Ok(_) => Err("not a call".to_string()),
                Err(e) => Err(e),
            }
        };
        assert_eq!(
            plan("ps", &["code", "--sort", "memory", "--limit", "3"]).unwrap(),
            (
                "list_processes".into(),
                json!({"filter": "code", "sort": "memory", "limit": 3})
            )
        );
        assert_eq!(
            plan("kill", &["42", "--tree"]).unwrap(),
            ("end_process".into(), json!({"pid": 42, "tree": true}))
        );
        assert!(plan("kill", &["chrome"]).is_err());
        assert!(plan("kill", &[]).is_err());
        assert_eq!(
            plan("service", &["stop", "Spooler", "--dependents"]).unwrap(),
            (
                "control_service".into(),
                json!({"name": "Spooler", "action": "stop", "with_dependents": true})
            )
        );
        assert_eq!(
            plan("service", &["startup", "Spooler", "delayed"]).unwrap(),
            (
                "set_service_startup".into(),
                json!({"name": "Spooler", "startup": "automatic_delayed"})
            )
        );
        assert_eq!(
            plan("service", &["Spooler"]).unwrap(),
            ("get_service".into(), json!({"name": "Spooler"}))
        );
        assert_eq!(
            plan(
                "call",
                &["get_cpu", "trace_seconds=3", "name=x", "cpus=[1,2]"]
            )
            .unwrap(),
            (
                "get_cpu".into(),
                json!({"trace_seconds": 3, "name": "x", "cpus": [1, 2]})
            )
        );
    }

    #[test]
    fn a_refusal_that_waits_for_agreement_says_to_add_force() {
        let text =
            format!("{AGREE_START}It is a game with an anti-cheat program.{AGREE_END} and ask...");
        assert_eq!(
            refusal(&text, false),
            "It is a game with an anti-cheat program.\nRun it again with --force to go ahead."
        );
        assert_eq!(refusal("No such process.", false), "No such process.");
    }

    #[test]
    fn every_command_is_one_and_has_help() {
        for c in COMMANDS {
            assert!(is_command(c.name), "{}", c.name);
            assert!(c.help().contains(c.name));
        }
        assert!(!is_command("--tray"));
        assert!(!is_command("--new"));
    }
}
