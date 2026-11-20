use super::super::error::RequestError;
use super::answer::block_result;
use super::params::{selector, string};
use super::{WriteMethod, WriteRequest};
use logseq_graph::domain::BlockSelector;
use logseq_graph::engine::{Graph, WriteError};
use serde_json::Value;

pub(super) fn ensure_block_id(
    graph: &mut Graph,
    selector: &BlockSelector,
) -> Result<Value, WriteError> {
    graph
        .ensure_block_id(selector)
        .map(super::answer::ensured_id_result)
}

pub(super) fn parse_upsert_block_property(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::UpsertBlockProperty {
        selector: selector(method, &args[0], 0)?,
        key: string(method, args, 1, "key")?,
        value: string(method, args, 2, "value")?,
    })
}

pub(super) fn upsert_block_property(
    graph: &mut Graph,
    selector: &BlockSelector,
    key: &str,
    value: &str,
) -> Result<Value, WriteError> {
    graph
        .upsert_block_property(selector, key, value)
        .map(block_result)
}

pub(super) fn parse_remove_block_property(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::RemoveBlockProperty {
        selector: selector(method, &args[0], 0)?,
        key: string(method, args, 1, "key")?,
    })
}

pub(super) fn remove_block_property(
    graph: &mut Graph,
    selector: &BlockSelector,
    key: &str,
) -> Result<Value, WriteError> {
    graph.remove_block_property(selector, key).map(block_result)
}
