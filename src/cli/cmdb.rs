use crate::body::{build_body, BodyInput};
use crate::cli::table::{build_client, build_profile, confirm_delete, unwrap_or_raw};
use crate::cli::GlobalFlags;
use crate::error::{Error, Result};
use clap::Subcommand;
use serde_json::{Map, Value};

/// `source` when the caller names none. A CLI write *is* a manual entry, so
/// this is the truthful value rather than a convenience; borrowing a discovery
/// tool's name would hand our record that tool's reconciliation precedence.
const DEFAULT_SOURCE: &str = "Manual Entry";

#[derive(Subcommand, Debug)]
pub enum CmdbSub {
    /// List CI records for a CMDB class.
    List(CmdbListArgs),
    /// Get a CI record with relations.
    Get(CmdbGetArgs),
    /// Create a CI record.
    Create(CmdbCreateArgs),
    /// Update a CI record (PATCH).
    Update(CmdbUpdateArgs),
    /// Get metadata for a CMDB class.
    Meta(CmdbMetaArgs),
    /// Relation operations on a CI.
    Relation {
        #[command(subcommand)]
        sub: CmdbRelationSub,
    },
}

#[derive(clap::Args, Debug)]
pub struct CmdbListArgs {
    /// CMDB class name (e.g. `cmdb_ci_server`).
    pub class: String,
    /// Encoded query, e.g. `active=true^priority=1`.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Maximum records returned. Maps to sysparm_limit.
    #[arg(long, alias = "sysparm-limit", alias = "limit", default_value_t = 1000)]
    pub setlimit: u32,
    /// Starting offset for manual pagination.
    #[arg(long, alias = "sysparm-offset")]
    pub offset: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbGetArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
}

#[derive(clap::Args, Debug)]
pub struct CmdbCreateArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Give the CI's fields flat; they are wrapped into the `attributes` envelope this API requires. A body whose `attributes` is a JSON object is already an envelope and is sent untouched (so `inbound_relations`/`outbound_relations` can ride along).
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value, wrapped into `attributes`. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Discovery source to record; required by the API. A choice value from `cmdb_ci.discovery_source` (list them with `sn schema choices cmdb_ci discovery_source`). Defaults to "Manual Entry" — name a real source only when standing in for it, since the IRE reconciles by source and a borrowed name lets that tool's next run overwrite this record. Rejected if the body already carries `source`.
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbUpdateArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Give the CI's fields flat; they are wrapped into the `attributes` envelope this API requires. A body whose `attributes` is a JSON object is already an envelope and is sent untouched.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value, wrapped into `attributes`. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
    /// Discovery source to record; required by the API. A choice value from `cmdb_ci.discovery_source` (list them with `sn schema choices cmdb_ci discovery_source`). Defaults to "Manual Entry" — name a real source only when standing in for it, since the IRE reconciles by source and a borrowed name lets that tool's next run overwrite this record. Rejected if the body already carries `source`.
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbMetaArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
}

#[derive(Subcommand, Debug)]
pub enum CmdbRelationSub {
    /// Create a relation on a CI.
    Add(CmdbRelationAddArgs),
    /// Delete a relation from a CI.
    Delete(CmdbRelationDeleteArgs),
}

#[derive(clap::Args, Debug)]
pub struct CmdbRelationAddArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// Body source: inline JSON, @file (path), or @- (stdin). Use a file to avoid shell quoting on multi-line values.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value. Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
    #[arg(long = "field", short = 'F', conflicts_with = "data")]
    pub field: Vec<String>,
}

