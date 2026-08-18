//! `sn api` — instance API discovery over the REST API Explorer's own endpoints.
//!
//! The Explorer's Angular client reads the instance's whole API catalogue from
//! plain REST endpoints that accept ordinary credentials, so the same catalogue
//! is scriptable. They are **undocumented and unversioned** — not in any
//! OpenAPI spec ServiceNow publishes — and an old instance may simply not have
//! them.
//!
//! That last possibility is *not* what most 404s here mean. Measured on a live
//! instance, `oas_3` answers a bad argument with a 404 whose body names it
//! precisely — "API Table API xyz not found in namespace now", "Version v99 not
//! found for now/Table API", "Namespace nowzz not found for any available APIs"
//! — while a missing endpoint family gives ServiceNow's generic "Requested URI
//! does not represent any resource". So [`diagnose`] proves which case it is
//! before claiming either: `/api/now/doc/namespaces` is the cheapest member of
//! the family (~1.5 KB against 460 KB for the full catalogue), and its answering
//! at all settles that the family is present, leaving the endpoint's own detail
//! as the honest explanation.

use crate::cli::kernel::{build_client_with_headers, build_profile, write_response};
use crate::cli::{GlobalFlags, OutputMode};
use crate::client::{Client, DownloadError};
use crate::error::{Error, Result};
use clap::{Subcommand, ValueEnum};
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use serde_json::{Map, Value, json};
use std::io::{self, Write};

