//! `sn context` — the instance-side session context: which application scope
//! and update set the caller's tracked writes are captured under.
//!
//! This is per-user **server-side** state, not CLI config. When an API user
//! writes to a tracked table (`sys_script`, `sys_properties`, …), the artifact
//! is scoped and captured under whatever application scope and update set that
//! account's session context names — invisibly, unless something reads it.
//! The state lives in `sys_user_preference` rows: `apps.current_app` (the
//! scope's sys_id, literal `global` for Global), `sys_update_set` (the active
//! set), and one `updateSetForScope<scopeSysId>` per scope the user has
//! touched (each scope remembers its own set — why the UI flips your update
//! set when you switch scope).
//!
//! The UI drives this through two undocumented endpoints,
//! `GET/PUT /api/now/ui/concoursepicker/{application,updateset}` ("Concourse"
//! is the UI frame's internal codename). Everything below was established
//! live against a PDI (2026-08-17), and two of the findings shape this
//! module:
//!
//! - **The application PUT is a decoy over REST.** `PUT …/application
//!   {"app_id": X}` answers success, stages the per-scope update-set
//!   preferences — and never writes `apps.current_app`, so the effective
//!   scope does not move. The real lever is writing the preference row
//!   directly, which `scope` does (after the PUT, so the platform's own
//!   staging still runs).
//! - **The updateset GET has side effects**: it heals a `sys_update_set`
//!   preference that disagrees with the current scope back to the per-scope
//!   memory. The read here is GraphQL against the raw rows — genuinely
//!   read-only — so it mirrors that reconciliation in code instead
//!   (`source: preference | scope-memory | scope-default`) and flags the
//!   stale preference rather than healing or hiding it.
//!
//! The read is **one round trip**: a single GraphQL document resolves the
//! preferences into their `sys_scope`/`sys_update_set` records server-side
//! via `javascript:gs.getPreference(…)` query terms. A dropped term (an
//! instance that won't evaluate `javascript:`) would silently return *every*
//! row, so every term-dependent alias caps at 2 results and treats a second
//! row as proof the filter is gone (`Error::Instance`), never as an answer.
//! Row ACLs on `sys_scope`/`sys_update_set` make this an admin/developer
//! surface: for a plain `itil` user the canaries come back empty and the
//! command says so, rather than reporting a context it cannot see.

use crate::cli::graphql::{errors_to_api_error, execute, graphql_errors};
use crate::cli::kernel::{connect, write_response};
use crate::cli::{auth, GlobalFlags};
use crate::client::Client;
use crate::error::{Error, Result};
use clap::Subcommand;
use serde_json::{json, Map, Value};

const PICKER_APPLICATION: &str = "/api/now/ui/concoursepicker/application";
const PICKER_UPDATESET: &str = "/api/now/ui/concoursepicker/updateset";
const PREF_TABLE: &str = "/api/now/table/sys_user_preference";
const CURRENT_APP_PREF: &str = "apps.current_app";

#[derive(Subcommand, Debug)]
pub enum ContextSub {
    /// Switch the current application scope (by scope name, display name, or
    /// sys_id) and stage that scope's update set; verified by re-read.
    Scope(ContextTargetArgs),
    /// Switch the current update set (by name or sys_id; must be in progress
    /// and in the current scope); verified by re-read.
    Updateset(ContextTargetArgs),
}

#[derive(clap::Args, Debug)]
pub struct ContextTargetArgs {
    /// The scope or update set to switch to: name or sys_id.
    pub target: String,
}

/// One resolved scope: the row `apps.current_app` points at.
#[derive(Debug, Clone, PartialEq)]
struct ScopeRow {
    sys_id: String,
    name: String,
    scope: String,
}

/// One resolved update set, with the scope it belongs to.
#[derive(Debug, Clone, PartialEq)]
struct UpdateSetRow {
    sys_id: String,
    name: String,
    application: String,
}

/// How the current update set was determined, mirroring the platform's own
/// reconciliation order. Anything but `Preference` means the raw
/// `sys_update_set` preference is stale for the current scope.
#[derive(Debug, Clone, Copy, PartialEq)]
enum UpdateSetSource {
    /// The `sys_update_set` preference, agreeing with the current scope.
    Preference,
    /// The scope's `updateSetForScope<sys_id>` memory.
    ScopeMemory,
    /// The scope's default update set.
    ScopeDefault,
}

