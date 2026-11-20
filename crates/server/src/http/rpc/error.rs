use logseq_graph::domain::BlockId;
use logseq_graph::engine::{EngineError, WriteError};
use serde_json::{json, Map, Value};

use super::{methods, writes};

/// Why one request went unanswered.
///
/// Every variant carries the facts an agent needs in `data`: the class, the former HTTP status, and
/// the specific parameter, name or pages involved. Nothing here names a `PageDocument` or a SQL
/// column, because the adapter never sees either.
#[derive(Debug)]
pub(super) enum RequestError {
    /// The request carries no bearer token this server accepts (the former 401).
    Unauthorized,
    /// The body is not JSON.
    Parse(String),
    /// The request is not one `{method, args}` call at `POST /api`.
    InvalidRequest { message: String, data: Value },
    /// No method serves that name.
    MethodNotFound { method: String, leaf: String },
    /// An argument, option, selectivity or query was refused (the former 400).
    InvalidParams { message: String, data: Value },
    /// The page or block the request named has no derived rows (the former 404).
    NotFound {
        resource: &'static str,
        name: String,
    },
    /// More than one block states the durable id the request named (the former 409).
    Ambiguous { id: BlockId, pages: Vec<String> },
    /// The startup scan has not finished, so there is no graph to read yet (the former 503).
    Scanning { scanned: u64, total: u64 },
    /// A scan or an index read failed (the former 500).
    Internal(String),
    /// A library write was refused or only partly completed.
    Write(WriteError),
}

impl RequestError {
    /// The JSON-RPC-compatible error `code`.
    pub(super) fn code(&self) -> i32 {
        match self {
            Self::Unauthorized => -32001,
            Self::Parse(_) => -32700,
            Self::InvalidRequest { .. } => -32600,
            Self::MethodNotFound { .. } => -32601,
            Self::InvalidParams { .. } => -32602,
            Self::Internal(_) => -32603,
            Self::Scanning { .. } => -32003,
            Self::NotFound { .. } => -32004,
            Self::Ambiguous { .. } => -32009,
            Self::Write(error) => writes::error_code(error),
        }
    }

    /// The former HTTP status, kept as a transport hint and stated in `data`.
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::NotFound { .. } => 404,
            Self::Ambiguous { .. } => 409,
            Self::Internal(_) => 500,
            Self::Scanning { .. } => 503,
            Self::Write(error) => writes::error_status(error),
            _ => 400,
        }
    }

    /// The one-line human message.
    pub(super) fn message(&self) -> String {
        match self {
            Self::Unauthorized => "Access Denied!".to_owned(),
            Self::Parse(reason) => format!("the request body is not JSON: {reason}"),
            Self::InvalidRequest { message, .. } | Self::InvalidParams { message, .. } => {
                message.clone()
            }
            Self::MethodNotFound { leaf, .. } => {
                format!("MethodNotExist: {leaf}")
            }
            Self::NotFound { resource, name } => format!("{resource} not found: {name}"),
            Self::Ambiguous { id, pages } => format!(
                "the id {id} is stated by more than one block; the pages involved are {}",
                pages.join(", ")
            ),
            Self::Scanning { scanned, total } => format!(
                "the graph is still being indexed: {scanned} of {total} files scanned so far"
            ),
            Self::Internal(reason) => reason.clone(),
            Self::Write(error) => error.to_string(),
        }
    }

    /// The structured `data` member of the error object.
    pub(super) fn data(&self) -> Value {
        let mut fields = Map::new();
        fields.insert("http_status".to_owned(), json!(self.status()));
        match self {
            Self::Unauthorized => {
                fields.insert("kind".to_owned(), json!("unauthorized"));
            }
            Self::Parse(reason) => {
                fields.insert("kind".to_owned(), json!("parse-error"));
                fields.insert("reason".to_owned(), json!(reason));
            }
            Self::InvalidRequest { data, .. } | Self::InvalidParams { data, .. } => {
                let default = if matches!(self, Self::InvalidRequest { .. }) {
                    "invalid-request"
                } else {
                    "invalid-params"
                };
                fields.insert("kind".to_owned(), json!(default));
                if let Value::Object(extra) = data {
                    fields.extend(extra.clone());
                }
            }
            Self::MethodNotFound { method, leaf } => {
                fields.insert("kind".to_owned(), json!("method-not-found"));
                fields.insert("method".to_owned(), json!(method));
                fields.insert("leaf".to_owned(), json!(leaf));
                fields.insert("methods".to_owned(), json!(methods::read_names()));
                fields.insert("write_methods".to_owned(), json!(methods::write_names()));
            }
            Self::NotFound { resource, name } => {
                fields.insert("kind".to_owned(), json!("not-found"));
                fields.insert("resource".to_owned(), json!(resource));
                fields.insert("name".to_owned(), json!(name));
            }
            Self::Ambiguous { id, pages } => {
                fields.insert("kind".to_owned(), json!("ambiguous-block-id"));
                fields.insert("id".to_owned(), json!(id.to_string()));
                fields.insert("pages".to_owned(), json!(pages));
            }
            Self::Scanning { scanned, total } => {
                fields.insert("kind".to_owned(), json!("scanning"));
                fields.insert("scanning".to_owned(), json!(true));
                fields.insert("scanned".to_owned(), json!(scanned));
                fields.insert("total".to_owned(), json!(total));
            }
            Self::Internal(reason) => {
                fields.insert("kind".to_owned(), json!("internal"));
                fields.insert("reason".to_owned(), json!(reason));
            }
            Self::Write(error) => {
                if let Value::Object(extra) = writes::error_data(error) {
                    fields.extend(extra);
                }
            }
        }
        Value::Object(fields)
    }

    /// The error body this surface answers with.
    ///
    /// The official server states Fastify's `{statusCode, error, message}` on a failed request (and a
    /// bare `null` or `{error: "MethodNotExist: ..."}` on an application-level miss at 200). This
    /// body keeps the Fastify members and adds this surface's own `code` and structured `data` beside
    /// them: an official client reads the transport status and ignores the body, so the extras cost
    /// nothing and keep every error class discoverable (ADR-0014, ADR-0018).
    pub(super) fn error_body(&self) -> Value {
        json!({
            "statusCode": self.status(),
            "error": reason_phrase(self.status()),
            "message": self.message(),
            "code": self.code(),
            "data": self.data(),
        })
    }
}

