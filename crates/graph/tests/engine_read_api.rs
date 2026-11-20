//! Page-level fact reads through the graph facade: properties, backlinks, and tags.
//!
//! Every test writes graph files, opens a [`Graph`], and asks through the public read methods, so it
//! covers the whole path from discovery through projection to the returned view. Each query is
//! asserted next to a case that looks like a hit but must not be one: a name that differs only in
//! case, a dangling reference, a page that does not exist, or a page name that is not a tag.

use logseq_graph::domain::{Backlink, TagUsage};
use logseq_graph::engine::Freshness;

mod support;

use support::TempGraph;

/// Names of the pages a backlink list names, in the order the read returned them.
fn backlink_names(backlinks: &[Backlink]) -> Vec<&str> {
    backlinks.iter().map(Backlink::page).collect()
}

/// Names of a tag summary, in the order the read returned them.
fn tag_names(tags: &[TagUsage]) -> Vec<&str> {
    tags.iter().map(TagUsage::name).collect()
}

#[test]
fn page_properties_keep_written_order_and_repeated_keys() {
    let graph = TempGraph::new("page-properties");
    graph.write(
        "pages/alpha.md",
        "title:: First\nauthor:: Ada\ntitle:: Second\n\n- a block\n",
    );
    graph.write("pages/beta.md", "title:: Beta\n\n- another block\n");
    graph.write("pages/bare.md", "- no property lines here\n");
    let mut opened = graph.open();

    let alpha = opened
        .page_properties("Second")
        .expect("query alpha properties");
    let properties = alpha.present().expect("alpha exists");
    let written: Vec<(&str, &str)> = properties
        .iter()
        .map(|property| (property.key(), property.value()))
        .collect();
    assert_eq!(
        written,
        [("title", "First"), ("author", "Ada"), ("title", "Second")],
        "both lines of a repeated key survive, in written order"
    );
    assert_eq!(
        properties
            .iter()
            .map(|property| property.normalized_key())
            .collect::<Vec<_>>(),
        ["title", "author", "title"],
        "the repeated lines share the key a caller would match on"
    );

    // A property belongs to the page that wrote it: beta has its own `title`, not alpha's.
    let beta = opened
        .page_properties("beta")
        .expect("query beta properties");
    assert_eq!(
        beta.present()
            .expect("beta exists")
            .iter()
            .map(|property| (property.key(), property.value()))
            .collect::<Vec<_>>(),
        [("title", "Beta")]
    );

    // A page without property lines exists and states nothing, which is not the same answer as a
    // page that has no rows at all.
    let bare = opened
        .page_properties("bare")
        .expect("query bare properties");
    assert!(bare.present().expect("bare exists").is_empty());

    let missing = opened
        .page_properties("missing")
        .expect("query missing properties");
    assert!(
        missing.present().is_none(),
        "a page that does not exist has no property list"
    );
}

