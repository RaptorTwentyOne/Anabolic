//! Resources and prompts. A resource is a tool call under a fixed address
//! (the same data, the same permissions); a prompt is a ready question that
//! tells the assistant which tools answer it and in what order.

use rmcp::model::{
    GetPromptResult, Prompt, PromptArgument, PromptMessage, Resource, ResourceTemplate, Role,
};
use serde_json::{Map, Value, json};

/// Fixed resources: address, name, what it holds, and the tool call behind it.
const RESOURCES: [(&str, &str, &str, &str); 6] = [
    (
        "anabolic://overview",
        "System overview",
        "CPU, memory, disks, network, GPUs, the busiest processes and what needs attention.",
        "get_system_overview",
    ),
    (
        "anabolic://processes",
        "Processes",
        "The 100 busiest processes by CPU, one line each.",
        "list_processes",
    ),
    (
        "anabolic://network",
        "Network usage",
        "Each program's traffic (no trace: rates from the last refresh).",
        "get_network_usage",
    ),
    (
        "anabolic://services",
        "Services",
        "Every service with its state, startup type and process.",
        "list_services",
    ),
    (
        "anabolic://startup",
        "Startup programs",
        "What starts with the computer or the session, whether it is on, and what it costs now.",
        "list_startup_items",
    ),
    (
        "anabolic://history/today",
        "Today's history",
        "Today's samples of the flight recorder (the latest 120).",
        "get_history",
    ),
];

pub fn resources() -> Vec<Resource> {
    RESOURCES
        .iter()
        .map(|(uri, name, description, _)| {
            Resource::new(*uri, *name)
                .with_description(*description)
                .with_mime_type("application/json")
        })
        .collect()
}

pub fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("anabolic://process/{pid}", "Process")
            .with_description("One process in full, as get_process gives it.")
            .with_mime_type("application/json"),
    ]
}

/// The tool call behind a resource address.
pub fn resolve(uri: &str) -> Option<(&'static str, Map<String, Value>)> {
    if let Some(pid) = uri.strip_prefix("anabolic://process/") {
        let pid: u32 = pid.trim_end_matches('/').parse().ok()?;
        return Some(("get_process", args(json!({ "pid": pid }))));
    }
    let (_, _, _, tool) = RESOURCES.iter().find(|(u, ..)| *u == uri)?;
    let arguments = match *tool {
        "list_processes" => json!({ "limit": 100 }),
        "get_network_usage" => json!({ "trace_seconds": 0 }),
        _ => json!({}),
    };
    Some((tool, args(arguments)))
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Prompts: name, title, description, arguments (name, description,
/// required).
type PromptDef = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str, bool)],
);

const PROMPTS: [PromptDef; 6] = [
    (
        "diagnose_slowness",
        "Why is the PC slow?",
        "Finds what makes the PC slow right now: CPU, memory, disk, GPU or a hung program.",
        &[],
    ),
    (
        "why_not_responding",
        "Why is a program not responding?",
        "Looks into one program that hangs or will not close.",
        &[("program", "The program's name or PID.", true)],
    ),
    (
        "game_stutters",
        "Why does a game stutter?",
        "Frame times, driver latency and what runs beside the game.",
        &[("game", "The game's name, if known.", false)],
    ),
    (
        "what_happened_at",
        "What happened at a time?",
        "Reads the history around a moment.",
        &[
            ("time", "Local time, HH:MM.", true),
            ("date", "The day, YYYY-MM-DD (default today).", false),
        ],
    ),
    (
        "network_check",
        "Who talks to where?",
        "Which programs use the network, where their traffic goes, and which ports are open.",
        &[],
    ),
    (
        "startup_cleanup",
        "What could stop starting automatically?",
        "Reviews the startup programs and suggests which to turn off.",
        &[],
    ),
];

pub fn prompts() -> Vec<Prompt> {
    PROMPTS
        .iter()
        .map(|(name, title, description, arguments)| {
            let arguments: Vec<PromptArgument> = arguments
                .iter()
                .map(|(n, d, required)| {
                    PromptArgument::new(*n)
                        .with_description(*d)
                        .with_required(*required)
                })
                .collect();
            let mut prompt = Prompt::new(
                *name,
                Some(*description),
                (!arguments.is_empty()).then_some(arguments),
            );
            prompt.title = Some(title.to_string());
            prompt
        })
        .collect()
}

