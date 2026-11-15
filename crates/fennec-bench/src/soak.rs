//! `ycsb soak`: fennec-server in a container under hours of load, a line
//! of figures a minute (`ycsb/run-soak.sh` runs it; `ycsb soak report`
//! judges a run). Part of `ycsb` rather than a program of its own: its
//! records, keys and histograms are YCSB's, and whether an answer is
//! allowed is decided by the one register checker `--verify` uses too
//! (`register.rs`). This module is the driver: the mix, the schedule, the
//! kills, the minute's line and the report.
//!
//! The server runs in a container whose `--memory` is under its file's
//! size, so the mapped file's pages come and go; a second follows it as a
//! replica. The load is open loop (`--rate`, a schedule a connection, an
//! operation's time from when it was due: a stall counts the wait of every
//! one queued behind it), or as fast as answered for the peak (`--peak`).
//! Its mix, over a fixed set of connections:
//!
//!   36% read and 37% update of YCSB's records (A), its scrambled zipfian,
//!       and 4% read of them through the replica, sent after the latest
//!       write answered (`Fennec-After`),
//!    2% increment of a counter (`set counters {n: n + 1}`), not
//!       idempotent: a write landing twice shows; 1% read of one,
//!    3% insert and 3% delete -- new keys, the oldest inserted deleted --
//!    2% read of an inserted key,
//!    4% insert into `sessions`, whose `seen @ttl` expires them, and 2%
//!       read of them -- by id, through a filter of ids, and counted,
//!    2% `match` on `docs`' `@text` body, 2% `near` on its `@hnsw`
//!       vector, 2% update of a doc,
//!
//! beside 64 subscriptions (`GET /usertable/changes`, a range of 64 keys
//! each) held open and held to the writes in their range; `compact
//! usertable` every `--compact-every` minutes; a backup over the
//! replication stream, restored and verified, every `--backup-every` from
//! `--backup-at`; and every `--kill-every` minutes the server killed (`kill
//! -9`), its file held to its checksums (`fennec verify`) before it starts
//! again -- every `--kill-in-compact-every` minutes in the middle of a
//! compact. Each minute a sample of `match` and `near` is held to a scan
//! of the same rows, and at the end every key is read back and the
//! replica's rows compared with the primary's.
//!
//! A minute's line (`results/soak.tsv`, family `soak`, cell
//! `<sync>:<label>:m<minute>`) is appended as the minute ends: a run cut
//! short keeps what it measured. `<sync>:<label>:start` says the run's
//! settings, `<sync>:<label>:end` closes a finished run; every line carries
//! the harness's own start (`started`), so a run id used again is told
//! apart, and the primary's life (`life`, the restarts before it).

use super::http;
use super::register::{Counters, Kills, Registers, W};
use super::{
    append_values, base_digests, digest, insert_text, json_rows, metric_sum, record_values, Cgroup,
    Hist, Rng, Scrambled, FIELDS, LOAD_BATCH, SEED,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// What a `kill -9` of the server -- the kernel surviving -- may take of the
/// writes it answered, in ms. Under `--sync always` nothing: a write is
/// answered after its fsync. Under `--sync <ms>` a write is answered once
/// its block is in the process's buffer (`fs.rs`'s pending appends), which
/// reaches the kernel by a `write` -- after which a kill takes nothing --
/// at the first of: a write behind after a request once the buffer holds
/// 64 KB (`BEHIND_AT`; passed over while a durability holds the disk), an
/// append past the buffer's bounds, or the syncer's next flush, whose
/// durability writes the buffer before its fsync (`run_syncer`). The
/// syncer sleeps its interval, then takes the lock, writes and fsyncs: a
/// write answered just after one flush's `write` waits out that sync's own
/// time, the interval, and the next one's lock and `write`. So it is in
/// the kernel within the interval and two syncs' own time -- the window the
/// power-cut rig holds a cut to (`.github/power-cut/client.py`: 250 ms and
/// two syncs' own time, `SYNC_OWN` 200 ms, 650 ms), which also covers the
/// second sync's fsync a kill does not need. A write answered earlier than
/// that before a kill must be there after it.
const SYNC_INTERVAL_MS: u64 = 250;
const SYNC_OWN_MS: u64 = 200;

fn loss_ms(sync: &str) -> u64 {
    match sync {
        "always" => 0,
        _ => SYNC_INTERVAL_MS + 2 * SYNC_OWN_MS,
    }
}

/// How long after the server answers again a failure still counts as the
/// restart's: a connection a worker held across the kill is found dead at
/// its next request, a few ms after.
const GRACE: u64 = 5_000_000_000;

/// How far behind the earliest read in flight the registers are let go of:
/// a subscription's event is held over the span from the write it reports,
/// and one later than this is `late` -- its values not judged -- and a
/// stream's failure.
const HELD_FOR: u64 = 60_000_000_000;

/// How long a stream may sit open past a write in its range without an
/// event reporting it before it is `silent`.
const SILENT_AFTER: u64 = 10_000_000_000;

/// A run's settings: `ycsb soak --help` lists them.
struct Opts {
    sync: String,
    rate: f64,
    peak: bool,
    label: String,
    minutes: u64,
    records: u64,
    docs: u64,
    counters: u64,
    dim: usize,
    memory: String,
    threads: usize,
    subscriptions: usize,
    kill_every: u64,
    kill_in_compact_every: u64,
    compact_every: u64,
    backup_every: u64,
    backup_at: u64,
    ttl: u64,
    slack: u64,
    restart_max: u64,
    lag_max: u64,
    catchup_max: u64,
    recall_min: f64,
    samples: u64,
    disk_floor: u64,
    out: PathBuf,
    run_id: String,
    image: String,
    tools: String,
    name: String,
}

const HELP: &str = "ycsb soak --sync 250|always (--rate <ops/s> [--label 50] | --peak)
    [--minutes 480] [--records 1000000] [--docs 20000] [--counters 1000] [--dim 16]
    [--memory 512m] [--threads 32] [--subscriptions 64] [--kill-every 30]
    [--kill-in-compact-every 90] [--compact-every 20] [--backup-every 120]
    [--backup-at 50] [--ttl 60] [--slack 5] [--restart-max 120] [--lag-max 5]
    [--catchup-max 300] [--recall-min 0.925] [--samples 3] [--disk-floor 3g]
    [--out soak.tsv] [--run-id <id>] [--image fennecdb-ycsb] [--tools p6-tools]
    [--name p6]
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
            counters: 1_000,
            dim: 16,
            memory: "512m".into(),
            threads: 32,
            subscriptions: 64,
            kill_every: 30,
            kill_in_compact_every: 90,
            compact_every: 20,
            backup_every: 120,
            backup_at: 50,
            ttl: 60,
            slack: 5,
            restart_max: 120,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            samples: 3,
            disk_floor: 3 << 30,
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
                "--counters" => o.counters = n(),
                "--dim" => o.dim = n() as usize,
                "--memory" => o.memory = v.clone(),
                "--threads" => o.threads = n() as usize,
                "--subscriptions" => o.subscriptions = n() as usize,
                "--kill-every" => o.kill_every = n(),
                "--kill-in-compact-every" => o.kill_in_compact_every = n(),
                "--compact-every" => o.compact_every = n(),
                "--backup-every" => o.backup_every = n(),
                "--backup-at" => o.backup_at = n(),
                "--ttl" => o.ttl = n(),
                "--slack" => o.slack = n(),
                "--restart-max" => o.restart_max = n(),
                "--lag-max" => o.lag_max = n(),
                "--catchup-max" => o.catchup_max = n(),
                "--recall-min" => o.recall_min = v.parse().expect("--recall-min <0..1>"),
                "--samples" => o.samples = n(),
                "--disk-floor" => {
                    o.disk_floor = super::bytes_of(&v).unwrap_or_else(|| panic!("--disk-floor {v}"))
                }
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
            o.records >= 1_000 && o.docs >= 100 && o.counters > 0 && o.threads > 0,
            "{HELP}"
        );
        // A session read is held present until `slack` before its time:
        // a slack past half the ttl would hold nothing.
        assert!(2 * o.slack < o.ttl, "--slack under half --ttl\n{HELP}");
        if o.peak {
            o.label = "peak".into();
            o.rate = 0.0;
            o.kill_every = 0;
            o.kill_in_compact_every = 0;
            o.compact_every = 0;
            o.backup_every = 0;
        } else if o.label.is_empty() {
            o.label = format!("{:.0}", o.rate);
        }
        if o.run_id.is_empty() {
            o.run_id = format!("s{}-{}", o.sync, o.label);
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

// ---------------------------------------------------------------- schedule

/// What the run does after a minute's line, besides the load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Doing {
    Compact,
    Backup,
    /// The server killed and verified; `in_compact`, in the middle of one.
    Kill {
        in_compact: bool,
    },
}

/// The doings of minute `m` of a run of `minutes`, in the order they run:
/// none after the last minute's line. A compact and a backup queue one
/// behind the other; a kill waits for them, but one every
/// `kill_in_compact_every` minutes, which lands in a compact of its own.
fn schedule(
    m: u64,
    minutes: u64,
    kill_every: u64,
    kill_in_compact_every: u64,
    compact_every: u64,
    backup_every: u64,
    backup_at: u64,
) -> Vec<Doing> {
    let due = |every: u64| every > 0 && m.is_multiple_of(every);
    let mut d = Vec::new();
    if m == 0 || m >= minutes {
        return d;
    }
    if due(compact_every) {
        d.push(Doing::Compact);
    }
    if backup_every > 0 && m >= backup_at && (m - backup_at).is_multiple_of(backup_every) {
        d.push(Doing::Backup);
    }
    if due(kill_every) {
        d.push(Doing::Kill {
            in_compact: due(kill_in_compact_every),
        });
    }
    d
}

/// The counts a run of `n` minutes' lines owes, by its schedule: kills
/// (and as many verifies), kills in a compact, compacts asked for (the
/// ones a kill interrupts aside), and backups (as many restores).
fn owed(n: u64, s: &Settings) -> (u64, u64, u64, u64) {
    let (mut kills, mut in_compact, mut compacts, mut backups) = (0, 0, 0, 0);
    // A minute's doings run after its line, and those of the run's last
    // line do not run: `n` lines hold the doings of minutes 1 to n - 1.
    for m in 1..n {
        for d in schedule(
            m,
            n,
            s.kill_every,
            s.kill_in_compact_every,
            s.compact_every,
            s.backup_every,
            s.backup_at,
        ) {
            match d {
                Doing::Compact => compacts += 1,
                Doing::Backup => backups += 1,
                Doing::Kill { in_compact: true } => {
                    kills += 1;
                    in_compact += 1
                }
                Doing::Kill { .. } => kills += 1,
            }
        }
    }
    (kills, in_compact, compacts, backups)
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
            format!("{}-look", self.server()),
            format!("{}-look", self.replica()),
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

    /// `script` in a container holding the tools' volume
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

    /// A server's process as a shell in its process namespace sees it: its
    /// cgroup, anonymous memory, threads, descriptors and major faults, its
    /// database file's length alone, and the free bytes of the disk under
    /// its volume -- the Docker VM's.
    fn look(&self, container: &str, file: &str) -> Option<Look> {
        let script = format!(
            "cd /proc/1/root/sys/fs/cgroup && cat memory.stat && \
             echo current $(cat memory.current) && grep oom_kill memory.events; \
             grep -E '^(Threads|RssAnon):' /proc/1/status; \
             echo fds $(ls /proc/1/fd | wc -l); \
             echo majflt $(cut -d' ' -f12 /proc/1/stat); \
             echo filesize $(stat -c %s /proc/1/root/data/{file}); \
             echo vmfree $(df -kP /proc/1/root/data | awk 'NR == 2 {{print $4}}')"
        );
        let (ok, out) = docker(&[
            "run",
            "--rm",
            "--name",
            &format!("{container}-look"),
            &format!("--pid=container:{container}"),
            // Reading another user's `/proc/<pid>/root` is a ptrace's right.
            "--cap-add",
            "SYS_PTRACE",
            "alpine",
            "sh",
            "-c",
            &script,
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
            vm_free: num("vmfree ").map(|kb| kb << 10),
        })
    }

    /// Whether a container runs, and whether the kernel killed it for its
    /// memory.
    fn state(&self, container: &str) -> (bool, bool) {
        let (ok, out) = docker(&[
            "inspect",
            "-f",
            "{{.State.Running}} {{.State.OOMKilled}}",
            container,
        ]);
        let mut w = out.split_whitespace();
        (
            ok && w.next() == Some("true"),
            ok && w.next() == Some("true"),
        )
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

    fn wait_health(&self, port: u16, within: Duration) -> bool {
        let until = Instant::now() + within;
        while !self.health(port) {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    }

    /// `/_replication/status` of the server on `port`, which the
    /// replication token reads.
    fn status(&self, port: u16) -> Option<String> {
        let mut c =
            http::Http::try_connect(&format!("127.0.0.1:{port}"), Duration::from_secs(10)).ok()?;
        let auth = format!("Authorization: Bearer {}\r\n", self.token);
        match c.try_request_with("GET", "/_replication/status", "text/plain", &auth, b"") {
            Ok((200, body)) => Some(String::from_utf8_lossy(body).into_owned()),
            _ => None,
        }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.clean();
    }
}

/// One look at a server's process.
#[derive(Default, Clone, Copy)]
struct Look {
    cgroup: Cgroup,
    rss_anon: u64,
    threads: u64,
    fds: u64,
    majflt: u64,
    file: u64,
    vm_free: Option<u64>,
}

/// The free bytes of the disk holding `path`, by `df`: the host's.
fn host_free(path: &Path) -> Option<u64> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let o = std::process::Command::new("df")
        .args(["-kP"])
        .arg(dir)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    let kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kb << 10)
}