#[test]
fn page_identity_follows_the_official_pre_block_probe() {
    let graph = TempGraph::new("page-identity");
    graph.write(
        "pages/filename.md",
        concat!(
            "title:: Chosen/Parent/Leaf\n",
            "alias:: Also known, [[Other Alias]]\n",
            "tags:: plain tag, [[Wrapped, Tag]], #hash\n",
            "related:: prose [[Unfiled]] #related-tag\n",
            "description:: \"[[Ignored]]\"\n\n",
            "- sample [[Block ref]] #block-tag\n"
        ),
    );
    graph.write(
        "pages/first bullet.md",
        concat!(
            "- title:: Not A Page Title\n",
            "  alias:: Not An Alias\n",
            "  custom:: [[Bullet ref]]\n",
            "- another\n"
        ),
    );
    graph.write(
        "pages/yaml.md",
        "---\ntitle: YAML Title\ntags: [[YAML Tag]]\n---\n\n- body\n",
    );
    graph.write(
        "pages/prose.md",
        "Leading prose\ntitle:: Late Title\n\n- body\n",
    );
    graph.write(
        "pages/separate.md",
        "author:: first\n\ntitle:: Later Group\n\n- body\n",
    );
    let mut opened = graph.open();
    for name in ["Late Title", "separate", "YAML Title"] {
        assert!(opened
            .page_metadata(name)
            .unwrap()
            .present()
            .unwrap()
            .has_file());
    }
    for name in [
        "Chosen",
        "Chosen/Parent",
        "Also known",
        "Other Alias",
        "Unfiled",
        "plain tag",
        "Wrapped, Tag",
        "hash",
        "related-tag",
        "Block ref",
        "block-tag",
        "YAML Tag",
    ] {
        let page = opened
            .page_metadata(name)
            .expect("read page")
            .into_value()
            .expect(name);
        assert!(!page.has_file(), "{name}");
        assert_eq!(
            opened.page_blocks(name).expect("read blocks").into_value(),
            Some(vec![])
        );
    }
    for absent in [
        "filename",
        "yaml",
        "Ignored",
        "Not A Page Title",
        "Later Group",
    ] {
        assert!(opened
            .page_metadata(absent)
            .expect("read page")
            .present()
            .is_none());
    }
    assert_eq!(
        opened
            .page_blocks("first bullet")
            .unwrap()
            .present()
            .unwrap()
            .len(),
        2
    );
    assert!(opened
        .page_properties("first bullet")
        .unwrap()
        .present()
        .unwrap()
        .is_empty());
    assert_eq!(
        opened.pages_with_tag("wrapped, tag").unwrap().value(),
        &["Chosen/Parent/Leaf"]
    );
    let backlinks = opened.backlinks("unfiled").expect("page property backlink");
    assert_eq!(backlink_names(backlinks.value()), ["Chosen/Parent/Leaf"]);
    assert!(backlinks.value()[0].pre_block());
    assert!(backlinks.value()[0]
        .text()
        .contains("related:: prose [[Unfiled]]"));

    let created = opened
        .create_page("Unfiled")
        .expect("materialize referenced page");
    assert!(created.value().has_file());
    opened
        .append_block("Chosen/Parent/Leaf", "added")
        .expect("write by title");
    assert_eq!(
        opened
            .page_snapshot("Chosen/Parent/Leaf")
            .unwrap()
            .present()
            .unwrap()
            .blocks()
            .len(),
        2
    );

    graph.write("pages/filename.md", "title:: New title\n\n- changed\n");
    opened.rescan().expect("reproject renamed file");
    assert!(opened
        .page_metadata("New title")
        .unwrap()
        .present()
        .unwrap()
        .has_file());
    for removed in [
        "Chosen/Parent/Leaf",
        "Chosen/Parent",
        "Also known",
        "Block ref",
    ] {
        assert!(
            opened.page_metadata(removed).unwrap().present().is_none(),
            "{removed}"
        );
    }
}

#[test]
fn backlinks_match_by_folded_key_once_per_page_and_in_name_order() {
    let graph = TempGraph::new("backlinks");
    graph.write("pages/Ä.md", "- the target page\n");
    graph.write(
        "pages/bravo.md",
        "- sees [[ä]] and [[Ä]]\n\t- nested [[ä|an alias]]\n- again [[ä]]\n",
    );
    graph.write("pages/Charlie.md", "- points at [[Ä][a description]]\n");
    graph.write(
        "pages/delta.md",
        "- unrelated [[other]] and unresolved [[ghost]]\n",
    );
    let mut opened = graph.open();

    // Three references from bravo are one page that references the target, and the result is
    // ordered by folded page name, so `bravo` comes before `Charlie`. The second name is the
    // spelling that page was stored with, not the spelling the target was queried with.
    let backlinks = opened.backlinks("ä").expect("query backlinks of ä");
    assert_eq!(backlink_names(backlinks.value()), ["bravo", "Charlie"]);

    // The folded key is the identity: the other spelling of the target asks the same question.
    let capitalized = opened.backlinks("Ä").expect("query backlinks of Ä");
    assert_eq!(backlink_names(capitalized.value()), ["bravo", "Charlie"]);

    // A reference to another page, and an unresolved reference, match neither of these targets.
    let other = opened.backlinks("other").expect("query backlinks of other");
    assert_eq!(backlink_names(other.value()), ["delta"]);
    let ghost = opened.backlinks("ghost").expect("query backlinks of ghost");
    assert_eq!(
        backlink_names(ghost.value()),
        ["delta"],
        "an unresolved reference still names the page that points at it"
    );
    let missing = opened
        .backlinks("missing")
        .expect("query backlinks of a missing page");
    assert!(
        missing.value().is_empty(),
        "no page references a name nothing links to"
    );
}

#[test]
fn tags_count_distinct_pages_rather_than_blocks() {
    let graph = TempGraph::new("tags");
    graph.write("pages/alpha.md", "- a #shared\n- b #shared\n\t- c #alpha\n");
    graph.write("pages/bravo.md", "- d #shared\n");
    graph.write("pages/charlie.md", "- e #shared\n- f #beta\n");
    graph.write("pages/delta.md", "- a reference [[shared]] but no tag\n");
    let mut opened = graph.open();

    let tags = opened.tags().expect("query tags");
    let counts: Vec<(&str, u32)> = tags
        .value()
        .iter()
        .map(|tag| (tag.name(), tag.page_count()))
        .collect();
    assert_eq!(
        counts,
        [("alpha", 1), ("beta", 1), ("shared", 3)],
        "one page that tags two blocks with #shared counts once, and an unused name is not a tag"
    );
}

