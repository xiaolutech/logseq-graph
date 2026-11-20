// #5 getPageBlocksTree / #2 getAllPages: deliberate-difference, ADR-0008 background freshness.
use super::*;

fn eventually(server: &Server, method: &str, args: Value, predicate: impl Fn(&Value) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = server.call(method, args.clone());
        if response.status == 200 {
            let value = response.json();
            if predicate(&value) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "background refresh timed out: {value}"
            );
        } else {
            assert!(
                Instant::now() < deadline,
                "background refresh timed out: {}",
                response.body
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn exercise(no_watch: bool) {
    let label = if no_watch { "polling" } else { "notifications" };
    let sandbox = Sandbox::new(label);
    let server = Server::spawn_with_mode(
        &sandbox.graph_root(),
        label,
        if no_watch { 25 } else { 600_000 },
        no_watch,
    );
    server.wait_until_ready(label);
    sandbox.write("pages/alpha.md", "- external edit\n");
    eventually(&server, METHODS[4], json!(["alpha"]), |v| {
        v.to_string().contains("external edit")
    });
    sandbox.write("pages/new-page.md", "- external creation\n");
    eventually(&server, METHODS[4], json!(["new-page"]), |v| {
        v.to_string().contains("external creation")
    });
    fs::remove_file(sandbox.graph_root().join("pages/new-page.md")).expect("delete page");
    eventually(&server, METHODS[1], json!([]), |v| {
        !v.to_string().contains("new-page")
    });
    sandbox.write(
        "logseq/config.edn",
        "{:journal/page-title-format \"yyyy-MM-dd\"}\n",
    );
    eventually(&server, METHODS[1], json!([]), |v| {
        v.to_string().contains("2026-08-13") && !v.to_string().contains("Aug 13th, 2026")
    });
    let (mode, cause) = if no_watch {
        ("poll", "poll")
    } else {
        ("notify", "structural")
    };
    eventually(&server, METHODS[0], json!([]), |v| {
        let watch = &v["watch"];
        watch["mode"] == json!(mode)
            && watch["full_rescans_by_cause"][cause].as_u64() >= Some(1)
            && watch["full_rescans"].as_u64() >= watch["full_rescans_by_cause"][cause].as_u64()
    });
}

#[test]
fn default_reads_observe_external_changes_without_polling() {
    exercise(false);
}

#[test]
fn no_watch_keeps_polling_available() {
    exercise(true);
}
