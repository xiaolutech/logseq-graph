use std::collections::{BTreeMap, HashSet};

use rusqlite::{params, Transaction};

use crate::document::{extract_page_refs, extract_tags};
use crate::domain::page_name_key;

use super::projection::ProjectedPage;
use super::IndexError;

pub(super) fn write_claims(
    tx: &Transaction<'_>,
    page_id: i64,
    name: &str,
    page: &ProjectedPage,
) -> Result<(), IndexError> {
    let mut claims = BTreeMap::new();
    let mut claim = |name: &str, kind| {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        claims
            .entry((page_name_key(name), kind))
            .or_insert_with(|| name.to_owned());
        if !name.split('/').any(str::is_empty) {
            for (at, _) in name.match_indices('/') {
                let parent = &name[..at];
                let parent_kind = if kind == "ref" { "ref" } else { "entity" };
                claims
                    .entry((page_name_key(parent), parent_kind))
                    .or_insert_with(|| parent.to_owned());
            }
        }
    };

    if !name.split('/').any(str::is_empty) {
        for (at, _) in name.match_indices('/') {
            claim(&name[..at], "entity");
        }
    }
    for block in page.blocks() {
        let links = page.block_links(block.preorder);
        for (name, _) in links.page_refs().iter().chain(links.tags()) {
            claim(name, "entity");
        }
    }
    let mut seen = HashSet::new();
    for property in page.properties().iter().rev() {
        let key = property.normalized_key();
        if !seen.insert(key) {
            continue;
        }
        let kind = match key {
            "alias" => "alias",
            "tags" => "tag",
            _ => "ref",
        };
        for target in property_targets(key, property.value()) {
            if kind != "alias" || page_name_key(&target) != page_name_key(name) {
                claim(&target, kind);
            }
        }
    }

    if !claims.is_empty() {
        let mut insert = tx.prepare_cached(
            "INSERT INTO page_claims (page_id, name_key, kind, name) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for ((key, kind), name) in claims {
            insert.execute(params![page_id, key, kind, name])?;
        }
    }
    Ok(())
}

fn property_targets(key: &str, value: &str) -> Vec<String> {
    if value.starts_with('"') && value.ends_with('"') {
        return Vec::new();
    }
    if matches!(
        key,
        "title"
            | "icon"
            | "template"
            | "template-including-parent"
            | "public"
            | "filters"
            | "exclude-from-graph-view"
            | "id"
            | "custom-id"
            | "background-color"
            | "heading"
            | "collapsed"
            | "created-at"
            | "updated-at"
            | "last-modified-at"
    ) || key.starts_with("logseq.")
    {
        return Vec::new();
    }
    if matches!(key, "alias" | "aliases" | "tags") {
        return crate::document::property_list_targets(value);
    }
    let mut targets = extract_page_refs(value);
    targets.extend(extract_tags(value));
    targets
}
