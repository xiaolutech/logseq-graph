//! Editing a block's editable text, and writing its marker explicitly.
//!
//! An optimistic editor shows a block's editable text — [`PageFacts`]'s answer without the property
//! lines — and sends that text back. It never sends back the `id::`, `collapsed::`, or custom
//! property lines it did not show, so the edit that receives it must leave those lines where the
//! file wrote them. That is the whole contract `set_block_text_text` has, and the reason it exists
//! next to `update_block_text`, whose `text` is the block's whole text and which re-emits the
//! identity lines after the new content.
//!
//! `set_block_marker_text` is the other half: turning a bullet into a heading, a heading into a
//! bullet, or one heading level into another, which no plain text edit can express because the text
//! an editor shows does not contain the marker bytes.
//!
//! Every test here parses source text, edits it through `PageDocument`, and asserts the exact output
//! bytes: what the bytes say is the contract. Each success also checks that every byte outside the
//! edited block's own lines is untouched, because "only this changed" is the promise that makes the
//! edits safe to run against a file an editor never showed in full.

use logseq_graph::document::{
    parse, Block, BlockLocator, EditError, LineSpan, PageDocument, PageFacts,
};
use logseq_graph::domain::BlockMarker;

const ID_A: &str = "11111111-1111-1111-1111-111111111111";
const ID_B: &str = "22222222-2222-2222-2222-222222222222";

/// Byte offset where line `index` starts, counting the BOM the source may begin with.
fn line_offset(document: &PageDocument, index: usize) -> usize {
    let bom = if document.has_bom() {
        "\u{feff}".len()
    } else {
        0
    };
    bom + document.lines()[..index]
        .iter()
        .map(|line| line.text().len() + line.ending().map_or(0, |ending| ending.as_str().len()))
        .sum::<usize>()
}

/// Own lines of the block that states `block_id`.
fn own_span_of(document: &PageDocument, block_id: &str) -> LineSpan {
    PageFacts::extract(document)
        .blocks()
        .iter()
        .find(|block| {
            block
                .property("id")
                .is_some_and(|property| property.value().eq_ignore_ascii_case(block_id))
        })
        .expect("the fixture names the block")
        .own_span()
}

/// The block at `indices`, one sibling position per level.
fn block_at<'a>(document: &'a PageDocument, indices: &[usize]) -> &'a Block {
    let (first, deeper) = indices.split_first().expect("a block is named");
    let mut block = &document.blocks()[*first];
    for sibling in deeper {
        block = &block.children()[*sibling];
    }
    block
}

/// The locator path of the block at `indices`.
///
/// [`BlockLocator::Path`] addresses the first level by preorder instead of by position, which is
/// the rule the index reads a handle's path with; tests name blocks the way the tree reads, and this
/// is the one place that translates.
fn locate(document: &PageDocument, indices: &[usize]) -> Vec<u32> {
    let (first, deeper) = indices.split_first().expect("a path names a block");
    let mut preorder = 0u32;
    for (position, root) in document.blocks().iter().enumerate() {
        if position == *first {
            let mut path = vec![preorder];
            let mut current = root;
            for sibling in deeper {
                current = &current.children()[*sibling];
                path.push(*sibling as u32);
            }
            return path;
        }
        preorder += count(root);
    }
    panic!("no top-level block at position {first}");
}

fn count(block: &Block) -> u32 {
    1 + block.children().iter().map(count).sum::<u32>()
}

/// Every byte outside `span`'s own lines, before and after the edit, is the same byte.
fn assert_outside_untouched(source: &str, edited: &str, before: &PageDocument, span: LineSpan) {
    let head = line_offset(before, span.start());
    let tail = line_offset(before, span.end());
    assert_eq!(
        &edited[..head],
        &source[..head],
        "every byte before the block's own lines is untouched"
    );
    assert_eq!(
        &edited[edited.len() - (source.len() - tail)..],
        &source[tail..],
        "every byte after the block's own lines is untouched"
    );
}

/// Edit the text of the block `block_id` names and assert the bytes and the promises.
fn assert_text_edit(source: &str, block_id: &str, text: &str, expected: &str) -> String {
    let before = parse(source);
    let span = own_span_of(&before, block_id);
    let properties: Vec<String> = PageFacts::extract(&before)
        .blocks()
        .iter()
        .find(|block| block.own_span() == span)
        .expect("the block has facts")
        .properties()
        .iter()
        .map(|property| before.lines()[property.line()].text().to_owned())
        .collect();

    let edited = before
        .set_block_text_text(BlockLocator::Id(block_id), text)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_outside_untouched(source, &edited, &before, span);

    let after = parse(&edited);
    let facts = PageFacts::extract(&after);
    let block = facts
        .blocks()
        .iter()
        .find(|block| block.own_span().start() == span.start())
        .expect("the block is still there");
    assert_eq!(block.editable_text(), text, "the text an editor shows");
    let kept: Vec<String> = block
        .properties()
        .iter()
        .map(|property| after.lines()[property.line()].text().to_owned())
        .collect();
    assert_eq!(kept, properties, "each property line, byte for byte");
    edited
}

