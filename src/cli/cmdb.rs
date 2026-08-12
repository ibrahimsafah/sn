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
    /// Body source: inline JSON, @file (path), or @- (stdin). Give the CI's fields flat; they are wrapped into the `attributes` envelope this API requires. A body whose `attributes` is a JSON object is already an envelope and keeps its shape (so `inbound_relations`/`outbound_relations` can ride along). Attribute values go out as strings either way, since the API casts each to a Java String and answers a JSON number or boolean with an HTTP 500; an object or array is refused up front. A flat body's top-level `source` is refused as ambiguous — pass provenance as --source.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value, wrapped into `attributes` and sent as a string (`cpu_count=8` goes out as "8"; the API 500s on a JSON number). Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
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
    /// Body source: inline JSON, @file (path), or @- (stdin). Give the CI's fields flat; they are wrapped into the `attributes` envelope this API requires. A body whose `attributes` is a JSON object is already an envelope and keeps its shape. Attribute values go out as strings either way, since the API casts each to a Java String and answers a JSON number or boolean with an HTTP 500; an object or array is refused up front. A flat body's top-level `source` is refused as ambiguous — pass provenance as --source.
    #[arg(long, short = 'D', conflicts_with = "field")]
    pub data: Option<String>,
    /// Repeatable name=value, wrapped into `attributes` and sent as a string (`operational_status=2` goes out as "2"; the API 500s on a JSON number). Use name=@file to read the value from a file (e.g. multi-line text). Mutually exclusive with --data.
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
/// Passthrough covers the envelope's *shape*, not its attribute values: those
/// are normalized either way (see `stringify_attributes`), because the type
/// constraint below is the API's and a hand-written envelope hits it just as
/// hard.
///
/// A **flat** body's top-level `source` is refused rather than read either way.
/// The IRE spends `source` on provenance, so it can never reach an attribute;
/// but `source` is also a real String column on `cmdb_ci_service_discovered`
/// and `cmdb_ci_service_calculated`, so lifting it into the envelope would
/// silently swallow a legitimate field write on those classes. Either reading
/// loses something silently, which is precisely the failure this function
/// exists to prevent — so the caller is asked which they meant.
fn ire_envelope(body: Value, source: Option<String>) -> Result<Value> {
    let mut envelope = match body {
        Value::Object(map) if map.get("attributes").is_some_and(Value::is_object) => map,
        Value::Object(flat) if flat.contains_key("source") => {
            return Err(Error::Usage(format!(
                "\"source\" in a flat body is ambiguous and would be dropped: to the CMDB \
                 Instance API a top-level \"source\" is the record's provenance, never an \
                 attribute. Pass provenance as --source {}, or write a class's own `source` \
                 column with an explicit envelope: \
                 {{\"attributes\": {{\"source\": ...}}, \"source\": \"<provenance>\"}}",
                flat["source"]
            )));
        }
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
        (Some(in_body), None) if !in_body.is_string() => {
            return Err(Error::Usage(format!(
                "\"source\": {in_body} must be a string — a choice value from \
                 cmdb_ci.discovery_source"
            )));
        }
        (None, chosen) => {
            let value = chosen.unwrap_or_else(|| DEFAULT_SOURCE.to_string());
            envelope.insert("source".into(), Value::String(value));
        }
        (Some(_), None) => {}
    }
    if let Some(attrs) = envelope.get_mut("attributes") {
        stringify_attributes(attrs)?;
    }
    Ok(Value::Object(envelope))
}

