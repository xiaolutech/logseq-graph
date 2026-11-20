//! Content-fact extraction over the fixtures inherited from the predecessor graph.
//!
//! These tests pin the facts a real Logseq page states: block text, page and block properties,
//! page refs, block refs, and tags. Unit-level rules live in `document::facts`; this file checks
//! that the rules compose on files written by Logseq itself.

mod support;

use logseq_graph::document::{parse, PageFacts};
use logseq_graph::domain::{Priority, TaskMarker};
use support::document_fixture;

const PARSER_FIXTURE: &str = document_fixture!("predecessor-fixture-parser.md");
const QUERY_FIXTURE: &str = document_fixture!("predecessor-todo-query.md");
const JOURNAL_FIXTURE: &str = document_fixture!("predecessor-journal.md");

fn facts_of(source: &str) -> PageFacts {
    PageFacts::extract(&parse(source))
}

#[test]
fn page_properties_are_read_from_the_preamble() {
    let facts = facts_of(PARSER_FIXTURE);

    assert_eq!(facts.properties().len(), 2);
    assert_eq!(facts.properties()[0].line(), 0);
    assert_eq!(facts.properties()[0].key(), "type");
    assert_eq!(facts.properties()[0].value(), "[[notes]]");
    assert_eq!(facts.properties()[1].key(), "tags");
    // A property value is raw text: comma separation and `[[...]]` are the projection's problem.
    assert_eq!(facts.properties()[1].value(), "fixture, 测试");

    assert_eq!(
        facts_of(QUERY_FIXTURE).property("icon").map(|p| p.value()),
        Some("✅")
    );
    assert!(facts_of(JOURNAL_FIXTURE).properties().is_empty());
}

#[test]
fn block_text_is_the_blocks_own_content_without_marker_or_indentation() {
    let facts = facts_of(PARSER_FIXTURE);
    let texts: Vec<&str> = facts.blocks().iter().map(|block| block.text()).collect();

    assert_eq!(
        texts,
        vec![
            "第一块 #tag1",
            "子块 ((aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee)) 引用",
            "DONE 完成的事 #done\nid:: 11111111-2222-3333-4444-555555555555\ncollapsed:: true",
            "### 标题块",
            "普通文本续行",
            "代码示例",
            "```bash\n# 注释行\necho hi\n```",
            "列表不是块标记",
            "1. 这是续行不是块\n2. 也是续行",
        ]
    );

    // `### 标题块` is a bullet whose content starts with `###`, so those hashes are content, and a
    // fenced block keeps its fence: block text is the block's own source, not a rendering of it.
    assert_eq!(facts.blocks()[3].text(), "### 标题块");
    assert_eq!(facts.blocks()[6].text(), "```bash\n# 注释行\necho hi\n```");
}