/// Write the marker of the block at `indices` and assert the bytes and the promises.
fn assert_marker(source: &str, indices: &[usize], marker: BlockMarker, expected: &str) -> String {
    let before = parse(source);
    let target = block_at(&before, indices);
    let span = target.own_span();
    let text = PageFacts::extract(&before)
        .blocks()
        .iter()
        .find(|block| block.own_span() == span)
        .expect("the block has facts")
        .editable_text();
    let path = locate(&before, indices);

    let edited = before
        .set_block_marker_text(BlockLocator::Path(&path), marker)
        .expect("the edit should succeed");
    assert_eq!(edited, expected, "byte-exact output");
    assert_outside_untouched(source, &edited, &before, span);

    let after = parse(&edited);
    assert_eq!(
        block_at(&after, indices).marker(),
        marker,
        "the block carries the requested marker"
    );
    assert_eq!(
        PageFacts::extract(&after)
            .blocks()
            .iter()
            .find(|block| block.own_span() == span)
            .expect("the block has facts")
            .editable_text(),
        text,
        "the text an editor shows is unchanged"
    );
    edited
}

#[test]
fn an_id_and_a_collapsed_line_stay_where_the_file_wrote_them() {
    let source = format!("- 正文\n  id:: {ID_A}\n  collapsed:: true\n- next\n");
    let expected = format!("- 新正文\n  id:: {ID_A}\n  collapsed:: true\n- next\n");
    let edited = assert_text_edit(&source, ID_A, "新正文", &expected);
    assert_eq!(
        edited, expected,
        "the identity and the folded state are not re-emitted after the content"
    );
}

#[test]
fn a_property_line_after_the_content_stays_after_it() {
    let source = format!("- a\n  more\n  id:: {ID_A}\n");
    assert_text_edit(&source, ID_A, "b\nc", &format!("- b\n  c\n  id:: {ID_A}\n"));
}

#[test]
fn a_custom_property_line_survives_a_text_edit() {
    let source = format!("- 正文\n  todo:: x\n  id:: {ID_A}\n- next\n");
    let expected = format!("- 新\n  todo:: x\n  id:: {ID_A}\n- next\n");
    assert_text_edit(&source, ID_A, "新", &expected);
}

#[test]
fn a_property_between_content_lines_is_refused() {
    // The editor shows `a\nb`, and there is no single place that text can go without moving
    // `id::` across one of its own lines.
    let source = format!("- a\n  id:: {ID_A}\n  b\n- next\n");
    let document = parse(&source);
    let error = document
        .set_block_text_text(BlockLocator::Id(ID_A), "c")
        .expect_err("the layout is refused");
    assert_eq!(error, EditError::InterleavedProperties);
    assert!(
        error.to_string().contains("property line between two"),
        "the message says why: {error}"
    );
}

#[test]
fn a_blocks_children_keep_their_bytes() {
    let source = format!("- parent\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- sibling\n");
    let expected = format!("- renamed\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- sibling\n");
    assert_text_edit(&source, ID_A, "renamed", &expected);
}

#[test]
fn the_identity_of_a_block_whose_text_becomes_empty_survives() {
    let source = format!("- a\n  id:: {ID_A}\n- next\n");
    let edited = assert_text_edit(&source, ID_A, "", &format!("-\n  id:: {ID_A}\n- next\n"));
    assert_eq!(
        edited,
        format!("-\n  id:: {ID_A}\n- next\n"),
        "the marker line loses the content and nothing else"
    );
}

#[test]
fn a_merged_block_can_still_have_its_text_edited() {
    // Merging a two-line block into a block whose `id::` sits after its content used to write the
    // second content line after the property: a property between two content lines, which
    // `set_block_text_text` refuses and no editor shows. The merge keeps the content lines
    // together, so "merge, then type" is one working sequence.
    let source = format!("- a\n  id:: {ID_A}\n- b\n  more\n");
    let document = parse(&source);
    let merged = document
        .merge_block_text(BlockLocator::Path(&locate(&document, &[1])))
        .expect("the merge should succeed");
    assert_eq!(
        merged,
        format!("- ab\n  more\n  id:: {ID_A}\n"),
        "the property line follows the whole content, not one line of it"
    );

    let after = parse(&merged);
    let block = after
        .blocks_in_preorder()
        .first()
        .copied()
        .expect("the merged block is there");
    assert_eq!(
        PageFacts::extract(&after)
            .blocks()
            .iter()
            .find(|facts| facts.own_span() == block.own_span())
            .expect("the block has facts")
            .editable_text(),
        "ab\nmore",
        "the merged text is both blocks' text"
    );
    let edited = after
        .set_block_text_text(BlockLocator::Id(ID_A), "ab\nmore!")
        .expect("the merged block can still have its text edited");
    assert_eq!(edited, format!("- ab\n  more!\n  id:: {ID_A}\n"));
}