#[test]
fn a_tag_is_one_row_whatever_its_spelling() {
    let graph = TempGraph::new("tag-spelling");
    graph.write("pages/alpha.md", "- a #Shared\n");
    graph.write("pages/bravo.md", "- b #SHARED\n");
    let mut opened = graph.open();

    let tags = opened.tags().expect("query tags");
    assert_eq!(
        tag_names(tags.value()),
        ["SHARED"],
        "one folded key is one tag, labelled by the smallest spelled form"
    );
    assert_eq!(tags.value()[0].page_count(), 2);

    let pages = opened
        .pages_with_tag("shared")
        .expect("query pages using the tag");
    assert_eq!(pages.value(), &["alpha".to_owned(), "bravo".to_owned()]);
}

#[test]
fn pages_with_tag_reports_each_page_once_in_folded_name_order() {
    let graph = TempGraph::new("pages-with-tag");
    graph.write("pages/Delta.md", "- a #Tag\n- b #tag\n");
    graph.write("pages/bravo.md", "- c #TAG\n");
    graph.write("pages/zulu.md", "- d #other\n");
    graph.write(
        "pages/echo.md",
        "- names the word [[tag]] without tagging it\n",
    );
    let mut opened = graph.open();

    // Two spellings on one page are one row, and the two pages are ordered by folded name, so
    // `bravo` comes before `Delta` while the result keeps the spelling each page was stored with.
    let pages = opened.pages_with_tag("tag").expect("query pages with tag");
    assert_eq!(pages.value(), &["bravo".to_owned(), "Delta".to_owned()]);

    let capitalized = opened
        .pages_with_tag("TAG")
        .expect("query pages with the capitalized tag");
    assert_eq!(
        capitalized.value(),
        &["bravo".to_owned(), "Delta".to_owned()]
    );

    // zulu tags something else, and echo only references a page with that name.
    let missing = opened
        .pages_with_tag("missing")
        .expect("query pages with a tag nothing uses");
    assert!(missing.value().is_empty());
}

#[test]
fn both_spellings_of_a_page_read_the_same_page() {
    let graph = TempGraph::new("folded-page-lookup");
    graph.write("pages/Ä.md", "title:: Ä\n\n- first block\n\t- child\n");
    graph.write("pages/done_at.md", "- second page\n");
    let mut opened = graph.open();

    // A page name is an identity, not a string: either spelling of it reaches the one page, and what
    // comes back keeps the spelling the page was stored with rather than the one the call used.
    let capitalized = opened.page_metadata("Ä").expect("query metadata of Ä");
    let lowercase = opened.page_metadata("ä").expect("query metadata of ä");
    assert_eq!(
        lowercase.present().expect("ä reaches the page Ä").name(),
        "Ä"
    );
    assert_eq!(lowercase.value(), capitalized.value());

    let tree = opened.page_tree("ä").expect("query tree of ä");
    let tree = tree.present().expect("ä reaches the page Ä");
    assert_eq!(tree.page().name(), "Ä");
    assert_eq!(tree.page().block_count(), 2);

    let properties = opened.page_properties("ä").expect("query properties of ä");
    assert_eq!(properties.present().expect("ä reaches the page Ä").len(), 1);

    // The page rule folds case and nothing else: `Done_At` reaches `done_at`, while `Done-At` names
    // another page. Renaming `_` to `-` is the property-key rule, not the page-name rule.
    let underscore = opened
        .page_metadata("Done_At")
        .expect("query metadata of Done_At");
    assert_eq!(
        underscore
            .present()
            .expect("Done_At reaches done_at")
            .name(),
        "done_at"
    );
    let hyphen = opened
        .page_metadata("Done-At")
        .expect("query metadata of Done-At");
    assert!(
        hyphen.present().is_none(),
        "a hyphen is another page name, not another spelling of the same one"
    );
}

#[test]
fn an_empty_graph_answers_empty_rather_than_absent() {
    let graph = TempGraph::new("empty");
    let mut opened = graph.open();

    let tags = opened.tags().expect("query tags of an empty graph");
    assert!(
        tags.value().is_empty(),
        "an empty graph has no tag row at all, not one row with an empty name: {:?}",
        tags.value()
    );
    assert!(opened
        .backlinks("anything")
        .expect("query backlinks")
        .value()
        .is_empty());
    assert!(opened
        .pages_with_tag("anything")
        .expect("query pages with tag")
        .value()
        .is_empty());
    assert!(opened
        .page_properties("anything")
        .expect("query page properties")
        .present()
        .is_none());
}

#[test]
fn a_self_reference_is_a_backlink_of_its_own_page() {
    let graph = TempGraph::new("self-backlink");
    graph.write("pages/self.md", "- I point at [[self]]\n");
    graph.write("pages/other.md", "- I point at [[Self]]\n");
    let mut opened = graph.open();

    // `self` references itself, and the read does not exclude the page it was asked about: the
    // question is which pages point here, and the queried page is one of them.
    let backlinks = opened.backlinks("self").expect("query backlinks of self");
    assert_eq!(backlink_names(backlinks.value()), ["other", "self"]);
}

