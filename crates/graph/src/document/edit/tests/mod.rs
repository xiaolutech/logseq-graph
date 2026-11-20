//! Shared edit test helpers.

mod append;
mod marker;
mod move_between;
mod move_within;
mod remove;
mod text;
mod update;

use super::locator::{block_path, block_shapes, facts_of, subtree_span};
use super::render::{indent_width, lead_whitespace};
use super::{BlockLocator, EditError, MoveTarget};
use crate::document::parser::UTF8_BOM;
use crate::document::{parse, LineSpan, PageDocument, PageFacts};
use crate::domain::BlockMarker;

const ID_A: &str = "11111111-1111-1111-1111-111111111111";
const ID_B: &str = "22222222-2222-2222-2222-222222222222";
const ID_C: &str = "33333333-3333-3333-3333-333333333333";

/// Facts of the last top-level block, joined back through its span.
fn last_top_level_text(document: &PageDocument) -> String {
    let last = document
        .blocks()
        .last()
        .expect("expected a top-level block");
    let facts = PageFacts::extract(document);
    let block = facts
        .blocks()
        .iter()
        .find(|block| block.own_span() == last.own_span())
        .expect("every block has facts");
    block.text().to_owned()
}

/// Append `text` to `source` and assert both the bytes and the resulting structure.
fn assert_edit(source: &str, text: &str, expected: &str) -> PageDocument {
    let before = parse(source);
    let edited = before.append_block(text).expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );

    let after = parse(&edited);
    assert_eq!(
        after.blocks().len(),
        before.blocks().len() + 1,
        "exactly one new top-level block"
    );
    assert_eq!(
        last_top_level_text(&after),
        text,
        "the new block carries the given content"
    );
    assert_eq!(
        after.ends_with_newline(),
        before.ends_with_newline(),
        "the final-newline state is preserved"
    );
    after
}

/// Own text of every block, in preorder.
fn block_texts(document: &PageDocument) -> Vec<String> {
    PageFacts::extract(document)
        .blocks()
        .iter()
        .map(|block| block.text().to_owned())
        .collect()
}

/// Preorder index of the block that states `block_id`.
fn block_index(document: &PageDocument, block_id: &str) -> usize {
    PageFacts::extract(document)
        .blocks()
        .iter()
        .position(|block| {
            block
                .property("id")
                .is_some_and(|property| property.value().eq_ignore_ascii_case(block_id))
        })
        .expect("the fixture names the block")
}

/// Own lines of the block that states `block_id`.
fn own_span_of(document: &PageDocument, block_id: &str) -> LineSpan {
    PageFacts::extract(document).blocks()[block_index(document, block_id)].own_span()
}

/// Byte offset where line `index` starts, counting the BOM.
fn line_offset(document: &PageDocument, index: usize) -> usize {
    let bom = if document.has_bom() {
        UTF8_BOM.len_utf8()
    } else {
        0
    };
    bom + document.lines()[..index]
        .iter()
        .map(|line| line.text().len() + line.ending().map_or(0, |ending| ending.as_str().len()))
        .sum::<usize>()
}

/// Update `block_id` to `text` and assert both the bytes and the resulting structure.
fn assert_update(source: &str, block_id: &str, text: &str, expected: &str) -> PageDocument {
    let before = parse(source);
    let index = block_index(&before, block_id);
    let edited = before
        .update_block_text(BlockLocator::Id(block_id), text)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );

    let after = parse(&edited);
    let before_texts = block_texts(&before);
    let after_texts = block_texts(&after);
    assert_eq!(
        after_texts.len(),
        before_texts.len(),
        "the number of blocks is unchanged"
    );
    // The block's own text is `text`, followed only by the identity lines that this edit keeps.
    let mut lines = after_texts[index].split('\n');
    let requested = text.split('\n').count();
    let content: Vec<&str> = lines.by_ref().take(requested).collect();
    assert_eq!(content.join("\n"), text, "the target block carries `text`");
    for kept in lines {
        let trimmed = kept.trim_start();
        assert!(
            trimmed.starts_with("id::") || trimmed.starts_with("collapsed::"),
            "only identity lines follow the new text, found {kept:?}"
        );
    }
    for (position, (old, new)) in before_texts.iter().zip(&after_texts).enumerate() {
        if position != index {
            assert_eq!(old, new, "block {position} keeps its own text");
        }
    }
    assert_eq!(
        after.ends_with_newline(),
        before.ends_with_newline(),
        "the final-newline state is preserved"
    );
    after
}

