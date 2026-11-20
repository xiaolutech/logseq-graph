//! append edit tests.

use super::*;

#[test]
fn appends_one_block_after_the_last_block() {
    assert_edit("- a\n", "b", "- a\n- b\n");
}

#[test]
fn keeps_the_bom() {
    let after = assert_edit("\u{feff}- a\n", "b", "\u{feff}- a\n- b\n");
    assert!(after.has_bom());
}

#[test]
fn a_crlf_file_gets_a_crlf_line() {
    assert_edit("- a\r\n", "b", "- a\r\n- b\r\n");
}

#[test]
fn a_mixed_file_follows_the_line_before_the_insertion_point() {
    // The last line ends with LF, so the new line does too, even though the file starts CRLF.
    assert_edit("- a\r\n- b\n", "c", "- a\r\n- b\n- c\n");
}

#[test]
fn a_missing_final_newline_stays_missing() {
    let after = assert_edit("- a", "b", "- a\n- b");
    assert!(!after.ends_with_newline());
}

#[test]
fn a_missing_final_newline_follows_the_file_ending() {
    // No line before the insertion point has an ending, so the CRLF convention of the file
    // decides instead of a hard-coded LF.
    assert_edit("- a\r\n- b", "c", "- a\r\n- b\r\n- c");
}

#[test]
fn trailing_blank_lines_stay_last() {
    assert_edit("- a\n\n\n", "b", "- a\n- b\n\n\n");
}

#[test]
fn a_trailing_blank_without_a_final_newline_stays_last() {
    let after = assert_edit("- a\n ", "b", "- a\n- b\n ");
    assert!(!after.ends_with_newline());
}

#[test]
fn a_document_with_only_a_preamble_gets_the_block_after_it() {
    let after = assert_edit("title:: x\n", "b", "title:: x\n- b\n");
    assert_eq!(after.preamble(), LineSpan::new(0, 1));
    assert_eq!(after.blocks().len(), 1);
}

#[test]
fn an_empty_document_gets_one_block() {
    let after = assert_edit("", "b", "- b");
    assert!(!after.has_bom());
    assert!(!after.ends_with_newline());
}

#[test]
fn a_blank_only_document_keeps_its_blank_lines() {
    assert_edit("\n", "b", "- b\n\n");
}

#[test]
fn an_unclosed_code_fence_does_not_hide_an_appended_block() {
    assert_edit("- a\n```\ncode\n", "b", "- a\n```\ncode\n- b\n");
}

#[test]
fn an_unclosed_begin_region_is_rejected() {
    let document = parse("- a\n  #+BEGIN_SRC js\n  const x = 1;\n");
    assert_eq!(
        document.append_block("b"),
        Err(EditError::EndsInsideOpaqueRegion)
    );
}

#[test]
fn an_unclosed_fence_in_the_preamble_does_not_hide_an_appended_block() {
    assert_edit("```\ncode\n", "b", "```\ncode\n- b\n");
}

#[test]
fn a_closed_fence_is_not_a_reason_to_reject() {
    assert_edit("- a\n```\ncode\n```\n", "b", "- a\n```\ncode\n```\n- b\n");
}

#[test]
fn blank_content_is_rejected() {
    let document = parse("- a\n");
    for text in ["", " ", "\t", "  \t "] {
        assert_eq!(document.append_block(text), Err(EditError::EmptyContent));
    }
}

#[test]
fn multiline_content_is_rejected() {
    let document = parse("- a\n");
    for text in ["b\nc", "b\n", "\nb", "b\r\nc", "b\r"] {
        assert_eq!(
            document.append_block(text),
            Err(EditError::MultilineContent)
        );
    }
}

#[test]
fn content_that_looks_like_a_marker_is_written_verbatim() {
    assert_edit("- a\n", "- b", "- a\n- - b\n");
    assert_edit("- a\n", "# b", "- a\n- # b\n");
    assert_edit("- a\n", "b  ", "- a\n- b  \n");
}

#[test]
fn leading_whitespace_is_copied_but_parsed_as_marker_whitespace() {
    let edited = parse("- a\n")
        .append_block("  b")
        .expect("the edit should succeed");
    assert_eq!(edited, "- a\n-   b\n");

    let after = parse(&edited);
    assert_eq!(after.blocks().len(), 2);
    assert_eq!(last_top_level_text(&after), "b");
}

#[test]
fn two_appends_chain() {
    let first = parse("- a\n").append_block("b").expect("first edit");
    assert_eq!(first, "- a\n- b\n");

    let second = parse(&first).append_block("c").expect("second edit");
    assert_eq!(second, "- a\n- b\n- c\n");

    let after = parse(&second);
    assert_eq!(after.blocks().len(), 3);
    assert_eq!(last_top_level_text(&after), "c");
}

#[test]
fn existing_bytes_are_untouched() {
    let source = "title:: x\n\n- a\n  continuation\n\t- nested\n- b\n\n\n";
    let before = parse(source);
    let edited = before.append_block("e").expect("the edit should succeed");

    // Every byte before the insertion point is the original byte at that position.
    let prefix_len = usize::from(before.has_bom())
        + before.lines()[..before.trailing().start()]
            .iter()
            .map(|line| line.text().len() + line.ending().map_or(0, |ending| ending.as_str().len()))
            .sum::<usize>();
    assert_eq!(&edited[..prefix_len], &source[..prefix_len]);
    assert_eq!(
        &edited[prefix_len..],
        format!("- e\n{}", &source[prefix_len..])
    );

    let after = parse(&edited);
    assert_eq!(after.blocks().len(), before.blocks().len() + 1);
    assert_eq!(after.trailing(), LineSpan::new(7, 9));
    assert_eq!(last_top_level_text(&after), "e");
}
