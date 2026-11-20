//! The five structure edits, checked on source bytes.
//!
//! `split`, `merge`, `unwrap`, `indent` and `outdent` are what an outliner's Enter, Backspace,
//! Delete, Tab and Shift-Tab mean. Each is a pure function from one parsed page to the next page's
//! bytes, so every test here parses a source string, edits it through `PageDocument`, and asserts on
//! the exact output text: what the bytes say is the whole contract, and a shape assertion alone
//! would not notice a rewritten line ending, a lost `id::`, or a marker character that changed.
//!
//! The cases are shapes an outliner is expected to answer the same way: a block split in the middle
//! keeps its children, a merge with no block above it is refused, an outdent keeps the parent's
//! remaining children in order. Alongside them, each refusal and each file shape that must survive
//! (BOM, CRLF, tabs, no final newline, code fences) gets a test of its own.

use logseq_graph::document::{parse, Block, BlockLocator, EditError, PageDocument, PageFacts};

/// The block tree as one line: `a[b c]` is `a` with children `b` and `c`, siblings joined by ` | `.
fn shape(source: &str) -> String {
    let document = parse(source);
    let facts = PageFacts::extract(&document);
    render(document.blocks(), &facts)
}

fn render(blocks: &[Block], facts: &PageFacts) -> String {
    blocks
        .iter()
        .map(|block| {
            let text = facts
                .blocks()
                .iter()
                .find(|facts| facts.own_span() == block.own_span())
                .expect("every block has facts")
                .text();
            if block.children().is_empty() {
                text.to_owned()
            } else {
                format!("{}[{}]", text, render(block.children(), facts))
            }
        })
        .collect::<Vec<String>>()
        .join(" | ")
}

/// The `PageDocument` locator path for the block at `indices`, one level per element, as `indices[0]`
/// is a position among the top-level blocks.
///
/// [`BlockLocator::Path`] addresses the first level by preorder instead, which is what ADR-0005
/// stores in a handle; tests name blocks the way the tree reads, and this is the one place that
/// translates.
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

/// The edit's bytes, checked for the invariant every edit here promises.
///
/// A structure edit never touches the page's preamble, and its bytes must parse back to themselves:
/// rendering a parsed document is the identity this layer exists to keep.
fn checked(source: &str, edited: String) -> String {
    let before = parse(source);
    let after = parse(&edited);
    assert_eq!(after.to_source(), edited, "the edited bytes round-trip");
    assert_eq!(
        before
            .raw_lines(before.preamble())
            .iter()
            .map(|line| line.text())
            .collect::<Vec<&str>>(),
        after
            .raw_lines(after.preamble())
            .iter()
            .map(|line| line.text())
            .collect::<Vec<&str>>(),
        "the preamble is untouched"
    );
    edited
}

fn split(source: &str, indices: &[usize], offset: usize) -> String {
    let document = parse(source);
    let path = locate(&document, indices);
    let edited = document
        .split_block_text(BlockLocator::Path(&path), offset)
        .expect("the split should succeed");
    checked(source, edited)
}

fn split_error(source: &str, indices: &[usize], offset: usize) -> EditError {
    let document = parse(source);
    let path = locate(&document, indices);
    document
        .split_block_text(BlockLocator::Path(&path), offset)
        .expect_err("the split should be refused")
}

fn merge(source: &str, indices: &[usize]) -> String {
    let document = parse(source);
    let path = locate(&document, indices);
    let edited = document
        .merge_block_text(BlockLocator::Path(&path))
        .expect("the merge should succeed");
    checked(source, edited)
}

fn merge_error(source: &str, indices: &[usize]) -> EditError {
    let document = parse(source);
    let path = locate(&document, indices);
    document
        .merge_block_text(BlockLocator::Path(&path))
        .expect_err("the merge should be refused")
}

fn unwrap(source: &str, indices: &[usize]) -> String {
    let document = parse(source);
    let path = locate(&document, indices);
    let edited = document
        .unwrap_block_text(BlockLocator::Path(&path))
        .expect("the unwrap should succeed");
    checked(source, edited)
}

fn indent(source: &str, indices: &[usize]) -> String {
    let document = parse(source);
    let path = locate(&document, indices);
    let edited = document
        .indent_block_text(BlockLocator::Path(&path))
        .expect("the indent should succeed");
    checked(source, edited)
}