/// A number after `"key":` in a JSON text.
fn json_num(text: &str, key: &str) -> Option<u64> {
    let at = text.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &text[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ------------------------------------------------------------------- load

/// A doc's body: `n` words of a 10 000-word vocabulary, the lower ones
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

/// The statements that make the collections, in the order they run.
fn schema(o: &Opts) -> Vec<String> {
    let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
    vec![
        format!("create collection usertable ({})", fields.join(", ")),
        format!(
            "create collection sessions (user int, seen timestamp @ttl({}s))",
            o.ttl
        ),
        format!(
            "create collection docs (body text @text, v vector<{}> @hnsw(cosine))",
            o.dim
        ),
        "create collection counters (n int)".into(),
    ]
}

/// The collections made and filled, YCSB's records as `ycsb` loads them.
fn load(addr: &str, o: &Opts) {
    let mut c = http::Http::connect(addr);
    for s in schema(o) {
        c.query(&s, "");
    }
    let t = Instant::now();
    let mut body = String::new();
    let mut post =
        |c: &mut http::Http, path: &str, n: u64, row: &mut dyn FnMut(u64, &mut String)| {
            let mut key = 1;
            while key <= n {
                let end = (key + LOAD_BATCH as u64).min(n + 1);
                body.clear();
                body.push('[');
                for k in key..end {
                    if k > key {
                        body.push(',');
                    }
                    row(k, &mut body);
                }
                body.push(']');
                c.post(path, "application/json", body.as_bytes());
                key = end;
            }
        };
    post(&mut c, "/usertable", o.records, &mut |k, b| {
        write!(b, "{{\"id\":{k}").unwrap();
        for (i, v) in record_values(k).iter().enumerate() {
            write!(b, ",\"field{i}\":\"{v}\"").unwrap();
        }
        b.push('}');
    });
    let mut rng = Rng::new(SEED ^ 0xD0C5);
    post(&mut c, "/docs", o.docs, &mut |k, b| {
        write!(
            b,
            "{{\"id\":{k},\"body\":\"{}\",\"v\":{}}}",
            words(&mut rng, 8),
            unit_vector(&mut rng, o.dim)
        )
        .unwrap();
    });
    post(&mut c, "/counters", o.counters, &mut |k, b| {
        write!(b, "{{\"id\":{k},\"n\":0}}").unwrap();
    });
    eprintln!(
        "soak: {} records, {} docs and {} counters loaded in {:.0} s",
        o.records,
        o.docs,
        o.counters,
        t.elapsed().as_secs_f64()
    );
}

// ---------------------------------------------------------------- checker

/// The register a record's being there is kept in, after its fields', and
/// a session's.
const EXISTS: u8 = FIELDS as u8;
const SESSION: u8 = FIELDS as u8 + 1;
/// A write's `field` for an insert of the whole record and for a delete.
const INSERT: u8 = FIELDS as u8;
const DELETE: u8 = FIELDS as u8 + 1;

/// An operation as a worker, a subscription or a reader logged it, its
/// span in ns from the run's start.
#[derive(Clone, Debug)]
enum Ev {
    /// A read of a record: whether it was there, and its fields' digests.
    Row {
        key: u64,
        s: u64,
        e: u64,
        found: bool,
        got: [u64; FIELDS],
    },
    /// An update of one field (`d` its value's digest), an insert or a
    /// delete; `unanswered` where it may or may not have landed.
    Write {
        key: u64,
        field: u8,
        d: u64,
        s: u64,
        e: u64,
        unanswered: bool,
        affected: u64,
        seq: Option<u64>,
    },
    /// A session inserted, `seen: now()`.
    SessionPut {
        id: u64,
        s: u64,
        e: u64,
        unanswered: bool,
        seq: Option<u64>,
    },
    /// Whether a session was there, by id or through a filter.
    Session {
        id: u64,
        s: u64,
        e: u64,
        found: bool,
    },
    /// How many sessions `[from, to)` answered counted.
    Sessions {
        from: u64,
        to: u64,
        s: u64,
        e: u64,
        n: u64,
    },
    /// A counter incremented.
    Incr {
        key: u64,
        s: u64,
        e: u64,
        unanswered: bool,
        affected: u64,
        seq: Option<u64>,
    },
    /// A counter read.
    Count { key: u64, s: u64, e: u64, n: u64 },
    /// An answer wrong on its face: two rows for one id, a row not asked
    /// for, `near` short of its limit.
    Wrong(String),
}

impl Ev {
    fn span(&self) -> Option<(u64, u64)> {
        match *self {
            Ev::Row { s, e, .. }
            | Ev::Write { s, e, .. }
            | Ev::SessionPut { s, e, .. }
            | Ev::Session { s, e, .. }
            | Ev::Sessions { s, e, .. }
            | Ev::Incr { s, e, .. }
            | Ev::Count { s, e, .. } => Some((s, e)),
            Ev::Wrong(_) => None,
        }
    }
}

/// An event a subscription read: a seed (or a part of one) or a change,
/// on its `conn`-th connection, opened at `opened`, read at `e`.
#[derive(Clone, Debug, Default)]
struct SEv {
    conn: u64,
    seed: bool,
    part: bool,
    more: bool,
    seq: u64,
    opened: u64,
    e: u64,
    rows: Vec<(u64, [u64; FIELDS])>,
    dels: Vec<u64>,
}

/// One subscription as the checker follows it.
#[derive(Default)]
struct StreamCheck {
    from: u64,
    to: u64,
    conn: u64,
    last: u64,
    opened: u64,
    seed_rows: HashSet<u64>,
    /// The writes in its range not yet reported, by the change each took.
    expect: BTreeMap<u64, Vec<(u64, W)>>,
    /// Keys a write not answered may have changed on this connection.
    unanswered: HashSet<u64>,
}

/// What the streams broke, counted.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct StreamFaults {
    missing: u64,
    twice: u64,
    order: u64,
    late: u64,
    silent: u64,
}

/// The soak's side of the checker: the reads held back until every write
/// begun before they ended is logged, the registers' settling a minute at
/// a time, and the subscriptions held to the writes in their ranges. Every
/// verdict on a value is the registers' (`register.rs`).
struct Checker {
    regs: Registers,
    counts: Counters,
    pending: Vec<Ev>,
    spending: Vec<(usize, SEv)>,
    records: u64,
    ttl: u64,
    slack: u64,
    reads: u64,
    writes: u64,
    mismatches: u64,
    examples: Vec<String>,
    faults: StreamFaults,
    streams: Vec<StreamCheck>,
    /// The change each write was answered as, and when it began: a stream
    /// event at a change reports the state after the write that took it.
    seqs: BTreeMap<u64, u64>,
    /// How far the registers were let go of.
    horizon: u64,
    /// Keys written within 10 s before a kill, or by a write not answered:
    /// what the restart's sweep reads back.
    touched: HashSet<u64>,
    base: Cell<(u64, [u64; FIELDS])>,
}

impl Checker {
    fn new(records: u64, kills: Kills, ttl: u64, slack: u64, ranges: &[(u64, u64)]) -> Checker {
        Checker {
            regs: Registers::new(kills),
            counts: Counters::default(),
            pending: Vec::new(),
            spending: Vec::new(),
            records,
            ttl,
            slack,
            reads: 0,
            writes: 0,
            mismatches: 0,
            examples: Vec::new(),
            faults: StreamFaults::default(),
            streams: ranges
                .iter()
                .map(|&(from, to)| StreamCheck {
                    from,
                    to,
                    ..StreamCheck::default()
                })
                .collect(),
            seqs: BTreeMap::new(),
            horizon: 0,
            touched: HashSet::new(),
            base: Cell::new((u64::MAX, [0; FIELDS])),
        }
    }

    fn wrong(&mut self, what: String) {
        self.mismatches += 1;
        if self.examples.len() < 20 {
            eprintln!("soak: mismatch: {what}");
            self.examples.push(what);
        }
    }

    /// What `(key, f)` was loaded with: YCSB's record for a field (an
    /// insert writes the same), a loaded record there, a session not.
    fn base(&self, key: u64, f: u8) -> u64 {
        match f {
            EXISTS => (key <= self.records) as u64,
            SESSION => 0,
            f => {
                let (k, v) = self.base.get();
                if k == key {
                    return v[f as usize];
                }
                let v = base_digests(key);
                self.base.set((key, v));
                v[f as usize]
            }
        }
    }

    fn may_hold(&mut self, key: u64, f: u8, s: u64, e: u64, d: u64) -> bool {
        let base = self.base(key, f);
        self.regs.may_hold(key, f, s, e, d, base)
    }

    /// Takes a log in: writes into their registers and the streams'
    /// expectations, reads to be held once `safe` passes them.
    fn take(&mut self, evs: Vec<Ev>) {
        for ev in evs {
            match ev {
                Ev::Write {
                    key,
                    field,
                    d,
                    s,
                    e,
                    unanswered,
                    affected,
                    seq,
                } => {
                    self.writes += 1;
                    let w = W {
                        s,
                        e,
                        d,
                        unanswered,
                    };
                    match field {
                        INSERT => {
                            for f in 0..FIELDS as u8 {
                                let d = self.base(key, f);
                                self.regs.write(key, f, W { d, ..w });
                            }
                            self.regs.write(key, EXISTS, W { d: 1, ..w });
                        }
                        DELETE => self.regs.write(key, EXISTS, W { d: 0, ..w }),
                        f => self.regs.write(key, f, w),
                    }
                    if let Some(q) = seq {
                        self.seqs.insert(q, s);
                    }
                    for st in self.streams.iter_mut() {
                        if (st.from..st.to).contains(&key) {
                            match seq {
                                Some(q) => st.expect.entry(q).or_default().push((key, w)),
                                None => {
                                    st.unanswered.insert(key);
                                }
                            }
                        }
                    }
                    if unanswered
                        || self
                            .regs
                            .kills
                            .at
                            .iter()
                            .any(|&(k, back)| e.saturating_add(10_000_000_000) >= k && s <= back)
                    {
                        self.touched.insert(key);
                    }
                    // A delete may find nothing where its insert was lost
                    // at a kill: judged once that is known (`hold`).
                    if !unanswered && affected != 1 && !(field == DELETE && affected == 0) {
                        self.wrong(format!("a write of {key} answered {affected} records"));
                    }
                    if !unanswered && field == DELETE && affected == 0 {
                        self.pending.push(Ev::Write {
                            key,
                            field,
                            d,
                            s,
                            e,
                            unanswered,
                            affected,
                            seq,
                        });
                    }
                }
                Ev::SessionPut {
                    id,
                    s,
                    e,
                    unanswered,
                    seq,
                } => {
                    self.writes += 1;
                    self.regs.write(
                        id,
                        SESSION,
                        W {
                            s,
                            e,
                            d: 1,
                            unanswered,
                        },
                    );
                    // Its `@ttl`: gone at `seen` and the ttl, `seen` the
                    // server's clock as the insert ran -- within its span
                    // as this host's clock has it, give or take `slack`
                    // between the two clocks. A write of its absence over
                    // that span, which happens whether or not the insert
                    // landed: every read after it leaves the row out.
                    self.regs.write(
                        id,
                        SESSION,
                        W::answered(
                            (s + self.ttl).saturating_sub(self.slack),
                            e + self.ttl + self.slack,
                            0,
                        ),
                    );
                    if let Some(q) = seq {
                        self.seqs.insert(q, s);
                    }
                }
                Ev::Incr {
                    key,
                    s,
                    e,
                    unanswered,
                    affected,
                    seq,
                } => {
                    self.writes += 1;
                    self.counts.add(
                        key,
                        W {
                            s,
                            e,
                            d: 1,
                            unanswered,
                        },
                    );
                    if !unanswered && affected != 1 {
                        self.wrong(format!("an increment of {key} answered {affected} records"));
                    }
                    if let Some(q) = seq {
                        self.seqs.insert(q, s);
                    }
                }
                Ev::Wrong(what) => self.wrong(what),
                read => self.pending.push(read),
            }
        }
    }

    /// Holds one read; whether it is held yet (`safe` passed its end).
    fn hold_one(&mut self, ev: &Ev, safe: u64) -> bool {
        let Some((_, e)) = ev.span() else {
            return true;
        };
        if e >= safe {
            return false;
        }
        match *ev {
            Ev::Row {
                key,
                s,
                e,
                found,
                got,
            } => {
                self.reads += 1;
                if !self.may_hold(key, EXISTS, s, e, found as u64) {
                    let what = if found { "found it" } else { "found nothing" };
                    self.wrong(format!("a read of {key} [{s}, {e}] ns {what}"));
                    return true;
                }
                if found {
                    for f in 0..FIELDS as u8 {
                        if !self.may_hold(key, f, s, e, got[f as usize]) {
                            self.wrong(format!(
                                "a read of {key} [{s}, {e}] ns: field{f} holds no value written to it"
                            ));
                            break;
                        }
                    }
                }
            }
            Ev::Session { id, s, e, found } => {
                self.reads += 1;
                if !self.may_hold(id, SESSION, s, e, found as u64) {
                    self.wrong(format!(
                        "session {id} read over [{s}, {e}] ns: {}",
                        if found {
                            "there past its time"
                        } else {
                            "gone inside it"
                        }
                    ));
                }
            }
            Ev::Sessions { from, to, s, e, n } => {
                self.reads += 1;
                let (mut lo, mut hi) = (0, 0);
                for id in from..to {
                    lo += !self.may_hold(id, SESSION, s, e, 0) as u64;
                    hi += self.may_hold(id, SESSION, s, e, 1) as u64;
                }
                if n < lo || n > hi {
                    self.wrong(format!(
                        "sessions {from}..{to} counted {n} over [{s}, {e}] ns: {lo} to {hi} there"
                    ));
                }
            }
            Ev::Count { key, s, e, n } => {
                self.reads += 1;
                let (lo, hi) = self.counts.range(key, s, e, 0, &self.regs.kills);
                if n < lo || n > hi {
                    self.wrong(format!(
                        "counter {key} read {n} over [{s}, {e}] ns: {lo} to {hi} increments"
                    ));
                }
            }
            // A delete that found nothing: its record may not have been
            // there as it began -- the delete's own write aside, which a
            // span ending before it never meets.
            Ev::Write { key, s, .. } if !self.may_hold(key, EXISTS, s, s.saturating_sub(1), 0) => {
                self.wrong(format!("a delete of {key} found no record"));
            }
            _ => {}
        }
        true
    }

    /// One subscription's event, once every write that began before it was
    /// read is logged: its changes against the writes in its range, its
    /// rows against the registers.
    fn stream_event(&mut self, i: usize, ev: SEv) {
        let mut rows: Vec<Ev> = Vec::new();
        let horizon = self.horizon;
        let kills = self.regs.kills.clone();
        let st = &mut self.streams[i];
        let mut faults = StreamFaults::default();
        let mut wrong: Vec<String> = Vec::new();
        let (from, to) = (st.from, st.to);
        let mut row =
            |key: u64, s: u64, found: bool, got: [u64; FIELDS], wrong: &mut Vec<String>| {
                if !(from..to).contains(&key) {
                    wrong.push(format!("stream {i} reported {key}, outside {from}..{to}"));
                } else {
                    rows.push(Ev::Row {
                        key,
                        s,
                        e: ev.e,
                        found,
                        got,
                    });
                }
            };
        if ev.seed {
            if ev.conn != st.conn || !ev.part {
                // A seed begins: the whole shape again, which a stream
                // opened again is sent -- the one thing a reconnect may
                // replay. What it covers is no longer owed.
                st.conn = ev.conn;
                st.last = ev.seq;
                st.opened = ev.opened;
                st.seed_rows.clear();
                st.unanswered.clear();
                st.expect = st.expect.split_off(&(ev.seq + 1));
            } else if ev.seq != st.last {
                faults.order += 1;
            }
            for &(key, got) in &ev.rows {
                if !st.seed_rows.insert(key) {
                    wrong.push(format!("stream {i}'s seed holds {key} twice"));
                }
                row(key, ev.opened, true, got, &mut wrong);
            }
            if !ev.more {
                // Every loaded record of the range is there, and the seed
                // must hold it.
                for key in st.from..st.to {
                    if !st.seed_rows.contains(&key) {
                        row(key, ev.opened, false, [0; FIELDS], &mut wrong);
                    }
                }
            }
        } else if ev.conn != st.conn {
            // A change before its connection's seed.
            faults.order += 1;
        } else {
            // A change at or before the last one read is out of order:
            // nothing it says can be placed.
            let back = ev.seq <= st.last;
            if back {
                faults.order += 1;
            }
            let mut keys: HashSet<u64> = ev.rows.iter().map(|r| r.0).collect();
            keys.extend(ev.dels.iter().copied());
            let owed: Vec<(u64, W)> = match back {
                true => Vec::new(),
                false => st
                    .expect
                    .range(st.last + 1..=ev.seq)
                    .flat_map(|(_, v)| v.iter().copied())
                    .collect(),
            };
            if back {
                keys.clear();
            }
            for (key, w) in &owed {
                if kills.certain_ever(w) && !keys.contains(key) {
                    faults.missing += 1;
                }
            }
            for key in &keys {
                if !owed.iter().any(|(k, _)| k == key) && !st.unanswered.contains(key) {
                    faults.twice += 1;
                }
            }
            st.expect = st.expect.split_off(&(ev.seq + 1));
            st.last = st.last.max(ev.seq);
            // The state the event reports is no older than the write that
            // took its change, nor the connection.
            let s = self
                .seqs
                .range(..=ev.seq)
                .next_back()
                .map_or(0, |(_, &s)| s)
                .max(ev.opened);
            if s < horizon {
                faults.late += 1;
            } else {
                for &(key, got) in &ev.rows {
                    row(key, s, true, got, &mut wrong);
                }
                for &key in &ev.dels {
                    row(key, s, false, [0; FIELDS], &mut wrong);
                }
            }
        }
        self.faults.missing += faults.missing;
        self.faults.twice += faults.twice;
        self.faults.order += faults.order;
        self.faults.late += faults.late;
        for w in wrong {
            self.wrong(w);
        }
        for r in rows {
            self.hold_one(&r, u64::MAX);
        }
    }

    /// Holds every read and stream event that ended before `safe`, judges
    /// the streams still open for silence (`open`: each one's connection
    /// opened at, `u64::MAX` closed, and the latest change it read), then
    /// lets go of what no read from `cutoff` on can be answered from.
    fn hold(&mut self, safe: u64, cutoff: u64, now: u64, open: &[(u64, u64)]) {
        let pending = std::mem::take(&mut self.pending);
        for ev in pending {
            if !self.hold_one(&ev, safe) {
                self.pending.push(ev);
            }
        }
        let spending = std::mem::take(&mut self.spending);
        for (i, ev) in spending {
            if ev.e < safe {
                self.stream_event(i, ev);
            } else {
                self.spending.push((i, ev));
            }
        }
        for (i, &(opened, seen)) in open.iter().enumerate() {
            let Some(st) = self.streams.get(i) else {
                continue;
            };
            let kills = &self.regs.kills;
            let quiet = opened != u64::MAX
                && st.expect.range(seen.max(st.last) + 1..).any(|(_, v)| {
                    v.iter().any(|(_, w)| {
                        kills.certain_ever(w) && w.e > opened && w.e + SILENT_AFTER < now
                    })
                });
            if quiet {
                self.faults.silent += 1;
            }
        }
        let cutoff = self
            .pending
            .iter()
            .filter_map(|ev| ev.span().map(|x| x.0))
            .chain(
                self.spending
                    .iter()
                    .map(|(_, ev)| ev.e.saturating_sub(HELD_FOR)),
            )
            .min()
            .unwrap_or(cutoff)
            .min(cutoff);
        let records = self.records;
        let cache: Cell<(u64, [u64; FIELDS])> = Cell::new((u64::MAX, [0; FIELDS]));
        self.regs.settle(cutoff, |key, f| match f {
            EXISTS => (key <= records) as u64,
            SESSION => 0,
            f => {
                let (k, v) = cache.get();
                if k == key {
                    return v[f as usize];
                }
                let v = base_digests(key);
                cache.set((key, v));
                v[f as usize]
            }
        });
        self.counts.settle(cutoff, &self.regs.kills);
        // A stream event's change is looked up from the oldest any stream
        // still owes.
        let oldest = self
            .streams
            .iter()
            .map(|s| s.last)
            .min()
            .unwrap_or(u64::MAX);
        if let Some((&q, _)) = self.seqs.range(..oldest).next_back() {
            self.seqs = self.seqs.split_off(&q);
        }
        // And none older than an event could still be judged against: a
        // stream stuck behind must not keep every write's change.
        let keep = cutoff.saturating_sub(HELD_FOR);
        self.seqs.retain(|_, s| *s >= keep);
        self.horizon = self.horizon.max(cutoff);
    }
}

// ---------------------------------------------------------------- workers

/// A worker's latencies: an operation is timed from when it was due, and
/// counted apart where it was due in a restart's window (from a second
/// before the kill until the server answered again) or a compact's.
struct Hists {
    steady: Hist,
    restart: Hist,
    compact: Hist,
}

impl Hists {
    fn new() -> Hists {
        Hists {
            steady: Hist::new(),
            restart: Hist::new(),
            compact: Hist::new(),
        }
    }
    fn merge(&mut self, o: &Hists) {
        self.steady.merge(&o.steady);
        self.restart.merge(&o.restart);
        self.compact.merge(&o.compact);
    }
}

/// A worker's own: its latencies since the minute's line took them, its
/// log since the checker did, the start of what it has in flight and the
/// operations it ended in the minute.
struct Slot {
    hists: Mutex<Hists>,
    log: Mutex<Vec<Ev>>,
    inflight: AtomicU64,
    done: AtomicU64,
}

/// A reader beside the workers -- the side jobs', the end's sweep -- whose
/// reads the checker holds as it holds theirs.
struct Reader {
    log: Mutex<Vec<Ev>>,
    inflight: AtomicU64,
}

/// A subscription's thread's own.
struct Stream {
    from: u64,
    log: Mutex<Vec<SEv>>,
    /// When its connection opened (`u64::MAX` while it has none), its
    /// connections so far, and the latest change it read.
    open_since: AtomicU64,
    conns: AtomicU64,
    seen: AtomicU64,
}

/// What the workers, the subscribers and the minute's look share.
struct Shared {
    t0: Instant,
    stop: AtomicBool,
    addr: String,
    raddr: String,
    rate: f64,
    threads: usize,
    records: u64,
    docs: u64,
    counters: u64,
    dim: usize,
    ttl: u64,
    ops: AtomicU64,
    errors: AtomicU64,
    refused: AtomicU64,
    down: AtomicU64,
    events: AtomicU64,
    /// Each restart's window: announced a second before the kill, the
    /// kill, and the server answering again (`u64::MAX` until it has), in
    /// ns from `t0`.
    windows: Mutex<Vec<(u64, u64, u64)>>,
    /// The latest restart's and compact's windows, for the latencies.
    restart_from: AtomicU64,
    restart_to: AtomicU64,
    compact_from: AtomicU64,
    compact_to: AtomicU64,
    /// The latest change a write was answered as: what a read through the
    /// replica is sent after.
    max_seq: AtomicU64,
    /// Inserted keys: the next to hand out, and those inserted and not yet
    /// deleted, oldest first.
    fifo: Mutex<(u64, VecDeque<u64>)>,
    /// Sessions inserted, with when, kept past their time for reads.
    sessions: Mutex<VecDeque<(u64, u64)>>,
    next_session: AtomicU64,
    /// Docs written: id and span, for the minute's sample to leave out.
    doc_writes: Mutex<Vec<(u64, u64, u64)>>,
    slots: Vec<Slot>,
    readers: Vec<Reader>,
    streams: Vec<Stream>,
    notes: Mutex<Vec<String>>,
}

impl Shared {
    fn ns(&self) -> u64 {
        self.t0.elapsed().as_nanos() as u64
    }

    /// Whether `t` falls in a restart's window, from the second before its
    /// kill to `GRACE` after the server answered again.
    fn restarting(&self, t: u64) -> bool {
        self.windows
            .lock()
            .unwrap()
            .iter()
            .any(|&(from, _, back)| t >= from && t <= back.saturating_add(GRACE))
    }

    fn note(&self, what: String) {
        let mut n = self.notes.lock().unwrap();
        if n.len() < 200 {
            eprintln!("soak: {what}");
            n.push(what);
        }
    }

    /// Counts what met a restart -- a connection refused or cut, a 503
    /// saying when to come back -- inside a restart's window as `down`, and
    /// anything else, or the same outside one, as an error.
    fn failed(&self, t: u64, restart_kind: bool, what: impl FnOnce() -> String) {
        if restart_kind && self.restarting(t) {
            self.down.fetch_add(1, Ordering::Relaxed);
        } else {
            self.errors.fetch_add(1, Ordering::Relaxed);
            self.note(what());
        }
    }

    fn kills(&self) -> Vec<(u64, u64)> {
        self.windows
            .lock()
            .unwrap()
            .iter()
            .map(|&(_, k, back)| (k, back))
            .collect()
    }
}

/// What a request came to.
enum Answer {
    Ok(Vec<u8>, Option<u64>),
    /// Refused by its status: counted already.
    Refused(u16),
    /// Not answered -- the connection refused or cut, a 503 -- so a write
    /// may or may not have landed.
    Unknown,
    Stopped,
}

struct Worker<'a> {
    sh: &'a Shared,
    slot: &'a Slot,
    c: Option<http::Http>,
    rc: Option<http::Http>,
    rng: Rng,
}

