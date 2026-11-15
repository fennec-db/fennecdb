//! `ycsb soak`: fennec-server in a container under hours of load, a line
//! of figures a minute (`ycsb/run-soak.sh` runs it; `ycsb soak report`
//! judges a run). Part of `ycsb` rather than a program of its own: its
//! records, its keys, its histograms and its way of holding an answer to
//! what was written are YCSB's (`check`), made to run for hours.
//!
//! The server runs in a container whose `--memory` is under its file's
//! size, so the mapped file's pages come and go; a second follows it as a
//! replica. The load is open loop (`--rate`, a schedule a connection, an
//! operation's time from when it was due: a stall counts the wait of every
//! one queued behind it), or as fast as answered for the peak (`--peak`).
//! Its mix, over a fixed set of connections:
//!
//!   40% read and 40% update of YCSB's records (A), its scrambled zipfian
//!    3% insert and 3% delete -- new keys, the oldest inserted deleted --
//!    2% read of an inserted key,
//!    4% insert into `sessions`, whose `seen @ttl` expires them, and 2%
//!       read of one: one past its time must be gone, one inside it there,
//!    2% `match` on `docs`' `@text` body, 2% `near` on its `@hnsw`
//!       vector, 2% update of a doc,
//!
//! beside 64 subscriptions (`GET /usertable/changes`, a range of keys
//! each) held open and read; `compact usertable` every
//! `--compact-every` minutes; a backup over the replication stream,
//! restored and verified, every `--backup-every`; and every
//! `--kill-every` minutes the server killed (`kill -9`), its file held to
//! its checksums (`fennec verify`) before it starts again.
//!
//! Every read and write is held to what was written as the run goes
//! (`Checker`): a read must answer, field by field, the value of a write
//! that could have been the last before it or one beside it, and a
//! record's being there or not likewise. Around a kill a write that was
//! not answered may or may not have landed, and under `--sync 250` one
//! answered in the `--loss-ms` before it may have been lost: either value
//! then holds until a write after the restart ends.
//!
//! A minute's line (`results/soak.tsv`, family `soak`, cell
//! `<sync>:<label>:m<minute>`): its operations and their p50, p99, p99.9
//! and longest, the p99 of the hour to then, errors, operations that met a
//! restart, the checker's mismatches, the file's size alone and the live
//! records' bytes, compactions so far, the server's anonymous memory
//! (`RssAnon`) and `memory_bytes`, its cgroup's anonymous and file pages,
//! its major faults, threads and descriptors, the replica's lag, the
//! subscriptions' events, the kills, verifies, backups and compacts the
//! minute held, and the host's load average -- the machine is shared, and a
//! busy minute must show. Appended as each minute ends: a run cut short
//! keeps what it measured. `<sync>:<label>:end` closes a finished run.

use super::http;
use super::{
    append_values, base_digests, digest, insert_text, json_rows, metric_sum, record_values, Cgroup,
    Hist, Rng, Scrambled, FIELDS, LOAD_BATCH, SEED,
};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A run's settings: `ycsb soak --help` lists them.
struct Opts {
    sync: String,
    rate: f64,
    peak: bool,
    label: String,
    minutes: u64,
    records: u64,
    docs: u64,
    dim: usize,
    memory: String,
    threads: usize,
    subscriptions: usize,
    kill_every: u64,
    compact_every: u64,
    backup_every: u64,
    ttl: u64,
    loss_ms: u64,
    out: PathBuf,
    run_id: String,
    image: String,
    tools: String,
    name: String,
}

const HELP: &str = "ycsb soak --sync 250|always (--rate <ops/s> [--label 50] | --peak)
    [--minutes 480] [--records 1000000] [--docs 20000] [--dim 16] [--memory 512m]
    [--threads 32] [--subscriptions 64] [--kill-every 30] [--compact-every 20]
    [--backup-every 120] [--ttl 60] [--loss-ms 2000] [--out soak.tsv]
    [--run-id <id>] [--image fennecdb-ycsb] [--tools p6-tools] [--name p6]
ycsb soak report <soak.tsv> [<run-id>]";

impl Opts {
    fn parse(args: &[String]) -> Opts {
        let mut o = Opts {
            sync: String::new(),
            rate: 0.0,
            peak: false,
            label: String::new(),
            minutes: 480,
            records: 1_000_000,
            docs: 20_000,
            dim: 16,
            memory: "512m".into(),
            threads: 32,
            subscriptions: 64,
            kill_every: 30,
            compact_every: 20,
            backup_every: 120,
            ttl: 60,
            loss_ms: 2_000,
            out: PathBuf::from("soak.tsv"),
            run_id: String::new(),
            image: "fennecdb-ycsb".into(),
            tools: "p6-tools".into(),
            name: "p6".into(),
        };
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--peak" {
                o.peak = true;
                i += 1;
                continue;
            }
            let v = args.get(i + 1).cloned().unwrap_or_default();
            let n = || {
                v.parse::<u64>()
                    .unwrap_or_else(|_| panic!("{}: {v}\n{HELP}", args[i]))
            };
            match args[i].as_str() {
                "--sync" => o.sync = v.clone(),
                "--rate" => o.rate = v.parse().expect("--rate <ops/s>"),
                "--label" => o.label = v.clone(),
                "--minutes" => o.minutes = n(),
                "--records" => o.records = n(),
                "--docs" => o.docs = n(),
                "--dim" => o.dim = n() as usize,
                "--memory" => o.memory = v.clone(),
                "--threads" => o.threads = n() as usize,
                "--subscriptions" => o.subscriptions = n() as usize,
                "--kill-every" => o.kill_every = n(),
                "--compact-every" => o.compact_every = n(),
                "--backup-every" => o.backup_every = n(),
                "--ttl" => o.ttl = n(),
                "--loss-ms" => o.loss_ms = n(),
                "--out" => o.out = PathBuf::from(&v),
                "--run-id" => o.run_id = v.clone(),
                "--image" => o.image = v.clone(),
                "--tools" => o.tools = v.clone(),
                "--name" => o.name = v.clone(),
                other => panic!("unknown argument {other}\n{HELP}"),
            }
            i += 2;
        }
        assert!(
            o.sync == "250" || o.sync == "always",
            "--sync 250 or --sync always\n{HELP}"
        );
        assert!(o.peak || o.rate > 0.0, "--rate <ops/s> or --peak\n{HELP}");
        assert!(
            o.records >= 1_000 && o.docs >= 100 && o.threads > 0,
            "{HELP}"
        );
        if o.peak {
            o.label = "peak".into();
            o.rate = 0.0;
            o.kill_every = 0;
            o.compact_every = 0;
            o.backup_every = 0;
        } else if o.label.is_empty() {
            o.label = format!("{:.0}", o.rate);
        }
        if o.run_id.is_empty() {
            o.run_id = format!("s{}-{}", o.sync, o.label);
        }
        // A write answered is on disk under `always`: only one in flight at
        // a kill may have been lost.
        if o.sync == "always" {
            o.loss_ms = 0;
        }
        o
    }
}

pub fn main(args: &[String]) {
    if args.first().map(String::as_str) == Some("report") {
        let path = args.get(1).expect("ycsb soak report <soak.tsv> [<run-id>]");
        std::process::exit(report(Path::new(path), args.get(2).map(String::as_str)));
    }
    if args.iter().any(|a| a == "--help") {
        println!("{HELP}");
        return;
    }
    run(&Opts::parse(args));
}

// ------------------------------------------------------------- containers

