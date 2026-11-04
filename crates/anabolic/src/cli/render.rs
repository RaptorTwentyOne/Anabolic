// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A tool's JSON answer as text for a terminal: values side by side with
//! their names, lists of like objects as tables. Names say their unit
//! (`_bytes`, `_bps`, `_percent`, `_ms`), and values are shown in it.

use std::fmt::Write;

use serde_json::{Map, Value};

/// How a command shows its answer.
pub enum View {
    /// Values and tables, laid out from the answer itself.
    Auto,
    /// The JSON as it is (`call`).
    Json,
    /// The list under `list` as a table of these columns ("key" or
    /// "key=Heading"), and the answer's other plain values on a line under it.
    Table {
        list: &'static str,
        columns: &'static [&'static str],
    },
    /// The list under `list` as an indented tree, each item's `children`
    /// under it; `columns` as for a table.
    Tree {
        list: &'static str,
        columns: &'static [&'static str],
    },
}

/// Longest text a table cell shows; the rest is cut with an ellipsis.
const CELL: usize = 56;
/// Most columns a table shows: a terminal is only so wide.
const COLUMNS: usize = 9;

pub fn render(value: &Value, view: &View) -> String {
    match view {
        View::Json => serde_json::to_string_pretty(value).unwrap_or_default() + "\n",
        View::Table { list, columns } => {
            let mut out = String::new();
            match value.get(*list).and_then(Value::as_array) {
                Some(items) if !items.is_empty() => table(&mut out, items, columns),
                _ => out.push_str("Nothing.\n"),
            }
            footer(&mut out, value, list);
            out
        }
        View::Tree { list, columns } => {
            let mut out = String::new();
            if let Some(items) = value.get(*list).and_then(Value::as_array) {
                for item in items {
                    branch(&mut out, item, columns, "", "");
                }
            }
            footer(&mut out, value, list);
            out
        }
        View::Auto => {
            let mut out = String::new();
            match value {
                Value::Object(map) => object(&mut out, map, 0),
                Value::Array(items) => list(&mut out, "", items, 0),
                other => {
                    let _ = writeln!(out, "{}", scalar("", other));
                }
            }
            out
        }
    }
}

fn object(out: &mut String, map: &Map<String, Value>, indent: usize) {
    let pad = " ".repeat(indent);
    let scalars: Vec<(&String, &Value)> = map.iter().filter(|(_, v)| is_flat(v)).collect();
    let width = scalars
        .iter()
        .map(|(k, _)| title(k).chars().count())
        .max()
        .unwrap_or(0);
    for (key, value) in &scalars {
        let text = match value {
            Value::Array(items) => items
                .iter()
                .map(|v| scalar("", v))
                .collect::<Vec<_>>()
                .join(", "),
            v => scalar(key, v),
        };
        if text.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{pad}{:<width$}  {text}", title(key));
    }
    for (key, value) in map.iter().filter(|(_, v)| !is_flat(v)) {
        match value {
            Value::Object(inner) if !inner.is_empty() => {
                let _ = writeln!(out, "\n{pad}{}", title(key));
                object(out, inner, indent + 2);
            }
            Value::Array(items) if !items.is_empty() => {
                let _ = writeln!(out, "\n{pad}{} ({})", title(key), items.len());
                list(out, key, items, indent + 2);
            }
            _ => {}
        }
    }
}

/// A list of objects as a table, of anything else one per line.
fn list(out: &mut String, _key: &str, items: &[Value], indent: usize) {
    let pad = " ".repeat(indent);
    if !items.iter().all(Value::is_object) {
        for item in items {
            match item {
                Value::Object(map) => object(out, map, indent),
                v => {
                    let _ = writeln!(out, "{pad}{}", scalar("", v));
                }
            }
        }
        return;
    }
    // Columns: the plain values, in the order they first appear.
    let mut columns: Vec<&str> = Vec::new();
    for item in items {
        if let Value::Object(map) = item {
            for (k, v) in map {
                if !matches!(v, Value::Object(_) | Value::Array(_))
                    && !columns.contains(&k.as_str())
                {
                    columns.push(k);
                }
            }
        }
    }
    // Columns empty in every row say nothing.
    columns.retain(|c| {
        items
            .iter()
            .any(|i| i.get(*c).is_some_and(|v| !scalar(c, v).is_empty()))
    });
    columns.truncate(COLUMNS);
    let cells: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|c| item.get(*c).map_or(String::new(), |v| cut(&scalar(c, v))))
                .collect()
        })
        .collect();
    let heads: Vec<String> = columns.iter().map(|c| title(c)).collect();
    let numeric: Vec<bool> = columns
        .iter()
        .map(|c| {
            items
                .iter()
                .filter_map(|i| i.get(*c))
                .all(|v| v.is_number() || v.is_null())
        })
        .collect();
    grid(out, &heads, &cells, &numeric, &pad);
}

