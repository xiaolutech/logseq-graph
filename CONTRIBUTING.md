# Contributing

This repository is a library, a CLI, and one Logseq API adapter with reads and writes. The Markdown files in a
Logseq graph are the source of truth; everything in SQLite is a disposable projection of them
(ADR-0001). Read these before changing code:

- `AGENTS.md` — where everything lives.
- `docs/adr/README.md` and the ADRs — the architecture you are working inside.
- `LESSONS.md` — mistakes this project has already paid for.
- `PERFORMANCE.md` — measured costs on real graphs, before you optimise anything.
- `HANDOFF.md` — what is done and what comes next.

## Before you open a pull request

The pre-commit and pre-push hooks run every gate this repository requires, so a commit that passes
them has already passed. To run them by hand:

```sh
just check          # spelling, tests, fmt, clippy, perf-doc self-test, rustdoc, commit subjects
```

That is the same list the hooks run:

```sh
typos --force-exclude
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/check_perf_doc.py --self-test
RUSTDOCFLAGS="-D warnings" cargo doc -p logseq-graph --no-deps
python3 scripts/check_commit_msg.py --range origin/main...HEAD
```

One gate is deliberately absent from that list because it needs the network: `cargo deny check` (or
`just deny`) covers RUSTSEC advisories, the licence allow list, duplicate crate versions, and where
every crate comes from. `deny.toml` says what it checks and why each rule is `deny` or `warn`. It is
a **pre-commit** hook filtered to the dependency graph and to `deny.toml`, because its answer can
only change when one of those two does, so any other commit skips it for free (~1.3s when it does
run). It fetches the advisory database on every run and fails when it cannot, so an offline commit
that touches one of those files is red — the warm cache is not a fallback. Be online and commit again, or run
`just deny` once to see the same result and warm the cache; nothing is broken. The filter cannot see
a new CVE published while `Cargo.lock` sits untouched, which is why `just deny` stays a habit rather
than a hook.

[CI](.github/workflows/ci.yml) runs the mise-pinned `just check` and `just deny` on Linux and macOS for pull requests and pushes to `main` or `devin/**`.

Which lint goes in which tier — P0 in `[workspace.lints]`, P1/P2 as crate-root attributes, and why —
is `docs/lint-policy.md`.

The doc gate is `-p logseq-graph --no-deps`, not `--workspace`: the CLI's bin target and the
library's lib target both document to `target/doc/index.html`, and `-D warnings` turns that
collision into a red build.

A behaviour change needs a test that fails when the behaviour breaks, not a test that only checks
the new happy path. Every test in `crates/server/tests/rpc/` names the official Logseq method it
corresponds to and its row in `docs/logseq-api-parity.md`; keep that mapping true when you add a
method or a test.

### Hot paths

`scripts/check_perf_doc.py` lists the measured hot paths. Stage a change under one of them and the
`perf-doc` pre-commit hook requires a `PERFORMANCE.md` row in the same commit. `prek.toml` is the only
hook configuration; run `mise install` once to get every pinned tool (prek, rust, typos, python,
just, cargo-deny), then enable the git hook shims once per clone:

```sh
prek install
```

A clone that used the old `git config core.hooksPath .githooks` line must unset it first, or prek
writes the shims into that `core.hooksPath` directory instead of `.git/hooks/` and no gate runs on
commit or push:

```sh
git config --unset core.hooksPath
prek install
```

After upgrading prek or mise, re-run `prek install --force` to refresh the shim's binary path. A row
saying `not measured` with one sentence explaining why is acceptable; an invented number is
not. Do not use `git commit --no-verify` — the hook exists to catch forgetting, and bypassing it
silences the one check that keeps the performance file honest.

### Real graphs

When testing served freshness, use a large `--poll-interval` to prove notifications drive the
update, and cover `serve --no-watch` separately with a short period. Notifications coalesce for
50 ms, bounded at 500 ms; only full scans advance `last_scan` and clear divergence. Watcher setup
failure automatically uses polling. Use `--no-watch` on mounts with unreliable notifications.

`scripts/verify-real-graph.sh <graph-root>` and the optional `real_graph` tests read a named graph.
They work on a temporary copy and never write into the graph root. Do not commit changes Logseq
itself wrote into a graph you use for verification.

## Architecture changes

Start with the ADR index. An ADR states the target architecture, not the story of how it was
implemented; when a decision changes, amend the existing ADR rather than adding a competing one.
A schema change needs a version bump and a measured reason — `PERFORMANCE.md` is where the numbers
live.

## Style

Project documentation, source comments, identifiers, and commit messages use English. Commit
subjects name the area and the user-visible change (`docs: ...`, `feat(server): ...`), not the
implementation step. Graph content is arbitrary Unicode and is never normalized for presentation.

## Conduct

See `CODE_OF_CONDUCT.md`.