#[test]
fn block_properties_refs_and_tags_land_on_the_block_that_states_them() {
    let facts = facts_of(PARSER_FIXTURE);

    let tagged = &facts.blocks()[0];
    assert_eq!(tagged.tags(), ["tag1".to_owned()]);
    assert!(tagged.page_refs().is_empty());

    let linked = &facts.blocks()[1];
    assert_eq!(
        linked.block_refs(),
        ["aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned()]
    );

    let task = &facts.blocks()[2];
    assert_eq!(task.tags(), ["done".to_owned()]);
    // `DONE` is a task marker and nothing follows it as a priority.
    assert_eq!(task.task_marker(), Some(TaskMarker::Done));
    assert_eq!(task.priority(), None);
    assert_eq!(task.properties().len(), 2);
    assert_eq!(task.properties()[0].line(), 6);
    assert_eq!(task.properties()[0].key(), "id");
    assert_eq!(task.properties()[1].normalized_key(), "collapsed");

    // The ordered-list line stays continuation text: no block, no marker, no property.
    let list = &facts.blocks()[8];
    assert_eq!(list.text(), "1. 这是续行不是块\n2. 也是续行");
    assert!(list.properties().is_empty());
}

#[test]
fn fenced_code_produces_no_tags_and_no_refs() {
    // `# 注释行` is a shell comment inside a fence, not a tag.
    let facts = facts_of(PARSER_FIXTURE);
    let code = &facts.blocks()[6];
    assert_eq!(code.text(), "```bash\n# 注释行\necho hi\n```");
    assert!(code.tags().is_empty());
    assert!(code.page_refs().is_empty());
}

#[test]
fn a_begin_query_region_produces_no_tags() {
    let facts = facts_of(QUERY_FIXTURE);
    let block = &facts.blocks()[0];

    // The query body contains `#{"NOW" "LATER" "DOING" "TODO"}`, which is not a tag.
    assert!(block.text().contains("#{\"NOW\""));
    assert!(block.tags().is_empty());
    assert!(block.page_refs().is_empty());
    // The property line above the region is still a block property.
    assert_eq!(
        block.property("query-table").map(|p| p.value()),
        Some("false")
    );
}

#[test]
fn journal_refs_are_page_refs_on_their_blocks() {
    let facts = facts_of(JOURNAL_FIXTURE);
    let refs: Vec<Vec<String>> = facts
        .blocks()
        .iter()
        .map(|block| block.page_refs().to_vec())
        .collect();

    assert_eq!(
        refs,
        vec![
            vec![],
            vec![],
            vec!["工作目标".to_owned()],
            vec!["杂项".to_owned()],
            vec!["活动日志".to_owned()],
            vec!["Logseq".to_owned()],
        ]
    );
    assert_eq!(
        facts.blocks()[0].text(),
        "{{renderer :todomaster}}",
        "a renderer line states no property and no ref"
    );
}

#[test]
fn every_task_marker_is_read_from_the_first_word_of_the_text() {
    for marker in TaskMarker::ALL {
        let source = format!("- {} buy milk\n", marker.as_str());
        let facts = facts_of(&source);
        let block = &facts.blocks()[0];

        assert_eq!(block.task_marker(), Some(marker), "{}", marker.as_str());
        assert_eq!(block.priority(), None, "{}", marker.as_str());
        // The fact is read out of the text, not cut out of it: the marker word stays where the file
        // wrote it, and so does everything after it.
        assert_eq!(block.text(), format!("{} buy milk", marker.as_str()));
    }
}

#[test]
fn a_marker_word_alone_in_other_casing_or_inside_a_longer_word_is_not_a_task() {
    for source in [
        "- TODO\n",
        "- TODO   \t\n",
        "- todo buy milk\n",
        "- Todo buy milk\n",
        "- tOdO buy milk\n",
        "- TODOS buy milk\n",
        "- TODO:buy milk\n",
        "- TODO-x\n",
    ] {
        let facts = facts_of(source);
        let block = &facts.blocks()[0];
        assert_eq!(block.task_marker(), None, "{source:?}");
        assert_eq!(block.priority(), None, "{source:?}");
    }
}

#[test]
fn a_priority_is_read_after_the_marker_or_at_the_start_of_the_text() {
    for priority in Priority::ALL {
        let with_task = format!("- TODO [#{}] buy milk\n", priority.as_str());
        let hole = facts_of(&with_task);
        let block = &hole.blocks()[0];
        assert_eq!(block.task_marker(), Some(TaskMarker::Todo));
        assert_eq!(block.priority(), Some(priority));

        // Without a marker the group still is the priority, as long as it begins the text.
        let alone = format!("- [#{}] buy milk\n", priority.as_str());
        let hole = facts_of(&alone);
        let block = &hole.blocks()[0];
        assert_eq!(block.task_marker(), None);
        assert_eq!(block.priority(), Some(priority));
    }

    // The letter reads in either case; the marker word does not, and a group that is not where a
    // priority belongs is not the priority.
    assert_eq!(
        facts_of("- DOING [#b] write it\n").blocks()[0].priority(),
        Some(Priority::B)
    );
    for source in [
        "- TODO buy [#A] milk\n",
        "- TODO [#D] buy milk\n",
        "- TODO [#AA] buy milk\n",
        "- TODO #A buy milk\n",
    ] {
        let facts = facts_of(source);
        let block = &facts.blocks()[0];
        assert_eq!(block.priority(), None, "{source:?}");
    }

    // The whitespace after the bullet is already gone from the text, so a group written with any
    // spacing there still begins the text and is the priority of a block that states no marker.
    let spaced = facts_of("-  [#A] buy milk\n");
    assert_eq!(spaced.blocks()[0].text(), "[#A] buy milk");
    assert_eq!(spaced.blocks()[0].priority(), Some(Priority::A));

    // A group is the whole fact, so nothing has to follow it.
    let facts = facts_of("- TODO [#C]\n");
    let bare = &facts.blocks()[0];
    assert_eq!(bare.task_marker(), Some(TaskMarker::Todo));
    assert_eq!(bare.priority(), Some(Priority::C));
}

#[test]
fn a_block_starting_with_a_code_fence_or_begin_src_is_not_a_task() {
    // `TaskMarker::leading` is a content-prefix check on byte 0 of `BlockFacts::text`, not a
    // Markdown parser that strips opaque regions: it never inspects continuation lines. When a
    // block opens with a fence delimiter or `#+BEGIN_SRC`, that delimiter is the first word of the
    // text, so a `TODO` on a continuation line is not a task.
    //
    // This genuinely guards against adding opaque-region stripping (like `VisibleLines`) into
    // `task_marker()`: stripping would drop the fence line, expose `TODO in a fence` as the
    // leading line, and falsely report a task.
    for source in [
        "- ```\n  TODO in a fence\n  ```\n",
        "- #+BEGIN_SRC text\n  TODO in a region\n  #+END_SRC\n",
    ] {
        let facts = facts_of(source);
        let block = &facts.blocks()[0];
        assert!(block.text().contains("TODO"));
        assert_eq!(block.task_marker(), None, "{source:?}");
        assert_eq!(block.priority(), None, "{source:?}");
    }
}

#[test]
fn a_marker_word_in_the_middle_of_the_text_is_not_a_task() {
    // Only byte zero is the leading position: a marker word appearing later in the description —
    // whether in plain text or inside an inline code span like `see `TODO`` — is content, not a
    // task state.
    for source in ["- buy TODO milk\n", "- see `TODO`\n"] {
        let facts = facts_of(source);
        let block = &facts.blocks()[0];
        assert_eq!(block.task_marker(), None, "{source:?}");
        assert_eq!(block.priority(), None, "{source:?}");
    }
}

#[test]
fn reading_task_facts_changes_neither_the_text_nor_its_properties() {
    let facts = facts_of(
        "- TODO [#A] buy milk\n  id:: 61111111-1111-1111-1111-111111111111\n  collapsed:: true\n",
    );
    let block = &facts.blocks()[0];

    assert_eq!(block.task_marker(), Some(TaskMarker::Todo));
    assert_eq!(block.priority(), Some(Priority::A));
    // The words stay in the text and the property lines stay where they were: the two facts are a
    // read, so nothing an editor replaces or a property read sees may move.
    assert_eq!(
        block.text(),
        "TODO [#A] buy milk\nid:: 61111111-1111-1111-1111-111111111111\ncollapsed:: true"
    );
    assert_eq!(block.properties().len(), 2);
    assert_eq!(
        block.property("collapsed").map(|property| property.value()),
        Some("true")
    );
    assert!(block.durable_id().is_some());
}

#[test]
fn facts_follow_the_structure_of_every_fixture() {
    for (name, source) in [
        ("fixture-parser", PARSER_FIXTURE),
        ("todo-query", QUERY_FIXTURE),
        ("journal", JOURNAL_FIXTURE),
    ] {
        let document = parse(source);
        let facts = PageFacts::extract(&document);
        let preorder = document.blocks_in_preorder();

        assert_eq!(facts.blocks().len(), preorder.len(), "{name} block count");
        for (facts, block) in facts.blocks().iter().zip(preorder) {
            assert_eq!(facts.own_span(), block.own_span(), "{name} span order");
            assert!(!facts.text().is_empty(), "{name} block text");
        }
    }
}
