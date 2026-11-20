use logseq_graph::domain::{
    BlockMarker, BlockSelector, BlockView, PageSnapshot, ReadMeta, SnapshotBlock,
};
use serde::Serialize;
use serde_json::Value;

use crate::render::rfc3339;

/// The freshness fields every answer reports, taken from the read's own metadata.
#[derive(Serialize)]
pub(super) struct Meta {
    revision: u64,
    stale: bool,
    last_scan: String,
}

impl Meta {
    pub(super) fn of(meta: &ReadMeta) -> Self {
        Self {
            revision: meta.revision(),
            stale: meta.is_stale(),
            last_scan: rfc3339(meta.last_scan()),
        }
    }
}

/// A stable, projection-derived numeric identity for a page.
///
/// The official API addresses a page by `block.page.id` (a DB entity id) and resolves it with
/// `Editor.getPage({id})`. This file-first projection has no entity table (ADR-0001), so an id of
/// its own stands in: the FNV-1a 64-bit hash of the folded page name, masked to 52 bits so it is a
/// JavaScript-safe integer. It is stable across scans (the name is the identity) and is resolved
/// back by hashing each page name, never by reading a row id. It is not a DB row id.
pub(super) fn page_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in logseq_graph::domain::page_name_key(name).as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash & 0x000f_ffff_ffff_ffff
}

/// The block id a caller can hand straight back to `getBlock`: the official answer states the id
/// bare, with no identity kind, so the same text must round-trip through a block read. A selector
/// that names the kind (`durable:<id>` or `ephemeral:<id>`) is still accepted on input.
pub(super) fn block_id(selector: &BlockSelector) -> String {
    match selector {
        BlockSelector::Durable(id) => id.to_string(),
        BlockSelector::Ephemeral(id) => id.to_string(),
    }
}

/// The shape of one page in a listing or an answer.
#[derive(Serialize, Clone)]
pub(super) struct PageEntry {
    pub(super) name: String,
    /// The projection-derived numeric page id ([`page_id`]): an official client addresses a page
    /// by `block.page.id` and this is what it gets back.
    pub(super) id: u64,
    /// `page` or `journal`, spelled the way the domain spells it.
    pub(super) kind: &'static str,
    /// The calendar day, `YYYY-MM-DD`, when this page is a journal, and `null` otherwise.
    pub(super) journal_date: Option<String>,
    pub(super) block_count: u32,
    pub(super) has_file: bool,
}

/// One key/value property line.
#[derive(Serialize)]
pub(super) struct PropertyEntry {
    pub(super) key: String,
    pub(super) value: String,
}

/// One block in a tree, block, snapshot or query answer.
///
/// `selector` is the identity the block answers to, so a read is actionable the same way a query
/// answer is. It is `null` for a snapshot block whose file states no `id::`: the snapshot is decoded
/// from the file alone, and an ephemeral identity is minted by the projection from index rows the
/// snapshot never reads — there is no identity to state.
#[derive(Serialize, Clone)]
pub(super) struct BlockEntry {
    pub(super) page: String,
    pub(super) preorder: u32,
    pub(super) depth: u32,
    pub(super) marker: String,
    pub(super) text: String,
    pub(super) selector: Option<Selector>,
}

/// One block's identity, stated as the two fields a caller needs to branch on and to spell back.
#[derive(Serialize, Clone)]
pub(super) struct Selector {
    /// `durable` or `ephemeral`, the two kinds that exist.
    kind: &'static str,
    id: String,
}

impl Selector {
    pub(super) fn of(selector: &BlockSelector) -> Self {
        match selector {
            BlockSelector::Durable(id) => Self {
                kind: "durable",
                id: id.to_string(),
            },
            BlockSelector::Ephemeral(id) => Self {
                kind: "ephemeral",
                id: id.to_string(),
            },
        }
    }
}

/// The JSON value this method answered, wrapped with the read's metadata.
pub(super) fn envelope<T: Serialize>(meta: &Meta, value: T) -> Value {
    #[derive(Serialize)]
    struct Envelope<'a, T: Serialize> {
        #[serde(flatten)]
        meta: &'a Meta,
        #[serde(flatten)]
        value: T,
    }

    // Every field is a String, a number, a bool, or a vector of those, so serialization cannot
    // fail. A value that cannot be represented would be a defect in a field type, not a request.
    #[expect(
        clippy::expect_used,
        reason = "serializing a struct of strings, numbers, bools and their vectors cannot fail"
    )]
    serde_json::to_value(Envelope { meta, value }).expect("serializing a method result cannot fail")
}

/// Turn one successful engine read into the `result` member of the response.
pub(super) fn answered<T: Serialize>((value, meta): (T, Meta)) -> Value {
    envelope(&meta, value)
}

/// The marker of one block, spelled the way both the tree and the snapshot answer it.
pub(super) fn marker_text(marker: BlockMarker) -> String {
    match marker {
        BlockMarker::Bullet => "-".to_owned(),
        BlockMarker::Heading { level } => "#".repeat(usize::from(level)),
    }
}

/// The blocks of one page snapshot, flattened to the entries the tree method answers.
///
/// A [`PageSnapshot`] nests its blocks because one parse of one file produced them; every caller of
/// either method restores the tree from `depth` anyway, so answering the nested shape here would
/// make it carry that restoration twice. `depth` is therefore the walk's own state, not a field the
/// snapshot stores.
pub(super) fn snapshot_entries(snapshot: &PageSnapshot) -> Vec<BlockEntry> {
    fn walk(block: &SnapshotBlock, page: &str, depth: u32, out: &mut Vec<BlockEntry>) {
        let selector = block
            .durable_id()
            .map(|id| Selector::of(&BlockSelector::Durable(id.clone())));
        out.push(BlockEntry {
            page: page.to_owned(),
            preorder: block.preorder(),
            depth,
            marker: marker_text(block.marker()),
            text: block.text().to_owned(),
            selector,
        });
        for child in block.children() {
            walk(child, page, depth + 1, out);
        }
    }

    let mut entries = Vec::new();
    for block in snapshot.blocks() {
        walk(block, snapshot.page(), 0, &mut entries);
    }
    entries
}

pub(super) fn block_entry(block: &BlockView) -> BlockEntry {
    BlockEntry {
        page: block.page().to_owned(),
        preorder: block.preorder(),
        depth: block.depth(),
        marker: marker_text(block.marker()),
        text: block.text().to_owned(),
        selector: Some(Selector::of(block.selector())),
    }
}
