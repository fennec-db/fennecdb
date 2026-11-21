//! A seed in parts against the native core, beside what the scenario file
//! holds both runners to: each part but the last kept in the replica's own
//! file (`_sync_seed`) as it came, the shape read between parts as it
//! stood, the last landing them all in one record, a stream dropped
//! mid-seed leaving none of them behind, and a replica closed mid-seed
//! sweeping what it kept as it opens again.

use fennec_abi::sync::Sync;
use fennec_core::json;
use fennec_core::prelude::*;
use std::path::{Path, PathBuf};

const CONFIG: &str = r#"{"url":"http://server","token":"t0","seed":"0123456789abcdef0123456789abcdef","shapes":[{"collection":"tasks","key":"key"}]}"#;

const COLLECTIONS: &str = r#"[{"name":"tasks","fields":[{"name":"key","type":"text","index":"unique","required":false},{"name":"title","type":"text","index":null,"required":false}]}]"#;

/// A replica in a file, its sync, and the stream its shape has open.
struct Replica {
    db: Database,
    sync: Sync,
    stream: Option<u64>,
    timers: Vec<u64>,
}

impl Replica {
    fn open(path: &Path) -> Replica {
        let mut db = fennec_core::fs::open(path).unwrap();
        let sync = Sync::start(&mut db, CONFIG).unwrap();
        let mut r = Replica {
            db,
            sync,
            stream: None,
            timers: Vec::new(),
        };
        r.settle();
        r
    }

    /// Every action due performed: `/collections` answered, a stream
    /// opened, the timers noted.
    fn settle(&mut self) {
        loop {
            let Value::List(acts) = json::parse_json(&self.sync.actions()).unwrap() else {
                unreachable!()
            };
            if acts.is_empty() {
                return;
            }
            for a in acts {
                let get = |k: &str| fennec_abi::sync::shape::member(&a, k).cloned();
                let id = match get("id") {
                    Some(Value::Int(n)) => n as u64,
                    _ => 0,
                };
                match get("do") {
                    Some(Value::Text(d)) if d == "request" => {
                        self.sync.response(&mut self.db, id, 200, 0, COLLECTIONS)
                    }
                    Some(Value::Text(d)) if d == "stream" => {
                        self.stream = Some(id);
                        self.sync.opened(&mut self.db, id, 200, "");
                    }
                    Some(Value::Text(d)) if d == "cancel" => {
                        self.stream = self.stream.filter(|s| *s != id);
                    }
                    Some(Value::Text(d)) if d == "wait" => self.timers.push(id),
                    _ => {}
                }
            }
        }
    }

    /// A part of a seed on the open stream, cut as a network cuts it.
    fn part(&mut self, seq: u64, part: u64, rows: &[(i64, &str)], more: bool) {
        let mut data = format!("{{\"seq\":{seq}");
        if part > 0 {
            data.push_str(&format!(",\"part\":{part}"));
        }
        data.push_str(",\"rows\":[");
        for (i, (id, title)) in rows.iter().enumerate() {
            if i > 0 {
                data.push(',');
            }
            data.push_str(&format!(
                "{{\"id\":{id},\"key\":\"k{id}\",\"title\":\"{title}\"}}"
            ));
        }
        data.push(']');
        if more {
            data.push_str(",\"more\":true");
        }
        data.push('}');
        let id = self.stream.expect("a stream open");
        let bytes = format!("event: seed\ndata: {data}\n\n").into_bytes();
        for piece in bytes.chunks(5) {
            self.sync.bytes(&mut self.db, id, piece);
        }
        self.settle();
    }

    fn rows(&self, sql: &str) -> Vec<Row> {
        match self.db.query(&fennec_ql::parse_one(sql).unwrap(), &[]) {
            Ok(Response::Rows(rs)) => rs.rows,
            _ => Vec::new(),
        }
    }

