//! Command line front end for the Logseq graph library.
//!
//! This binary is an adapter and nothing else (ADR-0003): it parses arguments, opens a [`Graph`],
//! calls one read or one write, and prints the answer. It holds no SQL, walks no directory of the
//! graph, and parses no Markdown; every fact comes from `logseq_graph`'s public surface.
//!
//! The one policy this binary owns is where the derived index lives, because the engine leaves that
//! to its caller; [`cache`] is that policy, and it never writes inside the graph.
//!
//! Exit codes are part of the interface: `0` for an answer, `2` for a command line that names no
//! valid invocation, `1` for a graph, index, or page that could not be read or for a write that left
//! the files alone, and `3` for a write whose file holds the new text while the index is behind it.
//! The `1`/`3` split is the reason [`WriteFailure`] exists: a script that cannot tell "nothing was
//! written" from "written, index stale" retries the second case and appends a second block.
//! Nothing here panics on input, so a wrong argument is a message rather than a backtrace.
//!
//! The lints below are the policy that keeps it that way in the code this crate ships. They are
//! warned about here rather than in `[workspace.lints]` because they are about production code: an
//! integration test is a separate crate and may assert with `expect` all it likes, and the unit
//! tests in this crate are not held to it either (`cfg_attr(not(test))`).
//!
//! `clippy::print_stdout` and `clippy::print_stderr` are deliberately absent. Printing an answer on
//! standard output and a failure on standard error is what a command line front end is for, so a
//! lint against it here would only earn `#[expect]` attributes around the crate's main job. The
//! library is where printing is the surprise, and it forbids both.

#![cfg_attr(
    not(test),
    warn(
        // P1: the process must not panic on input.
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        unreachable_pub,
        // P2: the zero-noise tier.
        clippy::wildcard_imports,
        unused_lifetimes,
        rust_2018_idioms,
        clippy::allow_attributes,
        clippy::allow_attributes_without_reason,
        clippy::let_underscore_must_use
    )
)]

mod args;
mod batch;
mod cache;
mod http;
mod render;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use logseq_graph::domain::{
    BlockId, BlockSelector, JournalDate, PageCommit, PageCommitResult, PageMatch, PageMetadata,
    QueryContext, QueryOutcome,
};
use logseq_graph::engine::{
    EngineError, Freshness, Graph, MoveTarget, OpenOptions, WriteError, WriteState,
};

use args::{Command, Invocation, MoveDirection, Parsed, USAGE};
use render::{one_line, rfc3339};

/// Why one invocation did not complete.
enum Failure {
    /// The arguments name no valid invocation: exit code 2.
    Usage(String),
    /// A query's text is not one this build can answer: exit code 2.
    ///
    /// The exit code matches [`Failure::Usage`] because both are the caller's input being wrong
    /// rather than the graph being unreadable, but no usage text is printed: the command line was
    /// well formed, and the grammar that refused it is the query's, whose message says where.
    Query(String),
    /// The graph, the index, or a page could not be read, or a write changed nothing: exit code 1.
    Runtime(String),
    /// The file holds the new text and the index is behind it: exit code 3.
    ///
    /// The message is the library's own, because it already says which file was replaced, that the
    /// index is behind, and that the next scan repairs it.
    Unindexed(String),
}

impl Failure {
    /// What to print on standard error.
    fn message(&self) -> &str {
        match self {
            Self::Usage(message)
            | Self::Query(message)
            | Self::Runtime(message)
            | Self::Unindexed(message) => message,
        }
    }

    /// The exit code a script branches on.
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Query(_) => 2,
            Self::Runtime(_) => 1,
            Self::Unindexed(_) => 3,
        }
    }
}

/// What a failed write means for the caller, which is what decides its exit code.
///
/// This split is the library's, not an invention here: `IndexAfterWrite` and `ReReadAfterWrite` mean
/// the file already holds the new text and only the index is behind, while every other variant means
/// the files are exactly as they were. Folding the two together into exit code 1 would tell a script
/// that the append never happened, and its retry would append the block a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteFailure {
    /// Nothing was written: exit code 1.
    Unwritten,
    /// The file holds the new text and the index is behind it: exit code 3.
    Unindexed,
}

/// Classify a failed write by whether the file changed.
///
fn classify_write(error: &WriteError) -> WriteFailure {
    match error.write_state() {
        WriteState::Unwritten => WriteFailure::Unwritten,
        WriteState::FileChanged => WriteFailure::Unindexed,
    }
}

