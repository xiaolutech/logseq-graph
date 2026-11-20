#!/usr/bin/env bash
#
# Verify the engine against a real Logseq graph and print the measured evidence.
#
# Usage:
#   scripts/verify-real-graph.sh <graph-root>
#   LOGSEQ_GRAPH_REAL=<graph-root> scripts/verify-real-graph.sh
#
# The graph is only read. Cold and warm start run against it directly, with the derived index in a
# temporary directory. The incremental phases change files, so they run on a temporary copy of the
# graph root; both temporary directories are removed by the test afterwards. The script also reports
# the storage cost of the projected block text and one bounded search measurement. The script is safe
# to repeat and leaves no state behind.
#
# Exit status: 0 all checks passed, 1 a check failed, 2 usage error.
set -uo pipefail

graph="${1:-${LOGSEQ_GRAPH_REAL:-}}"
if [[ -z "$graph" ]]; then
    echo "error: no graph root given" >&2
    echo "usage: scripts/verify-real-graph.sh <graph-root>" >&2
    echo "   or: LOGSEQ_GRAPH_REAL=<graph-root> scripts/verify-real-graph.sh" >&2
    exit 2
fi
if [[ ! -d "$graph" ]]; then
    echo "error: graph root is not a directory: $graph" >&2
    exit 2
fi
# Resolve the graph to an absolute path before the cd below: the test process runs from the repo
# root, not from the caller's cwd, so a path relative to the caller would not resolve there.
graph="$(cd "$graph" && pwd)" || exit 2

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

# --test-threads=1: the two tests measure cold and warm start, and running them in parallel makes
# each other's timings noise. Release build: a debug binary's numbers are not the numbers a user of
# the library would see, so evidence from it would not describe the engine.
output="$(LOGSEQ_GRAPH_REAL="$graph" cargo test --release -p logseq-graph --test real_graph -- --test-threads=1 --nocapture 2>&1)"
status=$?
# With --test-threads=1 libtest prints `test <name> ... ` on the same line as the test's first
# output, so the discovery and sandbox evidence lines do not start a line of their own. Take each
# line's REAL_GRAPH field run instead of anchoring to the start of the line.
evidence="$(printf '%s\n' "$output" | grep -o 'REAL_GRAPH .*')"

if [[ $status -ne 0 ]]; then
    printf '%s\n' "$evidence"
    echo "FAIL: real-graph verification failed"
    printf '%s\n' "$output" |
        grep -E '^(error|thread .* panicked|test result:)|assertion' |
        head -5
    exit 1
fi

if [[ -z "$evidence" ]]; then
    echo "error: the test binary produced no REAL_GRAPH evidence" >&2
    printf '%s\n' "$output" | tail -20 >&2
    exit 1
fi

printf '%s\n' "$evidence"
echo

# Pull one field out of the `REAL_GRAPH <phase> key=value ...` evidence line.
field() {
    printf '%s\n' "$evidence" |
        awk -v phase="$1" -v key="$2" '
            $2 == phase {
                for (i = 1; i <= NF; i++) {
                    split($i, pair, "=")
                    if (pair[1] == key) {
                        print pair[2]
                        exit
                    }
                }
            }'
}

pages="$(field discovery pages)"
journals="$(field discovery journals)"
files="$(field roundtrip files)"
cold_pages="$(field cold_start pages)"
blocks="$(field cold_start blocks)"
revision="$(field cold_start revision)"
cold_ms="$(field cold_start elapsed_ms)"
warm_revision="$(field warm_start revision)"
warm_ms="$(field warm_start elapsed_ms)"

sandbox_files="$(field sandbox files)"
sandbox_pages="$(field sandbox pages)"

edit_page="$(field edit page)"
edit_blocks="$(field edit block_count)"
edit_blocks_after="$(field edit block_count_after)"
edit_revision_before="$(field edit revision_before)"
edit_revision_after="$(field edit revision_after)"
edit_ms="$(field edit elapsed_ms)"
add_page="$(field add page)"
add_pages="$(field add pages)"
add_ms="$(field add elapsed_ms)"
delete_page="$(field delete page)"
delete_pages="$(field delete pages)"
delete_ms="$(field delete elapsed_ms)"
rename_from="$(field rename file_from)"
rename_to="$(field rename file_to)"
rename_page_from="$(field rename page_from)"
rename_page_to="$(field rename page_to)"
rename_pages="$(field rename pages)"
rename_ms="$(field rename elapsed_ms)"
touch_page="$(field touch page)"
touch_revision="$(field touch revision_after)"
touch_pages="$(field touch pages)"
touch_ms="$(field touch elapsed_ms)"

text_bytes="$(field volume text_bytes)"
index_bytes="$(field volume index_bytes)"
textless_bytes="$(field volume textless_index_bytes)"
delta_bytes="$(field volume delta_bytes)"
search_hits="$(field search hits)"
search_us="$(field search elapsed_us)"
edit_search_preorder="$(field search_after_edit preorder)"
edit_search_us="$(field search_after_edit elapsed_us)"

untouched=$((sandbox_pages - 1))

echo "PASS: ${pages} pages + ${journals} journals (${files} files) read, parsed, and rendered back byte for byte;"
echo "      cold start projected ${cold_pages} pages / ${blocks} blocks at revision ${revision} in ${cold_ms} ms;"
if [[ "$delta_bytes" == "unavailable" ]]; then
    echo "      block text holds ${text_bytes} bytes; the index is ${index_bytes} bytes (sqlite3 unavailable, so the textless size is not measured);"
else
    echo "      block text holds ${text_bytes} bytes and adds ${delta_bytes} bytes to a ${index_bytes} byte index (${textless_bytes} bytes with every block text blanked) for ${blocks} blocks in ${files} files;"
fi
echo "      bounded search query=logseq limit=20 returned ${search_hits} hits in ${search_us} us;"
echo "      warm start kept revision ${warm_revision} (nothing reprojected) in ${warm_ms} ms;"
echo "      one re-scan per change on a copy of ${sandbox_files} files (${sandbox_pages} pages):"
echo "        edit ${edit_page}: ${edit_blocks} -> ${edit_blocks_after} blocks, revision ${edit_revision_before} -> ${edit_revision_after},"
echo "             other ${untouched} pages kept their revision, page count unchanged (${edit_ms} ms);"
echo "             the appended block was searchable at preorder ${edit_search_preorder} in ${edit_search_us} us;"
echo "        add ${add_page}: page count ${sandbox_pages} -> ${add_pages} (${add_ms} ms);"
echo "        delete ${delete_page}: page count ${add_pages} -> ${delete_pages}, no readable page left (${delete_ms} ms);"
echo "        rename ${rename_from} -> ${rename_to}: ${rename_page_from} -> ${rename_page_to}, page count ${rename_pages} (${rename_ms} ms);"
echo "        touch ${touch_page}: revision stayed ${touch_revision}, page count ${touch_pages}, every page revision unchanged (${touch_ms} ms)."
exit 0
