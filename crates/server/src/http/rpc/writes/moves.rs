use super::super::error::RequestError;
use super::answer::page_result;
use super::params::{invalid, selector, target};
use super::{WriteMethod, WriteRequest};
use logseq_graph::domain::BlockSelector;
use logseq_graph::engine::{Graph, MoveTarget, WriteError};
use serde_json::{json, Value};

pub(super) fn parse_move_block(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    let moved = selector(method, &args[0], 0)?;
    let target = if args.len() == 2 {
        target(method, &args[1], 1)?
    } else {
        let direction = args[1].as_str().ok_or_else(|| {
            invalid(
                method,
                "argument 1 (direction) must be a string",
                json!({"index": 1, "parameter": "direction", "expected": "string"}),
            )
        })?;
        let anchor = selector(method, &args[2], 2)?;
        match direction {
            "before" => MoveTarget::Before(anchor),
            "after" => MoveTarget::After(anchor),
            "first_child_of" => MoveTarget::FirstChildOf(anchor),
            "last_child_of" => MoveTarget::LastChildOf(anchor),
            _ => {
                return Err(invalid(
                    method,
                    format!("unknown move direction {direction:?}"),
                    json!({
                        "index": 1,
                        "parameter": "direction",
                        "value": direction,
                        "allowed": ["before", "after", "first_child_of", "last_child_of"],
                    }),
                ));
            }
        }
    };
    Ok(WriteRequest::MoveBlock {
        selector: moved,
        target,
    })
}

pub(super) fn move_block(
    graph: &mut Graph,
    selector: &BlockSelector,
    target: &MoveTarget,
) -> Result<Value, WriteError> {
    graph.move_block(selector, target.clone()).map(page_result)
}
