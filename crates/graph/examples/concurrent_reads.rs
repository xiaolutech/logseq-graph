//! Compare the serialized request path with pooled reads on an already projected graph.
//!
//! `cargo run --release --example concurrent_reads -- <graph-root> <index-path> [query]`
//! Only the derived index is written. The graph files are never changed.

use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Instant;

use logseq_graph::engine::{Freshness, Graph, OpenOptions, SharedGraph};

const WORKERS: usize = 4;
const READS: usize = 40;

fn measure(label: &str, query: impl Fn() + Sync) {
    query();
    let barrier = Barrier::new(WORKERS + 1);
    let mut latencies = Vec::new();
    let elapsed = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..WORKERS {
            let query = &query;
            let barrier = &barrier;
            workers.push(scope.spawn(move || {
                barrier.wait();
                (0..READS)
                    .map(|_| {
                        let started = Instant::now();
                        query();
                        started.elapsed().as_secs_f64() * 1000.0
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let started = Instant::now();
        barrier.wait();
        for worker in workers {
            latencies.extend(worker.join().expect("read worker"));
        }
        started.elapsed().as_secs_f64()
    });
    latencies.sort_by(f64::total_cmp);
    println!("{label}: workers={WORKERS} reads={} wall_ms={:.2} reads_per_second={:.2} p50_ms={:.3} p95_ms={:.3}",
        latencies.len(), elapsed * 1000.0, latencies.len() as f64 / elapsed,
        latencies[latencies.len() / 2], latencies[latencies.len() * 95 / 100]);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 2 || args.len() > 3 {
        return Err("usage: concurrent_reads <graph-root> <index-path> [query]".into());
    }
    let options = OpenOptions::new(PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let needle = args
        .get(2)
        .map(|text| text.to_string_lossy().into_owned())
        .unwrap_or_else(|| "不存在".to_owned());
    let graph = Graph::open(options)?;
    let counts = Arc::new(Mutex::new(graph));
    {
        let pages = counts.lock().expect("graph").pages()?;
        let blocks: u64 = pages
            .value()
            .iter()
            .map(|page| u64::from(page.block_count()))
            .sum();
        println!(
            "pages={} blocks={blocks} query={needle:?} freshness=LastScan limit=20",
            pages.value().len()
        );
    }
    measure("serialized", || {
        std::hint::black_box(
            counts
                .lock()
                .expect("graph")
                .reads(Freshness::LastScan)
                .search(&needle, 20)
                .expect("search"),
        );
    });
    let graph = Arc::try_unwrap(counts)
        .expect("only graph owner")
        .into_inner()
        .expect("graph");
    let pooled = SharedGraph::new(graph)?;
    measure("pooled", || {
        std::hint::black_box(
            pooled
                .read(|reader| reader.default_reads().search(&needle, 20))
                .expect("search"),
        );
    });
    Ok(())
}