/// The reason phrase for one status — the single table both the HTTP status line and the
/// Fastify `error` member read, so a refused request never names `"Error"` where the line states
/// a real phrase.
pub(super) fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// An invalid-params error whose `data` names the class, the reason and the offending facts.
pub(super) fn params_error(
    kind: &'static str,
    message: impl Into<String>,
    detail: Value,
) -> RequestError {
    let message = message.into();
    let mut fields = Map::new();
    fields.insert("kind".to_owned(), json!(kind));
    fields.insert("reason".to_owned(), Value::String(message.clone()));
    if let Value::Object(extra) = detail {
        fields.extend(extra);
    }
    RequestError::InvalidParams {
        message,
        data: Value::Object(fields),
    }
}

/// An invalid-request error: the call did not arrive as one object at `POST /api`.
pub(super) fn request_error(message: impl Into<String>, detail: Value) -> RequestError {
    let message = message.into();
    let mut fields = Map::new();
    fields.insert("kind".to_owned(), json!("invalid-request"));
    fields.insert("reason".to_owned(), Value::String(message.clone()));
    fields.insert(
        "expected".to_owned(),
        json!("POST /api with one JSON object {method, args}"),
    );
    if let Value::Object(extra) = detail {
        fields.extend(extra);
    }
    RequestError::InvalidRequest {
        message,
        data: Value::Object(fields),
    }
}

/// The named resource has no derived rows.
pub(super) fn not_found(resource: &'static str, name: impl Into<String>) -> RequestError {
    RequestError::NotFound {
        resource,
        name: name.into(),
    }
}

/// Map an engine failure onto the one error the adapter owes a caller.
pub(super) fn internal(error: EngineError) -> RequestError {
    RequestError::Internal(error.to_string())
}

/// Map a query failure onto the error a caller can act on.
///
/// A query the grammar refuses is the request's own text being wrong: a structured invalid-params
/// error naming the query. An empty answer would read as "no block matches", and answering an
/// internal error would tell a caller to retry or report a defect when neither would help.
pub(super) fn query_error(error: EngineError, query: &str) -> RequestError {
    match error {
        EngineError::Query(error) => params_error(
            "invalid-query",
            format!("unanswerable query: {error}"),
            json!({
                "parameter": "query",
                "value": query,
                "reason": error.to_string(),
            }),
        ),
        other => internal(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_the_transport_emits_names_its_reason() {
        for status in [
            200, 204, 400, 401, 403, 404, 408, 409, 413, 415, 431, 500, 503,
        ] {
            assert_ne!(reason_phrase(status), "Error", "{status}");
        }
    }
}
