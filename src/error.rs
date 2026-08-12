use serde::Serialize;
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `Error::Api::status` for a failure that never carried an HTTP status: a CICD
/// operation the instance reports as failed *inside* a 200 response body. The
/// stderr envelope then omits `status_code` entirely rather than publishing a
/// fake `0` — agents branch on that key, and no HTTP response can carry 0.
///
/// A sentinel rather than `Option<u16>` because every other construction site of
/// `Error::Api` does have a real status; the option would be `Some(..)` noise at
/// all of them.
pub const NO_HTTP_STATUS: u16 = 0;

/// `" (404)"` for a real HTTP status, `""` for [`NO_HTTP_STATUS`], so the
/// `Display` text never claims a status the failure did not have.
fn status_suffix(status: u16) -> String {
    if status == NO_HTTP_STATUS {
        String::new()
    } else {
        format!(" ({status})")
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("usage error: {0}")]
    Usage(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("API error{}: {message}", status_suffix(*status))]
    Api {
        status: u16,
        message: String,
        detail: Option<String>,
        transaction_id: Option<String>,
        sn_error: Option<serde_json::Value>,
    },

    #[error("auth error ({status}): {message}")]
    Auth {
        status: u16,
        message: String,
        transaction_id: Option<String>,
    },

    #[error("transport error: {0}")]
    Transport(String),

    /// Downstream consumer closed the pipe (e.g. `sn … | head`). Silent exit 0.
    #[error("broken pipe")]
    BrokenPipe,

    /// The instance returned HTTP success, but the response does not answer what
    /// was asked and must not be handed back as if it did — a scripted query
    /// term the instance silently dropped, a filter that never applied.
    ///
    /// Deliberately carries **no** status: there was no HTTP error, so any
    /// `status_code` here would be a fabrication (`status_code: 200` on a
    /// failure), and agents branch on that field. Same exit code as [`Error::Api`]
    /// — this is still the instance failing to serve the request — but the key is
    /// absent from the stderr envelope rather than lying.
    #[error("instance error: {message}")]
    Instance {
        message: String,
        detail: Option<String>,
    },
}

impl Error {
    /// Map each error variant to the exit code defined in spec §6.5.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::BrokenPipe => 0,
            Error::Usage(_) | Error::Config(_) => 1,
            Error::Api { .. } => 2,
            Error::Transport(_) => 3,
            Error::Auth { .. } => 4,
            Error::Instance { .. } => 2,
        }
    }

    /// JSON envelope matching spec §6.4.
    pub fn to_stderr_json(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct Envelope<'a> {
            error: Inner<'a>,
        }
        #[derive(Serialize)]
        struct Inner<'a> {
            message: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            detail: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            status_code: Option<u16>,
            #[serde(skip_serializing_if = "Option::is_none")]
            transaction_id: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            sn_error: Option<&'a serde_json::Value>,
        }
        let (message, detail, status_code, tx, sn) = match self {
            Error::BrokenPipe => ("broken pipe".to_string(), None, None, None, None),
            Error::Usage(m) => (m.clone(), None, None, None, None),
            Error::Config(m) => (m.clone(), None, None, None, None),
            Error::Api {
                status,
                message,
                detail,
                transaction_id,
                sn_error,
            } => (
                message.clone(),
                detail.as_deref(),
                (*status != NO_HTTP_STATUS).then_some(*status),
                transaction_id.as_deref(),
                sn_error.as_ref(),
            ),
            Error::Auth {
                status,
                message,
                transaction_id,
            } => (
                message.clone(),
                None,
                Some(*status),
                transaction_id.as_deref(),
                None,
            ),
            Error::Transport(m) => (m.clone(), None, None, None, None),
            // No `status_code`: see the variant's doc comment.
            Error::Instance { message, detail } => {
                (message.clone(), detail.as_deref(), None, None, None)
            }
        };
        serde_json::to_value(Envelope {
            error: Inner {
                message,
                detail,
                status_code,
                transaction_id: tx,
                sn_error: sn,
            },
        })
        .expect("envelope should serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(Error::Usage("x".into()).exit_code(), 1);
        assert_eq!(Error::Config("x".into()).exit_code(), 1);
        assert_eq!(
            Error::Api {
                status: 400,
                message: "x".into(),
                detail: None,
                transaction_id: None,
                sn_error: None
            }
            .exit_code(),
            2
        );
        assert_eq!(Error::Transport("x".into()).exit_code(), 3);
        assert_eq!(
            Error::Auth {
                status: 401,
                message: "x".into(),
                transaction_id: None
            }
            .exit_code(),
            4
        );
    }

    #[test]
    fn stderr_envelope_includes_optional_fields() {
        let e = Error::Api {
            status: 404,
            message: "not found".into(),
            detail: Some("no record".into()),
            transaction_id: Some("tx1".into()),
            sn_error: Some(serde_json::json!({"message": "nope"})),
        };
        let v = e.to_stderr_json();
        assert_eq!(v["error"]["message"], "not found");
        assert_eq!(v["error"]["detail"], "no record");
        assert_eq!(v["error"]["status_code"], 404);
        assert_eq!(v["error"]["transaction_id"], "tx1");
        assert_eq!(v["error"]["sn_error"]["message"], "nope");
    }

    #[test]
    fn stderr_envelope_omits_status_code_for_statusless_api_errors() {
        // A failed CICD wait: the HTTP call succeeded, the *operation* failed. The
        // key must be absent, not null and not 0 — agents branch on its presence.
        let e = Error::Api {
            status: NO_HTTP_STATUS,
            message: "update set commit failed".into(),
            detail: None,
            transaction_id: None,
            sn_error: None,
        };
        let v = e.to_stderr_json();
        assert!(
            v["error"].get("status_code").is_none(),
            "status_code must be absent: {v}"
        );
        assert_eq!(e.exit_code(), 2);
        assert_eq!(e.to_string(), "API error: update set commit failed");
    }

    /// The sibling mechanism: `Api` with [`NO_HTTP_STATUS`] is an HTTP call that
    /// succeeded around a failed *operation*; `Instance` is an HTTP 200 whose
    /// body does not answer the question. Different causes, same contract — no
    /// invented `status_code` — so both are pinned.
    #[test]
    fn instance_errors_carry_no_status_code() {
        // An HTTP 200 whose body is untrustworthy is not an HTTP failure. Emitting
        // `status_code: 200` on it describes an error that never happened, and
        // agents branch on that field — so the key must be absent entirely.
        let e = Error::Instance {
            message: "the query was not evaluated".into(),
            detail: Some("scripted query evaluation appears to be disabled".into()),
        };
        assert_eq!(e.exit_code(), 2);
        let v = e.to_stderr_json();
        assert_eq!(v["error"]["message"], "the query was not evaluated");
        assert_eq!(
            v["error"]["detail"],
            "scripted query evaluation appears to be disabled"
        );
        assert!(
            v["error"].get("status_code").is_none(),
            "a non-HTTP failure must not report an HTTP status: {v}"
        );
    }

    #[test]
    fn stderr_envelope_omits_none_fields() {
        let e = Error::Transport("dns".into());
        let v = e.to_stderr_json();
        assert!(v["error"].get("status_code").is_none());
        assert!(v["error"].get("sn_error").is_none());
    }
}
