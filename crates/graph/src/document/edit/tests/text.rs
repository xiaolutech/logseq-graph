//! text edit tests.

use super::*;

#[test]
fn setting_the_text_a_block_already_has_changes_no_byte() {
    for source in idempotency_sources() {
        let document = parse(&source);
        let facts = PageFacts::extract(&document);
        for (position, block) in document.blocks_in_preorder().iter().enumerate() {
            let text = facts_of(&facts, block.own_span()).editable_text();
            let path = path_of(&document, position);
            let edited = document
                .set_block_text_text(BlockLocator::Path(&path), &text)
                .unwrap_or_else(|error| {
                    panic!("block {position} of {source:?} was refused: {error}")
                });
            assert_eq!(
                edited, source,
                "block {position} of {source:?}, set to its own editable text {text:?}, is the same document"
            );
        }
    }
}

#[test]
fn properties_before_the_content_stay_before_it() {
    assert_set_text(
        &format!("- id:: {ID_A}\n  old\n  more\n- next\n"),
        ID_A,
        "new\nline",
        &format!("- id:: {ID_A}\n  new\n  line\n- next\n"),
    );
}

#[test]
fn properties_after_the_content_stay_after_it() {
    assert_set_text(
        &format!("- old\n  more\n  id:: {ID_A}\n  collapsed:: true\n- next\n"),
        ID_A,
        "new",
        &format!("- new\n  id:: {ID_A}\n  collapsed:: true\n- next\n"),
    );
}

#[test]
fn properties_on_both_sides_of_the_content_stay_on_their_own_side() {
    assert_set_text(
        &format!("- id:: {ID_A}\n  old\n  collapsed:: true\n- next\n"),
        ID_A,
        "new\nline",
        &format!("- id:: {ID_A}\n  new\n  line\n  collapsed:: true\n- next\n"),
    );
}

#[test]
fn a_property_between_two_content_lines_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n  b\n- next\n"));
    assert_eq!(
        document.set_block_text_text(BlockLocator::Id(ID_A), "c"),
        Err(EditError::InterleavedProperties)
    );
}

#[test]
fn a_block_with_no_content_takes_its_first_content_after_the_properties() {
    assert_set_text(
        &format!("- id:: {ID_A}\n  collapsed:: true\n- next\n"),
        ID_A,
        "new",
        &format!("- id:: {ID_A}\n  collapsed:: true\n  new\n- next\n"),
    );
}

#[test]
fn emptying_the_text_of_a_block_whose_marker_line_is_a_property_drops_the_content_lines() {
    assert_set_text(
        &format!("- id:: {ID_A}\n  more\n  collapsed:: true\n- next\n"),
        ID_A,
        "",
        &format!("- id:: {ID_A}\n  collapsed:: true\n- next\n"),
    );
}

#[test]
fn empty_text_leaves_a_marker_line_without_trailing_whitespace() {
    // The marker line's own ending and the page's trailing blank lines survive.
    let document = parse("- a\n\n\n");
    assert_eq!(
        document
            .set_block_text_text(BlockLocator::Path(&[0]), "")
            .expect("the edit should succeed"),
        "-\n\n\n"
    );
    let after = assert_set_text(
        &format!("- a\n  id:: {ID_A}\n- next\n"),
        ID_A,
        "",
        &format!("-\n  id:: {ID_A}\n- next\n"),
    );
    assert_eq!(after.blocks()[0].own_span(), LineSpan::new(0, 2));
}

#[test]
fn a_heading_keeps_its_content_column_when_its_text_grows() {
    assert_set_text(
        &format!("## a\n   id:: {ID_A}\n"),
        ID_A,
        "b\nc",
        &format!("## b\n   c\n   id:: {ID_A}\n"),
    );
}

#[test]
fn a_continuation_line_is_aligned_to_the_content_column() {
    assert_set_text(
        &format!("- a\n  id:: {ID_A}\n- next\n"),
        ID_A,
        "one\ntwo",
        &format!("- one\n  two\n  id:: {ID_A}\n- next\n"),
    );
}

#[test]
fn set_text_reaches_a_tab_indented_block_with_tabs() {
    assert_set_text(
        &format!("- parent\n\t- child\n\t  id:: {ID_A}\n"),
        ID_A,
        "one\ntwo",
        &format!("- parent\n\t- one\n\t\t\ttwo\n\t  id:: {ID_A}\n"),
    );
}

#[test]
fn set_text_keeps_a_bom_crlf_and_a_missing_final_newline() {
    let after = assert_set_text(
        &format!("\u{feff}- a\r\n  id:: {ID_A}"),
        ID_A,
        "b",
        &format!("\u{feff}- b\r\n  id:: {ID_A}"),
    );
    assert!(after.has_bom());
    assert!(!after.ends_with_newline());
}

#[test]
fn a_continuation_that_would_change_the_structure_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- next\n"));
    for text in [
        "b\n- x",
        "b\n# x",
        "b\n  - x",
        "b\nkey:: value",
        "b\n#+BEGIN_SRC js",
    ] {
        assert_eq!(
            document.set_block_text_text(BlockLocator::Id(ID_A), text),
            Err(EditError::ContinuationChangesStructure),
            "{text:?} would change the structure"
        );
    }
}

#[test]
fn set_text_writes_a_marker_or_property_inside_a_fence_as_text() {
    let after = assert_set_text(
        &format!("- a\n  id:: {ID_A}\n- next\n"),
        ID_A,
        "b\n```\n- x\nkey:: value\n```",
        &format!("- b\n  ```\n  - x\n  key:: value\n  ```\n  id:: {ID_A}\n- next\n"),
    );
    assert_eq!(after.blocks().len(), 2);
}

#[test]
fn a_trailing_blank_content_line_needs_somewhere_to_go() {
    // Nothing but blank lines follows this block, so the blank line would become page trailing
    // space instead of block text.
    assert_eq!(
        parse("- a\n").set_block_text_text(BlockLocator::Path(&[0]), "b\n"),
        Err(EditError::ContinuationChangesStructure)
    );
    // A property line after the content gives the blank line a place: it stays inside the block.
    assert_set_text(
        &format!("- a\n  id:: {ID_A}\n"),
        ID_A,
        "b\n",
        &format!("- b\n\n  id:: {ID_A}\n"),
    );
}

#[test]
fn set_text_rejects_a_carriage_return() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    for text in ["b\r", "b\r\nc", "b\nc\r"] {
        assert_eq!(
            document.set_block_text_text(BlockLocator::Id(ID_A), text),
            Err(EditError::MultilineContent)
        );
    }
}