/// Turn a failed write into the failure this process reports.
///
/// A missing page or block is the one pair of `Unwritten` variants whose library wording is not the
/// CLI's: the reads answer `page not found: <name>` and `block not found: <id>`, and a write that
/// creates neither has to read the same way.
fn write_failure(error: WriteError) -> Failure {
    match classify_write(&error) {
        WriteFailure::Unindexed => Failure::Unindexed(error.to_string()),
        WriteFailure::Unwritten => match error {
            WriteError::PageNotFound { page } => page_not_found(&page),
            WriteError::BlockNotFound { id } => block_not_found(&id),
            other => Failure::Runtime(other.to_string()),
        },
    }
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("{}", failure.message());
            if matches!(failure, Failure::Usage(_)) {
                eprintln!("{USAGE}");
            }
            ExitCode::from(failure.exit_code())
        }
    }
}

/// The tokens `serve` accepts: every `--api-token`, then `LOGSEQ_API_TOKEN` when it is set, holds
/// no whitespace, and is not already named. The variable is the one the official-answer capture
/// script reads, so a shell that can call the official API can call this one with the same token.
fn api_tokens(invocation: &Invocation) -> Vec<String> {
    let mut tokens = invocation.api_tokens.clone();
    if let Ok(token) = std::env::var("LOGSEQ_API_TOKEN") {
        let usable = !token.is_empty() && !token.chars().any(char::is_whitespace);
        if usable && !tokens.iter().any(|named| named == &token) {
            tokens.push(token);
        }
    }
    tokens
}

/// Parse the arguments, open the graph, print one answer.
fn run(argv: Vec<String>) -> Result<(), Failure> {
    let invocation = match args::parse(argv).map_err(|error| Failure::Usage(error.to_string()))? {
        Parsed::Help => return print_stdout(args::HELP),
        Parsed::Run(invocation) => invocation,
    };

    let graph_root = resolve_graph_root(&invocation)?;
    let index_path = match &invocation.index {
        Some(path) => path.clone(),
        None => {
            cache::index_path(&graph_root).map_err(|error| Failure::Runtime(error.to_string()))?
        }
    };

    // `serve` is a long-lived process, so its one job here is to hand the resolved paths to the
    // HTTP module and never come back: the loop owns this thread until the process is killed. Its
    // graph is opened inside, on the scan thread that `serve` starts after it binds the port, so
    // this call returns a bound socket before the graph exists (ADR-0008).
    if let Command::Serve {
        bind,
        poll_interval,
        no_watch,
    } = &invocation.command
    {
        return http::serve(
            &graph_root,
            &index_path,
            bind,
            *poll_interval,
            *no_watch,
            api_tokens(&invocation),
        )
        .map_err(Failure::Runtime);
    }

    // Opening scans once under the default stat policy, which is what makes the handle readable
    // without the caller paying for a read first. A write does not need that scan: it resolves the
    // one file it edits from the index and falls back to one discovery, so it skips the scan and
    // leaves the freshness flags to the reads, which name their own policy anyway.
    let mut graph = Graph::open(
        OpenOptions::new(&graph_root, &index_path)
            .with_freshness(invocation.freshness)
            .with_startup_scan(invocation.command.needs_startup_scan()),
    )
    .map_err(|error| Failure::Runtime(error.to_string()))?;

    let answer = execute(&mut graph, &invocation, &graph_root, &index_path)?;
    print_stdout(&answer)
}

/// The graph root the arguments name, canonicalized.
///
/// `--graph` wins over `LOGSEQ_GRAPH`, and neither is optional: without a root there is nothing to
/// open, so that is a usage error rather than a read that fails later.
fn resolve_graph_root(invocation: &Invocation) -> Result<PathBuf, Failure> {
    let named = match &invocation.graph {
        Some(path) => path.clone(),
        None => match std::env::var_os("LOGSEQ_GRAPH") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                return Err(Failure::Usage(
                    "no graph root: pass --graph <dir> or set LOGSEQ_GRAPH".to_owned(),
                ))
            }
        },
    };

    // Canonical, because the cache key is derived from it: two spellings of one directory must not
    // produce two indexes, and a graph that was moved must not reuse its old one.
    std::fs::canonicalize(&named).map_err(|error| {
        Failure::Runtime(format!("graph not found: {} ({error})", named.display()))
    })
}