/// Remove `block_id` from `source` and assert both the bytes and the resulting structure.
///
/// `block_id` names a block whose whole subtree the edit deletes, so the only bytes allowed to
/// change are the lines of that subtree: the helper rebuilds the source without them and
/// requires the edit to produce exactly that.
fn assert_remove(source: &str, block_id: &str, expected: &str) -> PageDocument {
    let before = parse(source);
    let span = subtree_span(&before, own_span_of(&before, block_id));
    let head = line_offset(&before, span.start());
    let tail = line_offset(&before, span.end());

    let edited = before
        .remove_block_text(BlockLocator::Id(block_id))
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");

    let mut kept = String::new();
    kept.push_str(&source[..head]);
    kept.push_str(&source[tail..]);
    assert_eq!(
        edited, kept,
        "every byte outside the deleted lines is untouched"
    );

    let after = parse(&edited);
    let survivors: Vec<String> = PageFacts::extract(&before)
        .blocks()
        .iter()
        .filter(|block| !span.contains(block.own_span().start()))
        .map(|block| block.text().to_owned())
        .collect();
    assert_eq!(
        block_texts(&after),
        survivors,
        "exactly the deleted subtree's blocks are gone"
    );
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );
    after
}

/// The moved block's source lines, with the block's own indentation removed.
///
/// Comparing this before and after a move is what "the subtree keeps its relative structure"
/// means in bytes: the same lines, indented by the same amount relative to the block's own
/// marker line.
fn subtree_shape(document: &PageDocument, preorder: usize) -> Vec<String> {
    let block = &document.blocks_in_preorder()[preorder];
    let span = block.subtree_span();
    let lines = document.lines();
    let base = indent_width(lines[span.start()].text());
    (span.start()..span.end())
        .map(|index| {
            let text = lines[index].text();
            if text.trim().is_empty() {
                String::new()
            } else {
                text.chars().skip(base.min(indent_width(text))).collect()
            }
        })
        .collect()
}

/// Move `block_id` to `target` and assert the bytes, the BOM, and the structure.
fn assert_move(
    source: &str,
    block_id: &str,
    target: MoveTarget<'_>,
    expected: &str,
) -> PageDocument {
    let before = parse(source);
    let edited = before
        .move_block_text(BlockLocator::Id(block_id), target)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );

    let after = parse(&edited);
    assert_eq!(
        after.blocks_in_preorder().len(),
        before.blocks_in_preorder().len(),
        "a move changes no block count"
    );
    assert_eq!(
        subtree_shape(&after, block_index(&after, block_id)),
        subtree_shape(&before, block_index(&before, block_id)),
        "the moved subtree keeps the same lines in the same relative shape"
    );
    after
}

/// Move `block_id` out of `source` to `target` in `destination` and assert the bytes, the BOM,
/// and the structure of both pages.
fn assert_move_between(
    source: &str,
    destination: &str,
    block_id: &str,
    target: MoveTarget<'_>,
    expected_source: &str,
    expected_destination: &str,
) {
    let before_source = parse(source);
    let before_destination = parse(destination);
    let (edited_source, edited_destination) = before_source
        .move_block_text_to_page(BlockLocator::Id(block_id), &before_destination, target)
        .expect("the edit should succeed");
    assert_eq!(edited_source, expected_source, "byte-exact source page");
    assert_eq!(
        edited_destination, expected_destination,
        "byte-exact destination page"
    );
    assert_eq!(
        edited_source.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the source BOM state is preserved"
    );
    assert_eq!(
        edited_destination.starts_with(UTF8_BOM),
        destination.starts_with(UTF8_BOM),
        "the destination BOM state is preserved"
    );

    let after_source = parse(&edited_source);
    let after_destination = parse(&edited_destination);
    let source_count = before_source.blocks_in_preorder().len();
    let destination_count = before_destination.blocks_in_preorder().len();
    let moved_span =
        before_source.blocks_in_preorder()[block_index(&before_source, block_id)].subtree_span();
    let moved_count = before_source
        .blocks_in_preorder()
        .iter()
        .filter(|block| moved_span.contains(block.own_span().start()))
        .count();
    assert_eq!(
        after_source.blocks_in_preorder().len(),
        source_count - moved_count,
        "exactly the moved subtree's blocks left the source"
    );
    assert_eq!(
        after_destination.blocks_in_preorder().len(),
        destination_count + moved_count,
        "exactly the moved subtree's blocks joined the destination"
    );
    assert_eq!(
        subtree_shape(
            &after_destination,
            block_index(&after_destination, block_id)
        ),
        subtree_shape(&before_source, block_index(&before_source, block_id)),
        "the moved subtree keeps the same lines in the same relative shape"
    );
}

/// Locator path of the block at preorder `position`, the address a write states for a block.
fn path_of(document: &PageDocument, position: usize) -> Vec<u32> {
    block_path(document, document.blocks_in_preorder()[position].own_span())
}

