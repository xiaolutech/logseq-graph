use serde::Serialize;
use serde_json::Value;

use logseq_graph::domain::page_name_key;

use super::super::answer::{
    answered, block_entry, page_id, snapshot_entries, BlockEntry, PageEntry, PropertyEntry,
};
use super::super::call::{one_argument, Call};
use super::super::error::{internal, not_found, RequestError};
use super::super::state::Served;

/// One backlink: the referencing page, the block's position, and its text.
#[derive(Serialize)]
struct BacklinkEntry {
    page: String,
    preorder: u32,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pre_block: Option<bool>,
}

#[derive(Serialize)]
struct GetPageResult {
    /// The official top-level page fields the official client reads off the answer: folded name,
    /// the identity uuid, and the numeric id its `{id}` argument round-trips through.
    name: String,
    /// The page name as its file spells it; the official answer keeps it beside the folded `name`.
    #[serde(rename = "originalName")]
    original_name: String,
    uuid: String,
    id: u64,
    /// The official `journal?` boolean a client branches on.
    #[serde(rename = "journal?")]
    journal: bool,
    /// This projection's own page view, kept beside the official fields.
    page: PageEntry,
    revision: u64,
    properties: Vec<PropertyEntry>,
}

#[derive(Serialize)]
struct GetPageBlocksTreeResult {
    blocks: Vec<BlockEntry>,
}

#[derive(Serialize)]
struct GetPageLinkedReferencesResult {
    backlinks: Vec<BacklinkEntry>,
}

/// Every field is always stated, so a caller branches on `exists` rather than on a key that
/// appears. Two versions rather than one, because one cannot say which side moved.
#[derive(Serialize)]
struct GetPageVersionResult {
    exists: bool,
    index_version: Option<String>,
    disk_version: Option<String>,
}

/// One page as its file holds it now, in the shapes the rest of the surface already uses.
#[derive(Serialize)]
struct GetPageSnapshotResult {
    exists: bool,
    version: Option<String>,
    properties: Vec<PropertyEntry>,
    blocks: Vec<BlockEntry>,
}

/// Resolve the one name a page read was given. The official client also addresses a page by the
/// numeric `id` this surface assigns to the folded name; when the call carried that identity, the
/// name is the page whose hash it is.
fn resolve_page_name(
    reads: &mut logseq_graph::engine::Reads<'_>,
    name: &str,
    page_id_identity: Option<u64>,
) -> Result<String, RequestError> {
    match page_id_identity {
        Some(id) => {
            let pages = reads.pages().map_err(internal)?;
            pages
                .value()
                .iter()
                .map(|page| page.name().to_owned())
                .find(|page| page_id(page) == id)
                .ok_or_else(|| not_found("page", name))
        }
        None => Ok(name.to_owned()),
    }
}

pub(in crate::http::rpc) fn get_page(served: &Served, call: &Call) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let name = one_argument(call)?;
    let page_id_identity = call.page_id;
    served
        .read(|graph| {
            let mut reads = graph.reads(freshness);
            let name = resolve_page_name(&mut reads, name, page_id_identity)?;
            let metadata = reads.page_metadata(&name).map_err(internal)?;
            let meta = metadata.meta().clone();
            let Some(page) = metadata.into_value() else {
                return Err(not_found("page", name));
            };
            let properties = reads.page_properties(&name).map_err(internal)?;
            let properties = properties
                .value()
                .as_ref()
                .map(|properties| {
                    properties
                        .iter()
                        .map(|property| PropertyEntry {
                            key: property.key().to_owned(),
                            value: property.value().to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok((
                GetPageResult {
                    name: page_name_key(page.name()),
                    original_name: page.name().to_owned(),
                    uuid: page_name_key(page.name()),
                    id: page_id(page.name()),
                    journal: page.journal_date().is_some(),
                    page: PageEntry {
                        name: page.name().to_owned(),
                        id: page_id(page.name()),
                        kind: page.kind().as_str(),
                        journal_date: page.journal_date().map(|day| day.to_string()),
                        block_count: page.block_count(),
                        has_file: page.has_file(),
                    },
                    revision: page.revision(),
                    properties,
                },
                meta,
            ))
        })
        .map(answered)
}

pub(in crate::http::rpc) fn get_page_blocks_tree(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let name = one_argument(call)?;
    served
        .read(|graph| {
            let read = graph.reads(freshness).page_blocks(name).map_err(internal)?;
            let meta = read.meta().clone();
            let Some(blocks) = read.into_value() else {
                return Err(not_found("page", name));
            };
            let entries = blocks.iter().map(block_entry).collect();
            Ok((GetPageBlocksTreeResult { blocks: entries }, meta))
        })
        .map(answered)
}

pub(in crate::http::rpc) fn get_page_linked_references(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let name = one_argument(call)?;
    let (backlinks, meta) = served.read(|graph| {
        let read = graph.reads(freshness).backlinks(name).map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let list = backlinks
        .iter()
        .map(|backlink| BacklinkEntry {
            page: backlink.page().to_owned(),
            preorder: backlink.preorder(),
            text: backlink.text().to_owned(),
            pre_block: backlink.pre_block().then_some(true),
        })
        .collect();
    Ok(answered((
        GetPageLinkedReferencesResult { backlinks: list },
        meta,
    )))
}

pub(in crate::http::rpc) fn get_page_version(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    // The one file-tier read with no freshness policy at all. `parse_call` has already
    // refused any option, so there is no policy here to skip the file read with.
    let name = one_argument(call)?;
    let (versions, meta) = served.read(|graph| {
        let read = graph.page_versions(name).map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let Some(versions) = versions else {
        return Ok(answered((
            GetPageVersionResult {
                exists: false,
                index_version: None,
                disk_version: None,
            },
            meta,
        )));
    };
    Ok(answered((
        GetPageVersionResult {
            // `exists` is the file's side: a page deleted since the scan keeps its index
            // version but has no file to poll, and that third state is not the unknown
            // name above.
            exists: versions.disk().is_some(),
            index_version: Some(versions.index().as_str().to_owned()),
            disk_version: versions.disk().map(|disk| disk.as_str().to_owned()),
        },
        meta,
    )))
}

pub(in crate::http::rpc) fn get_page_snapshot(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    // The other file-tier read with no policy. `page_snapshot` locates the page's file
    // through its page row, so a page written since the last scan answers `exists: false`
    // until a scan projects it; a file that cannot be decoded is an internal error with the
    // reason, never an empty page.
    let name = one_argument(call)?;
    let (snapshot, meta) = served.read(|graph| {
        let read = graph.page_snapshot(name).map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let Some(snapshot) = snapshot else {
        return Ok(answered((
            GetPageSnapshotResult {
                exists: false,
                version: None,
                properties: Vec::new(),
                blocks: Vec::new(),
            },
            meta,
        )));
    };
    Ok(answered((
        GetPageSnapshotResult {
            exists: true,
            version: Some(snapshot.version().as_str().to_owned()),
            properties: snapshot
                .properties()
                .iter()
                .map(|property| PropertyEntry {
                    key: property.key().to_owned(),
                    value: property.value().to_owned(),
                })
                .collect(),
            blocks: snapshot_entries(&snapshot),
        },
        meta,
    )))
}