#[test]
fn a_bullet_keeps_the_character_the_file_wrote() {
    // Only `-` is a bullet marker; `*` and `+` are text, so there is no other spelling to keep.
    let source = "- a\n";
    let edited = assert_marker(source, &[0], BlockMarker::Bullet, source);
    assert_eq!(
        edited, source,
        "changing a bullet to a bullet changes nothing"
    );
}

#[test]
fn a_bullet_becomes_a_heading() {
    let source = format!("- a\n  more\n  id:: {ID_A}\n- next\n");
    let expected = format!("## a\n   more\n   id:: {ID_A}\n- next\n");
    assert_marker(&source, &[0], BlockMarker::Heading { level: 2 }, &expected);
}

#[test]
fn a_heading_becomes_a_bullet() {
    let source = format!("### a\n    more\n    id:: {ID_A}\n- next\n");
    let expected = format!("- a\n  more\n  id:: {ID_A}\n- next\n");
    assert_marker(&source, &[0], BlockMarker::Bullet, &expected);
}

#[test]
fn one_heading_level_becomes_another() {
    let source = "# a\n  more\n  todo:: x\n";
    let expected = "###### a\n       more\n       todo:: x\n";
    assert_marker(source, &[0], BlockMarker::Heading { level: 6 }, expected);
}

#[test]
fn a_heading_of_the_same_level_changes_nothing() {
    let source = "## a\n   more\n";
    let edited = assert_marker(source, &[0], BlockMarker::Heading { level: 2 }, source);
    assert_eq!(edited, source);
}

#[test]
fn a_heading_level_outside_one_to_six_is_refused() {
    let document = parse("# a\n  more\n");
    for level in [0, 7] {
        let error = document
            .set_block_marker_text(BlockLocator::Path(&[0]), BlockMarker::Heading { level })
            .expect_err("the level does not exist");
        assert_eq!(error, EditError::InvalidHeadingLevel { level });
        assert!(
            error.to_string().contains(&level.to_string()),
            "the message names the level: {error}"
        );
    }
}

#[test]
fn a_marker_change_realigns_the_properties_and_leaves_children_alone() {
    // The child keeps its own indentation: depth is the child's, not the parent's marker width.
    let source = format!("- a\n  more\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n");
    let expected =
        format!("###### a\n       more\n       id:: {ID_A}\n  - child\n    id:: {ID_B}\n");
    let edited = assert_marker(&source, &[0], BlockMarker::Heading { level: 6 }, &expected);
    let after = parse(&edited);
    assert_eq!(
        after.blocks()[0].children()[0].own_span(),
        LineSpan::new(3, 5)
    );
}

#[test]
fn the_text_of_a_block_can_carry_anything_an_editor_types() {
    let source = format!("- 前\n  id:: {ID_A}\n- 后\n");
    let text = "😀👍🏽 e\u{301}漢字 `code` [[页引用]] ((11111111-2222-3333-4444-555555555555))\n```\n- not a block\nkey:: not a property\n```";
    let expected = format!(
        "- 😀👍🏽 e\u{301}漢字 `code` [[页引用]] ((11111111-2222-3333-4444-555555555555))\n  ```\n  - not a block\n  key:: not a property\n  ```\n  id:: {ID_A}\n- 后\n"
    );
    let edited = assert_text_edit(&source, ID_A, text, &expected);
    let after = parse(&edited);
    assert_eq!(
        after.blocks().len(),
        2,
        "the fence keeps its lines inside the block"
    );
}

#[test]
fn a_text_edit_that_would_add_a_block_is_refused() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- next\n"));
    for text in ["b\n- x", "b\n# x", "b\nkey:: value"] {
        assert_eq!(
            document.set_block_text_text(BlockLocator::Id(ID_A), text),
            Err(EditError::ContinuationChangesStructure),
            "{text:?} would change the page's structure"
        );
    }
}

#[test]
fn a_bytes_final_newline_and_bom_survive_both_edits() {
    let source = format!("\u{feff}- a\r\n  more\r\n  id:: {ID_A}");
    let document = parse(&source);
    let edited = document
        .set_block_text_text(BlockLocator::Id(ID_A), "b")
        .expect("the edit should succeed");
    assert!(edited.starts_with('\u{feff}'));
    assert_eq!(
        edited.matches("\r\n").count(),
        1,
        "the CRLF ending survives"
    );
    assert_eq!(
        edited.matches("id::").count(),
        1,
        "the identity is written once"
    );
    assert!(
        !edited.ends_with('\n'),
        "the missing final newline survives"
    );

    let source = "\u{feff}- a\r\n  more\r\n";
    let edited = assert_marker(
        source,
        &[0],
        BlockMarker::Heading { level: 2 },
        "\u{feff}## a\r\n   more\r\n",
    );
    assert!(edited.starts_with('\u{feff}'));
}
