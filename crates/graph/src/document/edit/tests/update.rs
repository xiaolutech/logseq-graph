//! update edit tests.

use super::*;

#[test]
fn replaces_the_text_of_the_block_that_states_the_id() {
    // The id line is not part of `text`, so keeping it is the whole point of this edit.
    let after = assert_update(
        &format!("- a\n  id:: {ID_A}\n"),
        ID_A,
        "b",
        &format!("- b\n  id:: {ID_A}\n"),
    );
    assert_eq!(block_index(&after, ID_A), 0);
}

#[test]
fn the_marker_bytes_survive() {
    assert_update(
        &format!("### a\n  id:: {ID_A}\n"),
        ID_A,
        "b",
        &format!("### b\n    id:: {ID_A}\n"),
    );
}

#[test]
fn a_bare_bullet_gets_the_separator_the_content_needs() {
    // `-` alone is a block marker, but `-a` is not a marker line at all.
    assert_update(
        &format!("-\n  id:: {ID_A}\n"),
        ID_A,
        "a",
        &format!("- a\n  id:: {ID_A}\n"),
    );
}

#[test]
fn children_stay_where_they_are() {
    let source = format!("- parent\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- sibling\n");
    let expected = format!("- renamed\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- sibling\n");
    let after = assert_update(&source, ID_A, "renamed", &expected);
    assert_eq!(after.blocks().len(), 2);
    assert_eq!(after.blocks()[0].children().len(), 1);
    assert_eq!(after.blocks()[0].children()[0].indent(), 2);
    assert_eq!(block_index(&after, ID_B), 1);
}

#[test]
fn three_continuation_lines_become_one() {
    let source = format!("- a\n  one\n  two\n  three\n  id:: {ID_A}\n- next\n");
    let expected = format!("- b\n  id:: {ID_A}\n- next\n");
    let after = assert_update(&source, ID_A, "b", &expected);
    assert_eq!(after.blocks().len(), 2);
    assert_eq!(after.blocks()[1].own_span(), LineSpan::new(2, 3));
}

#[test]
fn one_line_becomes_three_continuation_lines() {
    let source = format!("- parent\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n");
    let expected = format!("- parent\n  id:: {ID_A}\n  - x\n    y\n    z\n    id:: {ID_B}\n");
    let after = assert_update(&source, ID_B, "x\ny\nz", &expected);
    assert_eq!(after.blocks_in_preorder().len(), 2);
    assert_eq!(
        after.blocks()[0].children()[0].own_span(),
        LineSpan::new(2, 6)
    );
}

#[test]
fn the_collapsed_state_survives() {
    assert_update(
        &format!("- a\n  id:: {ID_A}\n  collapsed:: true\n"),
        ID_A,
        "b",
        &format!("- b\n  id:: {ID_A}\n  collapsed:: true\n"),
    );
}

#[test]
fn an_id_declared_by_the_new_text_is_not_duplicated() {
    // The only place a new text can declare a property is its first line: a property line on a
    // continuation line is rejected as a structural change.
    let after = assert_update(
        &format!("- a\n  id:: {ID_A}\n"),
        ID_A,
        &format!("id:: {ID_B}"),
        &format!("- id:: {ID_B}\n"),
    );
    assert_eq!(block_index(&after, ID_B), 0);
}

#[test]
fn an_unknown_id_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    assert_eq!(
        document.update_block_text(BlockLocator::Id(ID_B), "b"),
        Err(EditError::BlockNotFound)
    );
}

#[test]
fn an_id_stated_by_two_blocks_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- b\n  id:: {ID_A}\n"));
    assert_eq!(
        document.update_block_text(BlockLocator::Id(ID_A), "c"),
        Err(EditError::AmbiguousBlockId)
    );
}

#[test]
fn the_id_lookup_ignores_ascii_case() {
    let upper = ID_A.to_uppercase();
    let expected = format!("- b\n  id:: {upper}\n");
    // The uppercase spelling in the file is preserved byte for byte, while the lookup takes
    // the lowercase form that the domain normalizes to.
    assert_update(&format!("- a\n  id:: {upper}\n"), ID_A, "b", &expected);
}

#[test]
fn a_continuation_that_would_become_a_block_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- next\n"));
    for text in ["b\n- x", "b\n# x", "b\n  - x", "b\n  #+BEGIN_SRC js"] {
        assert_eq!(
            document.update_block_text(BlockLocator::Id(ID_A), text),
            Err(EditError::ContinuationChangesStructure),
            "{text:?} would change the structure"
        );
    }
}