/// Rows under headings, each column as wide as its widest text; numbers
/// to the right.
fn grid(out: &mut String, heads: &[String], cells: &[Vec<String>], numeric: &[bool], pad: &str) {
    let widths: Vec<usize> = (0..heads.len())
        .map(|i| {
            cells
                .iter()
                .map(|row| row[i].chars().count())
                .chain([heads[i].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |texts: &[String]| {
        let mut row = pad.to_string();
        for (i, text) in texts.iter().enumerate() {
            if i > 0 {
                row.push_str("  ");
            }
            let w = widths[i];
            if numeric[i] {
                let _ = write!(row, "{text:>w$}");
            } else if i + 1 < texts.len() {
                let _ = write!(row, "{text:<w$}");
            } else {
                row.push_str(text);
            }
        }
        row.trim_end().to_string()
    };
    let _ = writeln!(out, "{}", line(heads));
    for row in cells {
        let _ = writeln!(out, "{}", line(row));
    }
}

/// A table of `columns` ("key" or "key=Heading") from `items`. Rates and
/// shares that are nil are left blank, so what is busy stands out.
fn table(out: &mut String, items: &[Value], columns: &[&str]) {
    let columns: Vec<(&str, String)> = columns
        .iter()
        .map(|c| match c.split_once('=') {
            Some((key, head)) => (key, head.to_string()),
            None => (*c, title(c)),
        })
        .filter(|(key, _)| items.iter().any(|i| i.get(*key).is_some()))
        .collect();
    let cells: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|(key, _)| match item.get(*key) {
                    Some(v) if is_nil_rate(key, v) => String::new(),
                    Some(v) => cut(&scalar(key, v)),
                    None => String::new(),
                })
                .collect()
        })
        .collect();
    let numeric: Vec<bool> = columns
        .iter()
        .map(|(key, _)| {
            items
                .iter()
                .filter_map(|i| i.get(*key))
                .all(|v| v.is_number() || v.is_null())
        })
        .collect();
    let heads: Vec<String> = columns.iter().map(|(_, head)| head.clone()).collect();
    grid(out, &heads, &cells, &numeric, "");
}

fn is_nil_rate(key: &str, value: &Value) -> bool {
    (key.ends_with("_bps") || key == "gpu_percent") && value.as_f64() == Some(0.0)
}

/// One item of a tree and, under it, its children.
fn branch(out: &mut String, item: &Value, columns: &[&str], lead: &str, rest: &str) {
    let texts: Vec<String> = columns
        .iter()
        .filter_map(|c| {
            let key = c.split_once('=').map_or(*c, |(key, _)| key);
            let v = item.get(key)?;
            let text = scalar(key, v);
            // A tree is for who started what: idle shares say nothing there.
            let idle = key.ends_with("_percent") && v.as_f64() == Some(0.0);
            if text.is_empty() || idle || is_nil_rate(key, v) {
                return None;
            }
            Some(if key.ends_with("_percent") {
                format!("{text} %")
            } else {
                text
            })
        })
        .collect();
    let _ = writeln!(out, "{lead}{}", texts.join("  "));
    if let Some(children) = item.get("children").and_then(Value::as_array) {
        for (i, child) in children.iter().enumerate() {
            let (mark, more) = if i + 1 == children.len() {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            branch(
                out,
                child,
                columns,
                &format!("{rest}{mark}"),
                &format!("{rest}{more}"),
            );
        }
    }
}

/// The answer's plain values other than its list, on one line; an error
/// it carries on a line of its own.
fn footer(out: &mut String, value: &Value, list: &str) {
    let Some(map) = value.as_object() else {
        return;
    };
    let mut notes = Vec::new();
    for (key, v) in map {
        // A "no" (not truncated, say) is nothing to remark on.
        if key == list || !is_flat(v) || v == &Value::Bool(false) {
            continue;
        }
        let text = scalar(key, v);
        if text.is_empty() {
            continue;
        }
        if key.contains("error") {
            let _ = writeln!(out, "{}: {text}", title(key));
        } else {
            notes.push(format!("{} {text}", title(key).to_lowercase()));
        }
    }
    if !notes.is_empty() {
        let _ = writeln!(out, "\n{}", notes.join(", "));
    }
}

/// Values a line or a cell can hold: not objects, and lists only of plain
/// values.
fn is_flat(value: &Value) -> bool {
    match value {
        Value::Object(_) => false,
        Value::Array(items) => items
            .iter()
            .all(|v| !matches!(v, Value::Object(_) | Value::Array(_))),
        _ => true,
    }
}

/// A value in the unit its name says.
fn scalar(key: &str, value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            if key.ends_with("_bytes") {
                bytes(x)
            } else if key.ends_with("_bps") {
                format!("{}/s", bytes(x))
            } else if key.ends_with("_ms") {
                format!("{} ms", decimal(x))
            } else if n.is_f64() {
                decimal(x)
            } else {
                n.to_string()
            }
        }
        Value::Array(items) => items
            .iter()
            .map(|v| scalar("", v))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => String::new(),
    }
}