/// The question a prompt asks, with its arguments filled in.
pub fn prompt(name: &str, arguments: &Map<String, Value>) -> Option<GetPromptResult> {
    let arg = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let text = match name {
        "diagnose_slowness" => "My PC feels slow right now. Use Anabolic's tools to find out \
            why: start with get_system_overview and look at what it lists under attention. Then \
            follow the resource that is short: get_cpu (with trace_seconds 5 to see who runs on \
            each core), get_memory, get_disks (trace_seconds 5 shows the files), get_gpus. Name \
            the processes responsible with get_process. Tell me what you found and what I could \
            do; do not change anything before I agree."
            .to_string(),
        "why_not_responding" => format!(
            "{} does not respond (or will not close). Find it with list_processes (filter by \
             name) and read get_process: is it marked not responding or ending? If it is ending \
             but cannot finish, use why_process_will_not_end. If it hangs, get_wait_chain shows \
             what its threads wait for: that opens the process, so tell me the risk first and \
             ask before calling it again with force=true. Explain the cause and the options.",
            if arg("program").is_empty() {
                "A program".to_string()
            } else {
                arg("program")
            }
        ),
        "game_stutters" => format!(
            "{} stutters. Use get_stutters (watch_seconds 15 while I play) for frame times, 1 % \
             lows and the stutters with what happened at the time, and the drivers with the \
             longest DPC/ISR times. Check get_system_overview and get_cpu (trace_seconds 5) for \
             what else runs. Do not open the game's process (no inspection tools on it): \
             anti-cheat software can take that badly. Tell me the likely cause.",
            if arg("game").is_empty() {
                "My game".to_string()
            } else {
                arg("game")
            }
        ),
        "what_happened_at" => {
            let date = arg("date");
            format!(
                "What happened on this PC at {}{}? Read get_history from ten minutes before to \
                 ten minutes after (from/to as HH:MM{}): which processes led, what started or \
                 exited, what hung. Summarise it and point out anything unusual.",
                arg("time"),
                if date.is_empty() {
                    String::new()
                } else {
                    format!(" on {date}")
                },
                if date.is_empty() {
                    String::new()
                } else {
                    format!(", date {date}")
                }
            )
        }
        "network_check" => "Check this PC's network use. Use get_network_usage \
            (trace_seconds 10) for who sends and receives how much and where to, \
            list_listening_ports for what others could connect to (point out ports reachable \
            from every network), and list_dns_lookups for the names programs looked up. Tell me \
            anything that looks unexpected; change nothing before I agree."
            .to_string(),
        "startup_cleanup" => format!(
            "Review what starts with {system} using list_startup_items: for each item that is \
             on, say what it is, whether it is part of {system}, and whether it seems needed at \
             startup. Suggest which could be turned off and why. Do not turn any off before I \
             agree (set_startup_item does it).",
            system = crate::platform::SYSTEM
        ),
        _ => return None,
    };
    Some(GetPromptResult::new(vec![PromptMessage::new_text(
        Role::User,
        text,
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_resource_resolves_to_a_tool() {
        for r in resources() {
            assert!(resolve(&r.uri).is_some(), "{}", r.uri);
        }
        let (tool, args) = resolve("anabolic://process/1234").unwrap();
        assert_eq!(tool, "get_process");
        assert_eq!(args["pid"], 1234);
        assert!(resolve("anabolic://process/x").is_none());
        assert!(resolve("anabolic://nothing").is_none());
    }

    #[test]
    fn every_prompt_has_a_text() {
        let mut arguments = Map::new();
        arguments.insert("program".into(), json!("chrome.exe"));
        arguments.insert("time".into(), json!("14:32"));
        for p in prompts() {
            let result = prompt(&p.name, &arguments).expect(&p.name);
            assert_eq!(result.messages.len(), 1);
        }
        assert!(prompt("nothing", &arguments).is_none());
    }
}
