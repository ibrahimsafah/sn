use crate::body::{self, EmptyBody};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::kernel::{build_client_with_headers, build_profile, write_response};

#[derive(clap::Args, Debug)]
pub struct RawArgs {
    /// HTTP method (GET, POST, PUT, PATCH, DELETE). Case-insensitive.
    pub method: String,
    /// Path on the instance, e.g. `/api/now/table/incident` or `/api/now/v2/table/incident/abc123`.
    pub path: String,
    /// Repeatable query parameter as `key=value` (e.g. `--query sysparm_limit=5`).
    #[arg(long = "query", short = 'q')]
    pub query: Vec<String>,
    /// Repeatable request header as `Name: Value` (e.g. `-H 'X-no-response-body: true'`).
    /// Overrides the header the client would otherwise send, `Content-Type` included;
    /// repeating a name sends it twice. `Authorization` is rejected — identity comes
    /// from the profile. Format is still JSON both ways: a non-JSON response fails to
    /// parse, and `--data`/`--field` always serialize JSON, so `Accept`/`Content-Type`
    /// cannot select another encoding.
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    pub header: Vec<String>,
    /// Request body source: inline JSON, @file (path), or @- (stdin). Required for POST/PUT/PATCH if --field is not given.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file for value from file. Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

pub fn run(global: &GlobalFlags, args: RawArgs) -> Result<()> {
    let method = args.method.to_uppercase();
    let q = parse_query(&args.query)?;
    // Parsed before the profile is resolved: a malformed header is a usage error
    // and should not depend on config being loadable.
    let headers = parse_headers(&args.header)?;

    let profile = build_profile(global)?;
    let client = build_client_with_headers(&profile, global.timeout, headers)?;

    let resp: Value = match method.as_str() {
        "GET" => {
            ensure_no_body(&args.data, &args.field)?;
            client.get(&args.path, &q)?
        }
        "DELETE" => {
            ensure_no_body(&args.data, &args.field)?;
            client.delete_json(&args.path, &q)?
        }
        "POST" | "PUT" | "PATCH" => {
            let body = build_request_body(args.data.clone(), args.field.clone())?;
            match method.as_str() {
                "POST" => client.post(&args.path, &q, &body)?,
                "PUT" => client.put(&args.path, &q, &body)?,
                "PATCH" => client.patch(&args.path, &q, &body)?,
                _ => unreachable!(),
            }
        }
        other => {
            return Err(Error::Usage(format!(
                "unknown method: {other}, expected GET/POST/PUT/PATCH/DELETE"
            )));
        }
    };

    write_response(global, &resp)
}

fn parse_query(raw: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--query '{item}' must be in key=value form")))?;
        if k.is_empty() {
            return Err(Error::Usage(format!("--query '{item}' has empty key")));
        }
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// Parse curl-style `--header 'Name: Value'` arguments into a `HeaderMap`.
///
/// Splits on the FIRST `:` so a value may contain colons (`X-Url: http://x`),
/// trims whitespace around the name and leading whitespace off the value, and
/// validates both through `http`'s own parsers so a malformed header is a usage
/// error (exit 1) rather than a panic deep in reqwest. Repeating a name appends
/// (valid HTTP, and what curl does).
///
/// `Authorization` is refused: a profile is the whole unit of identity in this
/// CLI, and a credential in argv is visible to `ps` and shell history.
fn parse_headers(raw: &[String]) -> Result<HeaderMap> {
    let mut out = HeaderMap::new();
    for item in raw {
        let (name, value) = item.split_once(':').ok_or_else(|| {
            Error::Usage(format!("--header '{item}' must be in 'Name: Value' form"))
        })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Usage(format!("--header '{item}' has empty name")));
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| Error::Usage(format!("--header '{item}' has invalid name: {e}")))?;
        if name == AUTHORIZATION {
            return Err(Error::Usage(
                "--header cannot set Authorization: credentials come from the selected \
                 profile — configure one with `sn init` or `sn profile add`"
                    .into(),
            ));
        }
        let value = HeaderValue::from_str(value.trim_start())
            .map_err(|e| Error::Usage(format!("--header '{item}' has invalid value: {e}")))?;
        out.append(name, value);
    }
    Ok(out)
}

fn ensure_no_body(data: &Option<String>, field: &[String]) -> Result<()> {
    if data.is_some() || !field.is_empty() {
        return Err(Error::Usage(
            "--data/--field not allowed for GET/DELETE".into(),
        ));
    }
    Ok(())
}

fn build_request_body(data: Option<String>, field: Vec<String>) -> Result<Value> {
    body::from_flags(data, field, EmptyBody::Object)
}

#[cfg(test)]
mod tests {
    use super::parse_headers;
    use crate::error::Error;

    fn parse(items: &[&str]) -> Result<reqwest::header::HeaderMap, Error> {
        parse_headers(&items.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn usage_message(items: &[&str]) -> String {
        match parse(items) {
            Err(Error::Usage(m)) => m,
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn parses_name_and_value() {
        let h = parse(&["X-no-response-body: true"]).unwrap();
        assert_eq!(h["x-no-response-body"], "true");
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn missing_colon_is_usage_error() {
        let m = usage_message(&["X-Broken"]);
        assert!(m.contains("'Name: Value'"), "message was: {m}");
    }

    #[test]
    fn empty_name_is_usage_error() {
        assert!(usage_message(&[": value"]).contains("empty name"));
        assert!(usage_message(&["   : value"]).contains("empty name"));
    }

    #[test]
    fn leading_space_is_trimmed_from_the_value() {
        // The conventional `Name: Value` spelling must not send " Value".
        let h = parse(&["X-Trim:   spaced"]).unwrap();
        assert_eq!(h["x-trim"], "spaced");
        // No separator space is equally fine.
        assert_eq!(parse(&["X-Trim:tight"]).unwrap()["x-trim"], "tight");
    }

    #[test]
    fn repeated_names_append_rather_than_replace() {
        let h = parse(&["X-Multi: a", "X-Multi: b"]).unwrap();
        let values: Vec<&str> = h
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(values, vec!["a", "b"]);
    }

    #[test]
    fn only_the_first_colon_splits() {
        // A value may contain colons — URLs, timestamps, `key:value` payloads.
        let h = parse(&["X-Link: https://example.service-now.com/x"]).unwrap();
        assert_eq!(h["x-link"], "https://example.service-now.com/x");
    }

    #[test]
    fn authorization_is_rejected_case_insensitively() {
        for item in [
            "Authorization: Basic abc",
            "authorization: Basic abc",
            "AUTHORIZATION: Basic abc",
        ] {
            let m = usage_message(&[item]);
            assert!(m.contains("Authorization"), "message was: {m}");
            assert!(m.contains("sn profile add"), "message was: {m}");
            assert!(m.contains("sn init"), "message was: {m}");
        }
    }

    #[test]
    fn malformed_name_or_value_is_a_usage_error_not_a_panic() {
        assert!(usage_message(&["Bad Name: v"]).contains("invalid name"));
        assert!(usage_message(&["X-Ctl: bad\u{7f}value"]).contains("invalid value"));
    }

    #[test]
    fn no_headers_yields_an_empty_map() {
        assert!(parse(&[]).unwrap().is_empty());
    }
}
