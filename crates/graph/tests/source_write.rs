//! Atomic write tests over temporary graph directories.
//!
//! Every test builds its own graph under the system temporary directory, so nothing depends on a
//! checked-out real graph or on absolute fixture paths.
//!
//! What is not testable here: that a crash leaves either the old or the new bytes. Reproducing a
//! process kill between fsync and rename needs a fault injection point, so the ordering itself is
//! documented in `source::write` rather than asserted.

use std::error::Error as _;
use std::fs;
use std::path::Path;

mod support;

use logseq_graph::source::SourceError;
use support::TempGraph;

/// Name prefix of every temporary file the write path creates.
const TEMP_PREFIX: &str = ".logseq-graph-tmp-";

#[cfg(unix)]
#[test]
fn creation_refuses_symlinked_parents_outside_the_root() {
    let graph = TempGraph::new("create-symlink-root");
    let outside = TempGraph::new("create-symlink-outside");
    let source = graph.open_source();
    std::os::unix::fs::symlink(outside.graph_root(), graph.graph_root().join("pages"))
        .expect("symlink pages");
    for relative in ["pages/Escaped.md", "pages/missing/Escaped.md"] {
        let error = source
            .create_text(Path::new(relative), "- escaped")
            .expect_err("outside parent");
        assert!(
            matches!(error, SourceError::Io { source, .. } if source.kind() == std::io::ErrorKind::InvalidInput)
        );
    }
    assert_eq!(
        fs::read_dir(outside.graph_root())
            .expect("outside directory")
            .count(),
        0
    );
    fs::remove_file(graph.graph_root().join("pages")).expect("remove symlink");
    source
        .create_text(Path::new("pages/nested/Empty.md"), "")
        .expect("create missing parents inside root");
    assert_eq!(
        fs::read(graph.graph_root().join("pages/nested/Empty.md")).expect("empty created file"),
        b""
    );
    graph.make_dir("actual");
    std::os::unix::fs::symlink(
        graph.graph_root().join("actual"),
        graph.graph_root().join("alias"),
    )
    .expect("inside symlink");
    source
        .create_text(Path::new("alias/Page.md"), "- inside")
        .expect("inside parent");
    assert_eq!(
        fs::read(graph.graph_root().join("actual/Page.md")).expect("inside file"),
        b"- inside"
    );
}

/// Leftover temporary files in `relative`, for the residue assertions.
fn temp_residues(graph: &TempGraph, relative: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(graph.graph_root().join(relative))
        .expect("read directory")
        .filter_map(|entry| {
            let name = entry
                .expect("list entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            name.starts_with(TEMP_PREFIX).then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// The Unix mode of a file, for the permission-preservation assertions.
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path).expect("stat file").permissions().mode() & 0o777
}

#[test]
fn a_write_lands_the_text_byte_for_byte_including_bom_crlf_and_no_final_newline() {
    let graph = TempGraph::new("write-bytes");
    graph.make_dir("pages");
    let source = graph.open_source();

    // Every case is a byte sequence the document layer can already produce and must survive: a
    // byte order mark, CRLF line endings, and a missing final newline.
    let cases = [
        "\u{feff}- alpha\r\n\t- beta\r\n",
        "- alpha\n\t- beta",
        "- plain\n",
    ];
    for (index, text) in cases.iter().enumerate() {
        let path = graph.graph_root().join(format!("pages/case-{index}.md"));
        source.write_text(&path, text).expect("write the text");

        assert_eq!(fs::read(&path).expect("read raw bytes"), text.as_bytes());
        let page = source
            .discover()
            .expect("discover")
            .into_iter()
            .find(|page| page.relative_path() == Path::new(&format!("pages/case-{index}.md")))
            .expect("the written file is discovered");
        assert_eq!(page.read_text().expect("read the text back"), *text);
    }
}

#[test]
fn a_write_replaces_the_whole_file_without_leaving_the_old_tail() {
    let graph = TempGraph::new("write-replace");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/replace.md");

    graph.write("pages/replace.md", "- old\n- content\n- tail\n");
    source
        .write_text(&path, "- new\n")
        .expect("overwrite the file");

    assert_eq!(fs::read(&path).expect("read the replaced file"), b"- new\n");
}

#[test]
fn a_successful_write_leaves_no_temporary_file_behind() {
    let graph = TempGraph::new("write-clean");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/clean.md");

    graph.write("pages/clean.md", "- old\n");
    source.write_text(&path, "- new\n").expect("write");

    assert!(
        temp_residues(&graph, "pages").is_empty(),
        "a successful write must remove its temporary file"
    );
}

#[test]
fn a_failed_write_keeps_the_previous_bytes_and_leaves_no_temporary_file() {
    let graph = TempGraph::new("write-failure");
    let source = graph.open_source();

    graph.write("pages/blocked.md", "- untouched\n");

    // The target is an existing directory: the temporary file is written and synced, then the
    // rename cannot replace it. The cleanup must still run.
    let target = graph.graph_root().join("pages");
    let error = source
        .write_text(&target, "- new\n")
        .expect_err("writing over a directory must fail");

    let message = error.to_string();
    assert!(
        message.contains(&target.display().to_string()),
        "the error must name the path: {message}"
    );
    assert!(
        error.source().is_some(),
        "the error must carry the underlying cause: {message}"
    );
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("pages/blocked.md")).expect("read original"),
        "- untouched\n",
        "the failed write must not touch the original file"
    );
    assert!(
        temp_residues(&graph, "pages").is_empty(),
        "a failed write must remove its temporary file"
    );
}