#[test]
fn a_journal_backlink_reports_its_date_title() {
    let graph = TempGraph::new("journal-backlink");
    graph.write("journals/2026_08_13.md", "- met [[Someone]]\n");
    graph.write("pages/someone.md", "- a page the journal mentioned\n");
    let mut opened = graph.open();

    // A journal's page name is the date title the configuration renders, not the file name and not
    // the path, so that is what a backlink from a journal carries.
    let backlinks = opened
        .backlinks("someone")
        .expect("query backlinks of someone");
    assert_eq!(backlink_names(backlinks.value()), ["Aug 13th, 2026"]);
}

#[test]
fn fact_reads_go_through_the_freshness_policy() {
    let graph = TempGraph::new("read-freshness");
    graph.write("pages/alpha.md", "- first [[target]]\n");
    let mut opened = graph.open();

    let before = opened.backlinks("target").expect("query backlinks");
    assert_eq!(backlink_names(before.value()), ["alpha"]);
    assert!(!before.is_stale(), "nothing has failed yet");
    let revision = before.meta().revision();
    assert_eq!(
        revision,
        opened
            .metadata()
            .expect("query metadata")
            .value()
            .revision(),
        "a fact read reports the revision it was answered from"
    );

    // An external writer adds a file. The committed scan does not see it until a policy scans again,
    // which is what ADR-0006 records rather than hides.
    graph.write("pages/bravo.md", "- second #fresh\n");
    assert!(
        opened.tags().expect("query tags").value().is_empty(),
        "the last committed scan does not know about the new file yet"
    );

    let checked = opened
        .reads(Freshness::CheckNow)
        .tags()
        .expect("query tags after a check");
    assert_eq!(tag_names(checked.value()), ["fresh"]);
    assert!(!checked.is_stale());
    assert!(
        checked.meta().revision() > revision,
        "applying the new file advanced the revision"
    );
}

#[test]
fn a_namespace_answers_its_children_and_its_subtree() {
    let graph = TempGraph::new("namespaces");
    graph.write("pages/a.md", "- the namespace page itself\n");
    graph.write("pages/a___b.md", "- one level down\n");
    graph.write("pages/a___b___c.md", "- two levels down\n");
    graph.write(
        "pages/ab___c.md",
        "- a different namespace, not a child of a\n",
    );
    let mut opened = graph.open();

    // Only one level answers for the children read: `a/b/c` is a grandchild, and `ab/c` shares no
    // namespace with `a` — the separator must be right after the name, not later in it.
    let children = opened
        .pages_from_namespace("a")
        .expect("query the children of a");
    assert_eq!(children.value(), &["a/b".to_owned()]);

    // The tree reports every depth, still ordered by folded name.
    let tree = opened
        .pages_tree_from_namespace("a")
        .expect("query the subtree of a");
    assert_eq!(
        tree.value(),
        &["a/b".to_owned(), "a/b/c".to_owned()],
        "the whole subtree answers, in page-list order"
    );

    // The namespace page itself is neither a child nor part of the subtree: it does not sit under
    // its own name.
    assert!(!children.value().contains(&"a".to_owned()));
    assert!(!tree.value().contains(&"a".to_owned()));

    // A namespace nothing extends answers empty, and so does an empty namespace: no page name
    // starts with a slash. Neither is an error.
    let missing = opened
        .pages_from_namespace("missing")
        .expect("query an unknown namespace");
    assert!(missing.value().is_empty());
    let empty = opened
        .pages_tree_from_namespace("")
        .expect("query the empty namespace");
    assert!(empty.value().is_empty());
}

#[test]
fn namespace_reads_fold_case_like_every_page_identity() {
    let graph = TempGraph::new("namespace-folding");
    graph.write("pages/Ä.md", "- the namespace page\n");
    graph.write("pages/Ä___Kind.md", "- a child stored with a capital\n");
    graph.write("pages/ä___andere.md", "- another child\n");
    let mut opened = graph.open();

    // The namespace is matched through `page_name_key`, the rule `[[ref]]` targets use, so `ä`
    // finds the pages stored under `Ä/…`, and what comes back keeps the stored spelling.
    let children = opened
        .pages_from_namespace("ä")
        .expect("query the children of ä");
    assert_eq!(
        children.value(),
        &["ä/andere".to_owned(), "Ä/Kind".to_owned()],
        "ordered by folded name, spelled as stored"
    );

    let tree = opened
        .pages_tree_from_namespace("Ä")
        .expect("query the subtree of Ä");
    assert_eq!(
        tree.value(),
        children.value(),
        "either spelling asks the same question"
    );
}