fn decimal(x: f64) -> String {
    let digits = if x.abs() >= 100.0 {
        0
    } else if x.abs() >= 10.0 {
        1
    } else {
        2
    };
    let text = format!("{x:.digits$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

fn bytes(x: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = x;
    let mut unit = 0;
    while value.abs() >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{} {}", decimal(value), UNITS[unit])
    }
}

/// A field's name as a heading: units dropped (the values carry them),
/// words spaced, the usual capitals.
fn title(key: &str) -> String {
    let mut base = key;
    let mut suffix = "";
    for (end, unit) in [
        ("_bytes", ""),
        ("_bps", ""),
        ("_ms", ""),
        ("_percent", " %"),
        ("_pct", " %"),
    ] {
        if let Some(stripped) = key.strip_suffix(end) {
            base = stripped;
            suffix = unit;
            break;
        }
    }
    let words: Vec<String> = base
        .split('_')
        .enumerate()
        .map(|(i, word)| match word {
            "pid" | "cpu" | "gpu" | "id" | "io" | "tcp" | "udp" | "dns" | "ip" | "dpc" | "isr"
            | "sid" | "url" => word.to_uppercase(),
            _ if i == 0 => {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
            _ => word.to_string(),
        })
        .collect();
    format!("{}{suffix}", words.join(" "))
}

fn cut(text: &str) -> String {
    let text = text.replace(['\n', '\r', '\t'], " ");
    if text.chars().count() <= CELL {
        text
    } else {
        let kept: String = text.chars().take(CELL - 1).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_become_headings_with_their_units() {
        assert_eq!(title("cpu_percent"), "CPU %");
        assert_eq!(title("private_bytes"), "Private");
        assert_eq!(title("read_bps"), "Read");
        assert_eq!(title("parent_pid"), "Parent PID");
        assert_eq!(title("name"), "Name");
    }

    #[test]
    fn values_come_in_their_units() {
        assert_eq!(scalar("memory_bytes", &json!(1536)), "1.5 KB");
        assert_eq!(scalar("memory_bytes", &json!(3_221_225_472u64)), "3 GB");
        assert_eq!(scalar("read_bps", &json!(2_097_152)), "2 MB/s");
        assert_eq!(scalar("cpu_percent", &json!(12.345)), "12.3");
        assert_eq!(scalar("took_ms", &json!(0.5)), "0.5 ms");
        assert_eq!(scalar("pid", &json!(42)), "42");
        assert_eq!(scalar("hung", &json!(true)), "yes");
        assert_eq!(scalar("x", &Value::Null), "");
    }

    #[test]
    fn a_table_shows_its_columns_and_the_rest_under_it() {
        let text = render(
            &json!({
                "matching": 2,
                "processes": [
                    {"pid": 4, "name": "System", "net_send_bps": 0.0, "private_bytes": 2048},
                    {"pid": 77, "name": "x.exe", "net_send_bps": 1024.0, "private_bytes": 1},
                ],
                "error": null
            }),
            &View::Table {
                list: "processes",
                columns: &["pid", "name", "private_bytes=Memory", "net_send_bps=Sent"],
            },
        );
        assert_eq!(
            text,
            "PID  Name    Memory    Sent\n  4  System    2 KB\n 77  x.exe      1 B  1 KB/s\n\nmatching 2\n"
        );
    }

    #[test]
    fn a_tree_hangs_children_under_their_parent() {
        let text = render(
            &json!({"tree": [{"pid": 1, "name": "a", "children": [
                {"pid": 2, "name": "b", "children": [{"pid": 4, "name": "d"}]},
                {"pid": 3, "name": "c"}
            ]}]}),
            &View::Tree {
                list: "tree",
                columns: &["pid", "name"],
            },
        );
        assert_eq!(text, "1  a\n├─ 2  b\n│  └─ 4  d\n└─ 3  c\n");
    }

    #[test]
    fn a_list_of_objects_is_a_table() {
        let text = render(
            &json!({
                "count": 2,
                "processes": [
                    {"pid": 4, "name": "System", "cpu_percent": 0.5},
                    {"pid": 1234, "name": "chrome.exe", "cpu_percent": 12.0},
                ]
            }),
            &View::Auto,
        );
        assert_eq!(
            text,
            "Count  2\n\nProcesses (2)\n   PID  Name        CPU %\n     4  System        0.5\n  \
             1234  chrome.exe     12\n"
        );
    }
}