    fn titles(&self) -> Vec<String> {
        (self.rows("get tasks select title order id").iter())
            .map(|r| match &r.values[0] {
                Value::Text(t) => t.clone(),
                _ => String::new(),
            })
            .collect()
    }

    /// The parts the file keeps aside.
    fn kept(&self) -> usize {
        self.rows("get _sync_seed select id").len()
    }

    fn drop_stream(&mut self) {
        let id = self.stream.take().expect("a stream open");
        self.sync.closed(&mut self.db, id, "connection reset");
        self.settle();
    }

    fn fire(&mut self) {
        for id in std::mem::take(&mut self.timers) {
            self.sync.timer(&mut self.db, id);
        }
        self.settle();
    }
}

fn file(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("fennec-parts-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("replica.fennec");
    (dir, path)
}

/// Each part but the last is a row of `_sync_seed` in the file, and the
/// shape reads as it stood until the last lands them all at once, the
/// parts gone with it.
#[test]
fn a_seed_s_parts_wait_in_the_file_and_land_in_one_block() {
    let (dir, path) = file("land");
    let mut r = Replica::open(&path);
    r.part(3, 0, &[(1, "one"), (2, "two")], false);
    assert_eq!(r.titles(), ["one", "two"]);

    // Seeded again in parts, as a server reseeds a stream past its ring.
    r.drop_stream();
    r.fire();
    r.part(9, 0, &[(1, "ONE")], true);
    r.part(9, 1, &[(4, "four")], true);
    assert_eq!(r.kept(), 2, "the parts are the file's");
    assert_eq!(r.titles(), ["one", "two"], "a read between parts");
    r.part(9, 2, &[(5, "five")], false);
    assert_eq!(r.titles(), ["ONE", "four", "five"]);
    assert_eq!(r.kept(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stream that drops mid-seed leaves the shape as it was and nothing of
/// the seed in the file; one whose next part is not the next is ended,
/// and seeds again from its start.
#[test]
fn a_seed_cut_short_leaves_nothing_behind() {
    let (dir, path) = file("cut");
    let mut r = Replica::open(&path);
    r.part(3, 0, &[(1, "one")], false);
    r.drop_stream();
    r.fire();
    r.part(9, 0, &[(1, "ONE")], true);
    r.part(9, 1, &[(2, "two")], true);
    assert_eq!(r.kept(), 2);
    r.drop_stream();
    assert_eq!(r.kept(), 0, "swept as the stream dropped");
    assert_eq!(r.titles(), ["one"]);

    r.fire();
    r.part(11, 0, &[(1, "uno")], true);
    // Part 2 where part 1 was due: the stream ends, its parts go.
    r.part(11, 2, &[(3, "three")], false);
    assert!(r.stream.is_none(), "the stream ended");
    r.fire();
    assert_eq!(r.kept(), 0);
    assert_eq!(r.titles(), ["one"]);
    r.part(12, 0, &[(1, "uno")], false);
    assert_eq!(r.titles(), ["uno"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A replica closed mid-seed keeps its parts in the file, as a crash
/// would; opened again it sweeps them before its stream opens, and the
/// shape is seeded again from its start.
#[test]
fn a_replica_opened_again_sweeps_the_parts_it_kept() {
    let (dir, path) = file("reopen");
    let mut r = Replica::open(&path);
    r.part(3, 0, &[(1, "one")], false);
    r.drop_stream();
    r.fire();
    r.part(9, 0, &[(1, "ONE")], true);
    assert_eq!(r.kept(), 1);
    let Replica { mut db, .. } = r;
    db.sync().unwrap();
    drop(db);

    let mut r = Replica::open(&path);
    assert_eq!(r.kept(), 0);
    assert_eq!(r.titles(), ["one"]);
    r.part(10, 0, &[(1, "ONE"), (2, "two")], false);
    assert_eq!(r.titles(), ["ONE", "two"]);
    let _ = std::fs::remove_dir_all(&dir);
}
