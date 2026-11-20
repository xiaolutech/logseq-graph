//! End-to-end tests of the `logseq-graph` binary.
//!
//! Every case writes a real graph into a temporary directory and runs the built binary as a child
//! process, so it covers argument parsing, the cache policy, the engine, and stdout/stderr shaping in
//! one path. Nothing here reaches into the library's internals: the assertions are about the text a
//! person sees and the exit code a script branches on.
//!
//! The graph root and the cache root both live inside the sandbox, and `HOME`/`LOGSEQ_GRAPH` are
//! cleared or redirected, so a test can never read or write the graph of the machine it runs on.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// The binary cargo built for this test run.
const BIN: &str = env!("CARGO_BIN_EXE_logseq-graph");

/// A temporary sandbox holding one graph root, one cache root, and one home directory.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A sandbox with an empty graph: `pages/`, `journals/`, and `logseq/config.edn`.
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "logseq-graph-cli-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sandbox = Self { root };
        for directory in [
            "graph/pages",
            "graph/journals",
            "graph/logseq",
            "cache",
            "home",
        ] {
            fs::create_dir_all(sandbox.root.join(directory)).expect("create sandbox directory");
        }
        sandbox.write("logseq/config.edn", "{}\n");
        sandbox
    }

    /// The graph root, which is what `--graph` takes.
    fn graph_root(&self) -> PathBuf {
        self.root.join("graph")
    }

    /// Writes one file inside the graph root, creating the parents its path names.
    fn write(&self, relative: &str, contents: &str) {
        let path = self.graph_root().join(relative);
        fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent");
        fs::write(path, contents).expect("write graph file");
    }

    /// Where the binary puts an index it was not told about.
    fn cache_root(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// A path the test passes with `--index`.
    fn explicit_index(&self) -> PathBuf {
        self.root.join("explicit").join("index.db")
    }

    /// Runs the binary with the graph root this sandbox owns.
    fn run(&self, args: &[&str]) -> Output {
        let graph_root = self.graph_root();
        let mut all = vec!["--graph", graph_root.to_str().expect("utf-8 path")];
        all.extend_from_slice(args);
        self.run_raw(&all)
    }

    /// Runs the binary with exactly the arguments given.
    fn run_raw(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("XDG_CACHE_HOME", self.cache_root())
            .env("HOME", self.root.join("home"))
            .env_remove("LOGSEQ_GRAPH")
            .output()
            .expect("run the binary")
    }

    /// Every file under the graph root, as sorted graph-relative paths.
    fn graph_files(&self) -> Vec<String> {
        let mut files = Vec::new();
        collect(&self.graph_root(), &self.graph_root(), &mut files);
        files.sort();
        files
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Collect graph content files, excluding the persistent root-level coordination lock.
fn collect(base: &Path, directory: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("list a sandbox directory") {
        let entry = entry.expect("read a directory entry");
        let path = entry.path();
        if path == base.join("write.lock") {
            continue;
        }
        if path.is_dir() {
            collect(base, &path, out);
        } else {
            out.push(
                path.strip_prefix(base)
                    .expect("entry is under the base")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

/// Standard output of a run that must succeed.
fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "expected success, got {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("stdout is utf-8")
}

/// Standard error of a run that must fail with `code`.
fn stderr_of(output: &Output, code: i32) {
    assert_eq!(output.status.code(), Some(code), "wrong exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.trim().is_empty(), "a failure states why");
}

/// The lines of an `info` answer that describe the graph rather than the run.
///
/// The index file's size and the observation time change between two runs of the same graph, and
/// neither is part of what the freshness flags decide, so both are dropped before comparing.
fn stable_lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.starts_with("index-size: ") && !line.starts_with("observed: "))
        .map(str::to_owned)
        .collect()
}

/// The block count one `page` or `append` answer states.
fn block_count(stdout: &str) -> u32 {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("blocks: "))
        .expect("the answer states a block count")
        .parse()
        .expect("a block count is a number")
}

/// A graph with three pages, one journal, a nested block, a durable id, and a tag.
fn populated(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.write(
        "pages/alpha.md",
        "- alpha root\n\t- alpha child\n\t\t- alpha grandchild\n",
    );
    sandbox.write(
        "pages/Bravo.md",
        "- bravo sees [[alpha]]\n\
         - bravo tagged #project\n\
         \t- stable block\n\
         \t  id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n",
    );
    sandbox.write("journals/2026_08_13.md", "- journal entry [[alpha]]\n");
    sandbox
}

#[test]
fn info_reports_the_graph_it_indexed() {
    let sandbox = populated("info");

    let stdout = stdout_of(&sandbox.run(&["info"]));

    let canonical_root = fs::canonicalize(sandbox.graph_root()).expect("the graph root exists");
    assert!(
        stdout.contains(&format!("graph: {}", canonical_root.display())),
        "the canonical graph root is printed: {stdout}"
    );
    assert!(
        stdout.contains(&format!("index: {}", sandbox.cache_root().display())),
        "the derived index lives under the cache root: {stdout}"
    );
    // Three pages: alpha, Bravo, and the journal, which is a page in the derived model. Six blocks:
    // three on alpha, three on Bravo, one on the journal.
    assert!(stdout.contains("\npages: 3\n"), "pages: {stdout}");
    assert!(stdout.contains("\nblocks: 7\n"), "blocks: {stdout}");
    assert!(
        stdout.contains("\nrevision: 3\n"),
        "the open scans three files, one revision each: {stdout}"
    );
    assert!(stdout.contains("\nstale: false\n"), "stale: {stdout}");
    assert!(
        stdout.contains("\nobserved: 20"),
        "the observation time is a UTC RFC3339 timestamp: {stdout}"
    );
    let size = stdout
        .lines()
        .find_map(|line| line.strip_prefix("index-size: "))
        .expect("the index size is printed")
        .parse::<u64>()
        .expect("the index size is a number");
    assert!(size > 0, "the index file has content");
}

#[test]
fn pages_lists_every_page_in_name_order() {
    let sandbox = populated("pages");

    let stdout = stdout_of(&sandbox.run(&["pages"]));

    // Folded-name order, so `Aug 13th, 2026` sorts between `alpha` and `Bravo`, and each line is the
    // stored spelling with that page's own block count.
    assert_eq!(
        stdout, "alpha  3\nAug 13th, 2026  1\nBravo  3\nproject  0\n",
        "one line per page: name, two spaces, block count"
    );
}

#[test]
fn pages_without_the_date_flag_prints_the_two_columns_it_always_did() {
    let sandbox = populated("pages-two-columns");

    // One page, one journal, one page whose name sorts after both: every line is two fields and a
    // block count, byte for byte, so a downstream `cut -f2` or `awk` keeps reading what it read
    // before the third column existed.
    let stdout = stdout_of(&sandbox.run(&["pages"]));

    assert_eq!(
        stdout,
        "alpha  3\nAug 13th, 2026  1\nBravo  3\nproject  0\n"
    );
    assert!(
        stdout.lines().all(|line| line.split("  ").count() == 2),
        "no line carries a third column: {stdout}"
    );
}

#[test]
fn pages_with_the_date_flag_states_a_journals_day_in_the_third_column() {
    let sandbox = populated("pages-date-journal");

    let stdout = stdout_of(&sandbox.run(&["pages", "--date"]));

    // The journal is named by its title, so the day it stands for is only readable in the third
    // column — `YYYY-MM-DD`, the spelling that sorts as time.
    assert!(
        stdout
            .lines()
            .any(|line| line == "Aug 13th, 2026  1  2026-08-13"),
        "the journal's day is the third column: {stdout}"
    );
    assert_eq!(stdout.lines().count(), 4, "one line per page: {stdout}");
}

#[test]
fn pages_with_the_date_flag_prints_a_dash_for_a_page_that_is_not_a_day() {
    let sandbox = populated("pages-date-dash");

    let stdout = stdout_of(&sandbox.run(&["pages", "--date"]));

    // A regular page has no day. It prints one dash rather than nothing, so every line has three
    // columns and a column count never depends on which pages the graph holds.
    assert!(
        stdout
            .lines()
            .any(|line| line == "alpha  3  -" || line == "Bravo  3  -"),
        "a page's third column is a dash: {stdout}"
    );
    assert!(
        stdout.lines().all(|line| line.split("  ").count() == 3),
        "the flag puts three columns on every line: {stdout}"
    );
}

#[test]
fn the_date_flag_belongs_to_pages_and_another_command_refuses_it() {
    let sandbox = populated("pages-date-refused");

    // `info` prints no listing, so it has no column to put a day in. Refusing beats answering while
    // dropping the flag, exactly as `--bind` is refused by every command that does not listen.
    let output = sandbox.run(&["--date", "info"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--date does not apply to info"), "{stderr}");
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
}

#[test]
fn tree_indents_one_level_by_two_spaces() {
    let sandbox = populated("tree");

    let stdout = stdout_of(&sandbox.run(&["tree", "alpha"]));

    assert_eq!(
        stdout, "alpha root\n  alpha child\n    alpha grandchild\n",
        "depth 0, 1, and 2 print with no indent, two spaces, and four"
    );
}

#[test]
fn tree_folds_a_block_property_line_onto_the_blocks_line() {
    let sandbox = populated("tree-property");

    let stdout = stdout_of(&sandbox.run(&["tree", "bravo"]));

    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("bravo sees [[alpha]]"));
    assert_eq!(lines.next(), Some("bravo tagged #project"));
    assert_eq!(
        lines.next(),
        Some("  stable block id:: 01234567-89AB-CDEF-0123-456789ABCDEF"),
        "a property line joins its block's line instead of breaking the record"
    );
    assert_eq!(lines.next(), None);
}

#[test]
fn backlinks_find_the_referencing_block() {
    let sandbox = populated("backlinks");

    let stdout = stdout_of(&sandbox.run(&["backlinks", "alpha"]));

    assert_eq!(
        stdout, "Aug 13th, 2026#0  journal entry [[alpha]]\nBravo#0  bravo sees [[alpha]]\n",
        "the page that references the target, its block's position, and that block's text"
    );

    // The page name is an identity, so the other spelling of the target answers the same question.
    let folded = stdout_of(&sandbox.run(&["backlinks", "ALPHA"]));
    assert_eq!(folded, stdout);
}

#[test]
fn tags_and_tag_agree() {
    let sandbox = populated("tags");

    let tags = stdout_of(&sandbox.run(&["tags"]));
    assert_eq!(tags, "project  1\n", "one tag and the pages that use it");

    let pages = stdout_of(&sandbox.run(&["tag", "project"]));
    assert_eq!(pages, "Bravo\n");

    // The tag name is matched by its folded key, so the spelling does not matter.
    assert_eq!(stdout_of(&sandbox.run(&["tag", "PROJECT"])), pages);
    assert_eq!(
        stdout_of(&sandbox.run(&["tag", "unused"])),
        "",
        "a tag nobody uses lists no page and still succeeds"
    );
}

#[test]
fn namespace_lists_direct_children_and_the_whole_subtree() {
    let sandbox = Sandbox::new("namespace");
    sandbox.write("pages/github.com___zhaochunqi___home-repo.md", "- root\n");
    sandbox.write(
        "pages/github.com___zhaochunqi___home-repo___AGENTS.md",
        "- instructions\n",
    );
    sandbox.write(
        "pages/github.com___zhaochunqi___home-repo___wiki___shared___repo-status-protocol.md",
        "- protocol\n",
    );
    sandbox.write(
        "pages/github.com___zhaochunqi___elsewhere.md",
        "- another namespace\n",
    );

    let direct = stdout_of(&sandbox.run(&["namespace", "github.com/zhaochunqi/home-repo"]));
    assert_eq!(
        direct,
        "github.com/zhaochunqi/home-repo/AGENTS\n\
         github.com/zhaochunqi/home-repo/wiki\n",
        "only the next level answers without --tree"
    );

    let tree = stdout_of(&sandbox.run(&["namespace", "github.com/zhaochunqi/home-repo", "--tree"]));
    assert_eq!(
        tree,
        "github.com/zhaochunqi/home-repo/AGENTS\n\
         github.com/zhaochunqi/home-repo/wiki\n\
         github.com/zhaochunqi/home-repo/wiki/shared\n\
         github.com/zhaochunqi/home-repo/wiki/shared/repo-status-protocol\n",
        "--tree includes every deeper level of a multi-segment namespace"
    );

    assert_eq!(
        stdout_of(&sandbox.run(&["namespace", "missing"])),
        "",
        "an unknown namespace lists nothing and still succeeds"
    );

    stderr_of(&sandbox.run(&["pages", "--tree"]), 2);
}

#[test]
fn search_finds_the_block_and_limit_bounds_the_hits() {
    let sandbox = populated("search");

    // Every block whose text contains the query, the selector first — the same shape `query`
    // prints, so a hit pipes into `block` or `update` the way a query answer does. The ids are
    // minted per projection, so the shape is asserted per line: selector, two spaces,
    // `page#preorder`, two spaces, the block text. The order is the scan's, which this CLI does
    // not rank, so the hits are compared as a set.
    let all = stdout_of(&sandbox.run(&["search", "alpha"]));
    let mut hits: Vec<&str> = all.lines().collect();
    hits.sort_unstable_by_key(|line| line.split_once("  ").unwrap().1.to_owned());
    let bodies: Vec<&str> = hits
        .iter()
        .map(|line| line.split_once("  ").unwrap().1)
        .collect();
    assert_eq!(
        bodies,
        [
            "Aug 13th, 2026#0  journal entry [[alpha]]",
            "Bravo#0  bravo sees [[alpha]]",
            "alpha#0  alpha root",
            "alpha#1  alpha child",
            "alpha#2  alpha grandchild",
        ],
        "every matching block is reported once, behind its selector"
    );
    for line in &hits {
        let (selector, _) = line.split_once("  ").expect("selector and body");
        let (kind, id) = selector
            .split_once(':')
            .expect("a selector states its kind");
        assert_eq!(kind, "ephemeral", "no block here states an id::: {line}");
        assert_eq!(id.len(), 36, "a UUID: {line}");
    }

    let one = stdout_of(&sandbox.run(&["search", "alpha", "--limit", "1"]));
    assert_eq!(one.lines().count(), 1, "the limit bounds the printed hits");

    let single = stdout_of(&sandbox.run(&["search", "grandchild"]));
    let selector = single.split_once("  ").expect("a selector").0;
    assert!(
        single.ends_with("  alpha#2  alpha grandchild\n"),
        "the preorder locates the block inside its page: {single}"
    );

    // The printed selector is the spelling `block` accepts, so a hit is actionable on its own: the
    // answer names the same page, position, and text the hit did.
    let read = stdout_of(&sandbox.run(&["block", selector]));
    assert_eq!(
        read,
        format!(
            "page: alpha\npreorder: 2\ndepth: 2\ntext: alpha grandchild\nselector: {selector}\n"
        ),
        "the selector a search printed reads the same block back"
    );
}

#[test]
fn find_prints_page_names_ahead_of_aliases() {
    let sandbox = Sandbox::new("find");
    sandbox.write(
        "pages/ADR.md",
        "alias:: 架构决策记录, ADRs\n\n- a decision\n",
    );
    sandbox.write("pages/紫米.md", "alias:: zmi\n");
    sandbox.write("pages/酷态科.md", "alias:: 紫米, ZMI\n");

    // A shared alias prints one line per page, each stating the alias that answered. A caller
    // takes the first line as the jump or offers all of them; the CLI does not choose.
    let shared = stdout_of(&sandbox.run(&["find", "zmi"]));
    let lines: Vec<&str> = shared.lines().collect();
    assert_eq!(lines.len(), 2, "every page aliasing zmi answers: {shared}");
    assert!(
        lines.iter().all(|line| line.contains("  alias: ")),
        "an alias hit states its alias: {shared}"
    );

    // The page's own name is the first line, bare; the page that only aliases it follows.
    let own_name = stdout_of(&sandbox.run(&["find", "紫米"]));
    let lines: Vec<&str> = own_name.lines().collect();
    assert_eq!(lines.len(), 2, "{own_name}");
    assert_eq!(
        lines[0], "紫米",
        "a page name outranks an alias: {own_name}"
    );
    assert_eq!(lines[1], "酷态科  alias: 紫米", "{own_name}");

    let limited = stdout_of(&sandbox.run(&["find", "zmi", "--limit", "1"]));
    assert_eq!(
        limited.lines().count(),
        1,
        "the limit bounds the list: {limited}"
    );
}

#[test]
fn page_prints_metadata_and_properties() {
    let sandbox = populated("page");
    sandbox.write(
        "pages/props.md",
        "title:: Example\nauthor:: Ada\ntitle:: Second\n\n- a block\n",
    );

    let stdout = stdout_of(&sandbox.run(&["page", "Second"]));

    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("name: Second"));
    assert_eq!(lines.next(), Some("blocks: 1"));
    let revision = lines
        .next()
        .and_then(|line| line.strip_prefix("revision: "))
        .expect("the page states its revision")
        .parse::<u64>()
        .expect("a revision is a number");
    assert!(revision > 0, "a projected page has a committed revision");
    assert_eq!(lines.next(), Some("kind: page"));
    assert!(lines
        .next()
        .and_then(|line| line.strip_prefix("version: "))
        .is_some_and(|version| version.len() == 16));
    assert_eq!(
        lines.collect::<Vec<_>>(),
        ["title: Example", "author: Ada", "title: Second"],
        "one line per property line, in written order"
    );
}

#[test]
fn page_states_the_kind_of_page_it_is() {
    let sandbox = populated("page-kind");

    let journal = stdout_of(&sandbox.run(&["page", "Aug 13th, 2026"]));
    assert!(
        journal.contains("\nkind: journal\n"),
        "a journal is answered as a journal: {journal}"
    );

    let page = stdout_of(&sandbox.run(&["page", "alpha"]));
    assert!(
        page.contains("\nkind: page\n"),
        "a file under pages/ is answered as a page: {page}"
    );
}

#[test]
fn a_journal_states_its_date_and_a_page_states_no_date_line() {
    let sandbox = populated("page-date");

    // The day is the journal's identity, so the answer states it where the title cannot: `date:`
    // follows `kind:` and nothing else stands between the two.
    let journal = stdout_of(&sandbox.run(&["page", "Aug 13th, 2026"]));
    assert!(
        journal.contains("\nkind: journal\ndate: 2026-08-13\n"),
        "a journal states its day right after its kind: {journal}"
    );

    // A regular page is not a day, so it prints no `date:` line at all rather than an empty one:
    // an absent field and a field holding nothing are different answers.
    let page = stdout_of(&sandbox.run(&["page", "alpha"]));
    assert!(
        !page.contains("date:"),
        "a page has no day to state: {page}"
    );
}

#[test]
fn a_missing_page_exits_one_with_one_line() {
    let sandbox = populated("missing-page");

    let output = sandbox.run(&["page", "nope"]);

    stderr_of(&output, 1);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "page not found: nope\n"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");

    // `tree` answers about a page too, so it fails the same way.
    let tree = sandbox.run(&["tree", "nope"]);
    stderr_of(&tree, 1);
    assert!(String::from_utf8_lossy(&tree.stderr).contains("page not found: nope"));
}

#[test]
fn a_block_is_read_by_the_id_it_states() {
    let sandbox = populated("block");

    let stdout = stdout_of(&sandbox.run(&["block", "01234567-89ab-cdef-0123-456789abcdef"]));

    assert_eq!(
        stdout,
        "page: Bravo\npreorder: 2\ndepth: 1\n\
         text: stable block id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\
         selector: durable:01234567-89ab-cdef-0123-456789abcdef\n",
        "the block's page, position, depth, text, and the selector it answers to"
    );

    let missing = sandbox.run(&["block", "fedcba98-7654-3210-fedc-ba9876543210"]);
    stderr_of(&missing, 1);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("block not found"));
}

#[test]
fn append_writes_one_block_and_answers_the_new_page() {
    let sandbox = populated("append");
    let path = sandbox.graph_root().join("pages/alpha.md");
    let before = block_count(&stdout_of(&sandbox.run(&["page", "alpha"])));

    let stdout = stdout_of(&sandbox.run(&["append", "alpha", "appended line"]));

    assert_eq!(
        fs::read_to_string(&path).expect("the page file is readable"),
        "- alpha root\n\t- alpha child\n\t\t- alpha grandchild\n- appended line\n",
        "the new line is the page's last one, and the file keeps its own indentation"
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "the answer is the page's metadata, one field per line: {stdout}"
    );
    assert_eq!(lines[0], "name: alpha");
    assert_eq!(lines[1], format!("blocks: {}", before + 1));
    let revision: u64 = lines[2]
        .strip_prefix("revision: ")
        .expect("the answer states a revision")
        .parse()
        .expect("a revision is a number");
    assert!(revision > 0, "the write committed a revision");
    assert_eq!(lines[3], "kind: page", "a file under pages/ is a page");
}

#[test]
fn append_is_visible_to_a_later_tree() {
    let sandbox = populated("append-tree");

    stdout_of(&sandbox.run(&["append", "alpha", "seen by the next process"]));

    assert_eq!(
        stdout_of(&sandbox.run(&["tree", "alpha"])),
        "alpha root\n  alpha child\n    alpha grandchild\nseen by the next process\n",
        "a second process reads the block the first one appended, at depth 0"
    );
}

#[test]
fn append_to_a_missing_page_exits_one_and_creates_no_file() {
    let sandbox = populated("append-missing");
    let before = sandbox.graph_files();

    let output = sandbox.run(&["append", "nope", "a new block"]);

    stderr_of(&output, 1);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "page not found: nope\n",
        "a write reports a missing page the way the reads do"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");
    assert_eq!(
        sandbox.graph_files(),
        before,
        "a write never creates a page, so the graph gains no file"
    );
}

#[test]
fn append_of_multiline_text_exits_one_and_leaves_the_bytes_alone() {
    let sandbox = populated("append-multiline");
    let path = sandbox.graph_root().join("pages/alpha.md");
    let before = fs::read(&path).expect("the page file is readable");

    let output = sandbox.run(&["append", "alpha", "first line\nsecond line"]);

    stderr_of(&output, 1);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must fit on one source line"),
        "the refusal says what a block text may not contain"
    );
    assert_eq!(
        fs::read(&path).expect("the page file is readable"),
        before,
        "a refused edit leaves the file byte for byte as it was"
    );
}

#[test]
fn append_of_blank_text_exits_one() {
    let sandbox = populated("append-blank");
    let path = sandbox.graph_root().join("pages/alpha.md");
    let before = fs::read(&path).expect("the page file is readable");

    for text in ["", "   "] {
        let output = sandbox.run(&["append", "alpha", text]);

        stderr_of(&output, 1);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("empty or only whitespace"),
            "the refusal names the reason for {text:?}"
        );
    }
    assert_eq!(
        fs::read(&path).expect("the page file is readable"),
        before,
        "neither refused edit touched the file"
    );
}

