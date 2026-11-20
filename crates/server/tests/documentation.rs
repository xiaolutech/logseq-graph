//! Contributor entry points must describe the adapter's write surface.

#[test]
fn contributor_entry_points_describe_rpc_reads_and_writes() {
    for document in [
        include_str!("../../../CONTRIBUTING.md"),
        include_str!("../../../AGENTS.md"),
    ] {
        assert!(document.contains("Logseq API adapter with reads and writes"));
        assert!(!document.contains("JSON-RPC adapter"));
    }
}
