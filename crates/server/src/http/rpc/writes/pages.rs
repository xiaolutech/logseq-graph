use super::super::error::RequestError;
use super::answer::{page_result, selector_result};
use super::params::string;
use super::{WriteMethod, WriteRequest};
use logseq_graph::engine::{Graph, WriteError};
use serde_json::Value;

pub(super) fn parse_create_page(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::CreatePage {
        name: string(method, args, 0, "name")?,
    })
}

pub(super) fn create_page(graph: &mut Graph, name: &str) -> Result<Value, WriteError> {
    graph.create_page(name).map(page_result)
}

pub(super) fn parse_delete_page(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::DeletePage {
        name: string(method, args, 0, "name")?,
    })
}

pub(super) fn delete_page(graph: &mut Graph, name: &str) -> Result<Value, WriteError> {
    graph.delete_page(name).map(page_result)
}

pub(super) fn parse_append_block(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::AppendBlock {
        page: string(method, args, 0, "page")?,
        text: string(method, args, 1, "text")?,
    })
}

pub(super) fn append_block(graph: &mut Graph, page: &str, text: &str) -> Result<Value, WriteError> {
    graph.append_block(page, text).map(page_result)
}

pub(super) fn parse_prepend_block_in_page(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::PrependBlockInPage {
        page: string(method, args, 0, "page")?,
        text: string(method, args, 1, "text")?,
    })
}

pub(super) fn prepend_block_in_page(
    graph: &mut Graph,
    page: &str,
    text: &str,
) -> Result<Value, WriteError> {
    graph.prepend_block_in_page(page, text).map(selector_result)
}
