use super::super::error::RequestError;
use super::answer::{block_result, page_result, selector_result};
use super::params::{invalid, selector, string, target};
use super::{WriteMethod, WriteRequest};
use logseq_graph::domain::BlockSelector;
use logseq_graph::engine::{Graph, MoveTarget, WriteError};
use serde_json::{json, Value};

pub(super) fn parse_insert_block(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::InsertBlock {
        target: target(method, &args[0], 0)?,
        text: string(method, args, 1, "text")?,
    })
}

pub(super) fn insert_block(
    graph: &mut Graph,
    target: &MoveTarget,
    text: &str,
) -> Result<Value, WriteError> {
    graph
        .insert_block(target.clone(), text)
        .map(selector_result)
}

pub(super) fn parse_update_block(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::UpdateBlock {
        selector: selector(method, &args[0], 0)?,
        text: string(method, args, 1, "text")?,
    })
}

pub(super) fn update_block(
    graph: &mut Graph,
    selector: &BlockSelector,
    text: &str,
) -> Result<Value, WriteError> {
    graph.update_block(selector, text).map(block_result)
}

pub(super) fn parse_remove_block(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::RemoveBlock {
        selector: selector(method, &args[0], 0)?,
    })
}

pub(super) fn remove_block(
    graph: &mut Graph,
    selector: &BlockSelector,
) -> Result<Value, WriteError> {
    graph.remove_block(selector).map(page_result)
}

pub(super) fn parse_set_block_collapsed(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::SetBlockCollapsed {
        selector: selector(method, &args[0], 0)?,
        collapsed: args[1].as_bool().ok_or_else(|| {
            invalid(
                method,
                "argument 1 (collapsed) must be a boolean",
                json!({"index": 1, "parameter": "collapsed", "expected": "boolean"}),
            )
        })?,
    })
}

pub(super) fn set_block_collapsed(
    graph: &mut Graph,
    selector: &BlockSelector,
    collapsed: &bool,
) -> Result<Value, WriteError> {
    graph
        .set_block_collapsed(selector, *collapsed)
        .map(block_result)
}
