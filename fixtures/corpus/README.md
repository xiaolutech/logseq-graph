# Corpus fixtures

What the official Logseq graph parser answered for one pinned public corpus, captured so
`crates/graph/tests/corpus_parity.rs` can compare this repository's projection against it.

| Fixture | Captured from |
| --- | --- |
| `docs-v0.9.2.json` | `logseq/docs` at tag `v0.9.2`, parsed by `logseq/og` at commit `6e7afa8e` running `logseq.graph-parser.cli/parse-graph` |

One row per page: graph-relative path, the name the official parser answers, the number of content
blocks, and a digest over those blocks' texts. The `source` member records both revisions, the entry
point, the digest definition, and the two normalization rules.

The fixture holds digests, not texts, because the corpus is AGPL-3.0 and this repository is MIT:
what is committed here is this repository's own computation over a public corpus. To see what a
pinned page actually differs on, run the capture into a scratch file outside the tree.

`scripts/capture_corpus_parity.sh` reproduces the file byte for byte from the two revisions above.
`docs/corpus-parity.md` states what is compared, which differences are pinned, and why.
