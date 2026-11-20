# Document fixtures

These files are copied unmodified from the source graph of an earlier implementation of this
library, at commit `df1fd66`. They are behavioral samples, not a production dependency.

| Fixture | Source path |
| --- | --- |
| `predecessor-fixture-parser.md` | `fixtures/graph/pages/fixture-parser.md` |
| `predecessor-todo-query.md` | `fixtures/graph/pages/TODO.md` |
| `predecessor-journal.md` | `fixtures/graph/journals/2026_08_14.md` |

The first covers a page preamble, nested blocks, block continuation lines, a marker-started code
fence, and an ordered-list line that must not become a block. The second covers a `#+BEGIN_QUERY`
region and a file without a final newline. The third covers top-level blocks and tab-indented
children.

Encoding fixtures are generated from string literals in unit tests, so the repository does not
depend on checkout line-ending conversion for CRLF, BOM, or mixed-ending coverage.

Integration tests include these files at compile time through the `document_fixture!` macro in
`crates/graph/tests/support/mod.rs`. The macro anchors the path at `CARGO_MANIFEST_DIR`, so a test
names only the fixture file and never repeats a relative hop out of the test directory.
