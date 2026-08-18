//! Record references — the `table:identifier` input form.
//!
//! One syntax names a record everywhere a command needs one: `table:sys_id`
//! (used directly) or `table:number` (resolved through one lookup). Parsing is
//! split from resolution on purpose: parse errors are pure argv mistakes
//! (exit 1) and must precede `connect` — and, for destructive verbs, the
//! confirmation gate — while resolution is a network call (exit 2 on failure)
//! that runs only after both.
//!
//! Resolution queries `number={n}` with `sysparm_limit=2`, the same canary
//! `auth::identify` uses: ServiceNow silently drops a query term it cannot
//! parse and returns unfiltered rows, so on a table with no usable `number`
//! field the lookup would otherwise resolve to whichever record sorts first.
//! A second row is proof the term is gone, and the error says so instead of
//! returning a stranger's sys_id. (A table whose *total* row count is 1
//! defeats the canary — the same accepted residual as `identify_via_sys_user`.)
//!
//! An identifier that is 32 ASCII hex chars is classified as a sys_id and
//! never looked up. A record number that happens to be exactly 32 hex chars
//! would be misclassified; real numbers are prefix+digits and far shorter, and
//! the escape hatch is `sn table list <t> --query number=<n>`.

use crate::cli::journal::{validate_identifier, validate_sys_id};
use crate::client::Client;
use crate::error::{Error, NO_HTTP_STATUS, Result};
use serde_json::Value;

/// A parsed record reference: the table plus either a sys_id or a number.
#[derive(Debug, PartialEq)]
pub(crate) struct RecordRef {
    pub table: String,
    pub id: RefId,
}

#[derive(Debug, PartialEq)]
pub(crate) enum RefId {
    SysId(String),
    Number(String),
}

impl std::fmt::Display for RecordRef {
    /// `table/sys_id` for a sys_id (the wording confirm prompts always used),
    /// `table:number` for a number — the guard names what the caller typed,
    /// because learning the sys_id would take a network call the guard must
    /// not make.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.id {
            RefId::SysId(id) => write!(f, "{}/{}", self.table, id),
            RefId::Number(n) => write!(f, "{}:{}", self.table, n),
        }
    }
}

/// Hardcoded number-prefix map (ITSM + Security Incident Response) for
/// `sn get`'s bare-number form. Instance-specific prefixes live in
/// `sys_number`; resolving those dynamically (with a cache) is a tracked
/// follow-up, not silently guessed here.
const NUMBER_PREFIXES: [(&str, &str); 9] = [
    ("CHG", "change_request"),
    ("CTASK", "change_task"),
    ("INC", "incident"),
    ("KB", "kb_knowledge"),
    ("PRB", "problem"),
    ("REQ", "sc_request"),
    ("RITM", "sc_req_item"),
    ("SCTASK", "sc_task"),
    ("SIR", "sn_si_incident"),
];