/// Set the editable text of `block_id` and assert the bytes plus the edit's promises.
///
/// The promises are the contract, not the implementation: the same blocks in the same shape, the
/// new editable text on the target block, every property line byte for byte, and every other
/// block's own lines untouched.
fn assert_set_text(source: &str, block_id: &str, text: &str, expected: &str) -> PageDocument {
    let before = parse(source);
    let before_facts = PageFacts::extract(&before);
    let index = block_index(&before, block_id);
    let property_text = |document: &PageDocument, facts: &PageFacts| -> Vec<String> {
        facts.blocks()[index]
            .properties()
            .iter()
            .map(|property| document.lines()[property.line()].text().to_owned())
            .collect()
    };
    let properties = property_text(&before, &before_facts);

    let edited = before
        .set_block_text_text(BlockLocator::Id(block_id), text)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );

    let after = parse(&edited);
    let after_facts = PageFacts::extract(&after);
    assert_eq!(
        after_facts.blocks().len(),
        before_facts.blocks().len(),
        "the number of blocks is unchanged"
    );
    assert_eq!(
        after_facts.blocks()[index].editable_text(),
        text,
        "the block's editable text is the new one"
    );
    assert_eq!(
        property_text(&after, &after_facts),
        properties,
        "each property line is the same bytes, in the same order"
    );
    for (position, (old, new)) in before_facts
        .blocks()
        .iter()
        .zip(after_facts.blocks())
        .enumerate()
    {
        if position == index {
            continue;
        }
        assert_eq!(
            new.editable_text(),
            old.editable_text(),
            "block {position} keeps its editable text"
        );
    }
    assert_eq!(
        after.ends_with_newline(),
        before.ends_with_newline(),
        "the final-newline state is preserved"
    );
    after
}

/// Set the marker of the block at preorder `position` and assert the bytes plus the promises.
///
/// The promises are the marker edit's own: same blocks at the same lines, the same editable text
/// on the target block, the same property keys, values, and offsets, and the target's children
/// untouched.
fn assert_marker(
    source: &str,
    position: usize,
    marker: BlockMarker,
    expected: &str,
) -> PageDocument {
    let before = parse(source);
    let before_facts = PageFacts::extract(&before);
    let path = path_of(&before, position);
    let block = &before.blocks_in_preorder()[position];
    let span = block.own_span();
    let text = facts_of(&before_facts, span).editable_text();
    let properties: Vec<(String, String, usize)> = facts_of(&before_facts, span)
        .properties()
        .iter()
        .map(|property| {
            (
                property.key().to_owned(),
                property.value().to_owned(),
                property.line() - span.start(),
            )
        })
        .collect();

    let edited = before
        .set_block_marker_text(BlockLocator::Path(&path), marker)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_eq!(
        edited.starts_with(UTF8_BOM),
        source.starts_with(UTF8_BOM),
        "the BOM state is preserved"
    );

    let after = parse(&edited);
    let after_facts = PageFacts::extract(&after);
    assert_eq!(
        after_facts.blocks().len(),
        before_facts.blocks().len(),
        "the number of blocks is unchanged"
    );
    let rewritten = facts_of(&after_facts, span);
    assert_eq!(
        rewritten.editable_text(),
        text,
        "the block's editable text is unchanged"
    );
    let restated: Vec<(String, String, usize)> = rewritten
        .properties()
        .iter()
        .map(|property| {
            (
                property.key().to_owned(),
                property.value().to_owned(),
                property.line() - span.start(),
            )
        })
        .collect();
    assert_eq!(restated, properties, "each property is unchanged");
    assert_eq!(
        after.blocks_in_preorder()[position].marker(),
        marker,
        "the block carries the requested marker"
    );
    assert_eq!(
        after.blocks_in_preorder()[position].children().len(),
        block.children().len(),
        "the block keeps its children"
    );
    assert_eq!(
        after.ends_with_newline(),
        before.ends_with_newline(),
        "the final-newline state is preserved"
    );
    after
}

/// One page per shape a block can have, used by the idempotency case.
fn idempotency_sources() -> Vec<String> {
    vec![
        format!("- a\n  id:: {ID_A}\n- b\n"),
        format!("- id:: {ID_A}\n  body\n"),
        format!("- id:: {ID_A}\n  body\n  collapsed:: true\n"),
        "- one\n  two\n  three\n".to_owned(),
        format!("### a\n   b\n   id:: {ID_A}\n"),
        format!("- parent\n\t- child\n\t  id:: {ID_A}\n"),
        format!("\u{feff}- a\r\n  id:: {ID_A}\r\n"),
        format!("- a\n  b\n  id:: {ID_A}"),
        "- \n-\n".to_owned(),
        format!("- a\n  - child\n    id:: {ID_A}\n"),
        format!("- a\n\n  id:: {ID_A}\n\n\n"),
        format!("- a\n  id:: {ID_A}\n  b\n"),
    ]
}
