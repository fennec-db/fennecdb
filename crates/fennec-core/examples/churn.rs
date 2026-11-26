//! A vector collection written over, and what its graph's tombstones cost:
//! `cargo run --release -p fennec-core --example churn -- [ROWS] [UPDATES] [DIM] [rebuild|none] [RATE] [LOOK_MS]`
//!
//! ROWS documents of a DIM vector under HNSW -- 20 000 x 16 by default, the
//! P6 soak's `docs`, whose 20 000 rows carried 490 000 tombstones by its
//! fifth hour -- then UPDATES `put`s (500 000), each giving a row a new
//! vector, a row at a time in turn. With `rebuild` a thread beside the
//! writer looks every LOOK_MS (5 000, a server's keeper's pace) and runs
//! `Database::rebuild_worn` where one is due, as the keeper does, timing
//! each rebuild and its holds of the lock; with `none` nothing rebuilds, as
//! before it. The writer puts RATE updates a second (0: as fast as it can).
//! Said at the end, and at the most during the run: the graph's nodes and
//! tombstones, its heap (`graph_bytes` and `arena_bytes`) and its record's
//! length (`serialize_graph`).

use fennec_core::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// A row's vector, the `gen`th written: a hash of the two, in [-1, 1).
fn vector(id: u64, gen: u64, dim: usize) -> Vec<f32> {
    let mut x = id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ gen.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    (0..dim)
        .map(|_| {
            x ^= x >> 33;
            x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
            x ^= x >> 33;
            (x >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        })
        .collect()
}

fn put(id: u64, v: Vec<f32>) -> Statement {
    Statement::Put {
        collection: "docs".into(),
        docs: vec![vec![
            ("id".into(), Expr::Lit(Value::Int(id as i64))),
            ("v".into(), Expr::Lit(Value::Vector(v))),
        ]],
        docs_param: None,
        insert: false,
        if_absent: false,
        else_set: None,
        require: None,
        pick: None,
    }
}

/// The graph as it stands: nodes, tombstones, heap bytes, record bytes.
#[derive(Clone, Copy, Default)]
struct Graph {
    nodes: usize,
    dead: usize,
    heap: usize,
    record: usize,
}

fn graph(db: &Database, record: bool) -> Graph {
    let ix = &db.collection("docs").expect("docs").vectors["v"];
    Graph {
        nodes: ix.nodes(),
        dead: ix.dead(),
        heap: ix.graph_bytes() + ix.arena_bytes(),
        record: if record {
            ix.serialize_graph().len()
        } else {
            0
        },
    }
}

fn said(g: Graph) -> String {
    format!(
        "{} nodes ({} tombstones), heap {:.1} MB, record {:.1} MB",
        g.nodes,
        g.dead,
        g.heap as f64 / 1e6,
        g.record as f64 / 1e6
    )
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize, or: u64| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(or);
    let rows = arg(1, 20_000);
    let updates = arg(2, 500_000);
    let dim = arg(3, 16) as usize;
    let rebuild = args.get(4).is_none_or(|m| m != "none");
    let rate = arg(5, 0);
    let look = Duration::from_millis(arg(6, 5_000));

    let mut db = Database::new();
    db.execute(
        &fennec_ql::parse_one(&format!(
            "create collection docs (v vector<{dim}> @hnsw(cosine))"
        ))
        .expect("create"),
    )
    .expect("create");
    for id in 1..=rows {
        db.execute(&put(id, vector(id, 0, dim))).expect("put");
    }
    println!(
        "{rows} x {dim}, {updates} updates, {} at {}",
        if rebuild { "rebuilt" } else { "never rebuilt" },
        match rate {
            0 => "full speed".to_string(),
            r => format!("{r} a second"),
        }
    );
    println!("loaded: {}", said(graph(&db, true)));
    let db = Arc::new(RwLock::new(db));
    let done = Arc::new(AtomicBool::new(false));
    // The keeper beside the writer, and the most the graph held at a look.
    let rebuilds = Arc::new(Mutex::new(Vec::new()));
    let most = Arc::new(Mutex::new(Graph::default()));
    let keeper = {
        let (db, done, rebuilds, most) = (
            Arc::clone(&db),
            Arc::clone(&done),
            Arc::clone(&rebuilds),
            Arc::clone(&most),
        );
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(look);
                let now = graph(&db.read().expect("lock"), true);
                {
                    let mut m = most.lock().expect("most");
                    if now.nodes > m.nodes {
                        *m = now;
                    }
                }
                if !rebuild {
                    continue;
                }
                let t = Instant::now();
                if let Some((r, holds)) = Database::rebuild_worn(&db) {
                    let took = t.elapsed();
                    let r = r.expect("rebuilt");
                    let after = graph(&db.read().expect("lock"), false);
                    let kept: String = holds.kept.iter().map(|k| format!("; {k}")).collect();
                    let held = ms(holds.longest());
                    println!(
                        "  {r:?} in {:.0} ms, the lock held {held:.2} ms at most ({holds}){kept}",
                        ms(took)
                    );
                    println!("    now {}", said(after));
                    rebuilds.lock().expect("rebuilds").push((took, holds));
                }
            }
        })
    };
    let t0 = Instant::now();
    let mut slowest = Duration::ZERO;
    for n in 0..updates {
        let id = 1 + n % rows;
        let stmt = put(id, vector(id, 1 + n / rows, dim));
        let t = Instant::now();
        db.write().expect("lock").execute(&stmt).expect("put");
        slowest = slowest.max(t.elapsed());
        if rate > 0 {
            let due = t0 + Duration::from_secs_f64((n + 1) as f64 / rate as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        if (n + 1) % 100_000 == 0 {
            println!(
                "{} updates in {:.1} s: {}",
                n + 1,
                t0.elapsed().as_secs_f64(),
                said(graph(&db.read().expect("lock"), false))
            );
        }
    }
    let wrote = t0.elapsed();
    done.store(true, Ordering::Relaxed);
    keeper.join().expect("keeper");
    let end = graph(&db.read().expect("lock"), true);
    let most = *most.lock().expect("most");
    let rebuilds = rebuilds.lock().expect("rebuilds");
    println!(
        "{updates} updates in {:.1} s, the slowest {:.2} ms",
        wrote.as_secs_f64(),
        ms(slowest)
    );
    println!("at the end: {}", said(end));
    println!("at the most a look found: {}", said(most));
    if !rebuilds.is_empty() {
        let mut took: Vec<f64> = rebuilds.iter().map(|(t, _)| ms(*t)).collect();
        let mut held: Vec<f64> = rebuilds.iter().map(|(_, h)| ms(h.longest())).collect();
        took.sort_by(f64::total_cmp);
        held.sort_by(f64::total_cmp);
        let kept = rebuilds.iter().filter(|(_, h)| !h.kept.is_empty()).count();
        println!(
            "{} rebuilds ({kept} kept a graph): each {:.0} ms p50, {:.0} max;",
            rebuilds.len(),
            took[took.len() / 2],
            took[took.len() - 1],
        );
        println!(
            "  the lock held {:.2} ms p50, {:.2} max",
            held[held.len() / 2],
            held[held.len() - 1]
        );
    }
}