fn indent_error(source: &str, indices: &[usize]) -> EditError {
    let document = parse(source);
    let path = locate(&document, indices);
    document
        .indent_block_text(BlockLocator::Path(&path))
        .expect_err("the indent should be refused")
}

fn outdent(source: &str, indices: &[usize]) -> String {
    let document = parse(source);
    let path = locate(&document, indices);
    let edited = document
        .outdent_block_text(BlockLocator::Path(&path))
        .expect("the outdent should succeed");
    checked(source, edited)
}

fn outdent_error(source: &str, indices: &[usize]) -> EditError {
    let document = parse(source);
    let path = locate(&document, indices);
    document
        .outdent_block_text(BlockLocator::Path(&path))
        .expect_err("the outdent should be refused")
}

/// Editable texts of every block, in preorder.
fn editable_texts(source: &str) -> Vec<String> {
    PageFacts::extract(&parse(source))
        .blocks()
        .iter()
        .map(|block| block.editable_text())
        .collect()
}

// --- split -----------------------------------------------------------------------------------------

#[test]
fn split_keeps_the_children_with_the_head_and_gives_the_tail_an_empty_sibling() {
    let out = split("- hello world\n  - child\n", &[0], 5);

    assert_eq!(out, "- hello\n  - child\n-  world\n");
    // The space the tail starts with joins the marker's own whitespace and is stripped again by the
    // parser, so the tail's text cannot begin with it: that one character is not representable.
    assert_eq!(shape(&out), "hello[child] | world");
}

#[test]
fn split_at_zero_moves_the_whole_text_into_the_new_block() {
    let out = split("- abc\n", &[0], 0);

    assert_eq!(out, "-\n- abc\n");
    assert_eq!(shape(&out), " | abc");
}

#[test]
fn split_at_the_end_leaves_the_new_block_empty() {
    let out = split("- abc\n", &[0], 3);

    assert_eq!(out, "- abc\n-\n");
    assert_eq!(shape(&out), "abc | ");
}

#[test]
fn split_an_empty_block_gives_two_empty_blocks() {
    let out = split("-\n", &[0], 0);

    assert_eq!(out, "-\n-\n");
    assert_eq!(shape(&out), " | ");
}

#[test]
fn split_a_two_line_body_puts_the_halves_in_their_own_blocks() {
    let out = split("- one\n  two\n  three\n", &[0], 4);

    assert_eq!(out, "- one\n  \n- two\n  three\n");
    assert_eq!(shape(&out), "one\n | two\nthree");
}

#[test]
fn split_writes_continuation_lines_at_each_blocks_own_content_column() {
    // The tail starts with the space before `world`, so its heading has two spaces after the `###`
    // and its own content column, five, is where the continuation line goes.
    let out = split("### hello world\n    more\n", &[0], 5);

    assert_eq!(out, "### hello\n###  world\n     more\n");
    assert_eq!(shape(&out), "hello | world\nmore");
}

#[test]
fn split_keeps_the_marker_character_of_the_block_it_came_from() {
    // Only `-` is a bullet marker, so the spellings this covers are the dash's own: with a space,
    // bare, and with several.
    assert_eq!(split("- a b\n", &[0], 1), "- a\n-  b\n");
    assert_eq!(split("-  a b\n", &[0], 1), "-  a\n-   b\n");
    assert_eq!(split("###### a b\n", &[0], 1), "###### a\n######  b\n");
}

#[test]
fn split_keeps_every_heading_level_it_splits() {
    for level in 1..=6 {
        let marker = "#".repeat(level);
        let source = format!("{marker} a b\n");
        let out = split(&source, &[0], 1);
        assert_eq!(out, format!("{marker} a\n{marker}  b\n"), "level {level}");
    }
}

#[test]
fn split_keeps_the_id_and_the_collapsed_line_on_the_original_block() {
    let source = "- 正文 abc\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n";
    // 正文 is six bytes, so offset 6 splits right after it.
    let out = split(source, &[0], 6);

    assert_eq!(
        out,
        "- 正文\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n-  abc\n"
    );
    let facts = PageFacts::extract(&parse(&out));
    assert!(facts.blocks()[0].property("id").is_some());
    assert!(facts.blocks()[0].property("collapsed").is_some());
    assert!(facts.blocks()[1].properties().is_empty(), "no new id::");
}

