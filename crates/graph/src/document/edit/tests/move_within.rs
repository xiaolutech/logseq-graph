//! move within edit tests.

use super::*;

#[test]
fn moves_a_block_after_a_later_sibling() {
    assert_move(
        &format!("- a\n  id:: {ID_A}\n- b\n  id:: {ID_B}\n"),
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_B)),
        &format!("- b\n  id:: {ID_B}\n- a\n  id:: {ID_A}\n"),
    );
}

#[test]
fn moves_a_block_before_an_earlier_sibling() {
    assert_move(
        &format!("- a\n  id:: {ID_A}\n- b\n  id:: {ID_B}\n"),
        ID_B,
        MoveTarget::Before(BlockLocator::Id(ID_A)),
        &format!("- b\n  id:: {ID_B}\n- a\n  id:: {ID_A}\n"),
    );
}

#[test]
fn moves_a_block_under_another_as_its_last_child() {
    let after = assert_move(
        &format!("- parent\n  id:: {ID_A}\n  - child\n- tail\n  id:: {ID_B}\n"),
        ID_B,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_A)),
        &format!("- parent\n  id:: {ID_A}\n  - child\n  - tail\n    id:: {ID_B}\n"),
    );
    assert_eq!(
        after.blocks()[0].children().len(),
        2,
        "the moved block is the parent's second child"
    );
}

#[test]
fn lands_before_existing_children_when_asked_for_the_first_child() {
    assert_move(
        &format!("- parent\n  id:: {ID_A}\n  - child\n    id:: {ID_C}\n- tail\n  id:: {ID_B}\n"),
        ID_B,
        MoveTarget::FirstChildOf(BlockLocator::Id(ID_A)),
        &format!(
            "- parent\n  id:: {ID_A}\n  - tail\n    id:: {ID_B}\n  - child\n    id:: {ID_C}\n"
        ),
    );
}

#[test]
fn moves_a_whole_subtree_and_keeps_its_relative_indentation() {
    assert_move(
        &format!("- a\n  id:: {ID_A}\n  - a child\n    - a grandchild\n- b\n  id:: {ID_B}\n"),
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_B)),
        &format!(
            "- b\n  id:: {ID_B}\n  - a\n    id:: {ID_A}\n    - a child\n      - a grandchild\n"
        ),
    );
}

#[test]
fn a_tab_indented_file_stays_tab_indented() {
    let after = assert_move(
        &format!("- parent\n\tid:: {ID_C}\n\t- child\n- other\n\tid:: {ID_A}\n"),
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_C)),
        &format!("- parent\n\tid:: {ID_C}\n\t- child\n\t- other\n\t\tid:: {ID_A}\n"),
    );
    assert!(
        after
            .to_source()
            .lines()
            .all(|line| !lead_whitespace(line).contains(' ')),
        "a tab-indented page gains no space in its indentation"
    );
}

#[test]
fn continuation_lines_move_and_are_reindented_with_the_block() {
    assert_move(
        &format!(
            "- a\n  id:: {ID_A}\n  first continuation\n  second continuation\n- b\n  id:: {ID_B}\n"
        ),
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_B)),
        &format!(
            "- b\n  id:: {ID_B}\n  - a\n    id:: {ID_A}\n    first continuation\n    second continuation\n"
        ),
    );
}

#[test]
fn a_shallower_move_strips_exactly_the_delta_and_leaves_the_rest() {
    // The identity line is indented further than its block, and the move removes two characters
    // from every line: it must not reformat the remaining whitespace to the new depth.
    assert_move(
        &format!("- parent\n  id:: {ID_B}\n  - deep\n      id:: {ID_A}\n- tail\n  id:: {ID_C}\n"),
        ID_A,
        MoveTarget::Before(BlockLocator::Id(ID_C)),
        &format!("- parent\n  id:: {ID_B}\n- deep\n    id:: {ID_A}\n- tail\n  id:: {ID_C}\n"),
    );
}

#[test]
fn refuses_a_target_that_is_the_moved_block_itself() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- b\n"));
    for target in [
        MoveTarget::Before(BlockLocator::Id(ID_A)),
        MoveTarget::After(BlockLocator::Id(ID_A)),
        MoveTarget::FirstChildOf(BlockLocator::Id(ID_A)),
        MoveTarget::LastChildOf(BlockLocator::Id(ID_A)),
    ] {
        assert_eq!(
            document.move_block_text(BlockLocator::Id(ID_A), target.clone()),
            Err(EditError::TargetInsideMovedSubtree),
            "{target:?} would move the block into itself"
        );
    }
}

