use super::super::error::{params_error, RequestError};
use super::WriteMethod;
use logseq_graph::domain::BlockSelector;
use logseq_graph::engine::MoveTarget;
use serde_json::{json, Value};

pub(super) fn invalid(
    method: WriteMethod,
    message: impl Into<String>,
    detail: Value,
) -> RequestError {
    params_error(
        "invalid-write-params",
        format!("{}: {}", method.canonical(), message.into()),
        detail,
    )
}

pub(super) fn string(
    method: WriteMethod,
    args: &[Value],
    index: usize,
    name: &'static str,
) -> Result<String, RequestError> {
    args.get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid(
                method,
                format!("argument {index} ({name}) must be a string"),
                json!({"index": index, "parameter": name, "expected": "string"}),
            )
        })
}

pub(super) fn selector(
    method: WriteMethod,
    value: &Value,
    index: usize,
) -> Result<BlockSelector, RequestError> {
    let raw = value.as_str().ok_or_else(|| {
        invalid(
            method,
            format!("argument {index} (selector) must be a string"),
            json!({"index": index, "parameter": "selector", "expected": "string"}),
        )
    })?;
    crate::args::parse_selector(raw, "rpc").map_err(|_| {
        invalid(
            method,
            format!("{raw:?} is not a block selector"),
            json!({
                "index": index,
                "parameter": "selector",
                "value": raw,
                "expected": "<uuid> | durable:<uuid> | ephemeral:<uuid>",
            }),
        )
    })
}

pub(super) fn target(
    method: WriteMethod,
    value: &Value,
    index: usize,
) -> Result<MoveTarget, RequestError> {
    let object = value.as_object().ok_or_else(|| {
        invalid(
            method,
            format!("argument {index} (target) must be an object with one position"),
            json!({"index": index, "parameter": "target", "expected": "one position object"}),
        )
    })?;
    if object.len() != 1 {
        return Err(invalid(
            method,
            "target must contain exactly one of before, after, firstChildOf, lastChildOf",
            json!({
                "index": index,
                "parameter": "target",
                "allowed": ["before", "after", "firstChildOf", "lastChildOf"],
            }),
        ));
    }
    let (position, anchor) = object.iter().next().ok_or_else(|| {
        invalid(
            method,
            "target must contain one position",
            json!({"index": index, "parameter": "target"}),
        )
    })?;
    let selector = selector(method, anchor, index)?;
    match position.as_str() {
        "before" => Ok(MoveTarget::Before(selector)),
        "after" => Ok(MoveTarget::After(selector)),
        "firstChildOf" => Ok(MoveTarget::FirstChildOf(selector)),
        "lastChildOf" => Ok(MoveTarget::LastChildOf(selector)),
        _ => Err(invalid(
            method,
            format!("unknown move target {position:?}"),
            json!({
                "index": index,
                "parameter": "target",
                "value": position,
                "allowed": ["before", "after", "firstChildOf", "lastChildOf"],
            }),
        )),
    }
}