#[test]
fn a_continuation_that_would_become_a_property_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    for text in ["b\nkey:: value", "b\nid:: value"] {
        assert_eq!(
            document.update_block_text(BlockLocator::Id(ID_A), text),
            Err(EditError::ContinuationChangesStructure),
            "{text:?} would add a property"
        );
    }
}

#[test]
fn a_marker_or_property_inside_a_fence_is_text() {
    // The fence makes these lines opaque, so they stay content of the same block.
    let source = format!("- a\n  id:: {ID_A}\n- next\n");
    let expected = format!("- b\n  ```\n  - x\n  key:: value\n  ```\n  id:: {ID_A}\n- next\n");
    assert_update(&source, ID_A, "b\n```\n- x\nkey:: value\n```", &expected);
}

#[test]
fn a_trailing_blank_line_that_would_become_page_trailing_space_is_rejected() {
    // Only reachable when the new text declares the id itself, because otherwise the identity
    // line is written after the content and the blank line stays inside the block.
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    assert_eq!(
        document.update_block_text(BlockLocator::Id(ID_A), &format!("id:: {ID_B}\n")),
        Err(EditError::ContinuationChangesStructure)
    );
}

#[test]
fn a_blank_last_line_stays_inside_the_block() {
    assert_update(
        &format!("- a\n  id:: {ID_A}\n\n\n"),
        ID_A,
        "b\n",
        &format!("- b\n\n  id:: {ID_A}\n\n\n"),
    );
}

#[test]
fn a_carriage_return_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    for text in ["b\r", "b\r\nc", "b\nc\r"] {
        assert_eq!(
            document.update_block_text(BlockLocator::Id(ID_A), text),
            Err(EditError::MultilineContent)
        );
    }
}

#[test]
fn empty_text_leaves_only_the_marker_line() {
    let source = format!("- a\n  id:: {ID_A}\n- next\n");
    let edited = parse(&source)
        .update_block_text(BlockLocator::Id(ID_A), "")
        .expect("an empty content keeps the identity line");
    assert_eq!(edited, format!("-\n id:: {ID_A}\n- next\n"));
    let after = parse(&edited);
    let facts = PageFacts::extract(&after);
    assert_eq!(facts.blocks()[0].editable_text(), "");
    assert_eq!(facts.blocks()[0].property("id").unwrap().value(), ID_A);
    assert_eq!(after.blocks()[0].own_span(), LineSpan::new(0, 2));
    assert_eq!(after.blocks()[0].marker(), BlockMarker::Bullet);
}

#[test]
fn leading_and_boundary_empty_content_lines_survive_projection() {
    let source = "- old\n- next\n";
    for text in ["\nfirst", "first\n", "first\n\n"] {
        let output = parse(source)
            .update_block_text(BlockLocator::Path(&[0]), text)
            .expect("whitespace-only boundary lines retain the projected newline");
        assert_eq!(PageFacts::extract(&parse(&output)).blocks()[0].text(), text);
    }
}

#[test]
fn a_bom_crlf_and_a_missing_final_newline_survive() {
    let source = format!("\u{feff}- a\r\n  id:: {ID_A}");
    let after = assert_update(&source, ID_A, "b", &format!("\u{feff}- b\r\n  id:: {ID_A}"));
    assert!(after.has_bom());
    assert!(!after.ends_with_newline());
}

#[test]
fn bytes_outside_the_edited_block_are_untouched() {
    let source = format!(
        "title:: x\n\n- a\n  one\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- b\n  id:: {ID_C}\n\n\n"
    );
    let expected = format!(
        "title:: x\n\n- new\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- b\n  id:: {ID_C}\n\n\n"
    );
    let before = parse(&source);
    let span = own_span_of(&before, ID_A);
    let head = line_offset(&before, span.start());
    let tail = line_offset(&before, span.end());

    let after = assert_update(&source, ID_A, "new", &expected);
    let edited = after.to_source();
    assert_eq!(
        &edited[..head],
        &source[..head],
        "every byte before the block"
    );
    assert_eq!(
        &edited[edited.len() - (source.len() - tail)..],
        &source[tail..],
        "every byte after the block"
    );
    assert_eq!(after.preamble(), LineSpan::new(0, 2));
    assert_eq!(after.trailing(), LineSpan::new(8, 10));
}