/// `docker <args>`: whether it succeeded, and what it printed.
fn docker(args: &[&str]) -> (bool, String) {
    match std::process::Command::new("docker").args(args).output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// The containers, volumes and network a run makes, every one named after
/// `--name`, and removed when it ends -- a panic's too.
struct Bench {
    name: String,
    image: String,
    tools: String,
    memory: String,
    sync: String,
    token: String,
    port: u16,
    replica_port: u16,
}

impl Bench {
    fn server(&self) -> String {
        format!("{}-server", self.name)
    }
    fn replica(&self) -> String {
        format!("{}-replica", self.name)
    }
    fn net(&self) -> String {
        format!("{}-net", self.name)
    }
    fn volume(&self, what: &str) -> String {
        format!("{}-{what}", self.name)
    }

    fn clean(&self) {
        for c in [
            self.server(),
            self.replica(),
            format!("{}-look", self.name),
            format!("{}-verify", self.name),
            format!("{}-backup", self.name),
        ] {
            docker(&["rm", "-f", &c]);
        }
        for v in ["data", "replica-data", "backup-data"] {
            docker(&["volume", "rm", "-f", &self.volume(v)]);
        }
        docker(&["network", "rm", &self.net()]);
    }

    /// `fennec-server` in a container on the run's network, its port on
    /// this machine's loopback, held to `--memory` with no swap past it.
    fn start(&self, container: &str, port: u16, volume: &str, args: &[&str]) {
        let (ok, out) = docker(
            &[
                &[
                    "run",
                    "-d",
                    "--name",
                    container,
                    "--network",
                    &self.net(),
                    "-p",
                    &format!("127.0.0.1:{port}:8080"),
                    "-v",
                    &format!("{volume}:/data"),
                    "--memory",
                    &self.memory,
                    "--memory-swap",
                    &self.memory,
                    &self.image,
                    "--http",
                    "0.0.0.0:8080",
                    "--insecure",
                    "--replication-token",
                    &self.token,
                ][..],
                args,
            ]
            .concat(),
        );
        assert!(ok, "docker run {container}: {out}");
    }

    fn start_primary(&self) {
        self.start(
            &self.server(),
            self.port,
            &self.volume("data"),
            &[
                "--file",
                "/data/soak.fennec",
                "--sync",
                &self.sync,
                // The subscriptions, the replica's stream, a backup's.
                "--http-max-streams",
                "128",
            ],
        );
    }

    fn start_replica(&self) {
        let primary = format!("http://{}:8080", self.server());
        self.start(
            &self.replica(),
            self.replica_port,
            &self.volume("replica-data"),
            &["--file", "/data/replica.fennec", "--replica-of", &primary],
        );
    }

    /// `fennec <args>` in a container holding the tools' volume
    /// (`ycsb/linux-tools.sh`) and `volume` at `/data`, on the run's
    /// network: whether it exited 0, and what it printed.
    fn fennec(&self, container: &str, volume: &str, script: &str) -> (bool, String) {
        docker(&[
            "run",
            "--rm",
            "--name",
            container,
            "--network",
            &self.net(),
            "-v",
            &format!("{}:/tools:ro", self.tools),
            "-v",
            &format!("{volume}:/data"),
            "alpine",
            "sh",
            "-c",
            script,
        ])
    }

    /// The primary's process as a shell in its process namespace sees it:
    /// its cgroup, its anonymous memory, threads, descriptors and major
    /// faults, and the database file's length alone.
    fn look(&self) -> Option<Look> {
        let script = "cd /proc/1/root/sys/fs/cgroup && cat memory.stat && \
            echo current $(cat memory.current) && grep oom_kill memory.events; \
            grep -E '^(Threads|RssAnon):' /proc/1/status; \
            echo fds $(ls /proc/1/fd | wc -l); \
            echo majflt $(cut -d' ' -f12 /proc/1/stat); \
            echo filesize $(stat -c %s /proc/1/root/data/soak.fennec)";
        let (ok, out) = docker(&[
            "run",
            "--rm",
            "--name",
            &format!("{}-look", self.name),
            &format!("--pid=container:{}", self.server()),
            // Reading another user's `/proc/<pid>/root` is a ptrace's right.
            "--cap-add",
            "SYS_PTRACE",
            "alpine",
            "sh",
            "-c",
            script,
        ]);
        if !ok {
            return None;
        }
        let num = |key: &str| -> Option<u64> {
            out.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        };
        Some(Look {
            cgroup: Cgroup::parse(&out).unwrap_or_default(),
            rss_anon: num("RssAnon:").map(|kb| kb << 10)?,
            threads: num("Threads:")?,
            fds: num("fds ")?,
            majflt: num("majflt ")?,
            file: num("filesize ")?,
        })
    }

    fn health(&self, port: u16) -> bool {
        let Ok(mut c) =
            http::Http::try_connect(&format!("127.0.0.1:{port}"), Duration::from_secs(5))
        else {
            return false;
        };
        matches!(
            c.try_request("GET", "/_health", "text/plain", b""),
            Ok((200, _))
        )
    }

    fn wait_health(&self, port: u16, within: Duration) {
        let until = Instant::now() + within;
        while !self.health(port) {
            assert!(
                Instant::now() < until,
                "fennec-server on {port} did not answer"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.clean();
    }
}

/// One look at the primary's process.
#[derive(Default, Clone, Copy)]
struct Look {
    cgroup: Cgroup,
    rss_anon: u64,
    threads: u64,
    fds: u64,
    majflt: u64,
    file: u64,
}

// ------------------------------------------------------------------- load

/// A doc's body: eight words of a 10 000-word vocabulary, the lower ones
/// likelier.
fn words(rng: &mut Rng, n: usize) -> String {
    let w: Vec<String> = (0..n)
        .map(|_| {
            let u = rng.f64();
            format!("w{}", (u * u * 10_000.0) as u64)
        })
        .collect();
    w.join(" ")
}

fn unit_vector(rng: &mut Rng, dim: usize) -> String {
    let v: Vec<f32> = (0..dim).map(|_| rng.f64() as f32 * 2.0 - 1.0).collect();
    http::vector_json(&v)
}

/// The collections made and filled, YCSB's records as `ycsb` loads them.
fn load(addr: &str, o: &Opts) {
    let mut c = http::Http::connect(addr);
    let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
    c.query(
        &format!("create collection usertable ({})", fields.join(", ")),
        "",
    );
    c.query(
        &format!(
            "create collection sessions (user int, seen timestamp @ttl({}s))",
            o.ttl
        ),
        "",
    );
    c.query(
        &format!(
            "create collection docs (body text @text, v vector<{}> @hnsw(cosine))",
            o.dim
        ),
        "",
    );
    let t = Instant::now();
    let mut body = String::new();
    let mut key = 1;
    while key <= o.records {
        let end = (key + LOAD_BATCH as u64).min(o.records + 1);
        body.clear();
        body.push('[');
        for k in key..end {
            if k > key {
                body.push(',');
            }
            write!(body, "{{\"id\":{k}").unwrap();
            for (i, v) in record_values(k).iter().enumerate() {
                write!(body, ",\"field{i}\":\"{v}\"").unwrap();
            }
            body.push('}');
        }
        body.push(']');
        c.post("/usertable", "application/json", body.as_bytes());
        key = end;
    }
    let mut rng = Rng::new(SEED ^ 0xD0C5);
    let mut key = 1;
    while key <= o.docs {
        let end = (key + LOAD_BATCH as u64).min(o.docs + 1);
        body.clear();
        body.push('[');
        for k in key..end {
            if k > key {
                body.push(',');
            }
            write!(
                body,
                "{{\"id\":{k},\"body\":\"{}\",\"v\":{}}}",
                words(&mut rng, 8),
                unit_vector(&mut rng, o.dim)
            )
            .unwrap();
        }
        body.push(']');
        c.post("/docs", "application/json", body.as_bytes());
        key = end;
    }
    eprintln!(
        "soak: {} records and {} docs loaded in {:.0} s",
        o.records,
        o.docs,
        t.elapsed().as_secs_f64()
    );
}

// ---------------------------------------------------------------- checker

/// The register a record's being there is kept in, after its fields'.
const EXISTS: u8 = FIELDS as u8;
/// A write's `field` for an insert of the whole record and for a delete.
const INSERT: u8 = FIELDS as u8;
const DELETE: u8 = FIELDS as u8 + 1;

/// An operation as a worker logged it, its span in ns from the run's
/// start.
#[derive(Clone, Debug)]
enum Ev {
    /// A read: whether it found the record, and its fields' digests.
    Read {
        key: u64,
        s: u64,
        e: u64,
        found: bool,
        got: [u64; FIELDS],
    },
    /// An update of one field (`d` its value's digest), an insert or a
    /// delete; `failed` where it was not answered -- landed or not.
    Write {
        key: u64,
        field: u8,
        d: u64,
        s: u64,
        e: u64,
        failed: bool,
        affected: u64,
    },
}

/// A write as a register holds it.
#[derive(Clone, Copy, Debug)]
struct Wr {
    s: u64,
    e: u64,
    d: u64,
    failed: bool,
}

/// `check`'s rule, run as the operations come rather than after a cell:
/// a read is held once every write that began before it ended has been
/// logged (`safe`), and a register keeps only the writes a later read can
/// still be answered from -- each field's settled value once one write is
/// all that can be.
struct Checker {
    regs: HashMap<(u64, u8), Vec<Wr>>,
    /// What a field holds where no write a read could see is kept, the
    /// record's presence after its fields; a key absent here holds what
    /// the load wrote (or, inserted since, nothing).
    settled: HashMap<u64, [u64; FIELDS + 1]>,
    pending: Vec<Ev>,
    /// The loaded records' keys run 1..=records; the inserted ones after.
    records: u64,
    /// Each kill and when the server answered again, in ns.
    kills: Vec<(u64, u64)>,
    /// How long before a kill an answered write may have been lost:
    /// `--sync 250`'s interval and its fsync, generously; 0 under always.
    loss: u64,
    reads: u64,
    writes: u64,
    mismatches: u64,
    examples: Vec<String>,
    base: std::cell::Cell<(u64, [u64; FIELDS])>,
}

impl Checker {
    fn new(records: u64, loss: u64) -> Checker {
        Checker {
            base: std::cell::Cell::new((u64::MAX, [0; FIELDS])),
            regs: HashMap::new(),
            settled: HashMap::new(),
            pending: Vec::new(),
            records,
            kills: Vec::new(),
            loss,
            reads: 0,
            writes: 0,
            mismatches: 0,
            examples: Vec::new(),
        }
    }

    fn wrong(&mut self, what: String) {
        self.mismatches += 1;
        if self.examples.len() < 20 {
            eprintln!("soak: mismatch: {what}");
            self.examples.push(what);
        }
    }

    /// Whether a write may not have landed: one not answered, or one
    /// answered in the `loss` before a kill, up to the restart.
    fn uncertain(&self, w: &Wr) -> bool {
        w.failed
            || (self.loss > 0
                && self
                    .kills
                    .iter()
                    .any(|&(k, back)| w.e + self.loss >= k && w.s <= back))
    }

    fn initial(&self, key: u64, f: u8) -> u64 {
        if let Some(v) = self.settled.get(&key) {
            return v[f as usize];
        }
        if f == EXISTS {
            return (key <= self.records) as u64;
        }
        // A read's fields are asked one after another: the last key's
        // values kept rather than drawn ten times.
        let (k, v) = self.base.get();
        if k == key {
            return v[f as usize];
        }
        let v = base_digests(key);
        self.base.set((key, v));
        v[f as usize]
    }

    fn push(&mut self, key: u64, f: u8, w: Wr) {
        self.regs.entry((key, f)).or_default().push(w);
    }

    /// Takes a worker's log in: writes into their registers, reads to be
    /// held once `safe` passes them.
    fn take(&mut self, evs: Vec<Ev>) {
        for ev in evs {
            match ev {
                Ev::Write {
                    key,
                    field,
                    d,
                    s,
                    e,
                    failed,
                    affected,
                } => {
                    self.writes += 1;
                    let w = Wr { s, e, d, failed };
                    match field {
                        INSERT => {
                            for (f, d) in base_digests(key).into_iter().enumerate() {
                                self.push(key, f as u8, Wr { d, ..w });
                            }
                            self.push(key, EXISTS, Wr { d: 1, ..w });
                        }
                        DELETE => self.push(key, EXISTS, Wr { d: 0, ..w }),
                        f => self.push(key, f, w),
                    }
                    // A delete may find nothing where its insert was lost
                    // at a kill: judged once that is known (`hold`).
                    if !failed && affected != 1 && !(field == DELETE && affected == 0) {
                        self.wrong(format!("a write of {key} answered {affected} records"));
                    }
                    if !failed && field == DELETE && affected == 0 {
                        self.pending.push(Ev::Write {
                            key,
                            field,
                            d,
                            s,
                            e,
                            failed,
                            affected,
                        });
                    }
                }
                read => self.pending.push(read),
            }
        }
    }

    /// Whether `d` is a value the register `(key, f)` can answer a read
    /// over `[s, e]` with: a write beside it, the last that ended before it
    /// -- every uncertain one after the last certain, and the value before
    /// them -- or, where none ended, the value it held.
    fn may_hold(&self, key: u64, f: u8, s: u64, e: u64, d: u64) -> bool {
        let reg = self.regs.get(&(key, f));
        let last_start = reg.and_then(|r| {
            r.iter()
                .filter(|w| w.e < s && !self.uncertain(w))
                .map(|w| w.s)
                .max()
        });
        if last_start.is_none() && self.initial(key, f) == d {
            return true;
        }
        reg.is_some_and(|r| {
            r.iter().any(|w| {
                w.d == d
                    && ((w.s <= e && w.e >= s) || (w.e < s && last_start.is_none_or(|m| m <= w.e)))
            })
        })
    }

    /// Holds every pending read that ended before `safe`, then lets go of
    /// what no read from `cutoff` on can be answered from.
    fn hold(&mut self, safe: u64, cutoff: u64) {
        let pending = std::mem::take(&mut self.pending);
        for ev in pending {
            match ev {
                Ev::Read {
                    key,
                    s,
                    e,
                    found,
                    got,
                } if e < safe => {
                    self.reads += 1;
                    if !self.may_hold(key, EXISTS, s, e, found as u64) {
                        let what = match found {
                            true => "found it",
                            false => "found nothing",
                        };
                        self.wrong(format!("a read of {key} [{s}, {e}] ns {what}"));
                        continue;
                    }
                    if !found {
                        continue;
                    }
                    for f in 0..FIELDS as u8 {
                        if !self.may_hold(key, f, s, e, got[f as usize]) {
                            self.wrong(format!(
                                "a read of {key} [{s}, {e}] ns: field{f} holds no value written to it"
                            ));
                            break;
                        }
                    }
                }
                Ev::Write { key, s, e, .. } if e < safe => {
                    // A delete that found nothing: its record may not have
                    // been there as it began -- the delete's own write
                    // aside, which a span ending before it never meets.
                    if !self.may_hold(key, EXISTS, s, s.saturating_sub(1), 0) {
                        self.wrong(format!("a delete of {key} found no record"));
                    }
                }
                ev => self.pending.push(ev),
            }
        }
        let cutoff = self
            .pending
            .iter()
            .map(|ev| match ev {
                Ev::Read { s, .. } | Ev::Write { s, .. } => *s,
            })
            .min()
            .unwrap_or(cutoff)
            .min(cutoff);
        let mut settle = Vec::new();
        let kills = self.kills.clone();
        let loss = self.loss;
        let uncertain = |w: &Wr| {
            w.failed
                || (loss > 0
                    && kills
                        .iter()
                        .any(|&(k, back)| w.e + loss >= k && w.s <= back))
        };
        self.regs.retain(|&(key, f), r| {
            let last = r
                .iter()
                .filter(|w| w.e < cutoff && !uncertain(w))
                .map(|w| w.s)
                .max();
            if let Some(m) = last {
                r.retain(|w| w.e >= m);
            }
            if r.len() == 1 && r[0].e < cutoff && !uncertain(&r[0]) {
                settle.push((key, f, r[0].d));
                return false;
            }
            true
        });
        for (key, f, d) in settle {
            let records = self.records;
            let v = self.settled.entry(key).or_insert_with(|| {
                let mut v = [0; FIELDS + 1];
                if key <= records {
                    v[..FIELDS].copy_from_slice(&base_digests(key));
                    v[FIELDS] = 1;
                } else {
                    v[..FIELDS].copy_from_slice(&base_digests(key));
                }
                v
            });
            v[f as usize] = d;
            // An inserted record deleted for good is read no more.
            if key > records && f == EXISTS && d == 0 {
                self.settled.remove(&key);
            }
        }
    }
}

// ---------------------------------------------------------------- workers

/// What the workers, the subscribers and the minute's look share.
struct Shared {
    t0: Instant,
    stop: AtomicBool,
    addr: String,
    rate: f64,
    threads: usize,
    records: u64,
    docs: u64,
    dim: usize,
    ttl: Duration,
    ops: AtomicU64,
    errors: AtomicU64,
    down: AtomicU64,
    ttl_wrong: AtomicU64,
    events: AtomicU64,
    streams: AtomicU64,
    /// Each kill and when the server answered again (`u64::MAX` until it
    /// has), in ns from `t0`.
    kills: Mutex<Vec<(u64, u64)>>,
    loss: u64,
    /// Inserted keys: the next to hand out, and those inserted and not yet
    /// deleted, oldest first.
    fifo: Mutex<(u64, VecDeque<u64>)>,
    sessions: Mutex<VecDeque<Session>>,
    next_session: AtomicU64,
    slots: Vec<Slot>,
    notes: Mutex<Vec<String>>,
}

/// A worker's own: the latencies since the minute's look took them, its
/// log since the checker did, and the start of what it has in flight.
struct Slot {
    hist: Mutex<Hist>,
    log: Mutex<Vec<Ev>>,
    inflight: AtomicU64,
}

#[derive(Clone, Copy)]
struct Session {
    id: u64,
    s: u64,
    acked: u64,
    failed: bool,
}

impl Shared {
    fn ns(&self) -> u64 {
        self.t0.elapsed().as_nanos() as u64
    }

    /// Whether `t` falls in a kill's window, from a moment before it to a
    /// few seconds after the server answered again.
    fn restarting(&self, t: u64) -> bool {
        let grace = 5_000_000_000;
        self.kills
            .lock()
            .unwrap()
            .iter()
            .any(|&(k, back)| t + 1_000_000_000 >= k && t <= back.saturating_add(grace))
    }

    fn note(&self, what: String) {
        let mut n = self.notes.lock().unwrap();
        if n.len() < 50 {
            eprintln!("soak: {what}");
            n.push(what);
        }
    }

    /// Counts a failed request as an error, or as one that met a restart.
    fn failed(&self, t: u64, what: impl FnOnce() -> String) {
        if self.restarting(t) {
            self.down.fetch_add(1, Ordering::Relaxed);
        } else {
            self.errors.fetch_add(1, Ordering::Relaxed);
            self.note(what());
        }
    }
}

/// What a request came to.
enum Answer {
    Ok(Vec<u8>),
    /// Refused by its status: counted already.
    Refused,
    /// Not answered -- the connection refused or cut: a write may or may
    /// not have landed.
    Unknown,
    Stopped,
}

struct Worker<'a> {
    sh: &'a Shared,
    slot: &'a Slot,
    c: Option<http::Http>,
    rng: Rng,
}

impl Worker<'_> {
    /// One request, sent again where it reads (`again`) until answered:
    /// across a restart a read waits for the server, its time counting the
    /// wait, and a write is not sent twice.
    fn ask(&mut self, text: &str, params: &str, again: bool) -> Answer {
        let mut counted = false;
        loop {
            if self.sh.stop.load(Ordering::Relaxed) {
                return Answer::Stopped;
            }
            if self.c.is_none() {
                self.c = http::Http::try_connect(&self.sh.addr, Duration::from_secs(60)).ok();
            }
            let got = match self.c.as_mut() {
                Some(c) => c
                    .try_query(text, params)
                    .map(|(status, body)| (status, body.to_vec())),
                None => Err(std::io::Error::other("no connection")),
            };
            match got {
                Ok((status, body)) if status < 300 => return Answer::Ok(body),
                Ok((status, body)) => {
                    let t = self.sh.ns();
                    // A server starting may say so (503): a read waits.
                    if again && status >= 500 && self.sh.restarting(t) {
                        if !counted {
                            self.sh.down.fetch_add(1, Ordering::Relaxed);
                            counted = true;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    self.sh.failed(t, || {
                        format!(
                            "{text}: {status} {}",
                            String::from_utf8_lossy(&body[..body.len().min(200)])
                        )
                    });
                    return Answer::Refused;
                }
                Err(e) => {
                    self.c = None;
                    let t = self.sh.ns();
                    if !counted {
                        self.sh.failed(t, || format!("{text}: {e}"));
                        counted = true;
                    }
                    if !again {
                        // Wait for the server before the next operation,
                        // which would only fail the same way.
                        self.wait_up();
                        return Answer::Unknown;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn wait_up(&mut self) {
        while !self.sh.stop.load(Ordering::Relaxed) {
            if let Ok(mut c) = http::Http::try_connect(&self.sh.addr, Duration::from_secs(5)) {
                if matches!(
                    c.try_request("GET", "/_health", "text/plain", b""),
                    Ok((200, _))
                ) {
                    self.c = Some(c);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn log(&self, ev: Ev) {
        self.slot.log.lock().unwrap().push(ev);
    }

    fn read(&mut self, key: u64) -> bool {
        let s = self.sh.ns();
        let Answer::Ok(body) = self.ask("get usertable where id = $1", &key.to_string(), true)
        else {
            return false;
        };
        let rows = json_rows(&body);
        let got = rows.first().map(|r| r.1).unwrap_or([0; FIELDS]);
        if rows.len() > 1 || rows.first().is_some_and(|r| r.0 != key) {
            self.sh
                .note(format!("a read of {key} answered {} rows", rows.len()));
        }
        self.log(Ev::Read {
            key,
            s,
            e: self.sh.ns(),
            found: rows.first().is_some_and(|r| r.0 == key),
            got,
        });
        true
    }

    fn write(&mut self, key: u64, field: u8, d: u64, text: &str, params: &str) -> bool {
        let s = self.sh.ns();
        let (failed, affected) = match self.ask(text, params, false) {
            Answer::Ok(body) => (false, affected(&body)),
            Answer::Unknown => (true, 1),
            Answer::Refused | Answer::Stopped => return false,
        };
        self.log(Ev::Write {
            key,
            field,
            d,
            s,
            e: self.sh.ns(),
            failed,
            affected,
        });
        !failed
    }

    fn insert(&mut self) -> bool {
        let key = {
            let mut f = self.sh.fifo.lock().unwrap();
            f.0 += 1;
            f.0
        };
        let mut params = key.to_string();
        for v in record_values(key) {
            write!(params, ",\"{v}\"").unwrap();
        }
        let ok = self.write(key, INSERT, 0, &insert_text(), &params);
        // Answered or not -- it may be there -- it can be read and deleted.
        self.sh.fifo.lock().unwrap().1.push_back(key);
        ok
    }

    fn session_read(&mut self) -> bool {
        let now = self.sh.ns();
        let ttl = self.sh.ttl.as_nanos() as u64;
        let slack = 5_000_000_000;
        let pick = {
            let sessions = self.sh.sessions.lock().unwrap();
            let old: Vec<&Session> = sessions
                .iter()
                .filter(|x| x.acked + ttl + slack < now)
                .collect();
            let young: Vec<&Session> = sessions
                .iter()
                .filter(|x| !x.failed && x.s + ttl > now + slack)
                .collect();
            let from = match self.rng.below(2) == 0 && !old.is_empty() {
                true => old,
                false => young,
            };
            match from.is_empty() {
                true => None,
                false => Some(*from[self.rng.below(from.len() as u64) as usize]),
            }
        };
        let Some(x) = pick else {
            let key = self.rng.below(self.sh.records) + 1;
            return self.read(key);
        };
        let Answer::Ok(body) = self.ask("get sessions where id = $1", &x.id.to_string(), true)
        else {
            return false;
        };
        let found = !http::ids(&body).is_empty();
        let expired = x.acked + ttl + slack < now;
        // One inserted near a kill may have been lost with it.
        let lost = self.sh.loss > 0 && {
            let kills = self.sh.kills.lock().unwrap();
            kills
                .iter()
                .any(|&(k, back)| x.acked + self.sh.loss >= k && x.s <= back)
        };
        if found == expired && !(lost && !found) {
            self.sh.ttl_wrong.fetch_add(1, Ordering::Relaxed);
            self.sh.note(format!(
                "session {} read {:.1} s after its insert was answered: {}",
                x.id,
                (now - x.acked) as f64 / 1e9,
                if found {
                    "there past its time"
                } else {
                    "gone inside it"
                }
            ));
        }
        true
    }

    fn op(&mut self, value: &mut String, zipf: &mut Scrambled) -> bool {
        let u = self.rng.f64();
        let records = self.sh.records;
        match u {
            u if u < 0.40 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                self.read(key)
            }
            u if u < 0.80 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                let f = self.rng.below(FIELDS as u64) as u8;
                self.rng.value(value);
                let params = format!("{key},\"{value}\"");
                let text = format!("set usertable {{field{f}: $2}} where id = $1");
                self.write(key, f, digest(value.as_bytes()), &text, &params)
            }
            u if u < 0.83 => self.insert(),
            u if u < 0.86 => {
                let key = self.sh.fifo.lock().unwrap().1.pop_front();
                match key {
                    Some(key) => self.write(
                        key,
                        DELETE,
                        0,
                        "del usertable where id = $1",
                        &key.to_string(),
                    ),
                    None => self.insert(),
                }
            }
            u if u < 0.88 => {
                let key = {
                    let f = self.sh.fifo.lock().unwrap();
                    match f.1.is_empty() {
                        true => None,
                        false => Some(f.1[self.rng.below(f.1.len() as u64) as usize]),
                    }
                };
                let key = key.unwrap_or_else(|| zipf.next(&mut self.rng, records) + 1);
                self.read(key)
            }
            u if u < 0.92 => {
                let id = self.sh.next_session.fetch_add(1, Ordering::Relaxed);
                let s = self.sh.ns();
                let params = format!("{id},{}", self.rng.below(1_000_000));
                let (ok, failed) = match self.ask(
                    "insert sessions {id: $1, user: $2, seen: now()}",
                    &params,
                    false,
                ) {
                    Answer::Ok(_) => (true, false),
                    Answer::Unknown => (false, true),
                    _ => return false,
                };
                let acked = self.sh.ns();
                let keep = (self.sh.ttl + Duration::from_secs(120)).as_nanos() as u64;
                let mut sessions = self.sh.sessions.lock().unwrap();
                sessions.push_back(Session {
                    id,
                    s,
                    acked,
                    failed,
                });
                while sessions.front().is_some_and(|x| x.acked + keep < acked) {
                    sessions.pop_front();
                }
                ok
            }
            u if u < 0.94 => self.session_read(),
            u if u < 0.96 => {
                let q = words(&mut self.rng, 2);
                matches!(
                    self.ask("get docs match body $1 limit 10", &format!("\"{q}\""), true),
                    Answer::Ok(_)
                )
            }
            u if u < 0.98 => {
                let v = unit_vector(&mut self.rng, self.sh.dim);
                let Answer::Ok(body) = self.ask("get docs near v $1 limit 10", &v, true) else {
                    return false;
                };
                let n = http::ids(&body).len();
                if n != 10 {
                    self.sh
                        .failed(self.sh.ns(), || format!("near answered {n} rows"));
                    return false;
                }
                true
            }
            _ => {
                let id = self.rng.below(self.sh.docs) + 1;
                let params = format!(
                    "{id},\"{}\",{}",
                    words(&mut self.rng, 8),
                    unit_vector(&mut self.rng, self.sh.dim)
                );
                matches!(
                    self.ask("set docs {body: $2, v: $3} where id = $1", &params, false),
                    Answer::Ok(_)
                )
            }
        }
    }
}

/// `{"affected":1}`'s count.
fn affected(body: &[u8]) -> u64 {
    body.strip_prefix(b"{\"affected\":")
        .and_then(|r| r.strip_suffix(b"}"))
        .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
        .unwrap_or(u64::MAX)
}

fn work(sh: &Shared, w: usize) {
    let slot = &sh.slots[w];
    let mut me = Worker {
        sh,
        slot,
        c: None,
        rng: Rng::new(SEED ^ 0x534F_414B ^ (w as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    };
    let mut zipf = Scrambled::new();
    let mut value = String::with_capacity(super::FIELD_LEN);
    // Open loop: an operation every `every`, the connections' schedules
    // staggered across it.
    let every = (sh.rate > 0.0).then(|| Duration::from_secs_f64(sh.threads as f64 / sh.rate));
    let first = every.map(|e| sh.t0 + e.mul_f64(w as f64 / sh.threads as f64));
    let mut sent = 0u64;
    while !sh.stop.load(Ordering::Relaxed) {
        let due = match (first, every) {
            (Some(first), Some(e)) => {
                let due = first + e.mul_f64(sent as f64);
                sent += 1;
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                due
            }
            _ => Instant::now(),
        };
        // Every write that began before this one is logged before the
        // checker reads past it (`safe`).
        slot.inflight.store(sh.ns(), Ordering::SeqCst);
        let ok = me.op(&mut value, &mut zipf);
        slot.inflight.store(u64::MAX, Ordering::SeqCst);
        if ok {
            slot.hist
                .lock()
                .unwrap()
                .record(due.elapsed().as_nanos() as u64);
            sh.ops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A subscription held open: `GET /usertable/changes` over a range of 64
/// keys, read as it comes, opened again when it ends.
fn subscribe(sh: &Shared, i: usize) {
    let mut rng = Rng::new(SEED ^ 0x5355_4253 ^ i as u64);
    let from = Scrambled::new().next(&mut rng, sh.records.saturating_sub(64)) + 1;
    let query = format!("id%20%3E%3D%20{from}%20and%20id%20%3C%20{}", from + 64);
    while !sh.stop.load(Ordering::Relaxed) {
        let opened = (|| -> std::io::Result<std::net::TcpStream> {
            let to = sh.addr.parse().map_err(std::io::Error::other)?;
            let mut s = std::net::TcpStream::connect_timeout(&to, Duration::from_secs(10))?;
            // The stop is looked at between reads.
            s.set_read_timeout(Some(Duration::from_secs(2)))?;
            write!(
                s,
                "GET /usertable/changes?where={query} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )?;
            Ok(s)
        })();
        let mut ended = String::from("refused");
        if let Ok(s) = opened {
            sh.streams.fetch_add(1, Ordering::Relaxed);
            let mut r = BufReader::new(s);
            let mut line = String::new();
            let mut head = true;
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) => {
                        ended = "ended".into();
                        break;
                    }
                    Ok(_) if head => {
                        if !line.contains(" 200") {
                            ended = line.trim().to_string();
                            break;
                        }
                        head = false;
                    }
                    Ok(_) => {
                        if line.starts_with("event: ") {
                            sh.events.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        if sh.stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Err(e) => {
                        ended = e.to_string();
                        break;
                    }
                }
            }
            sh.streams.fetch_sub(1, Ordering::Relaxed);
        }
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }
        sh.failed(sh.ns(), || format!("subscription {i}: {ended}"));
        std::thread::sleep(Duration::from_secs(1));
    }
}

// -------------------------------------------------------------------- run

/// What the minute's line counts of the run's own doings.
#[derive(Default)]
struct Doings {
    kills: AtomicU64,
    verifies: AtomicU64,
    verify_failed: AtomicU64,
    backups: AtomicU64,
    backup_failed: AtomicU64,
    compacts: AtomicU64,
    compact_failed: AtomicU64,
}

fn load_average() -> (f64, f64) {
    extern "C" {
        fn getloadavg(loadavg: *mut f64, nelem: i32) -> i32;
    }
    let mut l = [0f64; 3];
    // SAFETY: `l` holds the three doubles asked for, and libc writes no
    // more than `nelem` of them.
    let n = unsafe { getloadavg(l.as_mut_ptr(), 3) };
    match n >= 2 {
        true => (l[0], l[1]),
        false => (-1.0, -1.0),
    }
}

fn run(o: &Opts) {
    let token: String = {
        let mut rng = Rng::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
        );
        (0..4).map(|_| format!("{:016x}", rng.next())).collect()
    };
    let bench = Bench {
        name: o.name.clone(),
        image: o.image.clone(),
        tools: o.tools.clone(),
        memory: o.memory.clone(),
        sync: o.sync.clone(),
        token,
        port: http::free_port(),
        replica_port: http::free_port(),
    };
    bench.clean();
    let (ok, out) = docker(&["network", "create", &bench.net()]);
    assert!(ok, "docker network create: {out}");
    bench.start_primary();
    bench.wait_health(bench.port, Duration::from_secs(600));
    let addr = format!("127.0.0.1:{}", bench.port);
    load(&addr, o);
    bench.start_replica();
    bench.wait_health(bench.replica_port, Duration::from_secs(600));
    let raddr = format!("127.0.0.1:{}", bench.replica_port);
    let seq = |a: &str| {
        super::metrics_page(a).map(|p| metric_sum(p.as_bytes(), "fennec_change_sequence"))
    };
    let t = Instant::now();
    while seq(&raddr) != seq(&addr) && t.elapsed() < Duration::from_secs(1800) {
        std::thread::sleep(Duration::from_secs(2));
    }
    eprintln!(
        "soak: the replica followed in {:.0} s; {} connections at {}, {} subscriptions, --sync {}, --memory {}",
        t.elapsed().as_secs_f64(),
        o.threads,
        match o.rate > 0.0 {
            true => format!("{:.0} ops/s", o.rate),
            false => "the peak".into(),
        },
        o.subscriptions,
        o.sync,
        o.memory
    );

    let sh = Arc::new(Shared {
        t0: Instant::now(),
        stop: AtomicBool::new(false),
        addr: addr.clone(),
        rate: o.rate,
        threads: o.threads,
        records: o.records,
        docs: o.docs,
        dim: o.dim,
        ttl: Duration::from_secs(o.ttl),
        ops: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        down: AtomicU64::new(0),
        ttl_wrong: AtomicU64::new(0),
        events: AtomicU64::new(0),
        streams: AtomicU64::new(0),
        kills: Mutex::new(Vec::new()),
        loss: o.loss_ms * 1_000_000,
        fifo: Mutex::new((o.records, VecDeque::new())),
        sessions: Mutex::new(VecDeque::new()),
        next_session: AtomicU64::new(1),
        slots: (0..o.threads)
            .map(|_| Slot {
                hist: Mutex::new(Hist::new()),
                log: Mutex::new(Vec::new()),
                inflight: AtomicU64::new(u64::MAX),
            })
            .collect(),
        notes: Mutex::new(Vec::new()),
    });
    let mut handles = Vec::new();
    for w in 0..o.threads {
        let sh = Arc::clone(&sh);
        handles.push(std::thread::spawn(move || work(&sh, w)));
    }
    for i in 0..o.subscriptions {
        let sh = Arc::clone(&sh);
        handles.push(std::thread::spawn(move || subscribe(&sh, i)));
    }

    let doings = Arc::new(Doings::default());
    let mut checker = Checker::new(o.records, sh.loss);
    let mut hours: VecDeque<Hist> = VecDeque::new();
    let mut prev_majflt = None;
    let mut auto_before = 0u64;
    let mut auto_last = 0u64;
    let mut totals = (0u64, 0u64, 0u64);
    let mut compacts_before = 0u64;
    let mut peak = Vec::new();
    let mut side: Option<std::thread::JoinHandle<()>> = None;
    let mut last = sh.t0;
    for m in 1..=o.minutes {
        let at = sh.t0 + Duration::from_secs(60 * m);
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
        let now = Instant::now();
        let secs = now.duration_since(last).as_secs_f64();
        last = now;
        // The latencies of the minute, and of the hour to it.
        let mut hist = Hist::new();
        for s in &sh.slots {
            let h = std::mem::replace(&mut *s.hist.lock().unwrap(), Hist::new());
            hist.merge(&h);
        }
        let ops = sh.ops.swap(0, Ordering::Relaxed);
        if hours.len() == 60 {
            hours.pop_front();
        }
        hours.push_back(Hist::new());
        hours.back_mut().unwrap().merge(&hist);
        let mut hour = Hist::new();
        for h in &hours {
            hour.merge(h);
        }
        // The checker: every read that ended before the earliest
        // operation in flight, what no later read needs let go of, past
        // the window a kill could still take back.
        let safe = sh
            .slots
            .iter()
            .map(|s| s.inflight.load(Ordering::SeqCst))
            .min()
            .unwrap_or(u64::MAX)
            .min(sh.ns());
        for s in &sh.slots {
            let evs = std::mem::take(&mut *s.log.lock().unwrap());
            checker.take(evs);
        }
        checker.kills = sh.kills.lock().unwrap().clone();
        let before = checker.mismatches;
        checker.hold(safe, safe.saturating_sub(sh.loss + 10_000_000_000));
        let mismatches = checker.mismatches - before + sh.ttl_wrong.swap(0, Ordering::Relaxed);
        let errors = sh.errors.swap(0, Ordering::Relaxed);
        let down = sh.down.swap(0, Ordering::Relaxed);
        totals.0 += ops;
        totals.1 += errors;
        totals.2 += mismatches;
        // The server, as its process and its metrics have it.
        let look = bench.look();
        let page = super::metrics_page(&addr).unwrap_or_default();
        let rpage = super::metrics_page(&raddr).unwrap_or_default();
        let auto = metric_sum(page.as_bytes(), "fennec_auto_compactions_total");
        if auto < auto_last {
            // Started again: its count with it.
            auto_before += auto_last;
        }
        auto_last = auto;
        let compactions = auto_before + auto + doings.compacts.load(Ordering::Relaxed);
        let lag = metric_sum(page.as_bytes(), "fennec_change_sequence")
            .saturating_sub(metric_sum(rpage.as_bytes(), "fennec_change_sequence"));
        let (load1, load5) = load_average();
        let mut v: Vec<(&str, String)> = vec![
            ("secs", format!("{secs:.1}")),
            ("ops", ops.to_string()),
            ("ops_s", format!("{:.0}", ops as f64 / secs)),
            ("rate", format!("{:.0}", o.rate)),
            ("p50", format!("{:.0}", hist.pct(0.5))),
            ("p99", format!("{:.0}", hist.pct(0.99))),
            ("p999", format!("{:.0}", hist.pct(0.999))),
            ("max", format!("{:.0}", hist.max as f64 / 1e3)),
            ("p99_hour", format!("{:.0}", hour.pct(0.99))),
            ("errors", errors.to_string()),
            ("down", down.to_string()),
            ("mismatches", mismatches.to_string()),
            (
                "live",
                metric_sum(page.as_bytes(), "fennec_data_bytes").to_string(),
            ),
            ("compactions", compactions.to_string()),
            (
                "memory_bytes",
                metric_sum(page.as_bytes(), "fennec_memory_bytes").to_string(),
            ),
            ("lag", lag.to_string()),
            ("events", sh.events.swap(0, Ordering::Relaxed).to_string()),
            ("streams", sh.streams.load(Ordering::Relaxed).to_string()),
            (
                "sessions",
                page.lines()
                    .find(|l| l.starts_with("fennec_documents{collection=\"sessions\"}"))
                    .and_then(|l| l.rsplit(' ').next())
                    .unwrap_or("0")
                    .to_string(),
            ),
            ("load1", format!("{load1:.2}")),
            ("load5", format!("{load5:.2}")),
        ];
        if let Some(l) = look {
            let majflt = match prev_majflt {
                Some(p) if l.majflt >= p => l.majflt - p,
                _ => l.majflt,
            };
            prev_majflt = Some(l.majflt);
            v.extend([
                ("file", l.file.to_string()),
                ("rss_anon", l.rss_anon.to_string()),
                ("cg_anon", l.cgroup.anon.to_string()),
                ("cg_file", l.cgroup.file.to_string()),
                ("cg_current", l.cgroup.current.to_string()),
                ("majflt", majflt.to_string()),
                ("threads", l.threads.to_string()),
                ("fds", l.fds.to_string()),
            ]);
        }
        for (name, n) in [
            ("kill", &doings.kills),
            ("verify", &doings.verifies),
            ("verify_failed", &doings.verify_failed),
            ("backup", &doings.backups),
            ("backup_failed", &doings.backup_failed),
            ("compact_failed", &doings.compact_failed),
        ] {
            v.push((name, n.load(Ordering::Relaxed).to_string()));
        }
        // The doings' counts are the minute's, zeroed after its line; the
        // compacts' total stays, for `compactions`.
        let manual = doings.compacts.load(Ordering::Relaxed);
        v.push(("compact", (manual - compacts_before).to_string()));
        compacts_before = manual;
        let cell = format!("{}:{}:m{m:04}", o.sync, o.label);
        append_values(&o.out, "soak", &o.run_id, &cell, &v);
        for n in [
            &doings.kills,
            &doings.verifies,
            &doings.verify_failed,
            &doings.backups,
            &doings.backup_failed,
            &doings.compact_failed,
        ] {
            n.store(0, Ordering::Relaxed);
        }
        eprintln!(
            "soak {m:>4} min: {:.0} ops/s, p99 {:.0} us, p99.9 {:.0}, max {:.0} ms; {errors} errors, {down} at a restart, {mismatches} mismatches; file {} MB, live {} MB, anon {} MB, lag {lag}, load {load1:.1}",
            ops as f64 / secs,
            hist.pct(0.99),
            hist.pct(0.999),
            hist.max as f64 / 1e6,
            look.map(|l| l.file >> 20).unwrap_or(0),
            metric_sum(page.as_bytes(), "fennec_data_bytes") >> 20,
            look.map(|l| l.rss_anon >> 20).unwrap_or(0),
        );
        if o.peak && m > 1 {
            peak.push(ops as f64 / secs);
        }
        if m == o.minutes {
            break;
        }
        // The minute's doings, after its line: their cost shows in the next.
        let due = |every: u64| every > 0 && m % every == 0;
        if (due(o.compact_every) || due(o.backup_every) || due(o.kill_every))
            && side.as_ref().is_some_and(|h| !h.is_finished())
        {
            // One at a time: a kill waits for a backup or a compact.
            side.take().unwrap().join().unwrap();
        }
        if due(o.compact_every) {
            let (addr, doings) = (addr.clone(), Arc::clone(&doings));
            side = Some(std::thread::spawn(move || {
                let done = http::Http::try_connect(&addr, Duration::from_secs(3600))
                    .and_then(|mut c| c.try_query("compact usertable", "").map(|(s, _)| s));
                match done {
                    Ok(200) => doings.compacts.fetch_add(1, Ordering::Relaxed),
                    other => {
                        eprintln!("soak: compact failed: {other:?}");
                        doings.compact_failed.fetch_add(1, Ordering::Relaxed)
                    }
                };
            }));
        } else if due(o.backup_every) {
            let script = format!(
                "rm -rf /data/archive /data/restored.fennec && \
                 /tools/fennec backup http://{}:8080 /data/archive --token {} && \
                 /tools/fennec restore /data/archive /data/restored.fennec && \
                 /tools/fennec verify /data/restored.fennec; s=$?; \
                 rm -rf /data/archive /data/restored.fennec; exit $s",
                bench.server(),
                bench.token
            );
            let doings = Arc::clone(&doings);
            let (name, tools, net, volume) = (
                format!("{}-backup", bench.name),
                bench.tools.clone(),
                bench.net(),
                bench.volume("backup-data"),
            );
            side = Some(std::thread::spawn(move || {
                let t = Instant::now();
                let (ok, out) = docker(&[
                    "run",
                    "--rm",
                    "--name",
                    &name,
                    "--network",
                    &net,
                    "-v",
                    &format!("{tools}:/tools:ro"),
                    "-v",
                    &format!("{volume}:/data"),
                    "alpine",
                    "sh",
                    "-c",
                    &script,
                ]);
                eprintln!(
                    "soak: backup, restore and verify {} in {:.0} s",
                    if ok { "clean" } else { "FAILED" },
                    t.elapsed().as_secs_f64()
                );
                if !ok {
                    eprintln!("{out}");
                    doings.backup_failed.fetch_add(1, Ordering::Relaxed);
                }
                doings.backups.fetch_add(1, Ordering::Relaxed);
            }));
        }
        if due(o.kill_every) {
            if let Some(h) = side.take() {
                h.join().unwrap();
            }
            let k = sh.ns();
            sh.kills.lock().unwrap().push((k, u64::MAX));
            docker(&["kill", "--signal", "KILL", &bench.server()]);
            docker(&["wait", &bench.server()]);
            doings.kills.fetch_add(1, Ordering::Relaxed);
            let t = Instant::now();
            let (ok, out) = bench.fennec(
                &format!("{}-verify", bench.name),
                &bench.volume("data"),
                "/tools/fennec verify /data/soak.fennec",
            );
            eprintln!(
                "soak: killed at minute {m}; fennec verify {} in {:.0} s",
                if ok { "clean" } else { "FAILED" },
                t.elapsed().as_secs_f64()
            );
            if !ok {
                eprintln!("{out}");
                doings.verify_failed.fetch_add(1, Ordering::Relaxed);
            }
            doings.verifies.fetch_add(1, Ordering::Relaxed);
            let (ok, out) = docker(&["start", &bench.server()]);
            assert!(ok, "docker start: {out}");
            bench.wait_health(bench.port, Duration::from_secs(1800));
            let back = sh.ns();
            sh.kills.lock().unwrap().last_mut().unwrap().1 = back;
            prev_majflt = None;
            eprintln!(
                "soak: answering again {:.1} s after the kill",
                (back - k) as f64 / 1e9
            );
        }
    }

    // The end: the load stopped, every read held, the replica caught up
    // and held to the primary, both stopped as a supervisor stops them and
    // their files verified.
    sh.stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    if let Some(h) = side.take() {
        h.join().unwrap();
    }
    for s in &sh.slots {
        let evs = std::mem::take(&mut *s.log.lock().unwrap());
        checker.take(evs);
    }
    checker.kills = sh.kills.lock().unwrap().clone();
    let before = checker.mismatches;
    checker.hold(u64::MAX, u64::MAX);
    let late = checker.mismatches - before + sh.ttl_wrong.swap(0, Ordering::Relaxed);
    let t = Instant::now();
    while seq(&raddr) != seq(&addr) && t.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_secs(1));
    }
    let docs = |a: &str| -> Vec<String> {
        let page = super::metrics_page(a).unwrap_or_default();
        let mut d: Vec<String> = page
            .lines()
            .filter(|l| l.starts_with("fennec_documents{"))
            .map(str::to_string)
            .collect();
        d.sort();
        d
    };
    let (primary_docs, replica_docs) = (docs(&addr), docs(&raddr));
    let same = !primary_docs.is_empty() && primary_docs == replica_docs;
    if !same {
        eprintln!("soak: the replica holds {replica_docs:?}, the primary {primary_docs:?}");
    }
    docker(&["stop", "-t", "300", &bench.server()]);
    docker(&["stop", "-t", "300", &bench.replica()]);
    let verify = |volume: &str, file: &str| {
        let (ok, out) = bench.fennec(
            &format!("{}-verify", bench.name),
            volume,
            &format!("/tools/fennec verify /data/{file}"),
        );
        if !ok {
            eprintln!("{out}");
        }
        ok
    };
    let primary_ok = verify(&bench.volume("data"), "soak.fennec");
    let replica_ok = verify(&bench.volume("replica-data"), "replica.fennec");
    let cell = format!("{}:{}:end", o.sync, o.label);
    let mut v: Vec<(&str, String)> = vec![
        ("minutes", o.minutes.to_string()),
        ("ops", totals.0.to_string()),
        ("errors", totals.1.to_string()),
        ("mismatches", (totals.2 + late).to_string()),
        ("reads_held", checker.reads.to_string()),
        ("writes_held", checker.writes.to_string()),
        ("kills", sh.kills.lock().unwrap().len().to_string()),
        ("verify_failed", (!primary_ok as u8).to_string()),
        ("replica_verify_failed", (!replica_ok as u8).to_string()),
        ("replica_differs", (!same as u8).to_string()),
        (
            "memory_limit",
            super::bytes_of(&o.memory).unwrap_or(0).to_string(),
        ),
        ("records", o.records.to_string()),
    ];
    if o.peak && !peak.is_empty() {
        peak.sort_by(|a, b| a.total_cmp(b));
        let p = peak[peak.len() / 2];
        v.push(("peak_ops_s", format!("{p:.0}")));
        println!("peak {p:.0}");
    }
    append_values(&o.out, "soak", &o.run_id, &cell, &v);
    eprintln!(
        "soak: {} minutes, {} operations, {} errors, {} mismatches ({} reads and {} writes held), the files {} and {}, the replica {}",
        o.minutes,
        totals.0,
        totals.1,
        totals.2 + late,
        checker.reads,
        checker.writes,
        if primary_ok { "clean" } else { "NOT CLEAN" },
        if replica_ok { "clean" } else { "NOT CLEAN" },
        if same { "the same" } else { "DIFFERENT" },
    );
    drop(bench);
}

// ----------------------------------------------------------------- report

/// Judges each run in `path` (or the one named) by the soak's exit
/// criterion, a line each with the minutes that broke it: 8 hours; the
/// last hour's p99 at most 1.25 times the first's; the file at most 2.2
/// times the live bytes every minute; the server's anonymous memory grown
/// less than 5% after the first hour; threads and descriptors within 2;
/// no error; no mismatch; every verify clean. 0 when every run passed.
fn report(path: &Path, only: Option<&str>) -> i32 {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    // run -> cell -> metric -> value
    let mut runs: Vec<String> = Vec::new();
    let mut data: HashMap<String, HashMap<String, HashMap<String, f64>>> = HashMap::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 8 || f[4] != "soak" || only.is_some_and(|r| r != f[0]) {
            continue;
        }
        if !runs.iter().any(|r| r == f[0]) {
            runs.push(f[0].to_string());
        }
        if let Ok(x) = f[7].parse::<f64>() {
            data.entry(f[0].to_string())
                .or_default()
                .entry(f[5].to_string())
                .or_default()
                .insert(f[6].to_string(), x);
        }
    }
    let mut failed = 0;
    for run in &runs {
        let cells = &data[run];
        let mut minutes: Vec<(u64, &HashMap<String, f64>)> = cells
            .iter()
            .filter_map(|(cell, m)| {
                let n = cell.rsplit(':').next()?.strip_prefix('m')?.parse().ok()?;
                Some((n, m))
            })
            .collect();
        minutes.sort_by_key(|(n, _)| *n);
        let end = cells
            .iter()
            .find(|(c, _)| c.ends_with(":end"))
            .map(|(_, m)| m);
        let label = cells
            .keys()
            .next()
            .map(|c| c.rsplit_once(':').map_or(c.as_str(), |x| x.0).to_string())
            .unwrap_or_default();
        println!(
            "run {run} ({label}): {} minutes{}",
            minutes.len(),
            if end.is_some() {
                ", finished"
            } else {
                ", not finished"
            }
        );
        let get = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
        let list = |v: &[u64]| -> String {
            let mut s = v
                .iter()
                .take(20)
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            if v.len() > 20 {
                write!(s, " and {} more", v.len() - 20).unwrap();
            }
            s
        };
        let mut verdicts: Vec<(&str, bool, String)> = Vec::new();
        let n = minutes.len() as u64;
        verdicts.push((
            "8 hours",
            n >= 480 && end.is_some(),
            format!(
                "{n} minutes{}",
                if end.is_none() { ", no end line" } else { "" }
            ),
        ));
        // The hour's p99 as the run wrote it at minute 60 and at the end.
        let at = |k: u64| minutes.iter().rev().find(|(m, _)| *m <= k).map(|x| x.1);
        let first = at(60).and_then(|m| get(m, "p99_hour"));
        let last = minutes.last().and_then(|(_, m)| get(m, "p99_hour"));
        match (first, last) {
            (Some(a), Some(b)) if a > 0.0 => {
                let broke: Vec<u64> = minutes
                    .iter()
                    .filter(|(k, m)| *k > 60 && get(m, "p99_hour").is_some_and(|x| x > 1.25 * a))
                    .map(|(k, _)| *k)
                    .collect();
                verdicts.push((
                    "last hour's p99 <= 1.25x the first's",
                    b <= 1.25 * a && n >= 120,
                    format!(
                        "first {a:.0} us, last {b:.0} us: {:.2}x{}{}",
                        b / a,
                        if n < 120 {
                            " (not judged: under 2 hours the hours overlap)"
                        } else {
                            ""
                        },
                        if broke.is_empty() {
                            String::new()
                        } else {
                            format!("; an hour past 1.25x at minutes {}", list(&broke))
                        }
                    ),
                ));
            }
            _ => verdicts.push((
                "last hour's p99 <= 1.25x the first's",
                false,
                "no p99".into(),
            )),
        }
        let broke: Vec<u64> = minutes
            .iter()
            .filter(|(_, m)| match (get(m, "file"), get(m, "live")) {
                (Some(f), Some(l)) => f > 2.2 * l,
                _ => true,
            })
            .map(|(k, _)| *k)
            .collect();
        let worst = minutes
            .iter()
            .filter_map(|(_, m)| Some(get(m, "file")? / get(m, "live")?.max(1.0)))
            .fold(0.0f64, f64::max);
        verdicts.push((
            "file <= 2.2x live every minute",
            broke.is_empty(),
            match broke.is_empty() {
                true => format!("at most {worst:.2}x"),
                false => format!(
                    "at most {worst:.2}x; minutes {} (or not looked at)",
                    list(&broke)
                ),
            },
        ));
        let base = at(60).and_then(|m| get(m, "rss_anon"));
        match base {
            Some(b) if n > 60 => {
                let broke: Vec<u64> = minutes
                    .iter()
                    .filter(|(k, m)| *k > 60 && get(m, "rss_anon").is_some_and(|x| x > 1.05 * b))
                    .map(|(k, _)| *k)
                    .collect();
                let most = minutes
                    .iter()
                    .filter(|(k, _)| *k > 60)
                    .filter_map(|(_, m)| get(m, "rss_anon"))
                    .fold(0.0f64, f64::max);
                verdicts.push((
                    "anonymous memory grows < 5% after the first hour",
                    broke.is_empty(),
                    format!(
                        "{:.0} MB at minute 60, at most {:.0} MB after: {:+.1}%{}",
                        b / 1048576.0,
                        most / 1048576.0,
                        (most / b - 1.0) * 100.0,
                        if broke.is_empty() {
                            String::new()
                        } else {
                            format!("; minutes {}", list(&broke))
                        }
                    ),
                ));
            }
            _ => verdicts.push((
                "anonymous memory grows < 5% after the first hour",
                false,
                format!("not judged: {n} minutes, no hour after the first"),
            )),
        }
        for key in ["threads", "fds"] {
            let mut v: Vec<f64> = minutes.iter().filter_map(|(_, m)| get(m, key)).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            let Some(&mid) = v.get(v.len() / 2) else {
                verdicts.push((
                    "threads and descriptors within 2",
                    false,
                    format!("no {key}"),
                ));
                continue;
            };
            let broke: Vec<u64> = minutes
                .iter()
                .filter(|(_, m)| get(m, key).is_some_and(|x| (x - mid).abs() > 2.0))
                .map(|(k, _)| *k)
                .collect();
            verdicts.push((
                if key == "threads" {
                    "threads within 2 of their median"
                } else {
                    "descriptors within 2 of their median"
                },
                broke.is_empty(),
                format!(
                    "median {mid:.0}, {:.0} to {:.0}{}",
                    v[0],
                    v[v.len() - 1],
                    if broke.is_empty() {
                        String::new()
                    } else {
                        format!("; minutes {}", list(&broke))
                    }
                ),
            ));
        }
        for (what, key) in [("no error", "errors"), ("no mismatch", "mismatches")] {
            let broke: Vec<u64> = minutes
                .iter()
                .filter(|(_, m)| get(m, key).is_some_and(|x| x > 0.0))
                .map(|(k, _)| *k)
                .collect();
            let total: f64 = minutes
                .iter()
                .filter_map(|(_, m)| get(m, key))
                .fold(0.0, |a, b| a + b);
            let at_end = end.and_then(|m| get(m, key)).unwrap_or(0.0)
                - match key {
                    "mismatches" | "errors" => total,
                    _ => 0.0,
                };
            let at_end = at_end.max(0.0);
            verdicts.push((
                what,
                broke.is_empty() && at_end == 0.0,
                format!(
                    "{:.0}{}{}",
                    total + at_end,
                    if broke.is_empty() {
                        String::new()
                    } else {
                        format!("; minutes {}", list(&broke))
                    },
                    if at_end > 0.0 {
                        format!("; {at_end:.0} found at the end")
                    } else {
                        String::new()
                    }
                ),
            ));
        }
        // From +0.0: an empty f64 sum is -0.0, which prints as "-0".
        let sum = |k: &str| -> f64 {
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, k))
                .fold(0.0, |a, b| a + b)
        };
        let verifies = sum("verify");
        let bad = sum("verify_failed") + sum("backup_failed");
        let end_bad = end.map_or(1.0, |m| {
            get(m, "verify_failed").unwrap_or(1.0)
                + get(m, "replica_verify_failed").unwrap_or(1.0)
                + get(m, "replica_differs").unwrap_or(1.0)
        });
        verdicts.push((
            "every verify clean",
            bad == 0.0 && end_bad == 0.0,
            format!(
                "{verifies:.0} at restarts, {:.0} backups restored and verified, {bad:.0} failed; at the end {}",
                sum("backup"),
                match end {
                    None => "none (not finished)".to_string(),
                    Some(m) => format!(
                        "the primary {}, the replica {} and {}",
                        if get(m, "verify_failed") == Some(0.0) { "clean" } else { "NOT CLEAN" },
                        if get(m, "replica_verify_failed") == Some(0.0) { "clean" } else { "NOT CLEAN" },
                        if get(m, "replica_differs") == Some(0.0) {
                            "the same as the primary"
                        } else {
                            "DIFFERENT from the primary"
                        }
                    ),
                }
            ),
        ));
        let mut busy: Vec<u64> = minutes
            .iter()
            .filter(|(_, m)| get(m, "load1").is_some_and(|x| x >= 4.0))
            .map(|(k, _)| *k)
            .collect();
        busy.dedup();
        for (what, ok, detail) in &verdicts {
            println!(
                "  {:<52} {:<4} {detail}",
                what,
                if *ok { "PASS" } else { "FAIL" }
            );
            if !ok {
                failed += 1;
            }
        }
        if !busy.is_empty() {
            println!(
                "  the host's load was 4 or more in {} minutes: {}",
                busy.len(),
                list(&busy)
            );
        }
    }
    if runs.is_empty() {
        println!("no soak run in {path:?}");
        return 1;
    }
    (failed > 0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(key: u64, s: u64, e: u64, got: [u64; FIELDS]) -> Ev {
        Ev::Read {
            key,
            s,
            e,
            found: true,
            got,
        }
    }

    fn update(key: u64, field: u8, d: u64, s: u64, e: u64, failed: bool) -> Ev {
        Ev::Write {
            key,
            field,
            d,
            s,
            e,
            failed,
            affected: 1,
        }
    }

    fn with(key: u64, f: usize, d: u64) -> [u64; FIELDS] {
        let mut v = base_digests(key);
        v[f] = d;
        v
    }

    #[test]
    fn a_read_holds_the_last_write_across_minutes() {
        let mut c = Checker::new(100, 0);
        c.take(vec![update(7, 3, 42, 10, 20, false)]);
        c.take(vec![read(7, 30, 40, with(7, 3, 42))]);
        c.hold(50, 50);
        assert_eq!(c.mismatches, 0);
        // The register settled: the value before it no longer holds.
        assert!(c.regs.is_empty());
        c.take(vec![read(7, 60, 70, base_digests(7))]);
        c.hold(100, 100);
        assert_eq!(c.mismatches, 1);
        c.take(vec![read(7, 80, 90, with(7, 3, 42))]);
        c.hold(100, 100);
        assert_eq!(c.mismatches, 1);
    }

    #[test]
    fn a_read_waits_for_the_writes_that_began_before_it_ended() {
        let mut c = Checker::new(100, 0);
        // The read saw a write still in flight when the checker looked.
        c.take(vec![read(7, 30, 40, with(7, 3, 42))]);
        c.hold(35, 35);
        assert_eq!((c.reads, c.mismatches), (0, 0));
        c.take(vec![update(7, 3, 42, 33, 50, false)]);
        c.hold(60, 60);
        assert_eq!((c.reads, c.mismatches), (1, 0));
    }

    #[test]
    fn a_kill_makes_a_write_near_it_either_value() {
        // Under --sync 250 a write answered just before the kill may be
        // lost: after the restart the value before it holds too, until a
        // write after the restart ends.
        let mut c = Checker::new(100, 1_000);
        c.take(vec![
            update(7, 3, 41, 100, 200, false),
            update(7, 3, 42, 5_000, 5_100, false),
        ]);
        c.kills = vec![(5_500, 9_000)];
        c.take(vec![read(7, 10_000, 10_100, with(7, 3, 41))]);
        c.take(vec![read(7, 10_200, 10_300, with(7, 3, 42))]);
        c.hold(20_000, 0);
        assert_eq!(c.mismatches, 0);
        c.take(vec![update(7, 3, 43, 11_000, 11_100, false)]);
        c.take(vec![read(7, 12_000, 12_100, with(7, 3, 41))]);
        c.hold(20_000, 0);
        assert_eq!(c.mismatches, 1);
        // Without the kill the old value is a mismatch at once.
        let mut c = Checker::new(100, 1_000);
        c.take(vec![
            update(7, 3, 41, 100, 200, false),
            update(7, 3, 42, 5_000, 5_100, false),
            read(7, 10_000, 10_100, with(7, 3, 41)),
        ]);
        c.hold(20_000, 0);
        assert_eq!(c.mismatches, 1);
    }

    #[test]
    fn a_write_not_answered_may_have_landed_or_not() {
        let mut c = Checker::new(100, 0);
        c.take(vec![update(7, 3, 42, 100, 900, true)]);
        c.take(vec![read(7, 1_000, 1_100, base_digests(7))]);
        c.take(vec![read(7, 1_200, 1_300, with(7, 3, 42))]);
        c.hold(2_000, 0);
        assert_eq!(c.mismatches, 0);
    }

    #[test]
    fn an_inserted_record_is_there_until_its_delete() {
        let mut c = Checker::new(100, 0);
        let ins = |s, e| Ev::Write {
            key: 101,
            field: INSERT,
            d: 0,
            s,
            e,
            failed: false,
            affected: 1,
        };
        let del = |s, e, affected| Ev::Write {
            key: 101,
            field: DELETE,
            d: 0,
            s,
            e,
            failed: false,
            affected,
        };
        let absent = |s, e| Ev::Read {
            key: 101,
            s,
            e,
            found: false,
            got: [0; FIELDS],
        };
        // Before its insert it is not there; after it, it is, as written.
        c.take(vec![
            absent(1, 2),
            ins(10, 20),
            read(101, 30, 40, base_digests(101)),
        ]);
        c.hold(50, 50);
        assert_eq!(c.mismatches, 0);
        c.take(vec![absent(60, 70)]);
        c.hold(80, 80);
        assert_eq!(c.mismatches, 1);
        c.take(vec![del(90, 100, 1), absent(110, 120)]);
        c.hold(130, 130);
        assert_eq!(c.mismatches, 1);
        c.take(vec![read(101, 140, 150, base_digests(101))]);
        c.hold(160, 160);
        assert_eq!(c.mismatches, 2);
        // A delete that found nothing, of a record certainly there.
        let mut c = Checker::new(100, 0);
        c.take(vec![ins(10, 20), del(30, 40, 0)]);
        c.hold(50, 50);
        assert_eq!(c.mismatches, 1);
    }

    #[test]
    fn the_report_judges_every_criterion() {
        let dir = std::env::temp_dir().join(format!("soak-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("soak.tsv");
        for m in 1..=10u64 {
            let v: Vec<(&str, String)> = vec![
                ("p99_hour", "1000".into()),
                ("file", "300".into()),
                ("live", "100".into()),
                ("rss_anon", "1000".into()),
                ("threads", "40".into()),
                ("fds", "50".into()),
                ("errors", "0".into()),
                ("mismatches", "0".into()),
                ("verify", "0".into()),
            ];
            append_values(&path, "soak", "t1", &format!("250:50:m{m:04}"), &v);
        }
        // Ten minutes, the file past 2.2x: not a pass, and every line said.
        assert_eq!(report(&path, None), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