#[test]
#[cfg(unix)]
fn a_write_that_cannot_create_its_temporary_file_changes_nothing() {
    use std::os::unix::fs::PermissionsExt as _;

    let graph = TempGraph::new("write-no-temp");
    let source = graph.open_source();
    graph.write("pages/locked.md", "- untouched\n");

    let dir = graph.graph_root().join("pages");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555))
        .expect("make the directory read-only");

    // A process running as root ignores the mode; skip there instead of asserting a failure that
    // cannot happen. The probe is removed and the directory restored before returning.
    let probe = dir.join("probe.md");
    if fs::write(&probe, "- probe\n").is_ok() {
        let _ = fs::remove_file(&probe);
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
        eprintln!("skipping: the process can write to a read-only directory");
        return;
    }

    let target = dir.join("locked.md");
    let error = source
        .write_text(&target, "- new\n")
        .expect_err("creating the temporary file must fail");
    assert!(
        error.source().is_some(),
        "the error must carry the cause: {error}"
    );

    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
    assert_eq!(
        fs::read_to_string(&target).expect("read original"),
        "- untouched\n",
        "the failed write must not touch the original file"
    );
    assert!(
        temp_residues(&graph, "pages").is_empty(),
        "a failed write must remove its temporary file"
    );
}

#[test]
fn a_failure_before_the_rename_stays_unwritten() {
    let graph = TempGraph::new("write-before-rename");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/missing/target.md");

    let error = source
        .write_text(&path, "- new\n")
        .expect_err("the parent directory does not exist");

    assert!(
        matches!(error, SourceError::Io { .. }),
        "a failure before the rename must not claim the file changed: {error:?}"
    );
    assert!(
        !path.exists(),
        "a failed replacement must not create the target"
    );
}

#[test]
fn a_leftover_temporary_file_is_never_mistaken_for_the_target() {
    let graph = TempGraph::new("write-leftover");
    let source = graph.open_source();
    // A name shaped like the ones the write path produces, as a crashed process would leave it.
    graph.write(
        "pages/.logseq-graph-tmp-0-0-0",
        "- half a page that was never committed\n",
    );
    let path = graph.graph_root().join("pages/page.md");

    source.write_text(&path, "- new\n").expect("write");

    assert_eq!(
        fs::read_to_string(&path).expect("read the target"),
        "- new\n",
        "the write must not reuse the leftover as the target file"
    );
    assert_eq!(
        fs::read_to_string(graph.graph_root().join("pages/.logseq-graph-tmp-0-0-0"))
            .expect("read the leftover"),
        "- half a page that was never committed\n",
        "the write must not touch a leftover it did not create"
    );
    let discovered: Vec<_> = source
        .discover()
        .expect("discover")
        .into_iter()
        .map(|page| page.relative_path().to_owned())
        .collect();
    assert_eq!(
        discovered,
        vec![std::path::PathBuf::from("pages/page.md")],
        "a leftover temporary file must not be discovered as a page"
    );
}

#[test]
fn a_write_refuses_paths_outside_the_graph_root_and_relative_paths() {
    let graph = TempGraph::new("write-scope");
    let source = graph.open_source();

    let outside = graph.root().join("outside.md");
    let error = source
        .write_text(&outside, "- new\n")
        .expect_err("writing outside the root must fail");
    assert!(
        error.to_string().contains("outside the graph root"),
        "the error must say the path is outside: {error}"
    );
    assert!(
        !outside.exists(),
        "a refused write must not create the file"
    );

    let error = source
        .write_text(Path::new("pages/relative.md"), "- new\n")
        .expect_err("a relative path must be refused");
    assert!(
        error.to_string().contains("absolute"),
        "the error must say the path must be absolute: {error}"
    );
}

#[test]
fn a_write_into_a_missing_directory_fails_and_cleans_up() {
    let graph = TempGraph::new("write-missing-dir");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/missing/sub.md");

    let error = source
        .write_text(&path, "- new\n")
        .expect_err("the parent directory does not exist");
    assert!(
        error.source().is_some(),
        "the error must carry the underlying cause: {error}"
    );
    assert!(
        !graph.graph_root().join("pages/missing").exists(),
        "a failed write must not create missing directories"
    );
}

#[test]
#[cfg(unix)]
fn an_overwrite_keeps_the_existing_file_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let graph = TempGraph::new("write-mode");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/mode.md");

    graph.write("pages/mode.md", "- old\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("set mode");

    source.write_text(&path, "- new\n").expect("overwrite");

    assert_eq!(
        mode(&path),
        0o600,
        "replacing a file must keep its permission bits"
    );
}

#[test]
#[cfg(unix)]
fn a_new_file_gets_the_default_mode_shaped_by_umask() {
    let graph = TempGraph::new("write-new-mode");
    graph.make_dir("pages");
    let source = graph.open_source();
    let path = graph.graph_root().join("pages/fresh.md");

    source.write_text(&path, "- new\n").expect("create");

    // The exact mode depends on the process umask, so assert the shape rather than a fixed value:
    // the owner can still write the file and it is not executable.
    let file_mode = mode(&path);
    assert_eq!(
        file_mode & 0o100,
        0,
        "a written page must not be executable"
    );
    assert_ne!(file_mode & 0o200, 0, "the owner must keep write access");
}
