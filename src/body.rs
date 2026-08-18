use crate::error::{Error, Result};
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, Read};

/// Raw user input describing where the body comes from.
#[derive(Debug, Clone)]
pub enum BodyInput {
    /// `--data '<json>'` literal or `--data @file` or `--data @-` (stdin).
    Data(String),
    /// Repeated `--field name=value` (or `name=@file`).
    Fields(Vec<String>),
    None,
}

pub fn build_body(input: BodyInput) -> Result<Value> {
    match input {
        BodyInput::Data(spec) => parse_data_spec(&spec),
        BodyInput::Fields(specs) => parse_field_specs(&specs),
        BodyInput::None => Err(Error::Usage(
            "a request body is required; pass --data or one or more --field".into(),
        )),
    }
}

/// What an empty pair (neither `--data` nor `--field` given) means at a call
/// site. The write commands share everything about body parsing except this,
/// so it stays a per-site decision rather than a default.
#[derive(Clone, Copy, Debug)]
pub enum EmptyBody {
    /// A usage error — the endpoint cannot do anything with an empty write.
    Reject,
    /// Send `{}` — the endpoint accepts an empty payload (ordering a catalog
    /// item with no variables, creating a change from its type's defaults).
    Object,
}

/// The `--data`/`--field` pair as a [`BodyInput`]. `--data` is checked first,
/// but clap's `conflicts_with` has already rejected the pair together.
pub fn input_from(data: Option<String>, field: Vec<String>) -> BodyInput {
    if let Some(d) = data {
        BodyInput::Data(d)
    } else if !field.is_empty() {
        BodyInput::Fields(field)
    } else {
        BodyInput::None
    }
}

/// Parse the `--data`/`--field` pair into a JSON body, resolving an empty
/// pair per `empty`.
pub fn from_flags(data: Option<String>, field: Vec<String>, empty: EmptyBody) -> Result<Value> {
    let input = match input_from(data, field) {
        BodyInput::None => match empty {
            EmptyBody::Reject => BodyInput::None,
            EmptyBody::Object => BodyInput::Data("{}".into()),
        },
        input => input,
    };
    build_body(input)
}

fn parse_data_spec(raw: &str) -> Result<Value> {
    let value = parse_data_value(raw)?;
    if !value.is_object() {
        return Err(Error::Usage(
            "--data must be a JSON object at the top level".into(),
        ));
    }
    Ok(value)
}

/// Resolve a `--data` spec (inline JSON, `@file`, or `@-`) to any JSON value,
/// without the top-level-object requirement. For endpoints that take arrays
/// (e.g. `import bulk`).
pub fn parse_data_value(raw: &str) -> Result<Value> {
    let source = if raw == "@-" {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Error::Usage(format!("read stdin: {e}")))?;
        s
    } else if let Some(path) = raw.strip_prefix('@') {
        fs::read_to_string(path).map_err(|e| Error::Usage(format!("read {path}: {e}")))?
    } else {
        raw.to_string()
    };
    serde_json::from_str(&source)
        .map_err(|e| Error::Usage(format!("--data is not valid JSON: {e}")))
}

fn parse_field_specs(specs: &[String]) -> Result<Value> {
    if specs.is_empty() {
        return Err(Error::Usage("at least one --field is required".into()));
    }
    let mut map: Map<String, Value> = Map::new();
    for spec in specs {
        let (name, raw_value) = spec
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--field '{spec}' must be in name=value form")))?;
        if name.is_empty() {
            return Err(Error::Usage(format!("--field '{spec}' has empty name")));
        }
        if map.contains_key(name) {
            return Err(Error::Usage(format!(
                "--field '{name}' specified more than once"
            )));
        }
        let value = coerce_field_value(raw_value)?;
        map.insert(name.to_string(), value);
    }
    Ok(Value::Object(map))
}

fn coerce_field_value(raw: &str) -> Result<Value> {
    if let Some(path) = raw.strip_prefix('@') {
        let s = fs::read_to_string(path).map_err(|e| Error::Usage(format!("read {path}: {e}")))?;
        return Ok(Value::String(s));
    }
    // Try JSON scalars first (true/false/null/number), fall back to string.
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && (v.is_boolean() || v.is_null() || v.is_number())
    {
        return Ok(v);
    }
    Ok(Value::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_inline_json_parses() {
        let v = build_body(BodyInput::Data(r#"{"a": 1}"#.into())).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn data_top_level_must_be_object() {
        let err = build_body(BodyInput::Data("[1,2,3]".into())).unwrap_err();
        assert!(matches!(err, Error::Usage(_)));
    }

    #[test]
    fn data_value_allows_top_level_array() {
        let v = parse_data_value(r#"[{"a":1},{"a":2}]"#).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn fields_merge_into_object() {
        let v = build_body(BodyInput::Fields(vec![
            "a=1".into(),
            "b=x".into(),
            "c=true".into(),
        ]))
        .unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "x");
        assert_eq!(v["c"], true);
    }

    #[test]
    fn duplicate_field_is_usage_error() {
        let err = build_body(BodyInput::Fields(vec!["a=1".into(), "a=2".into()])).unwrap_err();
        assert!(matches!(err, Error::Usage(_)));
    }

    #[test]
    fn field_file_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.txt");
        std::fs::write(&path, "hello world").unwrap();
        let spec = format!("k=@{}", path.to_str().unwrap());
        let v = build_body(BodyInput::Fields(vec![spec])).unwrap();
        assert_eq!(v["k"], "hello world");
    }

    #[test]
    fn data_at_file_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        std::fs::write(&path, r#"{"x": 10}"#).unwrap();
        let v = build_body(BodyInput::Data(format!("@{}", path.to_str().unwrap()))).unwrap();
        assert_eq!(v["x"], 10);
    }
}