#[test]
fn split_keeps_a_custom_property_line_on_the_original_block() {
    let out = split("- a b\n  todo:: x\n", &[0], 1);

    assert_eq!(out, "- a\n  todo:: x\n-  b\n");
    assert_eq!(editable_texts(&out), vec!["a", "b"]);
}

#[test]
fn split_keeps_a_property_line_that_sits_between_two_content_lines() {
    let out = split("- a b\n  id:: u\n", &[0], 1);

    assert_eq!(out, "- a\n  id:: u\n-  b\n");
}

#[test]
fn split_of_a_block_whose_marker_line_states_a_property_writes_the_text_as_a_continuation() {
    // The marker line is a property line, so it is not editable text: `text` is `more`, and the
    // first half has to be written as a continuation line under the line that holds the property.
    let source = "- id:: u\n  more\n";
    let out = split(source, &[0], 2);

    assert_eq!(out, "- id:: u\n  mo\n- re\n");
    assert_eq!(shape(&out), "id:: u\nmo | re");
}

#[test]
fn split_of_a_multi_line_body_keeps_a_trailing_property_next_to_the_first_half() {
    // The first half has fewer content lines than the block had, so the property line moves up with
    // it. Where it lands is read off the rendered lines, not off the source offset: an edit that
    // claimed the source offset would refuse this split, which is an ordinary one.
    let out = split("- one\n  two\n  id:: u\n", &[0], 1);

    assert_eq!(out, "- o\n  id:: u\n- ne\n  two\n");
    assert_eq!(editable_texts(&out), vec!["o", "ne\ntwo"]);
}

#[test]
fn split_a_tab_indented_file_stays_tab_indented() {
    let out = split("- a\n\t- hello world\n", &[0, 0], 5);

    assert_eq!(out, "- a\n\t- hello\n\t-  world\n");
    assert_eq!(shape(&out), "a[hello | world]");
}

#[test]
fn split_a_two_space_file_stays_two_space_indented() {
    let out = split("- a\n  - hello world\n", &[0, 0], 5);

    assert_eq!(out, "- a\n  - hello\n  -  world\n");
}

#[test]
fn split_keeps_a_bom_crlf_and_the_file_ending_of_each_line() {
    let out = split("\u{feff}- a b\r\n- c\r\n", &[0], 1);

    assert_eq!(out, "\u{feff}- a\r\n-  b\r\n- c\r\n");
    assert!(out.starts_with('\u{feff}'));
}

#[test]
fn split_keeps_a_missing_final_newline() {
    let out = split("- a b", &[0], 1);

    assert_eq!(out, "- a\n-  b");
    assert!(!out.ends_with('\n'));
}

#[test]
fn split_keeps_the_trailing_blank_lines_last() {
    let out = split("- a b\n\n\n", &[0], 1);

    assert_eq!(out, "- a\n-  b\n\n\n");
}

#[test]
fn split_keeps_emoji_and_combining_characters_whole() {
    // `😀` is four bytes and `é` written as `e` + U+0301 is two characters of three and two bytes.
    let out = split("- a😀e\u{301}b\n", &[0], 5);

    assert_eq!(out, "- a😀\n- e\u{301}b\n");
}

#[test]
fn split_a_tail_that_starts_with_an_inline_code_span_is_text() {
    let out = split("- a `b` c\n", &[0], 1);

    assert_eq!(out, "- a\n-  `b` c\n");
}

#[test]
fn split_at_the_start_of_a_code_fence_line_keeps_the_fence_inside_the_tail() {
    let source = "- a\n  ```bash\n  echo hi\n  ```\n  b\n";
    let out = split(source, &[0], 2);

    assert_eq!(out, "- a\n  \n- ```bash\n  echo hi\n  ```\n  b\n");
    assert_eq!(shape(&out), "a\n | ```bash\necho hi\n```\nb");
}

#[test]
fn split_keeps_a_page_and_a_block_reference_inside_the_half_that_holds_it() {
    let out = split(
        "- [[页]] ((11111111-2222-3333-4444-555555555555))\n",
        &[0],
        7,
    );

    assert_eq!(
        out,
        "- [[页]]\n-  ((11111111-2222-3333-4444-555555555555))\n"
    );
}

