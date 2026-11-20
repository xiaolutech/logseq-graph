//! Embeddable Logseq graph library.
//!
//! Files remain the source of product facts. The `source` layer is the only code that touches the
//! graph filesystem: it opens a graph root, discovers Markdown pages and journals, reads their
//! bytes as text, and reports their stat. The `document` layer parses one such text into a
//! source-preserving AST and renders it back without touching the filesystem. The `index` layer
//! projects such a parsed page into a SQLite-backed derived read model that serves page metadata and
//! page-tree queries. The `engine` layer orchestrates the three: it opens a graph, scans its files
//! into the index, and answers reads under an explicit freshness policy, each result carrying the
//! instance, revision, staleness, and observation time it was produced at (ADR-0006).

// The lints below are the policy for the code this crate ships, so they are warned about in
// production builds only. `cfg_attr(not(test))` is what says so: `--all-targets` compiles this
// library a second time as its own test harness, and the unit tests inside it will assert with
// `expect` all they like. An integration test under `tests/` is a separate crate and never sees a
// crate-root attribute of this one, so it is unaffected either way.

#![cfg_attr(
    not(test),
    warn(
        // P1: a library that ships to callers does not panic on input.
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        unreachable_pub,
        // P2: the zero-noise tier. Each of these is at zero hits after the cleanup in this commit.
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::wildcard_imports,
        unused_lifetimes,
        rust_2018_idioms,
        clippy::allow_attributes,
        clippy::allow_attributes_without_reason,
        clippy::let_underscore_must_use
    )
)]

pub mod document;
pub mod domain;
pub mod engine;
pub mod index;
pub mod source;