/// Every namespace, API, version and endpoint on the instance in one response.
const DOC_PATH: &str = "/api/now/doc";
/// The same shape as [`DOC_PATH`] for a single namespace (`?namespace=<ns>`).
const SERVICES_PATH: &str = "/api/now/doc/services";
/// OpenAPI 3 export for one API (`?namespace=&name=&version=&format=`).
const OAS_PATH: &str = "/api/now/doc/oas_3";
/// Every namespace name on the instance, as a flat array — the cheapest member
/// of the family, used both to prove it exists and to spell-check `--namespace`.
/// It is a *superset* of the catalogue's keys: it also names scopes that publish
/// no REST API at all (4 of 94 on the reference instance).
const NAMESPACES_PATH: &str = "/api/now/doc/namespaces";

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
    /// Restrict the search to one namespace, which also makes it fetch only that namespace. Unknown names are an error.
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
    /// Namespace owning the API (e.g. `now`). Narrows the same name matching to one namespace, and fetches only it.
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
            // `fetch_catalogue` has already rejected a namespace that resolves
            // to nothing, so a miss here can only be the endpoint keying its
            // answer differently than it was asked — take it as authoritative
            // rather than inventing a second unknown-namespace path.
            let (key, apis) = namespace_entry(cat, ns)
                .ok_or_else(|| unknown_namespace(&client, ns))
                .and_then(|(key, apis)| match apis.as_object() {
                    Some(apis) => Ok((key.as_str(), apis)),
                    None => Err(unknown_namespace(&client, ns)),
                })?;
            apis.iter()
                .map(|(title, api)| api_row(key, title, api))
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
                if let Some(m) = &wanted_method
                    && !str_field(resource, "httpMethod").eq_ignore_ascii_case(m)
                {
                    continue;
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
    let (namespace, name) = resolve_api(&client, &args.name, args.namespace.as_deref())?;

    // The name and namespace came out of the catalogue, so an `oas_3` 404 is
    // about the one parameter that was never checked against it.
    let hint = format!("; `sn api list --namespace {namespace}` reports each API's versions");
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
                .map_err(|e| diagnose(&client, OAS_PATH, e, Some(&hint)))?;
            // No `result` envelope here — the export is the spec itself — so
            // `--output raw` and the default agree, and nothing is unwrapped.
            write_response(global, &spec)
        }
        SpecFormat::Yaml => {
            // YAML cannot go through the JSON emitter, so this is the one
            // response `sn api` writes to stdout itself, the way
            // `sn attachment download` does. `download_file` takes no query
            // parameters, hence the pre-encoded path.
            let mut download = client
                .download_file(&path_with_query(OAS_PATH, &query))
                .map_err(|e| diagnose(&client, OAS_PATH, e, Some(&hint)))?;
            // Buffered rather than streamed to stdout, unlike `sn attachment
            // download`: a spec is a bounded document (the instance's *entire*
            // catalogue is ~460 KB) and the JSON arm above already holds one
            // parsed in memory, so there is no attachment-sized payload to guard
            // against — and holding it keeps the trailing-newline fixup below a
            // question about the last byte rather than about writer state.
            let mut bytes = Vec::new();
            download.copy_to(&mut bytes).map_err(|e| match e {
                DownloadError::Source(err) => err,
                // A `Vec` sink cannot refuse bytes; kept as a real arm rather
                // than an `unreachable!` so a future change of sink is a
                // compile-time concern, not a panic.
                DownloadError::Sink(err) => Error::Transport(err.to_string()),
            })?;
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
///
/// A namespace that resolves to nothing is an error here rather than at each
/// call site, so `list`, `search` and `spec` cannot disagree about what an empty
/// `{"result":{}}` means: `services` answers an unknown namespace with exactly
/// that, under HTTP 200, so silently returning "no matches" would make a typo
/// indistinguishable from a real empty result.
fn fetch_catalogue(client: &Client, namespace: Option<&str>) -> Result<Value> {
    match namespace {
        Some(ns) => {
            let resp = client
                .get(SERVICES_PATH, &[("namespace".to_string(), ns.to_string())])
                .map_err(|e| diagnose(client, SERVICES_PATH, e, None))?;
            if catalogue(&resp).is_empty() {
                return Err(unknown_namespace(client, ns));
            }
            Ok(resp)
        }
        None => client
            .get(DOC_PATH, &[])
            .map_err(|e| diagnose(client, DOC_PATH, e, None)),
    }
}

/// Every namespace name the instance knows, catalogued or not.
fn fetch_namespaces(client: &Client) -> Result<Vec<String>> {
    let resp = client.get(NAMESPACES_PATH, &[])?;
    Ok(resp
        .get("result")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Why a `--namespace` came back empty, asked of the instance rather than
/// assumed: the namespace list distinguishes "you misspelled it" (with
/// near-matches to spell it right) from "it exists but publishes no REST API",
/// which are the same empty object on the wire.
fn unknown_namespace(client: &Client, ns: &str) -> Error {
    let Ok(all) = fetch_namespaces(client) else {
        return Error::Usage(format!(
            "no APIs in namespace '{ns}' on this instance; run `sn api list` for the namespaces that publish them"
        ));
    };
    if all.iter().any(|n| n == ns) {
        return Error::Usage(format!(
            "namespace '{ns}' exists on this instance but publishes no REST API; run `sn api list` for the namespaces that do"
        ));
    }
    let near = near_matches(ns, &all);
    let did_you_mean = if near.is_empty() {
        String::new()
    } else {
        format!(" — did you mean {}?", quoted(&near))
    };
    Error::Usage(format!(
        "no namespace '{ns}' on this instance{did_you_mean}; run `sn api list` for the namespaces that publish APIs"
    ))
}

/// Turn a 404 from one of the doc endpoints into what actually went wrong.
///
/// The two causes are indistinguishable in the status alone: the endpoint
/// family may be absent (it is undocumented and unversioned, so an old instance
/// may never have had it), or it may be present and rejecting an argument. So
/// this *proves* which before saying either — [`NAMESPACES_PATH`] answering at
/// all establishes the family, and its own 404 establishes the absence. Only
/// that proof licenses the "older releases" wording; anything less falls back on
/// the endpoint's `detail`, which names the bad argument outright. The original
/// body survives in `detail`/`sn_error` either way.
fn diagnose(client: &Client, path: &str, err: Error, hint: Option<&str>) -> Error {
    if !matches!(err, Error::Api { status: 404, .. }) {
        return err;
    }
    // A probe that fails some *other* way (network, 500) proves nothing, so it
    // is not taken as evidence of absence.
    let family_missing = matches!(
        fetch_namespaces(client),
        Err(Error::Api { status: 404, .. })
    );
    rewrite_404(path, err, family_missing, hint)
}

fn rewrite_404(path: &str, err: Error, family_missing: bool, hint: Option<&str>) -> Error {
    let Error::Api {
        status: 404,
        message,
        detail,
        transaction_id,
        sn_error,
    } = err
    else {
        return err;
    };
    let message = if family_missing {
        format!(
            "{path} not found on this instance ({message}); \
             it is an undocumented REST API Explorer endpoint and may be absent on older releases"
        )
    } else {
        // The family is there, so the 404 is about the request. ServiceNow puts
        // the reason in `detail` ("Version v99 not found for now/Table API");
        // promote it rather than inventing a diagnosis over the top of it.
        let what = detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or(message.as_str());
        format!("{path}: {what}{}", hint.unwrap_or(""))
    };
    Error::Api {
        status: 404,
        message,
        detail,
        transaction_id,
        sn_error,
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

/// How a name was matched, strongest first. The order is what keeps the
/// ambiguity advice below honest — see [`ambiguous`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    /// Equal to the catalogue key, which is unique inside a namespace.
    Title,
    /// Equal to `apiName` (`now/table`), which embeds its own namespace.
    ApiName,
    /// Case-insensitive substring of either.
    Partial,
}

/// Find the `(namespace, name)` that `oas_3` wants.
///
/// The export endpoint matches `name` exactly against the catalogue key and
/// 404s otherwise, so `sn api spec "table api"` would fail without this — and
/// `--namespace` narrows the identical matching rather than switching it off,
/// so the substring form the `<NAME>` help advertises works with or without it.
/// A substring must identify exactly one API, because silently exporting the
/// first of several is worse than being told which ones matched.
fn resolve_api(client: &Client, name: &str, namespace: Option<&str>) -> Result<(String, String)> {
    let resp = fetch_catalogue(client, namespace)?;
    let cat = catalogue(&resp);
    let (tier, mut matches) = match_apis(cat, name);
    match matches.len() {
        1 => Ok(matches.pop().expect("length checked")),
        0 => Err(no_such_api(cat, name, namespace)),
        _ => Err(ambiguous(tier, &matches, name)),
    }
}

/// Every API whose title or `apiName` answers to `name`, in the strongest tier
/// that matched anything.
fn match_apis(cat: &Map<String, Value>, name: &str) -> (Tier, Vec<(String, String)>) {
    let needle = name.to_lowercase();
    let (mut title, mut api_name, mut partial) = (Vec::new(), Vec::new(), Vec::new());
    for (ns, apis) in cat {
        let Some(apis) = apis.as_object() else {
            continue;
        };
        for (t, api) in apis {
            let an = str_field(api, "apiName");
            let hit = (ns.clone(), t.clone());
            if t.to_lowercase() == needle {
                title.push(hit);
            } else if an.to_lowercase() == needle {
                api_name.push(hit);
            } else if contains(t, &needle) || contains(an, &needle) {
                partial.push(hit);
            }
        }
    }
    // Title beats apiName rather than tying with it: that is what makes "use
    // the catalogue title" a resolution and not a restatement of the problem.
    if !title.is_empty() {
        (Tier::Title, title)
    } else if !api_name.is_empty() {
        (Tier::ApiName, api_name)
    } else {
        (Tier::Partial, partial)
    }
}

fn no_such_api(cat: &Map<String, Value>, name: &str, namespace: Option<&str>) -> Error {
    let titles: Vec<String> = cat
        .values()
        .filter_map(Value::as_object)
        .flat_map(|apis| apis.keys().cloned())
        .collect();
    let near = near_matches(name, &titles);
    let did_you_mean = if near.is_empty() {
        String::new()
    } else {
        format!(" — did you mean {}?", quoted(&near))
    };
    let scope = match namespace {
        Some(ns) => format!(" in namespace '{ns}'"),
        None => String::new(),
    };
    Error::Usage(format!(
        "no API matching '{name}'{scope}{did_you_mean}; run `sn api search '{name}'` or `sn api list`"
    ))
}

/// The advice here has to be one the caller can actually carry out. "Name it
/// exactly" cannot break an *exact* tie, and such a tie can only span
/// namespaces (a catalogue key is unique within one), so that case is sent to
/// `--namespace` instead. The one same-namespace exact tie possible — one API's
/// title colliding with another's `apiName` — is resolved by the title tier
/// outranking the `apiName` tier, so naming the title really does settle it.
fn ambiguous(tier: Tier, matches: &[(String, String)], name: &str) -> Error {
    let list = matches
        .iter()
        .map(|(ns, title)| format!("{ns}/{title}"))
        .collect::<Vec<_>>()
        .join(", ");
    let first_ns = &matches[0].0;
    let mut namespaces: Vec<&str> = matches.iter().map(|(ns, _)| ns.as_str()).collect();
    namespaces.dedup();
    let advice = match (namespaces.len() > 1, tier) {
        (true, Tier::Partial) => format!(
            "name one of them exactly, or narrow with `--namespace` (one of: {})",
            namespaces.join(", ")
        ),
        (true, _) => format!(
            "an exact name cannot break a tie across namespaces — narrow with `--namespace` (one of: {})",
            namespaces.join(", ")
        ),
        (false, Tier::Partial) => "name one of them exactly".to_string(),
        (false, _) => format!(
            "use the catalogue title from `sn api list --namespace {first_ns}`, which wins over a matching apiName"
        ),
    };
    Error::Usage(format!(
        "'{name}' matches {} APIs: {list}; {advice}",
        matches.len()
    ))
}

/// The entry for `ns`, tolerating the endpoint keying its answer in a different
/// case than the request was written in.
fn namespace_entry<'c>(cat: &'c Map<String, Value>, ns: &str) -> Option<(&'c String, &'c Value)> {
    cat.get_key_value(ns)
        .or_else(|| cat.iter().find(|(k, _)| k.eq_ignore_ascii_case(ns)))
}

fn quoted(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Up to three plausible spellings of `needle` drawn from `candidates`.
///
/// A fragment of the real name ranks first — after an exact match has already
/// failed, "chg" for `sn_chg_rest` is a stronger signal than any distance — then
/// anything within a length-scaled edit budget. Names are matched whole and
/// word-by-word, so `tabel` still finds `Table API`.
fn near_matches(needle: &str, candidates: &[String]) -> Vec<String> {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let budget = (1 + needle.chars().count() / 4).min(3);
    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .filter_map(|c| {
            let lc = c.to_lowercase();
            if lc.contains(&needle) || needle.contains(&lc) {
                return Some((0, c));
            }
            let d = lc
                .split_whitespace()
                .chain(std::iter::once(lc.as_str()))
                .map(|word| edit_distance(&needle, word))
                .min()
                .unwrap_or(usize::MAX);
            (d <= budget).then_some((d, c))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.truncate(3);
    scored.into_iter().map(|(_, c)| c.clone()).collect()
}

/// Optimal string alignment distance: Levenshtein plus adjacent transposition,
/// so `nwo` is one slip from `now` rather than two. Hand-rolled because the
/// alternative is a new dependency for twelve lines; unlike the implied-verb
/// rewrite in `cli::parse`, nothing here has to agree with clap's own metric.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
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

    fn not_found(detail: Option<&str>) -> Error {
        Error::Api {
            status: 404,
            message: "Not Found".into(),
            detail: detail.map(str::to_string),
            transaction_id: Some("tx1".into()),
            sn_error: Some(json!({"message": "x"})),
        }
    }

    #[test]
    fn a_404_blames_the_instance_only_when_the_endpoint_family_is_gone() {
        let err = rewrite_404(DOC_PATH, not_found(Some("original detail")), true, None);
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
        // The original body survives whichever way it is explained.
        assert_eq!(detail.as_deref(), Some("original detail"));
        assert_eq!(transaction_id.as_deref(), Some("tx1"));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn a_404_from_a_live_endpoint_family_reports_the_endpoints_own_reason() {
        let err = rewrite_404(
            OAS_PATH,
            not_found(Some("Version v99 not found for now/Table API")),
            false,
            Some("; try `sn api list --namespace now`"),
        );
        let Error::Api { message, .. } = &err else {
            panic!("expected an API error");
        };
        assert!(
            message.contains("Version v99 not found for now/Table API"),
            "{message}"
        );
        assert!(message.contains("sn api list --namespace now"), "{message}");
        assert!(
            !message.contains("older releases"),
            "a reachable endpoint family must not be blamed on the release: {message}"
        );
    }

    #[test]
    fn a_404_without_a_detail_falls_back_to_the_status_message() {
        let err = rewrite_404(OAS_PATH, not_found(None), false, None);
        let Error::Api { message, .. } = &err else {
            panic!("expected an API error");
        };
        assert_eq!(message, "/api/now/doc/oas_3: Not Found");
    }

    #[test]
    fn non_404_errors_pass_through_untouched() {
        let err = rewrite_404(
            DOC_PATH,
            Error::Api {
                status: 500,
                message: "boom".into(),
                detail: None,
                transaction_id: None,
                sn_error: None,
            },
            true,
            None,
        );
        let Error::Api { message, .. } = &err else {
            panic!("expected an API error");
        };
        assert_eq!(message, "boom");
    }

    fn two_namespaces() -> Map<String, Value> {
        let cat = json!({
            "now": {"Table API": {"apiName": "now/table"}, "Attachment API": {"apiName": "now/attachment"}},
            "sn_chg_rest": {"Attachment API": {"apiName": "sn_chg_rest/attachment"}},
        });
        cat.as_object().cloned().expect("object")
    }

    #[test]
    fn an_exact_title_outranks_a_matching_api_name() {
        // One API is titled "now/table"; another *is* now/table by apiName. The
        // title tier settles it, which is what makes the ambiguity advice below
        // ("use the catalogue title") something the caller can act on.
        let cat = json!({"now": {
            "now/table": {"apiName": "now/legacy_table"},
            "Table API": {"apiName": "now/table"},
        }});
        let (tier, matches) = match_apis(cat.as_object().unwrap(), "NOW/TABLE");
        assert_eq!(tier, Tier::Title);
        assert_eq!(matches, vec![("now".to_string(), "now/table".to_string())]);
    }

    #[test]
    fn an_exact_tie_across_namespaces_is_told_to_use_namespace_not_a_better_name() {
        let (tier, matches) = match_apis(&two_namespaces(), "attachment api");
        assert_eq!(tier, Tier::Title);
        assert_eq!(matches.len(), 2);
        let Error::Usage(msg) = ambiguous(tier, &matches, "attachment api") else {
            panic!("expected a usage error");
        };
        assert!(msg.contains("--namespace"), "{msg}");
        assert!(msg.contains("now, sn_chg_rest"), "{msg}");
        // Naming an already-exact name again would change nothing, so the
        // message must not ask for it.
        assert!(!msg.contains("exactly, or"), "{msg}");
    }

    #[test]
    fn a_substring_tie_may_still_be_resolved_by_naming_one_exactly() {
        let (tier, matches) = match_apis(&two_namespaces(), "att");
        assert_eq!(tier, Tier::Partial);
        let Error::Usage(msg) = ambiguous(tier, &matches, "att") else {
            panic!("expected a usage error");
        };
        assert!(msg.contains("name one of them exactly"), "{msg}");
        assert!(msg.contains("--namespace"), "{msg}");
    }

    #[test]
    fn a_namespace_narrows_the_same_matching_rather_than_disabling_it() {
        let mut cat = two_namespaces();
        cat.remove("sn_chg_rest");
        let (tier, matches) = match_apis(&cat, "att");
        assert_eq!(tier, Tier::Partial);
        assert_eq!(
            matches,
            vec![("now".to_string(), "Attachment API".to_string())]
        );
    }

    #[test]
    fn near_matches_catch_a_typo_a_transposition_and_a_fragment() {
        let all: Vec<String> = ["now", "sn_chg_rest", "sn_sc", "global"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(near_matches("nwo", &all), vec!["now".to_string()]);
        assert_eq!(near_matches("chg", &all), vec!["sn_chg_rest".to_string()]);
        assert_eq!(
            near_matches("sn_chg_res", &all),
            vec!["sn_chg_rest".to_string()]
        );
        assert!(near_matches("zzzzzzzzzz", &all).is_empty());
        assert!(near_matches("", &all).is_empty());
    }

    #[test]
    fn near_matches_look_inside_a_multi_word_api_name() {
        let titles = vec!["Table API".to_string(), "Attachment API".to_string()];
        assert_eq!(
            near_matches("tabel", &titles),
            vec!["Table API".to_string()]
        );
    }

    #[test]
    fn edit_distance_counts_a_transposition_once() {
        assert_eq!(edit_distance("now", "now"), 0);
        assert_eq!(edit_distance("nwo", "now"), 1);
        assert_eq!(edit_distance("tabel", "table"), 1);
        assert_eq!(edit_distance("", "now"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn namespace_entry_tolerates_a_differently_cased_key() {
        let cat = two_namespaces();
        assert_eq!(
            namespace_entry(&cat, "NOW").map(|(k, _)| k.as_str()),
            Some("now")
        );
        assert!(namespace_entry(&cat, "nope").is_none());
    }
}