#[test]
fn split_refuses_an_offset_past_the_editable_text() {
    assert_eq!(
        split_error("- ab\n", &[0], 5),
        EditError::TextOffsetOutOfRange { offset: 5, len: 2 }
    );
}

#[test]
fn split_accepts_the_offset_equal_to_the_editable_text_length() {
    // The boundary case above is not an error: everything stays in the first half.
    assert_eq!(split("- ab\n", &[0], 2), "- ab\n-\n");
}

#[test]
fn split_refuses_an_offset_inside_a_character() {
    assert_eq!(
        split_error("- a😀b\n", &[0], 2),
        EditError::TextOffsetSplitsCharacter { offset: 2 }
    );
}

#[test]
fn split_refuses_a_tail_line_that_would_become_the_new_blocks_property() {
    // The `a:: b` line is fenced code inside the block, so it is text; as the first line of a new
    // block it reads as a property line instead, which is a structure change.
    let source = "- ```\n  a:: b\n  ```\n";
    assert_eq!(
        split_error(source, &[0], 4),
        EditError::ContinuationChangesStructure
    );
}

#[test]
fn split_an_unmatched_fence_does_not_hide_the_next_block() {
    let source = "- a ```\n- after\n";
    let out = split(source, &[0], 2);
    assert_eq!(out, "- a \n- ```\n- after\n");
    assert_eq!(editable_texts(&out), vec!["a ", "```", "after"]);
}

#[test]
fn split_refuses_a_new_block_whose_last_line_would_become_page_trailing_space() {
    // The blank continuation line ends the block, and the subtree is the page's last content, so the
    // tail block would lose that line to the page's trailing space.
    let source = "- a\n  \n\t- child\n";
    assert_eq!(
        split_error(source, &[0], 1),
        EditError::ContinuationChangesStructure
    );
}

#[test]
fn split_refuses_a_heading_that_would_stop_being_a_marker_line() {
    // An empty first half renders `###`, which the parser does not read as a heading.
    assert_eq!(
        split_error("### x\n", &[0], 0),
        EditError::ContinuationChangesStructure
    );
}

#[test]
fn split_of_an_unknown_path_is_reported_as_out_of_range() {
    let document = parse("- a\n");
    assert_eq!(
        document.split_block_text(BlockLocator::Path(&[7]), 0),
        Err(EditError::PathOutOfRange)
    );
}

#[test]
fn split_leaves_the_lines_around_the_block_byte_for_byte() {
    let source = "title:: 首页\n\n- before\n- a b\n  - child\n- after\n";
    let out = split(source, &[1], 1);

    assert_eq!(
        out,
        "title:: 首页\n\n- before\n- a\n  - child\n-  b\n- after\n"
    );
}

// --- merge -----------------------------------------------------------------------------------------

#[test]
fn merge_appends_the_text_and_adopts_the_merged_blocks_children() {
    let source = "- first\n\t- kept\n- second\n\t- adopted\n";
    let out = merge(source, &[1]);

    assert_eq!(out, "- firstsecond\n\t- kept\n\t- adopted\n");
    assert_eq!(shape(&out), "firstsecond[kept | adopted]");
}

#[test]
fn merge_folds_the_first_child_into_its_parent_and_leaves_the_other_children_in_order() {
    let source = "- parent\n\t- first\n\t- sibling\n";
    let out = merge(source, &[0, 0]);

    assert_eq!(out, "- parentfirst\n\t- sibling\n");
    assert_eq!(shape(&out), "parentfirst[sibling]");
}

#[test]
fn merge_the_first_child_lifts_its_own_children_into_the_parent() {
    let source = "- parent\n\t- first\n\t\t- grand\n\t- sibling\n";
    let out = merge(source, &[0, 0]);

    assert_eq!(out, "- parentfirst\n\t- grand\n\t- sibling\n");
    assert_eq!(shape(&out), "parentfirst[grand | sibling]");
}

#[test]
fn merge_with_no_previous_sibling_at_the_top_level_is_refused() {
    assert_eq!(
        merge_error("- only one\n", &[0]),
        EditError::NoPreviousSibling
    );
    assert_eq!(
        merge_error("- first\n- second\n", &[0]),
        EditError::NoPreviousSibling
    );
}