/// Render every attribute value as a JSON string, because the CMDB Instance
/// API casts each one to `java.lang.String` before it reaches the record. This
/// is not a lenient coercion: measured on Zurich, `{"cpu_count": 8}` comes back
/// as HTTP 500 `class java.lang.Integer cannot be cast to class
/// java.lang.String` and writes nothing — likewise `Boolean`, `LinkedHashMap`
/// (object) and `ArrayList` (array). The same write as `"8"` succeeds and the
/// record reads back `cpu_count: "8"`; `"true"` sets a boolean column properly.
/// So `--field cpu_count=8` and the documented `--field operational_status=2`
/// only work if the number is stringified.
///
/// It happens here and not in `body.rs`: this is a constraint of *this* API,
/// and `coerce_field_value` is shared with `sn table`, whose Table API accepts
/// JSON numbers and booleans fine.
///
/// `null` is left alone — Java casts it without complaint and the API answers
/// 200, so it stays available for clearing a field. Objects and arrays are
/// refused rather than serialized: the record would receive the literal text
/// `{"a":1}`, which is never what the caller meant, and anyone who genuinely
/// wants JSON in a string column can quote it themselves.
fn stringify_attributes(attrs: &mut Value) -> Result<()> {
    let Some(map) = attrs.as_object_mut() else {
        return Ok(());
    };
    for (name, value) in map.iter_mut() {
        match value {
            Value::String(_) | Value::Null => {}
            Value::Number(_) | Value::Bool(_) => {
                *value = Value::String(value.to_string());
            }
            Value::Object(_) | Value::Array(_) => {
                return Err(Error::Usage(format!(
                    "attribute {name:?} is a JSON {}; the CMDB Instance API takes only \
                     string values for attributes (it rejects anything else with an HTTP \
                     500 cast error). Pass it as a string if the column really holds JSON",
                    if value.is_array() { "array" } else { "object" }
                )));
            }
        }
    }
    Ok(())
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

    /// `--field cpu_count=8` parses to a JSON number, which the Instance API
    /// answers with an HTTP 500 `Integer cannot be cast to String`.
    #[test]
    fn numbers_and_bools_become_strings() {
        let out = ire_envelope(
            json!({"cpu_count": 8, "virtual": true, "cost": 12.5, "name": "web01"}),
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            json!({
                "attributes": {
                    "cpu_count": "8",
                    "virtual": "true",
                    "cost": "12.5",
                    "name": "web01",
                },
                "source": "Manual Entry",
            })
        );
    }

    /// The passthrough covers the envelope's shape, not the cast the API does
    /// to every attribute value.
    #[test]
    fn an_explicit_envelope_gets_its_attribute_values_stringified_too() {
        let out = ire_envelope(
            json!({"attributes": {"operational_status": 2}, "source": "Altiris"}),
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"operational_status": "2"}, "source": "Altiris"})
        );
    }

    /// Java casts null to String happily and the API answers 200, so null
    /// stays available for clearing a field.
    #[test]
    fn null_attribute_survives_verbatim() {
        let out = ire_envelope(json!({"short_description": null}), None).unwrap();
        assert_eq!(
            out,
            json!({"attributes": {"short_description": null}, "source": "Manual Entry"})
        );
    }

    #[test]
    fn object_and_array_attribute_values_are_refused() {
        for body in [
            json!({"ports": {"a": 1}}),
            json!({"attributes": {"ports": ["a"]}, "source": "Manual Entry"}),
        ] {
            let err = ire_envelope(body.clone(), None).unwrap_err();
            let Error::Usage(msg) = err else {
                panic!("expected a usage error for {body}");
            };
            assert!(msg.contains("\"ports\""), "{msg}");
            assert!(msg.contains("string values"), "{msg}");
        }
    }

    /// The IRE spends a top-level `source` on provenance, so a flat one can
    /// never reach an attribute — and lifting it would swallow the real
    /// `source` column that `cmdb_ci_service_discovered` carries.
    #[test]
    fn a_flat_bodys_source_is_refused_rather_than_demoted() {
        for flag in [None, Some("Manual Entry".to_string())] {
            let err =
                ire_envelope(json!({"name": "web01", "source": "Altiris"}), flag).unwrap_err();
            let Error::Usage(msg) = err else {
                panic!("expected a usage error");
            };
            assert!(msg.contains("ambiguous"), "{msg}");
            assert!(msg.contains("Altiris"), "{msg}");
            assert!(msg.contains("--source"), "{msg}");
        }
    }

    #[test]
    fn a_non_string_source_in_an_envelope_is_a_usage_error() {
        let err =
            ire_envelope(json!({"attributes": {"name": "web01"}, "source": 7}), None).unwrap_err();
        let Error::Usage(msg) = err else {
            panic!("expected a usage error");
        };
        assert!(msg.contains("must be a string"), "{msg}");
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