impl UpdateSetSource {
    fn as_str(self) -> &'static str {
        match self {
            UpdateSetSource::Preference => "preference",
            UpdateSetSource::ScopeMemory => "scope-memory",
            UpdateSetSource::ScopeDefault => "scope-default",
        }
    }
}

#[derive(Debug, Clone)]
struct ContextState {
    scope: ScopeRow,
    update_set: Option<(UpdateSetRow, UpdateSetSource)>,
}

/// The one-round-trip read: every alias resolves a preference into its record
/// server-side. `canary`/`us_canary` probe rows that exist on any instance
/// (the Global scope; its default update set), so an empty canary is an
/// access verdict, not an empty context. `pagination: {limit: 2}` plus the
/// at-most-one-match shape of each condition is the dropped-`javascript:`
/// guard (see module docs).
const READ_DOC: &str = r#"query {
  GlideRecord_Query {
    canary: sys_scope(queryConditions: "sys_id=global", pagination: {limit: 2}) {
      _results { sys_id { value } name { value } scope { value } }
    }
    scope: sys_scope(queryConditions: "sys_id=javascript:gs.getPreference('apps.current_app')", pagination: {limit: 2}) {
      _results { sys_id { value } name { value } scope { value } }
    }
    active: sys_update_set(queryConditions: "sys_id=javascript:gs.getPreference('sys_update_set')", pagination: {limit: 2}) {
      _results { sys_id { value } name { value } application { value } }
    }
    remembered: sys_update_set(queryConditions: "sys_id=javascript:gs.getPreference('updateSetForScope' + (gs.getPreference('apps.current_app') || 'global'))", pagination: {limit: 2}) {
      _results { sys_id { value } name { value } application { value } }
    }
    fallback: sys_update_set(queryConditions: "application=javascript:gs.getPreference('apps.current_app') || 'global'^is_default=true", pagination: {limit: 2}) {
      _results { sys_id { value } name { value } application { value } }
    }
    us_canary: sys_update_set(queryConditions: "application=global^is_default=true", pagination: {limit: 2}) {
      _results { sys_id { value } }
    }
  }
}"#;

pub fn show(global: &GlobalFlags) -> Result<()> {
    let client = connect(global)?;
    let state = read_context(&client)?;
    write_response(global, &context_json(&state, None))
}

pub fn set_scope(global: &GlobalFlags, args: ContextTargetArgs) -> Result<()> {
    let target = validate_target(&args.target)?;
    let client = connect(global)?;
    let previous = read_context(&client)?;
    let scope = resolve_scope(&client, target)?;

    // The platform's own staging half first: the picker PUT creates the
    // per-scope update-set preferences the way the UI would. Its "success"
    // proves nothing about the scope itself (see module docs) — the
    // preference write below and the verifying re-read are what count.
    client.put(PICKER_APPLICATION, &[], &json!({ "app_id": scope.sys_id }))?;
    upsert_current_app_pref(&client, &scope.sys_id)?;

    let state = read_context(&client)?;
    if state.scope.sys_id != scope.sys_id {
        return Err(Error::Api {
            status: 200,
            message: format!(
                "scope switch did not persist: asked for {} ({}), instance reports {} ({})",
                scope.name, scope.sys_id, state.scope.name, state.scope.sys_id
            ),
            detail: None,
            transaction_id: None,
            sn_error: None,
        });
    }
    write_response(global, &context_json(&state, Some(&previous)))
}

pub fn set_updateset(global: &GlobalFlags, args: ContextTargetArgs) -> Result<()> {
    let target = validate_target(&args.target)?;
    let client = connect(global)?;
    let previous = read_context(&client)?;
    let set = resolve_update_set(&client, target, &previous.scope)?;

    let resp = client.put(PICKER_UPDATESET, &[], &json!({ "sysId": set.sys_id }))?;
    if resp.pointer("/result/success").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Instance {
            message: "the update-set picker did not acknowledge the change".into(),
            detail: Some(format!("response: {resp}")),
        });
    }

    let state = read_context(&client)?;
    match &state.update_set {
        Some((now, UpdateSetSource::Preference)) if now.sys_id == set.sys_id => {}
        _ => {
            return Err(Error::Api {
                status: 200,
                message: format!(
                    "update set selection did not persist: asked for {} ({})",
                    set.name, set.sys_id
                ),
                detail: None,
                transaction_id: None,
                sn_error: Some(context_json(&state, None)),
            })
        }
    }
    write_response(global, &context_json(&state, Some(&previous)))
}

