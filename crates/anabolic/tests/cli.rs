// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The terminal program from outside, as a terminal or an MCP client uses
//! it: a command's JSON and exit code, and the MCP server over standard
//! input and output. On Linux the commands are anabolic's own (the file
//! that ships); on Windows anabolic-cli's.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn program() -> Command {
    #[cfg(windows)]
    let path = env!("CARGO_BIN_EXE_anabolic-cli");
    #[cfg(not(windows))]
    let path = env!("CARGO_BIN_EXE_anabolic");
    let mut command = Command::new(path);
    // Not the settings, history or MCP permissions of whoever runs the tests.
    let dir = std::env::temp_dir().join(format!("anabolic-cli-test-{}", std::process::id()));
    command.env("ANABOLIC_DATA_DIR", dir);
    command
}

#[test]
fn a_command_answers_in_json_and_a_refusal_fails() {
    let out = program()
        .args(["ps", "--limit", "3", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let processes = answer["processes"].as_array().unwrap();
    assert!((1..=3).contains(&processes.len()), "{answer}");
    assert!(processes[0]["pid"].is_u64());

    let out = program().args(["kill", "4294967"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("4294967"));

    let out = program().args(["kill", "chrome"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// A server process that is killed if a test leaves it running.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[test]
fn the_mcp_server_speaks_over_standard_input_and_output() {
    let mut server = Server(
        program()
            .args(["mcp", "--allow", "read"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut input = server.0.stdin.take().unwrap();
    let mut output = BufReader::new(server.0.stdout.take().unwrap());
    let mut send = |message: Value| {
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    };
    let mut answer = || {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };

    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "1"}}}),
    );
    assert_eq!(answer()["result"]["serverInfo"]["name"], "anabolic");
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = answer()["result"]["tools"].as_array().unwrap().len();
    assert!(tools > 30, "{tools} tools");

    send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "list_processes", "arguments": {"limit": 2}}}),
    );
    let result = answer()["result"].clone();
    assert_ne!(result["isError"], true, "{result}");
    assert!(
        !result["structuredContent"]["processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Only reading was allowed.
    send(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
        "name": "end_process", "arguments": {"pid": 1}}}),
    );
    let result = answer()["result"].clone();
    assert_eq!(result["isError"], true, "{result}");

    // The client goes: so does the server.
    drop(send);
    drop(input);
    let since = Instant::now();
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(since.elapsed() < Duration::from_secs(15), "still running");
        std::thread::sleep(Duration::from_millis(100));
    }
}