/// Run one command and render its answer.
fn execute(
    graph: &mut Graph,
    invocation: &Invocation,
    graph_root: &Path,
    index_path: &Path,
) -> Result<String, Failure> {
    let freshness = invocation.freshness;
    let mut out = String::new();

    // `serve` never reaches this match: `run` hands it to [`http::serve`] before the graph for a
    // one-command invocation is opened. The match has no wildcard arm on purpose, so the arm
    // below names the variant rather than sweeping it into an empty answer.
    if let Command::Serve { .. } = &invocation.command {
        return Err(Failure::Usage(
            "serve cannot run as a one-command invocation".to_owned(),
        ));
    }

    // The early return above handles `serve` before this match runs, so the arm below can only
    // follow a broken invariant. It is kept because a wildcard would silently swallow a future
    // variant instead of failing to compile, and the arm panics rather than printing an answer
    // that names no command.
    match &invocation.command {
        Command::Info => {
            let metadata = graph
                .reads(freshness)
                .metadata()
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let pages = graph
                .reads(Freshness::LastScan)
                .pages()
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let block_count: u64 = pages
                .value()
                .iter()
                .map(|page| u64::from(page.block_count()))
                .sum();
            let index_size = graph
                .index_storage_usage()
                .map_err(|error| Failure::Runtime(error.to_string()))?
                .map_or(0, |usage| usage.live());

            line(&mut out, format_args!("graph: {}", graph_root.display()));
            line(&mut out, format_args!("index: {}", index_path.display()));
            line(
                &mut out,
                format_args!("revision: {}", metadata.meta().revision()),
            );
            let file_pages = pages.value().iter().filter(|page| page.has_file()).count();
            line(&mut out, format_args!("pages: {file_pages}"));
            line(&mut out, format_args!("blocks: {block_count}"));
            line(&mut out, format_args!("index-size: {index_size}"));
            line(
                &mut out,
                format_args!("observed: {}", rfc3339(metadata.meta().last_scan())),
            );
            line(
                &mut out,
                format_args!("stale: {}", metadata.meta().is_stale()),
            );
        }

        Command::Pages { date } => {
            let pages = graph
                .reads(freshness)
                .pages()
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for page in pages.value() {
                match (date, page.journal_date()) {
                    // Without the flag the two columns are byte for byte what they always were.
                    (false, _) => line(
                        &mut out,
                        format_args!("{}  {}", page.name(), page.block_count()),
                    ),
                    // The day goes last so the two columns others already read keep their places;
                    // a page that is not a day prints a placeholder, so the column count is the
                    // same on every line and `sort -k3` or `cut -f3` still lines up.
                    (true, Some(day)) => line(
                        &mut out,
                        format_args!("{}  {}  {}", page.name(), page.block_count(), day),
                    ),
                    (true, None) => line(
                        &mut out,
                        format_args!("{}  {}  -", page.name(), page.block_count()),
                    ),
                }
            }
        }

        Command::Page { name } => {
            let metadata = graph
                .reads(freshness)
                .page_metadata(name)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let Some(page) = metadata.present() else {
                return Err(page_not_found(name));
            };
            let properties = graph
                .reads(freshness)
                .page_properties(name)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let version = if page.has_file() {
                let versions = graph
                    .page_versions(name)
                    .map_err(|error| Failure::Runtime(error.to_string()))?
                    .into_value()
                    .ok_or_else(|| page_not_found(name))?;
                Some(
                    versions
                        .disk()
                        .ok_or_else(|| Failure::Runtime(format!("page file not found: {name}")))?
                        .to_owned(),
                )
            } else {
                None
            };

            line(&mut out, format_args!("name: {}", page.name()));
            line(&mut out, format_args!("blocks: {}", page.block_count()));
            line(&mut out, format_args!("revision: {}", page.revision()));
            line(&mut out, format_args!("kind: {}", page.kind()));
            journal_date(&mut out, page.journal_date());
            if let Some(version) = version {
                line(&mut out, format_args!("version: {version}"));
            } else {
                line(&mut out, format_args!("file: none"));
            }
            // A page that exists without property lines answers with an empty list, so this prints
            // nothing for it rather than pretending the page is absent.
            for property in properties.present().map(Vec::as_slice).unwrap_or(&[]) {
                line(
                    &mut out,
                    format_args!("{}: {}", property.key(), property.value()),
                );
            }
        }

        Command::Tree { name } => {
            let blocks = graph
                .reads(freshness)
                .page_blocks(name)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let Some(blocks) = blocks.present() else {
                return Err(page_not_found(name));
            };
            for block in blocks {
                // Two spaces per level: enough to read a tree, cheap enough not to measure anything.
                let indent = "  ".repeat(block.depth() as usize);
                line(&mut out, format_args!("{indent}{}", one_line(block.text())));
            }
        }

        Command::Backlinks { name } => {
            let backlinks = graph
                .reads(freshness)
                .backlinks(name)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for backlink in backlinks.value() {
                if backlink.pre_block() {
                    line(&mut out, format_args!("{}#pre-block", backlink.page()));
                    continue;
                }
                line(
                    &mut out,
                    format_args!(
                        "{}#{}  {}",
                        backlink.page(),
                        backlink.preorder(),
                        one_line(backlink.text())
                    ),
                );
            }
        }

        Command::Tags => {
            let tags = graph
                .reads(freshness)
                .tags()
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for tag in tags.value() {
                line(
                    &mut out,
                    format_args!("{}  {}", tag.name(), tag.page_count()),
                );
            }
        }

        Command::Tag { name } => {
            let pages = graph
                .reads(freshness)
                .pages_with_tag(name)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for page in pages.value() {
                line(&mut out, format_args!("{page}"));
            }
        }

        Command::Search { query, limit } => {
            let hits = graph
                .reads(freshness)
                .search(query, *limit)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for hit in hits.value() {
                // The same shape `query` prints: one line per block, the selector first so the hit
                // can be piped straight into `block` or `update`.
                line(
                    &mut out,
                    format_args!(
                        "{}  {}#{}  {}",
                        render::selector(hit.selector()),
                        hit.page(),
                        hit.preorder(),
                        one_line(hit.snippet())
                    ),
                );
            }
        }

        Command::Find { query, limit } => {
            let candidates = graph
                .reads(freshness)
                .page_candidates(query, *limit)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            for candidate in candidates.value() {
                // A page-name hit prints the name a page read accepts. An alias hit prints the
                // alias beside it, so the line says why this page answered without pretending the
                // alias is a page of its own.
                match candidate.via() {
                    PageMatch::Alias => line(
                        &mut out,
                        format_args!(
                            "{}  alias: {}",
                            candidate.name(),
                            candidate.alias().unwrap_or_default()
                        ),
                    ),
                    PageMatch::Name => line(&mut out, format_args!("{}", candidate.name())),
                }
            }
        }

        Command::Query { expr, limit, today } => {
            let day = match today {
                Some(day) => *day,
                None => render::today(std::time::SystemTime::now()).ok_or_else(|| {
                    Failure::Runtime(
                        "the system clock states no usable calendar day; pass --today".to_owned(),
                    )
                })?,
            };
            let read = graph
                .reads(freshness)
                .query(expr, &QueryContext::new(day, *limit))
                .map_err(query_failure)?;
            let result = read.value();
            match result.outcome() {
                QueryOutcome::Blocks(blocks) => {
                    for block in blocks {
                        line(
                            &mut out,
                            format_args!(
                                "{}  {}#{}  {}",
                                render::selector(block.selector()),
                                block.page(),
                                block.preorder(),
                                one_line(block.text())
                            ),
                        );
                    }
                }
                QueryOutcome::Pages(pages) => {
                    for page in pages {
                        line(
                            &mut out,
                            format_args!(
                                "{}  {}  {}",
                                page.name(),
                                page.kind(),
                                page.journal_date()
                                    .map_or_else(|| "-".to_owned(), |day| day.to_string())
                            ),
                        );
                    }
                }
            }
            if result.truncated() {
                // Standard error, because standard output is the answer: a listing piped into a
                // file must not grow a line that is not a match. Stated at all because a caller
                // that does not know the answer was cut would read a prefix as the whole.
                eprintln!("truncated at {limit}: raise --limit to see the rest");
            }
        }

        Command::Block { selector } => {
            let block = graph
                .reads(freshness)
                .block(selector)
                .map_err(|error| Failure::Runtime(error.to_string()))?;
            let Some(view) = block.present() else {
                return Err(selector_not_found(selector));
            };
            out.push_str(&render::block(view));
        }

        Command::Serve { .. } => {
            // Unreachable: the early return above handles it before this match runs.
            unreachable!("serve is dispatched in run before the graph is opened")
        }
        Command::Update { selector, text } => {
            // The answer is the write's own observation of the block it replaced, so printing it
            // needs no further read and cannot describe a different revision.
            let written = graph.update_block(selector, text).map_err(write_failure)?;
            out.push_str(&render::block(written.value()));
        }

        Command::Append { page, text } => {
            // The answer is the write's own: `append_block` returns the page metadata it committed,
            // so a second read here would be a different observation pretending to be this one.
            let written = graph.append_block(page, text).map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }

        Command::Create { name } => {
            // Same shape as `append`: a create answers the page it committed, which for a new page
            // is its name, its zero blocks, and the revision the create advanced to.
            let written = graph.create_page(name).map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }

        Command::Journal { date } => {
            let written = graph.create_journal(*date).map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }

        Command::Commit {
            page,
            base_version,
            operations,
        } => {
            let operations = operations
                .iter()
                .map(batch::operation)
                .collect::<Result<Vec<_>, _>>()
                .map_err(Failure::Usage)?;
            let commit = PageCommit::new(page, base_version.clone().into(), operations);
            let written = graph.commit_page(&commit).map_err(write_failure)?;
            commit_result(&mut out, &written);
        }

        Command::EnsureBlockId { selector } => {
            let written = graph.ensure_block_id(selector).map_err(write_failure)?;
            let answer = written.value();
            out.push_str(&format!("id\t{}\ncreated\t{}\n", answer.id, answer.created));
            out.push_str(&render::block(&answer.block));
        }
        Command::Remove { selector } => {
            // Same shape as `append`: a removal answers the page it left behind, which is the only
            // thing left to describe once the block and its subtree are gone.
            let written = graph.remove_block(selector).map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }

        Command::RemoveRange { first, last } => {
            let written = graph
                .remove_block_range(first, last)
                .map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }
        Command::Move {
            selector,
            direction,
            anchor,
        } => {
            // A move answers the page for the same reason a removal does: the block it moved has no
            // row of this write's own to answer with that `block` does not already answer, and the
            // page is where the caller can see the new shape.
            let target = match direction {
                MoveDirection::Before => MoveTarget::Before(anchor.clone()),
                MoveDirection::After => MoveTarget::After(anchor.clone()),
                MoveDirection::FirstChild => MoveTarget::FirstChildOf(anchor.clone()),
                MoveDirection::LastChild => MoveTarget::LastChildOf(anchor.clone()),
            };
            let written = graph.move_block(selector, target).map_err(write_failure)?;
            page_metadata(&mut out, written.value());
        }
    }

    Ok(out)
}