#[test]
fn merge_keeps_the_targets_id_and_deletes_the_merged_blocks_property_lines() {
    let source = "- one\n  id:: aaa\n- two\n  id:: bbb\n";
    let out = merge(source, &[1]);

    assert_eq!(out, "- onetwo\n  id:: aaa\n");
    // No `id:: bbb` survives and no new `id::` is minted for the target.
    let facts = PageFacts::extract(&parse(&out));
    assert_eq!(facts.blocks().len(), 1);
    assert_eq!(
        facts.blocks()[0]
            .property("id")
            .map(|property| property.value()),
        Some("aaa")
    );
}

#[test]
fn merge_keeps_a_custom_property_line_of_the_target() {
    let out = merge("- a\n  todo:: x\n- b\n", &[1]);

    assert_eq!(out, "- ab\n  todo:: x\n");
    assert_eq!(editable_texts(&out), vec!["ab"]);
}

#[test]
fn merge_keeps_a_property_line_between_the_targets_content_lines() {
    // The target states `id::` between two content lines, which is a shape Logseq reads even though it
    // does not write it. Merging must keep the key on the same line of the block.
    let source = "- a\n  id:: u\n  b\n- c\n";
    let out = merge(source, &[1]);

    assert_eq!(out, "- a\n  id:: u\n  bc\n");
    let facts = PageFacts::extract(&parse(&out));
    assert_eq!(facts.blocks().len(), 1);
    assert_eq!(
        facts.blocks()[0].property("id").map(|p| p.value()),
        Some("u")
    );
}

#[test]
fn merge_joins_the_two_texts_with_no_separator_of_its_own() {
    let out = merge("- a \n- b\n", &[1]);

    assert_eq!(out, "- a b\n");
}

#[test]
fn merge_joins_projected_text_without_the_dropped_boundary_blank() {
    let source = "- a\n\n- b\n";
    let out = merge(source, &[1]);

    assert_eq!(out, "- ab\n");
    assert_eq!(editable_texts(&out), vec!["ab"]);
}

#[test]
fn merge_a_block_with_continuation_lines_keeps_them_in_order() {
    let source = "- one\n  two\n- three\n  four\n";
    let out = merge(source, &[1]);

    // The texts join with no separator, so the last line of the first text and the first line of the
    // second become one line: that is what "append the text" means for a multi-line block.
    assert_eq!(out, "- one\n  twothree\n  four\n");
    assert_eq!(editable_texts(&out), vec!["one\ntwothree\nfour"]);
}

#[test]
fn merge_keeps_the_targets_marker_bytes() {
    assert_eq!(merge("- a\n- b\n", &[1]), "- ab\n");
    // `*` and `+` are text, so a merge that leaves one of them in place leaves it as a continuation
    // line of the merged block, never as a new block.
    assert_eq!(merge("- a\n- * b\n", &[1]), "- a* b\n");
}

#[test]
fn merge_a_two_space_child_into_a_tab_indented_parent_reindents_to_tabs() {
    let source = "- parent\n  - first\n\t- sibling\n";
    let out = merge(source, &[0, 0]);

    assert_eq!(out, "- parentfirst\n\t- sibling\n");
}

#[test]
fn merge_keeps_a_bom_crlf_and_missing_final_newline() {
    assert_eq!(merge("\u{feff}- a\r\n- b\r\n", &[1]), "\u{feff}- ab\r\n");
    assert_eq!(merge("- a\n- b", &[1]), "- ab");
}

#[test]
fn merge_refuses_when_the_joined_text_would_state_a_new_property() {
    // `x:: 1` is fenced code inside the second block, so the block states no property. Joined into
    // the block above, the fence no longer opens and the line reads as a property line.
    let source = "- a\n- ```\n  x:: 1\n  ```\n";
    assert_eq!(
        merge_error(source, &[1]),
        EditError::ContinuationChangesStructure
    );
}

#[test]
fn merge_leaves_the_lines_around_the_two_blocks_byte_for_byte() {
    let source = "title:: 首页\n\n- before\n- a\n- b\n- after\n";
    let out = merge(source, &[2]);

    assert_eq!(out, "title:: 首页\n\n- before\n- ab\n- after\n");
}

// --- unwrap ----------------------------------------------------------------------------------------

