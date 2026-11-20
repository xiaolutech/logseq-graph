# ADR-0015: Page identity includes derived entities

Status: Accepted
Date: 2026-11-19

## Context

Logseq lists tag pages, alias pages, namespace parents and unfiled link targets alongside
pages with files. A reader should not have to create a file to inspect one of those pages or
its linked references. This supersedes the file-only page identity interpretation of
ADR-0001 and ADR-0002, not Markdown's ownership of content or SQLite's disposability.

## Decision

- Identity is the folded page name. A leading `title::` or front-matter title takes
  precedence over the decoded file name. The old filename is not an implicit alias. A
  journal retains the date determined from its path.
- The page's pre-block supplies its page properties. Properties inside a dash block remain
  block properties, including inside the first dash block. A pre-block does not acquire
  a content-block position or selector in this projection.
- Tags, page refs, alias names and namespace parents can claim a page entity without a
  file. Claims are derived from their declaring file and are replaced or removed with it.
  A file-backed page takes precedence over any claim to the same name. Two file-backed
  pages claiming the same folded name are a conflict, not an implicit merge.
- An alias is a named entity and an edge. Linked references use the connected alias
  names, deduplicated by referencing page. An alias does not borrow another entity's
  file or block tree. Creating a file at a no-file name materializes that page.
- Page and namespace reads include derived entities. File versions, snapshots and edits
  still require a file. A pre-block backlink is labeled as such rather than inventing
  a usable block selector.

## Consequences

Deleting the index still loses no facts. A rebuild rederives all names and edges from
the files. An entity disappears when neither a file nor any remaining claim names it.
The RPC page shapes retain their existing fields and add `has_file`; callers cannot
infer that a page is missing from its zero block count.
