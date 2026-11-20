use super::super::error::RequestError;
use super::answer::page_result;
use super::params::{invalid, string};
use super::{WriteMethod, WriteRequest};
use logseq_graph::domain::JournalDate;
use logseq_graph::engine::{Graph, WriteError};
use serde_json::{json, Value};

pub(super) fn parse_create_journal(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    let raw = string(method, args, 0, "date")?;
    let date = crate::render::parse_day(&raw).ok_or_else(|| {
        invalid(
            method,
            format!("date must be YYYY-MM-DD, not {raw:?}"),
            json!({"index": 0, "parameter": "date", "value": raw}),
        )
    })?;
    Ok(WriteRequest::CreateJournal { date })
}

pub(super) fn create_journal(graph: &mut Graph, date: &JournalDate) -> Result<Value, WriteError> {
    graph.create_journal(*date).map(page_result)
}
