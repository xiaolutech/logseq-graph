/// One line that states the grammar, printed with every usage error.
pub(crate) const USAGE: &str =
    "usage: logseq-graph [--graph <dir>] [--index <path>] [--verify-content] \
                         [--stale] [--bind <addr>] [--api-token <token>] \
                         [--poll-interval <ms>] [--no-watch] <command> [args]";

/// Help text printed for `--help`.
pub(crate) const HELP: &str = "\
usage: logseq-graph [--graph <dir>] [--index <path>] [--verify-content] [--stale] [--bind <addr>] <command> [args]

Reads a Logseq graph and prints the answer. `create`, `journal`, `commit`, `append`, `update`, `remove`,
and `move` are the commands that write, and each writes only the page file it names. The index lives
outside the graph either way.

options:
  -g, --graph <dir>   graph root; also read from LOGSEQ_GRAPH
      --index <path>  index database; by default a per-graph file in the cache directory
      --bind <addr>   the address `serve` listens on; loopback addresses only, and 127.0.0.1:8765
                      by default. Every other command refuses it
      --api-token <token>  a bearer token `serve` accepts, with no whitespace; repeatable, and also
                      read from LOGSEQ_API_TOKEN. `serve` refuses to start without one, and every
                      other command refuses this option
      --no-watch            force polling instead of filesystem notifications
      --poll-interval <ms>  polling / failed-scan retry period (default 1000)
      --verify-content  read and fingerprint every file before answering
      --stale         answer from the last scan even when it cannot be verified
      --today <day>   the day `today`, `yesterday`, and every offset of a query are measured
                      from, as YYYY-MM-DD; the system clock's day by default

commands:
  info                          graph and index paths, revision, page and block counts, index
                                size, observation time, staleness
  pages [--date]                every page and its block count; --date adds a third column, the
                                journal's day as YYYY-MM-DD and `-` for a page
  page <name>                   one page's metadata and properties
  tree <name>                   one page's blocks, two spaces of indent per level
  backlinks <name>              pages that reference <name>, with the referencing block
  tags                          every tag and how many pages use it
  tag <name>                    pages that use one tag
  search <query> [--limit <n>]  blocks whose text contains <query> (default 20); each hit leads
                                with the selector that names the block again
  find <name> [--limit <n>]     pages whose own name or alias:: value contains <name> (default 20);
                                page names come first, and an alias hit states the alias
  query <expr> [--limit <n>]    one Logseq simple query, e.g. '(task TODO)' or
                                '(and [[project]] (property status open))'; answers the blocks
                                that match, or the pages when the query is page-level, and
                                prints a selector for each block (default 20)
  block <selector>              one block by its selector
  serve [--bind <addr>] [--api-token <token>] [--poll-interval <ms>] [--no-watch]
                                serve the Logseq API shape on POST /api until killed; binds
                                127.0.0.1:8765 by default, requires every request to present a
                                bearer token (--api-token or LOGSEQ_API_TOKEN), answers one
                                {method, args} call per request, and each call names its own
                                freshness policy (last|check|verify, default last)
  create <page-name>            create an empty page under pages/ and print it
  journal <YYYY-MM-DD>          create that day's empty journal under journals/ and print it
  commit <page> <base-version> <operations-json>
                                apply one ordered batch and print its page, new version, and blocks;
                                the JSON array uses snake_case operation names and zero-based child
                                index paths; the accepted operation shapes are listed below
  append <page> <text>          append one block to one existing page, then print the page
  update <selector> <text>      replace one block's text, then print the block
  remove <selector>             delete one block and every block under it, then print the page
  ensure-block-id <selector>    persist durable identity; print id, created flag and block
  remove-range <first> <last>   delete an inclusive sibling run and its subtrees
  move <selector> before|after|first-child|last-child <anchor-selector>
                                move one block and everything under it next to the block that
                                <anchor-selector> names, then print the page; moves within or across
                                pages accept durable or ephemeral selectors at either end

A selector names one block and has two spellings, because two kinds of identity exist:
`durable:<uuid>` is a block that states `id::`, and `ephemeral:<uuid>` is a block that states none and
is named by the random id the index minted for it (ADR-0012). A bare `<uuid>` still means
`durable:<uuid>`, so every command line written before selectors existed keeps its meaning. A query
answer prints a selector for every block it matched, so its output can be copied straight into
`block`, `update`, `remove`, or `move`. An ephemeral selector names a block only while the page's
bytes are the ones the index projected: an edit to that page, or a rebuilt index, makes it answer
nothing rather than name a neighbour, and `--verify-content` is how a caller checks first.

`create` and `journal` write one new file and never replace an existing one. `append`, `update`,
`remove`, `move`, and `commit` reject --verify-content and --stale: a write resolves the one file it
edits rather than scanning the graph, so those flags describe no behaviour it could take. Exit code 3
means the file already holds the new text and the index is behind it; the next scan repairs the index.

The operations accepted by `commit` are `edit_text` {block,text}, `split_block` {block,offset},
`merge_block`, `unwrap_block`, `indent_block`, and `outdent_block` {block}; `insert_block` {after,text}
(with `after` null to append); `move_block` {block,target} where target is one of
{before,anchor}, {after,anchor}, {first_child_of,anchor}, or {last_child_of,anchor};
`set_collapsed` {block,collapsed}; and `set_marker` {block,marker} with a dash or one through
six heading marks. A stale base version or any refused operation writes nothing.

Journals are pages in the derived model: they are listed and counted like any other page, and every
page answer states its `kind` — `page` or `journal` — so a caller never has to decode a name, and a
journal also states its `date` as `YYYY-MM-DD`, which sorts chronologically where the title does not.
";