/// Render the metadata one write committed, one field per line, ending with the page's kind.
///
/// `append` and `remove` both answer a page rather than a block, and they answer it the same way:
/// the same write must not have two descriptions. The `page` command prints the same four fields in
/// the same order before its property lines.
fn page_metadata(out: &mut String, page: &PageMetadata) {
    line(out, format_args!("name: {}", page.name()));
    line(out, format_args!("blocks: {}", page.block_count()));
    line(out, format_args!("revision: {}", page.revision()));
    line(out, format_args!("kind: {}", page.kind()));
    journal_date(out, page.journal_date());
}

fn commit_result(out: &mut String, result: &logseq_graph::domain::Read<PageCommitResult>) {
    let snapshot = result.value().snapshot();
    line(out, format_args!("name: {}", snapshot.page()));
    line(out, format_args!("applied: {}", result.value().applied()));
    line(out, format_args!("revision: {}", result.meta().revision()));
    line(out, format_args!("version: {}", snapshot.version()));
    fn render_blocks(out: &mut String, blocks: &[logseq_graph::domain::SnapshotBlock]) {
        for block in blocks {
            let path = block
                .child_path()
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            line(
                out,
                format_args!("block: [{path}]  {}", one_line(block.text())),
            );
            render_blocks(out, block.children());
        }
    }
    render_blocks(out, snapshot.blocks());
}

