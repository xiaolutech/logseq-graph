//! marker edit tests.

use super::*;

#[test]
fn a_bullet_becomes_a_heading_and_the_own_lines_follow_the_new_column() {
    assert_marker(
        &format!("- a\n  more\n  id:: {ID_A}\n"),
        0,
        BlockMarker::Heading { level: 2 },
        &format!("## a\n   more\n   id:: {ID_A}\n"),
    );
}

#[test]
fn a_heading_becomes_a_bullet_and_the_own_lines_follow_the_new_column() {
    assert_marker(
        &format!("### a\n    more\n    id:: {ID_A}\n"),
        0,
        BlockMarker::Bullet,
        &format!("- a\n  more\n  id:: {ID_A}\n"),
    );
}

#[test]
fn one_heading_level_becomes_another() {
    assert_marker(
        "### a\n    more\n",
        0,
        BlockMarker::Heading { level: 6 },
        "###### a\n       more\n",
    );
}

#[test]
fn a_bullet_keeps_the_character_the_file_wrote() {
    // Only `-` is a bullet marker; `*` and `+` are text, so there is no other spelling to keep.
    assert_marker("- a\n", 0, BlockMarker::Bullet, "- a\n");
}

#[test]
fn a_heading_of_the_same_level_changes_nothing() {
    assert_marker(
        "## a\n   more\n",
        0,
        BlockMarker::Heading { level: 2 },
        "## a\n   more\n",
    );
}

#[test]
fn an_empty_heading_becomes_a_bare_bullet() {
    // `## ` has no content, so the bullet needs no separator: the rendered marker line has none,
    // and the content column the property line follows is the narrower one that leaves.
    assert_marker(
        &format!("## \n   id:: {ID_A}\n"),
        0,
        BlockMarker::Bullet,
        &format!("-\n id:: {ID_A}\n"),
    );
}

#[test]
fn an_empty_bullet_becomes_a_heading_that_keeps_its_separator() {
    // `##` with no space after it is not a heading marker at all, so the separator stays even
    // though there is no content to separate.
    assert_marker(
        &format!("-\n id:: {ID_A}\n"),
        0,
        BlockMarker::Heading { level: 2 },
        &format!("## \n   id:: {ID_A}\n"),
    );
}

#[test]
fn a_heading_outside_the_six_levels_is_rejected() {
    let document = parse("- a\n");
    for level in [0, 7, u8::MAX] {
        assert_eq!(
            document
                .set_block_marker_text(BlockLocator::Path(&[0]), BlockMarker::Heading { level }),
            Err(EditError::InvalidHeadingLevel { level })
        );
    }
}

#[test]
fn children_keep_their_own_bytes_through_a_marker_change() {
    let after = assert_marker(
        &format!("- a\n  - child\n    id:: {ID_B}\n"),
        0,
        BlockMarker::Heading { level: 2 },
        &format!("## a\n  - child\n    id:: {ID_B}\n"),
    );
    assert_eq!(after.blocks()[0].children()[0].indent(), 2);
}

#[test]
fn a_property_line_can_end_a_page_with_or_without_a_final_newline() {
    // The page's only block is also its last line, so the property line has no following line
    // to take an ending from — the shape that used to index one line past the document.
    for (source, expected) in [
        ("- a\n", "- a\n  collapsed:: true\n"),
        ("- a", "- a\n  collapsed:: true"),
    ] {
        let before = parse(source);
        let edited = before
            .upsert_block_property_text(BlockLocator::Indices(&[0]), "collapsed", "true")
            .expect("the page's only block takes a property line");
        assert_eq!(edited, expected, "byte-exact output for {source:?}");
        assert_eq!(
            edited.ends_with('\n'),
            source.ends_with('\n'),
            "the final-newline state of {source:?} survives"
        );

        // The line reads back as a property of the same block, and the block carries the same
        // editable text and the same place in the page.
        let after = parse(&edited);
        assert_eq!(block_shapes(&after), block_shapes(&before));
        assert_eq!(after.blocks().len(), 1);
        let facts = PageFacts::extract(&after);
        let block = facts.blocks().first().expect("one block");
        assert_eq!(block.editable_text(), "a");
        assert_eq!(
            block.property("collapsed").map(|property| property.value()),
            Some("true")
        );
    }
}

#[test]
fn a_tab_indented_block_is_realigned_with_the_documents_own_unit() {
    assert_marker(
        &format!("- parent\n\t- child\n\t  more\n\t  id:: {ID_A}\n"),
        1,
        BlockMarker::Heading { level: 2 },
        &format!("- parent\n\t## child\n\t\t\t\tmore\n\t\t\t\tid:: {ID_A}\n"),
    );
}
