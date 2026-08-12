//! `sn api` — instance API discovery over the REST API Explorer's own endpoints.
//!
//! The Explorer's Angular client reads the instance's whole API catalogue from
//! plain REST endpoints that accept ordinary credentials, so the same catalogue
//! is scriptable. They are **undocumented and unversioned** — not in any
//! OpenAPI spec ServiceNow publishes — and an old instance may simply not have
//! them, which is why [`name_endpoint`] rewrites a bare 404 into something that
//! names the path that was missing.

use crate::cli::table::{build_client_with_headers, build_profile, write_response};
use crate::cli::{GlobalFlags, OutputMode};
use crate::client::Client;
use crate::error::{Error, Result};
use clap::{Subcommand, ValueEnum};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use serde_json::{json, Map, Value};
use std::io::{self, Write};

/// Every namespace, API, version and endpoint on the instance in one response.
const DOC_PATH: &str = "/api/now/doc";
/// The same shape as [`DOC_PATH`] for a single namespace (`?namespace=<ns>`).
const SERVICES_PATH: &str = "/api/now/doc/services";
/// OpenAPI 3 export for one API (`?namespace=&name=&version=&format=`).
const OAS_PATH: &str = "/api/now/doc/oas_3";

#[derive(Subcommand, Debug)]
pub enum ApiSub {
    /// Summarize the instance's REST APIs: every namespace with counts, or one namespace's APIs.
    ///
    /// `--output raw` prints the catalogue endpoint's own response instead of the
    /// summary — several hundred kilobytes, for piping to jq.
    List(ApiListArgs),
    /// Find endpoints by substring across API names, routes and descriptions.
    ///
    /// One row per matching endpoint, carrying namespace, API name, version,
    /// method and route — enough to call it with `sn raw`. `--output raw` prints
    /// the unfiltered catalogue instead, as `sn api list` does.
    Search(ApiSearchArgs),
    /// Print one API's OpenAPI 3 specification.
    Spec(ApiSpecArgs),
}

#[derive(clap::Args, Debug, Default)]
pub struct ApiListArgs {
    /// List the APIs inside one namespace (e.g. `now`) instead of summarizing every namespace.
    #[arg(long, short = 'n')]
    pub namespace: Option<String>,
}

