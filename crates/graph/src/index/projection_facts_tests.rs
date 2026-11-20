use crate::index::test_support::*;
use crate::index::Index;

#[test]
fn stored_parent_linkage_matches_the_parent_implied_by_depth() {
    let mut index = Index::open_in_memory().expect("open index");
    project(
        &mut index,
        "page",
        "- a\n\t- b\n\t\t- c\n\t- d\n- e\n\t- f\n",
    )
    .expect("replace page");
    assert_eq!(
        index
            .page_metadata("page")
            .expect("read metadata")
            .expect("page exists")
            .block_count(),
        6,
        "the check below must not pass on an empty page"
    );
    assert_eq!(
        parent_linkage_mismatches(&index, "page"),
        Vec::<String>::new()
    );

    // The assertion must be able to fail: point a top-level block at a parent it cannot have.
    index
        .conn
        .execute(
            "UPDATE blocks
                SET parent_id = (SELECT id FROM blocks WHERE preorder = 0)
              WHERE preorder = 4",
            [],
        )
        .expect("corrupt one parent link");
    let mismatches = parent_linkage_mismatches(&index, "page");
    assert_eq!(
        mismatches.len(),
        1,
        "exactly the corrupted link must be reported, got {mismatches:?}"
    );
    assert!(
        mismatches[0].contains("preorder 4"),
        "the report must name the corrupted row: {mismatches:?}"
    );
}

#[test]
fn content_facts_are_projected_alongside_the_structure() {
    let mut index = Index::open_in_memory().expect("open index");
    project(&mut index, "facts", FACTS_PAGE).expect("replace page");

    assert_eq!(
        fact_rows(&index, PAGE_PROPERTIES, "Index slice"),
        ["0|title|title|Index slice", "1|Done_At|done-at|2026-10-29"],
        "page properties keep the written key, the normalized key, and the raw value"
    );
    // The `id::` line belongs to the block that wrote it, not to the page.
    assert_eq!(
        fact_rows(&index, BLOCK_PROPERTIES, "Index slice"),
        ["0|0|id|id|11111111-2222-3333-4444-555555555555"]
    );
    // `[[a/b]]` also references its namespace parent, `#[[multi word]]` is a page ref as well as
    // a tag, and the block ref is stored lowercased.
    assert_eq!(
        fact_rows(&index, PAGE_REFS, "Index slice"),
        [
            "0|Linked Page|linked page",
            "1|a|a",
            "1|a/b|a/b",
            "1|multi word|multi word"
        ]
    );
    assert_eq!(
        fact_rows(&index, BLOCK_REFS, "Index slice"),
        ["0|61745deb-eeed-4426-8020-eb9e5c90d558"]
    );
    assert_eq!(
        fact_rows(&index, TAGS, "Index slice"),
        ["0|tag|tag", "1|multi word|multi word"]
    );
}

#[test]
fn block_text_is_stored_from_the_same_parse_as_the_structure() {
    let mut index = Index::open_in_memory().expect("open index");
    project(&mut index, "text", "- first\n  continuation\n- \n").expect("replace page");

    // The marker and the alignment indentation are gone, continuation lines are joined, and an
    // empty block stores an empty string rather than NULL.
    assert_eq!(
        all_rows(
            &index,
            "SELECT blocks.preorder || '|' || blocks.text
               FROM blocks
               JOIN pages ON pages.id = blocks.page_id
              WHERE pages.name = 'text'
              ORDER BY blocks.preorder"
        ),
        ["0|first\ncontinuation", "1|"]
    );
}

#[test]
fn a_first_block_property_stays_a_block_property_for_now() {
    // Known boundary, not the intended final behavior. Logseq also lets a page state a property on
    // its first block (`- title:: ...`), and `document::facts` correctly assigns that line to the
    // block. No fixture or spec in this repository says how a first-block property and a preamble
    // property of one key are ordered, so the read reports only what the projection calls a page
    // property instead of guessing; the compatibility slice that decides is the one to change this
    // test.
    let mut index = Index::open_in_memory().expect("open index");
    project(&mut index, "blocky", "- title:: 我的页\n- a second block\n").expect("replace page");

    assert_eq!(
        index
            .page_properties("blocky")
            .expect("read page properties"),
        Some(Vec::new()),
        "the page exists and states no preamble property"
    );
    assert_eq!(
        fact_rows(&index, BLOCK_PROPERTIES, "blocky"),
        ["0|0|title|title|我的页"],
        "the line is stored, just as a block property"
    );
    assert_eq!(
        fact_rows(&index, PAGE_PROPERTIES, "blocky"),
        Vec::<String>::new(),
        "no page property row was written for it"
    );
}

#[test]
fn text_that_only_looks_like_a_fact_stores_nothing() {
    let mut index = Index::open_in_memory().expect("open index");
    let source = "\
12:: a digit cannot start a key
foo bar:: a key cannot contain a space
real:: value

- 正文 C# ##heading [#A] [[unclosed ((not-a-uuid)) #/a/b
  ```bash
  [[fenced]] #fenced
  ```
  #+BEGIN_SRC text
  [[directive]] #directive
  #+END_SRC
  行内 `[[inline]] #inline` 之后
  真实 [[Real Page]] #real
";
    project(&mut index, "lookalike", source).expect("replace page");

    // The positive rows are asserted in full, so the negatives cannot pass on an empty table.
    assert_eq!(
        fact_rows(&index, PAGE_PROPERTIES, "lookalike"),
        ["0|real|real|value"]
    );
    assert_eq!(
        fact_rows(&index, PAGE_REFS, "lookalike"),
        ["0|Real Page|real page"]
    );
    assert_eq!(fact_rows(&index, TAGS, "lookalike"), ["0|real|real"]);
    assert_eq!(
        fact_rows(&index, BLOCK_PROPERTIES, "lookalike"),
        Vec::<String>::new()
    );
    assert_eq!(
        fact_rows(&index, BLOCK_REFS, "lookalike"),
        Vec::<String>::new()
    );
}