/// One `date:` line for a journal, and nothing at all for a regular page.
///
/// A journal is the only page that has a day, so a regular page prints no `date:` line rather than
/// an empty one: an absent field and a field holding nothing are different answers, and a script
/// reading `date:` should not get an empty string for a page that is not a journal.
fn journal_date(out: &mut String, date: Option<JournalDate>) {
    if let Some(date) = date {
        line(out, format_args!("date: {date}"));
    }
}

/// A page that has no derived rows, reported the way the CLI promises.
fn page_not_found(name: &str) -> Failure {
    Failure::Runtime(format!("page not found: {name}"))
}

/// A durable id that no block states, reported the way the `block` read reports it.
fn block_not_found(id: &BlockId) -> Failure {
    Failure::Runtime(format!("block not found: {id}"))
}

/// The failure of a selector that names no block the index holds.
///
/// The selector is echoed in the spelling the caller can type again, prefixes included: a message
/// that dropped the prefix would name the other namespace's id, which is a different block.
fn selector_not_found(selector: &BlockSelector) -> Failure {
    Failure::Runtime(format!("block not found: {}", render::selector(selector)))
}

/// Turn a failed query into the failure this process reports.
///
/// A query the grammar refuses is the caller's text being wrong, which is not the same class as an
/// index that could not be read, so the two keep different exit codes.
fn query_failure(error: EngineError) -> Failure {
    match error {
        EngineError::Query(error) => Failure::Query(error.to_string()),
        other => Failure::Runtime(other.to_string()),
    }
}

