use crate::index::test_support::*;
use crate::index::Index;

#[test]
fn a_blocks_task_facts_are_stored_from_the_same_text_as_the_rest_of_the_row() {
    let mut index = Index::open_in_memory().expect("open index");
    project(
        &mut index,
        "tasks",
        "- TODO [#A] buy milk\n- plain\n- DONE finished\n- [#C] no marker\n- TODO buy [#B] milk\n",
    )
    .expect("replace page");

    // The two columns read the text of the same parse that filled `blocks.text`: a marker word
    // is stored as the marker, a `[#A]`-shaped group only when it sits where a priority belongs,
    // and a block that states neither stores NULL rather than a word. A `[#B]` in the
    // description is description.
    assert_eq!(
        fact_rows(&index, TASK_FACTS, "tasks"),
        ["0|TODO|A", "1|-|-", "2|DONE|-", "3|-|C", "4|TODO|-"]
    );
}

#[test]
fn replacing_a_page_updates_task_facts_and_reuses_ids_only_for_identical_bytes() {
    let mut index = Index::open_in_memory().expect("open index");
    const BEFORE: &str = "- TODO [#A] buy milk\n- DOING [#C] write it\n";
    const AFTER: &str = "- DONE [#B] buy milk\n- plain\n";
    let stored_ids = |index: &Index| -> Vec<String> {
        all_rows(
            index,
            "SELECT blocks.ephemeral_id
               FROM blocks
               JOIN pages ON pages.id = blocks.page_id
              WHERE pages.name = 'tasks'
              ORDER BY blocks.preorder",
        )
    };

    project(&mut index, "tasks", BEFORE).expect("first projection");
    let first_ids = stored_ids(&index);
    assert_eq!(
        fact_rows(&index, TASK_FACTS, "tasks"),
        ["0|TODO|A", "1|DOING|C"]
    );

    // The same bytes are the same blocks, so the ephemeral ids are reused and the task facts
    // are written again unchanged: a re-projection that changes nothing expires no selector.
    project(&mut index, "tasks", BEFORE).expect("second projection");
    assert_eq!(stored_ids(&index), first_ids);
    assert_eq!(
        fact_rows(&index, TASK_FACTS, "tasks"),
        ["0|TODO|A", "1|DOING|C"]
    );

    // Different bytes replace the rows and therefore mint new ids, while both columns follow the
    // new text: the facts and the identity move with the file, not with the previous row.
    project(&mut index, "tasks", AFTER).expect("third projection");
    let replaced_ids = stored_ids(&index);
    assert!(
        replaced_ids.iter().all(|id| !first_ids.contains(id)),
        "a changed page mints new ephemeral ids: {replaced_ids:?}"
    );
    assert_eq!(
        fact_rows(&index, TASK_FACTS, "tasks"),
        ["0|DONE|B", "1|-|-"]
    );
}