/// Read the whole context in one GraphQL round trip and reconcile it the way
/// the platform would, without writing anything.
fn read_context(client: &Client) -> Result<ContextState> {
    let resp = execute(client, READ_DOC, None, None)?;
    let errors = graphql_errors(&resp);
    if !errors.is_empty() {
        return Err(errors_to_api_error(errors));
    }
    parse_state(&resp)
}

/// Parse the [`READ_DOC`] response and mirror the platform's update-set
/// reconciliation order: the active preference if it agrees with the current
/// scope, else the scope's memory, else the scope's default set.
fn parse_state(resp: &Value) -> Result<ContextState> {
    if alias_rows(resp, "canary")?.is_empty() || alias_rows(resp, "us_canary")?.is_empty() {
        return Err(Error::Instance {
            message: "cannot read sys_scope/sys_update_set, so the session context is invisible \
                      to this account"
                .into(),
            detail: Some(
                "row ACLs on these tables answer with empty results, not an error; reading the \
                 context needs an admin or delegated-developer role"
                    .into(),
            ),
        });
    }

    let scope = match scope_rows(resp, "scope")?.into_iter().next() {
        Some(row) => row,
        // No apps.current_app preference (or a dangling one): the platform
        // treats that as Global, whose row the canary already carries.
        None => scope_rows(resp, "canary")?
            .into_iter()
            .next()
            .expect("canary verified non-empty above"),
    };

    let in_scope = |row: &UpdateSetRow| row.application == scope.sys_id;
    let pick = |rows: Vec<UpdateSetRow>| rows.into_iter().find(in_scope);
    let update_set = pick(update_set_rows(resp, "active")?)
        .map(|r| (r, UpdateSetSource::Preference))
        .or_else(|| {
            pick(update_set_rows(resp, "remembered").ok()?)
                .map(|r| (r, UpdateSetSource::ScopeMemory))
        })
        .or_else(|| {
            pick(update_set_rows(resp, "fallback").ok()?)
                .map(|r| (r, UpdateSetSource::ScopeDefault))
        });

    Ok(ContextState { scope, update_set })
}

fn context_json(state: &ContextState, previous: Option<&ContextState>) -> Value {
    let mut out = Map::new();
    out.insert("scope".into(), scope_json(&state.scope));
    match &state.update_set {
        Some((row, source)) => {
            out.insert(
                "update_set".into(),
                json!({ "sys_id": row.sys_id, "name": row.name, "source": source.as_str() }),
            );
            if *source != UpdateSetSource::Preference {
                // The raw sys_update_set preference points elsewhere; the next
                // UI picker read (or `sn context updateset`) would heal it.
                out.insert("preference_stale".into(), Value::Bool(true));
            }
        }
        None => {
            out.insert("update_set".into(), Value::Null);
            out.insert(
                "note".into(),
                Value::String(
                    "no update set resolved for this scope (no active preference, no per-scope \
                     memory, no default set)"
                        .into(),
                ),
            );
        }
    }
    if let Some(prev) = previous {
        out.insert(
            "previous".into(),
            json!({
                "scope": scope_json(&prev.scope),
                "update_set": prev.update_set.as_ref().map(|(row, _)| {
                    json!({ "sys_id": row.sys_id, "name": row.name })
                }),
            }),
        );
    }
    Value::Object(out)
}

fn scope_json(scope: &ScopeRow) -> Value {
    json!({ "sys_id": scope.sys_id, "name": scope.name, "scope": scope.scope })
}

