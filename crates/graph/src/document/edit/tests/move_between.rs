//! move between edit tests.

use super::*;

#[test]
fn moves_a_subtree_between_pages_before_and_after_the_anchor() {
    // One source, four destinations: every anchor position is a different junction.
    let source = format!("- a\n  id:: {ID_A}\n  - a child\n- b\n  id:: {ID_B}\n");
    let destination = format!("- x\n  id:: {ID_C}\n  - x child\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::Before(BlockLocator::Id(ID_C)),
        &format!("- b\n  id:: {ID_B}\n"),
        &format!("- a\n  id:: {ID_A}\n  - a child\n- x\n  id:: {ID_C}\n  - x child\n"),
    );
    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_C)),
        &format!("- b\n  id:: {ID_B}\n"),
        &format!("- x\n  id:: {ID_C}\n  - x child\n- a\n  id:: {ID_A}\n  - a child\n"),
    );
    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::FirstChildOf(BlockLocator::Id(ID_C)),
        &format!("- b\n  id:: {ID_B}\n"),
        &format!("- x\n  id:: {ID_C}\n  - a\n    id:: {ID_A}\n    - a child\n  - x child\n"),
    );
    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_C)),
        &format!("- b\n  id:: {ID_B}\n"),
        &format!("- x\n  id:: {ID_C}\n  - x child\n  - a\n    id:: {ID_A}\n    - a child\n"),
    );
}

#[test]
fn the_destination_keeps_its_bom_line_endings_and_trailing_space() {
    // The source is plain LF without a final newline; the destination carries a BOM, ends CRLF,
    // and keeps trailing blank lines. Both stay exactly as they were outside the pasted run.
    let source = format!("- a\n  id:: {ID_A}\n  - a child");
    let destination = format!("\u{feff}- x\r\n  id:: {ID_C}\r\n  - x child\r\n\r\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_C)),
        "",
        &format!(
            "\u{feff}- x\r\n  id:: {ID_C}\r\n  - x child\r\n- a\n  id:: {ID_A}\n  - a child\r\n\r\n"
        ),
    );
}

#[test]
fn the_moved_lines_keep_their_endings_and_the_junction_takes_the_destination_line() {
    // The moved block carries CRLF lines into an LF destination: its own lines keep CRLF, and
    // the junction — where the run meets the destination — takes the ending of the line it
    // lands before, exactly as a same-page move does.
    let source = format!("- a\r\n  id:: {ID_A}\r\n- b\r\n");
    let destination = format!("- x\n  id:: {ID_C}\n- y\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::Before(BlockLocator::Id(ID_C)),
        "- b\r\n",
        &format!("- a\r\n  id:: {ID_A}\n- x\n  id:: {ID_C}\n- y\n"),
    );
}

#[test]
fn a_moved_subtree_without_a_final_newline_lands_at_the_destination_end() {
    // The moved subtree is the last thing in a source file without a final newline, and it is
    // pasted at the end of a destination that has one: the source stays without, the
    // destination keeps its own.
    let source = format!("- a\n- b\n  id:: {ID_A}\n  - b child");
    let destination = format!("- x\n  id:: {ID_C}\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_C)),
        "- a\n",
        &format!("- x\n  id:: {ID_C}\n- b\n  id:: {ID_A}\n  - b child\n"),
    );
}

#[test]
fn a_moved_subtree_landing_last_keeps_the_destination_final_newline_state() {
    // The destination has no final newline and the run lands at its end, so the pasted last
    // line must not gain one.
    let source = format!("- b\n  id:: {ID_A}\n");
    let destination = format!("- x\n  id:: {ID_C}");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_C)),
        "",
        &format!("- x\n  id:: {ID_C}\n- b\n  id:: {ID_A}"),
    );
}

#[test]
fn a_tab_indented_subtree_moves_into_a_two_space_destination() {
    // The destination's unit decides what deeper means: the pasted root gains two spaces, and
    // the subtree keeps its internal tabs below that.
    let source = format!("- b\n\tid:: {ID_A}\n\t- b child\n");
    let destination = format!("- x\n  id:: {ID_C}\n  - x child\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_C)),
        "",
        &format!("- x\n  id:: {ID_C}\n  - x child\n  - b\n  \tid:: {ID_A}\n  \t- b child\n"),
    );
}