/// Append one record and its line break.
fn line(out: &mut String, arguments: std::fmt::Arguments<'_>) {
    // The write is to a `String`, whose `fmt::Write` is infallible: the one error it can return is
    // for a formatter that refuses bytes, which `String` never does. Propagating it would turn every
    // one of the caller's record lines into a `?`, which is a lot of machinery for an answer that
    // cannot arrive.
    #[expect(
        clippy::expect_used,
        reason = "writing to a String cannot fail, so there is no error to propagate"
    )]
    out.write_fmt(arguments)
        .expect("writing to a String cannot fail");
    out.push('\n');
}

/// Write the whole answer at once, so a caller piping the output cannot see half a record.
fn print_stdout(text: &str) -> Result<(), Failure> {
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        // A closed pipe (`| head`) is the reader's decision, not a failure of this program.
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(Failure::Runtime(format!(
            "could not write to stdout: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::Duration;

    use logseq_graph::document::EditError;
    use logseq_graph::engine::EngineError;
    use logseq_graph::index::IndexError;
    use logseq_graph::source::{LockError, SourceError};

    /// The page file the write errors name.
    fn path() -> PathBuf {
        PathBuf::from("/graph/pages/alpha.md")
    }

    /// One index failure, enough to fill the variants that carry a cause.
    fn cause() -> IndexError {
        IndexError::CorruptRow {
            column: "pages.name",
            value: "alpha".to_owned(),
        }
    }

    #[test]
    fn a_write_that_left_the_files_alone_earns_exit_code_one() {
        // Every `WriteError` variant from before step 6 of `append_block`, one of each, so a variant
        // that gains or loses its classification fails here rather than in a script.
        let unwritten = [
            WriteError::Lock(LockError::Busy {
                waited: Duration::ZERO,
            }),
            WriteError::Engine(EngineError::StaleIndex {
                reason: "the scan did not complete".to_owned(),
            }),
            WriteError::Source(SourceError::NotUtf8 {
                path: path(),
                valid_up_to: 0,
                len: 0,
            }),
            WriteError::Index(cause()),
            WriteError::Edit(EditError::MultilineContent),
            WriteError::PageNotFound {
                page: "alpha".to_owned(),
            },
            WriteError::PageNameNotEncodable {
                name: "a/b".to_owned(),
                reason: "the encoded name would decode to another page".to_owned(),
            },
            WriteError::PageExists {
                page: "alpha".to_owned(),
                path: path(),
            },
            WriteError::BlockNotFound {
                id: logseq_graph::domain::BlockId::parse("01234567-89ab-cdef-0123-456789abcdef")
                    .expect("valid id"),
            },
            WriteError::AmbiguousBlockId {
                id: logseq_graph::domain::BlockId::parse("01234567-89ab-cdef-0123-456789abcdef")
                    .expect("valid id"),
                pages: vec!["alpha".to_owned(), "bravo".to_owned()],
            },
            WriteError::Operation {
                page: "alpha".to_owned(),
                index: 1,
                operation: "IndentBlock",
                cause: EditError::NoPreviousSibling,
            },
            // `WriteError::StaleVersion` is not built here: its payload is a `PageSnapshot`, and the
            // library has no public constructor for one — a snapshot is only ever the answer of a
            // read. The library's `WriteState` match is exhaustive, so a new variant cannot ship
            // without choosing a state.
        ];

        for error in unwritten {
            assert_eq!(
                classify_write(&error),
                WriteFailure::Unwritten,
                "{error} changed no file"
            );
            assert_eq!(write_failure(error).exit_code(), 1);
        }
    }

    #[test]
    fn a_write_whose_file_changed_earns_exit_code_three() {
        let unindexed = [
            WriteError::ReReadAfterWrite {
                page: "alpha".to_owned(),
                path: path(),
            },
            WriteError::IndexAfterWrite {
                page: "alpha".to_owned(),
                path: path(),
                cause: cause(),
            },
            WriteError::Source(SourceError::Committed {
                path: path(),
                step: path(),
                source: std::io::Error::other("directory sync failed"),
            }),
            // A partial cross-page move may have written either file; the intent says so, and
            // the next open reconciles it, which is the same contract as the two single-file
            // variants above. Constructed directly: forcing a real move to stop halfway needs
            // fault injection inside the write, and this classification is the only thing
            // under test.
            WriteError::PartialMove {
                from: "alpha".to_owned(),
                from_path: path(),
                to: "bravo".to_owned(),
                to_path: PathBuf::from("/graph/pages/bravo.md"),
                cause: "the source file could not be replaced".to_owned(),
            },
        ];
        for error in unindexed {
            assert_eq!(
                classify_write(&error),
                WriteFailure::Unindexed,
                "{error} replaced the file"
            );
            assert_eq!(write_failure(error).exit_code(), 3);
        }
    }

    #[test]
    fn a_partial_move_maps_to_the_file_changed_wording_and_exit_code_three() {
        let failure = write_failure(WriteError::PartialMove {
            from: "alpha".to_owned(),
            from_path: path(),
            to: "bravo".to_owned(),
            to_path: PathBuf::from("/graph/pages/bravo.md"),
            cause: "the source file could not be replaced".to_owned(),
        });
        let message = failure.message();

        assert_eq!(failure.exit_code(), 3);
        assert!(
            message.contains("stopped"),
            "the message says the move stopped short: {message}"
        );
        assert!(
            message.contains("some files were written"),
            "the message says which side may have changed, in the established \
             file-changed-index-behind wording: {message}"
        );
        assert!(
            message.contains("the next open"),
            "the message names the recovery: {message}"
        );
        assert!(
            message.contains("pages/alpha.md") && message.contains("pages/bravo.md"),
            "the message names both files: {message}"
        );
    }

    #[test]
    fn the_unindexed_message_says_the_file_is_written_and_the_index_is_behind() {
        let failure = write_failure(WriteError::IndexAfterWrite {
            page: "alpha".to_owned(),
            path: path(),
            cause: cause(),
        });
        let message = failure.message();

        assert_eq!(failure.exit_code(), 3);
        assert!(message.contains("was replaced"), "{message}");
        assert!(message.contains("index is behind the file"), "{message}");
        assert!(
            message.contains("next scan"),
            "the message names the repair: {message}"
        );
    }

    #[test]
    fn a_missing_page_reads_the_way_the_reads_report_it() {
        let failure = write_failure(WriteError::PageNotFound {
            page: "nope".to_owned(),
        });

        assert_eq!(failure.message(), "page not found: nope");
        assert_eq!(failure.exit_code(), 1);
    }

    #[test]
    fn a_refused_edit_keeps_the_library_wording() {
        let failure = write_failure(WriteError::Edit(EditError::MultilineContent));

        assert_eq!(
            failure.message(),
            "block content must fit on one source line"
        );
        assert_eq!(failure.exit_code(), 1);
    }

    #[test]
    fn the_usage_and_runtime_failures_keep_their_exit_codes() {
        assert_eq!(Failure::Usage(String::new()).exit_code(), 2);
        assert_eq!(Failure::Runtime(String::new()).exit_code(), 1);
        assert_eq!(Failure::Unindexed(String::new()).exit_code(), 3);
    }
}
