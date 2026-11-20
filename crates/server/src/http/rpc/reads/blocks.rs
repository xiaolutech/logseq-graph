use logseq_graph::domain::{page_name_key, BlockSelector, EphemeralId};
use logseq_graph::engine::EngineError;
use serde::Serialize;
use serde_json::{json, Value};

use super::super::answer::{answered, block_entry, block_id, marker_text, page_id, BlockEntry};
use super::super::call::{one_argument, Call};
use super::super::error::{internal, not_found, params_error, RequestError};
use super::super::state::Served;

/// Map a block lookup failure: an `id::` stated by more than one block is the caller's ambiguity to
/// resolve, and is the one lookup error this surface reports as itself rather than as internal.
fn block_lookup_error(error: EngineError) -> RequestError {
    match error {
        EngineError::AmbiguousBlockId { id, pages } => RequestError::Ambiguous { id, pages },
        other => internal(other),
    }
}

/// A block's page, in the shape an official client uses it: the client hands `block.page` straight
/// back to `Editor.getPage`, which reads `name` / `uuid` / `id` off an object. A page name string
/// would make that call send `[null]`. A numeric entity id is absent (ADR-0001).
#[derive(Serialize)]
struct BlockPageRef {
    name: String,
    uuid: String,
    /// The numeric id `Editor.getPage({id})` accepts for this page.
    id: u64,
}

#[derive(Serialize)]
struct GetBlockResult {
    /// The official top-level block fields the official client reads: identity, markdown content,
    /// the page reference, the marker, and the format.
    uuid: String,
    content: String,
    page: BlockPageRef,
    marker: String,
    format: &'static str,
    /// This surface's own block view, kept beside the official fields.
    block: BlockEntry,
}

pub(in crate::http::rpc) fn get_block(served: &Served, call: &Call) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    // A selector, because a query answer states one and this method is how a caller reads
    // the block it names. The official answer states a bare id and no identity kind, so a
    // bare id is read as durable first and then as ephemeral: a search hit's `block/uuid`
    // must round-trip through this method (ADR-0012 mints the ephemeral ids).
    let raw = one_argument(call)?;
    let selector = crate::args::parse_selector(raw, "getBlock").map_err(|_| {
        params_error(
            "invalid-selector",
            format!(
                "{raw:?} is not a block selector; write <uuid> for a durable id or \
                 ephemeral:<uuid> for an ephemeral one"
            ),
            json!({
                "parameter": "selector",
                "value": raw,
                "expected": "<uuid> | durable:<uuid> | ephemeral:<uuid>",
            }),
        )
    })?;
    let bare = !raw.starts_with(crate::args::DURABLE_PREFIX)
        && !raw.starts_with(crate::args::EPHEMERAL_PREFIX);
    let ephemeral = bare
        .then(|| EphemeralId::parse(raw).ok())
        .flatten()
        .map(BlockSelector::Ephemeral);
    let (block, meta) = served.read(|graph| {
        let mut reads = graph.reads(freshness);
        let read = reads.block(&selector).map_err(block_lookup_error)?;
        let meta = read.meta().clone();
        let mut found = read.into_value();
        if found.is_none() {
            if let Some(ephemeral) = &ephemeral {
                found = reads
                    .block(ephemeral)
                    .map_err(block_lookup_error)?
                    .into_value();
            }
        }
        Ok((found, meta))
    })?;
    let Some(block) = block else {
        return Err(not_found("block", crate::render::selector(&selector)));
    };
    let entry = block_entry(&block);
    Ok(answered((
        GetBlockResult {
            uuid: block_id(block.selector()),
            content: block.text().to_owned(),
            page: BlockPageRef {
                name: block.page().to_owned(),
                uuid: page_name_key(block.page()),
                id: page_id(block.page()),
            },
            marker: marker_text(block.marker()),
            format: "markdown",
            block: entry,
        },
        meta,
    )))
}
