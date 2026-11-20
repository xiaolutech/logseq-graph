//! Sibling range removal through the guarded write facade.

use logseq_graph::document::EditError;
use logseq_graph::domain::{BlockId, BlockSelector};
use logseq_graph::engine::WriteError;

mod support;
use support::{bound, TempGraph};

const A: &str = "01234567-89ab-cdef-0123-456789abcdef";
const B: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn durable(id: &str) -> BlockSelector {
    BlockSelector::Durable(BlockId::parse(id).expect("UUID"))
}

#[test]
fn ranges_with_either_identity_commit_one_revision_and_keep_other_pages() {
    for (first_durable, last_durable) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let fixture = TempGraph::new("range-identities");
        let first_id = if first_durable {
            format!("  id:: {A}\n")
        } else {
            String::new()
        };
        let last_id = if last_durable {
            format!("  id:: {B}\n")
        } else {
            String::new()
        };
        fixture.write("pages/alpha.md", format!("- keep\n- first\n{first_id}  - child\n- middle\n- last\n{last_id}  - child\n- tail\n"));
        fixture.write("pages/beta.md", "- untouched\n");
        let mut graph = fixture.open();
        let first = if first_durable {
            durable(A)
        } else {
            bound(&fixture, "alpha", &[1])
        };
        let last = if last_durable {
            durable(B)
        } else {
            bound(&fixture, "alpha", &[4])
        };
        let revision = graph.metadata().expect("metadata").into_value().revision();
        let result = graph.remove_block_range(&first, &last).expect("range");
        assert_eq!(result.value().block_count(), 2);
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision + 1
        );
        assert_eq!(
            std::fs::read_to_string(fixture.graph_root().join("pages/alpha.md")).expect("bytes"),
            "- keep\n- tail\n"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.graph_root().join("pages/beta.md")).expect("bytes"),
            "- untouched\n"
        );
    }
}

#[test]
fn nested_siblings_and_equal_endpoints_remove_whole_subtrees() {
    for same in [false, true] {
        let fixture = TempGraph::new("range-nested");
        fixture.write(
            "pages/alpha.md",
            "- parent\n  - first\n    - child\n  - last\n- tail\n",
        );
        let mut graph = fixture.open();
        let first = bound(&fixture, "alpha", &[0, 0]);
        let last = if same {
            first.clone()
        } else {
            bound(&fixture, "alpha", &[0, 1])
        };
        graph
            .remove_block_range(&first, &last)
            .expect("nested range");
        assert_eq!(
            std::fs::read_to_string(fixture.graph_root().join("pages/alpha.md")).expect("bytes"),
            if same {
                "- parent\n  - last\n- tail\n"
            } else {
                "- parent\n- tail\n"
            }
        );
    }
}

#[test]
fn refused_ranges_leave_both_files_and_revision_unchanged() {
    for case in 0..6 {
        let fixture = TempGraph::new("range-refusals");
        fixture.write("pages/alpha.md", "- parent\n  - child\n- last\n");
        fixture.write("pages/beta.md", "- other\n");
        if case == 5 {
            fixture.write(
                "pages/alpha.md",
                format!("- parent\n  id:: {A}\n  - child\n- last\n"),
            );
        }
        let mut graph = fixture.open();
        let parent = bound(&fixture, "alpha", &[0]);
        let child = bound(&fixture, "alpha", &[0, 0]);
        let last = bound(&fixture, "alpha", &[2]);
        let (first, end) = match case {
            0 => (parent, child),
            1 => (child, last),
            2 => (last, parent),
            3 => (parent, bound(&fixture, "beta", &[0])),
            4 => (parent, last),
            _ => (durable(A), last),
        };
        if case >= 4 {
            fixture.write(
                "pages/alpha.md",
                "- external\n- parent\n  - child\n- last\n",
            );
        }
        let before = std::fs::read(fixture.graph_root().join("pages/alpha.md")).expect("bytes");
        let revision = graph.metadata().expect("metadata").into_value().revision();
        let error = graph.remove_block_range(&first, &end).expect_err("refused");
        match case {
            0 | 1 => assert!(matches!(
                error,
                WriteError::Edit(EditError::RangeNotSiblings)
            )),
            2 => assert!(matches!(error, WriteError::Edit(EditError::RangeReversed))),
            3 => assert!(matches!(error, WriteError::RangeAcrossPages)),
            _ => assert!(matches!(error, WriteError::StaleVersion { .. })),
        }
        assert_eq!(
            std::fs::read(fixture.graph_root().join("pages/alpha.md")).expect("bytes"),
            before
        );
        assert_eq!(
            std::fs::read(fixture.graph_root().join("pages/beta.md")).expect("bytes"),
            b"- other\n"
        );
        assert_eq!(
            graph.metadata().expect("metadata").into_value().revision(),
            revision
        );
    }
}