/// `scope <TARGET>` resolution: one Table API read matching scope name,
/// display name, or sys_id, all exact.
fn resolve_scope(client: &Client, target: &str) -> Result<ScopeRow> {
    let resp = client.get(
        "/api/now/table/sys_scope",
        &query_pairs(&[
            (
                "sysparm_query",
                &format!("scope={target}^ORname={target}^ORsys_id={target}"),
            ),
            ("sysparm_fields", "sys_id,name,scope"),
            ("sysparm_display_value", "false"),
            ("sysparm_limit", "3"),
        ]),
    )?;
    let rows: Vec<ScopeRow> = table_rows(&resp)
        .iter()
        .filter_map(|r| {
            Some(ScopeRow {
                sys_id: str_field(r, "sys_id")?,
                name: str_field(r, "name")?,
                scope: str_field(r, "scope")?,
            })
        })
        .collect();
    match rows.len() {
        0 => Err(Error::Usage(format!(
            "no application scope matching '{target}' (by scope name, display name, or sys_id)"
        ))),
        1 => Ok(rows.into_iter().next().expect("len checked")),
        _ => Err(Error::Usage(format!(
            "'{target}' matches more than one scope: {}; use the sys_id",
            rows.iter()
                .map(|r| format!("{} ({})", r.scope, r.sys_id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The stored choice value for a selectable set. **The space is real**: the
/// column's value is `in progress`, not `in_progress`, and the wrong spelling
/// silently matches nothing (measured — every real set then "failed" the
/// diagnostic re-query's explanation instead).
const STATE_IN_PROGRESS: &str = "in progress";

/// `updateset <TARGET>` resolution, restricted to in-progress sets in the
/// current scope — the same population the UI picker offers. The OR group
/// leads and the AND terms trail because a trailing AND applies to the whole
/// `^OR` group. A miss re-queries without the restriction so the error can
/// say *why* (wrong scope, completed) instead of "not found".
fn resolve_update_set(client: &Client, target: &str, scope: &ScopeRow) -> Result<UpdateSetRow> {
    let rows = update_set_query(
        client,
        &format!(
            "sys_id={target}^ORname={target}^application={}^state={STATE_IN_PROGRESS}",
            scope.sys_id
        ),
    )?;
    match rows.len() {
        0 => {
            let near = update_set_query(client, &format!("sys_id={target}^ORname={target}"))?;
            match near.first() {
                Some(row) if row.application != scope.sys_id => Err(Error::Usage(format!(
                    "update set '{}' ({}) belongs to another scope; switch first with \
                     `sn context scope`, or pick one in {}",
                    row.name, row.sys_id, scope.name
                ))),
                Some(row) => Err(Error::Usage(format!(
                    "update set '{}' ({}) is not in progress; only in-progress sets can be \
                     selected",
                    row.name, row.sys_id
                ))),
                None => Err(Error::Usage(format!(
                    "no update set matching '{target}' (by name or sys_id)"
                ))),
            }
        }
        1 => Ok(rows.into_iter().next().expect("len checked")),
        _ => Err(Error::Usage(format!(
            "'{target}' matches more than one update set: {}; use the sys_id",
            rows.iter()
                .map(|r| format!("{} ({})", r.name, r.sys_id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn update_set_query(client: &Client, query: &str) -> Result<Vec<UpdateSetRow>> {
    let resp = client.get(
        "/api/now/table/sys_update_set",
        &query_pairs(&[
            ("sysparm_query", query),
            ("sysparm_fields", "sys_id,name,application,state"),
            ("sysparm_display_value", "false"),
            ("sysparm_limit", "3"),
        ]),
    )?;
    Ok(table_rows(&resp)
        .iter()
        .filter_map(|r| {
            Some(UpdateSetRow {
                sys_id: str_field(r, "sys_id")?,
                name: str_field(r, "name")?,
                application: reference_value(r.get("application")).unwrap_or_default(),
            })
        })
        .collect())
}

/// Point `apps.current_app` at a scope — the write the picker PUT refuses to
/// do over REST. The row is per user, so the caller must be nameable; update
/// the existing row or create the first one.
fn upsert_current_app_pref(client: &Client, scope_sys_id: &str) -> Result<()> {
    let user = auth::identify(client)?
        .and_then(|id| id.sys_id)
        .ok_or_else(|| Error::Instance {
            message: "the instance did not name the calling user, so their preference row \
                      cannot be located"
                .into(),
            detail: None,
        })?;
    let resp = client.get(
        PREF_TABLE,
        &query_pairs(&[
            (
                "sysparm_query",
                &format!("user={user}^name={CURRENT_APP_PREF}^ORDERBYsys_created_on"),
            ),
            ("sysparm_fields", "sys_id"),
            ("sysparm_limit", "2"),
        ]),
    )?;
    let existing = table_rows(&resp)
        .first()
        .and_then(|r| str_field(r, "sys_id"));
    match existing {
        Some(row_id) => {
            client.patch(
                &format!("{PREF_TABLE}/{row_id}"),
                &[],
                &json!({ "value": scope_sys_id }),
            )?;
        }
        None => {
            client.post(
                PREF_TABLE,
                &[],
                &json!({ "user": user, "name": CURRENT_APP_PREF, "value": scope_sys_id }),
            )?;
        }
    }
    Ok(())
}

/// A target goes into an encoded query verbatim, so a `^` in it would splice
/// extra terms into our own filter. No real scope or update-set name carries
/// one; refuse rather than misquery.
fn validate_target(target: &str) -> Result<&str> {
    let t = target.trim();
    if t.is_empty() {
        return Err(Error::Usage("target must not be empty".into()));
    }
    if t.contains('^') {
        return Err(Error::Usage(
            "target must not contain '^' (it would be parsed as an encoded-query separator)".into(),
        ));
    }
    Ok(t)
}

/// The `_results` array of one alias in the GraphQL response. A missing alias
/// is schema drift, not an empty result.
fn alias_rows<'a>(resp: &'a Value, alias: &str) -> Result<&'a Vec<Value>> {
    resp.pointer(&format!("/data/GlideRecord_Query/{alias}/_results"))
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Instance {
            message: format!("GraphQL response carries no '{alias}' results"),
            detail: Some(
                "the GlideRecord_Query schema did not answer in the expected shape".into(),
            ),
        })
}

/// A term-filtered alias can match at most one row; a second one is proof the
/// instance dropped the `javascript:` term and answered unfiltered.
fn guarded_rows<'a>(resp: &'a Value, alias: &str) -> Result<&'a Vec<Value>> {
    let rows = alias_rows(resp, alias)?;
    if rows.len() > 1 {
        return Err(Error::Instance {
            message: format!(
                "the instance did not evaluate the javascript: query term ('{alias}' matched \
                 {} rows where at most one is possible)",
                rows.len()
            ),
            detail: Some("an unfiltered result must not be reported as the session context".into()),
        });
    }
    Ok(rows)
}

fn scope_rows(resp: &Value, alias: &str) -> Result<Vec<ScopeRow>> {
    Ok(guarded_rows(resp, alias)?
        .iter()
        .filter_map(|r| {
            Some(ScopeRow {
                sys_id: graphql_value(r, "sys_id")?,
                name: graphql_value(r, "name")?,
                scope: graphql_value(r, "scope")?,
            })
        })
        .collect())
}

fn update_set_rows(resp: &Value, alias: &str) -> Result<Vec<UpdateSetRow>> {
    Ok(guarded_rows(resp, alias)?
        .iter()
        .filter_map(|r| {
            Some(UpdateSetRow {
                sys_id: graphql_value(r, "sys_id")?,
                name: graphql_value(r, "name")?,
                application: graphql_value(r, "application")?,
            })
        })
        .collect())
}

/// `{field: {value: "..."}}` — the GraphQL column wrapper.
fn graphql_value(row: &Value, field: &str) -> Option<String> {
    row.pointer(&format!("/{field}/value"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `result` array of a Table API list response.
fn table_rows(resp: &Value) -> Vec<Value> {
    resp.get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn str_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A Table API reference field under `sysparm_display_value=false` is
/// `{value, link}`; older shapes are a bare string. Take either.
fn reference_value(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn query_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graphql_resp(aliases: Value) -> Value {
        json!({ "data": { "GlideRecord_Query": aliases } })
    }

    fn scope_result(sys_id: &str, name: &str, scope: &str) -> Value {
        json!({ "sys_id": {"value": sys_id}, "name": {"value": name}, "scope": {"value": scope} })
    }

    fn us_result(sys_id: &str, name: &str, application: &str) -> Value {
        json!({ "sys_id": {"value": sys_id}, "name": {"value": name},
                "application": {"value": application} })
    }

    fn results(rows: Vec<Value>) -> Value {
        json!({ "_results": rows })
    }

    #[test]
    fn guarded_rows_rejects_a_second_row() {
        let resp = graphql_resp(json!({
            "scope": results(vec![
                scope_result("a", "A", "a"),
                scope_result("b", "B", "b"),
            ]),
        }));
        let err = guarded_rows(&resp, "scope").unwrap_err();
        assert!(matches!(err, Error::Instance { .. }));
        assert!(err.to_string().contains("javascript:"));
    }

    #[test]
    fn missing_alias_is_instance_error_not_empty() {
        let resp = graphql_resp(json!({}));
        assert!(matches!(
            alias_rows(&resp, "canary").unwrap_err(),
            Error::Instance { .. }
        ));
    }

    #[test]
    fn empty_canaries_are_an_access_verdict() {
        let resp = graphql_resp(json!({
            "canary": results(vec![]),
            "us_canary": results(vec![]),
            "scope": results(vec![]),
            "active": results(vec![]),
            "remembered": results(vec![]),
            "fallback": results(vec![]),
        }));
        let err = parse_state(&resp).unwrap_err();
        assert!(matches!(err, Error::Instance { .. }));
        assert!(err.to_string().contains("invisible"));
    }

    #[test]
    fn stale_preference_falls_back_to_scope_memory() {
        // Scope is WCC; the active preference still points at a Global set.
        let resp = graphql_resp(json!({
            "canary": results(vec![scope_result("global", "Global", "global")]),
            "us_canary": results(vec![json!({"sys_id": {"value": "d"}})]),
            "scope": results(vec![scope_result("wcc", "Weekend Change Console", "x_wcc")]),
            "active": results(vec![us_result("g1", "Probe", "global")]),
            "remembered": results(vec![us_result("w1", "Default", "wcc")]),
            "fallback": results(vec![us_result("w1", "Default", "wcc")]),
        }));
        let state = parse_state(&resp).unwrap();
        assert_eq!(state.scope.sys_id, "wcc");
        let (row, source) = state.update_set.clone().expect("resolved");
        assert_eq!(row.sys_id, "w1");
        assert_eq!(source, UpdateSetSource::ScopeMemory);

        let out = context_json(&state, None);
        assert_eq!(out["preference_stale"], true);
        assert_eq!(out["update_set"]["source"], "scope-memory");
    }

    #[test]
    fn missing_preference_means_global() {
        let resp = graphql_resp(json!({
            "canary": results(vec![scope_result("global", "Global", "global")]),
            "us_canary": results(vec![json!({"sys_id": {"value": "d"}})]),
            "scope": results(vec![]),
            "active": results(vec![us_result("d", "Default", "global")]),
            "remembered": results(vec![]),
            "fallback": results(vec![us_result("d", "Default", "global")]),
        }));
        let state = parse_state(&resp).unwrap();
        assert_eq!(state.scope.sys_id, "global");
        let (row, source) = state.update_set.clone().expect("resolved");
        assert_eq!(row.sys_id, "d");
        assert_eq!(source, UpdateSetSource::Preference);
        assert!(context_json(&state, None).get("preference_stale").is_none());
    }

    #[test]
    fn no_update_set_resolvable_is_null_with_note() {
        let resp = graphql_resp(json!({
            "canary": results(vec![scope_result("global", "Global", "global")]),
            "us_canary": results(vec![json!({"sys_id": {"value": "d"}})]),
            "scope": results(vec![scope_result("wcc", "WCC", "x_wcc")]),
            "active": results(vec![us_result("g1", "Probe", "global")]),
            "remembered": results(vec![]),
            "fallback": results(vec![]),
        }));
        let state = parse_state(&resp).unwrap();
        assert!(state.update_set.is_none());
        let out = context_json(&state, None);
        assert_eq!(out["update_set"], Value::Null);
        assert!(out["note"].as_str().unwrap().contains("no update set"));
    }

    #[test]
    fn previous_context_rides_along() {
        let prev = ContextState {
            scope: ScopeRow {
                sys_id: "global".into(),
                name: "Global".into(),
                scope: "global".into(),
            },
            update_set: Some((
                UpdateSetRow {
                    sys_id: "d".into(),
                    name: "Default".into(),
                    application: "global".into(),
                },
                UpdateSetSource::Preference,
            )),
        };
        let state = ContextState {
            scope: ScopeRow {
                sys_id: "wcc".into(),
                name: "WCC".into(),
                scope: "x_wcc".into(),
            },
            update_set: prev.update_set.clone(),
        };
        let out = context_json(&state, Some(&prev));
        assert_eq!(out["previous"]["scope"]["sys_id"], "global");
        assert_eq!(out["previous"]["update_set"]["name"], "Default");
    }

    #[test]
    fn target_validation() {
        assert!(validate_target("  x_scope ").is_ok());
        assert!(matches!(validate_target(""), Err(Error::Usage(_))));
        assert!(matches!(validate_target("a^b"), Err(Error::Usage(_))));
    }

    #[test]
    fn reference_value_takes_both_shapes() {
        assert_eq!(
            reference_value(Some(&json!({"value": "abc", "link": "https://x"}))),
            Some("abc".into())
        );
        assert_eq!(reference_value(Some(&json!("abc"))), Some("abc".into()));
        assert_eq!(reference_value(Some(&json!(""))), None);
        assert_eq!(reference_value(None), None);
    }
}