#[test]
fn unwrap_removes_a_block_and_lifts_what_it_held_into_its_place() {
    let source = "- parent\n  -\n  - kept\n";
    let out = unwrap(source, &[0, 0]);

    assert_eq!(out, "- parent\n  - kept\n");
    assert_eq!(shape(&out), "parent[kept]");
}

#[test]
fn unwrap_of_an_empty_leaf_leaves_the_siblings_untouched() {
    let out = unwrap("- a\n- \n- c\n", &[1]);

    assert_eq!(out, "- a\n- c\n");
}

#[test]
fn unwrap_lifts_a_whole_subtree_one_level_up() {
    let source = "- parent\n\t- gone\n\t\t- kept\n\t\t- also\n\t- after\n";
    let out = unwrap(source, &[0, 0]);

    assert_eq!(out, "- parent\n\t- kept\n\t- also\n\t- after\n");
    assert_eq!(shape(&out), "parent[kept | also | after]");
}

#[test]
fn unwrap_of_a_top_level_block_lifts_its_children_to_the_top_level() {
    let source = "- gone\n\t- kept\n\t\t- deeper\n- after\n";
    let out = unwrap(source, &[0]);

    assert_eq!(out, "- kept\n\t- deeper\n- after\n");
}

#[test]
fn unwrap_a_child_indented_wider_than_the_documents_unit_still_reaches_the_top_level() {
    // The page's indentation unit is two spaces, taken from `a`, but `child` is indented four. One
    // shared delta would leave `child` at column two — a child of `a` — instead of at the top level
    // the deleted block sat on.
    let source = "- a\n  - kept\n- gone\n    - child\n";
    let out = unwrap(source, &[1]);

    assert_eq!(out, "- a\n  - kept\n- child\n");
    assert_eq!(shape(&out), "a[kept] | child");
}

#[test]
fn unwrap_lifts_children_with_different_indents_to_the_one_level_it_empties() {
    // `wide` is indented six and `narrow` three, and both are children of `gone` at two. Each lands on
    // two, so both become children of `parent` again.
    let source = "- parent\n  - gone\n      - wide\n   - narrow\n";
    let out = unwrap(source, &[0, 0]);

    assert_eq!(out, "- parent\n  - wide\n  - narrow\n");
    assert_eq!(shape(&out), "parent[wide | narrow]");
}

#[test]
fn unwrap_keeps_the_inner_indentation_of_each_lifted_subtree() {
    let source = "- gone\n    - a\n        - deeper\n- after\n";
    let out = unwrap(source, &[0]);

    assert_eq!(out, "- a\n    - deeper\n- after\n");
    assert_eq!(shape(&out), "a[deeper] | after");
}

#[test]
fn unwrap_deletes_the_blocks_own_property_lines_with_it() {
    let source = "- parent\n  - gone\n    id:: 11111111-2222-3333-4444-555555555555\n    - kept\n";
    let out = unwrap(source, &[0, 0]);

    assert_eq!(out, "- parent\n  - kept\n");
    let facts = PageFacts::extract(&parse(&out));
    assert_eq!(facts.blocks().len(), 2);
    for block in facts.blocks() {
        assert!(block.properties().is_empty(), "the deleted id:: is gone");
    }
}

#[test]
fn unwrap_moves_no_text_into_the_parent() {
    // Unlike a merge, the deleted block's own text disappears instead of joining the block above.
    let out = unwrap("- parent\n\t- child text\n", &[0, 0]);

    assert_eq!(out, "- parent\n");
    assert_eq!(editable_texts(&out), vec!["parent"]);
}

#[test]
fn unwrap_the_last_block_keeps_the_files_final_newline_state() {
    assert_eq!(unwrap("- a\n- b\n", &[1]), "- a\n");
    // The surviving line keeps its own ending, which is the newline it already had.
    assert_eq!(unwrap("- a\n- b", &[1]), "- a\n");
    assert_eq!(unwrap("- a\n- b\n\n", &[1]), "- a\n\n");
}

#[test]
fn unwrap_keeps_a_bom_and_crlf() {
    assert_eq!(unwrap("\u{feff}- a\r\n- b\r\n", &[0]), "\u{feff}- b\r\n");
}

