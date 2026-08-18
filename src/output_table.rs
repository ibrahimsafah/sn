use crate::error::Result;
use crate::output::map_stdout_err;
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use serde_json::Value;
use std::io::{self, Write};

const MAX_CELL: usize = 60;

/// Render a JSON value to stdout as a human-readable columnar table.
pub fn write_table(value: &Value) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    render(&mut out, value)
}

fn render<W: Write>(w: &mut W, value: &Value) -> Result<()> {
    match value {
        Value::Array(arr) => {
            if arr.is_empty() {
                return w.write_all(b"(no records)\n").map_err(map_stdout_err);
            }
            if arr.iter().all(|v| v.is_object()) {
                let table = build_array_table(arr);
                writeln!(w, "{table}").map_err(map_stdout_err)
            } else {
                // Heterogeneous array: render as single-column "Value" table.
                let mut t = new_table();
                t.set_header(vec!["Value"]);
                for v in arr {
                    t.add_row(vec![cell_for(v)]);
                }
                writeln!(w, "{t}").map_err(map_stdout_err)
            }
        }
        Value::Object(_) => {
            let table = build_object_table(value);
            writeln!(w, "{table}").map_err(map_stdout_err)
        }
        Value::Null => writeln!(w).map_err(map_stdout_err),
        Value::String(s) => writeln!(w, "{s}").map_err(map_stdout_err),
        Value::Bool(b) => writeln!(w, "{b}").map_err(map_stdout_err),
        Value::Number(n) => writeln!(w, "{n}").map_err(map_stdout_err),
    }
}

fn new_table() -> Table {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    t
}

fn build_array_table(arr: &[Value]) -> Table {
    // Union of keys, preserving first-seen order.
    let mut headers: Vec<String> = Vec::new();
    for v in arr {
        if let Some(obj) = v.as_object() {
            for k in obj.keys() {
                if !headers.iter().any(|h| h == k) {
                    headers.push(k.clone());
                }
            }
        }
    }
    let mut t = new_table();
    t.set_header(headers.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    for v in arr {
        let obj = v.as_object();
        let row: Vec<String> = headers
            .iter()
            .map(|h| match obj.and_then(|o| o.get(h)) {
                Some(val) => cell_for(val),
                None => String::new(),
            })
            .collect();
        t.add_row(row);
    }
    t
}

fn build_object_table(value: &Value) -> Table {
    let mut t = new_table();
    t.set_header(vec!["Key", "Value"]);
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            t.add_row(vec![k.clone(), cell_for(v)]);
        }
    }
    t
}

fn cell_for(v: &Value) -> String {
    let raw = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(v).unwrap_or_else(|_| String::new())
        }
    };
    truncate_cell(&raw, MAX_CELL)
}

fn truncate_cell(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 3).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render_to_string(v: &Value) -> String {
        let mut buf = Vec::new();
        render(&mut buf, v).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn renders_array_of_objects_with_headers_and_cells() {
        let v = json!([
            {"number": "INC0001", "state": "New"},
            {"number": "INC0002", "state": "Closed", "extra": "x"}
        ]);
        let s = render_to_string(&v);
        assert!(s.contains("number"), "missing 'number' header in:\n{s}");
        assert!(s.contains("state"), "missing 'state' header in:\n{s}");
        assert!(s.contains("extra"), "missing 'extra' header in:\n{s}");
        assert!(s.contains("INC0001"), "missing cell value INC0001 in:\n{s}");
        assert!(s.contains("Closed"), "missing cell value Closed in:\n{s}");
    }

    #[test]
    fn renders_single_object_as_two_columns() {
        let v = json!({"name": "alice", "active": true});
        let s = render_to_string(&v);
        assert!(s.contains("Key"), "missing Key header:\n{s}");
        assert!(s.contains("Value"), "missing Value header:\n{s}");
        assert!(s.contains("name"), "missing key 'name':\n{s}");
        assert!(s.contains("alice"), "missing value 'alice':\n{s}");
        assert!(s.contains("true"), "missing bool value:\n{s}");
    }

    #[test]
    fn renders_empty_array_as_no_records() {
        let s = render_to_string(&json!([]));
        assert_eq!(s, "(no records)\n");
    }

    #[test]
    fn truncates_long_cells() {
        let long = "a".repeat(120);
        let out = truncate_cell(&long, 60);
        assert_eq!(out.chars().count(), 60);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn nested_renders_as_compact_json() {
        let v = json!({"meta": {"k": 1}});
        let s = render_to_string(&v);
        assert!(s.contains("{\"k\":1}"), "expected compact JSON cell:\n{s}");
    }
}
