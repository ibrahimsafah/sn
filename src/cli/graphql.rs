//! `sn graphql` — GraphQL passthrough (POST /api/now/graphql).
//!
//! One endpoint serves the whole GraphQL surface: the scripted namespaces and
//! the generated `GlideRecord_Query` / `GlideRecord_Mutation` /
//! `GlideAggregateRecord_Query` namespaces (a query field and CRUD mutations
//! per table, auto-derived from the data dictionary). `sn raw` can technically
//! reach it, but GraphQL breaks the exit-code contract `raw` relies on: a
//! failed query still returns **HTTP 200**, with the failure in an `errors`
//! array — sometimes alongside partial `data`. This command restores the
//! contract: a response with errors exits 2 with the standard stderr envelope
//! (the full `errors` array rides in `sn_error`), and any partial `data` is
//! still written to stdout first so a caller that wants it has it.
//!
//! On success, stdout gets `data` unwrapped — the GraphQL analogue of
//! stripping the `{"result": ...}` envelope; `--output raw` keeps the whole
//! response body.

use crate::cli::kernel::{connect, write_response};
use crate::cli::{GlobalFlags, OutputMode};
use crate::error::{Error, Result};
use serde_json::{Map, Value};
use std::fs;
use std::io::Read;

#[derive(clap::Args, Debug)]
pub struct GraphqlArgs {
    /// GraphQL document: inline text, @file (path), or @- (stdin).
    pub query: String,
    /// Repeatable variable as key=value; the value is always a JSON string.
    /// For non-string variables (Int, input objects) use --variables.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    pub var: Vec<String>,
    /// All variables as one JSON object: inline, @file, or @- (stdin).
    /// --var entries are applied on top and win on conflict.
    #[arg(long, value_name = "JSON")]
    pub variables: Option<String>,
    /// Operation name to execute, for a document that defines more than one.
    #[arg(long, value_name = "NAME")]
    pub operation: Option<String>,
}

pub fn run(global: &GlobalFlags, args: GraphqlArgs) -> Result<()> {
    let query = read_text_input(&args.query, "query")?;
    let variables = build_variables(args.variables.as_deref(), &args.var)?;

    let client = connect(global)?;

    let resp = execute(&client, &query, variables, args.operation.as_deref())?;
    let errors = graphql_errors(&resp);

    if errors.is_empty() {
        let out = match global.output {
            OutputMode::Raw => resp,
            OutputMode::Default | OutputMode::Table => resp.get("data").cloned().unwrap_or(resp),
        };
        return write_response(global, &out);
    }

    // Failed (or partially failed) query: any data still goes to stdout, the
    // errors become the standard envelope on stderr, and the exit code is 2 —
    // GraphQL's in-band failure must not masquerade as success.
    if let Some(data) = resp.get("data").filter(|d| !d.is_null()) {
        let out = match global.output {
            OutputMode::Raw => resp.clone(),
            OutputMode::Default | OutputMode::Table => data.clone(),
        };
        write_response(global, &out)?;
    }
    Err(errors_to_api_error(errors))
}

/// POST one GraphQL request and return the raw response body
/// (`{"data": ..., "errors": [...]}`). Shared with `sn journal`.
pub(crate) fn execute(
    client: &crate::client::Client,
    query: &str,
    variables: Option<Value>,
    operation: Option<&str>,
) -> Result<Value> {
    let mut body = Map::new();
    body.insert("query".into(), Value::String(query.to_string()));
    if let Some(vars) = variables {
        body.insert("variables".into(), vars);
    }
    if let Some(op) = operation {
        body.insert("operationName".into(), Value::String(op.to_string()));
    }
    client.post("/api/now/graphql", &[], &Value::Object(body))
}

/// The `errors` array of a GraphQL response, empty when the query succeeded.
pub(crate) fn graphql_errors(resp: &Value) -> Vec<Value> {
    match resp.get("errors").and_then(Value::as_array) {
        Some(errs) => errs.clone(),
        None => Vec::new(),
    }
}

