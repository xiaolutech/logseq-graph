//! Startup scan tests over temporary graph roots and on-disk indexes.
//!
//! Every test builds its own graph and its own index file under the system temporary directory, so
//! nothing depends on a checked-out real graph and no test shares state with another one.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use logseq_graph::domain::PageKind;
use logseq_graph::engine::{EngineError, Graph, OpenOptions, ScanProgress};
use logseq_graph::index::{Index, IndexError};
use logseq_graph::source::SourceError;

mod support;

use support::{block_count, TempGraph};

fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3_600)
}

#[test]
fn startup_projects_every_discovered_file_and_answers_reads_immediately() {
    let graph = TempGraph::new("startup");
    graph.write("pages/alpha.md", "- a\n\t- a1\n\t- a2\n");
    graph.write("pages/beta.md", "- b\n");

    let mut opened = graph.open();

    // Read-your-writes: the queries below run immediately after the traversal, with nothing
    // refreshed in between.
    assert_eq!(block_count(&mut opened, "alpha"), 3);
    assert_eq!(block_count(&mut opened, "beta"), 1);

    let metadata = opened.metadata().expect("read metadata").into_value();
    assert_eq!(metadata.revision(), 2, "one revision per projected page");
    assert_eq!(metadata.instance_id().len(), 32);
    assert!(metadata
        .instance_id()
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    assert!(
        graph.index_path().is_file(),
        "opening the graph must create the index file, including its directory"
    );

    let alpha = opened
        .page_tree("alpha")
        .expect("query alpha")
        .into_value()
        .expect("alpha is projected");
    assert_eq!(alpha.page().name(), "alpha");
    assert_eq!(alpha.page().revision(), 1);
    assert_eq!(alpha.blocks().len(), 1);
    assert_eq!(alpha.blocks()[0].children().len(), 2);

    assert!(opened
        .page_metadata("missing")
        .expect("query missing page")
        .present()
        .is_none());
}

#[test]
fn page_metadata_states_which_kind_of_page_it_is() {
    let graph = TempGraph::new("page-kind");
    graph.write("pages/alpha.md", "- a\n");
    graph.write("journals/2026_08_13.md", "- entry\n");

    let mut opened = graph.open();

    let page = opened
        .page_metadata("alpha")
        .expect("query alpha")
        .into_value()
        .expect("alpha is projected");
    assert_eq!(
        page.kind(),
        PageKind::Page,
        "a file under pages/ is a page, and the metadata says so without a second read"
    );

    let journal = opened
        .page_metadata("Aug 13th, 2026")
        .expect("query the journal by its date title")
        .into_value()
        .expect("the journal is projected");
    assert_eq!(
        journal.kind(),
        PageKind::Journal,
        "the same read states a journal as a journal"
    );
}

#[test]
fn reopening_an_unchanged_graph_projects_nothing() {
    let graph = TempGraph::new("unchanged");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");

    let mut first = graph.open();
    let instance = first
        .metadata()
        .expect("read metadata")
        .value()
        .instance_id()
        .to_owned();
    let revision = first.metadata().expect("read metadata").value().revision();
    let page_revision = first
        .page_metadata("alpha")
        .expect("query alpha")
        .present()
        .expect("alpha is projected")
        .revision();
    drop(first);

    let mut second = graph.open();
    let reopened = second.metadata().expect("read metadata").into_value();
    assert_eq!(reopened.instance_id(), instance, "the cache is reused");
    assert_eq!(reopened.revision(), revision, "no file changed");
    assert_eq!(
        second
            .page_metadata("alpha")
            .expect("query alpha")
            .present()
            .expect("alpha is projected")
            .revision(),
        page_revision
    );
    assert_eq!(block_count(&mut second, "alpha"), 2);
}

#[test]
fn a_touched_file_with_unchanged_content_keeps_the_revision() {
    let graph = TempGraph::new("touched");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");

    let mut first = graph.open();
    let revision = first.metadata().expect("read metadata").value().revision();
    let page_revision = first
        .page_metadata("alpha")
        .expect("query alpha")
        .present()
        .expect("alpha is projected")
        .revision();
    drop(first);

    // Same bytes, new modification time: a save that changed nothing, or a `git checkout`.
    let touched = later();
    graph.set_modified("pages/alpha.md", touched);

    let mut second = graph.open();
    assert_eq!(
        second.metadata().expect("read metadata").value().revision(),
        revision,
        "unchanged content must not advance the revision"
    );
    assert_eq!(
        second
            .page_metadata("alpha")
            .expect("query alpha")
            .present()
            .expect("alpha is projected")
            .revision(),
        page_revision
    );
    assert_eq!(block_count(&mut second, "alpha"), 2);

    // The baseline moved with the file, so the next start compares against the new observation
    // instead of re-reading the file forever.
    let index = Index::open_at(graph.index_path()).expect("open index directly");
    let baseline = index
        .file_baseline(Path::new("pages/alpha.md"))
        .expect("read baseline")
        .expect("alpha has a baseline");
    assert_eq!(baseline.metadata().modified(), Some(touched));
}

#[test]
fn changed_content_is_reprojected_and_advances_the_revision_once() {
    let graph = TempGraph::new("changed");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");
    graph.write("pages/beta.md", "- b\n");

    let mut first = graph.open();
    let revision = first.metadata().expect("read metadata").value().revision();
    drop(first);

    graph.write("pages/alpha.md", "- a\n\t- a1\n\t- a2\n- a3\n");
    graph.set_modified("pages/alpha.md", later());

    let mut second = graph.open();
    assert_eq!(
        second.metadata().expect("read metadata").value().revision(),
        revision + 1,
        "exactly the changed page is projected again"
    );
    assert_eq!(block_count(&mut second, "alpha"), 4);
    assert_eq!(
        second
            .page_metadata("beta")
            .expect("query beta")
            .present()
            .expect("beta is projected")
            .revision(),
        2,
        "an untouched page keeps the revision that produced its rows"
    );
}

#[test]
fn a_file_added_after_startup_is_projected_on_the_next_start() {
    let graph = TempGraph::new("added");
    graph.write("pages/alpha.md", "- a\n");

    let mut first = graph.open();
    let revision = first.metadata().expect("read metadata").value().revision();
    assert!(first
        .page_tree("gamma")
        .expect("query gamma")
        .present()
        .is_none());
    drop(first);

    graph.write("pages/gamma.md", "- g\n\t- g1\n");

    let mut second = graph.open();
    assert_eq!(
        second.metadata().expect("read metadata").value().revision(),
        revision + 1
    );
    assert_eq!(block_count(&mut second, "gamma"), 2);
}

#[test]
fn an_incompatible_database_is_discarded_and_rebuilt_from_the_files() {
    let graph = TempGraph::new("incompatible");
    graph.write("pages/alpha.md", "- a\n\t- a1\n");

    let mut first = graph.open();
    let instance = first
        .metadata()
        .expect("read metadata")
        .value()
        .instance_id()
        .to_owned();
    assert_eq!(
        first.metadata().expect("read metadata").value().revision(),
        1
    );
    drop(first);

    // Rewrite the stored version marker and add a page that only exists in the database, so a
    // migration would keep it and a rebuild cannot.
    {
        let conn = rusqlite::Connection::open(
            Index::open_at(graph.index_path())
                .expect("resolve active index")
                .storage_path()
                .expect("file-backed index"),
        )
        .expect("open index file");
        conn.execute("UPDATE graph SET parser_version = parser_version - 1", [])
            .expect("downgrade the stored parser version");
        conn.execute(
            "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
             VALUES ('pages/ghost.md', 0, NULL, 0)",
            [],
        )
        .expect("insert the baseline the page row references");
        conn.execute(
            "INSERT INTO pages (name, name_key, kind, relative_path, block_count, revision)
             VALUES ('ghost', 'ghost', 'page', 'pages/ghost.md', 0, 99)",
            [],
        )
        .expect("insert a page that exists only in the index");
    }

    let mut rebuilt = graph.open();
    let metadata = rebuilt.metadata().expect("read metadata").into_value();
    assert_ne!(
        metadata.instance_id(),
        instance,
        "a rebuilt database is a new instance"
    );
    assert_eq!(
        metadata.revision(),
        1,
        "the rebuilt index holds exactly the projected files"
    );
    assert_eq!(block_count(&mut rebuilt, "alpha"), 2);
    assert!(
        rebuilt
            .page_tree("ghost")
            .expect("query ghost")
            .present()
            .is_none(),
        "the incompatible database must be deleted, not migrated"
    );
}

#[test]
fn an_old_schema_version_is_rebuilt_instead_of_reported() {
    let graph = TempGraph::new("old-schema");
    graph.write("pages/alpha.md", "- a\n");

    let first = graph.open();
    drop(first);

    {
        let conn = rusqlite::Connection::open(
            Index::open_at(graph.index_path())
                .expect("resolve active index")
                .storage_path()
                .expect("file-backed index"),
        )
        .expect("open index file");
        conn.execute("UPDATE graph SET schema_version = 2", [])
            .expect("downgrade the stored schema version");
    }

    let mut rebuilt = graph.open();
    assert_eq!(
        rebuilt
            .metadata()
            .expect("read metadata")
            .value()
            .revision(),
        1
    );
    assert_eq!(block_count(&mut rebuilt, "alpha"), 1);
}

#[test]
fn an_index_file_of_another_application_is_refused_and_left_in_place() {
    const FOREIGN: i64 = 0x1234_5678;
    let graph = TempGraph::new("foreign-index");
    graph.write("pages/alpha.md", "- a\n");
    let index_path = graph.index_path();
    fs::create_dir_all(index_path.parent().expect("index file has a parent"))
        .expect("create index directory");
    {
        let foreign = rusqlite::Connection::open(&index_path).expect("open a foreign database");
        foreign
            .pragma_update(None, "application_id", FOREIGN)
            .expect("mark it as another application's");
    }

    match graph.open_error() {
        EngineError::Index(IndexError::NotAnIndex { path, found }) => {
            assert_eq!(path, index_path);
            assert_eq!(found, FOREIGN);
        }
        other => panic!("expected a foreign index file to be refused, got {other:?}"),
    }

    assert!(
        index_path.is_file(),
        "startup must never delete a database it does not own"
    );
}

#[test]
fn an_unreadable_file_fails_the_startup_instead_of_projecting_an_empty_page() {
    let graph = TempGraph::new("unreadable");
    graph.write("pages/alpha.md", b"- a\n\xff\xfe\n");

    match graph.open_error() {
        EngineError::Source(SourceError::NotUtf8 { path, .. }) => {
            assert!(
                path.ends_with("pages/alpha.md"),
                "the failure must name the file: {}",
                path.display()
            );
        }
        other => panic!("expected a UTF-8 failure, got {other:?}"),
    }
}

#[test]
fn a_scan_counts_its_files_from_zero_to_the_discovered_total() {
    // The projection has to last long enough for another thread to observe it, so this graph is large
    // enough that the scan is measurable work rather than an instant: a cold projection costs about
    // half a millisecond per file.
    let graph = TempGraph::new("progress");
    let files = 400usize;
    for index in 0..files {
        graph.write(&format!("pages/page {index}.md"), "- one block\n");
    }

    let progress = Arc::new(ScanProgress::new());
    assert_eq!(progress.snapshot(), (0, 0), "no scan has begun yet");

    let opened = {
        let progress = Arc::clone(&progress);
        let options = OpenOptions::new(graph.graph_root(), graph.index_path());
        std::thread::spawn(move || Graph::open_with_progress(options, progress))
    };

    // Poll while the scan runs, keeping one sample per distinct value. The scan holds the index, so
    // this thread may not read anything through it: the counter is the only thing it touches.
    let mut samples: Vec<(u64, u64)> = Vec::new();
    while !opened.is_finished() {
        let sample = progress.snapshot();
        if samples.last() != Some(&sample) {
            samples.push(sample);
        }
    }
    let _opened = opened
        .join()
        .expect("the opening thread did not panic")
        .expect("the graph opens");

    assert!(
        samples.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "the count only moves forward: {samples:?}"
    );
    assert!(
        samples
            .iter()
            .any(|&(scanned, total)| scanned > 0 && scanned < total),
        "the counter must be readable while the scan runs, not only after it returns: {samples:?}"
    );
    assert_eq!(
        progress.snapshot(),
        (files as u64, files as u64),
        "the scan reports every discovered file and then stops"
    );
}