/// 32 ASCII hex chars — the shape of every platform-generated sys_id.
pub(crate) fn is_sys_id(s: &str) -> bool {
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Split and validate one `table:identifier` token. Splits on the first `:`
/// (a table name can never contain one), then validates each half on its own —
/// the identifier half through the same charset guard every sys_id gets, so
/// nothing spliced into an encoded query can carry a `:` or a `^`.
pub(crate) fn parse_ref(token: &str, what: &str) -> Result<RecordRef> {
    let Some((table, id)) = token.split_once(':') else {
        return Err(Error::Usage(format!(
            "'{token}' is not a {what}:identifier reference"
        )));
    };
    validate_identifier(table, what)?;
    validate_sys_id(id)?;
    let id = if is_sys_id(id) {
        RefId::SysId(id.to_string())
    } else {
        RefId::Number(id.to_string())
    };
    Ok(RecordRef {
        table: table.to_string(),
        id,
    })
}

/// The `(first, second)` positional pair every record-addressing command has.
/// Two tokens keep today's behavior exactly (second is the sys_id, verbatim);
/// one token must be a combined reference. A ref *and* a second token is
/// refused rather than picking a winner — either reading loses something
/// silently.
pub(crate) fn parse_pair(first: &str, second: Option<&str>, what: &str) -> Result<RecordRef> {
    match second {
        Some(sys_id) => {
            if first.contains(':') {
                return Err(Error::Usage(format!(
                    "give the record once: either `<{0}> <SYS_ID>` or a combined \
                     `<{0}>:<ID>` reference, not both",
                    what.to_uppercase()
                )));
            }
            validate_identifier(first, what)?;
            validate_sys_id(sys_id)?;
            Ok(RecordRef {
                table: first.to_string(),
                id: RefId::SysId(sys_id.to_string()),
            })
        }
        None => {
            if !first.contains(':') {
                return Err(Error::Usage(format!(
                    "missing SYS_ID: pass `<{0}> <SYS_ID>` or a combined \
                     `<{0}>:<SYS_ID|NUMBER>` reference (e.g. incident:INC0010001)",
                    what.to_uppercase()
                )));
            }
            parse_ref(first, what)
        }
    }
}

/// `attachment upload`'s flag pair: `--record` may carry the whole reference,
/// `--table` is the split form's other half.
pub(crate) fn parse_flag_pair(table: Option<&str>, record: &str) -> Result<RecordRef> {
    if record.contains(':') {
        if table.is_some() {
            return Err(Error::Usage(
                "give the table once: either --table with a bare --record sys_id, \
                 or a combined --record `table:id` reference, not both"
                    .into(),
            ));
        }
        return parse_ref(record, "table");
    }
    let Some(table) = table else {
        return Err(Error::Usage(
            "--table is required unless --record is a `table:identifier` reference".into(),
        ));
    };
    validate_identifier(table, "table")?;
    validate_sys_id(record)?;
    Ok(RecordRef {
        table: table.to_string(),
        id: RefId::SysId(record.to_string()),
    })
}

/// The table a bare number's prefix names, from the hardcoded map. The prefix
/// is the leading run of ASCII uppercase letters; matching the whole run is
/// the longest-prefix match (SCTASK's run is "SCTASK", never "SC").
pub(crate) fn table_for_number(number: &str) -> Option<&'static str> {
    let run: String = number
        .chars()
        .take_while(char::is_ascii_uppercase)
        .collect();
    if run.is_empty() {
        return None;
    }
    NUMBER_PREFIXES
        .iter()
        .find(|(p, _)| *p == run)
        .map(|(_, t)| *t)
}

/// `sn get`'s REF positional: a `table:identifier` reference or a bare number
/// with a known prefix. A bare sys_id names no table and is refused rather
/// than guessed.
pub(crate) fn parse_get_ref(token: &str) -> Result<RecordRef> {
    if token.contains(':') {
        return parse_ref(token, "table");
    }
    if is_sys_id(token) {
        return Err(Error::Usage(format!(
            "a bare sys_id names no table; use `table:{token}`"
        )));
    }
    validate_sys_id(token)?;
    match table_for_number(token) {
        Some(table) => Ok(RecordRef {
            table: table.to_string(),
            id: RefId::Number(token.to_string()),
        }),
        None => {
            let known: Vec<&str> = NUMBER_PREFIXES.iter().map(|(p, _)| *p).collect();
            Err(Error::Usage(format!(
                "'{token}' has no recognized number prefix (known: {}; prefixes are \
                 uppercase); use a `table:number` reference to name the table yourself",
                known.join(", ")
            )))
        }
    }
}