/// Map a non-empty `errors` array to the CLI's API error (exit 2). The HTTP
/// status genuinely was 200 — GraphQL reports failure in-band — so that is
/// what the envelope says; the full array rides in `sn_error`.
pub(crate) fn errors_to_api_error(errors: Vec<Value>) -> Error {
    let message = errors
        .first()
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("GraphQL query failed")
        .to_string();
    let detail = (errors.len() > 1).then(|| format!("{} errors in total", errors.len()));
    Error::Api {
        status: 200,
        message,
        detail,
        transaction_id: None,
        sn_error: Some(Value::Array(errors)),
    }
}

/// Resolve an inline / `@file` / `@-` spec to its text, unparsed.
fn read_text_input(raw: &str, what: &str) -> Result<String> {
    if raw == "@-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Error::Usage(format!("read {what} from stdin: {e}")))?;
        Ok(s)
    } else if let Some(path) = raw.strip_prefix('@') {
        fs::read_to_string(path).map_err(|e| Error::Usage(format!("read {path}: {e}")))
    } else {
        Ok(raw.to_string())
    }
}

/// Merge `--variables` (a JSON object) and repeated `--var k=v` (strings, which
/// win on conflict) into the request's `variables` value. `None` when neither
/// was given, so the request body omits the key entirely.
fn build_variables(variables: Option<&str>, vars: &[String]) -> Result<Option<Value>> {
    let mut map = match variables {
        Some(spec) => {
            let text = read_text_input(spec, "--variables")?;
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Usage(format!("--variables is not valid JSON: {e}")))?;
            match v {
                Value::Object(m) => m,
                _ => {
                    return Err(Error::Usage(
                        "--variables must be a JSON object at the top level".into(),
                    ));
                }
            }
        }
        None => Map::new(),
    };
    for spec in vars {
        let (k, v) = spec
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--var '{spec}' must be in key=value form")))?;
        if k.is_empty() {
            return Err(Error::Usage(format!("--var '{spec}' has empty key")));
        }
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok((!map.is_empty()).then_some(Value::Object(map)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn variables_none_when_nothing_given() {
        assert_eq!(build_variables(None, &[]).unwrap(), None);
    }

    #[test]
    fn var_flags_build_string_variables() {
        let v = build_variables(None, &["id=abc".into(), "q=active=true".into()])
            .unwrap()
            .unwrap();
        assert_eq!(v["id"], "abc");
        // Only the first '=' splits, so encoded queries pass through intact.
        assert_eq!(v["q"], "active=true");
    }

    #[test]
    fn var_overrides_variables_json() {
        let v = build_variables(Some(r#"{"id": "old", "limit": 5}"#), &["id=new".into()])
            .unwrap()
            .unwrap();
        assert_eq!(v["id"], "new");
        assert_eq!(v["limit"], 5);
    }

    #[test]
    fn variables_must_be_an_object() {
        let err = build_variables(Some("[1,2]"), &[]).unwrap_err();
        assert!(matches!(err, Error::Usage(_)));
    }

    #[test]
    fn malformed_var_is_usage_error() {
        for bad in ["novalue", "=v"] {
            let err = build_variables(None, &[bad.to_string()]).unwrap_err();
            assert!(matches!(err, Error::Usage(_)), "{bad} should be rejected");
        }
    }

    #[test]
    fn errors_map_to_api_exit_2_with_sn_error() {
        let errs = vec![
            json!({"message": "Validation error", "errorType": "ValidationError"}),
            json!({"message": "second"}),
        ];
        let err = errors_to_api_error(errs);
        assert_eq!(err.exit_code(), 2);
        match err {
            Error::Api {
                status,
                message,
                detail,
                sn_error,
                ..
            } => {
                assert_eq!(status, 200);
                assert_eq!(message, "Validation error");
                assert_eq!(detail.as_deref(), Some("2 errors in total"));
                assert_eq!(sn_error.unwrap().as_array().unwrap().len(), 2);
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn empty_errors_for_clean_response() {
        assert!(graphql_errors(&json!({"data": {"x": 1}})).is_empty());
        assert_eq!(
            graphql_errors(&json!({"data": null, "errors": [{"message": "m"}]})).len(),
            1
        );
    }
}