#[test]
fn unwrap_of_an_unknown_id_is_reported() {
    let document = parse("- a\n");
    assert_eq!(
        document.unwrap_block_text(BlockLocator::Id("nope")),
        Err(EditError::BlockNotFound)
    );
}

#[test]
fn unwrap_leaves_the_lines_around_the_block_byte_for_byte() {
    let source = "title:: 首页\n\n- before\n- a\n- b\n- after\n";
    let out = unwrap(source, &[2]);

    assert_eq!(out, "title:: 首页\n\n- before\n- a\n- after\n");
}

// --- indent ----------------------------------------------------------------------------------------

#[test]
fn indent_makes_a_block_the_last_child_of_the_one_above_it() {
    let source = "- a\n\t- existing\n- b\n";
    let out = indent(source, &[1]);

    assert_eq!(out, "- a\n\t- existing\n\t- b\n");
    assert_eq!(shape(&out), "a[existing | b]");
}

#[test]
fn indent_a_first_child_becomes_its_previous_siblings_last_child() {
    let source = "- a\n\t- x\n\t- y\n";
    let out = indent(source, &[0, 1]);

    assert_eq!(out, "- a\n\t- x\n\t\t- y\n");
    assert_eq!(shape(&out), "a[x[y]]");
}

#[test]
fn indent_accepts_an_id_as_the_address_of_the_block() {
    // A bare `-` is the other marker spelling the file can write, and it must survive the re-indent.
    let source = "- a\n-\n  id:: 11111111-2222-3333-4444-555555555555\n";
    let document = parse(source);
    let out = document
        .indent_block_text(BlockLocator::Id("11111111-2222-3333-4444-555555555555"))
        .expect("the indent should succeed");

    assert_eq!(
        out,
        "- a\n  -\n    id:: 11111111-2222-3333-4444-555555555555\n"
    );
}

#[test]
fn indent_a_middle_child_leaves_the_old_parent_and_the_new_one_with_the_right_children() {
    // `y` leaves `p`, which keeps `x` and `z`, and joins `x`, which had none.
    let source = "- p\n\t- x\n\t- y\n\t- z\n";
    let out = indent(source, &[0, 1]);

    assert_eq!(out, "- p\n\t- x\n\t\t- y\n\t- z\n");
    assert_eq!(shape(&out), "p[x[y] | z]");
}

#[test]
fn indent_with_no_previous_sibling_is_refused() {
    assert_eq!(
        indent_error("- a\n- b\n", &[0]),
        EditError::NoPreviousSibling
    );
}

#[test]
fn indent_moves_the_whole_subtree_and_keeps_its_inner_indentation() {
    let source = "- a\n- b\n  - c\n    - d\n- e\n";
    let out = indent(source, &[1]);

    assert_eq!(out, "- a\n  - b\n    - c\n      - d\n- e\n");
}

#[test]
fn indent_a_tab_indented_file_gains_tabs() {
    let source = "- a\n- b\n\t- c\n";
    let out = indent(source, &[1]);

    assert_eq!(out, "- a\n\t- b\n\t\t- c\n");
}

#[test]
fn indent_keeps_the_blocks_text_id_and_marker_bytes() {
    let source = "- a\n- b\n  id:: 11111111-2222-3333-4444-555555555555\n";
    let out = indent(source, &[1]);

    assert_eq!(
        out,
        "- a\n  - b\n    id:: 11111111-2222-3333-4444-555555555555\n"
    );
    assert_eq!(editable_texts(&out), vec!["a", "b"]);
}

#[test]
fn indent_keeps_a_star_line_as_continuation_text() {
    // `curl -v` output under a block: `* b` belongs to `a`, so indenting `c` moves only `c`. Before
    // only `-` opened a block, `* b` was a second block and this line was a structural edit.
    let source = "- a\n  * b\n- c\n";
    let out = indent(source, &[1]);

    assert_eq!(out, "- a\n  * b\n  - c\n");
    assert_eq!(shape(&out), "a\n* b[c]");
}

#[test]
fn indent_the_last_block_of_the_page_keeps_the_final_newline_state() {
    assert_eq!(indent("- a\n- b\n", &[1]), "- a\n  - b\n");
    assert_eq!(indent("- a\n- b", &[1]), "- a\n  - b");
}

