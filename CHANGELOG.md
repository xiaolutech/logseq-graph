# Changelog

All notable changes to this project are recorded here. Versions follow [SemVer](https://semver.org/);
before 1.0.0 any minor or patch release may change the API.

## 0.0.1 - 2026-11-20

First public release: prebuilt `logseq-graph` binaries for macOS (arm64, x86_64) and Linux (x86_64)
on GitHub Releases, with a shell installer. Not published to crates.io.

### Added

- Embeddable Rust engine (`logseq-graph`) that projects a file-based Logseq graph into a disposable
  SQLite index: lossless Markdown parsing and local edits, pages, journals, properties, refs, tags,
  backlinks, namespaces, favorites, block search and Logseq simple queries.
- Page and block snapshots with version tokens, stale-write refusal for edit batches, atomic page
  replacement, recoverable cross-page moves, and deletion into `logseq/.recycle/`.
- `logseq-graph` CLI for reading and editing a graph from the shell.
- `logseq-graph serve`: a loopback `POST /api` server behind a bearer token, answering in the
  official Logseq API wire shape, with filesystem notifications and a polling fallback.
