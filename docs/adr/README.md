# Architecture decisions

ADRs record the accepted target architecture. Each ADR covers one cohesive decision and fits on a
small number of screens. Implementation status belongs in the project README, not in these target
decisions.

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-markdown-files-own-product-facts.md) | Markdown files own all product facts | Accepted |
| [0002](0002-sqlite-is-the-derived-read-model.md) | SQLite is the sole derived read model | Accepted |
| [0003](0003-code-is-organized-around-five-boundaries.md) | Code is organized around five explicit boundaries | Accepted |
| [0004](0004-writes-commit-files-before-replacing-index-pages.md) | Writes commit files before replacing index pages | Accepted |
| [0005](0005-block-identity-is-explicitly-durable-or-ephemeral.md) | Block identity is explicitly durable or ephemeral | Accepted |
| [0006](0006-read-freshness-is-an-explicit-policy.md) | Read freshness is an explicit policy | Accepted |
| [0007](0007-block-text-is-derived-searchable-state.md) | Block text is derived searchable state | Accepted; search execution and ordering superseded by 0016 |
| [0008](0008-serving-freshness-in-a-long-lived-process.md) | Serving freshness in a long-lived process | Accepted |
| [0009](0009-ephemeral-block-identity-at-an-api-boundary.md) | Ephemeral block identity at an API boundary | Accepted |
| [0010](0010-concurrent-edits-are-resolved-by-the-sync-layer.md) | Concurrent edits are resolved by the sync layer, not the library | Accepted |
| [0011](0011-a-commit-is-a-versioned-batch-of-positional-operations.md) | A commit is a versioned batch of positional operations | Accepted |
| [0012](0012-ephemeral-block-identity-is-projection-scoped.md) | Ephemeral block identity is projection-scoped | Accepted |
| [0013](0013-simple-queries-are-executed-by-the-index.md) | Simple queries are executed by the index | Accepted |
| [0014](0014-served-writes-preserve-file-versioned-commit-contracts.md) | Served writes preserve file-versioned commit contracts | Accepted |
| [0015](0015-page-identity-includes-derived-entities.md) | Page identity includes derived entities | Accepted |
| [0016](0016-content-search-uses-a-folded-trigram-fts5-index.md) | Content search uses folded FTS5 indexes | Accepted |
| [0017](0017-concurrent-reads-use-a-bounded-connection-pool.md) | Concurrent reads use a bounded connection pool | Accepted |
| [0018](0018-the-served-api-requires-a-bearer-token.md) | The served API requires a bearer token, like the official one | Accepted |

## Format

Use this structure for new decisions:

```markdown
# ADR-NNNN: Short decision title

Status: Accepted | Rejected | Superseded
Date: YYYY-MM-DD

## Context

Only the forces needed to understand this decision.

## Decision

Checkable statements that an implementation either satisfies or violates.

## Consequences

The benefits, costs, and deliberate limitations.
```

Do not add migration history or implementation narration to an ADR. Amend the existing decision
when the target changes; use a superseding ADR only when the decision itself is replaced.
