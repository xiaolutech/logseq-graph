# Repository map

This file only maps repository locations. Architecture rules live in the linked ADRs.

| Location | Contents |
| --- | --- |
| `README.md` | Project status, purpose, and documentation entry points |
| `docs/adr/README.md` | ADR index and format |
| `docs/adr/` | Accepted architecture decisions, one decision per file |
| `docs/logseq-api-parity.md` | Official Logseq API ↔ the RPC reads, and the test pinning each row |
| `docs/corpus-parity.md` | Official graph parser ↔ this projection on a pinned public corpus, the normalization invariants, and every pinned difference |
| `docs/lint-policy.md` | Which lints are enabled and what each prevents, the suppression budget, and the rules that were measured and refused |
| `docs/engine-reference.md` | Detailed parsing, identity, write, RPC, hook, and verification contracts |
| `LESSONS.md` | Lessons accumulated in this repository |
| `PERFORMANCE.md` | Measured costs on real graphs, and what each change did to them |
| `HANDOFF.md` | Current implementation state and next slices in order |
| `LICENSE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md` | MIT licence, contribution gates, conduct standard |
| `crates/graph/` | Embeddable library: `document`, `source`, `index`, `domain`, `engine`; `examples/measure.rs` is what `bench.sh`'s engine phase runs |
| `crates/server/` | The `logseq-graph` CLI and its Logseq API adapter with reads and writes (`POST /api`); `examples/measure_serve.rs` is what `bench.sh`'s served phase runs |
| `fixtures/document/` | Document fixtures copied from an earlier implementation, plus their README |
| `fixtures/expected/` | Answers captured from the official Logseq API, plus their README |
| `fixtures/corpus/` | What the official graph parser answered for a pinned public corpus: one row per page, digests only |
| `scripts/` | `verify-real-graph.sh`, `bench.sh`, `check_perf_doc.py`, `capture_official_api.py`, `capture_corpus_parity.sh` and its `corpus_parity.cljs` program |

Start with the ADR index when changing architecture, read `LESSONS.md` before changing code, and
read `PERFORMANCE.md` before optimising anything. The prek pre-commit hook refuses a hot-path change
that carries no performance row; `prek.toml` is the only hook configuration, and the README says how
to install the shims.
