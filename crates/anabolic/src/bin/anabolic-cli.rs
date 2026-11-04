// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! anabolic-cli: Anabolic's terminal commands (`anabolic-cli ps`) and its
//! MCP server without a window (`anabolic-cli mcp`). Unlike anabolic.exe a
//! console program, so a terminal waits for it and shows what it prints,
//! and one that runs with the rights of whoever starts it (see build.rs).
//! On Linux the same commands are anabolic's own.

fn main() -> std::process::ExitCode {
    anabolic::cli_main()
}
