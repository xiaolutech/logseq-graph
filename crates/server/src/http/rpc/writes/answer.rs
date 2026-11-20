use super::super::answer::{envelope, snapshot_entries, Meta, PageEntry, PropertyEntry, Selector};
use logseq_graph::domain::{BlockSelector, BlockView, PageMetadata, PageSnapshot, Read};
use serde_json::{json, Value};

pub(super) fn ensured_id_result(read: Read<logseq_graph::engine::EnsuredBlockId>) -> Value {
    written(read, |answer, meta| {
        envelope(
            meta,
            json!({
                "id": answer.id.as_str(),
                "created": answer.created,
                "block": block_entry(&answer.block),
            }),
        )
    })
}

fn page_entry(page: &PageMetadata) -> PageEntry {
    PageEntry {
        name: page.name().to_owned(),
        id: super::super::answer::page_id(page.name()),
        kind: page.kind().as_str(),
        journal_date: page.journal_date().map(|day| day.to_string()),
        block_count: page.block_count(),
        has_file: page.has_file(),
    }
}

fn block_entry(block: &BlockView) -> super::super::answer::BlockEntry {
    super::super::answer::block_entry(block)
}

pub(super) fn snapshot_value(snapshot: &PageSnapshot) -> Value {
    json!({
        "exists": true,
        "page": snapshot.page(),
        "version": snapshot.version().as_str(),
        "properties": snapshot.properties().iter().map(|property| PropertyEntry {
            key: property.key().to_owned(),
            value: property.value().to_owned(),
        }).collect::<Vec<_>>(),
        "blocks": snapshot_entries(snapshot),
    })
}

pub(super) fn written<T>(read: Read<T>, value: impl FnOnce(T, &Meta) -> Value) -> Value {
    let meta = Meta::of(read.meta());
    value(read.into_value(), &meta)
}

pub(super) fn page_result(read: Read<PageMetadata>) -> Value {
    written(read, |page, meta| {
        envelope(meta, json!({"page": page_entry(&page)}))
    })
}

pub(super) fn selector_result(read: Read<BlockSelector>) -> Value {
    written(read, |selector, meta| {
        envelope(meta, json!({"selector": Selector::of(&selector)}))
    })
}

pub(super) fn block_result(read: Read<BlockView>) -> Value {
    written(read, |block, meta| {
        envelope(meta, json!({"block": block_entry(&block)}))
    })
}