#[derive(clap::Args, Debug)]
pub struct CmdbRelationDeleteArgs {
    /// CMDB class name (e.g. `cmdb_ci_linux_server`).
    pub class: String,
    /// sys_id of the CI.
    pub sys_id: String,
    /// sys_id of the relation to delete.
    pub rel_sys_id: String,
    /// Skip confirmation prompt (required for non-interactive use).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

pub fn list(global: &GlobalFlags, args: CmdbListArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}", args.class);
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(v) = args.query {
        query.push(("sysparm_query".into(), v));
    }
    query.push(("sysparm_limit".into(), args.setlimit.to_string()));
    if let Some(v) = args.offset {
        query.push(("sysparm_offset".into(), v.to_string()));
    }
    let resp = client.get(&path, &query)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn get(global: &GlobalFlags, args: CmdbGetArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}/{}", args.class, args.sys_id);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

/// Wrap a flat field map in the IRE envelope the CMDB Instance API demands:
/// `{"attributes": {...}, "source": "..."}`. A flat body is not merely refused
/// — it reaches the server as an unguarded `Map.put` and comes back as an
/// HTTP 500 Java NPE (`"attributes" is null`), and an envelope without `source`
/// is a 400. So every write is enveloped here; neither failure is reachable.
///
/// A body whose `attributes` is a JSON **object** is taken as an envelope the
/// caller wrote themselves and passed through, which is also how relations ride
/// along on create. The test is the value's type, not the key's presence,
/// because `attributes` is a real CMDB column on 718 classes of a stock Zurich
/// instance — every one of them String or Field List, i.e. never an object on
/// the wire, and `--field` cannot produce an object at all. So `attributes` as
/// a scalar is unambiguously a field to write, and gets wrapped like any other.
fn ire_envelope(body: Value, source: Option<String>) -> Result<Value> {
    let mut envelope = match body {
        Value::Object(map) if map.get("attributes").is_some_and(Value::is_object) => map,
        flat => {
            let mut map = Map::new();
            map.insert("attributes".into(), flat);
            map
        }
    };
    match (envelope.get("source"), source) {
        // Silently preferring one would misattribute the record's provenance,
        // and provenance is what the IRE reconciles on.
        (Some(in_body), Some(flag)) => {
            return Err(Error::Usage(format!(
                "source given twice: --source {flag:?} and \"source\": {in_body} in the body; pass it one way"
            )));
        }
        (None, chosen) => {
            let value = chosen.unwrap_or_else(|| DEFAULT_SOURCE.to_string());
            envelope.insert("source".into(), Value::String(value));
        }
        (Some(_), None) => {}
    }
    Ok(Value::Object(envelope))
}

pub fn create(global: &GlobalFlags, args: CmdbCreateArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}", args.class);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = ire_envelope(build_body(body_input)?, args.source)?;
    let resp = client.post(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn update(global: &GlobalFlags, args: CmdbUpdateArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/instance/{}/{}", args.class, args.sys_id);
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = ire_envelope(build_body(body_input)?, args.source)?;
    let resp = client.patch(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn meta(global: &GlobalFlags, args: CmdbMetaArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!("/api/now/cmdb/meta/{}", args.class);
    let resp = client.get(&path, &[])?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

pub fn relation(global: &GlobalFlags, sub: CmdbRelationSub) -> Result<()> {
    match sub {
        CmdbRelationSub::Add(args) => relation_add(global, args),
        CmdbRelationSub::Delete(args) => relation_delete(global, args),
    }
}

fn relation_add(global: &GlobalFlags, args: CmdbRelationAddArgs) -> Result<()> {
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!(
        "/api/now/cmdb/instance/{}/{}/relation",
        args.class, args.sys_id
    );
    let body_input = if let Some(d) = args.data {
        BodyInput::Data(d)
    } else if !args.field.is_empty() {
        BodyInput::Fields(args.field)
    } else {
        BodyInput::None
    };
    let body = build_body(body_input)?;
    let resp = client.post(&path, &[], &body)?;
    let out = unwrap_or_raw(resp, global.output);
    crate::cli::table::write_response(global, &out)
}

fn relation_delete(global: &GlobalFlags, args: CmdbRelationDeleteArgs) -> Result<()> {
    confirm_delete(
        args.yes,
        &format!(
            "relation {} on {}/{}",
            args.rel_sys_id, args.class, args.sys_id
        ),
    )?;
    let profile = build_profile(global)?;
    let client = build_client(&profile, global.timeout)?;
    let path = format!(
        "/api/now/cmdb/instance/{}/{}/relation/{}",
        args.class, args.sys_id, args.rel_sys_id
    );
    client.delete(&path, &[])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_body_is_wrapped_with_the_default_source() {
        let out = ire_envelope(json!({"name": "web01"}), None).unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"name": "web01"}, "source": "Manual Entry"})
        );
    }

    #[test]
    fn flag_source_lands_beside_attributes() {
        let out = ire_envelope(json!({"name": "web01"}), Some("ServiceNow".into())).unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"name": "web01"}, "source": "ServiceNow"})
        );
    }

    #[test]
    fn object_valued_attributes_passes_through() {
        let body = json!({
            "attributes": {"name": "web01"},
            "source": "Other Automated",
            "outbound_relations": [{"target": "abc", "type": "def"}],
        });
        assert_eq!(ire_envelope(body.clone(), None).unwrap(), body);
    }

    #[test]
    fn enveloped_body_without_source_gets_the_default() {
        let out = ire_envelope(json!({"attributes": {"name": "web01"}}), None).unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"name": "web01"}, "source": "Manual Entry"})
        );
    }

    /// 718 CMDB classes ship a real String column named `attributes`; writing
    /// it must not be mistaken for the envelope.
    #[test]
    fn scalar_attributes_is_a_field_not_an_envelope() {
        let out = ire_envelope(json!({"attributes": "cpu=8"}), None).unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"attributes": "cpu=8"}, "source": "Manual Entry"})
        );
    }

    #[test]
    fn source_in_both_places_is_a_usage_error() {
        let err = ire_envelope(
            json!({"attributes": {"name": "web01"}, "source": "ServiceNow"}),
            Some("Manual Entry".into()),
        )
        .unwrap_err();
        let Error::Usage(msg) = err else {
            panic!("expected a usage error");
        };
        assert!(msg.contains("given twice"), "{msg}");
        assert!(
            msg.contains("ServiceNow") && msg.contains("Manual Entry"),
            "{msg}"
        );
    }
}