#[derive(clap::Args, Debug, Default)]
pub struct ApiSearchArgs {
    /// Case-insensitive substring, matched against namespace, API name, endpoint route and both descriptions.
    pub term: String,
    /// Restrict the search to one namespace, which also makes it fetch only that namespace.
    #[arg(long, short = 'n')]
    pub namespace: Option<String>,
    /// Only endpoints served by this HTTP method (e.g. `GET`). Case-insensitive.
    #[arg(long, short = 'm')]
    pub method: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct ApiSpecArgs {
    /// API name as `sn api list` reports it (e.g. `Table API`). Case-insensitive; a unique substring is enough.
    pub name: String,
    /// Namespace owning the API (e.g. `now`). Given, the name is used verbatim and no catalogue lookup happens.
    #[arg(long, short = 'n')]
    pub namespace: Option<String>,
    /// API version to export (e.g. `v2`). Defaults to whatever the instance calls latest.
    #[arg(long)]
    pub version: Option<String>,
    /// Serialization of the spec. `yaml` is written verbatim and ignores --pretty/--compact/--output.
    #[arg(long, value_enum, default_value_t = SpecFormat::Json)]
    pub format: SpecFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum SpecFormat {
    Json,
    Yaml,
}

pub fn list(global: &GlobalFlags, args: ApiListArgs) -> Result<()> {
    let client = catalogue_client(global)?;
    let resp = fetch_catalogue(&client, args.namespace.as_deref())?;
    if global.output == OutputMode::Raw {
        return write_response(global, &resp);
    }
    let cat = catalogue(&resp);
    let rows = match args.namespace.as_deref() {
        Some(ns) => {
            // `services` answers an unknown namespace with an empty object
            // rather than a 404, and a real namespace always holds at least one
            // API — so empty here means the name is wrong, and saying so beats
            // printing "(no records)".
            let apis = cat.get(ns).and_then(Value::as_object).ok_or_else(|| {
                Error::Usage(format!(
                    "no namespace '{ns}' on this instance; run `sn api list` for the full list"
                ))
            })?;
            apis.iter()
                .map(|(title, api)| api_row(ns, title, api))
                .collect()
        }
        None => cat
            .iter()
            .map(|(ns, apis)| namespace_row(ns, apis))
            .collect(),
    };
    write_response(global, &Value::Array(rows))
}

pub fn search(global: &GlobalFlags, args: ApiSearchArgs) -> Result<()> {
    let client = catalogue_client(global)?;
    let resp = fetch_catalogue(&client, args.namespace.as_deref())?;
    if global.output == OutputMode::Raw {
        return write_response(global, &resp);
    }
    let needle = args.term.to_lowercase();
    let wanted_method = args.method.as_deref().map(str::to_uppercase);
    let mut rows = Vec::new();
    for (ns, apis) in catalogue(&resp) {
        let Some(apis) = apis.as_object() else {
            continue;
        };
        for (title, api) in apis {
            // An API-level hit (its name or blurb) matches every endpoint it
            // serves: "is there an API for X" is answered by the endpoints, not
            // by the API's own row.
            let api_hit = contains(ns, &needle)
                || contains(title, &needle)
                || contains(str_field(api, "apiName"), &needle)
                || contains(str_field(api, "description"), &needle);
            for (version, resource) in resources(api) {
                if let Some(m) = &wanted_method {
                    if !str_field(resource, "httpMethod").eq_ignore_ascii_case(m) {
                        continue;
                    }
                }
                if !api_hit
                    && !contains(str_field(resource, "route"), &needle)
                    && !contains(str_field(resource, "description"), &needle)
                {
                    continue;
                }
                rows.push(json!({
                    "namespace": ns,
                    "name": title,
                    "api_name": str_field(api, "apiName"),
                    "version": version,
                    "method": str_field(resource, "httpMethod"),
                    "route": str_field(resource, "route"),
                    "description": str_field(resource, "description"),
                }));
            }
        }
    }
    write_response(global, &Value::Array(rows))
}

pub fn spec(global: &GlobalFlags, args: ApiSpecArgs) -> Result<()> {
    let client = catalogue_client(global)?;
    let (namespace, name) = match args.namespace {
        Some(ns) => (ns, args.name),
        None => resolve_api(&client, &args.name)?,
    };

    let mut query = vec![
        ("namespace".to_string(), namespace),
        ("name".to_string(), name),
        (
            "format".to_string(),
            match args.format {
                SpecFormat::Json => "json".to_string(),
                SpecFormat::Yaml => "yaml".to_string(),
            },
        ),
    ];
    if let Some(v) = args.version {
        query.push(("version".to_string(), v));
    }

    match args.format {
        SpecFormat::Json => {
            let spec = client
                .get(OAS_PATH, &query)
                .map_err(|e| name_endpoint(OAS_PATH, e))?;
            // No `result` envelope here — the export is the spec itself — so
            // `--output raw` and the default agree, and nothing is unwrapped.
            write_response(global, &spec)
        }
        SpecFormat::Yaml => {
            // YAML cannot go through the JSON emitter, so this is the one
            // response `sn api` writes to stdout itself, the way
            // `sn attachment download` does. `download_file` takes no query
            // parameters, hence the pre-encoded path.
            let (bytes, _content_type) = client
                .download_file(&path_with_query(OAS_PATH, &query))
                .map_err(|e| name_endpoint(OAS_PATH, e))?;
            let mut out = io::stdout().lock();
            out.write_all(&bytes)
                .map_err(crate::output::map_stdout_err)?;
            if !bytes.ends_with(b"\n") {
                out.write_all(b"\n")
                    .map_err(crate::output::map_stdout_err)?;
            }
            Ok(())
        }
    }
}

/// A client whose `Accept` is `*/*`.
///
/// Measured on a live instance: `oas_3` answers `Accept: application/json` with
/// **406** ("Supported response media types for this service are:
/// [application/octet-stream]"), so the client default cannot be used for the
/// spec export. The doc/services endpoints are content-negotiation-agnostic and
/// still return JSON under `*/*`, so one client serves all three verbs.
fn catalogue_client(global: &GlobalFlags) -> Result<Client> {
    let profile = build_profile(global)?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    build_client_with_headers(&profile, global.timeout, headers)
}

/// The whole catalogue, or one namespace's slice of it. Both endpoints answer
/// with the identical `{result: {namespace: {api_name: api}}}` shape, so every
/// consumer below is written once.
fn fetch_catalogue(client: &Client, namespace: Option<&str>) -> Result<Value> {
    match namespace {
        Some(ns) => client
            .get(SERVICES_PATH, &[("namespace".to_string(), ns.to_string())])
            .map_err(|e| name_endpoint(SERVICES_PATH, e)),
        None => client
            .get(DOC_PATH, &[])
            .map_err(|e| name_endpoint(DOC_PATH, e)),
    }
}

/// Rewrite a 404 to name the endpoint that is missing.
///
/// These paths are internal to the REST API Explorer and unversioned; an
/// instance old enough to lack them answers with ServiceNow's generic
/// "Requested URI does not represent any resource", which points at nothing a
/// caller could act on. The original body survives in `detail`/`sn_error`.
fn name_endpoint(path: &str, err: Error) -> Error {
    match err {
        Error::Api {
            status: 404,
            message,
            detail,
            transaction_id,
            sn_error,
        } => Error::Api {
            status: 404,
            message: format!(
                "{path} not found on this instance ({message}); \
                 it is an undocumented REST API Explorer endpoint and may be absent on older releases"
            ),
            detail,
            transaction_id,
            sn_error,
        },
        other => other,
    }
}

fn catalogue(resp: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    resp.get("result")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// `{namespace, apis, endpoints}` — the compact summary `sn api list` prints
/// instead of the multi-hundred-kilobyte catalogue it derives it from.
fn namespace_row(namespace: &str, apis: &Value) -> Value {
    let apis = apis.as_object();
    let endpoints: usize = apis
        .map(|m| m.values().map(|api| resources(api).count()).sum())
        .unwrap_or(0);
    json!({
        "namespace": namespace,
        "apis": apis.map(Map::len).unwrap_or(0),
        "endpoints": endpoints,
    })
}

/// One API as `sn api list --namespace` reports it. `name` is what
/// `sn api spec` takes, so the two verbs compose without a lookup table.
fn api_row(namespace: &str, name: &str, api: &Value) -> Value {
    let versions: Vec<&str> = api
        .get("versions")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    json!({
        "namespace": namespace,
        "name": name,
        "api_name": str_field(api, "apiName"),
        "versions": versions,
        "endpoints": resources(api).count(),
        "scripted": api.get("scripted").and_then(Value::as_bool).unwrap_or(false),
        "description": str_field(api, "description"),
    })
}

/// Every `(version, resource)` pair an API advertises, flattened across its
/// version map. Versions are BTreeMap-ordered, so the walk is byte-stable.
fn resources(api: &Value) -> impl Iterator<Item = (&str, &Value)> {
    api.get("versions")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|versions| {
            versions.iter().flat_map(|(version, v)| {
                v.get("resources")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .map(move |r| (version.as_str(), r))
            })
        })
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn contains(haystack: &str, lowercase_needle: &str) -> bool {
    haystack.to_lowercase().contains(lowercase_needle)
}

/// Find the `(namespace, name)` that `oas_3` wants for a name given on its own.
///
/// The export endpoint matches `name` exactly against the catalogue key and
/// 404s otherwise, so `sn api spec "table api"` would fail without this. An
/// exact (case-insensitive) hit on the catalogue key or on `apiName` wins
/// outright; otherwise a substring must identify exactly one API, because
/// silently exporting the first of several is worse than being told which ones
/// matched.
fn resolve_api(client: &Client, name: &str) -> Result<(String, String)> {
    let resp = fetch_catalogue(client, None)?;
    let needle = name.to_lowercase();
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for (ns, apis) in catalogue(&resp) {
        let Some(apis) = apis.as_object() else {
            continue;
        };
        for (title, api) in apis {
            let api_name = str_field(api, "apiName");
            let hit = (ns.clone(), title.clone());
            if title.to_lowercase() == needle || api_name.to_lowercase() == needle {
                exact.push(hit);
            } else if contains(title, &needle) || contains(api_name, &needle) {
                partial.push(hit);
            }
        }
    }
    let matches = if exact.is_empty() { partial } else { exact };
    match matches.len() {
        1 => Ok(matches.into_iter().next().expect("length checked")),
        0 => Err(Error::Usage(format!(
            "no API matching '{name}'; run `sn api search '{name}'` or `sn api list`"
        ))),
        _ => Err(Error::Usage(format!(
            "'{name}' matches {} APIs: {}; name one of them exactly, or narrow with --namespace",
            matches.len(),
            matches
                .iter()
                .map(|(ns, title)| format!("{ns}/{title}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// `path?k=v&…`, form-encoded, for the binary fetch that carries no query
/// parameters of its own. Built through `Url` rather than by hand so a space or
/// `&` in an API name cannot break out of its parameter.
fn path_with_query(path: &str, query: &[(String, String)]) -> String {
    let mut url = reqwest::Url::parse("https://encoder.invalid/").expect("static URL parses");
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    match url.query() {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_api() -> Value {
        json!({
            "apiName": "now/table",
            "description": "CRUD on existing tables",
            "scripted": false,
            "svcName": "Table API",
            "versions": {
                "v1": {"resources": [
                    {"description": "Query records", "httpMethod": "GET", "route": "/now/v1/table/{tableName}"}
                ]},
                "v2": {"resources": [
                    {"description": "Query records", "httpMethod": "GET", "route": "/now/v2/table/{tableName}"},
                    {"description": "Create a record", "httpMethod": "POST", "route": "/now/v2/table/{tableName}"}
                ]}
            }
        })
    }

    #[test]
    fn resources_flatten_every_version_in_stable_order() {
        let api = table_api();
        let seen: Vec<(&str, &str)> = resources(&api)
            .map(|(v, r)| (v, str_field(r, "httpMethod")))
            .collect();
        assert_eq!(seen, vec![("v1", "GET"), ("v2", "GET"), ("v2", "POST")]);
    }

    #[test]
    fn api_row_reports_versions_and_endpoint_count() {
        let row = api_row("now", "Table API", &table_api());
        assert_eq!(row["name"], "Table API");
        assert_eq!(row["api_name"], "now/table");
        assert_eq!(row["endpoints"], 3);
        assert_eq!(row["versions"], json!(["v1", "v2"]));
        assert_eq!(row["scripted"], false);
    }

    #[test]
    fn namespace_row_counts_apis_and_their_endpoints() {
        let apis = json!({"Table API": table_api(), "Other": table_api()});
        let row = namespace_row("now", &apis);
        assert_eq!(row["apis"], 2);
        assert_eq!(row["endpoints"], 6);
    }

    #[test]
    fn missing_or_malformed_fields_never_panic() {
        // The catalogue is undocumented: a version block without `resources`,
        // or an API that is not an object, must degrade rather than abort.
        let api = json!({"versions": {"latest": {}}});
        assert_eq!(resources(&api).count(), 0);
        assert_eq!(namespace_row("x", &json!("not an object"))["apis"], 0);
        assert_eq!(api_row("x", "y", &json!({}))["endpoints"], 0);
        assert!(catalogue(&json!({"result": "nope"})).is_empty());
    }

    #[test]
    fn query_values_are_encoded_not_interpolated() {
        let q = vec![
            ("name".to_string(), "Table API & more".to_string()),
            ("namespace".to_string(), "now".to_string()),
        ];
        let s = path_with_query(OAS_PATH, &q);
        assert!(s.starts_with("/api/now/doc/oas_3?"), "{s}");
        assert!(!s.contains("Table API"), "space left unencoded: {s}");
        assert!(
            s.contains("%26") || s.contains("+%26+"),
            "'&' must not split the parameter: {s}"
        );
        assert!(s.ends_with("&namespace=now"), "{s}");
    }

    #[test]
    fn empty_query_leaves_the_path_alone() {
        assert_eq!(path_with_query(OAS_PATH, &[]), OAS_PATH);
    }

    #[test]
    fn a_404_names_the_endpoint_but_keeps_the_original_body() {
        let err = name_endpoint(
            DOC_PATH,
            Error::Api {
                status: 404,
                message: "Requested URI does not represent any resource".into(),
                detail: Some("original detail".into()),
                transaction_id: Some("tx1".into()),
                sn_error: Some(json!({"message": "x"})),
            },
        );
        let Error::Api {
            message,
            detail,
            transaction_id,
            ..
        } = &err
        else {
            panic!("expected an API error, got {err:?}");
        };
        assert!(message.contains(DOC_PATH), "{message}");
        assert!(message.contains("older releases"), "{message}");
        assert_eq!(detail.as_deref(), Some("original detail"));
        assert_eq!(transaction_id.as_deref(), Some("tx1"));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn non_404_errors_pass_through_untouched() {
        let err = name_endpoint(
            DOC_PATH,
            Error::Api {
                status: 500,
                message: "boom".into(),
                detail: None,
                transaction_id: None,
                sn_error: None,
            },
        );
        let Error::Api { message, .. } = &err else {
            panic!("expected an API error");
        };
        assert_eq!(message, "boom");
    }
}