#[test]
fn indent_keeps_a_bom_and_crlf() {
    assert_eq!(
        indent("\u{feff}- a\r\n- b\r\n", &[1]),
        "\u{feff}- a\r\n  - b\r\n"
    );
}

#[test]
fn indent_keeps_a_block_that_holds_a_code_fence_intact() {
    let source = "- a\n- b\n  ```\n  - not a block\n  ```\n";
    let out = indent(source, &[1]);

    assert_eq!(out, "- a\n  - b\n    ```\n    - not a block\n    ```\n");
    assert_eq!(shape(&out), "a[b\n```\n- not a block\n```]");
}

#[test]
fn indent_leaves_the_lines_around_the_block_byte_for_byte() {
    let source = "title:: 首页\n\n- before\n- a\n- b\n- after\n";
    let out = indent(source, &[2]);

    assert_eq!(out, "title:: 首页\n\n- before\n- a\n  - b\n- after\n");
}

// --- outdent ---------------------------------------------------------------------------------------

#[test]
fn outdent_lifts_a_block_to_a_sibling_of_its_parent_right_after_it() {
    let source = "- a\n\t- child\n- after\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(out, "- a\n- child\n- after\n");
    assert_eq!(shape(&out), "a | child | after");
}

#[test]
fn outdent_of_a_top_level_block_is_refused() {
    assert_eq!(
        outdent_error("- top\n", &[0]),
        EditError::TopLevelCannotOutdent
    );
}

#[test]
fn outdent_moves_the_whole_subtree_and_keeps_the_remaining_children_in_order() {
    let source = "- a\n\t- b\n\t\t- deep\n\t- c\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(out, "- a\n\t- c\n- b\n\t- deep\n");
    assert_eq!(shape(&out), "a[c] | b[deep]");
}

#[test]
fn outdent_a_block_two_levels_deep_becomes_its_parents_next_sibling() {
    // It lands after the parent's whole subtree, so after the sibling that used to follow it.
    let source = "- a\n\t- b\n\t\t- c\n\t\t- c2\n";
    let out = outdent(source, &[0, 0, 0]);

    assert_eq!(out, "- a\n\t- b\n\t\t- c2\n\t- c\n");
    assert_eq!(shape(&out), "a[b[c2] | c]");
}

#[test]
fn outdent_a_two_space_child_loses_two_spaces_from_every_moved_line() {
    let source = "- a\n  - b\n    - deep\n  - c\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(out, "- a\n  - c\n- b\n  - deep\n");
}

#[test]
fn outdent_keeps_the_blocks_text_id_and_marker_bytes() {
    let source =
        "- a\n\t- b\n\t  id:: 11111111-2222-3333-4444-555555555555\n\t  collapsed:: true\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(
        out,
        "- a\n- b\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n"
    );
}

#[test]
fn outdent_the_last_child_lands_after_every_sibling_of_its_parent() {
    let source = "- a\n\t- b\n- c\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(out, "- a\n- b\n- c\n");
}

#[test]
fn outdent_keeps_a_bom_and_crlf() {
    assert_eq!(
        outdent("\u{feff}- a\r\n\t- b\r\n", &[0, 0]),
        "\u{feff}- a\r\n- b\r\n"
    );
}

#[test]
fn outdent_keeps_a_missing_final_newline() {
    assert_eq!(outdent("- a\n\t- b", &[0, 0]), "- a\n- b");
}

#[test]
fn outdent_leaves_the_lines_around_the_block_byte_for_byte() {
    let source = "title:: 首页\n\n- a\n\t- b\n- after\n";
    let out = outdent(source, &[0, 0]);

    assert_eq!(out, "title:: 首页\n\n- a\n- b\n- after\n");
}

// --- round trip of the editable text ---------------------------------------------------------------

#[test]
fn writing_the_editable_text_back_leaves_identity_lines_in_place() {
    // An editor writes back the text it shows; `update_block_text` keeps `id::` and `collapsed::`, so
    // for the shape Logseq writes — content first, then its identity — the bytes do not move.
    let source = "- 正文\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n";
    let document = parse(source);
    let path = locate(&document, &[0]);
    let text = PageFacts::extract(&document).blocks()[0].editable_text();
    let out = document
        .update_block_text(BlockLocator::Path(&path), &text)
        .expect("the update should succeed");

    assert_eq!(out, source);
}
