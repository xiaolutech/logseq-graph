//! remove edit tests.

use super::*;

#[test]
fn a_sibling_range_cuts_one_run_and_preserves_surrounding_bytes() {
    for ending in ["\n", "\r\n"] {
        let text = format!("\u{feff}title:: page{ending}- keep{ending}- first{ending}  - child{ending}- last{ending}  - grandchild{ending}- tail{ending}{ending}");
        let document = parse(&text);
        let result = document
            .remove_block_range_text(BlockLocator::Indices(&[1]), BlockLocator::Indices(&[2]))
            .expect("range");
        assert_eq!(
            result,
            format!("\u{feff}title:: page{ending}- keep{ending}- tail{ending}{ending}")
        );
    }
    let document = parse("- parent\n  - first\n    - child\n  - last\n- tail");
    assert_eq!(
        document
            .remove_block_range_text(
                BlockLocator::Indices(&[0, 0]),
                BlockLocator::Indices(&[0, 1])
            )
            .expect("nested siblings"),
        "- parent\n- tail"
    );
    let document = parse("\u{feff}title:: page\r\n- first\n- last\r\n\r\n");
    assert_eq!(
        document
            .remove_block_range_text(BlockLocator::Indices(&[0]), BlockLocator::Indices(&[1]))
            .expect("all blocks"),
        "\u{feff}title:: page\r\n\r\n"
    );
}

#[test]
fn range_resolution_refuses_missing_non_sibling_and_reversed_endpoints() {
    let document = parse("- parent\n  - child\n- last\n");
    for (first, last, expected) in [
        (vec![0], vec![0, 0], EditError::RangeNotSiblings),
        (vec![0, 0], vec![1], EditError::RangeNotSiblings),
        (vec![1], vec![0], EditError::RangeReversed),
        (vec![], vec![0], EditError::PathOutOfRange),
        (vec![0], vec![3], EditError::PathOutOfRange),
    ] {
        assert_eq!(
            document.remove_block_range_text(
                BlockLocator::Indices(&first),
                BlockLocator::Indices(&last)
            ),
            Err(expected)
        );
    }
    assert_eq!(
        document.remove_block_range_text(BlockLocator::Id(ID_A), BlockLocator::Indices(&[0])),
        Err(EditError::BlockNotFound)
    );
    assert_eq!(
        document
            .remove_block_range_text(BlockLocator::Indices(&[0]), BlockLocator::Indices(&[0]))
            .expect("one block"),
        document
            .remove_block_text(BlockLocator::Indices(&[0]))
            .expect("remove")
    );
}

#[test]
fn removing_the_last_block_keeps_the_files_final_newline() {
    let after = assert_remove(&format!("- a\n- b\n  id:: {ID_A}\n"), ID_A, "- a\n");
    assert!(
        after.ends_with_newline(),
        "the file still ends with a newline"
    );
}

#[test]
fn the_line_before_a_deleted_last_line_keeps_its_own_ending() {
    // The file has no final newline, and the line that now ends it is the byte range that was
    // already there. Its newline is a byte of that line, so the edit does not drop it to keep a
    // missing final newline missing.
    assert_remove(&format!("- a\n- b\n  id:: {ID_A}"), ID_A, "- a\n");
}

#[test]
fn a_file_without_a_final_newline_still_ends_without_one() {
    let before = parse(&format!("- a\n  id:: {ID_A}\n- b"));
    assert!(!before.ends_with_newline());
    let after = assert_remove(&before.to_source(), ID_A, "- b");
    assert!(!after.ends_with_newline());
}

#[test]
fn a_blocks_children_are_deleted_with_it() {
    assert_remove(
        &format!("- a\n  id:: {ID_A}\n  - child\n    - grandchild\n- b\n"),
        ID_A,
        "- b\n",
    );
}

#[test]
fn a_middle_block_leaves_its_siblings_byte_for_byte() {
    let after = assert_remove(
        &format!("- one\n- two\n  id:: {ID_A}\n- three\n"),
        ID_A,
        "- one\n- three\n",
    );
    assert_eq!(block_texts(&after), ["one", "three"]);
}

#[test]
fn a_page_whose_only_block_is_deleted_keeps_its_preamble() {
    let after = assert_remove(
        &format!("title:: x\n\n- a\n  id:: {ID_A}\n"),
        ID_A,
        "title:: x\n\n",
    );
    assert!(after.blocks().is_empty());
    assert_eq!(after.preamble(), LineSpan::new(0, 1));
    assert_eq!(after.trailing(), LineSpan::new(1, 2));
}

#[test]
fn a_bom_and_crlf_file_keeps_both() {
    let after = assert_remove(
        &format!("\u{feff}- a\r\n- b\r\n  id:: {ID_A}\r\n- c\r\n"),
        ID_A,
        "\u{feff}- a\r\n- c\r\n",
    );
    assert!(after.has_bom());
}

#[test]
fn trailing_blank_lines_stay_last_after_a_removal() {
    let after = assert_remove(&format!("- a\n- b\n  id:: {ID_A}\n\n\n"), ID_A, "- a\n\n\n");
    assert_eq!(after.trailing(), LineSpan::new(1, 3));
}

#[test]
fn a_tab_indented_subtree_is_deleted_whole() {
    assert_remove(
        &format!("- a\n\t- b\n\t  id:: {ID_A}\n\t\t- c\n- d\n"),
        ID_A,
        "- a\n- d\n",
    );
}

#[test]
fn an_opaque_region_inside_the_subtree_is_deleted_with_it() {
    assert_remove(
        &format!("- a\n  id:: {ID_A}\n  - b\n    #+BEGIN_SRC js\n    code\n    #+END_SRC\n- c\n"),
        ID_A,
        "- c\n",
    );
}

#[test]
fn a_child_stating_its_own_id_does_not_shadow_the_target() {
    assert_remove(
        &format!("- a\n  id:: {ID_A}\n  - child\n    id:: {ID_B}\n- b\n"),
        ID_A,
        "- b\n",
    );
}

#[test]
fn removing_an_id_stated_by_two_blocks_is_rejected() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n- b\n  id:: {ID_A}\n"));
    assert_eq!(
        document.remove_block_text(BlockLocator::Id(ID_A)),
        Err(EditError::AmbiguousBlockId)
    );
}

#[test]
fn removing_an_id_no_block_states_is_an_error() {
    let document = parse(&format!("- a\n  id:: {ID_A}\n"));
    assert_eq!(
        document.remove_block_text(BlockLocator::Id(ID_B)),
        Err(EditError::BlockNotFound)
    );
}

#[test]
fn a_target_with_continuation_lines_takes_them_along() {
    assert_remove(
        &format!("- a\n  id:: {ID_A}\n  more text\n  still more\n- b\n"),
        ID_A,
        "- b\n",
    );
}

#[test]
fn a_tab_indented_block_keeps_tab_indentation_when_its_text_grows() {
    // The continuation column is filled from the document's own unit, so a tab-indented page
    // gains tabs instead of turning into a file that mixes tabs and spaces.
    assert_update(
        &format!("- parent\n\t- child\n\t  id:: {ID_A}\n"),
        ID_A,
        "one\ntwo",
        &format!("- parent\n\t- one\n\t\t\ttwo\n\t\t\tid:: {ID_A}\n"),
    );
}