impl RecordRef {
    /// The record's sys_id — free for a sys_id reference, one Table API lookup
    /// for a number. See the module doc for the limit-2 canary this rides on.
    pub(crate) fn resolve(&self, client: &Client) -> Result<String> {
        let number = match &self.id {
            RefId::SysId(id) => return Ok(id.clone()),
            RefId::Number(n) => n,
        };
        let pairs = vec![
            ("sysparm_query".to_string(), format!("number={number}")),
            ("sysparm_fields".to_string(), "sys_id".to_string()),
            ("sysparm_limit".to_string(), "2".to_string()),
            // Pinned false so the sys_id comes back raw whatever the
            // instance's display-value defaults are.
            ("sysparm_display_value".to_string(), "false".to_string()),
        ];
        let resp = client.get(&format!("/api/now/table/{}", self.table), &pairs)?;
        let rows = resp
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match rows.len() {
            0 => Err(Error::Api {
                // The HTTP call succeeded; the *operation* found nothing. No
                // status is published rather than fabricating a 404.
                status: NO_HTTP_STATUS,
                message: format!("no {} record with number {number}", self.table),
                detail: None,
                transaction_id: None,
                sn_error: None,
            }),
            1 => rows[0]
                .pointer("/sys_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| Error::Instance {
                    message: format!(
                        "the {} record matching number {number} came back without a sys_id",
                        self.table
                    ),
                    detail: None,
                }),
            // `number` is unique where it exists, so a second row is proof the
            // instance dropped the term and returned unfiltered rows.
            _ => Err(Error::Instance {
                message: format!(
                    "cannot resolve {number}: the number={number} query term was dropped \
                     by the instance, so the rows returned are arbitrary"
                ),
                detail: Some(format!(
                    "{0} likely has no queryable `number` field; pass the record's \
                     sys_id instead ({0}:<sys_id>)",
                    self.table
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "1c741bd70b2322007518478d83673af3";

    #[test]
    fn sys_id_classification() {
        assert!(is_sys_id(HEX));
        assert!(is_sys_id(&HEX.to_uppercase()));
        assert!(!is_sys_id(&HEX[..31]));
        assert!(!is_sys_id(&format!("{HEX}0")));
        assert!(!is_sys_id("INC0010001"));
    }

    #[test]
    fn parse_ref_splits_and_classifies() {
        let r = parse_ref(&format!("incident:{HEX}"), "table").unwrap();
        assert_eq!(r.table, "incident");
        assert_eq!(r.id, RefId::SysId(HEX.into()));

        let r = parse_ref("incident:INC0010001", "table").unwrap();
        assert_eq!(r.id, RefId::Number("INC0010001".into()));
    }

    #[test]
    fn parse_ref_rejects_bad_halves() {
        // Empty halves.
        assert!(matches!(parse_ref(":abc", "table"), Err(Error::Usage(_))));
        assert!(matches!(
            parse_ref("incident:", "table"),
            Err(Error::Usage(_))
        ));
        // Split is on the FIRST colon, so the second lands in the identifier
        // half and fails its charset guard — never a silent truncation.
        assert!(matches!(parse_ref("a:b:c", "table"), Err(Error::Usage(_))));
        // Uppercase table name.
        assert!(matches!(
            parse_ref("Incident:abc", "table"),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn parse_pair_two_tokens_is_verbatim() {
        let r = parse_pair("incident", Some("abc"), "table").unwrap();
        assert_eq!(r.id, RefId::SysId("abc".into()));
    }

    #[test]
    fn parse_pair_ref_plus_second_token_is_refused() {
        let err = parse_pair(&format!("incident:{HEX}"), Some("abc"), "table").unwrap_err();
        assert!(err.to_string().contains("give the record once"), "{err}");
    }

    #[test]
    fn parse_pair_bare_single_token_names_both_forms() {
        let err = parse_pair("incident", None, "table").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("missing SYS_ID"), "{text}");
        assert!(text.contains("incident:INC0010001"), "{text}");
    }

    #[test]
    fn flag_pair_forms() {
        let r = parse_flag_pair(None, &format!("incident:{HEX}")).unwrap();
        assert_eq!(r.table, "incident");

        let r = parse_flag_pair(Some("incident"), "abc").unwrap();
        assert_eq!(r.id, RefId::SysId("abc".into()));

        assert!(matches!(
            parse_flag_pair(Some("incident"), "incident:abc"),
            Err(Error::Usage(_))
        ));
        let err = parse_flag_pair(None, "abc").unwrap_err();
        assert!(err.to_string().contains("--table is required"), "{err}");
    }

    #[test]
    fn prefix_map_matches_whole_uppercase_run() {
        for (prefix, table) in [
            ("CHG", "change_request"),
            ("CTASK", "change_task"),
            ("INC", "incident"),
            ("KB", "kb_knowledge"),
            ("PRB", "problem"),
            ("REQ", "sc_request"),
            ("RITM", "sc_req_item"),
            ("SCTASK", "sc_task"),
            ("SIR", "sn_si_incident"),
        ] {
            assert_eq!(table_for_number(&format!("{prefix}0010001")), Some(table));
        }
        // The run is matched whole: SCTASK is never truncated to SC, and an
        // unknown run maps to nothing.
        assert_eq!(table_for_number("TASK0010001"), None);
        assert_eq!(table_for_number("inc0010001"), None);
        assert_eq!(table_for_number("0010001"), None);
    }

    #[test]
    fn get_ref_forms() {
        assert_eq!(
            parse_get_ref("INC0010001").unwrap(),
            RecordRef {
                table: "incident".into(),
                id: RefId::Number("INC0010001".into()),
            }
        );
        assert_eq!(
            parse_get_ref(&format!("sys_user:{HEX}")).unwrap().table,
            "sys_user"
        );

        let err = parse_get_ref(HEX).unwrap_err();
        assert!(err.to_string().contains("names no table"), "{err}");

        let err = parse_get_ref("FOO0001").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("INC") && text.contains("SIR"), "{text}");
        assert!(text.contains("table:number"), "{text}");
    }

    #[test]
    fn display_names_what_the_caller_typed() {
        let r = parse_ref(&format!("incident:{HEX}"), "table").unwrap();
        assert_eq!(r.to_string(), format!("incident/{HEX}"));
        let r = parse_ref("incident:INC0010001", "table").unwrap();
        assert_eq!(r.to_string(), "incident:INC0010001");
    }
}