#[test]
fn append_without_text_exits_two() {
    let sandbox = populated("append-no-text");

    let output = sandbox.run(&["append", "alpha"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("append needs block text"), "{stderr}");
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
}

#[test]
fn append_refuses_the_freshness_flags_instead_of_ignoring_them() {
    let sandbox = populated("append-freshness");
    let path = sandbox.graph_root().join("pages/alpha.md");
    let before = fs::read(&path).expect("the page file is readable");

    for flag in ["--verify-content", "--stale"] {
        let output = sandbox.run(&[flag, "append", "alpha", "a new block"]);

        stderr_of(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{flag} has no effect on append")),
            "{stderr}"
        );
        assert!(
            stderr.contains("reads every file"),
            "the refusal states why the flag cannot apply: {stderr}"
        );
    }
    assert_eq!(
        fs::read(&path).expect("the page file is readable"),
        before,
        "a refused command line writes nothing"
    );
}

/// The id `populated` states on Bravo's child block.
const DURABLE_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

/// The Bravo file `populated` writes, which is the one the durable id lives in.
const BRAVO: &str = "pages/Bravo.md";

#[test]
fn update_rewrites_one_block_and_answers_it_the_way_block_does() {
    let sandbox = populated("update");
    let path = sandbox.graph_root().join(BRAVO);

    let stdout = stdout_of(&sandbox.run(&["update", DURABLE_ID, "rewritten block"]));

    assert_eq!(
        stdout,
        "page: Bravo\npreorder: 2\ndepth: 1\n\
         text: rewritten block id:: 01234567-89AB-CDEF-0123-456789ABCDEF\n\
         selector: durable:01234567-89ab-cdef-0123-456789abcdef\n",
        "the write answers the block in the shape `block` prints"
    );

    let contents = fs::read_to_string(&path).expect("the page file is readable");
    assert!(contents.contains("rewritten block"), "{contents}");
    assert!(
        !contents.contains("stable block"),
        "exactly the addressed block was replaced: {contents}"
    );
    assert!(
        contents.contains("id:: 01234567-89AB-CDEF-0123-456789ABCDEF"),
        "the id a caller addressed the block by is not part of what it edits: {contents}"
    );

    assert_eq!(
        stdout_of(&sandbox.run(&["block", DURABLE_ID])),
        stdout,
        "the next process reads back what this one wrote"
    );
}

#[test]
fn update_of_an_unknown_id_exits_one_and_leaves_the_files_alone() {
    let sandbox = populated("update-missing");
    let id = "fedcba98-7654-3210-fedc-ba9876543210";
    let before = fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable");
    let files = sandbox.graph_files();

    let output = sandbox.run(&["update", id, "must not land"]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("block not found: {id}")),
        "a missing block is reported the way the `block` read reports it: {stderr}"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");
    assert_eq!(
        fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable"),
        before,
        "a write that resolves no block changes no file"
    );
    assert_eq!(sandbox.graph_files(), files, "and creates none");
}

#[test]
fn update_with_an_invalid_id_exits_two() {
    let sandbox = populated("update-invalid-id");

    let output = sandbox.run(&["update", "not-an-id", "some text"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("update needs a durable or ephemeral block selector, not \"not-an-id\""),
        "{stderr}"
    );
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
}

#[test]
fn update_refuses_the_freshness_flags_instead_of_ignoring_them() {
    let sandbox = populated("update-freshness");
    let before = fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable");

    for flag in ["--verify-content", "--stale"] {
        let output = sandbox.run(&[flag, "update", DURABLE_ID, "a newer text"]);

        stderr_of(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{flag} has no effect on update")),
            "{stderr}"
        );
    }
    assert_eq!(
        fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable"),
        before,
        "a refused command line writes nothing"
    );
}

#[test]
fn remove_deletes_the_block_and_answers_the_page() {
    let sandbox = populated("remove");
    let path = sandbox.graph_root().join(BRAVO);

    let stdout = stdout_of(&sandbox.run(&["remove", DURABLE_ID]));

    assert_eq!(
        fs::read_to_string(&path).expect("the page file is readable"),
        "- bravo sees [[alpha]]\n- bravo tagged #project\n",
        "the block's lines are gone and the siblings keep their bytes"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "the answer is the page's metadata, in the shape `append` prints: {stdout}"
    );
    assert_eq!(lines[0], "name: Bravo");
    assert_eq!(lines[1], "blocks: 2", "the page kept its two other blocks");
    let revision: u64 = lines[2]
        .strip_prefix("revision: ")
        .expect("the answer states a revision")
        .parse()
        .expect("a revision is a number");
    assert!(revision > 0, "the write committed a revision");
    assert_eq!(lines[3], "kind: page");

    assert_eq!(
        stdout_of(&sandbox.run(&["tree", "Bravo"])),
        "bravo sees [[alpha]]\nbravo tagged #project\n",
        "the next process reads a page without the deleted subtree"
    );
}

#[test]
fn remove_of_an_unknown_id_exits_one_and_leaves_the_files_alone() {
    let sandbox = populated("remove-missing");
    let id = "fedcba98-7654-3210-fedc-ba9876543210";
    let before = fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable");
    let files = sandbox.graph_files();

    let output = sandbox.run(&["remove", id]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("block not found: {id}")),
        "a missing block is reported the way the `block` read reports it: {stderr}"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");
    assert_eq!(
        fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable"),
        before,
        "a removal that resolves no block changes no file"
    );
    assert_eq!(sandbox.graph_files(), files, "and creates none");
}

#[test]
fn remove_with_an_invalid_id_exits_two() {
    let sandbox = populated("remove-invalid-id");
    let before = fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable");

    let output = sandbox.run(&["remove", "not-an-id"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("remove needs a durable or ephemeral block selector, not \"not-an-id\""),
        "{stderr}"
    );
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
    assert_eq!(
        fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable"),
        before,
        "a refused command line writes nothing"
    );
}

#[test]
fn remove_refuses_the_freshness_flags_instead_of_ignoring_them() {
    let sandbox = populated("remove-freshness");
    let before = fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable");

    for flag in ["--verify-content", "--stale"] {
        let output = sandbox.run(&[flag, "remove", DURABLE_ID]);

        stderr_of(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{flag} has no effect on remove")),
            "{stderr}"
        );
    }
    assert_eq!(
        fs::read(sandbox.graph_root().join(BRAVO)).expect("the page file is readable"),
        before,
        "a refused command line writes nothing"
    );
}

#[test]
fn a_command_line_without_a_graph_root_exits_two() {
    let sandbox = Sandbox::new("no-graph");

    let output = sandbox.run_raw(&["info"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LOGSEQ_GRAPH"), "{stderr}");
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
}

#[test]
fn the_graph_root_comes_from_the_environment_too() {
    let sandbox = populated("env-graph");

    let output = Command::new(BIN)
        .arg("info")
        .env("LOGSEQ_GRAPH", sandbox.graph_root())
        .env("XDG_CACHE_HOME", sandbox.cache_root())
        .env("HOME", sandbox.root.join("home"))
        .output()
        .expect("run the binary");

    assert!(stdout_of(&output).contains("\npages: 3\n"));
}

#[test]
fn an_unknown_command_exits_two() {
    let sandbox = populated("unknown-command");

    let output = sandbox.run(&["explode"]);

    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command: explode"), "{stderr}");
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");
}

#[test]
fn an_unknown_option_exits_two() {
    let sandbox = populated("unknown-option");

    let output = sandbox.run(&["--nope", "info"]);

    stderr_of(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option: --nope"));
}

#[test]
fn a_graph_that_does_not_exist_exits_one() {
    let sandbox = Sandbox::new("no-such-graph");

    let output = sandbox.run_raw(&["--graph", "/definitely/not/a/graph", "info"]);

    stderr_of(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("graph not found"));
}

#[test]
fn a_default_index_writes_nothing_inside_the_graph() {
    let sandbox = populated("cache-guard");
    let before = sandbox.graph_files();

    stdout_of(&sandbox.run(&["pages"]));
    stdout_of(&sandbox.run(&["tree", "alpha"]));

    assert_eq!(
        sandbox.graph_files(),
        before,
        "the index is cached outside the graph, because a real graph is a git repository"
    );
    assert!(
        sandbox.cache_root().join("logseq-graph").is_dir(),
        "the cache directory was created on demand"
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_non_git_graph_can_be_read_and_rebuilt_but_not_written() {
    use std::os::unix::fs::PermissionsExt as _;

    let sandbox = populated("read-only-graph");
    let root = sandbox.graph_root();
    let lock = root.join("write.lock");
    let before = sandbox.graph_files();
    let page = root.join("pages/alpha.md");
    let original = fs::read(&page).expect("original page");
    let permissions = fs::metadata(&root).expect("graph root").permissions();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).expect("make root read-only");
    match fs::File::create_new(&lock) {
        Ok(_) => {
            // A privileged test runner can ignore the directory's mode.
            fs::set_permissions(&root, permissions).expect("restore graph root mode");
            return;
        }
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied),
    }

    let read = sandbox.run(&["page", "alpha"]);
    let rescan = sandbox.run(&["--verify-content", "page", "alpha"]);
    fs::remove_dir_all(sandbox.cache_root().join("logseq-graph"))
        .expect("discard the cached index");
    let rebuild = sandbox.run(&["page", "alpha"]);
    let write = sandbox.run(&["append", "alpha", "not written"]);
    fs::set_permissions(&root, permissions).expect("restore graph root mode");

    for answer in [read, rescan, rebuild] {
        assert_eq!(block_count(&stdout_of(&answer)), 3);
    }
    stderr_of(&write, 1);
    let stderr = String::from_utf8_lossy(&write.stderr);
    assert!(stderr.contains("cannot open the write lock"), "{stderr}");
    assert!(!lock.exists(), "a read-only graph still has no lock file");
    assert_eq!(sandbox.graph_files(), before);
    assert_eq!(fs::read(page).expect("unchanged page"), original);
}

#[test]
fn an_explicit_index_is_used_instead_of_the_cache() {
    let sandbox = populated("explicit-index");
    let index = sandbox.explicit_index();
    let index_argument = index.to_str().expect("utf-8 path");

    let stdout = stdout_of(&sandbox.run_raw(&[
        "--graph",
        sandbox.graph_root().to_str().expect("utf-8 path"),
        "--index",
        index_argument,
        "pages",
    ]));

    assert!(stdout.starts_with("alpha  3\n"), "{stdout}");
    assert!(index.is_file(), "the named index file was created");
    assert!(
        !sandbox.cache_root().join("logseq-graph").exists(),
        "the cache location is not touched when --index is given"
    );
    assert_eq!(
        sandbox.graph_files(),
        vec![
            "journals/2026_08_13.md".to_owned(),
            "logseq/config.edn".to_owned(),
            "pages/Bravo.md".to_owned(),
            "pages/alpha.md".to_owned(),
        ],
        "the graph itself stays untouched"
    );
}

#[test]
fn freshness_flags_preserve_facts_and_verify_content_advances_the_revision() {
    let sandbox = populated("freshness");

    let default = stable_lines(&stdout_of(&sandbox.run(&["info"])));
    let verified = stable_lines(&stdout_of(&sandbox.run(&["--verify-content", "info"])));
    let stale = stable_lines(&stdout_of(&sandbox.run(&["--stale", "info"])));

    let revision = |lines: &[String]| {
        lines
            .iter()
            .find_map(|line| line.strip_prefix("revision: "))
            .expect("revision line")
            .parse::<u64>()
            .expect("revision number")
    };
    assert!(
        revision(&verified) > revision(&default),
        "full shadow publication advances the revision"
    );
    let facts = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| !line.starts_with("revision: "))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(facts(&verified), facts(&default));
    assert_eq!(facts(&stale), facts(&verified));
    assert_eq!(revision(&stale), revision(&verified));
}

#[test]
fn help_prints_the_grammar_and_succeeds() {
    let sandbox = Sandbox::new("help");

    let stdout = stdout_of(&sandbox.run_raw(&["--help"]));

    assert!(stdout.starts_with("usage: logseq-graph"), "{stdout}");
    assert!(
        stdout.contains("pages accept durable or ephemeral selectors at either end"),
        "the help permits either identity for cross-page moves: {stdout}"
    );
    assert!(
        stdout.contains("Journals are pages in the derived model"),
        "the help states how journals are counted: {stdout}"
    );
}

/// The three durable ids the move fixtures state.
const MOVE_ID_A: &str = "01234567-89ab-cdef-0123-456789abcdef";
const MOVE_ID_B: &str = "fedcba98-7654-3210-fedc-ba9876543210";
const MOVE_ID_C: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// Two identified blocks on one page, and a third on another, so a cross-page move has a target.
fn movable(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.write(
        "pages/Alpha.md",
        "- first\n\
         \x20 id:: 01234567-89ab-cdef-0123-456789abcdef\n\
         \x20 - child\n\
         - second\n\
         \x20 id:: fedcba98-7654-3210-fedc-ba9876543210\n",
    );
    sandbox.write(
        "pages/Beta.md",
        "- other\n  id:: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
    );
    sandbox
}

#[test]
fn move_places_the_block_where_each_direction_says() {
    // One fresh sandbox per direction: each run rewrites the page, so the same file cannot answer
    // two moves in a row.
    for (direction, args, expected) in [
        (
            "after",
            ["move", MOVE_ID_A, "after", MOVE_ID_B],
            format!("- second\n  id:: {MOVE_ID_B}\n- first\n  id:: {MOVE_ID_A}\n  - child\n"),
        ),
        (
            "before",
            ["move", MOVE_ID_B, "before", MOVE_ID_A],
            format!("- second\n  id:: {MOVE_ID_B}\n- first\n  id:: {MOVE_ID_A}\n  - child\n"),
        ),
        (
            "first-child",
            ["move", MOVE_ID_B, "first-child", MOVE_ID_A],
            format!("- first\n  id:: {MOVE_ID_A}\n  - second\n    id:: {MOVE_ID_B}\n  - child\n"),
        ),
        (
            "last-child",
            ["move", MOVE_ID_B, "last-child", MOVE_ID_A],
            format!("- first\n  id:: {MOVE_ID_A}\n  - child\n  - second\n    id:: {MOVE_ID_B}\n"),
        ),
    ] {
        let sandbox = movable(direction);
        let path = sandbox.graph_root().join("pages/Alpha.md");

        let stdout = stdout_of(&sandbox.run(&args));

        assert_eq!(
            fs::read_to_string(&path).expect("the page file is readable"),
            expected,
            "`{direction}` wrote the wrong bytes"
        );
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines.len(),
            4,
            "the answer is the page's metadata, in the shape `remove` prints: {stdout}"
        );
        assert_eq!(lines[0], "name: Alpha");
        assert_eq!(
            lines[1], "blocks: 3",
            "a move keeps every block, and the projection says so"
        );
        assert_eq!(lines[3], "kind: page");
    }
}

#[test]
fn move_of_a_block_to_another_page_writes_both_files_and_answers_the_destination_page() {
    // A cross-page move replaces two files and projects both pages in one commit, so the answer is
    // the destination page's metadata and the bytes of both files are the assertion that matters:
    // the block leaves the page it came from and lands where the anchor says.
    let sandbox = movable("move-cross-page");
    let alpha = sandbox.graph_root().join("pages/Alpha.md");
    let beta = sandbox.graph_root().join("pages/Beta.md");

    let output = sandbox.run(&["move", MOVE_ID_A, "after", MOVE_ID_C]);

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("name: Beta"),
        "a move answers the page it moved into: {stdout}"
    );
    assert!(
        stdout.contains("blocks: 3"),
        "the destination page holds the anchor, the moved block, and its child: {stdout}"
    );
    assert_eq!(
        fs::read_to_string(&alpha).expect("the page file is readable"),
        format!("- second\n  id:: {MOVE_ID_B}\n"),
        "the source page lost the moved subtree and kept every other byte"
    );
    assert_eq!(
        fs::read_to_string(&beta).expect("the page file is readable"),
        format!("- other\n  id:: {MOVE_ID_C}\n- first\n  id:: {MOVE_ID_A}\n  - child\n"),
        "the destination page gained the subtree after the anchor"
    );
}

#[test]
fn move_with_an_invalid_id_or_direction_exits_two() {
    let sandbox = movable("move-invalid");
    let before = fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable");

    let output = sandbox.run(&["move", "not-an-id", "after", MOVE_ID_B]);
    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("move needs a durable or ephemeral block selector, not \"not-an-id\""),
        "{stderr}"
    );
    assert!(stderr.contains("usage: logseq-graph"), "{stderr}");

    let output = sandbox.run(&["move", MOVE_ID_A, "sideways", MOVE_ID_B]);
    stderr_of(&output, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("before, after, first-child, or last-child"),
        "{stderr}"
    );

    assert_eq!(
        fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable"),
        before,
        "a refused command line writes nothing"
    );
}

#[test]
fn move_of_an_unknown_id_exits_one_and_leaves_the_files_alone() {
    let sandbox = movable("move-missing");
    let id = "00000000-0000-0000-0000-000000000000";
    let before = fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable");
    let files = sandbox.graph_files();

    let output = sandbox.run(&["move", id, "after", MOVE_ID_B]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("block not found: {id}")),
        "a missing block is reported the way the `block` read reports it: {stderr}"
    );
    assert_eq!(
        fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable"),
        before,
        "a move that resolved no block changes no file"
    );
    assert_eq!(sandbox.graph_files(), files, "and creates none");
}

#[test]
fn move_refuses_the_freshness_flags_instead_of_ignoring_them() {
    let sandbox = movable("move-freshness");
    let before = fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable");

    for flag in ["--verify-content", "--stale"] {
        let output = sandbox.run(&[flag, "move", MOVE_ID_A, "after", MOVE_ID_B]);

        stderr_of(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{flag} has no effect on move")),
            "{stderr}"
        );
    }
    assert_eq!(
        fs::read(sandbox.graph_root().join("pages/Alpha.md")).expect("readable"),
        before,
        "a refused command line writes nothing"
    );
}

/// The id two pages state in the ambiguity tests below.
const SHARED_ID: &str = "11111111-2222-3333-4444-555555555555";

/// A graph where two pages state the same `id::`.
///
/// This is a legal input: the projection records both rows, because one duplicated identity line is
/// not a reason to hide a whole page. Everything that addresses the id has to deal with that.
fn ambiguous(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.write(
        "pages/alpha.md",
        "- alpha block\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    sandbox.write(
        "pages/beta.md",
        "- beta block\n  id:: 11111111-2222-3333-4444-555555555555\n",
    );
    sandbox
}

#[test]
fn block_of_an_id_stated_on_two_pages_exits_one_and_names_both() {
    // `block` answers about the index and cannot open a file, so the only honest answer is the
    // refusal. Printing one of the two candidates would look like an answer and hide the other, and
    // an exit code of 0 would tell a script the block was identified.
    let sandbox = ambiguous("block-ambiguous-id");
    let output = sandbox.run(&["block", SHARED_ID]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for page in ["alpha", "beta"] {
        assert!(
            stderr.contains(page),
            "the refusal must name {page:?} so a person can find the duplicate: {stderr}"
        );
    }
    assert!(
        output.stdout.is_empty(),
        "a refused read prints no answer: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn update_of_an_id_stated_on_two_pages_exits_one_and_writes_nothing() {
    // Exit 3 is reserved for a write whose file already holds the new text. This one never read a
    // file, so nothing was written and 1 is the code a script retries nothing on.
    let sandbox = ambiguous("update-ambiguous-id");
    let alpha = sandbox.graph_root().join("pages/alpha.md");
    let beta = sandbox.graph_root().join("pages/beta.md");
    let alpha_before = fs::read(&alpha).expect("readable");
    let beta_before = fs::read(&beta).expect("readable");

    let output = sandbox.run(&["update", SHARED_ID, "changed"]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for page in ["alpha", "beta"] {
        assert!(
            stderr.contains(page),
            "the refusal must name {page:?} so a person can find the duplicate: {stderr}"
        );
    }
    assert_eq!(
        fs::read(&alpha).expect("readable"),
        alpha_before,
        "a refused update must not rewrite the page one candidate is on"
    );
    assert_eq!(
        fs::read(&beta).expect("readable"),
        beta_before,
        "a refused update must not rewrite the page the other candidate is on"
    );
}

#[test]
fn create_writes_an_empty_page_and_answers_it() {
    let sandbox = populated("create");
    let before = sandbox.graph_files();

    let output = sandbox.run(&["create", "Brand new page"]);

    let stdout = stdout_of(&output);
    assert!(stdout.contains("name: Brand new page\n"), "name: {stdout}");
    assert!(stdout.contains("\nblocks: 0\n"), "blocks: {stdout}");
    assert!(
        stdout.contains("\nrevision: "),
        "the answer states the revision the create committed: {stdout}"
    );

    let mut after = before;
    after.push("pages/Brand new page.md".to_owned());
    after.sort();
    assert_eq!(
        sandbox.graph_files(),
        after,
        "exactly one file appeared, under pages/"
    );
    assert_eq!(
        fs::read(sandbox.graph_root().join("pages/Brand new page.md")).expect("readable"),
        b"",
        "the page file is empty: a page with no block is still a page"
    );

    // The next process reads the page the first one created.
    let tree = stdout_of(&sandbox.run(&["page", "Brand new page"]));
    assert!(tree.contains("blocks: 0\n"), "page: {tree}");
}

#[test]
fn create_of_an_existing_page_exits_one_and_keeps_its_bytes() {
    let sandbox = populated("create-exists");
    let path = sandbox.graph_root().join("pages/alpha.md");
    let before = fs::read(&path).expect("readable");

    let output = sandbox.run(&["create", "alpha"]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists"),
        "the refusal says the page is there: {stderr}"
    );
    assert!(
        stderr.contains("alpha.md"),
        "the refusal names the file that holds it: {stderr}"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");
    assert_eq!(
        fs::read(&path).expect("readable"),
        before,
        "a refused create leaves the existing bytes alone"
    );
}

#[test]
fn create_of_an_unencodable_name_exits_one_and_writes_nothing() {
    let sandbox = populated("create-unencodable");
    let before = sandbox.graph_files();

    // `  a` starts or ends with whitespace, which discovery trims: the file would decode to a
    // page other than the one asked for. The create refuses instead of writing a page no read can
    // find again.
    let output = sandbox.run(&["create", "  a"]);

    stderr_of(&output, 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be written as a file name"),
        "the refusal says the name is not representable: {stderr}"
    );
    assert!(output.stdout.is_empty(), "a failure prints no answer");
    assert_eq!(
        sandbox.graph_files(),
        before,
        "a refused create writes no file"
    );
}

#[test]
fn journal_creates_the_configured_day_and_answers_its_metadata() {
    let sandbox = populated("journal");

    let stdout = stdout_of(&sandbox.run(&["journal", "2026-11-12"]));

    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec![
            "name: Nov 12th, 2026",
            "blocks: 0",
            "revision: 4",
            "kind: journal",
            "date: 2026-11-12",
        ]
    );
    assert_eq!(
        fs::read(sandbox.graph_root().join("journals/2026_11_12.md")).expect("journal exists"),
        b""
    );
    assert!(sandbox
        .graph_files()
        .contains(&"journals/2026_11_12.md".to_owned()));
}

#[test]
fn journal_rejects_bad_dates_duplicates_and_freshness_flags_without_writing() {
    let sandbox = Sandbox::new("journal-errors");

    for args in [
        &["journal"][..],
        &["journal", "2026-02-30"],
        &["--stale", "journal", "2026-02-28"],
    ] {
        let output = sandbox.run(args);
        stderr_of(&output, 2);
        assert!(output.stdout.is_empty());
    }
    assert_eq!(sandbox.graph_files(), vec!["logseq/config.edn"]);

    stdout_of(&sandbox.run(&["journal", "2026-02-28"]));
    let output = sandbox.run(&["journal", "2026-02-28"]);
    stderr_of(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
}

#[test]
fn commit_applies_an_ordered_json_batch_and_answers_the_new_version() {
    let sandbox = Sandbox::new("commit");
    sandbox.write("pages/alpha.md", "- old\n- second\n");
    stdout_of(&sandbox.run(&["info"]));
    let initial = stdout_of(&sandbox.run(&["page", "alpha"]));
    let base_version = initial
        .lines()
        .find_map(|line| line.strip_prefix("version: "))
        .expect("page read states the base version");
    let operations = concat!(
        r#"[{"op":"edit_text","block":[0],"text":"changed"},"#,
        r#"{"op":"insert_block","after":[0],"text":"inserted"},"#,
        r###"{"op":"set_marker","block":[0],"marker":"##"}]"###,
    );

    let stdout = stdout_of(&sandbox.run(&["commit", "alpha", base_version, operations]));

    assert_eq!(
        fs::read_to_string(sandbox.graph_root().join("pages/alpha.md")).expect("read page"),
        "## changed\n- inserted\n- second\n"
    );
    assert!(stdout.starts_with("name: alpha\napplied: 3\nrevision: 2\nversion: "));
    assert!(stdout.contains("block: [0]  changed\nblock: [1]  inserted\nblock: [2]  second\n"));

    let version = stdout
        .lines()
        .find_map(|line| line.strip_prefix("version: "))
        .expect("new version");
    let second = stdout_of(&sandbox.run(&[
        "commit",
        "alpha",
        version,
        r#"[{"op":"set_collapsed","block":[1],"collapsed":true}]"#,
    ]));
    assert!(second.contains("applied: 1\n"));
    assert!(second.contains("block: [1]  inserted"));
    assert!(
        fs::read_to_string(sandbox.graph_root().join("pages/alpha.md"))
            .expect("read committed page")
            .contains("collapsed:: true")
    );
}

#[test]
fn commit_rejects_stale_versions_bad_operations_and_bad_json_before_writing() {
    let sandbox = Sandbox::new("commit-errors");
    let path = "pages/alpha.md";
    sandbox.write(path, "- old\n");
    let before = fs::read(sandbox.graph_root().join(path)).expect("read page");

    let stale = sandbox.run(&[
        "commit",
        "alpha",
        "stale",
        r#"[{"op":"merge_block","block":[0]}]"#,
    ]);
    stderr_of(&stale, 1);
    assert!(String::from_utf8_lossy(&stale.stderr).contains("base version"));

    for json in [
        "not json",
        r#"[{"op":"merge_block","block":[0],"extra":true}]"#,
        "[{\"op\":\"set_marker\",\"block\":[0],\"marker\":\"#######\"}]",
    ] {
        let output = sandbox.run(&["commit", "alpha", "current", json]);
        stderr_of(&output, 2);
        assert!(output.stdout.is_empty());
    }
    assert_eq!(
        fs::read(sandbox.graph_root().join(path)).expect("read page"),
        before
    );

    for flag in ["--verify-content", "--stale"] {
        stderr_of(&sandbox.run(&[flag, "commit", "alpha", "current", "[]"]), 2);
    }
}

/// The selector column of a `query` answer, in order.
fn selectors(output: &Output) -> Vec<String> {
    stdout_of(output)
        .lines()
        .filter_map(|line| line.split("  ").next().map(str::to_owned))
        .collect()
}

#[test]
fn query_answers_the_blocks_a_simple_query_matches() {
    let sandbox = Sandbox::new("query-blocks");
    sandbox.write(
        "pages/project.md",
        "- TODO ship the query\n- DONE write the parser\n",
    );
    sandbox.write("pages/notes.md", "- just a note\n");

    let answer = sandbox.run(&["query", "(task TODO)"]);
    let text = stdout_of(&answer);

    assert_eq!(text.lines().count(), 1, "one TODO block: {text}");
    assert!(text.contains("project#0"), "the page and position: {text}");
    assert!(text.contains("TODO ship the query"), "the text: {text}");
    assert!(
        !text.contains("write the parser"),
        "a DONE block is not a TODO: {text}"
    );
}

#[test]
fn a_query_selector_names_the_block_a_later_write_edits() {
    // The whole point of stating a selector: the answer can be handed straight back to a write,
    // including for the 98.8% of blocks that state no `id::`.
    let sandbox = Sandbox::new("query-selector-write");
    sandbox.write("pages/project.md", "- TODO one\n- TODO two\n");

    let listed = sandbox.run(&["query", "(task TODO)", "--limit", "1"]);
    let selector = selectors(&listed).remove(0);
    assert!(
        selector.starts_with("ephemeral:"),
        "a block with no id:: is addressed ephemerally, got {selector}"
    );

    let written = sandbox.run(&["update", &selector, "DONE one"]);
    let text = stdout_of(&written);

    assert!(
        text.contains("DONE one"),
        "the write answers the block: {text}"
    );
    let page = fs::read_to_string(sandbox.graph_root().join("pages/project.md"))
        .expect("read the page back");
    assert!(
        page.contains("- DONE one") && page.contains("- TODO two"),
        "only the addressed block changed: {page}"
    );
}

#[test]
fn a_stale_ephemeral_selector_writes_nothing() {
    // Fail closed: the id names a block only while the page's bytes are the ones the index
    // projected, so an edit made outside the index must not redirect the write at a neighbour.
    let sandbox = Sandbox::new("query-stale");
    sandbox.write("pages/project.md", "- first\n- second\n");

    let listed = sandbox.run(&["query", "(task TODO)"]);
    // A query that matches nothing still proves the index is built; the page then moves under it.
    let _ = listed;
    let listed = sandbox.run(&["query", "\"second\""]);
    let selector = selectors(&listed).remove(0);
    sandbox.write("pages/project.md", "- a new first\n- first\n- second\n");

    let written = sandbox.run(&["update", &selector, "rewritten"]);
    assert_eq!(written.status.code(), Some(1), "a stale id is refused");
    let page = fs::read_to_string(sandbox.graph_root().join("pages/project.md"))
        .expect("read the page back");
    assert!(
        page.contains("- second") && !page.contains("rewritten"),
        "nothing was written: {page}"
    );
}

#[test]
fn a_page_level_query_answers_pages_with_their_kind() {
    let sandbox = Sandbox::new("query-pages");
    sandbox.write("pages/area.md", "- the namespace page\n");
    sandbox.write("pages/area___one.md", "- a page under it\n");
    sandbox.write("journals/2026_08_10.md", "- a journal day\n");

    let answer = sandbox.run(&["query", "(namespace area)"]);
    let text = stdout_of(&answer);

    assert!(text.contains("area  page  -"), "the namespace page: {text}");
    assert!(text.contains("area/one  page  -"), "the child: {text}");
    assert!(!text.contains("2026-08-10"), "not the journal: {text}");
}

#[test]
fn a_truncated_answer_says_so_on_standard_error() {
    let sandbox = Sandbox::new("query-truncated");
    sandbox.write("pages/project.md", "- TODO one\n- TODO two\n- TODO three\n");

    let answer = sandbox.run(&["query", "(task TODO)", "--limit", "2"]);
    let stdout = stdout_of(&answer);
    let stderr = String::from_utf8_lossy(&answer.stderr);

    assert_eq!(
        stdout.lines().count(),
        2,
        "the limit is the answer: {stdout}"
    );
    assert!(
        stderr.contains("truncated at 2"),
        "the answer states it was cut: {stderr}"
    );
}

#[test]
fn a_query_the_grammar_refuses_is_an_input_error_not_an_empty_answer() {
    let sandbox = Sandbox::new("query-refused");

    // An advanced query is not a simple one: refusing beats answering nothing and letting the
    // caller read that as "no block matches".
    let advanced = sandbox.run(&["query", "(sort-by priority)"]);
    assert_eq!(advanced.status.code(), Some(2), "a refused query exits 2");
    let stderr = String::from_utf8_lossy(&advanced.stderr);
    assert!(
        stderr.contains("query"),
        "the message names the input: {stderr}"
    );
    assert!(
        !stderr.contains("usage: logseq-graph"),
        "the command line was well formed, so no usage text: {stderr}"
    );

    let datalog = sandbox.run(&["query", "[:find ?b :where [?b :block/text]]"]);
    assert_eq!(datalog.status.code(), Some(2), "Datalog is not answered");
    assert!(
        String::from_utf8_lossy(&datalog.stdout).trim().is_empty(),
        "a refusal prints no answer"
    );
}

#[test]
fn today_moves_the_window_a_relative_operand_counts_from() {
    let sandbox = Sandbox::new("query-today");
    sandbox.write("journals/2026_08_09.md", "- nine\n");
    sandbox.write("journals/2026_08_10.md", "- ten\n");
    sandbox.write(
        "journals/2026_11_06.md",
        "- the machine's own day is not these\n",
    );

    let answer = sandbox.run(&["query", "(between -1d today)", "--today", "2026-08-10"]);
    let text = stdout_of(&answer);

    assert!(text.contains("Aug 10th, 2026"), "the named day: {text}");
    assert!(
        text.contains("Aug 9th, 2026"),
        "the start of the window, one day before it: {text}"
    );
    assert!(
        !text.contains("Nov 6th, 2026"),
        "the clock's day is overridden: {text}"
    );

    // Moving the day moves the window: the same query one day earlier ends at the ninth.
    let earlier = sandbox.run(&["query", "(between -1d today)", "--today", "2026-08-09"]);
    let text = stdout_of(&earlier);
    assert!(text.contains("Aug 9th, 2026"), "the new window: {text}");
    assert!(
        !text.contains("Aug 10th, 2026"),
        "the window no longer reaches the tenth: {text}"
    );
}

#[test]
fn a_today_that_is_not_a_day_is_refused() {
    let sandbox = Sandbox::new("query-bad-today");

    let answer = sandbox.run(&["query", "(journal)", "--today", "2026-02-30"]);
    assert_eq!(answer.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&answer.stderr).contains("YYYY-MM-DD"),
        "the message names the shape it wanted"
    );

    let misplaced = sandbox.run(&["pages", "--today", "2026-02-28"]);
    assert_eq!(misplaced.status.code(), Some(2), "--today is a query's");
}

#[test]
fn block_reads_a_block_by_the_selector_a_query_printed() {
    let sandbox = Sandbox::new("query-block-selector");
    sandbox.write("pages/project.md", "- the only block\n");

    let listed = sandbox.run(&["query", "\"the only block\""]);
    let selector = selectors(&listed).remove(0);

    let read = sandbox.run(&["block", &selector]);
    let text = stdout_of(&read);

    assert!(text.contains("text: the only block"), "the block: {text}");
    assert!(text.contains("preorder: 0"), "its position: {text}");
}