#[test]
fn a_two_space_subtree_moves_into_a_tab_destination() {
    let source = format!("- b\n  id:: {ID_A}\n  - b child\n");
    let destination = format!("- x\n\tid:: {ID_C}\n\t- x child\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::FirstChildOf(BlockLocator::Id(ID_C)),
        "",
        &format!("- x\n\tid:: {ID_C}\n\t- b\n\t  id:: {ID_A}\n\t  - b child\n\t- x child\n"),
    );
}

#[test]
fn a_blank_line_inside_the_moved_subtree_survives_the_cross_page_move() {
    let source = format!("- a\n  id:: {ID_A}\n\n  after blank\n- b\n");
    let destination = format!("- x\n  id:: {ID_C}\n");

    assert_move_between(
        &source,
        &destination,
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_C)),
        "- b\n",
        &format!("- x\n  id:: {ID_C}\n- a\n  id:: {ID_A}\n\n  after blank\n"),
    );
}

#[test]
fn bytes_outside_both_edited_ranges_are_untouched() {
    // Preambles, siblings, and trailing blank lines of both pages come back byte for byte.
    let source = format!("title:: s\n\n- a\n  id:: {ID_A}\n  - a child\n- b\n  id:: {ID_B}\n\n\n");
    let destination = format!("title:: d\n\n- x\n  id:: {ID_C}\n- y\n\n");

    let (edited_source, edited_destination) = parse(&source)
        .move_block_text_to_page(
            BlockLocator::Id(ID_A),
            &parse(&destination),
            MoveTarget::LastChildOf(BlockLocator::Id(ID_C)),
        )
        .expect("the edit should succeed");
    assert_eq!(
        edited_source,
        format!("title:: s\n\n- b\n  id:: {ID_B}\n\n\n")
    );
    assert_eq!(
        edited_destination,
        format!("title:: d\n\n- x\n  id:: {ID_C}\n  - a\n    id:: {ID_A}\n    - a child\n- y\n\n")
    );
}

#[test]
fn the_source_bom_survives_even_when_the_first_block_leaves() {
    let source = format!("\u{feff}- a\n  id:: {ID_A}\n- b\n");
    let destination = format!("- x\n  id:: {ID_C}\n");

    let (edited_source, edited_destination) = parse(&source)
        .move_block_text_to_page(
            BlockLocator::Id(ID_A),
            &parse(&destination),
            MoveTarget::After(BlockLocator::Id(ID_C)),
        )
        .expect("the edit should succeed");
    assert_eq!(edited_source, "\u{feff}- b\n");
    assert_eq!(
        edited_destination,
        format!("- x\n  id:: {ID_C}\n- a\n  id:: {ID_A}\n")
    );
}

#[test]
fn a_cross_page_missing_or_ambiguous_id_is_reported_per_side_and_document() {
    let source = format!("- a\n  id:: {ID_A}\n");
    let destination = format!("- x\n  id:: {ID_C}\n");
    let destination_document = parse(&destination);

    // The moved id is missing: `BlockNotFound` names the source side.
    assert_eq!(
        parse(&source).move_block_text_to_page(
            BlockLocator::Id(ID_B),
            &destination_document,
            MoveTarget::Before(BlockLocator::Id(ID_C)),
        ),
        Err(EditError::BlockNotFound)
    );
    // The target id is missing: `MoveTargetNotFound` names the destination side.
    assert_eq!(
        parse(&source).move_block_text_to_page(
            BlockLocator::Id(ID_A),
            &destination_document,
            MoveTarget::Before(BlockLocator::Id(ID_B)),
        ),
        Err(EditError::MoveTargetNotFound)
    );
    // A source id stated twice is ambiguous; a destination id stated twice is the same error.
    let ambiguous_source = parse(&format!("- a\n  id:: {ID_A}\n- b\n  id:: {ID_A}\n"));
    assert_eq!(
        ambiguous_source.move_block_text_to_page(
            BlockLocator::Id(ID_A),
            &destination_document,
            MoveTarget::Before(BlockLocator::Id(ID_C)),
        ),
        Err(EditError::AmbiguousBlockId)
    );
    let ambiguous_destination = parse(&format!("- x\n  id:: {ID_C}\n- y\n  id:: {ID_C}\n"));
    assert_eq!(
        parse(&source).move_block_text_to_page(
            BlockLocator::Id(ID_A),
            &ambiguous_destination,
            MoveTarget::Before(BlockLocator::Id(ID_C)),
        ),
        Err(EditError::AmbiguousBlockId)
    );
}