impl Worker<'_> {
    /// One request, to the primary or the replica, sent again where it
    /// reads (`again`) until answered: across a restart a read waits for
    /// the server, its time counting the wait, and a write is not sent
    /// twice.
    fn ask(
        &mut self,
        replica: bool,
        text: &str,
        params: &str,
        headers: &str,
        again: bool,
    ) -> Answer {
        let mut counted = false;
        loop {
            if self.sh.stop.load(Ordering::Relaxed) {
                return Answer::Stopped;
            }
            let addr = if replica {
                &self.sh.raddr
            } else {
                &self.sh.addr
            };
            let c = if replica { &mut self.rc } else { &mut self.c };
            if c.is_none() {
                *c = http::Http::try_connect(addr, Duration::from_secs(60)).ok();
            }
            let got = match c.as_mut() {
                Some(c) => match c.try_query_with(text, params, headers) {
                    Ok((status, body)) => {
                        let body = body.to_vec();
                        Ok((status, body, c.seq(), c.retry_after()))
                    }
                    Err(e) => Err(e),
                },
                None => Err(std::io::Error::other("no connection")),
            };
            let t = self.sh.ns();
            match got {
                Ok((status, body, seq, _)) if status < 300 => return Answer::Ok(body, seq),
                Ok((503, body, _, true)) => {
                    // A server that cannot take it yet, saying when to come
                    // back: nothing ran.
                    if !counted {
                        self.sh.failed(t, true, || {
                            format!("{text}: 503 {}", String::from_utf8_lossy(&body))
                        });
                        counted = true;
                    }
                    if !again {
                        return Answer::Unknown;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Ok((status, body, _, _)) => {
                    self.sh.failed(t, false, || {
                        format!(
                            "{text}: {status} {}",
                            String::from_utf8_lossy(&body[..body.len().min(200)])
                        )
                    });
                    return Answer::Refused(status);
                }
                Err(e) => {
                    *c = None;
                    if !counted {
                        self.sh.failed(t, true, || format!("{text}: {e}"));
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

    /// A read of a record, from the primary or through the replica, sent
    /// after the latest write answered: the replica answers once it holds
    /// it, so its answer is held to the registers as the primary's is.
    fn read(&mut self, key: u64, replica: bool) -> bool {
        let headers = match replica {
            true => format!(
                "Fennec-After: {}\r\n",
                self.sh.max_seq.load(Ordering::SeqCst)
            ),
            false => String::new(),
        };
        let s = self.sh.ns();
        let Answer::Ok(body, _) = self.ask(
            replica,
            "get usertable where id = $1",
            &key.to_string(),
            &headers,
            true,
        ) else {
            return false;
        };
        let e = self.sh.ns();
        let rows = json_rows(&body);
        if rows.len() > 1 || rows.iter().any(|r| r.0 != key) {
            self.log(Ev::Wrong(format!(
                "a read of {key}{} answered {} rows: {:?}",
                if replica { " through the replica" } else { "" },
                rows.len(),
                rows.iter().map(|r| r.0).collect::<Vec<_>>()
            )));
        }
        let first = rows.iter().find(|r| r.0 == key);
        self.log(Ev::Row {
            key,
            s,
            e,
            found: first.is_some(),
            got: first.map_or([0; FIELDS], |r| r.1),
        });
        true
    }

    /// A write's answer: whether it may not have landed, what it said it
    /// wrote, and the change it was answered as; `None` where it was
    /// refused (4xx: nothing ran) or the run stopped.
    fn written(&mut self, text: &str, params: &str) -> Option<(bool, u64, Option<u64>)> {
        match self.ask(false, text, params, "", false) {
            Answer::Ok(body, seq) => {
                if let Some(q) = seq {
                    self.sh.max_seq.fetch_max(q, Ordering::SeqCst);
                }
                Some((false, affected(&body), seq))
            }
            Answer::Unknown => Some((true, 1, None)),
            Answer::Refused(status) => {
                self.sh.refused.fetch_add(1, Ordering::Relaxed);
                // A 5xx may come after the write ran in memory -- a sync
                // that failed: it may or may not be there.
                (status >= 500).then_some((true, 1, None))
            }
            Answer::Stopped => None,
        }
    }

    fn write(&mut self, key: u64, field: u8, d: u64, text: &str, params: &str) -> bool {
        let s = self.sh.ns();
        let Some((unanswered, affected, seq)) = self.written(text, params) else {
            return false;
        };
        self.log(Ev::Write {
            key,
            field,
            d,
            s,
            e: self.sh.ns(),
            unanswered,
            affected,
            seq,
        });
        !unanswered
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

    fn session_insert(&mut self) -> bool {
        let id = self.sh.next_session.fetch_add(1, Ordering::Relaxed);
        let s = self.sh.ns();
        let params = format!("{id},{}", self.rng.below(1_000_000));
        let Some((unanswered, affected, seq)) =
            self.written("insert sessions {id: $1, user: $2, seen: now()}", &params)
        else {
            return false;
        };
        let e = self.sh.ns();
        if !unanswered && affected != 1 {
            self.log(Ev::Wrong(format!(
                "session {id}'s insert answered {affected}"
            )));
        }
        self.log(Ev::SessionPut {
            id,
            s,
            e,
            unanswered,
            seq,
        });
        let keep = (self.sh.ttl + 120) * 1_000_000_000;
        let mut sessions = self.sh.sessions.lock().unwrap();
        sessions.push_back((id, e));
        while sessions.front().is_some_and(|x| x.1 + keep < e) {
            sessions.pop_front();
        }
        !unanswered
    }

    /// A read of sessions, some past their time and some inside it: one by
    /// id, a range through a filter, or a range counted -- each held to the
    /// registers, which hold the row's absence from its time on.
    fn session_read(&mut self) -> bool {
        let pick = {
            let s = self.sh.sessions.lock().unwrap();
            (!s.is_empty()).then(|| s[self.rng.below(s.len() as u64) as usize].0)
        };
        let Some(id) = pick else {
            let key = self.rng.below(self.sh.records) + 1;
            return self.read(key, false);
        };
        let (from, to) = (id, id + 16);
        let how = self.rng.below(4);
        let s = self.sh.ns();
        let (text, params) = match how {
            0 | 1 => ("get sessions where id = $1", id.to_string()),
            2 => (
                "get sessions where id >= $1 and id < $2",
                format!("{from},{to}"),
            ),
            _ => (
                "get sessions where id >= $1 and id < $2 count",
                format!("{from},{to}"),
            ),
        };
        let Answer::Ok(body, _) = self.ask(false, text, &params, "", true) else {
            return false;
        };
        let e = self.sh.ns();
        match how {
            0 | 1 => {
                let ids = http::ids(&body);
                if ids.iter().any(|&x| x != id as i64) || ids.len() > 1 {
                    self.log(Ev::Wrong(format!("session {id} read answered {ids:?}")));
                }
                self.log(Ev::Session {
                    id,
                    s,
                    e,
                    found: ids.contains(&(id as i64)),
                });
            }
            2 => {
                let ids = http::ids(&body);
                let set: HashSet<i64> = ids.iter().copied().collect();
                if set.len() != ids.len() || ids.iter().any(|&x| x < from as i64 || x >= to as i64)
                {
                    self.log(Ev::Wrong(format!(
                        "sessions {from}..{to} through a filter answered {ids:?}"
                    )));
                }
                for x in from..to {
                    self.log(Ev::Session {
                        id: x,
                        s,
                        e,
                        found: set.contains(&(x as i64)),
                    });
                }
            }
            _ => match count_of(&body) {
                Some(n) => self.log(Ev::Sessions { from, to, s, e, n }),
                None => self.log(Ev::Wrong(format!(
                    "sessions {from}..{to} counted: {}",
                    String::from_utf8_lossy(&body)
                ))),
            },
        }
        true
    }

    fn op(&mut self, value: &mut String, zipf: &mut Scrambled) -> bool {
        let u = self.rng.f64();
        let records = self.sh.records;
        match u {
            u if u < 0.36 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                self.read(key, false)
            }
            u if u < 0.40 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                // Not across a restart: the replica would wait out the
                // primary's absence for the write it is sent after.
                let replica = !self.sh.restarting(self.sh.ns());
                self.read(key, replica)
            }
            u if u < 0.77 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                let f = self.rng.below(FIELDS as u64) as u8;
                self.rng.value(value);
                let params = format!("{key},\"{value}\"");
                let text = format!("set usertable {{field{f}: $2}} where id = $1");
                self.write(key, f, digest(value.as_bytes()), &text, &params)
            }
            u if u < 0.79 => {
                let key = self.rng.below(self.sh.counters) + 1;
                let s = self.sh.ns();
                let Some((unanswered, affected, seq)) =
                    self.written("set counters {n: n + 1} where id = $1", &key.to_string())
                else {
                    return false;
                };
                self.log(Ev::Incr {
                    key,
                    s,
                    e: self.sh.ns(),
                    unanswered,
                    affected,
                    seq,
                });
                !unanswered
            }
            u if u < 0.80 => {
                let key = self.rng.below(self.sh.counters) + 1;
                let s = self.sh.ns();
                let Answer::Ok(body, _) = self.ask(
                    false,
                    "get counters where id = $1",
                    &key.to_string(),
                    "",
                    true,
                ) else {
                    return false;
                };
                let e = self.sh.ns();
                match http::ints(&body, "n").as_slice() {
                    [n] if *n >= 0 => self.log(Ev::Count {
                        key,
                        s,
                        e,
                        n: *n as u64,
                    }),
                    other => self.log(Ev::Wrong(format!("counter {key} answered {other:?}"))),
                }
                true
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
                self.read(key, false)
            }
            u if u < 0.92 => self.session_insert(),
            u if u < 0.94 => self.session_read(),
            u if u < 0.96 => {
                let q = words(&mut self.rng, 2);
                matches!(
                    self.ask(
                        false,
                        "get docs match body $1 limit 10",
                        &format!("\"{q}\""),
                        "",
                        true
                    ),
                    Answer::Ok(..)
                )
            }
            u if u < 0.98 => {
                let v = unit_vector(&mut self.rng, self.sh.dim);
                let Answer::Ok(body, _) =
                    self.ask(false, "get docs near v $1 limit 10", &v, "", true)
                else {
                    return false;
                };
                let n = http::ids(&body).len();
                if n != 10 {
                    self.log(Ev::Wrong(format!("near answered {n} rows of 10")));
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
                let s = self.sh.ns();
                let done = self.written("set docs {body: $2, v: $3} where id = $1", &params);
                // Answered or not, the minute's sample leaves the doc out
                // around it.
                self.sh
                    .doc_writes
                    .lock()
                    .unwrap()
                    .push((id, s, self.sh.ns()));
                matches!(done, Some((false, ..)))
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

/// A `count`'s answer, `[{"count":3}]`.
fn count_of(body: &[u8]) -> Option<u64> {
    match http::ints(body, "count").as_slice() {
        [n] if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn work(sh: &Shared, w: usize) {
    let slot = &sh.slots[w];
    let mut me = Worker {
        sh,
        slot,
        c: None,
        rc: None,
        rng: Rng::new(SEED ^ 0x534F_414B ^ (w as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    };
    let mut zipf = Scrambled::new();
    let mut value = String::with_capacity(super::FIELD_LEN);
    // Open loop: an operation every `every`, the connections' schedules
    // staggered across it.
    let every = (sh.rate > 0.0).then(|| Duration::from_secs_f64(sh.threads as f64 / sh.rate));
    let first = every.map(|e| sh.t0 + e.mul_f64(w as f64 / sh.threads as f64));
    let mut sent = 0u64;
    let within = |t: u64, from: &AtomicU64, to: &AtomicU64| {
        t >= from.load(Ordering::Relaxed) && t <= to.load(Ordering::Relaxed)
    };
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
        slot.done.fetch_add(1, Ordering::Relaxed);
        if ok {
            let at = due.saturating_duration_since(sh.t0).as_nanos() as u64;
            let took = due.elapsed().as_nanos() as u64;
            let mut h = slot.hists.lock().unwrap();
            if within(at, &sh.restart_from, &sh.restart_to) {
                h.restart.record(took);
            } else if within(at, &sh.compact_from, &sh.compact_to) {
                h.compact.record(took);
            } else {
                h.steady.record(took);
            }
            drop(h);
            sh.ops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An SSE `data:` line of a subscription: its change, rows and deletions.
fn stream_data(data: &str, seed: bool) -> Option<SEv> {
    let seq = json_num(data, "seq")?;
    let list = |key: &str| -> Option<&str> {
        let at = data.find(&format!("\"{key}\":["))? + key.len() + 4;
        let end = data[at..].find(']')?;
        Some(&data[at..at + end])
    };
    let rows = list(if seed { "rows" } else { "puts" })
        .map(|r| json_rows(format!("[{r}]").as_bytes()))
        .unwrap_or_default();
    let dels = list("dels")
        .map(|d| d.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    Some(SEv {
        seed,
        part: data.contains("\"part\":"),
        more: data.contains("\"more\":true"),
        seq,
        rows,
        dels,
        ..SEv::default()
    })
}

/// A subscription held open: `GET /usertable/changes` over a range of 64
/// keys, its events logged for the checker as they come, opened again when
/// it ends.
fn subscribe(sh: &Shared, i: usize) {
    let me = &sh.streams[i];
    let query = format!(
        "id%20%3E%3D%20{}%20and%20id%20%3C%20{}",
        me.from,
        me.from + 64
    );
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
            let conn = me.conns.fetch_add(1, Ordering::SeqCst) + 1;
            let at = sh.ns();
            me.open_since.store(at, Ordering::SeqCst);
            let mut r = BufReader::new(s);
            let mut line = String::new();
            let mut head = true;
            let mut event = String::new();
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
                        if let Some(name) = line.strip_prefix("event: ") {
                            event = name.trim().to_string();
                        } else if let Some(data) = line.strip_prefix("data: ") {
                            let e = sh.ns();
                            match stream_data(data.trim(), event == "seed") {
                                Some(mut ev) => {
                                    ev.conn = conn;
                                    ev.opened = at;
                                    ev.e = e;
                                    me.seen.fetch_max(ev.seq, Ordering::SeqCst);
                                    me.log.lock().unwrap().push(ev);
                                    sh.events.fetch_add(1, Ordering::Relaxed);
                                }
                                None => {
                                    sh.failed(e, false, || {
                                        format!(
                                            "subscription {i}: an event read as no change: {data}"
                                        )
                                    });
                                }
                            }
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
            me.open_since.store(u64::MAX, Ordering::SeqCst);
        }
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }
        sh.failed(sh.ns(), true, || format!("subscription {i}: {ended}"));
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ----------------------------------------------------------------- sample

/// A doc as a scan reads it: its words and its vector.
fn doc_rows(body: &[u8]) -> Vec<(u64, String, Vec<f32>)> {
    let text = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    for obj in text.split("{\"id\":").skip(1) {
        let id = obj
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse().ok());
        let body = obj
            .split_once("\"body\":\"")
            .and_then(|(_, r)| r.split_once('"'))
            .map(|(b, _)| b.to_string());
        let v = obj
            .split_once("\"v\":[")
            .and_then(|(_, r)| r.split_once(']'))
            .map(|(v, _)| v.split(',').filter_map(|x| x.trim().parse().ok()).collect());
        if let (Some(id), Some(body), Some(v)) = (id, body, v) {
            out.push((id, body, v));
        }
    }
    out
}

/// The cosine distance `near ... (cosine)` ranks by, in f64.
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    1.0 - dot / (na.sqrt() * nb.sqrt()).max(f64::MIN_POSITIVE)
}

/// A minute's sample of `match` and `near`, held to a scan of the same
/// rows.
#[derive(Default, Clone, Copy, Debug)]
struct Sample {
    matches: u64,
    match_differs: u64,
    nears: u64,
    exact_differs: u64,
    /// `near`'s recall@10 against `exact`, summed over `nears`.
    recall: f64,
    /// Queries left out: a doc written while they ran was in an answer.
    skipped: u64,
}

/// `samples` of each: every doc read (the scan), then `match` as a filter
/// (`order id`: every row holding a word of the query) held to the docs
/// the scan read holding one, its ranked page held to be among them; and
/// `near` held two ways -- `exact`, the server's own scan, to the ranking
/// over the vectors read here, to the last digits of the f32 sums, and the
/// `@hnsw` walk to `exact` by recall. A doc written while it ran is left
/// out of the comparison, and a query whose answer held one is skipped.
fn sample(sh: &Shared, rng: &mut Rng, samples: u64) -> Option<Sample> {
    let mut c = http::Http::try_connect(&sh.addr, Duration::from_secs(60)).ok()?;
    let s1 = sh.ns();
    let mut docs = Vec::new();
    let page = 5_000;
    let mut from = 1;
    while from <= sh.docs {
        let (status, body) = c
            .try_query(
                "get docs where id >= $1 and id < $2",
                &format!("{from},{}", from + page),
            )
            .ok()?;
        if status != 200 {
            return None;
        }
        docs.extend(doc_rows(body));
        from += page;
    }
    let mut out = Sample::default();
    for _ in 0..samples {
        let q = words(rng, 2);
        let s = c
            .try_query("get docs match body $1 order id", &format!("\"{q}\""))
            .ok()
            .filter(|x| x.0 == 200)
            .map(|x| http::ids(x.1))?;
        let ranked = c
            .try_query("get docs match body $1 limit 10", &format!("\"{q}\""))
            .ok()
            .filter(|x| x.0 == 200)
            .map(|x| http::ids(x.1))?;
        let written = written_docs(sh, s1);
        let terms: Vec<&str> = q.split(' ').collect();
        let scan: HashSet<i64> = docs
            .iter()
            .filter(|d| d.1.split(' ').any(|w| terms.contains(&w)))
            .map(|d| d.0 as i64)
            .filter(|id| !written.contains(id))
            .collect();
        let filter: HashSet<i64> = s
            .iter()
            .copied()
            .filter(|id| !written.contains(id))
            .collect();
        out.matches += 1;
        if scan != filter
            || ranked
                .iter()
                .any(|id| !s.contains(id) && !written.contains(id))
        {
            out.match_differs += 1;
            sh.note(format!(
                "match {q:?}: {} rows as a filter, {} by the scan, ranked {ranked:?}",
                filter.len(),
                scan.len()
            ));
        }
    }
    for _ in 0..samples {
        let v: Vec<f32> = (0..sh.dim).map(|_| rng.f64() as f32 * 2.0 - 1.0).collect();
        let vj = http::vector_json(&v);
        let ann = c
            .try_query("get docs near v $1 limit 10", &vj)
            .ok()
            .filter(|x| x.0 == 200)
            .map(|x| http::ids(x.1))?;
        let exact = c
            .try_query("get docs near v $1 exact limit 10", &vj)
            .ok()
            .filter(|x| x.0 == 200)
            .map(|x| http::ids(x.1))?;
        let written = written_docs(sh, s1);
        let mut brute: Vec<(f64, i64)> = docs
            .iter()
            .map(|d| (cosine(&v, &d.2), d.0 as i64))
            .collect();
        brute.sort_by(|a, b| a.0.total_cmp(&b.0));
        let touched = brute
            .iter()
            .take(11)
            .map(|x| x.1)
            .chain(ann.iter().copied())
            .chain(exact.iter().copied())
            .any(|id| written.contains(&id));
        if touched || brute.len() < 10 {
            out.skipped += 1;
            continue;
        }
        out.nears += 1;
        // The f32 sums the server ranks by against f64 here: equal up to
        // their last digits, so a doc within 1e-5 of the tenth is the tenth.
        let tenth = brute[9].0;
        let far = exact.iter().any(|id| {
            brute
                .iter()
                .find(|x| x.1 == *id)
                .is_none_or(|x| x.0 > tenth + 1e-5)
        });
        if exact.len() != 10 || far {
            out.exact_differs += 1;
            sh.note(format!(
                "near exact answered {exact:?}, the scan's ten {:?}",
                brute.iter().take(10).map(|x| x.1).collect::<Vec<_>>()
            ));
        }
        out.recall += ann.iter().filter(|id| exact.contains(id)).count() as f64 / 10.0;
    }
    Some(out)
}

/// The docs a write may have changed since `from`, a write not answered
/// included.
fn written_docs(sh: &Shared, from: u64) -> HashSet<i64> {
    sh.doc_writes
        .lock()
        .unwrap()
        .iter()
        .filter(|&&(_, _, e)| e >= from)
        .map(|&(id, _, _)| id as i64)
        .collect()
}

// -------------------------------------------------------------------- run

/// What the minute's line counts of the run's own doings.
#[derive(Default)]
struct Doings {
    kills: AtomicU64,
    deaths: AtomicU64,
    oom: AtomicU64,
    verifies: AtomicU64,
    verify_failed: AtomicU64,
    backups: AtomicU64,
    restores: AtomicU64,
    backup_failed: AtomicU64,
    compacts: AtomicU64,
    compact_failed: AtomicU64,
    compact_killed: AtomicU64,
    restart_ms: AtomicU64,
    swept: AtomicU64,
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

/// A job beside the load, one at a time in the order asked.
enum Job {
    Compact,
    Backup,
    /// Read these records back, after a restart.
    Sweep(Vec<u64>),
    /// Answered once every job before it has ended.
    Fence(mpsc::Sender<()>),
}

/// What the side thread needs of the run.
struct Side {
    sh: Arc<Shared>,
    doings: Arc<Doings>,
    server: String,
    net: String,
    tools: String,
    volume: String,
    token: String,
}

impl Side {
    fn compact(&self) {
        let sh = &self.sh;
        sh.compact_to.store(u64::MAX, Ordering::SeqCst);
        sh.compact_from.store(sh.ns(), Ordering::SeqCst);
        let done = http::Http::try_connect(&sh.addr, Duration::from_secs(3600))
            .and_then(|mut c| c.try_query("compact usertable", "").map(|(s, _)| s));
        sh.compact_to.store(sh.ns(), Ordering::SeqCst);
        match done {
            Ok(200) => {
                self.doings.compacts.fetch_add(1, Ordering::Relaxed);
            }
            // Cut by a kill the schedule landed in it: what the run means
            // it to meet, not a failure -- the file must verify after.
            Err(_) if sh.restarting(sh.ns()) => {
                self.doings.compact_killed.fetch_add(1, Ordering::Relaxed);
            }
            other => {
                sh.note(format!("compact failed: {other:?}"));
                self.doings.compact_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A backup over the replication stream into an archive, restored into
    /// a file, the file verified: each step said on its own.
    fn backup(&self) {
        let script = format!(
            "rm -rf /data/archive /data/restored.fennec; \
             /tools/fennec backup http://{}:8080 /data/archive --token {} && echo BACKUP-OK && \
             /tools/fennec restore /data/archive /data/restored.fennec && echo RESTORE-OK && \
             /tools/fennec verify /data/restored.fennec && echo VERIFY-OK; \
             rm -rf /data/archive /data/restored.fennec",
            self.server, self.token
        );
        let t = Instant::now();
        let (_, out) = docker(&[
            "run",
            "--rm",
            "--name",
            &format!("{}-backup", self.net.trim_end_matches("-net")),
            "--network",
            &self.net,
            "-v",
            &format!("{}:/tools:ro", self.tools),
            "-v",
            &format!("{}:/data", self.volume),
            "alpine",
            "sh",
            "-c",
            &script,
        ]);
        let backed = out.contains("BACKUP-OK");
        let restored = out.contains("RESTORE-OK") && out.contains("VERIFY-OK");
        eprintln!(
            "soak: backup {}, restore and verify {} in {:.0} s",
            if backed { "taken" } else { "FAILED" },
            if restored { "clean" } else { "FAILED" },
            t.elapsed().as_secs_f64()
        );
        if backed {
            self.doings.backups.fetch_add(1, Ordering::Relaxed);
        }
        if restored {
            self.doings.restores.fetch_add(1, Ordering::Relaxed);
        }
        if !backed || !restored {
            eprintln!("{out}");
            self.doings.backup_failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Reads `keys` back, a thousand a request, each held as a read.
    fn sweep(&self, keys: Vec<u64>) {
        let sh = &self.sh;
        let me = &sh.readers[0];
        let Ok(mut c) = http::Http::try_connect(&sh.addr, Duration::from_secs(60)) else {
            return;
        };
        for chunk in keys.chunks(1_000) {
            let list = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let s = sh.ns();
            me.inflight.store(s, Ordering::SeqCst);
            let got = c.try_query("get usertable where id in $1", &format!("[{list}]"));
            let e = sh.ns();
            let mut log = me.log.lock().unwrap();
            match got {
                Ok((200, body)) => {
                    let rows = json_rows(body);
                    log.extend(page_reads(&rows, chunk.iter().copied(), s, e, "the sweep"));
                    self.doings
                        .swept
                        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                other => sh.failed(e, false, || {
                    format!("the sweep's read: {:?}", other.map(|x| x.0))
                }),
            }
            drop(log);
            me.inflight.store(u64::MAX, Ordering::SeqCst);
        }
    }

    fn run(self, jobs: mpsc::Receiver<Job>) {
        for job in jobs {
            match job {
                Job::Compact => self.compact(),
                Job::Backup => self.backup(),
                Job::Sweep(keys) => self.sweep(keys),
                Job::Fence(done) => {
                    let _ = done.send(());
                }
            }
        }
    }
}

/// A page of records read by id -- `keys` asked for, `rows` answered --
/// as reads: one for each key asked, there or not, and a row twice or one
/// not asked for wrong on its face.
fn page_reads(
    rows: &[(u64, [u64; FIELDS])],
    keys: impl Iterator<Item = u64>,
    s: u64,
    e: u64,
    what: &str,
) -> Vec<Ev> {
    let mut by: HashMap<u64, [u64; FIELDS]> = HashMap::new();
    let mut out = Vec::new();
    let asked: Vec<u64> = keys.collect();
    let set: HashSet<u64> = asked.iter().copied().collect();
    for &(k, d) in rows {
        if by.insert(k, d).is_some() {
            out.push(Ev::Wrong(format!("{what} answered {k} twice")));
        }
        if !set.contains(&k) {
            out.push(Ev::Wrong(format!("{what} answered {k}, not asked for")));
        }
    }
    for key in asked {
        let got = by.get(&key);
        out.push(Ev::Row {
            key,
            s,
            e,
            found: got.is_some(),
            got: got.copied().unwrap_or([0; FIELDS]),
        });
    }
    out
}

/// The run's settings as its start line has them: what the report holds a
/// run's counts to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    minutes: u64,
    kill_every: u64,
    kill_in_compact_every: u64,
    compact_every: u64,
    backup_every: u64,
    backup_at: u64,
    restart_max: u64,
    lag_max: u64,
    catchup_max: u64,
    recall_min: f64,
    subscriptions: u64,
    threads: u64,
}

impl Settings {
    fn of(o: &Opts) -> Settings {
        Settings {
            minutes: o.minutes,
            kill_every: o.kill_every,
            kill_in_compact_every: o.kill_in_compact_every,
            compact_every: o.compact_every,
            backup_every: o.backup_every,
            backup_at: o.backup_at,
            restart_max: o.restart_max,
            lag_max: o.lag_max,
            catchup_max: o.catchup_max,
            recall_min: o.recall_min,
            subscriptions: o.subscriptions as u64,
            threads: o.threads as u64,
        }
    }

    fn values(&self) -> Vec<(&'static str, String)> {
        vec![
            ("minutes", self.minutes.to_string()),
            ("kill_every", self.kill_every.to_string()),
            (
                "kill_in_compact_every",
                self.kill_in_compact_every.to_string(),
            ),
            ("compact_every", self.compact_every.to_string()),
            ("backup_every", self.backup_every.to_string()),
            ("backup_at", self.backup_at.to_string()),
            ("restart_max", self.restart_max.to_string()),
            ("lag_max", self.lag_max.to_string()),
            ("catchup_max", self.catchup_max.to_string()),
            ("recall_min", format!("{}", self.recall_min)),
            ("subscriptions", self.subscriptions.to_string()),
            ("threads", self.threads.to_string()),
        ]
    }

    fn read(m: &HashMap<String, f64>) -> Option<Settings> {
        let g = |k: &str| m.get(k).copied();
        Some(Settings {
            minutes: g("minutes")? as u64,
            kill_every: g("kill_every")? as u64,
            kill_in_compact_every: g("kill_in_compact_every")? as u64,
            compact_every: g("compact_every")? as u64,
            backup_every: g("backup_every")? as u64,
            backup_at: g("backup_at")? as u64,
            restart_max: g("restart_max")? as u64,
            lag_max: g("lag_max")? as u64,
            catchup_max: g("catchup_max")? as u64,
            recall_min: g("recall_min")?,
            subscriptions: g("subscriptions")? as u64,
            threads: g("threads")? as u64,
        })
    }
}

/// The records of every collection read in id order, a page at a time,
/// their bytes digested: the same ordered read on the primary and the
/// replica gives the same digest when they hold the same rows. On the
/// primary (`checker`) every record and counter read is held as a read
/// too, and every session's being there.
fn read_all(
    addr: &str,
    sh: &Shared,
    o: &Opts,
    mut checker: Option<&mut Checker>,
) -> Option<[u64; 3]> {
    let mut c = http::Http::try_connect(addr, Duration::from_secs(600)).ok()?;
    let mut digests = [0u64; 3];
    let last_key = sh.fifo.lock().unwrap().0;
    let tables: [(&str, u64, u64); 3] = [
        ("usertable", last_key, 1_000),
        ("counters", o.counters, 1_000),
        ("docs", o.docs, 2_000),
    ];
    for (t, &(name, last, page)) in tables.iter().enumerate() {
        let mut from = 1;
        while from <= last {
            let to = (from + page).min(last + 1);
            let s = sh.ns();
            let (status, body) = c
                .try_query(
                    &format!("get {name} where id >= $1 and id < $2"),
                    &format!("{from},{to}"),
                )
                .ok()?;
            let e = sh.ns();
            if status != 200 {
                return None;
            }
            digests[t] = (digests[t] ^ digest(body)).wrapping_mul(0x100_0000_01B3);
            if let Some(ch) = checker.as_deref_mut() {
                match name {
                    "usertable" => {
                        let rows = json_rows(body);
                        ch.take(page_reads(&rows, from..to, s, e, "the end's sweep"));
                    }
                    "counters" => {
                        let ids = http::ids(body);
                        let ns = http::ints(body, "n");
                        let mut evs = Vec::new();
                        for key in from..to {
                            match ids.iter().position(|&x| x == key as i64) {
                                Some(i) => evs.push(Ev::Count {
                                    key,
                                    s,
                                    e,
                                    n: ns.get(i).copied().unwrap_or(-1).max(0) as u64,
                                }),
                                None => evs.push(Ev::Wrong(format!(
                                    "the end's sweep found no counter {key}"
                                ))),
                            }
                        }
                        ch.take(evs);
                    }
                    _ => {}
                }
                // Settled only to now: a session's expiry ends in the
                // future, and the reads after this one come before it.
                ch.hold(u64::MAX, sh.ns(), u64::MAX, &[]);
            }
            from = to;
        }
    }
    if let Some(ch) = checker {
        // The sessions' being there: by time, not by the position, so
        // held as reads and kept out of the digest.
        let last = sh.next_session.load(Ordering::Relaxed);
        let mut from = 1;
        while from < last {
            let to = (from + 1_000).min(last);
            let s = sh.ns();
            let (status, body) = c
                .try_query(
                    "get sessions where id >= $1 and id < $2",
                    &format!("{from},{to}"),
                )
                .ok()?;
            let e = sh.ns();
            if status != 200 {
                return None;
            }
            let ids: HashSet<i64> = http::ids(body).into_iter().collect();
            ch.take(
                (from..to)
                    .map(|id| Ev::Session {
                        id,
                        s,
                        e,
                        found: ids.contains(&(id as i64)),
                    })
                    .collect(),
            );
            ch.hold(u64::MAX, sh.ns(), u64::MAX, &[]);
            from = to;
        }
    }
    Some(digests)
}

fn run(o: &Opts) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token: String = {
        let mut rng = Rng::new(started ^ std::process::id() as u64);
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
    assert!(
        bench.wait_health(bench.port, Duration::from_secs(600)),
        "fennec-server did not answer"
    );
    let addr = format!("127.0.0.1:{}", bench.port);
    load(&addr, o);
    bench.start_replica();
    assert!(
        bench.wait_health(bench.replica_port, Duration::from_secs(600)),
        "the replica did not answer"
    );
    let raddr = format!("127.0.0.1:{}", bench.replica_port);
    let seq = |a: &str| {
        super::metrics_page(a).map(|p| metric_sum(p.as_bytes(), "fennec_change_sequence"))
    };
    let caught_up = |within: Duration| -> Option<f64> {
        let t = Instant::now();
        loop {
            let (p, r) = (seq(&addr), seq(&raddr));
            if p.is_some() && p == r {
                return Some(t.elapsed().as_secs_f64());
            }
            if t.elapsed() >= within {
                return None;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    // The replica's first follow copies the loaded file: bounded by half
    // an hour, which a gigabyte over the VM's network takes a fraction of.
    let follow_max = 1_800;
    let follow = caught_up(Duration::from_secs(follow_max));
    eprintln!(
        "soak: the replica {}; {} connections at {}, {} subscriptions, --sync {}, --memory {}",
        match follow {
            Some(s) => format!("followed in {s:.0} s"),
            None => format!("did NOT catch up in {follow_max} s"),
        },
        o.threads,
        match o.rate > 0.0 {
            true => format!("{:.0} ops/s", o.rate),
            false => "the peak".into(),
        },
        o.subscriptions,
        o.sync,
        o.memory
    );
    let settings = Settings::of(o);
    let mut v: Vec<(&str, String)> = settings.values();
    v.extend([
        ("started", started.to_string()),
        ("loss_ms", loss_ms(&o.sync).to_string()),
        ("ttl", o.ttl.to_string()),
        ("slack", o.slack.to_string()),
        ("records", o.records.to_string()),
        ("rate", format!("{:.0}", o.rate)),
        ("follow_s", follow.unwrap_or(-1.0).to_string()),
        ("follow_max", follow_max.to_string()),
    ]);
    append_values(
        &o.out,
        "soak",
        &o.run_id,
        &format!("{}:{}:start", o.sync, o.label),
        &v,
    );

    let ranges: Vec<(u64, u64)> = (0..o.subscriptions)
        .map(|i| {
            let mut rng = Rng::new(SEED ^ 0x5355_4253 ^ i as u64);
            let from = Scrambled::new().next(&mut rng, o.records.saturating_sub(64)) + 1;
            (from, from + 64)
        })
        .collect();
    let sh = Arc::new(Shared {
        t0: Instant::now(),
        stop: AtomicBool::new(false),
        addr: addr.clone(),
        raddr: raddr.clone(),
        rate: o.rate,
        threads: o.threads,
        records: o.records,
        docs: o.docs,
        counters: o.counters,
        dim: o.dim,
        ttl: o.ttl,
        ops: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        refused: AtomicU64::new(0),
        down: AtomicU64::new(0),
        events: AtomicU64::new(0),
        windows: Mutex::new(Vec::new()),
        restart_from: AtomicU64::new(u64::MAX),
        restart_to: AtomicU64::new(u64::MAX),
        compact_from: AtomicU64::new(u64::MAX),
        compact_to: AtomicU64::new(u64::MAX),
        max_seq: AtomicU64::new(0),
        fifo: Mutex::new((o.records, VecDeque::new())),
        sessions: Mutex::new(VecDeque::new()),
        next_session: AtomicU64::new(1),
        doc_writes: Mutex::new(Vec::new()),
        slots: (0..o.threads)
            .map(|_| Slot {
                hists: Mutex::new(Hists::new()),
                log: Mutex::new(Vec::new()),
                inflight: AtomicU64::new(u64::MAX),
                done: AtomicU64::new(0),
            })
            .collect(),
        readers: vec![Reader {
            log: Mutex::new(Vec::new()),
            inflight: AtomicU64::new(u64::MAX),
        }],
        streams: ranges
            .iter()
            .map(|&(from, _)| Stream {
                from,
                log: Mutex::new(Vec::new()),
                open_since: AtomicU64::new(u64::MAX),
                conns: AtomicU64::new(0),
                seen: AtomicU64::new(0),
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
    let (jobs, side_jobs) = mpsc::channel::<Job>();
    let side = Side {
        sh: Arc::clone(&sh),
        doings: Arc::clone(&doings),
        server: bench.server(),
        net: bench.net(),
        tools: bench.tools.clone(),
        volume: bench.volume("backup-data"),
        token: bench.token.clone(),
    };
    let side = std::thread::spawn(move || side.run(side_jobs));

    let loss = loss_ms(&o.sync) * 1_000_000;
    let mut checker = Checker::new(
        o.records,
        Kills {
            at: Vec::new(),
            loss,
        },
        o.ttl * 1_000_000_000,
        o.slack * 1_000_000_000,
        &ranges,
    );
    let mut sample_rng = Rng::new(SEED ^ 0x5A4D);
    let mut hours: VecDeque<Hist> = VecDeque::new();
    let mut restarts = Hist::new();
    let mut prev_majflt = None;
    let mut prev_oom = 0;
    let mut auto_before = 0u64;
    let mut auto_last = 0u64;
    let mut totals = (0u64, 0u64, 0u64);
    let mut peak = Vec::new();
    let mut last = sh.t0;
    let mut life = 0u64;
    let mut env_failed = None;
    let mut faults_before = StreamFaults::default();
    let mut minutes_run = 0;
    let mut restart_ms_max = 0u64;
    // Kills the restart's sweep has not read back after yet.
    let mut swept_after = 0usize;
    let floor = o.disk_floor;
    for m in 1..=o.minutes {
        let at = sh.t0 + Duration::from_secs(60 * m);
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
        minutes_run = m;
        let now = Instant::now();
        let secs = now.duration_since(last).as_secs_f64();
        last = now;
        // The latencies of the minute, and of the hour to it: the steady
        // operations apart from those due in a restart's or a compact's
        // window.
        let mut h = Hists::new();
        let mut idle = 0;
        for s in &sh.slots {
            let x = std::mem::replace(&mut *s.hists.lock().unwrap(), Hists::new());
            h.merge(&x);
            idle += (s.done.swap(0, Ordering::Relaxed) == 0) as u64;
        }
        restarts.merge(&h.restart);
        let ops = sh.ops.swap(0, Ordering::Relaxed);
        if hours.len() == 60 {
            hours.pop_front();
        }
        let mut minute = Hist::new();
        minute.merge(&h.steady);
        hours.push_back(minute);
        let mut hour = Hist::new();
        for x in &hours {
            hour.merge(x);
        }
        // The checker: every read that ended before the earliest
        // operation in flight, what no later read needs let go of, past
        // the window a kill could still take back.
        let now_ns = sh.ns();
        let safe = sh
            .slots
            .iter()
            .map(|s| s.inflight.load(Ordering::SeqCst))
            .chain(sh.readers.iter().map(|r| r.inflight.load(Ordering::SeqCst)))
            .min()
            .unwrap_or(u64::MAX)
            .min(now_ns);
        // The kills first: a write taken in is the restart sweep's when
        // it ended in the 10 s before one.
        checker.regs.kills.at = sh.kills();
        for s in &sh.slots {
            checker.take(std::mem::take(&mut *s.log.lock().unwrap()));
        }
        for r in &sh.readers {
            checker.take(std::mem::take(&mut *r.log.lock().unwrap()));
        }
        for (i, st) in sh.streams.iter().enumerate() {
            let evs = std::mem::take(&mut *st.log.lock().unwrap());
            checker.spending.extend(evs.into_iter().map(|e| (i, e)));
        }
        let open: Vec<(u64, u64)> = sh
            .streams
            .iter()
            .map(|s| {
                (
                    s.open_since.load(Ordering::SeqCst),
                    s.seen.load(Ordering::SeqCst),
                )
            })
            .collect();
        let before = checker.mismatches;
        checker.hold(safe, safe.saturating_sub(loss + HELD_FOR), now_ns, &open);
        let mismatches = checker.mismatches - before;
        let f = checker.faults;
        let faults = StreamFaults {
            missing: f.missing - faults_before.missing,
            twice: f.twice - faults_before.twice,
            order: f.order - faults_before.order,
            late: f.late - faults_before.late,
            silent: f.silent - faults_before.silent,
        };
        faults_before = f;
        // The keys a kill since the last sweep may have touched, read back
        // now that the writes around it are in.
        let kills_now = sh.windows.lock().unwrap().len();
        if kills_now > swept_after {
            swept_after = kills_now;
            let keys: Vec<u64> = std::mem::take(&mut checker.touched).into_iter().collect();
            if !keys.is_empty() {
                let _ = jobs.send(Job::Sweep(keys));
            }
        } else {
            checker.touched.clear();
        }
        let errors = sh.errors.swap(0, Ordering::Relaxed);
        let down = sh.down.swap(0, Ordering::Relaxed);
        totals.0 += ops;
        totals.1 += errors;
        totals.2 += mismatches;
        // The servers, as their processes and their metrics have them.
        let look = bench.look(&bench.server(), "soak.fennec");
        let rlook = bench.look(&bench.replica(), "replica.fennec");
        let page = super::metrics_page(&addr).unwrap_or_default();
        let rpage = super::metrics_page(&raddr).unwrap_or_default();
        let status = bench.status(bench.replica_port).unwrap_or_default();
        let auto = metric_sum(page.as_bytes(), "fennec_auto_compactions_total");
        if auto < auto_last {
            // Started again: its count with it.
            auto_before += auto_last;
        }
        auto_last = auto;
        let compactions = auto_before + auto + doings.compacts.load(Ordering::Relaxed);
        let behind = json_num(&status, "behind");
        // About half the mix writes: the replica's lag in seconds of them.
        let write_rate = ops as f64 / secs * 0.5;
        let lag_s = behind.map_or(-1.0, |b| b as f64 / write_rate.max(1.0));
        let (load1, load5) = load_average();
        let sample = match o.samples > 0 && !o.peak {
            true => sample(&sh, &mut sample_rng, o.samples),
            false => None,
        };
        sh.doc_writes
            .lock()
            .unwrap()
            .retain(|w| w.2 + 120_000_000_000 > now_ns);
        let hfree = host_free(&o.out);
        let vfree = look.and_then(|l| l.vm_free);
        let mut v: Vec<(&str, String)> = vec![
            ("started", started.to_string()),
            ("life", life.to_string()),
            ("secs", format!("{secs:.1}")),
            ("ops", ops.to_string()),
            ("ops_s", format!("{:.0}", ops as f64 / secs)),
            ("rate", format!("{:.0}", o.rate)),
            ("idle_workers", idle.to_string()),
            ("p50", format!("{:.0}", h.steady.pct(0.5))),
            ("p99", format!("{:.0}", h.steady.pct(0.99))),
            ("p999", format!("{:.0}", h.steady.pct(0.999))),
            ("max", format!("{:.0}", h.steady.max as f64 / 1e3)),
            ("p99_hour", format!("{:.0}", hour.pct(0.99))),
            ("restart_ops", h.restart.n.to_string()),
            ("p99_restart", format!("{:.0}", h.restart.pct(0.99))),
            ("compact_ops", h.compact.n.to_string()),
            ("p99_compact", format!("{:.0}", h.compact.pct(0.99))),
            ("errors", errors.to_string()),
            ("refused", sh.refused.swap(0, Ordering::Relaxed).to_string()),
            ("down", down.to_string()),
            ("mismatches", mismatches.to_string()),
            ("stream_missing", faults.missing.to_string()),
            ("stream_twice", faults.twice.to_string()),
            ("stream_order", faults.order.to_string()),
            ("stream_late", faults.late.to_string()),
            ("stream_silent", faults.silent.to_string()),
            ("events", sh.events.swap(0, Ordering::Relaxed).to_string()),
            (
                "streams",
                open.iter().filter(|x| x.0 != u64::MAX).count().to_string(),
            ),
            (
                "live",
                metric_sum(page.as_bytes(), "fennec_data_bytes").to_string(),
            ),
            ("compactions", compactions.to_string()),
            (
                "memory_bytes",
                metric_sum(page.as_bytes(), "fennec_memory_bytes").to_string(),
            ),
            (
                "connections",
                metric_sum(page.as_bytes(), "fennec_connections").to_string(),
            ),
            (
                "r_connections",
                metric_sum(rpage.as_bytes(), "fennec_connections").to_string(),
            ),
            ("lag", behind.map_or(-1, |b| b as i64).to_string()),
            ("lag_s", format!("{lag_s:.2}")),
            (
                "r_connected",
                (status.contains("\"connected\":true") as u8).to_string(),
            ),
            (
                "sessions",
                page.lines()
                    .find(|l| l.starts_with("fennec_documents{collection=\"sessions\"}"))
                    .and_then(|l| l.rsplit(' ').next())
                    .unwrap_or("0")
                    .to_string(),
            ),
            ("host_free", hfree.map_or(-1, |b| b as i64).to_string()),
            ("vm_free", vfree.map_or(-1, |b| b as i64).to_string()),
            ("load1", format!("{load1:.2}")),
            ("load5", format!("{load5:.2}")),
        ];
        if let Some(s) = sample {
            v.extend([
                ("match_n", s.matches.to_string()),
                ("match_differs", s.match_differs.to_string()),
                ("near_n", s.nears.to_string()),
                ("exact_differs", s.exact_differs.to_string()),
                ("recall_sum", format!("{:.2}", s.recall)),
                ("sample_skipped", s.skipped.to_string()),
            ]);
        }
        if let Some(l) = look {
            let majflt = match prev_majflt {
                Some(p) if l.majflt >= p => l.majflt - p,
                _ => l.majflt,
            };
            prev_majflt = Some(l.majflt);
            let oom = match l.cgroup.oom_kill >= prev_oom {
                true => l.cgroup.oom_kill - prev_oom,
                false => l.cgroup.oom_kill,
            };
            prev_oom = l.cgroup.oom_kill;
            doings.oom.fetch_add(oom, Ordering::Relaxed);
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
        if let Some(l) = rlook {
            v.extend([
                ("r_file", l.file.to_string()),
                ("r_rss_anon", l.rss_anon.to_string()),
                ("r_threads", l.threads.to_string()),
                ("r_fds", l.fds.to_string()),
            ]);
        }
        // A server gone that the run did not kill: its death and whether
        // the kernel killed it for its memory, on this minute's line.
        let (up, oom_killed) = bench.state(&bench.server());
        let (rup, _) = bench.state(&bench.replica());
        if oom_killed {
            doings.oom.fetch_add(1, Ordering::Relaxed);
        }
        for (name, n) in [
            ("kill", &doings.kills),
            ("died", &doings.deaths),
            ("oom", &doings.oom),
            ("verify", &doings.verifies),
            ("verify_failed", &doings.verify_failed),
            ("backup", &doings.backups),
            ("restore", &doings.restores),
            ("backup_failed", &doings.backup_failed),
            ("compact", &doings.compacts),
            ("compact_failed", &doings.compact_failed),
            ("compact_killed", &doings.compact_killed),
            ("restart_ms", &doings.restart_ms),
            ("swept", &doings.swept),
        ] {
            v.push((name, n.swap(0, Ordering::Relaxed).to_string()));
        }
        v.push(("r_died", (!rup as u8).to_string()));
        // The disk on both sides: one short of the floor ends the run as
        // the environment's, before a write fails for it.
        let short = [("this host", hfree), ("Docker's VM", vfree)]
            .into_iter()
            .find(|(_, free)| free.is_some_and(|b| b < floor));
        if let Some((side_name, free)) = short {
            env_failed = Some(format!(
                "minute {m}: {side_name} has {} MB free, under the floor of {} MB",
                free.unwrap_or(0) >> 20,
                floor >> 20
            ));
            v.push(("env_failed", "1".into()));
        }
        let cell = format!("{}:{}:m{m:04}", o.sync, o.label);
        append_values(&o.out, "soak", &o.run_id, &cell, &v);
        eprintln!(
            "soak {m:>4} min: {:.0} ops/s, p99 {:.0} us, p99.9 {:.0}, max {:.0} ms; {errors} errors, {down} at a restart, {mismatches} mismatches; file {} MB, live {} MB, anon {} MB, lag {lag_s:.1} s, load {load1:.1}",
            ops as f64 / secs,
            h.steady.pct(0.99),
            h.steady.pct(0.999),
            h.steady.max as f64 / 1e6,
            look.map(|l| l.file >> 20).unwrap_or(0),
            metric_sum(page.as_bytes(), "fennec_data_bytes") >> 20,
            look.map(|l| l.rss_anon >> 20).unwrap_or(0),
        );
        if let Some(why) = &env_failed {
            eprintln!("soak: the environment failed the run: {why}");
            break;
        }
        if o.peak && m > 1 {
            peak.push(ops as f64 / secs);
        }
        if !rup {
            sh.note(format!("the replica was found stopped at minute {m}"));
            let _ = docker(&["start", &bench.replica()]);
        }
        if !up {
            // Died between looks: from its last look on, an answered
            // write may be gone -- held as a kill from there.
            sh.note(format!(
                "the server was found stopped at minute {m}{}",
                if oom_killed { ", OOM-killed" } else { "" }
            ));
            doings.deaths.fetch_add(1, Ordering::Relaxed);
            let from = now_ns.saturating_sub(60_000_000_000);
            restart(&bench, &sh, &doings, from, from, &mut restart_ms_max);
            life += 1;
            prev_majflt = None;
            prev_oom = 0;
        }
        // The minute's doings, after its line: their cost shows in the next.
        for d in schedule(
            m,
            o.minutes,
            o.kill_every,
            o.kill_in_compact_every,
            o.compact_every,
            o.backup_every,
            o.backup_at,
        ) {
            match d {
                Doing::Compact => {
                    let _ = jobs.send(Job::Compact);
                }
                Doing::Backup => {
                    let _ = jobs.send(Job::Backup);
                }
                Doing::Kill { in_compact } => {
                    // One at a time: a kill waits for what is queued.
                    let (done, wait) = mpsc::channel();
                    let _ = jobs.send(Job::Fence(done));
                    let _ = wait.recv();
                    let from = sh.ns();
                    sh.windows.lock().unwrap().push((from, u64::MAX, u64::MAX));
                    sh.restart_to.store(u64::MAX, Ordering::SeqCst);
                    sh.restart_from.store(from, Ordering::SeqCst);
                    if in_compact {
                        // The compact the kill lands in: asked, given a
                        // second to be under way, the kill after.
                        let _ = jobs.send(Job::Compact);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    // The announced second: operations due from here on
                    // are the restart's.
                    std::thread::sleep(Duration::from_secs(1));
                    let k = sh.ns();
                    restart(&bench, &sh, &doings, from, k, &mut restart_ms_max);
                    doings.kills.fetch_add(1, Ordering::Relaxed);
                    life += 1;
                    prev_majflt = None;
                    prev_oom = 0;
                }
            }
        }
    }

    // The end: the load stopped, every read held, every key read back, the
    // replica caught up and its rows held to the primary's by the same
    // ordered read, both stopped as a supervisor stops them and their files
    // verified.
    sh.stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    drop(jobs);
    side.join().unwrap();
    for s in &sh.slots {
        checker.take(std::mem::take(&mut *s.log.lock().unwrap()));
    }
    for r in &sh.readers {
        checker.take(std::mem::take(&mut *r.log.lock().unwrap()));
    }
    for (i, st) in sh.streams.iter().enumerate() {
        let evs = std::mem::take(&mut *st.log.lock().unwrap());
        checker.spending.extend(evs.into_iter().map(|e| (i, e)));
    }
    checker.regs.kills.at = sh.kills();
    let before = checker.mismatches;
    // Every log is in (the load stopped): every read is held, and the
    // registers settled to now -- the end's reads come after it, and a
    // session's expiry may end after them.
    checker.hold(u64::MAX, sh.ns(), u64::MAX, &[]);
    let t = Instant::now();
    let primary = match env_failed {
        None => read_all(&addr, &sh, o, Some(&mut checker)),
        Some(_) => None,
    };
    let swept_s = t.elapsed().as_secs_f64();
    let late = checker.mismatches - before;
    let catchup = match env_failed {
        None => caught_up(Duration::from_secs(o.catchup_max)),
        Some(_) => None,
    };
    let replica = match (catchup, &primary) {
        (Some(_), Some(_)) => read_all(&raddr, &sh, o, None),
        _ => None,
    };
    let same = primary.is_some() && primary == replica;
    if !same {
        eprintln!(
            "soak: the replica's rows {} the primary's (usertable, counters, docs: {replica:?} against {primary:?})",
            if replica.is_none() { "were not read beside" } else { "DIFFER from" }
        );
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
        ("started", started.to_string()),
        ("minutes", minutes_run.to_string()),
        ("ops", totals.0.to_string()),
        ("errors", totals.1.to_string()),
        ("mismatches", (totals.2 + late).to_string()),
        ("reads_held", checker.reads.to_string()),
        ("writes_held", checker.writes.to_string()),
        ("kills", sh.windows.lock().unwrap().len().to_string()),
        (
            "swept",
            match primary {
                Some(_) => (sh.fifo.lock().unwrap().0 + o.counters).to_string(),
                None => "0".into(),
            },
        ),
        ("sweep_s", format!("{swept_s:.0}")),
        ("keys", (sh.fifo.lock().unwrap().0 + o.counters).to_string()),
        ("catchup_s", catchup.unwrap_or(-1.0).to_string()),
        ("p99_restart", format!("{:.0}", restarts.pct(0.99))),
        ("restart_ms_max", restart_ms_max.to_string()),
        ("verify_failed", (!primary_ok as u8).to_string()),
        ("replica_verify_failed", (!replica_ok as u8).to_string()),
        ("replica_differs", (!same as u8).to_string()),
        (
            "memory_limit",
            super::bytes_of(&o.memory).unwrap_or(0).to_string(),
        ),
        ("records", o.records.to_string()),
        ("env_failed", (env_failed.is_some() as u8).to_string()),
    ];
    if o.peak && !peak.is_empty() {
        peak.sort_by(|a, b| a.total_cmp(b));
        let p = peak[peak.len() / 2];
        v.push(("peak_ops_s", format!("{p:.0}")));
        println!("peak {p:.0}");
    }
    append_values(&o.out, "soak", &o.run_id, &cell, &v);
    eprintln!(
        "soak: {minutes_run} minutes, {} operations, {} errors, {} mismatches ({} reads and {} writes held), the files {} and {}, the replica {}{}",
        totals.0,
        totals.1,
        totals.2 + late,
        checker.reads,
        checker.writes,
        if primary_ok { "clean" } else { "NOT CLEAN" },
        if replica_ok { "clean" } else { "NOT CLEAN" },
        if same { "the same" } else { "DIFFERENT or not compared" },
        match &env_failed {
            Some(why) => format!("; the ENVIRONMENT failed it: {why}"),
            None => String::new(),
        }
    );
    drop(bench);
}

/// The server killed (`kill -9`) -- or found dead -- its file verified,
/// started again and waited for: the restart's window closed when it
/// answers. `from` is when the window began (a second before the kill, or
/// the last look before a death), `k` the kill.
fn restart(bench: &Bench, sh: &Shared, doings: &Doings, from: u64, k: u64, longest: &mut u64) {
    {
        let mut w = sh.windows.lock().unwrap();
        match w.last_mut() {
            Some(x) if x.0 == from && x.1 == u64::MAX => x.1 = k,
            _ => w.push((from, k, u64::MAX)),
        }
    }
    sh.restart_to.store(u64::MAX, Ordering::SeqCst);
    sh.restart_from.store(from, Ordering::SeqCst);
    docker(&["kill", "--signal", "KILL", &bench.server()]);
    docker(&["wait", &bench.server()]);
    let t = Instant::now();
    let (ok, out) = bench.fennec(
        &format!("{}-verify", bench.name),
        &bench.volume("data"),
        "/tools/fennec verify /data/soak.fennec",
    );
    eprintln!(
        "soak: killed; fennec verify {} in {:.0} s",
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
    // However long it takes: the report holds it to `--restart-max`.
    assert!(
        bench.wait_health(bench.port, Duration::from_secs(3600)),
        "fennec-server did not answer within an hour of its restart"
    );
    let back = sh.ns();
    sh.restart_to.store(back, Ordering::SeqCst);
    sh.windows.lock().unwrap().last_mut().unwrap().2 = back;
    let ms = (back - k) / 1_000_000;
    doings.restart_ms.fetch_max(ms, Ordering::Relaxed);
    *longest = (*longest).max(ms);
    eprintln!(
        "soak: answering again {:.1} s after the kill",
        ms as f64 / 1e3
    );
}

// ----------------------------------------------------------------- report

/// One criterion's verdict: its name, whether it passed, and its figures
/// with the minutes that broke it.
type Verdict = (&'static str, bool, String);

/// A run judged: its attempt (`started`), its minutes, whether it ended,
/// the environment's failure if one ended it, the verdicts, and the
/// minutes the host was busy.
struct Judged {
    run: String,
    label: String,
    attempts: usize,
    minutes: u64,
    finished: bool,
    env: Option<String>,
    verdicts: Vec<Verdict>,
    busy: Vec<u64>,
}

pub const CRITERIA: [&str; 19] = [
    "8 hours and an end line",
    "ran its planned minutes",
    "every minute has operations from every connection",
    "last hour's steady p99 <= 1.25x the first's",
    "file <= 2.2x live every minute",
    "primary's anonymous memory < +5% within each life",
    "replica's anonymous memory < +5% after the first hour",
    "threads less connections within 2 of their median",
    "descriptors less connections within 2 of their median",
    "no error",
    "no mismatch",
    "every stream held",
    "the replica followed, caught up and holds the primary's rows",
    "every restart within its bound",
    "the schedule's kills, compacts, backups and restores",
    "every verify clean",
    "match and near answer as a scan does",
    "every key read back at the end",
    "no server died, none OOM-killed",
];

fn list(v: &[u64]) -> String {
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
}

fn minutes_said(broke: &[u64]) -> String {
    match broke.is_empty() {
        true => String::new(),
        false => format!("; minutes {}", list(broke)),
    }
}

/// A cell's lines as one append wrote them: run, cell, metrics.
type Block = (String, String, Vec<(String, f64)>);

/// Judges each run in a results file's text (or the one named) by the
/// soak's exit criterion, each attempt of a run id apart and the latest
/// judged.
fn judge(text: &str, only: Option<&str>) -> Vec<Judged> {
    // run -> started -> cell -> metric -> value
    type Cells = HashMap<String, HashMap<String, f64>>;
    let mut runs: Vec<String> = Vec::new();
    let mut data: HashMap<String, BTreeMap<u64, Cells>> = HashMap::new();
    // A line without `started` is one this run wrote before it did: the
    // cell's other lines say which attempt it is.
    // A cell's lines are appended together, one block: the block's
    // `started` says whose attempt it is.
    let mut blocks: Vec<Block> = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 8 || f[4] != "soak" || only.is_some_and(|r| r != f[0]) {
            continue;
        }
        let Ok(x) = f[7].parse::<f64>() else {
            continue;
        };
        match blocks.last_mut() {
            Some((run, cell, m))
                if run == f[0] && cell == f[5] && !m.iter().any(|(k, _)| k == f[6]) =>
            {
                m.push((f[6].into(), x))
            }
            _ => blocks.push((f[0].into(), f[5].into(), vec![(f[6].into(), x)])),
        }
    }
    let mut latest: HashMap<String, u64> = HashMap::new();
    for (run, cell, metrics) in blocks {
        // The script's mark that the environment ended a run (`:env`,
        // `ycsb mark`) is the attempt's that wrote the lines before it.
        let started = match metrics.iter().find(|(k, _)| k == "started") {
            Some(x) => x.1 as u64,
            None if cell.ends_with(":env") => latest.get(&run).copied().unwrap_or(0),
            None => 0,
        };
        latest.insert(run.clone(), started);
        if !runs.contains(&run) {
            runs.push(run.clone());
        }
        data.entry(run)
            .or_default()
            .entry(started)
            .or_default()
            .entry(cell)
            .or_default()
            .extend(metrics);
    }
    let mut out = Vec::new();
    for run in runs {
        let attempts = &data[&run];
        let (_, cells) = attempts.iter().next_back().unwrap();
        out.push(judge_run(&run, attempts.len(), cells));
    }
    out
}

fn judge_run(run: &str, attempts: usize, cells: &HashMap<String, HashMap<String, f64>>) -> Judged {
    let mut minutes: Vec<(u64, &HashMap<String, f64>)> = cells
        .iter()
        .filter_map(|(cell, m)| {
            let n = cell.rsplit(':').next()?.strip_prefix('m')?.parse().ok()?;
            Some((n, m))
        })
        .collect();
    minutes.sort_by_key(|(n, _)| *n);
    let find = |suffix: &str| {
        cells
            .iter()
            .find(|(c, _)| c.ends_with(suffix))
            .map(|(_, m)| m)
    };
    let end = find(":end");
    let start = find(":start");
    let settings = start.and_then(Settings::read);
    let label = cells
        .keys()
        .next()
        .map(|c| c.rsplit_once(':').map_or(c.as_str(), |x| x.0).to_string())
        .unwrap_or_default();
    let get = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
    let n = minutes.len() as u64;
    let busy: Vec<u64> = minutes
        .iter()
        .filter(|(_, m)| get(m, "load1").is_some_and(|x| x >= 4.0))
        .map(|(k, _)| *k)
        .collect();
    let env = minutes
        .iter()
        .find(|(_, m)| get(m, "env_failed") == Some(1.0))
        .map(|(k, m)| {
            format!(
                "at minute {k}: {} MB free on this host, {} MB in Docker's VM",
                get(m, "host_free").unwrap_or(-1.0) as i64 >> 20,
                get(m, "vm_free").unwrap_or(-1.0) as i64 >> 20
            )
        })
        .or_else(|| {
            find(":env")
                .filter(|m| get(m, "env_failed") == Some(1.0))
                .map(|_| {
                    format!(
                        "after minute {}, the script's look (its log says which disk)",
                        minutes.last().map_or(0, |x| x.0)
                    )
                })
        });
    let mut judged = Judged {
        run: run.to_string(),
        label,
        attempts,
        minutes: n,
        finished: end.is_some(),
        env,
        verdicts: Vec::new(),
        busy,
    };
    if judged.env.is_some() {
        return judged;
    }
    let v = &mut judged.verdicts;
    let sum = |k: &str| -> f64 {
        minutes
            .iter()
            .filter_map(|(_, m)| get(m, k))
            .fold(0.0, |a, b| a + b)
    };
    let broke = |f: &dyn Fn(&HashMap<String, f64>) -> bool| -> Vec<u64> {
        minutes
            .iter()
            .filter(|(_, m)| f(m))
            .map(|(k, _)| *k)
            .collect()
    };
    v.push((
        CRITERIA[0],
        n >= 480 && end.is_some(),
        format!(
            "{n} minutes{}",
            if end.is_none() { ", no end line" } else { "" }
        ),
    ));
    let planned = settings.map_or(0, |s| s.minutes);
    v.push((
        CRITERIA[1],
        settings.is_some() && n >= planned && end.is_some(),
        match settings {
            None => "no start line: its settings unknown".into(),
            Some(_) => format!(
                "{n} of {planned} minutes{}",
                if end.is_none() { ", no end line" } else { "" }
            ),
        },
    ));
    // The dead-man: a minute nothing was answered in, or a connection
    // that answered nothing all minute -- a worker stuck or dead.
    let dead = broke(&|m| {
        get(m, "ops").is_none_or(|x| x == 0.0) || get(m, "idle_workers").is_none_or(|x| x > 0.0)
    });
    v.push((
        CRITERIA[2],
        n > 0 && dead.is_empty(),
        format!("{n} minutes{}", minutes_said(&dead)),
    ));
    // The steady part's hour as the run wrote it at minute 60 and at the
    // end: the restarts' windows and the compacts' apart, so that hours
    // with more of them compare alike.
    let at = |k: u64| minutes.iter().rev().find(|(m, _)| *m <= k).map(|x| x.1);
    match (
        at(60).and_then(|m| get(m, "p99_hour")),
        minutes.last().and_then(|(_, m)| get(m, "p99_hour")),
    ) {
        (Some(a), Some(b)) if a > 0.0 && n >= 120 => {
            let over = broke(&|m| get(m, "p99_hour").is_some_and(|x| x > 1.25 * a));
            let over: Vec<u64> = over.into_iter().filter(|k| *k > 60).collect();
            v.push((
                CRITERIA[3],
                b <= 1.25 * a,
                format!(
                    "first {a:.0} us, last {b:.0} us: {:.2}x{}",
                    b / a,
                    match over.is_empty() {
                        true => String::new(),
                        false => format!("; an hour past 1.25x at minutes {}", list(&over)),
                    }
                ),
            ));
        }
        _ => v.push((
            CRITERIA[3],
            false,
            format!("not judged: {n} minutes, two hours needed"),
        )),
    }
    let big = broke(&|m| match (get(m, "file"), get(m, "live")) {
        (Some(f), Some(l)) => f > 2.2 * l,
        _ => true,
    });
    let worst = minutes
        .iter()
        .filter_map(|(_, m)| Some(get(m, "file")? / get(m, "live")?.max(1.0)))
        .fold(0.0f64, f64::max);
    v.push((
        CRITERIA[4],
        n > 0 && big.is_empty(),
        format!("at most {worst:.2}x{}", minutes_said(&big)),
    ));
    // The primary is killed every half hour, so a leak is looked for
    // within each life: its fifth minute against its last.
    let mut lives: BTreeMap<u64, Vec<(u64, f64)>> = BTreeMap::new();
    for (k, m) in &minutes {
        if let (Some(life), Some(a)) = (get(m, "life"), get(m, "rss_anon")) {
            lives.entry(life as u64).or_default().push((*k, a));
        }
    }
    let mut grew = Vec::new();
    let mut most = 0.0f64;
    let mut judged_lives = 0;
    for (life, xs) in &lives {
        if xs.len() < 6 {
            continue;
        }
        judged_lives += 1;
        let (fifth, last) = (xs[4].1, xs[xs.len() - 1].1);
        let g = last / fifth.max(1.0) - 1.0;
        most = most.max(g);
        if g >= 0.05 {
            grew.push(*life);
        }
    }
    v.push((
        CRITERIA[5],
        judged_lives > 0 && grew.is_empty(),
        format!(
            "{judged_lives} lives of 6 minutes or more, the most {:+.1}% from its fifth minute to its last{}",
            most * 100.0,
            match grew.is_empty() {
                true => String::new(),
                false => format!("; lives {}", list(&grew)),
            }
        ),
    ));
    // The replica is never killed: minute 60 against every minute after.
    match at(60).and_then(|m| get(m, "r_rss_anon")) {
        Some(b) if n > 60 => {
            let over = broke(&|m| get(m, "r_rss_anon").is_some_and(|x| x >= 1.05 * b));
            let over: Vec<u64> = over.into_iter().filter(|k| *k > 60).collect();
            let last = minutes
                .last()
                .and_then(|(_, m)| get(m, "r_rss_anon"))
                .unwrap_or(0.0);
            v.push((
                CRITERIA[6],
                over.is_empty(),
                format!(
                    "{:.0} MB at minute 60, {:.0} MB at the last{}",
                    b / 1048576.0,
                    last / 1048576.0,
                    minutes_said(&over)
                ),
            ));
        }
        _ => v.push((
            CRITERIA[6],
            false,
            format!("not judged: {n} minutes, no hour after the first"),
        )),
    }
    // Threads and descriptors less the connections open as they were
    // looked at: each connection is a thread and a socket, and the
    // harness's own -- its workers', subscriptions', the scrape's -- move
    // with its doings.
    for (c, key, conn, rkey, rconn) in [
        (
            CRITERIA[7],
            "threads",
            "connections",
            "r_threads",
            "r_connections",
        ),
        (CRITERIA[8], "fds", "connections", "r_fds", "r_connections"),
    ] {
        let mut detail = String::new();
        let mut ok = true;
        for (who, k, cn) in [("the primary", key, conn), ("the replica", rkey, rconn)] {
            let net: Vec<(u64, f64)> = minutes
                .iter()
                .filter_map(|(m, x)| Some((*m, get(x, k)? - get(x, cn)?)))
                .collect();
            let mut sorted: Vec<f64> = net.iter().map(|x| x.1).collect();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let Some(&mid) = sorted.get(sorted.len() / 2) else {
                ok = false;
                write!(detail, "{who}: none; ").unwrap();
                continue;
            };
            let off: Vec<u64> = net
                .iter()
                .filter(|x| (x.1 - mid).abs() > 2.0)
                .map(|x| x.0)
                .collect();
            ok &= off.is_empty();
            write!(
                detail,
                "{who}: median {mid:.0}, {:.0} to {:.0}{}; ",
                sorted[0],
                sorted[sorted.len() - 1],
                minutes_said(&off)
            )
            .unwrap();
        }
        v.push((c, ok, detail.trim_end_matches("; ").to_string()));
    }
    for (c, key) in [(CRITERIA[9], "errors"), (CRITERIA[10], "mismatches")] {
        let total = sum(key);
        let at_end = end
            .and_then(|m| get(m, key))
            .map_or(0.0, |x| (x - total).max(0.0));
        let b = broke(&|m| get(m, key).is_some_and(|x| x > 0.0));
        v.push((
            c,
            b.is_empty() && at_end == 0.0,
            format!(
                "{:.0}{}{}",
                total + at_end,
                minutes_said(&b),
                match at_end > 0.0 {
                    true => format!("; {at_end:.0} found at the end"),
                    false => String::new(),
                }
            ),
        ));
    }
    let subs = settings.map_or(0.0, |s| s.subscriptions as f64);
    let faults = [
        "stream_missing",
        "stream_twice",
        "stream_order",
        "stream_late",
        "stream_silent",
    ];
    let bad = broke(&|m| {
        faults.iter().any(|k| get(m, k).is_none_or(|x| x > 0.0))
            || get(m, "streams").is_none_or(|x| x < subs)
    });
    v.push((
        CRITERIA[11],
        settings.is_some() && n > 0 && bad.is_empty(),
        format!(
            "{:.0} events; missing {:.0}, twice {:.0}, out of order {:.0}, late {:.0}, silent {:.0}{}",
            sum("events"),
            sum("stream_missing"),
            sum("stream_twice"),
            sum("stream_order"),
            sum("stream_late"),
            sum("stream_silent"),
            minutes_said(&bad)
        ),
    ));
    let lag_max = settings.map_or(0.0, |s| s.lag_max as f64);
    let lagging = broke(&|m| {
        get(m, "r_connected") != Some(1.0) || get(m, "lag_s").is_none_or(|x| x < 0.0 || x > lag_max)
    });
    let follow = start.and_then(|m| Some((get(m, "follow_s")?, get(m, "follow_max")?)));
    let catchup = end.and_then(|m| get(m, "catchup_s"));
    let catchup_max = settings.map_or(0.0, |s| s.catchup_max as f64);
    let differs = end.and_then(|m| get(m, "replica_differs"));
    v.push((
        CRITERIA[12],
        lagging.is_empty()
            && follow.is_some_and(|(s, max)| s >= 0.0 && s <= max)
            && catchup.is_some_and(|s| s >= 0.0 && s <= catchup_max)
            && differs == Some(0.0),
        format!(
            "connected and within {lag_max:.0} s of the primary{}; followed in {}; caught up at the end in {}; {}",
            minutes_said(&lagging),
            follow.map_or("?".into(), |(s, _)| format!("{s:.0} s")),
            catchup.map_or("? (no end line)".into(), |s| format!("{s:.0} s of {catchup_max:.0}")),
            match differs {
                Some(0.0) => "the same rows",
                Some(_) => "DIFFERENT rows",
                None => "rows not compared",
            }
        ),
    ));
    // A restart's own time is bounded, and so is the wait of an operation
    // due in it: the second announced before the kill, the restart, and a
    // request's time after it -- one held longer was held by something
    // else than the restart.
    let restart_max = settings.map_or(0, |s| s.restart_max) as f64;
    let slow = broke(&|m| get(m, "restart_ms").is_some_and(|x| x > restart_max * 1e3));
    let p99r = end.and_then(|m| get(m, "p99_restart"));
    v.push((
        CRITERIA[13],
        settings.is_some() && slow.is_empty() && p99r.is_some_and(|x| x <= (restart_max + 2.0) * 1e6),
        format!(
            "the longest {:.1} s of {restart_max:.0}{}; p99 of the operations due in one {}; {:.0} met a restart",
            end.and_then(|m| get(m, "restart_ms_max")).unwrap_or(0.0) / 1e3,
            minutes_said(&slow),
            p99r.map_or("? (no end line)".into(), |x| format!("{:.1} s of {:.0}", x / 1e6, restart_max + 2.0)),
            sum("down")
        ),
    ));
    let (kills, in_compact, compacts, backups) = settings.map_or((0, 0, 0, 0), |s| owed(n, &s));
    let got = |k: &str| sum(k) as u64;
    let mut short = Vec::new();
    for (what, have, want, exact) in [
        ("kills", got("kill"), kills, true),
        ("verifies", got("verify"), kills, true),
        ("compacts", got("compact"), compacts, false),
        (
            "compacts a kill cut",
            got("compact_killed") + got("compact"),
            in_compact + compacts,
            false,
        ),
        ("backups", got("backup"), backups, false),
        ("restores", got("restore"), backups, false),
    ] {
        if (exact && have != want) || have < want {
            short.push(format!("{what} {have} of {want}"));
        }
    }
    v.push((
        CRITERIA[14],
        settings.is_some() && short.is_empty(),
        match short.is_empty() {
            true => format!(
                "{kills} kills ({in_compact} in a compact) and verifies, {} compacts, {} backups and restores",
                got("compact"),
                got("backup")
            ),
            false => short.join(", "),
        },
    ));
    let bad = sum("verify_failed") + sum("backup_failed") + sum("compact_failed");
    let end_bad = end.map_or(1.0, |m| {
        get(m, "verify_failed").unwrap_or(1.0) + get(m, "replica_verify_failed").unwrap_or(1.0)
    });
    v.push((
        CRITERIA[15],
        bad == 0.0 && end_bad == 0.0,
        format!(
            "{:.0} at restarts, {:.0} restores, {bad:.0} failed (verify, backup or compact); at the end {}",
            sum("verify"),
            sum("restore"),
            match end {
                None => "none (not finished)".to_string(),
                Some(m) => format!(
                    "the primary {}, the replica {}",
                    if get(m, "verify_failed") == Some(0.0) { "clean" } else { "NOT CLEAN" },
                    if get(m, "replica_verify_failed") == Some(0.0) { "clean" } else { "NOT CLEAN" },
                ),
            }
        ),
    ));
    let recall_min = settings.map_or(1.0, |s| s.recall_min);
    let nears = sum("near_n");
    let recall = sum("recall_sum") / nears.max(1.0);
    let differ = sum("match_differs") + sum("exact_differs");
    v.push((
        CRITERIA[16],
        sum("match_n") > 0.0 && nears > 0.0 && differ == 0.0 && recall >= recall_min,
        format!(
            "{:.0} match and {nears:.0} near samples, {:.0} and {:.0} differing from the scan; @hnsw recall@10 {recall:.3} against exact (at least {recall_min}); {:.0} skipped for a write beside them",
            sum("match_n"),
            sum("match_differs"),
            sum("exact_differs"),
            sum("sample_skipped")
        ),
    ));
    let swept = end.and_then(|m| Some((get(m, "swept")?, get(m, "keys")?)));
    v.push((
        CRITERIA[17],
        swept.is_some_and(|(s, k)| s >= k && k > 0.0),
        match swept {
            None => "no end line".into(),
            Some((s, k)) => format!(
                "{s:.0} of {k:.0} keys at the end, and {:.0} read back after the restarts",
                sum("swept")
            ),
        },
    ));
    let gone = broke(&|m| {
        get(m, "died").is_some_and(|x| x > 0.0)
            || get(m, "oom").is_some_and(|x| x > 0.0)
            || get(m, "r_died").is_some_and(|x| x > 0.0)
    });
    v.push((
        CRITERIA[18],
        gone.is_empty(),
        format!(
            "{:.0} deaths, {:.0} OOM kills, the replica stopped {:.0} times{}",
            sum("died"),
            sum("oom"),
            sum("r_died"),
            minutes_said(&gone)
        ),
    ));
    judged
}

/// Prints each run's verdicts: 0 when every run passed, 1 when one failed,
/// 2 when the environment ended one (neither the server's pass nor its
/// failure).
fn report(path: &Path, only: Option<&str>) -> i32 {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let runs = judge(&text, only);
    if runs.is_empty() {
        println!("no soak run in {path:?}");
        return 1;
    }
    let mut code = 0;
    for j in &runs {
        println!(
            "run {} ({}): {} minutes, {}{}",
            j.run,
            j.label,
            j.minutes,
            if j.finished {
                "finished"
            } else {
                "not finished"
            },
            match j.attempts {
                1 => String::new(),
                a => format!(" -- the latest of {a} attempts under this id, the others not judged"),
            }
        );
        if let Some(why) = &j.env {
            println!("  ENVIRONMENT: the disk ran short {why}: not judged, to be run again");
            code = code.max(2);
            continue;
        }
        for (what, ok, detail) in &j.verdicts {
            println!(
                "  {:<62} {:<4} {detail}",
                what,
                if *ok { "PASS" } else { "FAIL" }
            );
            if !ok {
                code = code.max(1);
            }
        }
        if !j.busy.is_empty() {
            println!(
                "  the host's load was 4 or more in {} minutes: {}",
                j.busy.len(),
                list(&j.busy)
            );
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kills() -> Kills {
        Kills::default()
    }

    fn row(key: u64, s: u64, e: u64, got: [u64; FIELDS]) -> Ev {
        Ev::Row {
            key,
            s,
            e,
            found: true,
            got,
        }
    }

    fn absent(key: u64, s: u64, e: u64) -> Ev {
        Ev::Row {
            key,
            s,
            e,
            found: false,
            got: [0; FIELDS],
        }
    }

    fn write(key: u64, field: u8, d: u64, s: u64, e: u64, seq: Option<u64>) -> Ev {
        Ev::Write {
            key,
            field,
            d,
            s,
            e,
            unanswered: false,
            affected: 1,
            seq,
        }
    }

    fn with(key: u64, f: usize, d: u64) -> [u64; FIELDS] {
        let mut v = base_digests(key);
        v[f] = d;
        v
    }

    #[test]
    fn the_checker_holds_reads_across_minutes_and_waits_for_writes_in_flight() {
        let mut c = Checker::new(100, kills(), 60, 5, &[]);
        c.take(vec![write(7, 3, 42, 10, 20, None)]);
        c.take(vec![row(7, 30, 40, with(7, 3, 42))]);
        c.hold(50, 50, 50, &[]);
        assert_eq!(c.mismatches, 0);
        c.take(vec![row(7, 60, 70, base_digests(7))]);
        c.hold(100, 100, 100, &[]);
        assert_eq!(c.mismatches, 1);
        // A read that saw a write still in flight when the checker looked.
        let mut c = Checker::new(100, kills(), 60, 5, &[]);
        c.take(vec![row(7, 30, 40, with(7, 3, 42))]);
        c.hold(35, 35, 35, &[]);
        assert_eq!((c.reads, c.mismatches), (0, 0));
        c.take(vec![write(7, 3, 42, 33, 50, None)]);
        c.hold(60, 60, 60, &[]);
        assert_eq!((c.reads, c.mismatches), (1, 0));
    }

    #[test]
    fn an_inserted_record_is_there_until_its_delete() {
        let mut c = Checker::new(100, kills(), 60, 5, &[]);
        c.take(vec![
            absent(101, 1, 2),
            write(101, INSERT, 0, 10, 20, None),
            row(101, 30, 40, base_digests(101)),
        ]);
        c.hold(50, 50, 50, &[]);
        assert_eq!(c.mismatches, 0);
        c.take(vec![absent(101, 60, 70)]);
        c.hold(80, 80, 80, &[]);
        assert_eq!(c.mismatches, 1);
        c.take(vec![
            write(101, DELETE, 0, 90, 100, None),
            absent(101, 110, 120),
        ]);
        c.hold(130, 130, 130, &[]);
        assert_eq!(c.mismatches, 1);
        c.take(vec![row(101, 140, 150, base_digests(101))]);
        c.hold(160, 160, 160, &[]);
        assert_eq!(c.mismatches, 2);
        // Two rows for one id is wrong on its face.
        c.take(vec![Ev::Wrong("a read of 7 answered 2 rows".into())]);
        assert_eq!(c.mismatches, 3);
    }

    #[test]
    fn a_session_is_there_until_its_time_and_gone_after_by_id_filter_and_count() {
        // ttl 1 000, slack 100: there to 900 after its insert ended, gone
        // from 1 100 after.
        let mut c = Checker::new(100, kills(), 1_000, 100, &[]);
        c.take(vec![Ev::SessionPut {
            id: 1,
            s: 10,
            e: 20,
            unanswered: false,
            seq: None,
        }]);
        let session = |s, e, found| Ev::Session { id: 1, s, e, found };
        c.take(vec![
            session(30, 40, true),
            session(800, 850, true),
            session(1_200, 1_210, false),
            // Inside the slack either is allowed.
            session(1_000, 1_010, true),
            session(1_000, 1_010, false),
            Ev::Sessions {
                from: 1,
                to: 3,
                s: 30,
                e: 40,
                n: 1,
            },
            Ev::Sessions {
                from: 1,
                to: 3,
                s: 1_200,
                e: 1_210,
                n: 0,
            },
        ]);
        c.hold(5_000, 0, 5_000, &[]);
        assert_eq!(c.mismatches, 0);
        c.take(vec![
            session(30, 40, false),
            session(1_200, 1_210, true),
            Ev::Sessions {
                from: 1,
                to: 3,
                s: 1_200,
                e: 1_210,
                n: 1,
            },
        ]);
        c.hold(5_000, 0, 5_000, &[]);
        assert_eq!(c.mismatches, 3);
    }

    #[test]
    fn an_increment_landing_twice_is_a_mismatch() {
        let mut c = Checker::new(100, kills(), 60, 5, &[]);
        let incr = |s, e, unanswered| Ev::Incr {
            key: 1,
            s,
            e,
            unanswered,
            affected: 1,
            seq: None,
        };
        c.take(vec![incr(10, 20, false), incr(30, 40, true)]);
        c.take(vec![
            Ev::Count {
                key: 1,
                s: 50,
                e: 60,
                n: 1,
            },
            Ev::Count {
                key: 1,
                s: 50,
                e: 60,
                n: 2,
            },
        ]);
        c.hold(100, 100, 100, &[]);
        assert_eq!(c.mismatches, 0);
        c.take(vec![
            Ev::Count {
                key: 1,
                s: 150,
                e: 160,
                n: 3,
            },
            Ev::Count {
                key: 1,
                s: 150,
                e: 160,
                n: 0,
            },
        ]);
        c.hold(200, 200, 200, &[]);
        assert_eq!(c.mismatches, 2);
    }

    fn change(conn: u64, seq: u64, e: u64, rows: Vec<(u64, [u64; FIELDS])>) -> SEv {
        SEv {
            conn,
            seq,
            e,
            opened: 1,
            rows,
            ..SEv::default()
        }
    }

    #[test]
    fn a_stream_is_held_to_the_writes_in_its_range() {
        let range = [(10, 12)];
        let seed = |conn, seq, e| SEv {
            conn,
            seed: true,
            seq,
            opened: e - 1,
            e,
            rows: vec![(10, base_digests(10)), (11, base_digests(11))],
            ..SEv::default()
        };
        // A seed, a write reported once: held.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(10, 0, 42, 20, 30, Some(5))]);
        c.spending.push((0, seed(1, 4, 10)));
        c.spending
            .push((0, change(1, 5, 40, vec![(10, with(10, 0, 42))])));
        c.hold(100, 0, 100, &[(1, 5)]);
        assert_eq!((c.faults, c.mismatches), (StreamFaults::default(), 0));
        // Missing: the change after it does not hold it.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(10, 0, 42, 20, 30, Some(5))]);
        c.spending.push((0, seed(1, 4, 10)));
        c.spending
            .push((0, change(1, 6, 40, vec![(11, base_digests(11))])));
        c.hold(100, 0, 100, &[(1, 6)]);
        assert_eq!((c.faults.missing, c.faults.twice), (1, 1));
        // Twice: a row reported again with no write.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(10, 0, 42, 20, 30, Some(5))]);
        c.spending.push((0, seed(1, 4, 10)));
        c.spending
            .push((0, change(1, 5, 40, vec![(10, with(10, 0, 42))])));
        c.spending
            .push((0, change(1, 6, 50, vec![(10, with(10, 0, 42))])));
        c.hold(100, 0, 100, &[(1, 6)]);
        assert_eq!((c.faults.missing, c.faults.twice), (0, 1));
        // Out of order, and a reconnect replaying the seed alone.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(10, 0, 42, 20, 30, Some(5))]);
        let mut s = seed(1, 5, 35);
        s.rows[0].1 = with(10, 0, 42);
        c.spending.push((0, s));
        c.spending.push((0, change(1, 5, 40, vec![])));
        let mut s = seed(2, 5, 60);
        s.rows[0].1 = with(10, 0, 42);
        c.spending.push((0, s));
        c.hold(100, 0, 100, &[(1, 5)]);
        assert_eq!(c.faults.order, 1);
        // Silent: open, a write in its range answered long ago, nothing.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(10, 0, 42, 20, 30, Some(5))]);
        c.spending.push((0, seed(1, 4, 10)));
        c.hold(100, 0, 30 + SILENT_AFTER + 1, &[(1, 4)]);
        assert_eq!(c.faults.silent, 1);
        // A seed that left a record of its range out.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        let mut s = seed(1, 4, 10);
        s.rows.pop();
        c.spending.push((0, s));
        c.hold(100, 0, 100, &[(1, 4)]);
        assert_eq!(c.mismatches, 1);
    }

    #[test]
    fn the_schedule_queues_a_backup_beside_a_compact_and_owes_its_counts() {
        let s = Settings {
            minutes: 480,
            kill_every: 30,
            kill_in_compact_every: 90,
            compact_every: 20,
            backup_every: 120,
            backup_at: 50,
            restart_max: 120,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            subscriptions: 64,
            threads: 32,
        };
        let at = |m| schedule(m, 480, 30, 90, 20, 120, 50);
        assert_eq!(at(50), vec![Doing::Backup]);
        assert_eq!(
            at(60),
            vec![Doing::Compact, Doing::Kill { in_compact: false }]
        );
        assert_eq!(at(90), vec![Doing::Kill { in_compact: true }]);
        assert_eq!(at(480), vec![]);
        // 480 lines: kills at 30..450, five of them in a compact; compacts
        // at 20..460; backups at 50, 170, 290 and 410 -- at least one each
        // 120 minutes.
        assert_eq!(owed(480, &s), (15, 5, 23, 4));
        assert_eq!(owed(120, &s), (3, 1, 5, 1));
        // A backup every minute it is due, even one that is a compact's
        // too: the old schedule took the compact alone.
        let s = schedule(120, 480, 30, 0, 20, 120, 0);
        assert_eq!(
            s,
            vec![
                Doing::Compact,
                Doing::Backup,
                Doing::Kill { in_compact: false }
            ]
        );
    }

    /// A finished run of `minutes` that passes every criterion, a line a
    /// metric as `append_values` writes it; `tweak` changes a minute's
    /// values (minute 0 the start line, `u64::MAX` the end line).
    fn synthetic(minutes: u64, tweak: &dyn Fn(u64, &mut HashMap<&'static str, f64>)) -> String {
        let s = Settings {
            minutes,
            kill_every: 30,
            kill_in_compact_every: 90,
            compact_every: 20,
            backup_every: 120,
            backup_at: 50,
            restart_max: 120,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            subscriptions: 64,
            threads: 32,
        };
        let mut out = String::new();
        let mut line = |cell: &str, m: &HashMap<&'static str, f64>| {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort();
            for k in keys {
                writeln!(out, "t1\tc\tmac\t2026-11-15\tsoak\t{cell}\t{k}\t{}", m[k]).unwrap();
            }
        };
        let mut start: HashMap<&'static str, f64> = HashMap::new();
        for (k, x) in s.values() {
            start.insert(k, x.parse().unwrap());
        }
        start.insert("started", 1.0);
        start.insert("follow_s", 30.0);
        start.insert("follow_max", 1800.0);
        tweak(0, &mut start);
        line("250:50:start", &start);
        let mut life = 0.0;
        for m in 1..=minutes {
            let mut x: HashMap<&'static str, f64> = HashMap::new();
            // A minute's doings show on the next line.
            let done = schedule(m - 1, minutes, 30, 90, 20, 120, 50);
            for d in &done {
                match d {
                    Doing::Compact => *x.entry("compact").or_default() += 1.0,
                    Doing::Backup => {
                        x.insert("backup", 1.0);
                        x.insert("restore", 1.0);
                    }
                    Doing::Kill { in_compact } => {
                        x.insert("kill", 1.0);
                        x.insert("verify", 1.0);
                        x.insert("restart_ms", 20_000.0);
                        if *in_compact {
                            x.insert("compact_killed", 1.0);
                        }
                        life += 1.0;
                    }
                }
            }
            for (k, val) in [
                ("started", 1.0),
                ("life", life),
                ("ops", 60_000.0),
                ("idle_workers", 0.0),
                ("p99_hour", 2_000.0),
                ("file", 2.0e9),
                ("live", 1.1e9),
                ("rss_anon", 100e6),
                ("r_rss_anon", 90e6),
                ("threads", 140.0),
                ("connections", 100.0),
                ("fds", 120.0),
                ("r_threads", 50.0),
                ("r_connections", 33.0),
                ("r_fds", 60.0),
                ("errors", 0.0),
                ("mismatches", 0.0),
                ("streams", 64.0),
                ("events", 300.0),
                ("stream_missing", 0.0),
                ("stream_twice", 0.0),
                ("stream_order", 0.0),
                ("stream_late", 0.0),
                ("stream_silent", 0.0),
                ("r_connected", 1.0),
                ("lag_s", 0.3),
                ("match_n", 3.0),
                ("match_differs", 0.0),
                ("near_n", 3.0),
                ("exact_differs", 0.0),
                ("recall_sum", 2.97),
                ("load1", 1.0),
            ] {
                x.entry(k).or_insert(val);
            }
            tweak(m, &mut x);
            line(&format!("250:50:m{m:04}"), &x);
        }
        let mut end: HashMap<&'static str, f64> = HashMap::new();
        for (k, val) in [
            ("started", 1.0),
            ("minutes", minutes as f64),
            ("errors", 0.0),
            ("mismatches", 0.0),
            ("swept", 1_100_000.0),
            ("keys", 1_100_000.0),
            ("catchup_s", 4.0),
            ("p99_restart", 21e6),
            ("restart_ms_max", 20_000.0),
            ("verify_failed", 0.0),
            ("replica_verify_failed", 0.0),
            ("replica_differs", 0.0),
        ] {
            end.insert(k, val);
        }
        tweak(u64::MAX, &mut end);
        line("250:50:end", &end);
        out
    }

    fn failing(text: &str) -> Vec<&'static str> {
        let j = judge(text, None);
        assert_eq!(j.len(), 1);
        assert!(j[0].env.is_none());
        j[0].verdicts.iter().filter(|v| !v.1).map(|v| v.0).collect()
    }

    #[test]
    fn the_report_passes_a_good_run_and_fails_each_criterion_alone() {
        let none: &dyn Fn(u64, &mut HashMap<&'static str, f64>) = &|_, _| {};
        assert_eq!(failing(&synthetic(480, none)), Vec::<&str>::new());
        // Two hours, all else held: only the eight hours fail -- what the
        // first run's report must show.
        assert_eq!(failing(&synthetic(120, none)), vec![CRITERIA[0]]);
        type Tweak = Box<dyn Fn(u64, &mut HashMap<&'static str, f64>)>;
        let cases: Vec<(&str, Tweak, Vec<&str>)> = vec![
            (
                "a minute with no operations",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("ops", 0.0);
                    }
                }),
                vec![CRITERIA[2]],
            ),
            (
                "a writer that died",
                Box::new(|m, x| {
                    if (300..480).contains(&m) {
                        x.insert("idle_workers", 1.0);
                    }
                }),
                vec![CRITERIA[2]],
            ),
            (
                "the tail drifting",
                Box::new(|m, x| {
                    if m > 420 && m < u64::MAX {
                        x.insert("p99_hour", 2_600.0);
                    }
                }),
                vec![CRITERIA[3]],
            ),
            (
                "the file past 2.2x live",
                Box::new(|m, x| {
                    if m == 100 {
                        x.insert("file", 2.5e9);
                    }
                }),
                vec![CRITERIA[4]],
            ),
            (
                "the primary leaking within a life",
                Box::new(|m, x| {
                    if (91..=120).contains(&m) {
                        x.insert("rss_anon", 100e6 + (m - 90) as f64 * 1e6);
                    }
                }),
                vec![CRITERIA[5]],
            ),
            (
                "the replica leaking",
                Box::new(|m, x| {
                    if m > 60 && m < u64::MAX {
                        x.insert("r_rss_anon", 90e6 + (m - 60) as f64 * 0.1e6);
                    }
                }),
                vec![CRITERIA[6]],
            ),
            (
                "a thread more each hour",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("threads", 140.0 + (m / 60) as f64);
                    }
                }),
                vec![CRITERIA[7]],
            ),
            (
                "descriptors left open",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("r_fds", 60.0 + (m / 40) as f64);
                    }
                }),
                vec![CRITERIA[8]],
            ),
            (
                "an error",
                Box::new(|m, x| {
                    if m == 7 {
                        x.insert("errors", 1.0);
                    }
                }),
                vec![CRITERIA[9]],
            ),
            (
                "a mismatch found at the end",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("mismatches", 1.0);
                    }
                }),
                vec![CRITERIA[10]],
            ),
            (
                "a silent stream",
                Box::new(|m, x| {
                    if m == 333 {
                        x.insert("stream_silent", 1.0);
                    }
                }),
                vec![CRITERIA[11]],
            ),
            (
                "a stream not open",
                Box::new(|m, x| {
                    if m == 334 {
                        x.insert("streams", 63.0);
                    }
                }),
                vec![CRITERIA[11]],
            ),
            (
                "the replica lagging",
                Box::new(|m, x| {
                    if m == 250 {
                        x.insert("lag_s", 12.0);
                    }
                }),
                vec![CRITERIA[12]],
            ),
            (
                "the replica not caught up",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("catchup_s", -1.0);
                    }
                }),
                vec![CRITERIA[12]],
            ),
            (
                "the replica's rows differing",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("replica_differs", 1.0);
                    }
                }),
                vec![CRITERIA[12]],
            ),
            (
                "a restart too long",
                Box::new(|m, x| {
                    if m == 91 {
                        x.insert("restart_ms", 150_000.0);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                "zero backups",
                Box::new(|m, x| {
                    if m < u64::MAX {
                        x.remove("backup");
                    }
                }),
                vec![CRITERIA[14]],
            ),
            (
                "a kill missing",
                Box::new(|m, x| {
                    if m == 91 {
                        x.remove("kill");
                    }
                }),
                vec![CRITERIA[14]],
            ),
            (
                "a verify that failed",
                Box::new(|m, x| {
                    if m == 31 {
                        x.insert("verify_failed", 1.0);
                    }
                }),
                vec![CRITERIA[15]],
            ),
            (
                "near's recall short",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("recall_sum", 2.4);
                    }
                }),
                vec![CRITERIA[16]],
            ),
            (
                "match differing from the scan",
                Box::new(|m, x| {
                    if m == 9 {
                        x.insert("match_differs", 1.0);
                    }
                }),
                vec![CRITERIA[16]],
            ),
            (
                "keys not read back",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("swept", 0.0);
                    }
                }),
                vec![CRITERIA[17]],
            ),
            (
                "an OOM kill",
                Box::new(|m, x| {
                    if m == 77 {
                        x.insert("oom", 1.0);
                    }
                }),
                vec![CRITERIA[18]],
            ),
        ];
        for (name, tweak, want) in cases {
            assert_eq!(failing(&synthetic(480, &*tweak)), want, "{name}");
        }
        // A run cut short: no end line, the minutes it had.
        let text = synthetic(480, none);
        let cut: String = text
            .lines()
            .filter(|l| {
                let cell = l.split('\t').nth(5).unwrap();
                !cell.ends_with(":end")
                    && cell
                        .rsplit_once(":m")
                        .is_none_or(|(_, n)| n.parse::<u64>().unwrap() <= 300)
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let f = failing(&cut);
        for c in [
            CRITERIA[0],
            CRITERIA[1],
            CRITERIA[12],
            CRITERIA[13],
            CRITERIA[15],
            CRITERIA[17],
        ] {
            assert!(f.contains(&c), "a run cut short: {c} in {f:?}");
        }
        // The environment's failure: no verdict, the run to be made again.
        let env = synthetic(480, &|m, x| {
            if m == 100 {
                x.insert("env_failed", 1.0);
                x.insert("vm_free", 1e9);
            }
        });
        let j = judge(&env, None);
        assert!(j[0]
            .env
            .as_deref()
            .is_some_and(|w| w.contains("minute 100")));
        assert!(j[0].verdicts.is_empty());
        // The script's mark, after the soak it stopped.
        let marked = format!(
            "{}t1\tc\tmac\t2026-11-15\tsoak\t250:50:env\tenv_failed\t1\n",
            synthetic(480, none)
                .lines()
                .filter(|l| l.split('\t').nth(5).is_some_and(|c| c < "250:50:m0101"))
                .map(|l| format!("{l}\n"))
                .collect::<String>()
        );
        let j = judge(&marked, None);
        assert!(j[0]
            .env
            .as_deref()
            .is_some_and(|w| w.contains("minute 100")));
        // A run id used again: the latest attempt judged alone.
        let again = format!(
            "{}{}",
            synthetic(30, none),
            synthetic(480, &|_, x| {
                if x.contains_key("started") {
                    x.insert("started", 2.0);
                }
            })
        );
        let j = judge(&again, None);
        assert_eq!((j[0].attempts, j[0].minutes), (2, 480));
        assert!(j[0].verdicts.iter().all(|v| v.1));
    }

    #[test]
    fn a_stream_event_and_a_doc_page_are_read_as_written() {
        let ev = stream_data(
            r#"{"seq":43,"puts":[{"id":7,"field0":"abc"}],"dels":[8,9],"schema":false}"#,
            false,
        )
        .unwrap();
        assert_eq!(
            (ev.seq, ev.rows.len(), ev.rows[0].0, ev.dels.clone()),
            (43, 1, 7, vec![8, 9])
        );
        let seed = stream_data(r#"{"seq":42,"part":1,"rows":[],"more":true}"#, true).unwrap();
        assert!(seed.seed && seed.part && seed.more && seed.rows.is_empty());
        let docs =
            doc_rows(br#"[{"id":1,"body":"w1 w2","v":[0.5,-1.0]},{"id":2,"body":"w3","v":[1,0]}]"#);
        assert_eq!(docs.len(), 2);
        assert_eq!(
            (docs[1].0, docs[1].1.as_str(), docs[1].2.clone()),
            (2, "w3", vec![1.0, 0.0])
        );
        assert!(cosine(&[1.0, 0.0], &[1.0, 0.0]).abs() < 1e-12);
    }

    /// The soak's statements, run in process against the engine the server
    /// runs: each parses, and answers as the soak reads it.
    #[test]
    fn the_soaks_statements_answer_as_it_reads_them() {
        let o = Opts::parse(&["--sync".into(), "250".into(), "--rate".into(), "100".into()]);
        let mut db = fennec_core::prelude::Database::new();
        let run = |db: &mut fennec_core::prelude::Database,
                   q: &str,
                   params: &[fennec_core::prelude::Value]| {
            db.execute_with(&super::super::stmt(q), params)
                .unwrap_or_else(|e| panic!("{q}: {e}"))
        };
        use fennec_core::prelude::{Response, Value};
        for s in schema(&o) {
            run(&mut db, &s, &[]);
        }
        run(&mut db, "insert counters {id: $1, n: 0}", &[Value::Int(1)]);
        for _ in 0..3 {
            match run(
                &mut db,
                "set counters {n: n + 1} where id = $1",
                &[Value::Int(1)],
            ) {
                Response::Affected(1) => {}
                other => panic!("an increment answered {other:?}"),
            }
        }
        let r = run(&mut db, "get counters where id = $1", &[Value::Int(1)]);
        let rows = r.rows().unwrap();
        let n = rows.columns.iter().position(|c| c == "n").unwrap();
        assert_eq!(rows.rows[0].values[n], Value::Int(3));
        for id in 1..=3 {
            run(
                &mut db,
                "insert sessions {id: $1, user: $2, seen: now()}",
                &[Value::Int(id), Value::Int(5)],
            );
        }
        let r = run(
            &mut db,
            "get sessions where id >= $1 and id < $2 count",
            &[Value::Int(1), Value::Int(3)],
        );
        let rows = r.rows().unwrap();
        assert_eq!(rows.columns, vec!["count".to_string()]);
        assert_eq!(rows.rows[0].values[0], Value::Int(2));
        let r = run(
            &mut db,
            "get sessions where id >= $1 and id < $2",
            &[Value::Int(1), Value::Int(3)],
        );
        assert_eq!(r.rows().unwrap().rows.len(), 2);
        for (id, body) in [(1, "w1 w2"), (2, "w2 w9"), (3, "w5"), (4, "w7 w1")] {
            run(
                &mut db,
                "insert docs {id: $1, body: $2, v: $3}",
                &[
                    Value::Int(id),
                    Value::Text(body.into()),
                    Value::Vector(
                        (0..o.dim)
                            .map(|i| ((i as i64 + id) % 5) as f32 + 0.5)
                            .collect(),
                    ),
                ],
            );
        }
        let ids = |r: &Response| -> Vec<i64> {
            r.rows().unwrap().rows.iter().map(|x| x.id as i64).collect()
        };
        // `match` beside `order` is a filter: every row holding a word.
        let r = run(
            &mut db,
            "get docs match body $1 order id",
            &[Value::Text("w1 w9".into())],
        );
        assert_eq!(ids(&r), vec![1, 2, 4]);
        let q = Value::Vector((0..o.dim).map(|i| (i % 3) as f32).collect());
        let ann = run(
            &mut db,
            "get docs near v $1 limit 10",
            std::slice::from_ref(&q),
        );
        let exact = run(&mut db, "get docs near v $1 exact limit 10", &[q]);
        assert_eq!(ids(&ann).len(), 4);
        assert_eq!(ids(&exact).len(), 4);
        run(
            &mut db,
            "insert usertable {id: $1, field0: $2}",
            &[Value::Int(5), Value::Text("a".into())],
        );
        let r = run(
            &mut db,
            "get usertable where id in $1",
            &[Value::List(vec![Value::Int(5), Value::Int(6)])],
        );
        assert_eq!(ids(&r), vec![5]);
    }
}
