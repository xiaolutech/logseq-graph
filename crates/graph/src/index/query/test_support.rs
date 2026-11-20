/// The `detail` column of `EXPLAIN QUERY PLAN` for `sql`.
///
/// The statements are the constants the reads use, so a rewritten query cannot drift away from
/// the plan this module asserts.
pub(super) fn query_plan(sql: &str) -> Vec<String> {
    let index = crate::index::Index::open_in_memory().expect("open index");
    let mut stmt = index
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare plan query");
    // Plans do not depend on the value, but every parameter still needs a binding.
    let binds = vec!["x"; stmt.parameter_count()];
    let mut rows = stmt
        .query(rusqlite::params_from_iter(binds))
        .expect("explain query");
    let mut steps = Vec::new();
    while let Some(row) = rows.next().expect("read plan row") {
        steps.push(row.get::<_, String>(3).expect("plan detail"));
    }
    steps
}
