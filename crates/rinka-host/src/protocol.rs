//! Versioned control protocol between the editor and the playback host.
//!
//! Transport: newline-delimited JSON over a local socket. Each request is an
//! object with a client-chosen `id` and a `cmd`; the host answers each request
//! with exactly one response carrying the same `id`:
//!
//! ```text
//! -> {"id":1,"cmd":"hello","protocol":1,"client":"rinka-app"}
//! <- {"id":1,"ok":true,"result":{"protocol":1,...}}
//! -> {"id":2,"cmd":"assign","output":"DP-2","project":"/path/to/project"}
//! <- {"id":2,"ok":false,"error":{"code":"unknown_output","message":"..."}}
//! ```
//!
//! Rendered frames never travel over this channel.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Must be the first request on a connection.
    Hello {
        protocol: u32,
        client: Option<String>,
    },
    /// GPU, presentation and input capabilities of this host.
    Capabilities,
    /// Connected outputs and their assignments.
    Outputs,
    /// Shows a project on one output, or on every connected output when
    /// `output` is omitted. Assignments survive output reconnection.
    Assign {
        output: Option<String>,
        project: PathBuf,
    },
    /// Removes the wallpaper from one output, or from all outputs.
    Unassign {
        output: Option<String>,
    },
    /// Re-reads a project from disk on every output showing it.
    Reload {
        project: PathBuf,
    },
    /// Pauses video playback on every output.
    Pause,
    Resume,
    /// Jumps videos to `position` seconds, on one output or on all.
    Seek {
        output: Option<String>,
        position: f64,
    },
    /// Stops the host and removes all wallpaper surfaces.
    Shutdown,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    HelloRequired,
    UnsupportedProtocol,
    UnknownOutput,
    Project,
    Gpu,
}

pub type Reply = Result<Value, Error>;

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Response {
    pub fn new(id: Value, reply: Reply) -> Self {
        match reply {
            Ok(result) => Self {
                id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                id,
                ok: false,
                result: None,
                error: Some(error),
            },
        }
    }
}

/// Splits a request line into its id and request. The id is echoed back even
/// when the request itself is malformed.
pub fn parse_line(line: &str) -> (Value, Result<Request, Error>) {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return (
                Value::Null,
                Err(Error::new(ErrorCode::BadRequest, e.to_string())),
            );
        }
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let request =
        serde_json::from_value(value).map_err(|e| Error::new(ErrorCode::BadRequest, e.to_string()));
    (id, request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests_and_keeps_ids() {
        let (id, req) = parse_line(r#"{"id":7,"cmd":"assign","project":"/p"}"#);
        assert_eq!(id, 7);
        assert!(matches!(req, Ok(Request::Assign { output: None, .. })));

        let (id, req) = parse_line(r#"{"id":"x","cmd":"bogus"}"#);
        assert_eq!(id, "x");
        assert_eq!(req.unwrap_err().code, ErrorCode::BadRequest);

        let (id, req) = parse_line("not json");
        assert_eq!(id, Value::Null);
        assert!(req.is_err());
    }

    #[test]
    fn serializes_responses() {
        let ok = Response::new(1.into(), Ok(Value::Null));
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":1,"ok":true,"result":null}"#
        );
        let err = Response::new(2.into(), Err(Error::new(ErrorCode::UnknownOutput, "DP-9")));
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"id":2,"ok":false,"error":{"code":"unknown_output","message":"DP-9"}}"#
        );
    }
}
