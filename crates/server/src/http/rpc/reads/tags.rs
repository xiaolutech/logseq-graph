use serde::Serialize;
use serde_json::Value;

use super::super::answer::answered;
use super::super::call::{one_argument, Call};
use super::super::error::{internal, RequestError};
use super::super::state::Served;

/// One tag and how many pages use it.
#[derive(Serialize)]
struct TagUsageEntry {
    name: String,
    page_count: u32,
}

#[derive(Serialize)]
struct GetAllTagsResult {
    tags: Vec<TagUsageEntry>,
}

#[derive(Serialize)]
struct GetPagesWithTagResult {
    pages: Vec<String>,
}

pub(in crate::http::rpc) fn get_all_tags(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let (tags, meta) = served.read(|graph| {
        let read = graph.reads(freshness).tags().map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let list = tags
        .iter()
        .map(|tag| TagUsageEntry {
            name: tag.name().to_owned(),
            page_count: tag.page_count(),
        })
        .collect();
    Ok(answered((GetAllTagsResult { tags: list }, meta)))
}

pub(in crate::http::rpc) fn get_pages_with_tag(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let tag = one_argument(call)?;
    let (pages, meta) = served.read(|graph| {
        let read = graph
            .reads(freshness)
            .pages_with_tag(tag)
            .map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    Ok(answered((GetPagesWithTagResult { pages }, meta)))
}