#[test]
fn refuses_a_target_inside_the_subtree_being_moved() {
    let document = parse(&format!(
        "- a\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- c\n"
    ));
    for target in [
        MoveTarget::Before(BlockLocator::Id(ID_B)),
        MoveTarget::After(BlockLocator::Id(ID_B)),
        MoveTarget::FirstChildOf(BlockLocator::Id(ID_B)),
        MoveTarget::LastChildOf(BlockLocator::Id(ID_B)),
    ] {
        assert_eq!(
            document.move_block_text(BlockLocator::Id(ID_A), target.clone()),
            Err(EditError::TargetInsideMovedSubtree),
            "{target:?} names a block the same edit deletes"
        );
    }
}

#[test]
fn lands_after_every_existing_child_of_the_target() {
    assert_move(
        &format!("- parent\n  id:: {ID_A}\n  - one\n  - two\n- tail\n  id:: {ID_B}\n"),
        ID_B,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_A)),
        &format!("- parent\n  id:: {ID_A}\n  - one\n  - two\n  - tail\n    id:: {ID_B}\n"),
    );
}

#[test]
fn a_crlf_file_stays_crlf() {
    let after = assert_move(
        &format!("- a\r\n  id:: {ID_A}\r\n- b\r\n  id:: {ID_B}\r\n"),
        ID_A,
        MoveTarget::After(BlockLocator::Id(ID_B)),
        &format!("- b\r\n  id:: {ID_B}\r\n- a\r\n  id:: {ID_A}\r\n"),
    );
    let source = after.to_source();
    assert!(
        !source.replace("\r\n", "").contains('\n'),
        "no line was written with a bare LF"
    );
}

#[test]
fn the_last_moved_line_takes_the_ending_of_the_line_it_lands_before() {
    // The two blocks end differently, so which newline the junction gets says where the ending
    // came from: the pasted block's own lines keep theirs, the last one takes the next line's.
    assert_move(
        &format!("- a\r\n  id:: {ID_A}\r\n- b\n  id:: {ID_B}\n"),
        ID_B,
        MoveTarget::Before(BlockLocator::Id(ID_A)),
        &format!("- b\n  id:: {ID_B}\r\n- a\r\n  id:: {ID_A}\r\n"),
    );
}

#[test]
fn the_bom_and_the_preamble_stay_where_they_are() {
    let after = assert_move(
        &format!("\u{feff}title:: x\n\n- a\n  id:: {ID_B}\n- b\n  id:: {ID_A}\n"),
        ID_A,
        MoveTarget::Before(BlockLocator::Id(ID_B)),
        &format!("\u{feff}title:: x\n\n- b\n  id:: {ID_A}\n- a\n  id:: {ID_B}\n"),
    );
    assert!(after.has_bom());
    assert_eq!(after.preamble(), LineSpan::new(0, 2));
}

#[test]
fn moves_the_last_block_to_the_front() {
    assert_move(
        &format!("- a\n  id:: {ID_B}\n- c\n- b\n  id:: {ID_A}\n"),
        ID_A,
        MoveTarget::Before(BlockLocator::Id(ID_B)),
        &format!("- b\n  id:: {ID_A}\n- a\n  id:: {ID_B}\n- c\n"),
    );
}

#[test]
fn a_missing_source_or_target_id_is_reported_per_side() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    assert_eq!(
        document.move_block_text(
            BlockLocator::Id(ID_B),
            MoveTarget::Before(BlockLocator::Id(ID_A))
        ),
        Err(EditError::BlockNotFound),
        "the moved id is the one that is missing"
    );
    assert_eq!(
        document.move_block_text(
            BlockLocator::Id(ID_A),
            MoveTarget::Before(BlockLocator::Id(ID_B))
        ),
        Err(EditError::MoveTargetNotFound),
        "the target id is the one that is missing"
    );

    let ambiguous = parse(&format!(
        "- a\n  id:: {ID_A}\n- b\n  id:: {ID_B}\n- c\n  id:: {ID_B}\n"
    ));
    assert_eq!(
        ambiguous.move_block_text(
            BlockLocator::Id(ID_A),
            MoveTarget::After(BlockLocator::Id(ID_B))
        ),
        Err(EditError::AmbiguousBlockId)
    );
}

#[test]
fn a_blank_line_inside_the_moved_subtree_stays_blank() {
    assert_move(
        &format!("- a\n  id:: {ID_A}\n\n  after blank\n- b\n  id:: {ID_B}\n"),
        ID_A,
        MoveTarget::LastChildOf(BlockLocator::Id(ID_B)),
        &format!("- b\n  id:: {ID_B}\n  - a\n    id:: {ID_A}\n\n    after blank\n"),
    );
}
