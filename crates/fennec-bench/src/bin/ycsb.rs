//! YCSB's core workloads, fennecdb against SQLite, PostgreSQL, MongoDB and
//! Redis: `make ycsb`.
//!
//! Written here rather than run through the Java YCSB, keeping its
//! definitions: a record is a key and ten fields of 100 random characters
//! (`field0`..`field9`), a read reads the whole record, an update writes one
//! field, and the keys are drawn as YCSB draws them -- its scrambled
//! zipfian (constant 0.99, the item hashed with FNV-1a so the popular keys
//! are spread over the key space), its skewed-latest for D, and a scan's
//! length uniform from 1 to 100 for E:
//!
//!   A  50% read, 50% update          zipfian
//!   B  95% read,  5% update          zipfian
//!   C 100% read                      zipfian
//!   D  95% read,  5% insert          latest
//!   E  95% scan,  5% insert          zipfian, 1-100 records a scan
//!   F  50% read, 50% read-modify-write  zipfian
//!
//! The key is an integer, 1 to the record count, and an insert takes the
//! next: YCSB's `insertorder=ordered` without the `user` prefix, so that
//! every system keys by the integer it keys best by -- fennecdb's `id`,
//! SQLite's `INTEGER PRIMARY KEY`, a `bigint` primary key, MongoDB's `_id`.
//! A read of a key not yet acknowledged is never asked: the inserts in
//! flight are published, and the latest key is the one below the lowest.
//!
//! The systems, each at its documented best with the key indexed:
//!
//!   * `fennec`: fennec-core in process, reads under the shared lock and
//!     writes under the exclusive one, as the server takes them. Durable is
//!     a flush after each write and its fsync outside the lock; buffered a
//!     flush every 250 ms, as `--sync 250` does. A `Compactor` compacts the
//!     file beside the database once half of it is dead, as the native
//!     library and the server do unless told not to (`--no-auto-compact`
//!     for neither);
//!   * `sqlite`: SQLite through rusqlite, a connection a thread, WAL,
//!     `synchronous=FULL` with `fullfsync` or `synchronous=NORMAL`, the file
//!     mapped (`mmap_size`) as fennecdb's is;
//!   * `server`: fennec-server over HTTP, `POST /query` with parameters on a
//!     kept-alive connection a thread, `--sync always` or `--sync 250`;
//!     `server-docker` the same server in a container, to show what
//!     Docker's virtual machine adds to the two below;
//!   * `pg`: PostgreSQL 17 in a container, prepared statements,
//!     `synchronous_commit` on or off;
//!   * `mongo`: MongoDB 8 in a container, the official driver, `j: true`
//!     or `w: 1, j: false`;
//!   * `redis`: Redis 7 in a container, redis-rs, a record mapped as YCSB's
//!     own Redis binding maps it (`RedisClient`, below): `appendfsync
//!     always` or `everysec` with the append-only file on, and a third
//!     mode, `snapshot`, its image as it comes -- RDB snapshots alone,
//!     which no other system has, since a crash loses minutes of writes.
//!
//! A system runs the modes it has among those asked for (`System::modes`),
//! and C, which writes nothing, in one of them: buffered where it runs.
//!
//! The load is timed apart, batches of 1 000 records from one client, each
//! system's documented bulk path. Each cell runs a 5 s warm-up and 30 s
//! measured; before each one the harness waits for a CPU probe to run at
//! the speed it ran at idle (`cool`), since the fanless M1 Air this was
//! measured on slows to a third under minutes of load on every core. A
//! latency is counted in a log-linear histogram of 64 steps an octave
//! (under 1.6% off), the longest kept exact.
//!
//! Each cell's line goes to a TSV file (`--out`); `ycsb report <file>`
//! prints the median of the runs of each cell. After each phase a system
//! that can be measured so adds its bytes on disk (`size-<phase>`) and,
//! one in a container, the memory the container holds of its own
//! (`memory-<phase>`), each in the `ops` column.
//!
//! `--verify <file>` logs every operation and what it was answered, and
//! after each cell holds every read, scan and write to what was written
//! (`check`), the verdict a line of `<file>`. A read under concurrent
//! writes is right if it returns any value a write that could have been
//! the last one left, so judging it needs every write's span, not a
//! sample: each thread logs into its own `Vec` and the check runs once
//! the cell has ended. A client keeps its last answer either way, so the
//! timed path is the one measured without `--verify`; a verifying run
//! times a read-modify-write's digests too and its figures are not the
//! published ones.
//!
//! `--rate <ops/s>` sends a cell's operations open loop, an operation's
//! time from when it was due (`Config::rate`). `--memory <limit>` runs a
//! system under a memory limit -- `server-docker`'s container started with
//! it, or this process in a container started so -- each cell then
//! carrying its cgroup's faults and memory, a failed one a line saying so
//! (`ycsb/run-larger-than-memory.sh`, family `ycsb_larger`). `ycsb heap`
//! measures the heap a row of each index kind holds, `ycsb fill` writes
//! the cells a killed process could not, and `ycsb soak` (`src/soak.rs`) is
//! hours of load against fennec-server in a container (`ycsb/run-soak.sh`).
//! Built `--no-default-features`, it has fennecdb's systems alone and none
//! of the rivals' crates.

#[path = "../http.rs"]
mod http;
#[path = "../register.rs"]
mod register;
#[path = "../soak.rs"]
mod soak;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use fennec_core::prelude::*;

const FIELDS: usize = 10;
const FIELD_LEN: usize = 100;
const SEED: u64 = 0x5943_5342; // "YCSB"
const LOAD_BATCH: usize = 1_000;
#[cfg(feature = "rivals")]
const PG_URL: &str = "host=127.0.0.1 port=55433 user=postgres password=fennec dbname=ycsb";
#[cfg(feature = "rivals")]
const MONGO_URL: &str = "mongodb://127.0.0.1:27018/?maxPoolSize=64";
// Apart from `make geo-bench`'s Redis (56379), so the two never meet.
#[cfg(feature = "rivals")]
const REDIS_URL: &str = "redis://127.0.0.1:56380/";

// ------------------------------------------------------------ randomness

/// xoshiro256** seeded through splitmix64.
#[derive(Clone)]
struct Rng([u64; 4]);

impl Rng {
    fn new(seed: u64) -> Rng {
        let mut x = seed;
        let mut next = || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Rng([next(), next(), next(), next()])
    }
    fn next(&mut self) -> u64 {
        let s = &mut self.0;
        let out = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        out
    }
    /// Uniform in [0, 1).
    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: u64) -> u64 {
        ((self.next() as u128 * n as u128) >> 64) as u64
    }
    /// A field's value: 100 random letters and digits, as YCSB's random
    /// bytes are printable -- and none a JSON or SQL string escapes.
    fn value(&mut self, out: &mut String) {
        const ABC: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        out.clear();
        let mut bits = 0u64;
        for i in 0..FIELD_LEN {
            if i % 10 == 0 {
                bits = self.next();
            }
            out.push(ABC[(bits & 63) as usize % 62] as char);
            bits >>= 6;
        }
    }
}

const ZIPF: f64 = 0.99;

fn zeta(from: u64, to: u64, theta: f64, start: f64) -> f64 {
    let mut sum = start;
    for i in from..to {
        sum += 1.0 / ((i + 1) as f64).powf(theta);
    }
    sum
}

/// YCSB's `ZipfianGenerator` (Gray et al., "Quickly generating
/// billion-record synthetic databases"), its item count allowed to grow as
/// YCSB's `nextLong(itemcount)` lets it: the zeta sum is carried on.
#[derive(Clone)]
struct Zipfian {
    items: u64,
    zetan: f64,
    zeta2: f64,
    alpha: f64,
    eta: f64,
}

impl Zipfian {
    fn new(items: u64) -> Zipfian {
        Zipfian::with_zeta(items, zeta(0, items, ZIPF, 0.0))
    }
    fn with_zeta(items: u64, zetan: f64) -> Zipfian {
        let zeta2 = zeta(0, 2, ZIPF, 0.0);
        let mut z = Zipfian {
            items,
            zetan,
            zeta2,
            alpha: 1.0 / (1.0 - ZIPF),
            eta: 0.0,
        };
        z.eta = z.eta_for();
        z
    }
    fn eta_for(&self) -> f64 {
        (1.0 - (2.0 / self.items as f64).powf(1.0 - ZIPF)) / (1.0 - self.zeta2 / self.zetan)
    }
    /// An item in [0, items).
    fn next(&mut self, rng: &mut Rng, items: u64) -> u64 {
        if items > self.items {
            self.zetan = zeta(self.items, items, ZIPF, self.zetan);
            self.items = items;
            self.eta = self.eta_for();
        }
        let u = rng.f64();
        let uz = u * self.zetan;
        if uz < 1.0 {
            return 0;
        }
        if uz < 1.0 + 0.5f64.powf(ZIPF) {
            return 1;
        }
        let v = (self.items as f64 * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
        v.min(self.items - 1)
    }
}

fn fnv64(mut v: u64) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for _ in 0..8 {
        h ^= v & 0xff;
        v >>= 8;
        h = h.wrapping_mul(1_099_511_628_211);
    }
    (h as i64).unsigned_abs()
}

/// YCSB's `ScrambledZipfianGenerator`: a zipfian over ten billion items,
/// with the zeta YCSB precomputes for them, hashed onto the key space.
#[derive(Clone)]
struct Scrambled {
    z: Zipfian,
}

const SCRAMBLED_ITEMS: u64 = 10_000_000_000;
const SCRAMBLED_ZETAN: f64 = 26.469_028_201_783_02;

impl Scrambled {
    fn new() -> Scrambled {
        Scrambled {
            z: Zipfian::with_zeta(SCRAMBLED_ITEMS + 1, SCRAMBLED_ZETAN),
        }
    }
    fn next(&mut self, rng: &mut Rng, items: u64) -> u64 {
        let v = self.z.next(rng, SCRAMBLED_ITEMS + 1);
        fnv64(v) % items
    }
}

// --------------------------------------------------------------- workload

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Op {
    Read,
    Update,
    Insert,
    Scan,
    Rmw,
}

const OPS: [Op; 5] = [Op::Read, Op::Update, Op::Insert, Op::Scan, Op::Rmw];

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Read => "READ",
            Op::Update => "UPDATE",
            Op::Insert => "INSERT",
            Op::Scan => "SCAN",
            Op::Rmw => "RMW",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Dist {
    Zipfian,
    Latest,
}

struct Workload {
    name: char,
    /// Each op's share, summing to 1.
    mix: &'static [(Op, f64)],
    dist: Dist,
}

const WORKLOADS: [Workload; 6] = [
    Workload {
        name: 'A',
        mix: &[(Op::Read, 0.5), (Op::Update, 0.5)],
        dist: Dist::Zipfian,
    },
    Workload {
        name: 'B',
        mix: &[(Op::Read, 0.95), (Op::Update, 0.05)],
        dist: Dist::Zipfian,
    },
    Workload {
        name: 'C',
        mix: &[(Op::Read, 1.0)],
        dist: Dist::Zipfian,
    },
    Workload {
        name: 'D',
        mix: &[(Op::Read, 0.95), (Op::Insert, 0.05)],
        dist: Dist::Latest,
    },
    Workload {
        name: 'E',
        mix: &[(Op::Scan, 0.95), (Op::Insert, 0.05)],
        dist: Dist::Zipfian,
    },
    Workload {
        name: 'F',
        mix: &[(Op::Read, 0.5), (Op::Rmw, 0.5)],
        dist: Dist::Zipfian,
    },
];

/// The keys handed out to inserts and those acknowledged. `last` is the
/// highest key every key at or below which is in the database: the next
/// key less one, or the key below the lowest insert still in flight.
struct Keys {
    next: AtomicU64,
    in_flight: Vec<AtomicU64>,
}

impl Keys {
    fn new(records: u64, threads: usize) -> Keys {
        Keys {
            next: AtomicU64::new(records + 1),
            in_flight: (0..threads).map(|_| AtomicU64::new(u64::MAX)).collect(),
        }
    }
    fn last(&self) -> u64 {
        let mut low = self.next.load(Ordering::SeqCst);
        for f in &self.in_flight {
            low = low.min(f.load(Ordering::SeqCst));
        }
        low - 1
    }
}

/// One thread's way of choosing keys.
struct Chooser {
    dist: Dist,
    scrambled: Scrambled,
    latest: Zipfian,
    /// The key space a scrambled draw covers: the records, and room for
    /// the inserts a cell expects (YCSB's `insertcount + expectednewkeys`).
    space: u64,
}

impl Chooser {
    fn next(&mut self, rng: &mut Rng, keys: &Keys) -> u64 {
        let last = keys.last();
        match self.dist {
            // YCSB's `SkewedLatestGenerator`: the latest key, less a
            // zipfian draw over the keys there are.
            Dist::Latest => last - self.latest.next(rng, last),
            // A key not acknowledged yet is drawn again, as YCSB does.
            Dist::Zipfian => loop {
                let k = self.scrambled.next(rng, self.space) + 1;
                if k <= last {
                    return k;
                }
            },
        }
    }
}

// -------------------------------------------------------------- histogram

/// Log-linear: values under 128 ns exact, then 64 steps an octave.
struct Hist {
    counts: Vec<u64>,
    n: u64,
    max: u64,
}

const SUB: u32 = 6;

impl Hist {
    fn new() -> Hist {
        Hist {
            counts: vec![0; 64 << SUB],
            n: 0,
            max: 0,
        }
    }
    fn index(v: u64) -> usize {
        if v < (2 << SUB) {
            return v as usize;
        }
        let shift = 63 - v.leading_zeros() - SUB;
        ((shift as u64) << SUB) as usize + (v >> shift) as usize
    }
    /// The middle of the bucket `i` covers.
    fn value(i: usize) -> f64 {
        if i < (2 << SUB) {
            return i as f64;
        }
        let shift = (i >> SUB) as u32 - 1;
        let m = (i - ((shift as usize) << SUB)) as u64;
        ((m << shift) as f64) + ((1u64 << shift) as f64 - 1.0) / 2.0
    }
    fn record(&mut self, ns: u64) {
        self.counts[Hist::index(ns)] += 1;
        self.n += 1;
        self.max = self.max.max(ns);
    }
    fn merge(&mut self, o: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(&o.counts) {
            *a += b;
        }
        self.n += o.n;
        self.max = self.max.max(o.max);
    }
    /// The `p` quantile in microseconds.
    fn pct(&self, p: f64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let want = ((self.n as f64 * p).ceil() as u64).max(1);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                return (Hist::value(i) / 1e3).min(self.max as f64 / 1e3);
            }
        }
        self.max as f64 / 1e3
    }
}

// ---------------------------------------------------------------- clients

/// What a client thread asks of a system. Each checks its answer enough to
/// know the work was done -- a read found a record, a scan its rows -- and
/// keeps what it was answered, which `--verify` reads after the timed span
/// (`got`, `affected`) and holds to what was written (`Check`).
trait Client: Send {
    fn read(&mut self, key: u64);
    fn update(&mut self, key: u64, field: usize, value: &str);
    fn insert(&mut self, key: u64, values: &[String]);
    fn scan(&mut self, key: u64, len: u64);
    /// The rows the last read or scan was answered, in the order they came:
    /// each one's key and its fields' digests (`digest`, 0 for a field the
    /// row did not hold).
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])>;
    /// The records the last write's answer said it wrote, where the client
    /// does not hold it to 1 itself.
    fn affected(&mut self) -> u64 {
        1
    }
    /// The least the system can be asked over its wire -- an empty query,
    /// a health check -- or false in process: what the network alone
    /// costs a request, printed beside the cells.
    fn ping(&mut self) -> bool {
        false
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Durable,
    Buffered,
    /// Redis as its image comes: RDB snapshots alone, no append-only file.
    /// Not durable in any sense the other two are -- a crash loses the
    /// writes since the last snapshot, minutes of them -- so it is a mode
    /// of its own, which no other system has, rather than Redis's buffered.
    Snapshot,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Durable => "durable",
            Mode::Buffered => "buffered",
            Mode::Snapshot => "snapshot",
        }
    }
}

/// A system under test: it loads the records, is set to a mode, and hands
/// out a client a thread.
trait System {
    fn name(&self) -> &'static str;
    fn load(&mut self, records: u64);
    fn set_mode(&mut self, mode: Mode);
    fn client(&self) -> Box<dyn Client>;
    /// The modes it has; a run asking for others passes them over.
    fn modes(&self) -> &'static [Mode] {
        &[Mode::Durable, Mode::Buffered]
    }
    /// The bytes the data takes on disk, where they can be read: an
    /// engine that writes a record again elsewhere grows by every update.
    fn size(&self) -> Option<u64> {
        None
    }
    /// [`size`](System::size) while a cell runs, every 100 ms: only where
    /// reading it takes nothing from the system measured, and `None`
    /// otherwise, its size then read once the cell's clients have stopped.
    /// Read through a container started for each look -- fennec-server's
    /// image holds no `du` -- two a second ran in the VM beside the server
    /// and took C at one client from 3.14-3.17 k a second to 2.33-2.38 k
    /// (100 000 records, in turns), its p99 1.3 -> 1.9 ms.
    fn size_beside(&self) -> Option<u64> {
        self.size()
    }
    /// The memory its container holds of its own (`container_memory`),
    /// for a system in one.
    fn memory(&self) -> Option<u64> {
        None
    }
    /// Its cgroup as last read, under `--memory` alone: the server's
    /// container through a process beside it, a look a second
    /// (`Watch`), or the container this process runs in (`Cgroup::own`).
    fn cgroup(&self) -> Option<Cgroup> {
        None
    }
    /// Whether the kernel killed it for its memory: what a cell that
    /// failed is told apart by.
    fn oom_killed(&self) -> bool {
        false
    }
    /// The bytes of its live records, where it says: what a cell under
    /// `--memory` is held to twice the limit by.
    fn live(&self) -> Option<u64> {
        None
    }
}

/// A cgroup's memory as one look read it (`memory.stat`,
/// `memory.current`, `memory.events`, `memory.max`): the major faults its
/// processes took so far, what it is charged in all, its anonymous memory,
/// its files' pages -- the mapped database's among them --, the OOM kills
/// it had, and the limit the kernel holds it to, none where `memory.max`
/// says `max` or was not read: the limit a line names is the cgroup's own,
/// never the `--memory` asked for.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Cgroup {
    pgmajfault: u64,
    current: u64,
    anon: u64,
    file: u64,
    oom_kill: u64,
    limit: Option<u64>,
}

impl Cgroup {
    /// A look's lines: `memory.stat`'s, `current <n>`, `memory.events`'
    /// `oom_kill <n>` and `limit <memory.max>`, in any order. The limit's
    /// key is none of the files' own: `memory.events` has a `max`, the times
    /// the limit was met.
    fn parse(text: &str) -> Option<Cgroup> {
        let get = |key: &str| -> Option<u64> {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
                .and_then(|v| v.trim().parse().ok())
        };
        Some(Cgroup {
            pgmajfault: get("pgmajfault")?,
            current: get("current")?,
            anon: get("anon")?,
            file: get("file")?,
            oom_kill: get("oom_kill").unwrap_or(0),
            limit: get("limit"),
        })
    }

    /// The container this process runs in, from its own cgroup: cgroup v2
    /// in a namespace of its own, where `/sys/fs/cgroup` is the
    /// container's.
    fn own() -> Option<Cgroup> {
        let dir = Path::new("/sys/fs/cgroup");
        let mut text = std::fs::read_to_string(dir.join("memory.stat")).ok()?;
        let current = std::fs::read_to_string(dir.join("memory.current")).ok()?;
        writeln!(text, "current {}", current.trim()).unwrap();
        if let Ok(max) = std::fs::read_to_string(dir.join("memory.max")) {
            writeln!(text, "limit {}", max.trim()).unwrap();
        }
        if let Ok(events) = std::fs::read_to_string(dir.join("memory.events")) {
            // `oom_kill` alone: its `max` line is no limit.
            for l in events.lines().filter(|l| l.starts_with("oom_kill ")) {
                writeln!(text, "{l}").unwrap();
            }
        }
        Cgroup::parse(&text)
    }
}

/// What a shell beside a container prints of the container's cgroup, a look
/// a second, a blank line after each: the lines `Cgroup::parse` reads.
const CGROUP_LOOK: &str = "cd /proc/1/root/sys/fs/cgroup && while :; do \
    cat memory.stat; echo current $(cat memory.current); \
    echo limit $(cat memory.max); grep oom_kill memory.events; echo; sleep 1; done";

/// One look at a container's cgroup, `CGROUP_LOOK`'s lines once, by a
/// shell in its process namespace (`<container>-look`): what a step reads
/// before and after it rather than a look a second.
fn container_cgroup(container: &str) -> Option<Cgroup> {
    let out = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--pull",
            "never",
            &format!("--pid=container:{container}"),
        ])
        .args(["--name", &format!("{container}-look")])
        // Reading another user's `/proc/<pid>/root` is a ptrace's right.
        .args(["--cap-add", "SYS_PTRACE", "alpine", "sh", "-c"])
        .arg(
            "cd /proc/1/root/sys/fs/cgroup && cat memory.stat && \
             echo current $(cat memory.current) && echo limit $(cat memory.max) && \
             grep oom_kill memory.events",
        )
        .output()
        .ok()?;
    Cgroup::parse(&String::from_utf8_lossy(&out.stdout))
}

/// A container's cgroup looked at each second by a shell in its process
/// namespace (`CGROUP_LOOK`), the last look kept: one process the run
/// long, where a `docker run` a look took a quarter of a one-client
/// server's reads (`size_beside`). It ends with the container's processes.
struct Watch {
    child: std::process::Child,
    name: String,
    last: Arc<std::sync::Mutex<Option<Cgroup>>>,
}

impl Watch {
    fn start(container: &str) -> Option<Watch> {
        let name = format!("{container}-watch");
        let mut child = std::process::Command::new("docker")
            .args([
                "run",
                "--rm",
                "--pull",
                "never",
                &format!("--pid=container:{container}"),
            ])
            .args(["--name", &name])
            // Reading another user's `/proc/<pid>/root` is a ptrace's right.
            .args(["--cap-add", "SYS_PTRACE", "alpine", "sh", "-c", CGROUP_LOOK])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let last = Arc::new(std::sync::Mutex::new(None));
        let keep = Arc::clone(&last);
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let mut look = String::new();
            for line in std::io::BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if !line.is_empty() {
                    look.push_str(&line);
                    look.push('\n');
                    continue;
                }
                if let Some(c) = Cgroup::parse(&look) {
                    *keep.lock().unwrap() = Some(c);
                }
                look.clear();
            }
        });
        Some(Watch { child, name, last })
    }

    fn last(&self) -> Option<Cgroup> {
        *self.last.lock().unwrap()
    }
}

impl Drop for Watch {
    /// The container goes with its own: killing `docker run` leaves it.
    /// The client is killed first; a container the daemon was still making
    /// for it -- left `Created` once, a watch dropped as it started -- is
    /// removed again with the server's (`Server::stop`).
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// What a container holds of its own -- its processes' anonymous memory
/// and the shared memory PostgreSQL's buffers are, which neither can give
/// back -- from its cgroup (`memory.stat`), the page cache of its files
/// apart: as `make scale-bench` reads PostgreSQL's. Read by a container
/// sharing its processes, through the first one's root, since
/// fennec-server's image is empty but for the binary and has no `cat`.
fn container_memory(name: &str) -> Option<u64> {
    let out = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--pull",
            "never",
            &format!("--pid=container:{name}"),
        ])
        // Reading another user's `/proc/<pid>/root` is a ptrace's right.
        .args(["--cap-add", "SYS_PTRACE", "alpine"])
        .args(["cat", "/proc/1/root/sys/fs/cgroup/memory.stat"])
        .output()
        .ok()?;
    memory_stat(&String::from_utf8_lossy(&out.stdout))
}

/// `anon` and `shmem` of a cgroup's `memory.stat`, summed.
fn memory_stat(text: &str) -> Option<u64> {
    let get = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
            .and_then(|v| v.trim().parse().ok())
    };
    Some(get("anon")? + get("shmem")?)
}

/// A record's values: the same for every system, drawn from its key.
fn record_values(key: u64) -> Vec<String> {
    let mut rng = Rng::new(SEED ^ key.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (0..FIELDS)
        .map(|_| {
            let mut s = String::with_capacity(FIELD_LEN);
            rng.value(&mut s);
            s
        })
        .collect()
}

/// A field's value as `--verify` keeps it: FNV-1a over its bytes. The
/// values are 100 random characters, so two that differ share one with a
/// chance of 2^-64 and a wrong value never passes for the right one.
fn digest(b: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100_0000_01B3);
    }
    h
}

/// A record's fields in one: what a scan's row is held to.
fn combine(fields: &[u64; FIELDS]) -> u64 {
    fields.iter().fold(0x9E37_79B9_7F4A_7C15, |h, d| {
        (h ^ d).wrapping_mul(0x100_0000_01B3).rotate_left(29)
    })
}

fn base_digests(key: u64) -> [u64; FIELDS] {
    let mut out = [0; FIELDS];
    for (o, v) in out.iter_mut().zip(record_values(key)) {
        *o = digest(v.as_bytes());
    }
    out
}

// ---- fennecdb in process

struct FennecLocal {
    dir: PathBuf,
    db: Option<Arc<RwLock<Database>>>,
    durable: bool,
    syncer: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    /// What an app's database has unless it opts out: the file compacted
    /// beside the queries once half of it is dead.
    auto_compact: bool,
    compactor: Option<fennec_core::engine::Compactor>,
    /// Run under `--memory`, in a container of its own: its cgroup is
    /// looked at beside each cell.
    contained: bool,
}

fn stmt(src: &str) -> Statement {
    fennec_ql::parse_one(src).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

const MAX_SCAN: u64 = 100;

/// E's scan: the records from a key on, in key order -- a scan with an
/// `id` floor walks the id index from there and stops at the limit.
fn scan_text(len: u64) -> String {
    format!("get usertable where id >= $1 limit {len}")
}

fn insert_text() -> String {
    let fields: Vec<String> = (0..FIELDS)
        .map(|i| format!("field{i}: ${}", i + 2))
        .collect();
    format!("insert usertable {{id: $1, {}}}", fields.join(", "))
}

impl FennecLocal {
    fn new(dir: &Path, auto_compact: bool, contained: bool) -> FennecLocal {
        FennecLocal {
            dir: dir.to_path_buf(),
            db: None,
            durable: false,
            syncer: None,
            auto_compact,
            compactor: None,
            contained,
        }
    }
    fn stop_syncer(&mut self) {
        if let Some((stop, h)) = self.syncer.take() {
            stop.store(true, Ordering::Relaxed);
            h.join().unwrap();
        }
    }
}

impl Drop for FennecLocal {
    fn drop(&mut self) {
        self.stop_syncer();
    }
}

impl System for FennecLocal {
    fn name(&self) -> &'static str {
        "fennec"
    }
    fn size(&self) -> Option<u64> {
        std::fs::metadata(self.dir.join("ycsb.fennec"))
            .ok()
            .map(|m| m.len())
    }
    fn cgroup(&self) -> Option<Cgroup> {
        self.contained.then(Cgroup::own).flatten()
    }
    fn live(&self) -> Option<u64> {
        let db = self.db.as_ref()?.read().unwrap();
        Some(db.stats().iter().map(|c| c.bytes as u64).sum())
    }
    fn load(&mut self, records: u64) {
        // A load starts afresh: the syncer holds the database it replaces.
        self.stop_syncer();
        self.compactor = None;
        self.db = None;
        let path = self.dir.join("ycsb.fennec");
        let _ = std::fs::remove_file(&path);
        let mut db = fennec_core::fs::open(&path).unwrap();
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
        db.execute(&stmt(&format!(
            "create collection usertable ({})",
            fields.join(", ")
        )))
        .unwrap();
        let mut key = 1;
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
            let docs = (key..end)
                .map(|k| {
                    let mut d = vec![("id".to_string(), Expr::Lit(Value::Int(k as i64)))];
                    for (i, v) in record_values(k).into_iter().enumerate() {
                        d.push((format!("field{i}"), Expr::Lit(Value::Text(v))));
                    }
                    d
                })
                .collect();
            db.execute(&Statement::Put {
                collection: "usertable".into(),
                docs,
                insert: true,
                if_absent: false,
                docs_param: None,
                else_set: None,
                require: None,
                pick: None,
            })
            .unwrap();
            key = end;
        }
        db.sync().unwrap();
        let db = Arc::new(RwLock::new(db));
        if self.auto_compact {
            self.compactor =
                Some(fennec_core::engine::Compactor::start(&db, Default::default()).unwrap());
        }
        self.db = Some(db);
    }
    fn set_mode(&mut self, mode: Mode) {
        self.stop_syncer();
        self.durable = mode == Mode::Durable;
        let db = Arc::clone(self.db.as_ref().unwrap());
        {
            let d = db.write().unwrap().flush().unwrap();
            if let Some(d) = d {
                d().unwrap();
            }
        }
        if mode == Mode::Buffered {
            // `--sync 250`: what was written goes to disk every 250 ms,
            // the fsync outside the lock, as the server's syncer runs it.
            let stop = Arc::new(AtomicBool::new(false));
            let s = Arc::clone(&stop);
            let h = std::thread::spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    let d = db.write().unwrap().flush().unwrap();
                    if let Some(d) = d {
                        d().unwrap();
                    }
                }
            });
            self.syncer = Some((stop, h));
        }
    }
    fn client(&self) -> Box<dyn Client> {
        Box::new(FennecLocalClient {
            db: Arc::clone(self.db.as_ref().unwrap()),
            durable: self.durable,
            get: stmt("get usertable where id = $1"),
            // A limit is a number in the text, not a parameter: a
            // statement for each length a scan can have.
            scan: (1..=MAX_SCAN).map(|n| stmt(&scan_text(n))).collect(),
            set: (0..FIELDS)
                .map(|i| stmt(&format!("set usertable {{field{i}: $2}} where id = $1")))
                .collect(),
            insert: stmt(&insert_text()),
            args: Vec::with_capacity(FIELDS + 1),
            last: None,
        })
    }
}

struct FennecLocalClient {
    db: Arc<RwLock<Database>>,
    durable: bool,
    get: Statement,
    scan: Vec<Statement>,
    set: Vec<Statement>,
    insert: Statement,
    args: Vec<Value>,
    /// The last read's or scan's answer, kept rather than dropped.
    last: Option<Response>,
}

impl FennecLocalClient {
    fn write(&mut self, which: Option<usize>) {
        let durability = {
            let mut g = self.db.write().unwrap();
            let stmt = match which {
                Some(f) => &self.set[f],
                None => &self.insert,
            };
            let out = g.execute_with(stmt, &self.args).unwrap();
            assert!(matches!(out, Response::Affected(1)), "{out:?}");
            match self.durable {
                true => g.flush().unwrap(),
                false => None,
            }
        };
        if let Some(d) = durability {
            d().unwrap();
        }
    }
}

impl Client for FennecLocalClient {
    fn read(&mut self, key: u64) {
        let out = self
            .db
            .read()
            .unwrap()
            .query(&self.get, &[Value::Int(key as i64)])
            .unwrap();
        let rows = out.rows().unwrap();
        assert!(rows.rows.len() == 1 && rows.rows[0].values.len() > FIELDS);
        self.last = Some(out);
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        self.args.clear();
        self.args.push(Value::Int(key as i64));
        self.args.push(Value::Text(value.to_string()));
        self.write(Some(field));
    }
    fn insert(&mut self, key: u64, values: &[String]) {
        self.args.clear();
        self.args.push(Value::Int(key as i64));
        self.args
            .extend(values.iter().map(|v| Value::Text(v.clone())));
        self.write(None);
    }
    fn scan(&mut self, key: u64, len: u64) {
        let out = self
            .db
            .read()
            .unwrap()
            .query(&self.scan[len as usize - 1], &[Value::Int(key as i64)])
            .unwrap();
        let rows = out.rows().unwrap();
        assert!(!rows.rows.is_empty() && rows.rows[0].id == key);
        assert!(rows.rows.windows(2).all(|w| w[0].id < w[1].id));
        self.last = Some(out);
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        let Some(rs) = self.last.as_ref().and_then(|o| o.rows()) else {
            return Vec::new();
        };
        let at: Vec<Option<usize>> = (0..FIELDS)
            .map(|i| rs.columns.iter().position(|c| *c == format!("field{i}")))
            .collect();
        rs.rows
            .iter()
            .map(|r| {
                let mut d = [0; FIELDS];
                for (o, a) in d.iter_mut().zip(&at) {
                    if let Some(Value::Text(t)) = a.and_then(|a| r.values.get(a)) {
                        *o = digest(t.as_bytes());
                    }
                }
                (r.id, d)
            })
            .collect()
    }
}

// ---- SQLite

#[cfg(feature = "rivals")]
struct Sqlite {
    path: PathBuf,
    durable: bool,
}

#[cfg(feature = "rivals")]
fn sqlite_conn(path: &Path, durable: bool) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(path).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.pragma_update(None, "synchronous", if durable { "FULL" } else { "NORMAL" })
        .unwrap();
    c.pragma_update(None, "fullfsync", durable).unwrap();
    // The file read through the page cache's mapping, as fennecdb reads
    // its own, and 64 MB of page cache a connection.
    c.pragma_update(None, "mmap_size", 8i64 << 30).unwrap();
    c.pragma_update(None, "cache_size", -65_536).unwrap();
    c.busy_timeout(Duration::from_secs(30)).unwrap();
    c
}

#[cfg(feature = "rivals")]
impl System for Sqlite {
    fn name(&self) -> &'static str {
        "sqlite"
    }
    fn size(&self) -> Option<u64> {
        let len = |ext: &str| {
            std::fs::metadata(format!("{}{ext}", self.path.display()))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        Some(len("") + len("-wal"))
    }
    fn load(&mut self, records: u64) {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", self.path.display()));
        }
        let mut c = sqlite_conn(&self.path, false);
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
        c.execute(
            &format!(
                "create table usertable (ycsb_key integer primary key, {})",
                fields.join(", ")
            ),
            [],
        )
        .unwrap();
        let mut key = 1;
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
            let tx = c.transaction().unwrap();
            {
                let mut ins = tx.prepare_cached(&sqlite_insert()).unwrap();
                for k in key..end {
                    let v = record_values(k);
                    ins.execute(rusqlite::params![
                        k as i64, v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9]
                    ])
                    .unwrap();
                }
            }
            tx.commit().unwrap();
            key = end;
        }
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    fn set_mode(&mut self, mode: Mode) {
        self.durable = mode == Mode::Durable;
    }
    fn client(&self) -> Box<dyn Client> {
        Box::new(SqliteClient {
            c: sqlite_conn(&self.path, self.durable),
            last: Vec::new(),
        })
    }
}

#[cfg(feature = "rivals")]
fn sqlite_insert() -> String {
    let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i}")).collect();
    let marks: Vec<String> = (0..=FIELDS).map(|i| format!("?{}", i + 1)).collect();
    format!(
        "insert into usertable (ycsb_key, {}) values ({})",
        fields.join(", "),
        marks.join(", ")
    )
}

#[cfg(feature = "rivals")]
struct SqliteClient {
    c: rusqlite::Connection,
    last: Vec<(i64, [String; FIELDS])>,
}

#[cfg(feature = "rivals")]
fn sqlite_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, [String; FIELDS])> {
    let mut out: [String; FIELDS] = Default::default();
    for (i, o) in out.iter_mut().enumerate() {
        *o = r.get(i + 1)?;
    }
    Ok((r.get(0)?, out))
}

/// Rows read into strings, as every client reads them, to their digests.
#[cfg(feature = "rivals")]
fn digests_of(rows: &[(i64, [String; FIELDS])]) -> Vec<(u64, [u64; FIELDS])> {
    rows.iter()
        .map(|(k, f)| {
            let mut d = [0; FIELDS];
            for (o, v) in d.iter_mut().zip(f) {
                *o = digest(v.as_bytes());
            }
            (*k as u64, d)
        })
        .collect()
}

#[cfg(feature = "rivals")]
impl Client for SqliteClient {
    fn read(&mut self, key: u64) {
        let mut s = self
            .c
            .prepare_cached("select * from usertable where ycsb_key = ?1")
            .unwrap();
        let row = s.query_row([key as i64], sqlite_row).unwrap();
        self.last.clear();
        self.last.push(row);
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        let mut s = self
            .c
            .prepare_cached(&format!(
                "update usertable set field{field} = ?2 where ycsb_key = ?1"
            ))
            .unwrap();
        assert_eq!(s.execute(rusqlite::params![key as i64, value]).unwrap(), 1);
    }
    fn insert(&mut self, key: u64, v: &[String]) {
        let mut s = self.c.prepare_cached(&sqlite_insert()).unwrap();
        s.execute(rusqlite::params![
            key as i64, v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9]
        ])
        .unwrap();
    }
    fn scan(&mut self, key: u64, len: u64) {
        let mut s = self
            .c
            .prepare_cached(
                "select * from usertable where ycsb_key >= ?1 order by ycsb_key limit ?2",
            )
            .unwrap();
        let rows: Vec<(i64, [String; FIELDS])> = s
            .query_map([key as i64, len as i64], sqlite_row)
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!rows.is_empty());
        self.last = rows;
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        digests_of(&self.last)
    }
}

// ---- fennec-server over HTTP

struct Server {
    /// Native: the file the server is started over; in Docker the volume.
    dir: PathBuf,
    docker: bool,
    port: u16,
    proc: Option<http::Server>,
    image: String,
    /// `--auto-compact off` when the run says so; the server's default
    /// otherwise.
    auto_compact: bool,
    /// In Docker, what the container and its volume are named after
    /// (`<name>-server`, `<name>-data`: `--docker-name`, `fennecycsb`
    /// unless given), and the memory it may hold (`--memory`), page cache
    /// and all, its cgroup looked at each second while it runs.
    name: String,
    memory: Option<String>,
    watch: Option<Watch>,
}

impl Server {
    fn start(&mut self, sync: &str) {
        self.stop();
        if !self.docker {
            let file = self.dir.join("ycsb-server.fennec");
            let mut args = vec!["--sync", sync];
            if !self.auto_compact {
                args.extend(["--auto-compact", "off"]);
            }
            self.proc = Some(http::start_fennec(&file, self.port, "ycsb", &args));
            return;
        }
        let mut run = std::process::Command::new("docker");
        run.args(["run", "-d", "--pull", "never", "--name", &self.container()])
            .args(["-p", &format!("127.0.0.1:{}:8080", self.port)])
            .args(["-v", &format!("{}:/data", self.volume())]);
        if let Some(m) = &self.memory {
            // No swap past it: a page the limit pushes out is read again
            // from the file, which is what is measured, and the server's
            // own memory past it is an OOM kill rather than a slow cell.
            run.args(["--memory", m, "--memory-swap", m]);
        }
        let st = run
            .arg(&self.image)
            .args(["--http", "0.0.0.0:8080", "--insecure", "--no-checkpoint"])
            .args(["--file", "/data/ycsb.fennec", "--sync", sync])
            .args(match self.auto_compact {
                true => &[][..],
                false => &["--auto-compact", "off"][..],
            })
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "docker run {}", self.image);
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.port)) {
                // Docker's proxy takes the connection before the server
                // listens: ask for something.
                let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
                let mut b = [0u8; 16];
                if matches!(std::io::Read::read(&mut s, &mut b), Ok(n) if n > 0) {
                    break;
                }
            }
            assert!(Instant::now() < until, "the container did not start");
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.memory.is_some() {
            self.watch = Watch::start(&self.container());
        }
    }
    fn container(&self) -> String {
        format!("{}-server", self.name)
    }
    fn volume(&self) -> String {
        format!("{}-data", self.name)
    }
    /// Stopped as a supervisor stops it, so that the writes of the last
    /// `--sync 250` interval are on disk when it starts again.
    fn stop(&mut self) {
        if let Some(p) = self.proc.take() {
            p.terminate();
        }
        self.watch = None;
        if self.docker {
            let _ = std::process::Command::new("docker")
                .args(["stop", "-t", "60", &self.container()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &self.container()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            // A watch the daemon made after its drop, which can no longer
            // start now that the server's processes are gone.
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", &format!("{}-watch", self.container())])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        if self.docker {
            remove_volume(&self.volume());
        }
    }
}

/// Removes the server's volume, waiting for the container that held it to
/// go: started with `--rm`, a container was still being removed when
/// `docker rm -f` returned, its volume stayed, and the next load found the
/// collection made (409) -- or a GB or more stayed in the VM after a run.
/// The container is started without it now, which makes the removal
/// synchronous; the wait stays for a daemon that answers late.
fn remove_volume(volume: &str) {
    for _ in 0..40 {
        let gone = std::process::Command::new("docker")
            .args(["volume", "rm", "-f", volume])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if gone {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("the volume {volume} is still in use");
}

impl System for Server {
    fn name(&self) -> &'static str {
        if self.docker {
            "server-docker"
        } else {
            "server"
        }
    }
    fn size(&self) -> Option<u64> {
        match self.docker {
            // The container's file is in the VM's volume: its length read
            // by a container mounting the volume beside the server.
            true => {
                let out = std::process::Command::new("docker")
                    .args([
                        "run",
                        "--rm",
                        "--pull",
                        "never",
                        "-v",
                        &format!("{}:/data", self.volume()),
                    ])
                    .args(["alpine", "du", "-b", "/data/ycsb.fennec"])
                    .output()
                    .ok()?;
                String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            }
            false => std::fs::metadata(self.dir.join("ycsb-server.fennec"))
                .ok()
                .map(|m| m.len()),
        }
    }
    fn size_beside(&self) -> Option<u64> {
        // A `docker run` a look, in the server's VM.
        (!self.docker).then(|| self.size()).flatten()
    }
    fn memory(&self) -> Option<u64> {
        // The mapped file's pages are the page cache's, not the server's.
        self.docker
            .then(|| container_memory(&self.container()))
            .flatten()
    }
    fn cgroup(&self) -> Option<Cgroup> {
        self.watch.as_ref()?.last()
    }
    fn oom_killed(&self) -> bool {
        self.docker
            && std::process::Command::new("docker")
                .args(["inspect", "-f", "{{.State.OOMKilled}}", &self.container()])
                .output()
                .is_ok_and(|o| o.stdout.starts_with(b"true"))
    }
    fn live(&self) -> Option<u64> {
        // A connection of its own, after the cell's clients have stopped.
        let mut c = http::Http::try_connect(&self.addr(), Duration::from_secs(5)).ok()?;
        let (status, body) = c.try_request("GET", "/_metrics", "text/plain", b"").ok()?;
        (status == 200).then(|| metric_sum(body, "fennec_data_bytes"))
    }
    fn load(&mut self, records: u64) {
        if self.docker {
            self.stop();
            remove_volume(&self.volume());
        } else {
            let _ = std::fs::remove_file(self.dir.join("ycsb-server.fennec"));
        }
        self.start("250");
        let mut c = http::Http::connect(&self.addr());
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
        c.query(
            &format!("create collection usertable ({})", fields.join(", ")),
            "",
        );
        let mut key = 1;
        let mut body = String::new();
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
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
    }
    fn set_mode(&mut self, mode: Mode) {
        // The server is started again over its file with the policy.
        self.start(match mode {
            Mode::Durable => "always",
            Mode::Buffered => "250",
            Mode::Snapshot => unreachable!("not a mode of the server's"),
        });
    }
    fn client(&self) -> Box<dyn Client> {
        Box::new(ServerClient {
            c: http::Http::connect(&self.addr()),
            scan: (1..=MAX_SCAN).map(scan_text).collect(),
            params: String::with_capacity(2048),
            insert: insert_text(),
            set: (0..FIELDS)
                .map(|i| format!("set usertable {{field{i}: $2}} where id = $1"))
                .collect(),
        })
    }
}

struct ServerClient {
    c: http::Http,
    scan: Vec<String>,
    params: String,
    insert: String,
    set: Vec<String>,
}

fn expect(body: &[u8], what: &[u8]) {
    assert!(
        body.windows(what.len()).any(|w| w == what),
        "{}",
        String::from_utf8_lossy(&body[..body.len().min(300)])
    );
}

impl Client for ServerClient {
    fn ping(&mut self) -> bool {
        self.c.get("/_health");
        true
    }
    fn read(&mut self, key: u64) {
        let out = self
            .c
            .query("get usertable where id = $1", &key.to_string());
        expect(out, b"\"field9\"");
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        self.params.clear();
        write!(self.params, "{key},\"{value}\"").unwrap();
        let out = self.c.query(&self.set[field], &self.params);
        expect(out, b"1");
    }
    fn insert(&mut self, key: u64, values: &[String]) {
        self.params.clear();
        write!(self.params, "{key}").unwrap();
        for v in values {
            write!(self.params, ",\"{v}\"").unwrap();
        }
        self.c.query(&self.insert, &self.params);
    }
    fn scan(&mut self, key: u64, len: u64) {
        let out = self.c.query(&self.scan[len as usize - 1], &key.to_string());
        expect(out, b"\"field9\"");
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        json_rows(self.c.body())
    }
    fn affected(&mut self) -> u64 {
        // `{"affected":1}`, all of it.
        let b = self.c.body();
        b.strip_prefix(b"{\"affected\":")
            .and_then(|r| r.strip_suffix(b"}"))
            .and_then(|n| std::str::from_utf8(n).ok()?.parse().ok())
            .unwrap_or(u64::MAX)
    }
}

/// The sum of a family's samples in a `/_metrics` page, every label's:
/// `fennec_data_bytes{collection="usertable"} 123`.
fn metric_sum(page: &[u8], family: &str) -> u64 {
    String::from_utf8_lossy(page)
        .lines()
        .filter(|l| {
            l.strip_prefix(family)
                .is_some_and(|r| r.starts_with('{') || r.starts_with(' '))
        })
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
        .sum::<f64>() as u64
}

/// The rows of an answer to `get`, `[{"id":1,"field0":"..",..},..]`: each
/// one's `id` and its fields' digests. The values are letters and digits,
/// so a string ends at the next quote; anything else in the body is no row.
fn json_rows(body: &[u8]) -> Vec<(u64, [u64; FIELDS])> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(open) = body[i..].iter().position(|&b| b == b'{') {
        let start = i + open + 1;
        let Some(close) = body[start..].iter().position(|&b| b == b'}') else {
            break;
        };
        let obj = &body[start..start + close];
        i = start + close + 1;
        let mut key = u64::MAX;
        let mut d = [0; FIELDS];
        for pair in obj.split(|&b| b == b',') {
            let Some(colon) = pair.iter().position(|&b| b == b':') else {
                continue;
            };
            let (name, value) = (&pair[..colon], &pair[colon + 1..]);
            let name = name.strip_prefix(b"\"").and_then(|n| n.strip_suffix(b"\""));
            match name {
                Some(b"id") => {
                    key = std::str::from_utf8(value)
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(u64::MAX)
                }
                Some(n) if n.starts_with(b"field") => {
                    let f: Option<usize> = std::str::from_utf8(&n[5..])
                        .ok()
                        .and_then(|f| f.parse().ok());
                    let v = value
                        .strip_prefix(b"\"")
                        .and_then(|v| v.strip_suffix(b"\""));
                    if let (Some(f), Some(v)) = (f, v) {
                        if f < FIELDS {
                            d[f] = digest(v);
                        }
                    }
                }
                _ => {}
            }
        }
        out.push((key, d));
    }
    out
}

// ---- PostgreSQL

#[cfg(feature = "rivals")]
struct Pg {
    url: String,
    durable: bool,
    container: Option<Container>,
}

/// PostgreSQL 17 as its docs tune it for this machine's Docker VM (3.8 GB):
/// a quarter of the memory as shared buffers, the rest left to the page
/// cache it counts on, and checkpoints spaced so the load is not one. Its
/// data in a volume, the VM's own file system, not the image's layers.
#[cfg(feature = "rivals")]
const PG_RUN: &[&str] = &[
    "-e",
    "POSTGRES_PASSWORD=fennec",
    "-e",
    "POSTGRES_DB=ycsb",
    "-p",
    "127.0.0.1:55433:5432",
    "--shm-size=1g",
    "-v",
    "fennecycsb-pg:/var/lib/postgresql/data",
    "postgres:17",
    "-c",
    "shared_buffers=1GB",
    "-c",
    "effective_cache_size=2GB",
    "-c",
    "max_wal_size=4GB",
    "-c",
    "max_connections=200",
];

/// MongoDB 8 as it comes: WiredTiger's cache is half the memory less 1 GB.
#[cfg(feature = "rivals")]
const MONGO_RUN: &[&str] = &[
    "-p",
    "127.0.0.1:27018:27017",
    "-v",
    "fennecycsb-mongo:/data/db",
    "mongo:8",
];

/// A container started for a system's turn, removed with its volume after,
/// so that one system's memory is not in the VM beside another's.
#[cfg(feature = "rivals")]
struct Container(String);

#[cfg(feature = "rivals")]
impl Container {
    fn start(name: &str, args: &[&str]) -> Container {
        let c = Container(format!("fennecycsb-{name}"));
        c.remove();
        let st = std::process::Command::new("docker")
            .args(["run", "-d", "--name", &c.0])
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "docker run {args:?}");
        c
    }
    fn remove(&self) {
        let quiet = |args: &[&str]| {
            let _ = std::process::Command::new("docker")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        quiet(&["rm", "-f", "-v", &self.0]);
        quiet(&["volume", "rm", "-f", &self.0]);
    }
}

#[cfg(feature = "rivals")]
impl Drop for Container {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(feature = "rivals")]
impl System for Pg {
    fn name(&self) -> &'static str {
        "pg"
    }
    fn size(&self) -> Option<u64> {
        // The table, its TOAST and its primary key; the WAL apart.
        let mut c = postgres::Client::connect(&self.url, postgres::NoTls).ok()?;
        let n: i64 = c
            .query_one("SELECT pg_total_relation_size('usertable')", &[])
            .ok()?
            .get(0);
        Some(n as u64)
    }
    fn size_beside(&self) -> Option<u64> {
        // A connection a look: a backend forked and a SCRAM exchange.
        None
    }
    fn memory(&self) -> Option<u64> {
        container_memory(&self.container.as_ref()?.0)
    }
    fn load(&mut self, records: u64) {
        let mut c = postgres::Client::connect(&self.url, postgres::NoTls).unwrap();
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i} text")).collect();
        c.batch_execute(&format!(
            "DROP TABLE IF EXISTS usertable;
             CREATE TABLE usertable (ycsb_key bigint PRIMARY KEY, {});",
            fields.join(", ")
        ))
        .unwrap();
        // COPY, PostgreSQL's bulk path, a transaction of 1 000 rows each.
        let mut key = 1;
        let mut line = String::new();
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
            let mut w = c.copy_in("COPY usertable FROM STDIN").unwrap();
            for k in key..end {
                line.clear();
                write!(line, "{k}").unwrap();
                for v in record_values(k) {
                    write!(line, "\t{v}").unwrap();
                }
                line.push('\n');
                w.write_all(line.as_bytes()).unwrap();
            }
            w.finish().unwrap();
            key = end;
        }
        c.batch_execute("VACUUM ANALYZE usertable").unwrap();
        c.batch_execute("CHECKPOINT").unwrap();
    }
    fn set_mode(&mut self, mode: Mode) {
        self.durable = mode == Mode::Durable;
    }
    fn client(&self) -> Box<dyn Client> {
        let mut c = postgres::Client::connect(&self.url, postgres::NoTls).unwrap();
        c.batch_execute(if self.durable {
            "SET synchronous_commit = on"
        } else {
            "SET synchronous_commit = off"
        })
        .unwrap();
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i}")).collect();
        let marks: Vec<String> = (0..=FIELDS).map(|i| format!("${}", i + 1)).collect();
        let read = c
            .prepare("SELECT * FROM usertable WHERE ycsb_key = $1")
            .unwrap();
        let scan = c
            .prepare("SELECT * FROM usertable WHERE ycsb_key >= $1 ORDER BY ycsb_key LIMIT $2")
            .unwrap();
        let set = (0..FIELDS)
            .map(|i| {
                c.prepare(&format!(
                    "UPDATE usertable SET field{i} = $2 WHERE ycsb_key = $1"
                ))
                .unwrap()
            })
            .collect();
        let insert = c
            .prepare(&format!(
                "INSERT INTO usertable (ycsb_key, {}) VALUES ({})",
                fields.join(", "),
                marks.join(", ")
            ))
            .unwrap();
        Box::new(PgClient {
            c,
            last: Vec::new(),
            read,
            scan,
            set,
            insert,
        })
    }
}

#[cfg(feature = "rivals")]
struct PgClient {
    c: postgres::Client,
    last: Vec<(i64, [String; FIELDS])>,
    read: postgres::Statement,
    scan: postgres::Statement,
    set: Vec<postgres::Statement>,
    insert: postgres::Statement,
}

#[cfg(feature = "rivals")]
fn pg_fields(r: &postgres::Row) -> (i64, [String; FIELDS]) {
    let mut out: [String; FIELDS] = Default::default();
    for (i, o) in out.iter_mut().enumerate() {
        *o = r.get(i + 1);
    }
    (r.get(0), out)
}

#[cfg(feature = "rivals")]
impl Client for PgClient {
    fn ping(&mut self) -> bool {
        self.c.simple_query("").unwrap();
        true
    }
    fn read(&mut self, key: u64) {
        let r = self.c.query_one(&self.read, &[&(key as i64)]).unwrap();
        self.last.clear();
        self.last.push(pg_fields(&r));
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        let n = self
            .c
            .execute(&self.set[field], &[&(key as i64), &value])
            .unwrap();
        assert_eq!(n, 1);
    }
    fn insert(&mut self, key: u64, v: &[String]) {
        let k = key as i64;
        self.c
            .execute(
                &self.insert,
                &[
                    &k, &v[0], &v[1], &v[2], &v[3], &v[4], &v[5], &v[6], &v[7], &v[8], &v[9],
                ],
            )
            .unwrap();
    }
    fn scan(&mut self, key: u64, len: u64) {
        let rows = self
            .c
            .query(&self.scan, &[&(key as i64), &(len as i64)])
            .unwrap();
        assert!(!rows.is_empty());
        self.last = rows.iter().map(pg_fields).collect();
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        digests_of(&self.last)
    }
}

// ---- MongoDB

#[cfg(feature = "rivals")]
use mongodb::bson::{doc, Document};
#[cfg(feature = "rivals")]
use mongodb::options::{Acknowledgment, CollectionOptions, WriteConcern};

#[cfg(feature = "rivals")]
struct Mongo {
    client: mongodb::sync::Client,
    durable: bool,
    container: Option<Container>,
}

#[cfg(feature = "rivals")]
impl Mongo {
    fn coll(&self, durable: bool) -> mongodb::sync::Collection<Document> {
        let wc = WriteConcern::builder()
            .w(Acknowledgment::Nodes(1))
            .journal(durable)
            .build();
        self.client.database("ycsb").collection_with_options(
            "usertable",
            CollectionOptions::builder().write_concern(wc).build(),
        )
    }
}

#[cfg(feature = "rivals")]
fn mongo_doc(key: u64, values: &[String]) -> Document {
    let mut d = doc! { "_id": key as i64 };
    for (i, v) in values.iter().enumerate() {
        d.insert(format!("field{i}"), v.as_str());
    }
    d
}

#[cfg(feature = "rivals")]
impl System for Mongo {
    fn name(&self) -> &'static str {
        "mongo"
    }
    fn size(&self) -> Option<u64> {
        // The collection's files and its _id index, as WiredTiger holds
        // them compressed.
        let d = self
            .client
            .database("ycsb")
            .run_command(doc! {"collStats": "usertable"})
            .run()
            .ok()?;
        let num = |k: &str| match d.get(k) {
            Some(mongodb::bson::Bson::Int32(n)) => *n as u64,
            Some(mongodb::bson::Bson::Int64(n)) => *n as u64,
            Some(mongodb::bson::Bson::Double(n)) => *n as u64,
            _ => 0,
        };
        Some(num("storageSize") + num("totalIndexSize"))
    }
    fn memory(&self) -> Option<u64> {
        container_memory(&self.container.as_ref()?.0)
    }
    fn load(&mut self, records: u64) {
        let coll = self.coll(false);
        coll.drop().run().unwrap();
        let mut key = 1;
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
            let docs: Vec<Document> = (key..end)
                .map(|k| mongo_doc(k, &record_values(k)))
                .collect();
            coll.insert_many(docs).run().unwrap();
            key = end;
        }
        // The load's writes on disk, as the others' are.
        self.client
            .database("admin")
            .run_command(doc! {"fsync": 1})
            .run()
            .unwrap();
    }
    fn set_mode(&mut self, mode: Mode) {
        self.durable = mode == Mode::Durable;
    }
    fn client(&self) -> Box<dyn Client> {
        Box::new(MongoClient {
            admin: self.client.database("admin"),
            coll: self.coll(self.durable),
            fields: (0..FIELDS).map(|i| format!("field{i}")).collect(),
            last: Vec::new(),
        })
    }
}

#[cfg(feature = "rivals")]
struct MongoClient {
    admin: mongodb::sync::Database,
    coll: mongodb::sync::Collection<Document>,
    fields: Vec<String>,
    last: Vec<Document>,
}

#[cfg(feature = "rivals")]
impl Client for MongoClient {
    fn ping(&mut self) -> bool {
        self.admin.run_command(doc! {"ping": 1}).run().unwrap();
        true
    }
    fn read(&mut self, key: u64) {
        let d = self
            .coll
            .find_one(doc! {"_id": key as i64})
            .run()
            .unwrap()
            .expect("the record");
        assert_eq!(d.len(), FIELDS + 1);
        self.last.clear();
        self.last.push(d);
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        let mut set = Document::new();
        set.insert(self.fields[field].as_str(), value);
        let r = self
            .coll
            .update_one(doc! {"_id": key as i64}, doc! {"$set": set})
            .run()
            .unwrap();
        assert_eq!(r.matched_count, 1);
    }
    fn insert(&mut self, key: u64, values: &[String]) {
        self.coll.insert_one(mongo_doc(key, values)).run().unwrap();
    }
    fn scan(&mut self, key: u64, len: u64) {
        let docs: Vec<Document> = self
            .coll
            .find(doc! {"_id": {"$gte": key as i64}})
            .sort(doc! {"_id": 1})
            .limit(len as i64)
            .run()
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!docs.is_empty());
        self.last = docs;
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        self.last
            .iter()
            .map(|d| {
                let mut out = [0; FIELDS];
                for (o, f) in out.iter_mut().zip(&self.fields) {
                    if let Ok(v) = d.get_str(f) {
                        *o = digest(v.as_bytes());
                    }
                }
                (d.get_i64("_id").map(|k| k as u64).unwrap_or(u64::MAX), out)
            })
            .collect()
    }
}

// ---- Redis

/// Redis 7 as its image comes but for persistence: one thread executing
/// commands (`io-threads 1`), the data set in memory, the snapshots of its
/// default `save 3600 1 300 100 60 10000` on in every mode -- Redis's
/// persistence page has the append-only file and the snapshots both for
/// "a degree of data safety comparable to what PostgreSQL can provide".
/// It loads with the append-only file fsynced once a second, as
/// fennec-server loads under `--sync 250`, and `set_mode` sets the mode
/// over it. Its data in a volume, as PostgreSQL's and MongoDB's are.
#[cfg(feature = "rivals")]
const REDIS_RUN: &[&str] = &[
    "-p",
    "127.0.0.1:56380:6379",
    "-v",
    "fennecycsb-redis:/data",
    "redis:7",
    "redis-server",
    "--appendonly",
    "yes",
    "--appendfsync",
    "everysec",
];

/// The sorted set of every key YCSB's binding keeps for its scans, under
/// the binding's name for it (`INDEX_KEY`).
#[cfg(feature = "rivals")]
const REDIS_INDEX: &str = "_indices";

/// A record as YCSB's own Redis binding keeps it (`redis/src/main/java/
/// site/ycsb/db/RedisClient.java` in brianfrankcooper/YCSB): a hash a key,
/// a field of it each, read whole by `HGETALL` and written by `HSET`
/// (`HMSET` there, the name it had before Redis 4), every key also a
/// member of the sorted set `_indices`, and a scan the members from the
/// key's score on (`ZRANGEBYSCORE ... LIMIT 0 n`) and then each one's
/// record by `HGETALL`. The key is the integer every system here keys by,
/// and its score is the key itself, where the binding scores a key by
/// Java's `hashCode` of it -- "scattered along the whole space of doubles",
/// its comment says, adding that a real one would use the keys' own
/// values: so a scan reads the keys from its own in order, as every other
/// system's does. Two things go as Redis's docs would have them rather
/// than as the binding sends them: an insert's `HSET` and `ZADD` are one
/// `MULTI`/`EXEC`, where the binding sends two commands and a scan could
/// meet the key before its record -- one round trip and one fsync, as
/// every other system's insert is one statement -- and a scan asks for
/// its records in one pipeline, where the binding asks one at a time, a
/// round trip each.
#[cfg(feature = "rivals")]
struct Redis {
    url: String,
    /// Started afresh for each load and removed with its volume, so a load
    /// begins from nothing on disk or in memory; `None` for a server of
    /// the reader's own (`FENNECBENCH_YCSB_REDIS`), which a load empties.
    container: Option<Container>,
    own: bool,
}

/// A connection with `TCP_NODELAY`, which redis-rs leaves off unless told:
/// a pipeline written in pieces would wait on the ACK of the first.
/// Waits for the server: Docker's port takes a connection before Redis
/// listens, so a `PING` answered is the sign.
#[cfg(feature = "rivals")]
fn redis_connect(url: &str) -> redis::Connection {
    use redis::IntoConnectionInfo as _;
    let info = url
        .into_connection_info()
        .unwrap()
        .set_tcp_settings(redis::io::tcp::TcpSettings::default().set_nodelay(true));
    let client = redis::Client::open(info).unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let c = client.get_connection().and_then(|mut c| {
            redis::cmd("PING").query::<String>(&mut c)?;
            Ok(c)
        });
        match c {
            Ok(c) => return c,
            Err(e) => {
                assert!(Instant::now() < until, "Redis did not start: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// One field of `INFO <section>`.
#[cfg(feature = "rivals")]
fn redis_info(c: &mut redis::Connection, section: &str, field: &str) -> String {
    let info: String = redis::cmd("INFO").arg(section).query(c).unwrap();
    info.lines()
        .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Waits for Redis's child process, a snapshot or a rewrite of the
/// append-only file, to end and for none to be due: the load's end, and
/// what is measured after a phase, are the server's own and not a fork's
/// copy of its pages or a file half written.
#[cfg(feature = "rivals")]
fn redis_idle(c: &mut redis::Connection) {
    let until = Instant::now() + Duration::from_secs(1200);
    loop {
        let info: String = redis::cmd("INFO").arg("persistence").query(c).unwrap();
        let busy = [
            "rdb_bgsave_in_progress:1",
            "aof_rewrite_in_progress:1",
            "aof_rewrite_scheduled:1",
        ]
        .iter()
        .any(|f| info.lines().any(|l| l.trim() == *f));
        if !busy {
            return;
        }
        assert!(Instant::now() < until, "Redis's child did not end");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Runs a background job -- `BGSAVE`, `BGREWRITEAOF` -- to its end, and
/// holds its status to `ok`.
#[cfg(feature = "rivals")]
fn redis_background(c: &mut redis::Connection, cmd: &str, status: &str) {
    redis_idle(c);
    redis::cmd(cmd).query::<String>(c).unwrap();
    redis_idle(c);
    assert_eq!(redis_info(c, "persistence", status), "ok", "{cmd}");
}

#[cfg(feature = "rivals")]
impl Redis {
    fn connect(&self) -> redis::Connection {
        redis_connect(&self.url)
    }
}

#[cfg(feature = "rivals")]
impl System for Redis {
    fn name(&self) -> &'static str {
        "redis"
    }
    fn modes(&self) -> &'static [Mode] {
        &[Mode::Durable, Mode::Buffered, Mode::Snapshot]
    }
    fn size(&self) -> Option<u64> {
        // The snapshot and the append-only file's parts in the volume,
        // with no rewrite's file half written beside them.
        let name = &self.container.as_ref()?.0;
        redis_idle(&mut self.connect());
        let out = std::process::Command::new("docker")
            .args(["exec", name, "du", "-sb", "/data"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }
    fn size_beside(&self) -> Option<u64> {
        // A `docker exec` a look, and a wait for Redis's child.
        None
    }
    fn memory(&self) -> Option<u64> {
        let name = &self.container.as_ref()?.0;
        let mut c = self.connect();
        redis_idle(&mut c);
        eprintln!(
            "redis: used_memory {} MB, rss {} MB, by its own count",
            redis_info(&mut c, "memory", "used_memory")
                .parse::<u64>()
                .unwrap_or(0)
                >> 20,
            redis_info(&mut c, "memory", "used_memory_rss")
                .parse::<u64>()
                .unwrap_or(0)
                >> 20
        );
        container_memory(name)
    }
    fn load(&mut self, records: u64) {
        if self.own {
            // The last load's container and volume go first. The second
            // the new one takes to start is in the load's time, which is
            // tens of seconds at a million records.
            self.container = None;
            self.container = Some(Container::start("redis", REDIS_RUN));
        }
        let mut c = self.connect();
        if !self.own {
            redis::cmd("FLUSHALL").query::<String>(&mut c).unwrap();
            for (k, v) in [("appendonly", "yes"), ("appendfsync", "everysec")] {
                redis::cmd("CONFIG")
                    .arg("SET")
                    .arg(k)
                    .arg(v)
                    .query::<String>(&mut c)
                    .unwrap();
            }
        }
        // Pipelined, Redis's bulk path (its docs' "mass insertion" is the
        // protocol written ahead of the answers): a batch's records and
        // one `ZADD` of their keys, sent before any answer is read.
        let fields: Vec<String> = (0..FIELDS).map(|i| format!("field{i}")).collect();
        let mut key = 1;
        let mut pipe = redis::pipe();
        while key <= records {
            let end = (key + LOAD_BATCH as u64).min(records + 1);
            pipe.clear();
            let mut zadd = redis::cmd("ZADD");
            zadd.arg(REDIS_INDEX);
            for k in key..end {
                let mut hset = redis::cmd("HSET");
                hset.arg(k);
                for (f, v) in fields.iter().zip(record_values(k)) {
                    hset.arg(f).arg(v);
                }
                pipe.add_command(hset);
                zadd.arg(k).arg(k);
            }
            pipe.add_command(zadd);
            let added: Vec<i64> = pipe.query(&mut c).unwrap();
            let (zadded, hsets) = added.split_last().unwrap();
            assert!(hsets.iter().all(|&n| n == FIELDS as i64), "a record twice");
            assert_eq!(*zadded, (end - key) as i64);
            key = end;
        }
        // The load on disk, as the others' is, in both of Redis's forms
        // -- a snapshot, and the append-only file rewritten from the data
        // -- so no fork the load made due runs in the first cell.
        redis_background(&mut c, "BGSAVE", "rdb_last_bgsave_status");
        redis_background(&mut c, "BGREWRITEAOF", "aof_last_bgrewrite_status");
        let n: u64 = redis::cmd("DBSIZE").query(&mut c).unwrap();
        assert_eq!(n, records + 1, "the records and the index");
    }
    fn set_mode(&mut self, mode: Mode) {
        let mut c = self.connect();
        let set = |c: &mut redis::Connection, k: &str, v: &str| {
            redis::cmd("CONFIG")
                .arg("SET")
                .arg(k)
                .arg(v)
                .query::<String>(c)
                .unwrap();
        };
        match mode {
            // An fsync of the append-only file before each answer.
            Mode::Durable => set(&mut c, "appendfsync", "always"),
            // An fsync a second, on a thread of its own.
            Mode::Buffered => set(&mut c, "appendfsync", "everysec"),
            // No append-only file: the snapshots alone, as the image comes.
            Mode::Snapshot => set(&mut c, "appendonly", "no"),
        }
        let get = |c: &mut redis::Connection, k: &str| -> String {
            let v: Vec<String> = redis::cmd("CONFIG").arg("GET").arg(k).query(c).unwrap();
            v.get(1).cloned().unwrap_or_default()
        };
        eprintln!(
            "redis {}: appendonly {}, appendfsync {}, save \"{}\", io-threads {}",
            mode.name(),
            get(&mut c, "appendonly"),
            get(&mut c, "appendfsync"),
            get(&mut c, "save"),
            get(&mut c, "io-threads")
        );
    }
    fn client(&self) -> Box<dyn Client> {
        let mut insert = redis::pipe();
        insert.atomic();
        Box::new(RedisClient {
            c: self.connect(),
            fields: (0..FIELDS).map(|i| format!("field{i}")).collect(),
            insert,
            scan: redis::pipe(),
            last: Vec::new(),
            affected: 0,
        })
    }
}

#[cfg(feature = "rivals")]
struct RedisClient {
    c: redis::Connection,
    fields: Vec<String>,
    /// An insert's `MULTI`/`EXEC`, and a scan's pipeline of `HGETALL`s.
    insert: redis::Pipeline,
    scan: redis::Pipeline,
    /// The last read's or scan's answer: each key and its `HGETALL` reply.
    last: Vec<(u64, redis::Value)>,
    /// What the last write said it wrote, in records (`Client::affected`).
    affected: u64,
}

/// An `HGETALL` reply's field-value pairs, a flat array under RESP2 and a
/// map under RESP3.
#[cfg(feature = "rivals")]
fn redis_pairs(v: &redis::Value) -> Vec<(&redis::Value, &redis::Value)> {
    match v {
        redis::Value::Array(a) => a.as_chunks::<2>().0.iter().map(|[k, v]| (k, v)).collect(),
        redis::Value::Map(m) => m.iter().map(|(k, v)| (k, v)).collect(),
        _ => Vec::new(),
    }
}

/// A record's fields' digests from its `HGETALL` reply, 0 for a field it
/// did not hold.
#[cfg(feature = "rivals")]
fn redis_digests(v: &redis::Value) -> [u64; FIELDS] {
    let mut d = [0; FIELDS];
    for (k, v) in redis_pairs(v) {
        let (redis::Value::BulkString(k), redis::Value::BulkString(v)) = (k, v) else {
            continue;
        };
        let f: Option<usize> = k
            .strip_prefix(b"field")
            .and_then(|f| std::str::from_utf8(f).ok()?.parse().ok());
        if let Some(f) = f.filter(|f| *f < FIELDS) {
            d[f] = digest(v);
        }
    }
    d
}

#[cfg(feature = "rivals")]
impl Client for RedisClient {
    fn ping(&mut self) -> bool {
        redis::cmd("PING").query::<String>(&mut self.c).unwrap();
        true
    }
    fn read(&mut self, key: u64) {
        let v: redis::Value = redis::cmd("HGETALL").arg(key).query(&mut self.c).unwrap();
        assert_eq!(redis_pairs(&v).len(), FIELDS, "the record {key}");
        self.last.clear();
        self.last.push((key, v));
    }
    fn update(&mut self, key: u64, field: usize, value: &str) {
        // `HSET` answers the fields it added: none where the record held
        // the field, and one where there was no record to write over.
        let added: i64 = redis::cmd("HSET")
            .arg(key)
            .arg(&self.fields[field])
            .arg(value)
            .query(&mut self.c)
            .unwrap();
        assert_eq!(added, 0, "no record {key} to update");
        self.affected = 1;
    }
    fn insert(&mut self, key: u64, values: &[String]) {
        let mut hset = redis::cmd("HSET");
        hset.arg(key);
        for (f, v) in self.fields.iter().zip(values) {
            hset.arg(f).arg(v);
        }
        self.insert.clear();
        self.insert.add_command(hset);
        self.insert.cmd("ZADD").arg(REDIS_INDEX).arg(key).arg(key);
        let (fields, indexed): (i64, i64) = self.insert.query(&mut self.c).unwrap();
        assert!(
            fields == FIELDS as i64 && indexed == 1,
            "insert of {key}: {fields} fields, {indexed} indexed"
        );
        self.affected = 1;
    }
    fn scan(&mut self, key: u64, len: u64) {
        let keys: Vec<u64> = redis::cmd("ZRANGEBYSCORE")
            .arg(REDIS_INDEX)
            .arg(key)
            .arg("+inf")
            .arg("LIMIT")
            .arg(0)
            .arg(len)
            .query(&mut self.c)
            .unwrap();
        assert!(!keys.is_empty());
        self.scan.clear();
        for k in &keys {
            self.scan.cmd("HGETALL").arg(*k);
        }
        let rows: Vec<redis::Value> = self.scan.query(&mut self.c).unwrap();
        self.last.clear();
        self.last.extend(keys.into_iter().zip(rows));
    }
    fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
        // A key the index named with no record behind it is a row of no
        // fields, which the check finds not as written.
        self.last
            .iter()
            .map(|(k, v)| (*k, redis_digests(v)))
            .collect()
    }
    fn affected(&mut self) -> u64 {
        self.affected
    }
}

// ------------------------------------------------------------- the runs

struct Config {
    records: u64,
    warmup: Duration,
    run: Duration,
    threads: Vec<usize>,
    workloads: Vec<char>,
    modes: Vec<Mode>,
    systems: Vec<String>,
    out: PathBuf,
    runs: usize,
    /// The probe's time at idle, in ms; measured at the start unless given.
    probe_base: Option<f64>,
    /// The least idle time between two cells, and between two systems.
    gap: Duration,
    turn_gap: Duration,
    image: String,
    run_id: String,
    /// Where `--verify` writes each cell's verdict; every operation is
    /// then logged and held to what was written (`check`).
    verify: Option<PathBuf>,
    /// fennecdb's files compacted on their own, in process and in the
    /// server, as each ships; `--no-auto-compact` for the runs before.
    auto_compact: bool,
    /// The operations a second the cell's threads send together, open
    /// loop (`--rate`): each thread keeps a schedule and an operation's
    /// time runs from when it was due, so a stall counts the wait of every
    /// one queued behind it rather than slowing the client down with the
    /// server -- coordinated omission. 0, as fast as each is answered.
    rate: f64,
    /// The family the lines are of (`--family`): `ycsb`, or a regime of its
    /// own whose cells must not join the published grid's medians.
    family: String,
    /// `server-docker`'s container and volume are named after it
    /// (`--docker-name`).
    docker_name: String,
    /// The memory the system runs under (`--memory`, Docker's form: `2g`):
    /// the server's container is started with it, and a process run in a
    /// container started with it says so to look at its own cgroup. Each
    /// cell then carries its cgroup's faults and memory, its limit and the
    /// live bytes, and a failed cell is a line saying so.
    memory: Option<String>,
    /// The bytes `--verify`'s logs may hold before the cell's measured
    /// window ends (`--verify-max`, in MB): in a container under a limit
    /// the logs are the process's own memory, which would otherwise push
    /// out the very pages measured, or past the limit.
    verify_max: Option<u64>,
    /// A directory the script beside the run answers in before each load
    /// and cell (`--gate`, `ask_gate`): whether the machine is quiet and
    /// the disks have room, which a process in a container cannot see.
    gate: Option<PathBuf>,
}

/// What a cell measured, and with `--verify` what each thread logged.
struct Cell {
    ops: u64,
    hist: BTreeMap<Op, Hist>,
    logs: Vec<Vec<Entry>>,
    /// The process's major faults over the measured window -- a page the
    /// kernel read from the disk, the in-process systems' -- and the
    /// largest the system's files stood, sampled every 100 ms -- or, for
    /// a system whose size is not read during a cell (`size_beside`),
    /// what they stood at once its clients stopped (`sampled` false).
    majflt: u64,
    largest: u64,
    sampled: bool,
    /// How long the measured window ran: `--seconds`, or less where the
    /// logs reached `--verify-max`.
    secs: f64,
    /// The system's cgroup, a look a second through the measured window.
    looks: Vec<Cgroup>,
    /// What ended it early: a client's panic -- a refused write, the
    /// server gone.
    failed: Option<String>,
}

/// A panic's message, as a cell that failed names it.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "a panic".into())
}

/// Docker's form of a size -- `512m`, `2g`, `1073741824` -- in bytes.
fn bytes_of(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let (n, mul) = match v.char_indices().last()? {
        (i, 'k') => (&v[..i], 1u64 << 10),
        (i, 'm') => (&v[..i], 1 << 20),
        (i, 'g') => (&v[..i], 1 << 30),
        (i, 'b') => (&v[..i], 1),
        _ => (&v[..], 1),
    };
    let n: f64 = n.parse().ok()?;
    Some((n * mul as f64) as u64)
}

/// The process's major faults so far (`getrusage`), `rusage`'s layout on
/// Linux and macOS alike: two `timeval`s and fourteen longs.
fn major_faults() -> u64 {
    #[repr(C)]
    struct Rusage {
        times: [i64; 4],
        rest: [i64; 14],
    }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut r = Rusage {
        times: [0; 4],
        rest: [0; 14],
    };
    // RUSAGE_SELF.
    unsafe { getrusage(0, &mut r) };
    r.rest[5] as u64
}

fn run_cell(
    sys: &dyn System,
    w: &Workload,
    threads: usize,
    records_now: u64,
    cfg: &Config,
) -> (Cell, u64) {
    let keys = Keys::new(records_now, threads);
    let expect_new = if w.mix.iter().any(|(o, _)| *o == Op::Insert) {
        (records_now / 10).max(10_000)
    } else {
        0
    };
    let space = records_now + expect_new;
    let base_scrambled = Scrambled::new();
    let base_latest = Zipfian::new(records_now);
    let measuring = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let verify = cfg.verify.is_some();
    // The bytes the threads' logs hold, told every 256 entries.
    let logged = AtomicU64::new(0);
    let clients: Vec<Box<dyn Client>> = (0..threads).map(|_| sys.client()).collect();
    let mut cell = Cell {
        ops: 0,
        hist: BTreeMap::new(),
        logs: Vec::new(),
        majflt: 0,
        largest: 0,
        sampled: false,
        secs: 0.0,
        looks: Vec::new(),
        failed: None,
    };
    // Open loop: each thread an operation every `every`, the threads'
    // schedules staggered across it.
    let every = match cfg.rate > 0.0 {
        true => Some(Duration::from_secs_f64(threads as f64 / cfg.rate)),
        false => None,
    };
    let t0 = Instant::now();
    std::thread::scope(|s| {
        let handles: Vec<_> = clients
            .into_iter()
            .enumerate()
            .map(|(t, mut client)| {
                let (keys, measuring, stop, logged) = (&keys, &measuring, &stop, &logged);
                let first = every.map(|e| t0 + e.mul_f64(t as f64 / threads as f64));
                let mut chooser = Chooser {
                    dist: w.dist,
                    scrambled: base_scrambled.clone(),
                    latest: base_latest.clone(),
                    space,
                };
                // The same seed for every system: the cell and the thread.
                let mut rng =
                    Rng::new(SEED ^ ((w.name as u64) << 32) ^ ((threads as u64) << 16) ^ t as u64);
                let mix = w.mix;
                s.spawn(move || {
                    let mut hist: BTreeMap<Op, Hist> = BTreeMap::new();
                    let mut value = String::with_capacity(FIELD_LEN);
                    let mut ops = 0u64;
                    let mut log = Vec::new();
                    let (mut sent, mut told) = (0u64, 0usize);
                    let at = |i: Instant| (i - t0).as_nanos() as u64;
                    while !stop.load(Ordering::Relaxed) {
                        // When this operation is due: now, closed loop, or
                        // its place in the schedule, waited for if early.
                        let due = match (first, every) {
                            (Some(first), Some(e)) => {
                                let due = first + e.mul_f64(sent as f64);
                                sent += 1;
                                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                                due
                            }
                            _ => Instant::now(),
                        };
                        let mut u = rng.f64();
                        let mut op = mix[mix.len() - 1].0;
                        for (o, p) in mix {
                            if u < *p {
                                op = *o;
                                break;
                            }
                            u -= p;
                        }
                        let key: u64;
                        let (mut field, mut len, mut seen) = (0usize, 0u64, 0u64);
                        // A read-modify-write's read, its end and its rows,
                        // which the update's answer would put out of reach.
                        let mut rmw_read = None;
                        let start = Instant::now();
                        match op {
                            Op::Read => {
                                key = chooser.next(&mut rng, keys);
                                client.read(key)
                            }
                            Op::Update => {
                                key = chooser.next(&mut rng, keys);
                                rng.value(&mut value);
                                field = rng.below(FIELDS as u64) as usize;
                                client.update(key, field, &value);
                            }
                            Op::Rmw => {
                                key = chooser.next(&mut rng, keys);
                                client.read(key);
                                if verify {
                                    rmw_read = Some((Instant::now(), client.got()));
                                }
                                rng.value(&mut value);
                                field = rng.below(FIELDS as u64) as usize;
                                client.update(key, field, &value);
                            }
                            Op::Scan => {
                                key = chooser.next(&mut rng, keys);
                                len = rng.below(MAX_SCAN) + 1;
                                if verify {
                                    // The key was in when it was drawn.
                                    seen = keys.last().max(key);
                                }
                                client.scan(key, len);
                            }
                            Op::Insert => {
                                // Published before the key is taken, a
                                // bound below it: a reader that sees the
                                // key handed out sees this too.
                                let below = keys.next.load(Ordering::SeqCst);
                                keys.in_flight[t].store(below, Ordering::SeqCst);
                                key = keys.next.fetch_add(1, Ordering::SeqCst);
                                keys.in_flight[t].store(key, Ordering::SeqCst);
                                client.insert(key, &record_values(key));
                                keys.in_flight[t].store(u64::MAX, Ordering::SeqCst);
                            }
                        }
                        let end = Instant::now();
                        if measuring.load(Ordering::Relaxed) {
                            // From when it was due: the same as its start
                            // closed loop, its wait in the queue too open.
                            let ns = (end - due.min(start)).as_nanos() as u64;
                            hist.entry(op).or_insert_with(Hist::new).record(ns);
                            ops += 1;
                        }
                        if !verify {
                            continue;
                        }
                        if log.len() >= told + 256 {
                            let more: u64 =
                                log[told..].iter().map(|e: &Entry| e.bytes() as u64).sum();
                            logged.fetch_add(more, Ordering::Relaxed);
                            told = log.len();
                        }
                        // What the operation was answered, kept after the
                        // timed span; the warm-up's writes are logged too,
                        // since the reads after them see them.
                        let (s, e) = (at(start), at(end));
                        let write =
                            |client: &mut Box<dyn Client>, s, field: usize, d| Entry::Write {
                                key,
                                field: field as u8,
                                value: d,
                                s,
                                e,
                                affected: client.affected(),
                            };
                        match op {
                            Op::Read => log.push(Entry::read(key, s, e, client.got())),
                            Op::Rmw => {
                                let (mid, rows) = rmw_read.take().unwrap();
                                log.push(Entry::read(key, s, at(mid), rows));
                                log.push(write(
                                    &mut client,
                                    at(mid),
                                    field,
                                    digest(value.as_bytes()),
                                ));
                            }
                            Op::Update => {
                                log.push(write(&mut client, s, field, digest(value.as_bytes())))
                            }
                            Op::Insert => log.push(write(&mut client, s, FIELDS, 0)),
                            Op::Scan => log.push(Entry::Scan {
                                key,
                                len,
                                seen,
                                next: keys.next.load(Ordering::SeqCst),
                                rows: client.got().iter().map(|(k, d)| (*k, combine(d))).collect(),
                            }),
                        }
                    }
                    (ops, hist, log)
                })
            })
            .collect();
        std::thread::sleep(cfg.warmup);
        measuring.store(true, Ordering::Relaxed);
        let faults = major_faults();
        let began = Instant::now();
        let ends = began + cfg.run;
        let mut look = began;
        loop {
            let now = Instant::now();
            if now >= look {
                cell.looks.extend(sys.cgroup());
                look += Duration::from_secs(1);
            }
            if now >= ends {
                break;
            }
            if let Some(max) = cfg.verify_max {
                if logged.load(Ordering::Relaxed) >= max {
                    eprintln!(
                        "  the verify logs reached {} MB after {:.1} s: the cell ends there",
                        max >> 20,
                        now.duration_since(began).as_secs_f64()
                    );
                    break;
                }
            }
            // Every client gone -- the server they ask killed -- ends it.
            if handles.iter().all(|h| h.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100).min(ends - now));
            if let Some(bytes) = sys.size_beside() {
                cell.largest = cell.largest.max(bytes);
                cell.sampled = true;
            }
        }
        cell.secs = began.elapsed().as_secs_f64();
        cell.majflt = major_faults() - faults;
        measuring.store(false, Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            match h.join() {
                Ok((ops, hist, log)) => {
                    cell.ops += ops;
                    for (op, h) in hist {
                        cell.hist.entry(op).or_insert_with(Hist::new).merge(&h);
                    }
                    cell.logs.push(log);
                }
                Err(p) => {
                    let why = panic_text(p.as_ref());
                    cell.failed.get_or_insert(why);
                }
            }
        }
        if !cell.sampled {
            cell.largest = sys.size().unwrap_or(0);
        }
    });
    let inserted = keys.next.load(Ordering::Acquire) - (records_now + 1);
    (cell, records_now + inserted)
}

// ----------------------------------------------------------------- verify

/// An operation as `--verify` logged it, its span in ns from the cell's
/// start: what it asked and what it was answered.
enum Entry {
    /// A read: the rows it was answered, the first one's key and digests.
    Read {
        key: u64,
        s: u64,
        e: u64,
        rows: usize,
        got: (u64, [u64; FIELDS]),
    },
    /// A scan: the highest key every key below which was in the database
    /// when it began (`seen`), the next key to be handed out when it ended,
    /// and each row's key and record digest.
    Scan {
        key: u64,
        len: u64,
        seen: u64,
        next: u64,
        rows: Vec<(u64, u64)>,
    },
    /// An update of one field, or an insert (`field` = `FIELDS`) of the
    /// record `record_values` draws for its key.
    Write {
        key: u64,
        field: u8,
        value: u64,
        s: u64,
        e: u64,
        affected: u64,
    },
}

impl Entry {
    /// What it holds, a scan's rows too: what `--verify-max` counts.
    fn bytes(&self) -> usize {
        std::mem::size_of::<Entry>()
            + match self {
                Entry::Scan { rows, .. } => rows.capacity() * std::mem::size_of::<(u64, u64)>(),
                _ => 0,
            }
    }

    fn read(key: u64, s: u64, e: u64, rows: Vec<(u64, [u64; FIELDS])>) -> Entry {
        Entry::Read {
            key,
            s,
            e,
            rows: rows.len(),
            got: rows.first().copied().unwrap_or((u64::MAX, [0; FIELDS])),
        }
    }
}

/// What each field holds as a cell begins, where a cell before it in the
/// phase wrote it -- the values it can hold, more than one only where the
/// writes that could have been the last overlapped -- in the registers
/// every answer is held to (`register.rs`), and the records' loaded values.
#[derive(Default)]
struct Model {
    regs: register::Registers,
    base: std::collections::HashMap<u64, [u64; FIELDS]>,
}

impl Model {
    fn base(&mut self, key: u64) -> [u64; FIELDS] {
        *self.base.entry(key).or_insert_with(|| base_digests(key))
    }
}

#[derive(Default)]
struct Verdict {
    reads: u64,
    scans: u64,
    rows: u64,
    writes: u64,
    mismatches: u64,
    /// Scan rows whose fields could have been too many combinations of
    /// overlapping writes to try; none in YCSB's E, which updates nothing.
    undecided: u64,
    examples: Vec<String>,
}

impl Verdict {
    fn wrong(&mut self, what: String) {
        self.mismatches += 1;
        if self.examples.len() < 8 {
            self.examples.push(what);
        }
    }
}

/// Holds every logged operation to what was written: a read answered the
/// record it asked for, each field the value of a write that could have
/// been the last before it -- one that ended before the read began and was
/// not followed by another that also did, or one that overlapped it -- or
/// the value the field held as the cell began where no write had ended; a
/// scan answered consecutive keys from its own as far as every key it
/// could see, no more than it asked for, each record as written; a write
/// wrote one record. A key's value is FNV-1a of its bytes (`digest`).
/// The model is moved on to what the fields hold after the cell.
fn check(logs: &[Vec<Entry>], model: &mut Model) -> Verdict {
    let mut v = Verdict::default();
    // Every write, into its field's register.
    for e in logs.iter().flatten() {
        if let Entry::Write {
            key,
            field,
            value,
            s,
            e,
            affected,
        } = *e
        {
            v.writes += 1;
            if affected != 1 {
                v.wrong(format!("write of {key} answered {affected} records"));
            }
            if field as usize == FIELDS {
                for (f, d) in model.base(key).into_iter().enumerate() {
                    model
                        .regs
                        .write(key, f as u8, register::W::answered(s, e, d));
                }
            } else {
                model
                    .regs
                    .write(key, field, register::W::answered(s, e, value));
            }
        }
    }
    // The reads in the order they ended: a register is atomic, so a read
    // is held to every read that ended before it began (`register.rs`).
    let mut reads: Vec<&Entry> = logs
        .iter()
        .flatten()
        .filter(|e| matches!(e, Entry::Read { .. }))
        .collect();
    reads.sort_by_key(|x| match x {
        Entry::Read { e, .. } => *e,
        _ => 0,
    });
    let scans = logs
        .iter()
        .flatten()
        .filter(|e| matches!(e, Entry::Scan { .. }));
    for e in reads.into_iter().chain(scans) {
        match e {
            Entry::Read {
                key,
                s,
                e,
                rows,
                got,
            } => {
                v.reads += 1;
                if *rows != 1 || got.0 != *key {
                    v.wrong(format!(
                        "read of {key} answered {rows} rows, the first {}",
                        got.0
                    ));
                    continue;
                }
                let base = model.base(*key);
                for (f, (&d, &b)) in got.1.iter().zip(&base).enumerate() {
                    if !model.regs.read(*key, f as u8, *s, *e, d, b) {
                        v.wrong(format!(
                            "read of {key} [{s}, {e}] ns: field{f} holds no value written to it"
                        ));
                        break;
                    }
                }
            }
            Entry::Scan {
                key,
                len,
                seen,
                next,
                rows,
            } => {
                v.scans += 1;
                v.rows += rows.len() as u64;
                let visible = (seen + 1).saturating_sub(*key).min(*len);
                if (rows.len() as u64) < visible || rows.len() as u64 > *len {
                    v.wrong(format!(
                        "scan of {len} from {key} answered {} rows, {visible} visible",
                        rows.len()
                    ));
                    continue;
                }
                for (i, (k, c)) in rows.iter().enumerate() {
                    let i = i as u64;
                    let in_order = if i < visible {
                        *k == key + i
                    } else {
                        *k > rows[i as usize - 1].0 && *k < *next
                    };
                    if !in_order {
                        v.wrong(format!("scan from {key}: row {i} is key {k}"));
                        break;
                    }
                    // The record as written: each field's starting value or
                    // any written to it in the cell, every combination
                    // tried up to a bound.
                    let base = model.base(*k);
                    let cands: Vec<Vec<u64>> = (0..FIELDS)
                        .map(|f| model.regs.candidates(*k, f as u8, base[f]))
                        .collect();
                    if cands.iter().map(Vec::len).product::<usize>() > 4096 {
                        v.undecided += 1;
                        continue;
                    }
                    let mut pick = [0usize; FIELDS];
                    let found = 'combos: loop {
                        let mut d = [0; FIELDS];
                        for f in 0..FIELDS {
                            d[f] = cands[f][pick[f]];
                        }
                        if combine(&d) == *c {
                            break true;
                        }
                        for f in 0..=FIELDS {
                            if f == FIELDS {
                                break 'combos false;
                            }
                            pick[f] += 1;
                            if pick[f] < cands[f].len() {
                                break;
                            }
                            pick[f] = 0;
                        }
                    };
                    if !found {
                        v.wrong(format!(
                            "scan from {key}: the record of {k} is not as written"
                        ));
                        break;
                    }
                }
            }
            Entry::Write { .. } => {}
        }
    }
    // What each field written holds now: the writes no other began after.
    let Model { regs, base } = model;
    regs.settle(u64::MAX, |key, f| {
        base.get(&key)
            .map_or_else(|| base_digests(key)[f as usize], |b| b[f as usize])
    });
    v
}

/// A fixed piece of work on one core, the best of three, in ms: the
/// machine's speed as it stands. The Air slows every core as it heats.
fn probe() -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let mut x = 1u64;
        for i in 0..40_000_000u64 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i) ^ (x >> 17);
        }
        std::hint::black_box(x);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    best
}

/// Waits `least`, then until the probe runs within 4% of the idle speed,
/// 10 minutes at the most; returns the probe's ratio to it when the cell
/// starts.
fn cool(base: f64, least: Duration) -> f64 {
    std::thread::sleep(least);
    let until = Instant::now() + Duration::from_secs(600);
    loop {
        let p = probe();
        let ratio = p / base;
        if ratio <= 1.04 || Instant::now() > until {
            return ratio;
        }
        eprintln!("  warm ({ratio:.2}x), waiting");
        std::thread::sleep(Duration::from_secs(15));
    }
}

const HEADER: &str = "run\tsystem\tmode\tworkload\tthreads\trecords\tseconds\tops\tops_s\tprobe";

/// The columns after the operations', each empty -- and so no line -- in a
/// run that did not ask for it: the open loop's rate (`--rate`); under
/// `--memory` the cgroup's major faults a second over the window and in
/// its worst second, the most it was charged and the most of that
/// anonymous, its files' pages on average, the limit its own `memory.max`
/// says, the live records' bytes, and whether the cell failed and the
/// kernel killed the system for its memory; with `--verify` its verdict;
/// and `env_failed`, a cell the environment failed -- the disk short or
/// the machine busy before it, or the disk short during it (`ycsb fill
/// --env-failed`) -- which is no measurement and no failure of the
/// system's.
const TAIL: [&str; 13] = [
    "rate",
    "majflt_s",
    "majflt_s_max",
    "cg_peak",
    "anon_peak",
    "file_mean",
    "limit",
    "live",
    "failed",
    "oom",
    "mismatches",
    "undecided",
    "env_failed",
];

fn header() -> String {
    let mut h = HEADER.to_string();
    for op in OPS {
        for q in ["n", "p50", "p95", "p99", "max"] {
            write!(h, "\t{}_{q}", op.name()).unwrap();
        }
    }
    for t in TAIL {
        write!(h, "\t{t}").unwrap();
    }
    h
}

/// The columns of a results file, one value a line, as every family's file
/// in `crates/fennec-bench/results/` has them (`site/build.py` holds the
/// site's figures to their medians).
const RESULT_COLUMNS: &str = "run\tcommit\tmachine\tdate\tfamily\tcell\tmetric\tvalue";

/// What a line names beside its values: the commit the bench was built from
/// (`-dirty` with uncommitted changes; `FENNEC_BENCH_COMMIT` for a build run
/// where there is no checkout, a container's), the machine
/// (`FENNEC_BENCH_MACHINE`, else the host's name) and the day. Asked once a
/// process.
fn provenance() -> &'static (String, String, String) {
    static P: std::sync::OnceLock<(String, String, String)> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        let commit = match std::env::var("FENNEC_BENCH_COMMIT") {
            Ok(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => {
                let mut commit = run("git", &["rev-parse", "--short=8", "HEAD"]);
                if !run("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty() {
                    commit.push_str("-dirty");
                }
                commit
            }
        };
        let machine = std::env::var("FENNEC_BENCH_MACHINE")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| run("hostname", &["-s"]));
        // A blank commit or machine would be a line no run can be told
        // from: fail rather than write one (Windows has no `hostname -s`).
        assert!(!commit.is_empty(), "no commit: run from a git checkout");
        assert!(
            !machine.is_empty(),
            "no machine name: set FENNEC_BENCH_MACHINE"
        );
        (commit, machine, utc_date())
    })
}

/// Today's UTC date as `YYYY-MM-DD`, from the system clock (days since the
/// epoch to a civil date, Hinnant's algorithm), so no `date` command is
/// needed.
fn utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is before 1970")
        .as_secs();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

/// Appends a wide line (`header()`'s columns) to a results file as one line
/// a value: the cell is `<system>:<mode>:<workload>:<threads>`, each column
/// after the thread count a metric, an empty one left out.
fn append(path: &Path, family: &str, line: &str) {
    let head = header();
    let head: Vec<&str> = head.split('\t').collect();
    let v: Vec<&str> = line.split('\t').collect();
    let values: Vec<(&str, String)> = head
        .iter()
        .zip(&v)
        .skip(5)
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (*name, value.to_string()))
        .collect();
    append_values(path, family, v[0], &v[1..5].join(":"), &values);
}

/// Appends a cell's values to a results file, a line each, the header first
/// into a file that is new: the one write a line takes, so a run cut short
/// keeps every line it finished.
fn append_values(path: &Path, family: &str, run: &str, cell: &str, values: &[(&str, String)]) {
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    if new {
        writeln!(f, "{RESULT_COLUMNS}").unwrap();
    }
    let (commit, machine, date) = provenance();
    let mut text = String::new();
    for (name, value) in values {
        writeln!(
            text,
            "{run}\t{commit}\t{machine}\t{date}\t{family}\t{cell}\t{name}\t{value}"
        )
        .unwrap();
    }
    f.write_all(text.as_bytes()).unwrap();
}

fn make_system(name: &str, dir: &Path, cfg: &Config) -> Box<dyn System> {
    match name {
        "fennec" => Box::new(FennecLocal::new(
            dir,
            cfg.auto_compact,
            cfg.memory.is_some(),
        )),
        "server" | "server-docker" => Box::new(Server {
            dir: dir.to_path_buf(),
            docker: name == "server-docker",
            port: http::free_port(),
            proc: None,
            image: cfg.image.clone(),
            auto_compact: cfg.auto_compact,
            name: cfg.docker_name.clone(),
            memory: cfg.memory.clone(),
            watch: None,
        }),
        #[cfg(feature = "rivals")]
        other => rival(other, dir),
        #[cfg(not(feature = "rivals"))]
        other => panic!(
            "no system `{other}` in a build without the rivals: fennec, server, server-docker"
        ),
    }
}

/// SQLite, PostgreSQL, MongoDB or Redis, each started for its turn.
#[cfg(feature = "rivals")]
fn rival(name: &str, dir: &Path) -> Box<dyn System> {
    match name {
        "sqlite" => Box::new(Sqlite {
            path: dir.join("ycsb.sqlite"),
            durable: false,
        }),
        "pg" => {
            // A server of the reader's own when named, else a container
            // started here and removed with its volume after.
            let (url, container) = match std::env::var("FENNECBENCH_YCSB_PG") {
                Ok(url) => (url, None),
                Err(_) => (PG_URL.to_string(), Some(Container::start("pg", PG_RUN))),
            };
            let until = Instant::now() + Duration::from_secs(120);
            let mut c = loop {
                match postgres::Client::connect(&url, postgres::NoTls) {
                    Ok(c) => break c,
                    Err(e) => {
                        assert!(Instant::now() < until, "PostgreSQL did not start: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            };
            let v: String = c.query_one("SELECT version()", &[]).unwrap().get(0);
            let sb: String = c.query_one("SHOW shared_buffers", &[]).unwrap().get(0);
            eprintln!("pg: {v}, shared_buffers {sb}");
            Box::new(Pg {
                url,
                durable: false,
                container,
            })
        }
        "mongo" => {
            let (url, container) = match std::env::var("FENNECBENCH_YCSB_MONGO") {
                Ok(url) => (url, None),
                Err(_) => (
                    MONGO_URL.to_string(),
                    Some(Container::start("mongo", MONGO_RUN)),
                ),
            };
            let client = mongodb::sync::Client::with_uri_str(url).unwrap();
            let until = Instant::now() + Duration::from_secs(120);
            let info = loop {
                match client
                    .database("admin")
                    .run_command(doc! {"buildInfo": 1})
                    .run()
                {
                    Ok(d) => break d,
                    Err(e) => {
                        assert!(Instant::now() < until, "MongoDB did not start: {e}");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            };
            eprintln!("mongo: MongoDB {}", info.get_str("version").unwrap_or("?"));
            Box::new(Mongo {
                client,
                durable: false,
                container,
            })
        }
        "redis" => {
            let (url, own) = match std::env::var("FENNECBENCH_YCSB_REDIS") {
                Ok(url) => (url, false),
                Err(_) => (REDIS_URL.to_string(), true),
            };
            // Started now to say which Redis it is, and again by each load.
            let container = own.then(|| Container::start("redis", REDIS_RUN));
            let mut c = redis_connect(&url);
            eprintln!(
                "redis: Redis {}, {}",
                redis_info(&mut c, "server", "redis_version"),
                redis_info(&mut c, "memory", "mem_allocator")
            );
            Box::new(Redis {
                url,
                container,
                own,
            })
        }
        other => {
            panic!("no system `{other}`: fennec, sqlite, server, server-docker, pg, mongo, redis")
        }
    }
}

fn run_system(name: &str, cfg: &Config, base: f64, run: usize) {
    let dir = std::env::temp_dir().join(format!("fennec-ycsb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut sys = make_system(name, &dir, cfg);
    assert_eq!(sys.name(), name);
    let mut pinged = false;
    let modes: Vec<Mode> = cfg
        .modes
        .iter()
        .copied()
        .filter(|m| sys.modes().contains(m))
        .collect();
    // C writes nothing: one mode measures it, buffered where it runs.
    let c_mode = match modes.contains(&Mode::Buffered) {
        true => Some(Mode::Buffered),
        false => modes.first().copied(),
    };
    for &mode in &modes {
        // YCSB's own sequence: load, then A, B, C, F and D over that
        // data, then load afresh for E. Run after the others, E's scans
        // read records an update rewrote somewhere else -- fennecdb writes
        // the whole record again at its file's end -- and measured that
        // rather than the workload: its buffered scans went 3 108 -> 795
        // ops/s in the run that showed it.
        for phase in ["ABCFD", "E"] {
            let order: Vec<&Workload> = phase
                .chars()
                .filter(|c| cfg.workloads.contains(c))
                .filter(|c| *c != 'C' || c_mode == Some(mode))
                .map(|c| WORKLOADS.iter().find(|w| w.name == c).unwrap())
                .collect();
            if order.is_empty() {
                continue;
            }
            if let Err(why) = ask_gate(cfg, name, "load") {
                eprintln!("{name}: the environment's failure before a load: {why}");
                drop(sys);
                let _ = std::fs::remove_dir_all(&dir);
                std::process::exit(EXIT_ENV);
            }
            let ratio = cool(base, cfg.gap);
            eprintln!("{name}: loading {} records", cfg.records);
            let t = Instant::now();
            // A system that fails -- a server killed for its memory -- is a
            // failed line for each cell it would have run, never a line
            // missing (`--memory`'s runs are judged by every cell).
            let mut broken = attempt(|| sys.load(cfg.records));
            let secs = t.elapsed().as_secs_f64();
            let rate = cfg.records as f64 / secs;
            if broken.is_none() {
                eprintln!("{name}: loaded at {rate:.0} records/s");
                append(
                    &cfg.out,
                    &cfg.family,
                    &format!(
                        "{}\t{name}\t{}\tload\t1\t{}\t{secs:.2}\t{}\t{rate:.0}\t{ratio:.3}",
                        cfg.run_id,
                        mode.name(),
                        cfg.records,
                        cfg.records
                    ),
                );
            }
            // Under `--memory` the live records must be twice the limit
            // before a cell runs: a run whose data fits is not the regime
            // measured, and three hours of cells would only find it out.
            if broken.is_none() && cfg.memory.is_some() {
                if let Err(why) = twice_the_limit(sys.as_ref()) {
                    eprintln!("{name}: refused before any cell: {why}");
                    // Said where the script reads it: an exit status can be
                    // a wrapper's (`caffeinate`) rather than ours.
                    if let Some(gate) = &cfg.gate {
                        let _ = std::fs::write(gate.join("refused"), format!("{name}: {why}\n"));
                    }
                    drop(sys);
                    let _ = std::fs::remove_dir_all(&dir);
                    std::process::exit(EXIT_SMALL);
                }
            }
            if !pinged && broken.is_none() {
                pinged = true;
                broken = attempt(|| ping(sys.as_ref(), name, cfg, ratio));
            }
            if broken.is_none() {
                broken = attempt(|| sys.set_mode(mode));
            }
            let mut oom = broken.is_some() && sys.oom_killed();
            let mut records = cfg.records;
            // What the load wrote, which each cell's writes move on.
            let mut model = Model::default();
            for w in order {
                for &threads in &cfg.threads {
                    if let Some(why) = &broken {
                        let cell = Cell::failed(format!("not run: {why}"));
                        let after = After {
                            live: None,
                            oom,
                            verdict: None,
                        };
                        write_cell(
                            name, mode, w, threads, records, &cell, 0.0, cfg, run, &after,
                        );
                        continue;
                    }
                    if let Err(why) = ask_gate(cfg, name, "cell") {
                        eprintln!(
                            "{name} {} {} x{threads}: the environment's failure before it: {why}",
                            mode.name(),
                            w.name
                        );
                        drop(sys);
                        let _ = std::fs::remove_dir_all(&dir);
                        std::process::exit(EXIT_ENV);
                    }
                    let ratio = cool(base, cfg.gap);
                    let (cell, now) =
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run_cell(sys.as_ref(), w, threads, records, cfg)
                        })) {
                            Ok(out) => out,
                            Err(p) => (Cell::failed(panic_text(p.as_ref())), records),
                        };
                    records = now;
                    oom = cell.failed.is_some() && sys.oom_killed();
                    let verdict = match (&cfg.verify, &cell.failed) {
                        (Some(_), None) => Some(check(&cell.logs, &mut model)),
                        _ => None,
                    };
                    let after = After {
                        live: match (&cfg.memory, &cell.failed) {
                            (Some(_), None) => sys.live(),
                            _ => None,
                        },
                        oom,
                        verdict: verdict.as_ref(),
                    };
                    write_cell(
                        name, mode, w, threads, records, &cell, ratio, cfg, run, &after,
                    );
                    if let Some(why) = &cell.failed {
                        eprintln!(
                            "{name} {} {} x{threads} failed{}: {why}",
                            mode.name(),
                            w.name,
                            if oom { ", killed for its memory" } else { "" }
                        );
                        broken = Some(why.clone());
                    }
                    if let (Some(path), Some(v)) = (&cfg.verify, &verdict) {
                        eprintln!(
                            "verify {name} {} {} x{threads}: {} reads, {} scans ({} rows), {} writes: {} mismatches, {} undecided",
                            mode.name(),
                            w.name,
                            v.reads,
                            v.scans,
                            v.rows,
                            v.writes,
                            v.mismatches,
                            v.undecided
                        );
                        for e in &v.examples {
                            eprintln!("  {e}");
                        }
                        let new = !path.exists();
                        let mut f = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)
                            .unwrap();
                        if new {
                            writeln!(f, "run\tsystem\tmode\tworkload\tthreads\treads\tscans\tscan_rows\twrites\tmismatches\tundecided").unwrap();
                        }
                        writeln!(
                            f,
                            "{}\t{name}\t{}\t{}\t{threads}\t{}\t{}\t{}\t{}\t{}\t{}",
                            cfg.run_id,
                            mode.name(),
                            w.name,
                            v.reads,
                            v.scans,
                            v.rows,
                            v.writes,
                            v.mismatches,
                            v.undecided
                        )
                        .unwrap();
                    }
                }
            }
            if let Some(bytes) = sys.size() {
                eprintln!("{name}: {} MB on disk after {phase}", bytes >> 20);
                append(
                    &cfg.out,
                    &cfg.family,
                    &format!(
                        "{}\t{name}\t{}\tsize-{phase}\t1\t{records}\t0\t{bytes}\t0\t0",
                        cfg.run_id,
                        mode.name()
                    ),
                );
            }
            if let Some(bytes) = sys.memory() {
                eprintln!("{name}: {} MB in memory after {phase}", bytes >> 20);
                append(
                    &cfg.out,
                    &cfg.family,
                    &format!(
                        "{}\t{name}\t{}\tmemory-{phase}\t1\t{records}\t0\t{bytes}\t0\t0",
                        cfg.run_id,
                        mode.name()
                    ),
                );
            }
        }
    }
    drop(sys);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ycsb`'s exit when the live records after a load are under twice the
/// cgroup's limit (`twice_the_limit`): the run is refused, no cell written.
const EXIT_SMALL: i32 = 3;
/// `ycsb`'s exit when the gate (`--gate`) said the environment cannot take
/// the next step: the script writes the cells left as the environment's
/// (`ycsb fill --env-failed`).
const EXIT_ENV: i32 = 4;

/// Whether the live records `sys` holds are at least twice the limit its
/// cgroup's own `memory.max` says, both read -- the reason, naming both,
/// where not. A server's cgroup is looked at a second apart: a few seconds
/// for the first look.
fn twice_the_limit(sys: &dyn System) -> std::result::Result<(), String> {
    let mut limit = None;
    for _ in 0..20 {
        limit = sys.cgroup().and_then(|c| c.limit);
        if limit.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let live = sys.live();
    match (live, limit) {
        (Some(live), Some(limit)) if live >= 2 * limit => {
            eprintln!(
                "{}: {} MB live, {:.2} times the cgroup's limit of {} MB",
                sys.name(),
                live >> 20,
                live as f64 / limit as f64,
                limit >> 20
            );
            Ok(())
        }
        (Some(live), Some(limit)) => Err(format!(
            "{live} bytes live ({} MB) are under twice the cgroup's limit of {limit} bytes ({} MB): raise --records",
            live >> 20,
            limit >> 20
        )),
        (live, limit) => Err(format!(
            "the live bytes ({live:?}) or the cgroup's limit ({limit:?}, memory.max) could not be read"
        )),
    }
}

/// Asks the script beside the run whether the environment can take the
/// next step (`--gate <dir>`, a directory both see: a container's run
/// mounts it): writes `ask.<id>` holding `what` (`load` or `cell`) and
/// waits for `ok.<id>` or `stop.<id>`, the latter holding why. The script
/// answers once the host's load is down to its quiet line and the host's
/// and Docker's VM's free disk cover the step (`ycsb/env-check.sh`), which
/// a process in a container cannot see. No gate, no question.
fn ask_gate(cfg: &Config, system: &str, what: &str) -> std::result::Result<(), String> {
    static ASKED: AtomicU64 = AtomicU64::new(0);
    let Some(dir) = &cfg.gate else {
        return Ok(());
    };
    let n = ASKED.fetch_add(1, Ordering::Relaxed) + 1;
    // Unique across the run's processes, which share the directory.
    let id = format!("{}-{system}-{}-{n}", cfg.run_id, std::process::id());
    std::fs::write(dir.join(format!("ask.{id}")), what)
        .map_err(|e| format!("the gate {dir:?}: {e}"))?;
    // The script waits out the machine's load for a bounded time; past an
    // hour more no answer is one.
    let until = Instant::now() + Duration::from_secs(3600);
    loop {
        if dir.join(format!("ok.{id}")).exists() {
            return Ok(());
        }
        if let Ok(why) = std::fs::read_to_string(dir.join(format!("stop.{id}"))) {
            return Err(why.trim().to_string());
        }
        if Instant::now() > until {
            return Err("the gate gave no answer in an hour".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `f` run, its panic -- a refused request, the server gone -- the reason
/// it failed.
fn attempt(f: impl FnOnce()) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .err()
        .map(|p| panic_text(p.as_ref()))
}

/// What a cell's line says beside what it measured: its system's live
/// bytes after it, whether the system was killed for its memory, and the
/// verdict `--verify` gave it.
struct After<'a> {
    live: Option<u64>,
    oom: bool,
    verdict: Option<&'a Verdict>,
}

impl Cell {
    /// A cell that measured nothing, and why.
    fn failed(why: String) -> Cell {
        Cell {
            ops: 0,
            hist: BTreeMap::new(),
            logs: Vec::new(),
            majflt: 0,
            largest: 0,
            sampled: false,
            secs: 0.0,
            looks: Vec::new(),
            failed: Some(why),
        }
    }

    /// The columns of `TAIL` its cgroup's looks give: the major faults a
    /// second over the window and in the worst second between two looks,
    /// the most charged, the most of it anonymous, the files' pages on
    /// average.
    fn cgroup_columns(&self) -> [String; 5] {
        let l = &self.looks;
        if l.len() < 2 {
            return Default::default();
        }
        let faults = l[l.len() - 1].pgmajfault.saturating_sub(l[0].pgmajfault);
        let worst = l
            .windows(2)
            .map(|w| w[1].pgmajfault.saturating_sub(w[0].pgmajfault))
            .max()
            .unwrap_or(0);
        let file = l.iter().map(|c| c.file).sum::<u64>() / l.len() as u64;
        [
            format!("{:.1}", faults as f64 / (l.len() - 1) as f64),
            worst.to_string(),
            l.iter().map(|c| c.current).max().unwrap_or(0).to_string(),
            l.iter().map(|c| c.anon).max().unwrap_or(0).to_string(),
            file.to_string(),
        ]
    }
}

/// The round trip of the least request, 2 000 of them after 200.
fn ping(sys: &dyn System, name: &str, cfg: &Config, ratio: f64) {
    let mut c = sys.client();
    if !c.ping() {
        return;
    }
    let mut h = Hist::new();
    for i in 0..2_200 {
        let t = Instant::now();
        c.ping();
        if i >= 200 {
            h.record(t.elapsed().as_nanos() as u64);
        }
    }
    eprintln!(
        "{name}: the least request's round trip p50 {:.1} us, p99 {:.1}",
        h.pct(0.5),
        h.pct(0.99)
    );
    let mut line = format!(
        "{}\t{name}\t-\tping\t1\t{}\t0\t{}\t0\t{ratio:.3}",
        cfg.run_id, cfg.records, h.n
    );
    write!(
        line,
        "\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
        h.n,
        h.pct(0.5),
        h.pct(0.95),
        h.pct(0.99),
        h.max as f64 / 1e3
    )
    .unwrap();
    for _ in 1..OPS.len() {
        line.push_str("\t0\t\t\t\t");
    }
    append(&cfg.out, &cfg.family, &line);
}

#[allow(clippy::too_many_arguments)]
fn write_cell(
    name: &str,
    mode: Mode,
    w: &Workload,
    threads: usize,
    records: u64,
    cell: &Cell,
    ratio: f64,
    cfg: &Config,
    run: usize,
    after: &After,
) {
    if cell.failed.is_some() {
        // No measured value: a failed cell's `ops_s 0` would join a
        // median once a figure names the cell. What failed it is the log's.
        let cell = format!("{name}:{}:{}:{threads}", mode.name(), w.name);
        write_failed(cfg, &cell, after.oom, false);
        return;
    }
    let secs = cell.secs;
    let rate = match secs > 0.0 {
        true => cell.ops as f64 / secs,
        false => 0.0,
    };
    let mut line = format!(
        "{}\t{name}\t{}\t{}\t{threads}\t{records}\t{secs:.0}\t{}\t{rate:.0}\t{ratio:.3}",
        cfg.run_id,
        mode.name(),
        w.name,
        cell.ops,
    );
    let mut summary = String::new();
    for op in OPS {
        match cell.hist.get(&op) {
            Some(h) if h.n > 0 => {
                write!(
                    line,
                    "\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
                    h.n,
                    h.pct(0.5),
                    h.pct(0.95),
                    h.pct(0.99),
                    h.max as f64 / 1e3
                )
                .unwrap();
                write!(summary, " {} p99 {:.0}us", op.name(), h.pct(0.99)).unwrap();
            }
            _ => line.push_str("\t0\t\t\t\t"),
        }
    }
    // `TAIL`, in its order.
    let bounded = cfg.memory.is_some();
    let flag = |b: bool| match bounded {
        true => (b as u8).to_string(),
        false => String::new(),
    };
    let [majflt_s, majflt_max, cg_peak, anon_peak, file_mean] = cell.cgroup_columns();
    let tail = [
        match cfg.rate > 0.0 {
            true => format!("{:.0}", cfg.rate),
            false => String::new(),
        },
        majflt_s,
        majflt_max,
        cg_peak,
        anon_peak,
        file_mean,
        // What the kernel held the cgroup to, from its own `memory.max`.
        cell.looks
            .iter()
            .rev()
            .find_map(|c| c.limit)
            .map(|b| b.to_string())
            .unwrap_or_default(),
        after.live.map(|b| b.to_string()).unwrap_or_default(),
        flag(cell.failed.is_some()),
        flag(after.oom),
        after
            .verdict
            .map(|v| v.mismatches.to_string())
            .unwrap_or_default(),
        after
            .verdict
            .map(|v| v.undecided.to_string())
            .unwrap_or_default(),
        String::new(),
    ];
    for t in &tail {
        write!(line, "\t{t}").unwrap();
    }
    append(&cfg.out, &cfg.family, &line);
    let faults = match cell.looks.len() >= 2 {
        true => format!("{} major faults a second", tail[1]),
        false => format!("{} major faults", cell.majflt),
    };
    eprintln!(
        "run {run} {name} {} {} x{threads}: {rate:.0} ops/s{summary} (probe {ratio:.2}), {faults}, the file {} MB {}",
        mode.name(),
        w.name,
        cell.largest >> 20,
        if cell.sampled { "at the most" } else { "after it" }
    );
}

/// What a run of a cell stands for in `ycsb report`'s medians.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Standing {
    /// A measurement: in the medians.
    Counts,
    /// The system failed it: counted, and its figures are none.
    Failed,
    /// Under `--memory`, its live bytes were not twice its cgroup's limit,
    /// or it did not say either: another regime than the one measured.
    Small,
    /// The environment failed it (`env_failed`): no run of the system's.
    Env,
}

impl Standing {
    /// A run's standing from its metrics (`get`) in a file holding the
    /// columns `--memory` writes (`bounded`).
    fn of(get: impl Fn(&str) -> Option<f64>, bounded: bool) -> Standing {
        if get("env_failed") == Some(1.0) {
            return Standing::Env;
        }
        if get("failed") == Some(1.0) {
            return Standing::Failed;
        }
        let small = bounded
            && get("failed") == Some(0.0)
            && match (get("live"), get("limit")) {
                (Some(live), Some(limit)) => live < 2.0 * limit,
                _ => true,
            };
        match small {
            true => Standing::Small,
            false => Standing::Counts,
        }
    }
}

/// A cell that measured nothing, as a line: `failed 1` and whether the
/// kernel killed the system for its memory, or `env_failed 1` where the
/// environment failed it -- never a figure, which a median would take.
fn write_failed(cfg: &Config, cell: &str, oom: bool, env: bool) {
    let values: Vec<(&str, String)> = match env {
        true => vec![("env_failed", "1".into())],
        false => vec![("failed", "1".into()), ("oom", (oom as u8).to_string())],
    };
    append_values(&cfg.out, &cfg.family, &cfg.run_id, cell, &values);
}

// ----------------------------------------------------------------- report

/// The median of each cell's runs: ops/s and every op's p99, as the
/// lines `ycsb` wrote them, of the family the file's first line is of. A
/// file holding the columns `--memory` writes (`ycsb_larger`'s) gets them
/// too: each cell's faults a second, the most its cgroup was charged and
/// held anonymous, its limit and live bytes, and how many of its runs
/// failed, were killed for their memory, held live bytes under twice the
/// limit (`under_2x`: or did not say either), were failed by the
/// environment (`env`) or found a mismatch -- a run failed, under twice
/// the limit or the environment's counting in no median, and a line at
/// the end naming every cell short of a run that counts.
fn report(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        RESULT_COLUMNS,
        "a results file is one value a line: {RESULT_COLUMNS}"
    );
    let family = text
        .lines()
        .nth(1)
        .and_then(|l| l.split('\t').nth(4))
        .unwrap_or("ycsb")
        .to_string();
    // The wide lines the median reads: each run's cell as the columns
    // `header()` names, a metric written again by a later line winning (a
    // run cut short and completed under its own id), the file's order kept.
    let head: Vec<String> = header().split('\t').map(str::to_string).collect();
    let col = |name: &str| head.iter().position(|h| h == name).unwrap();
    let mut order: Vec<(String, String)> = Vec::new();
    let mut wide: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    // `ycsb heap`'s lines, apart: kind -> metric -> each run's value.
    let mut heaps: BTreeMap<String, BTreeMap<String, Vec<(String, f64)>>> = BTreeMap::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 8 || f[4] != family {
            continue;
        }
        if let Some(kind) = f[5].strip_prefix("server-docker:heap:") {
            if let Ok(x) = f[7].parse::<f64>() {
                let runs = heaps
                    .entry(kind.to_string())
                    .or_default()
                    .entry(f[6].to_string())
                    .or_default();
                // A run's value measured again counts as its last.
                runs.retain(|(run, _)| run != f[0]);
                runs.push((f[0].to_string(), x));
            }
            continue;
        }
        let key = (f[0].to_string(), f[5].to_string());
        let row = wide.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            let mut row = vec![String::new(); head.len()];
            row[0] = key.0.clone();
            for (i, part) in key.1.split(':').enumerate() {
                row[1 + i] = part.to_string();
            }
            row
        });
        if let Some(i) = head.iter().position(|h| h == f[6]) {
            row[i] = f[7].to_string();
        }
    }
    // The runs a later one replaced, by run and system (`superseded.tsv`
    // beside the results): they stay in the file, which is only appended
    // to, and count in no median.
    let superseded: Vec<(String, String)> =
        std::fs::read_to_string(path.with_file_name("superseded.tsv"))
            .unwrap_or_default()
            .lines()
            .skip(1)
            .filter_map(|l| {
                let mut f = l.split('\t');
                (f.next()? == family).then_some(())?;
                Some((f.next()?.to_string(), f.next()?.to_string()))
            })
            .collect();
    let lines = order
        .into_iter()
        .map(|k| wide.remove(&k).unwrap())
        .filter(|f| {
            !superseded
                .iter()
                .any(|(run, sys)| f[col("run")] == *run && f[col("system")] == *sys)
        });
    // A run's cell measured again -- a run cut short and completed by a
    // later invocation with its `--run-id` loads and pings again -- counts
    // once, as its last line: the file is only ever appended to.
    let mut latest: BTreeMap<(String, String, String, String, usize), Vec<String>> =
        BTreeMap::new();
    for f in lines {
        // A line no cell of the grid is -- another tool's in the family --
        // is no figure here.
        let Ok(threads) = f[col("threads")].parse() else {
            continue;
        };
        let key = (
            f[col("system")].clone(),
            f[col("mode")].clone(),
            f[col("workload")].clone(),
            f[col("run")].clone(),
            threads,
        );
        latest.insert(key, f);
    }
    let mut cells: BTreeMap<(String, String, String, usize), Vec<Vec<String>>> = BTreeMap::new();
    for ((sys, mode, wl, _, threads), f) in latest {
        cells.entry((sys, mode, wl, threads)).or_default().push(f);
    }
    let median = |v: &mut Vec<f64>| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[v.len() / 2])
    };
    // A size line stops at the probe's column: what it has not got is no
    // figure.
    let num = |f: &Vec<String>, c: &str| f.get(col(c))?.parse::<f64>().ok();
    let bounded = cells
        .values()
        .flatten()
        .any(|f| num(f, "failed").is_some() || num(f, "env_failed").is_some());
    let mut title = String::from("system\tmode\tworkload\tthreads\truns\tops_s\tREAD_p99\tUPDATE_p99\tINSERT_p99\tSCAN_p99\tRMW_p99\tREAD_p50\tUPDATE_p50\tINSERT_p50\tSCAN_p50\tRMW_p50\tprobe_max");
    if bounded {
        title.push_str(
            "\tmajflt_s\tcg_peak_mb\tanon_peak_mb\tlimit_mb\tlive_mb\tfailed\toom\tunder_2x\tenv\tmismatches",
        );
    }
    println!("{title}");
    let mut short: Vec<String> = Vec::new();
    for ((sys, mode, wl, threads), all) in cells {
        let standing: Vec<Standing> = all
            .iter()
            .map(|f| Standing::of(|c| num(f, c), bounded))
            .collect();
        let n = |s: Standing| standing.iter().filter(|x| **x == s).count();
        let (envs, failed, under) = (n(Standing::Env), n(Standing::Failed), n(Standing::Small));
        let oom = all
            .iter()
            .zip(&standing)
            .filter(|(f, s)| **s == Standing::Failed && num(f, "oom") == Some(1.0))
            .count();
        let runs: Vec<&Vec<String>> = all
            .iter()
            .zip(&standing)
            .filter(|(_, s)| **s == Standing::Counts)
            .map(|(f, _)| f)
            .collect();
        if bounded && (failed > 0 || under > 0 || envs > 0) {
            short.push(format!(
                "{sys}:{mode}:{wl}:{threads} ({} counted; {failed} failed, {under} under twice the limit, {envs} the environment's)",
                runs.len()
            ));
        }
        let mut ops: Vec<f64> = runs.iter().filter_map(|f| num(f, "ops_s")).collect();
        let mut probe: Vec<f64> = runs.iter().filter_map(|f| num(f, "probe")).collect();
        probe.sort_by(|a, b| a.total_cmp(b));
        let mut out = format!("{sys}\t{mode}\t{wl}\t{threads}\t{}", runs.len());
        match median(&mut ops) {
            Some(x) => write!(out, "\t{x:.0}").unwrap(),
            None => out.push_str("\t-"),
        }
        for q in ["p99", "p50"] {
            for op in OPS {
                let c = format!("{}_{q}", op.name());
                let mut v: Vec<f64> = runs.iter().filter_map(|f| num(f, &c)).collect();
                match median(&mut v) {
                    Some(x) => write!(out, "\t{x:.0}").unwrap(),
                    None => out.push_str("\t-"),
                }
            }
        }
        write!(out, "\t{:.2}", probe.last().copied().unwrap_or(0.0)).unwrap();
        if bounded {
            for (c, scale) in [
                ("majflt_s", 1.0),
                ("cg_peak", 1048576.0),
                ("anon_peak", 1048576.0),
                ("limit", 1048576.0),
                ("live", 1048576.0),
            ] {
                let mut v: Vec<f64> = runs.iter().filter_map(|f| num(f, c)).collect();
                match median(&mut v) {
                    Some(x) => write!(out, "\t{:.0}", x / scale).unwrap(),
                    None => out.push_str("\t-"),
                }
            }
            let mismatches: f64 = all
                .iter()
                .filter_map(|f| num(f, "mismatches"))
                .fold(0.0, |a, b| a + b);
            write!(out, "\t{failed}\t{oom}\t{under}\t{envs}\t{mismatches:.0}").unwrap();
        }
        println!("{out}");
    }
    if !short.is_empty() {
        println!(
            "\n{} cells hold a run that does not count: {}",
            short.len(),
            short.join("; ")
        );
    }
    if heaps.is_empty() {
        return;
    }
    println!("\nindex\truns\trows\tbytes_row\tcg_anon_bytes_row\theap_mb\tbuild_s\tlimit_mb\tfailed\toom\tenv");
    for (kind, m) in heaps {
        // A run the environment failed counts as its alone.
        let envs: Vec<&str> = m.get("env_failed").map_or(Vec::new(), |v| {
            v.iter()
                .filter(|x| x.1 == 1.0)
                .map(|x| x.0.as_str())
                .collect()
        });
        let count = |metric: &str| {
            m.get(metric).map_or(0, |v| {
                v.iter()
                    .filter(|x| x.1 == 1.0 && !envs.contains(&x.0.as_str()))
                    .count()
            })
        };
        let (failed, oom, env) = (count("failed"), count("oom"), envs.len());
        let mid = |metric: &str, scale: f64, digits: usize| -> String {
            let mut v: Vec<f64> = m
                .get(metric)
                .map_or(Vec::new(), |v| v.iter().map(|x| x.1).collect());
            match median(&mut v) {
                Some(x) => format!("{:.*}", digits, x / scale),
                None => "-".into(),
            }
        };
        println!(
            "{kind}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{failed}\t{oom}\t{env}",
            m.get("rows").map_or(0, Vec::len),
            mid("rows", 1.0, 0),
            mid("bytes_row", 1.0, 1),
            mid("cg_anon_bytes_row", 1.0, 1),
            mid("bytes", 1048576.0, 0),
            mid("build_s", 1.0, 1),
            mid("limit", 1048576.0, 0),
        );
    }
}

// ------------------------------------------------------------------- main

fn list<T>(s: &str, f: impl Fn(&str) -> T) -> Vec<T> {
    s.split(',').filter(|x| !x.is_empty()).map(f).collect()
}

/// For each cell of `cfg` the file lacks a line of under `cfg.run_id` --
/// a run whose process the kernel killed (`oom`) or that ended otherwise
/// -- a failed line, so that no cell asked for is missing:
/// `ycsb fill` with the arguments the run was given. With `env`
/// (`--env-failed 1`: the script's gate stopped the run, or the disk ran
/// short during it) each such cell is the environment's instead, and so is
/// one whose line says it failed: a server meeting a full disk fails its
/// cell, and that is no failure of the server's. A cell that measured
/// stays as it is.
fn fill(cfg: &Config, oom: bool, env: bool) {
    let text = std::fs::read_to_string(&cfg.out).unwrap_or_default();
    let (have, failed) = written(&text, cfg);
    let modes: Vec<Mode> = cfg
        .modes
        .iter()
        .copied()
        .filter(|m| *m != Mode::Snapshot)
        .collect();
    let c_mode = match modes.contains(&Mode::Buffered) {
        true => Some(Mode::Buffered),
        false => modes.first().copied(),
    };
    let mut filled = 0;
    for name in &cfg.systems {
        for &mode in &modes {
            for w in WORKLOADS.iter().filter(|w| cfg.workloads.contains(&w.name)) {
                if w.name == 'C' && c_mode != Some(mode) {
                    continue;
                }
                for &threads in &cfg.threads {
                    let cell = format!("{name}:{}:{}:{threads}", mode.name(), w.name);
                    if have.contains(&cell) && !(env && failed.contains(&cell)) {
                        continue;
                    }
                    write_failed(cfg, &cell, oom, env);
                    filled += 1;
                }
            }
        }
    }
    eprintln!(
        "{filled} cells of run {} written as {}",
        cfg.run_id,
        match (env, oom) {
            (true, _) => "the environment's failures",
            (false, true) => "failed, the process killed for its memory",
            (false, false) => "failed, the process ended before them",
        }
    );
}

/// The cells of `cfg.run_id` in its family a results file holds a line
/// of -- one that measured (`ops_s`) or one that says it failed -- and,
/// of those, the ones whose last `failed` is 1.
fn written(
    text: &str,
    cfg: &Config,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut have = std::collections::HashSet::new();
    let mut failed = std::collections::HashSet::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 8 || f[0] != cfg.run_id || f[4] != cfg.family {
            continue;
        }
        match f[6] {
            "ops_s" | "env_failed" => {
                have.insert(f[5].to_string());
            }
            "failed" => {
                have.insert(f[5].to_string());
                match f[7] == "1" {
                    true => failed.insert(f[5].to_string()),
                    false => failed.remove(f[5]),
                };
            }
            _ => {}
        }
    }
    (have, failed)
}

/// The `/_metrics` page of the server at `addr`, read on a connection of
/// its own.
fn metrics_page(addr: &str) -> Option<String> {
    let mut c = http::Http::try_connect(addr, Duration::from_secs(30)).ok()?;
    let (status, body) = c.try_request("GET", "/_metrics", "text/plain", b"").ok()?;
    (status == 200).then(|| String::from_utf8_lossy(body).into_owned())
}

/// The heap a row of each index kind holds, from the server's own
/// `memory_bytes` (`fennec_memory_bytes`): `server-docker` under
/// `--memory` over `--records` rows of a collection made for it -- a token
/// of 16 letters for `@hash`, an integer for `@sorted`, eight words of a
/// 10 000-word vocabulary drawn zipfian for `@text`, a `vector<dim>` for
/// `@hnsw(cosine)` -- each index made, measured once the server says no
/// vector waits to be linked, and dropped before the next: `ycsb heap`.
/// A line a kind, `server-docker:heap:<kind>`, failed where the server
/// was killed, and started again for the next.
///
/// Beside the engine's own accounting (`bytes_row`, which counts what each
/// index's tables hold: `@sorted` came out at exactly 20.0 bytes a row),
/// what the kernel charged: the container's anonymous memory
/// (`memory.stat`'s `anon`, one look before the index and one after it is
/// linked) changed by `cg_anon_bytes`, `cg_anon_bytes_row` a row -- the
/// allocator's own and every buffer the build kept with it. The server is
/// started again over its file before each kind, so that the kind before
/// it, dropped with its field, leaves no freed memory for this one's build
/// to reuse: its anonymous memory starts from what an open holds. Run with
/// no `--memory` (`run-larger-than-memory.sh`): `@hnsw` over millions of
/// vectors is heap that has to fit, and under the cells' limit it was
/// killed, which says nothing of how much it takes.
fn heap(cfg: &Config, dim: usize, vectors: u64) {
    let dir = std::env::temp_dir().join(format!("fennec-ycsb-heap-{}", std::process::id()));
    let mut srv = Server {
        dir: dir.clone(),
        docker: true,
        port: http::free_port(),
        proc: None,
        image: cfg.image.clone(),
        auto_compact: cfg.auto_compact,
        name: cfg.docker_name.clone(),
        memory: cfg.memory.clone(),
        watch: None,
    };
    srv.stop();
    remove_volume(&srv.volume());
    srv.start("250");
    let addr = srv.addr();
    let mut c = http::Http::connect(&addr);
    c.query(
        &format!("create collection kinds (k text, at int, body text, v vector<{dim}>)"),
        "",
    );
    let mut rng = Rng::new(SEED ^ 0x4845_4150);
    let mut words = Zipfian::new(10_000);
    let rows = cfg.records.max(vectors);
    let t = Instant::now();
    let mut body = String::new();
    let mut key = 0;
    while key < rows {
        let end = (key + LOAD_BATCH as u64).min(rows);
        body.clear();
        body.push('[');
        for k in key..end {
            if k > key {
                body.push(',');
            }
            let token: String = (0..16)
                .map(|_| (b'a' + rng.below(26) as u8) as char)
                .collect();
            let text: Vec<String> = (0..8)
                .map(|_| format!("w{}", words.next(&mut rng, 10_000)))
                .collect();
            write!(
                body,
                "{{\"k\":\"{token}\",\"at\":{},\"body\":\"{}\"",
                rng.below(1 << 40),
                text.join(" ")
            )
            .unwrap();
            if k < vectors {
                let v: Vec<f32> = (0..dim).map(|_| rng.f64() as f32 * 2.0 - 1.0).collect();
                write!(body, ",\"v\":{}", http::vector_json(&v)).unwrap();
            }
            body.push('}');
        }
        body.push(']');
        c.post("/kinds", "application/json", body.as_bytes());
        key = end;
    }
    drop(c);
    eprintln!(
        "heap: {rows} rows, {vectors} with a vector<{dim}>, loaded in {:.0} s",
        t.elapsed().as_secs_f64()
    );
    for (kind, field, index, count) in [
        ("hash", "k", "@hash", rows),
        ("sorted", "at", "@sorted", rows),
        ("text", "body", "@text", rows),
        ("hnsw", "v", "@hnsw(cosine)", vectors),
    ] {
        srv.start("250");
        let look = container_cgroup(&srv.container());
        let page = metrics_page(&addr).unwrap_or_default();
        let before = metric_sum(page.as_bytes(), "fennec_memory_bytes");
        let live = metric_sum(page.as_bytes(), "fennec_data_bytes");
        let t = Instant::now();
        let mut failed = attempt(|| {
            let mut c = http::Http::connect(&addr);
            c.query(&format!("create index on kinds ({field}) {index}"), "");
        });
        let mut after = before;
        while failed.is_none() {
            match metrics_page(&addr) {
                Some(p) if metric_sum(p.as_bytes(), "fennec_vectors_unlinked") == 0 => {
                    after = metric_sum(p.as_bytes(), "fennec_memory_bytes");
                    break;
                }
                Some(_) => std::thread::sleep(Duration::from_secs(1)),
                None => failed = Some("the server stopped answering".into()),
            }
        }
        let built = t.elapsed().as_secs_f64();
        let oom = failed.is_some() && srv.oom_killed();
        let held = after.saturating_sub(before);
        let anon = match (&failed, look, container_cgroup(&srv.container())) {
            (None, Some(a), Some(b)) => Some(b.anon as i64 - a.anon as i64),
            _ => None,
        };
        let mut values = vec![
            ("rows", count.to_string()),
            ("live", live.to_string()),
            // What the server held before: the kinds measured before this
            // one let go of with their fields.
            ("held_before", before.to_string()),
            ("failed", (failed.is_some() as u8).to_string()),
            ("oom", (oom as u8).to_string()),
        ];
        // The cgroup's own limit, where it has one.
        if let Some(l) = look.and_then(|c| c.limit) {
            values.push(("limit", l.to_string()));
        }
        match &failed {
            None => {
                values.push(("bytes", held.to_string()));
                values.push(("bytes_row", format!("{:.1}", held as f64 / count as f64)));
                values.push(("build_s", format!("{built:.1}")));
                if let Some(a) = anon {
                    values.push(("cg_anon_bytes", a.to_string()));
                    values.push((
                        "cg_anon_bytes_row",
                        format!("{:.1}", a as f64 / count as f64),
                    ));
                }
                eprintln!(
                    "heap: {kind} on {field}: {} MB, {:.1} bytes a row, built in {built:.1} s; the cgroup's anonymous memory {}",
                    held >> 20,
                    held as f64 / count as f64,
                    match anon {
                        Some(a) => format!("{:+} MB, {:.1} bytes a row", a >> 20, a as f64 / count as f64),
                        None => "not read".into(),
                    }
                );
            }
            Some(why) => {
                eprintln!(
                    "heap: {kind} on {field} failed{}: {why}",
                    if oom { ", killed for its memory" } else { "" }
                );
            }
        }
        append_values(
            &cfg.out,
            &cfg.family,
            &cfg.run_id,
            &format!("server-docker:heap:{kind}"),
            &values,
        );
        if failed.is_none() {
            let mut c = http::Http::connect(&addr);
            // A field's own index goes with the field (`drop index` is an
            // index over several fields'): its values stay in the
            // documents. The next kind starts the server again over the
            // file -- as the kill left it, where this one failed.
            c.query(&format!("alter collection kinds drop field {field}"), "");
        }
    }
    drop(srv);
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("report") => {
            report(Path::new(args.get(1).expect("ycsb report <file.tsv>")));
            return;
        }
        Some("soak") => {
            soak::main(&args[1..]);
            return;
        }
        // `ycsb mark <file> <family> <run> <cell> <metric> <value>`: one
        // line, with the run's provenance -- what a script writes of its
        // own, an environment's failure among them.
        Some("mark") => {
            let [file, family, run, cell, metric, value] = &args[1..] else {
                panic!("ycsb mark <file> <family> <run> <cell> <metric> <value>");
            };
            append_values(
                Path::new(file),
                family,
                run,
                cell,
                &[(metric.as_str(), value.clone())],
            );
            return;
        }
        _ => {}
    }
    let (sub, args) = match args.first().map(String::as_str) {
        Some(s @ ("fill" | "heap")) => (Some(s.to_string()), args[1..].to_vec()),
        _ => (None, args),
    };
    let (mut oom, mut env, mut dim, mut vectors) = (false, false, 32usize, None);
    let mut cfg = Config {
        records: 1_000_000,
        warmup: Duration::from_secs(5),
        run: Duration::from_secs(30),
        threads: vec![1, 4, 16],
        workloads: "ABCDEF".chars().collect(),
        // `snapshot` is Redis's alone; the others pass it over.
        modes: vec![Mode::Durable, Mode::Buffered, Mode::Snapshot],
        systems: list("fennec,sqlite,server,pg,mongo,redis", str::to_string),
        out: PathBuf::from("ycsb.tsv"),
        runs: 1,
        probe_base: None,
        gap: Duration::from_secs(10),
        turn_gap: Duration::from_secs(180),
        image: "fennecdb-ycsb".into(),
        run_id: String::new(),
        verify: None,
        auto_compact: true,
        rate: 0.0,
        family: "ycsb".into(),
        docker_name: "fennecycsb".into(),
        memory: None,
        verify_max: None,
        gate: None,
    };
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--no-auto-compact" {
            cfg.auto_compact = false;
            i += 1;
            continue;
        }
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--records" => cfg.records = v.parse().unwrap(),
            "--seconds" => cfg.run = Duration::from_secs(v.parse().unwrap()),
            "--warmup" => cfg.warmup = Duration::from_secs(v.parse().unwrap()),
            "--threads" => cfg.threads = list(&v, |x| x.parse().unwrap()),
            "--workloads" => cfg.workloads = v.chars().filter(|c| *c != ',').collect(),
            "--modes" => {
                cfg.modes = list(&v, |x| match x {
                    "durable" => Mode::Durable,
                    "buffered" => Mode::Buffered,
                    "snapshot" => Mode::Snapshot,
                    _ => panic!("--modes durable,buffered,snapshot"),
                })
            }
            "--systems" => cfg.systems = list(&v, str::to_string),
            "--out" => cfg.out = PathBuf::from(&v),
            "--runs" => cfg.runs = v.parse().unwrap(),
            "--probe-base" => cfg.probe_base = Some(v.parse().unwrap()),
            "--gap" => cfg.gap = Duration::from_secs(v.parse().unwrap()),
            "--turn-gap" => cfg.turn_gap = Duration::from_secs(v.parse().unwrap()),
            "--image" => cfg.image = v,
            "--run-id" => cfg.run_id = v,
            "--verify" => cfg.verify = Some(PathBuf::from(&v)),
            "--rate" => cfg.rate = v.parse().unwrap(),
            "--family" => cfg.family = v,
            "--docker-name" => cfg.docker_name = v,
            "--memory" => {
                assert!(bytes_of(&v).is_some(), "--memory {v}: a size, 512m or 2g");
                cfg.memory = Some(v)
            }
            "--verify-max" => cfg.verify_max = Some(v.parse::<u64>().unwrap() << 20),
            // `fill`: the run's process was killed for its memory, or the
            // environment failed it.
            "--oom" => oom = v == "1",
            "--env-failed" => env = v == "1",
            "--gate" => cfg.gate = Some(PathBuf::from(&v)),
            // `heap`: the vectors' dimension, and how many rows hold one.
            "--dim" => dim = v.parse().unwrap(),
            "--vectors" => vectors = Some(v.parse().unwrap()),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    match sub.as_deref() {
        Some("fill") => return fill(&cfg, oom, env),
        Some("heap") => return heap(&cfg, dim, vectors.unwrap_or(cfg.records)),
        _ => {}
    }
    let base = cfg.probe_base.unwrap_or_else(|| {
        // The idle speed: the best of a few probes a few seconds apart.
        let mut best = f64::MAX;
        for _ in 0..5 {
            best = best.min(probe());
            std::thread::sleep(Duration::from_secs(2));
        }
        best
    });
    eprintln!(
        "{} records, {}s + {}s warm-up a cell, threads {:?}, probe at idle {base:.1} ms",
        cfg.records,
        cfg.run.as_secs(),
        cfg.warmup.as_secs(),
        cfg.threads
    );
    let first_id = cfg.run_id.clone();
    for run in 1..=cfg.runs {
        if cfg.runs > 1 || first_id.is_empty() {
            cfg.run_id = format!("{}{run}", first_id);
        }
        for (n, name) in cfg.systems.clone().iter().enumerate() {
            if n > 0 || run > 1 {
                eprintln!("idle {} s before {name}", cfg.turn_gap.as_secs());
                std::thread::sleep(cfg.turn_gap);
            }
            run_system(name, &cfg, base, run);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(key: u64, s: u64, e: u64, fields: [u64; FIELDS]) -> Entry {
        Entry::read(key, s, e, vec![(key, fields)])
    }

    fn update(key: u64, field: u8, value: u64, s: u64, e: u64) -> Entry {
        Entry::Write {
            key,
            field,
            value,
            s,
            e,
            affected: 1,
        }
    }

    #[test]
    fn a_read_sees_the_last_write_or_one_beside_it() {
        let base = base_digests(7);
        let mut new = base;
        new[3] = 42;
        let w = || update(7, 3, 42, 10, 20);
        // After the write ended: its value, never the one before it.
        let ok = check(&[vec![w(), read(7, 30, 40, new)]], &mut Model::default());
        assert_eq!(ok.mismatches, 0);
        let stale = check(&[vec![w(), read(7, 30, 40, base)]], &mut Model::default());
        assert_eq!(stale.mismatches, 1);
        // Overlapping it, either.
        for got in [base, new] {
            let v = check(
                &[vec![w()], vec![read(7, 15, 25, got)]],
                &mut Model::default(),
            );
            assert_eq!(v.mismatches, 0);
        }
        // A write followed by another that ended before the read is gone.
        let v = check(
            &[vec![w(), update(7, 3, 43, 21, 25), read(7, 30, 40, new)]],
            &mut Model::default(),
        );
        assert_eq!(v.mismatches, 1);
        // Another field's or another record's value is no value of this one.
        let mut moved = base;
        moved[4] = 42;
        let v = check(&[vec![w(), read(7, 30, 40, moved)]], &mut Model::default());
        assert_eq!(v.mismatches, 1);
        let v = check(
            &[vec![Entry::read(7, 1, 2, vec![(8, base_digests(8))])]],
            &mut Model::default(),
        );
        assert_eq!(v.mismatches, 1);
    }

    #[test]
    fn the_model_carries_a_cell_into_the_next() {
        let mut model = Model::default();
        check(&[vec![update(7, 3, 42, 10, 20)]], &mut model);
        let mut new = base_digests(7);
        new[3] = 42;
        assert_eq!(check(&[vec![read(7, 1, 2, new)]], &mut model).mismatches, 0);
        assert_eq!(
            check(&[vec![read(7, 1, 2, base_digests(7))]], &mut model).mismatches,
            1
        );
    }

    #[test]
    fn a_scan_answers_every_key_it_could_see_in_order() {
        let row = |k: u64| (k, combine(&base_digests(k)));
        let scan = |rows: Vec<(u64, u64)>| Entry::Scan {
            key: 5,
            len: 3,
            seen: 100,
            next: 101,
            rows,
        };
        let mut m = Model::default();
        assert_eq!(
            check(&[vec![scan(vec![row(5), row(6), row(7)])]], &mut m).mismatches,
            0
        );
        assert_eq!(
            check(&[vec![scan(vec![row(5), row(7), row(8)])]], &mut m).mismatches,
            1
        );
        assert_eq!(
            check(&[vec![scan(vec![row(5), row(6)])]], &mut m).mismatches,
            1
        );
        assert_eq!(
            check(&[vec![scan(vec![row(5), row(6), (7, 1)])]], &mut m).mismatches,
            1
        );
    }

    #[test]
    fn the_servers_rows_are_read_as_written() {
        let v = record_values(3);
        let mut body = String::from("[{\"id\":3");
        for (i, f) in v.iter().enumerate() {
            body.push_str(&format!(",\"field{i}\":\"{f}\""));
        }
        body.push_str("}]");
        assert_eq!(json_rows(body.as_bytes()), vec![(3, base_digests(3))]);
    }

    #[test]
    #[cfg(feature = "rivals")]
    fn redis_hashes_are_read_as_written_in_either_protocol() {
        use redis::Value::{Array, BulkString, Map};
        let v = record_values(3);
        let pairs: Vec<(redis::Value, redis::Value)> = v
            .iter()
            .enumerate()
            .rev()
            .map(|(i, f)| {
                (
                    BulkString(format!("field{i}").into_bytes()),
                    BulkString(f.clone().into_bytes()),
                )
            })
            .collect();
        let flat = Array(
            pairs
                .iter()
                .flat_map(|(k, v)| [k.clone(), v.clone()])
                .collect(),
        );
        assert_eq!(redis_digests(&flat), base_digests(3));
        assert_eq!(redis_digests(&Map(pairs)), base_digests(3));
        // No record: no field holds a value.
        assert_eq!(redis_digests(&Array(Vec::new())), [0; FIELDS]);
    }

    #[test]
    fn a_containers_memory_is_its_anonymous_and_shared_memory() {
        let stat = "anon 8740864\nfile 458752\nkernel 1\nshmem 4096\nfile_mapped 0\n";
        assert_eq!(memory_stat(stat), Some(8740864 + 4096));
        assert_eq!(memory_stat("file 1\n"), None);
    }

    #[test]
    fn a_cgroups_look_is_read_whole_or_not_at_all() {
        let look = "anon 100\nfile 200\npgmajfault 7\nshmem 0\ncurrent 400\noom_kill 1\n";
        let c = Cgroup::parse(look).unwrap();
        assert_eq!(
            c,
            Cgroup {
                pgmajfault: 7,
                current: 400,
                anon: 100,
                file: 200,
                oom_kill: 1,
                limit: None,
            }
        );
        // The limit is the cgroup's own `memory.max`, none where it has
        // none, and never `memory.events`' `max`, the times it was met.
        let held = format!("{look}limit 2147483648\nmax 12\n");
        assert_eq!(Cgroup::parse(&held).unwrap().limit, Some(2 << 30));
        let free = format!("{look}limit max\n");
        assert_eq!(Cgroup::parse(&free).unwrap().limit, None);
        // `file_mapped` is not `file`, and a look cut short is none.
        assert_eq!(Cgroup::parse("file_mapped 3\nanon 1\ncurrent 2\n"), None);
        assert_eq!(bytes_of("512m"), Some(512 << 20));
        assert_eq!(bytes_of("2g"), Some(2 << 30));
        assert_eq!(bytes_of("1048576"), Some(1 << 20));
        assert_eq!(bytes_of("lots"), None);
        let page = b"# TYPE fennec_data_bytes gauge\nfennec_data_bytes{collection=\"a\"} 10\nfennec_data_bytes{collection=\"b\"} 32\nfennec_data_bytes_x 5\n";
        assert_eq!(metric_sum(page, "fennec_data_bytes"), 42);
    }

    /// A client whose `stall`th read takes 200 ms, the rest nothing.
    struct Stalling(u64, u64);

    impl Client for Stalling {
        fn read(&mut self, _: u64) {
            self.0 += 1;
            if self.0 == self.1 {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        fn update(&mut self, _: u64, _: usize, _: &str) {}
        fn insert(&mut self, _: u64, _: &[String]) {}
        fn scan(&mut self, _: u64, _: u64) {}
        fn got(&mut self) -> Vec<(u64, [u64; FIELDS])> {
            Vec::new()
        }
    }

    struct Fake;

    impl System for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn load(&mut self, _: u64) {}
        fn set_mode(&mut self, _: Mode) {}
        fn client(&self) -> Box<dyn Client> {
            Box::new(Stalling(0, 100))
        }
    }

    fn config(rate: f64) -> Config {
        Config {
            records: 1_000,
            warmup: Duration::ZERO,
            run: Duration::from_secs(1),
            threads: vec![1],
            workloads: vec!['C'],
            modes: vec![Mode::Buffered],
            systems: Vec::new(),
            out: PathBuf::new(),
            runs: 1,
            probe_base: None,
            gap: Duration::ZERO,
            turn_gap: Duration::ZERO,
            image: String::new(),
            run_id: String::new(),
            verify: None,
            auto_compact: true,
            rate,
            family: "ycsb".into(),
            docker_name: String::new(),
            memory: None,
            verify_max: None,
            gate: None,
        }
    }

    #[test]
    fn open_loop_counts_the_wait_behind_a_stall() {
        let c = &WORKLOADS[2];
        // At 1 000 a second, the 200 operations due while one stalls each
        // wait for it: a fifth of the second's operations, so its p99 is
        // the stall's.
        let (open, _) = run_cell(&Fake, c, 1, 1_000, &config(1_000.0));
        let p99 = open.hist[&Op::Read].pct(0.99);
        assert!(p99 >= 100_000.0, "open loop p99 {p99} us");
        assert!(
            open.ops <= 1_100,
            "{} operations at 1 000 a second",
            open.ops
        );
        // Closed loop the stall is one operation's time among many.
        let (closed, _) = run_cell(&Fake, c, 1, 1_000, &config(0.0));
        let p99 = closed.hist[&Op::Read].pct(0.99);
        assert!(
            closed.ops > open.ops && p99 < 150_000.0,
            "closed loop p99 {p99} us"
        );
    }

    /// A results file of its own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(what: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("ycsb-{what}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        /// `(cell, metric, value)` of each line of `run`.
        fn lines(&self, run: &str) -> Vec<(String, String, String)> {
            std::fs::read_to_string(self.0.join("r.tsv"))
                .unwrap_or_default()
                .lines()
                .skip(1)
                .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
                .filter(|f| f[0] == run)
                .map(|f| (f[5].clone(), f[6].clone(), f[7].clone()))
                .collect()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn bounded(s: &Scratch) -> Config {
        let mut cfg = config(0.0);
        cfg.out = s.0.join("r.tsv");
        cfg.family = "ycsb_larger".into();
        cfg.run_id = "lm1".into();
        cfg.memory = Some("2g".into());
        cfg.systems = vec!["server-docker".into()];
        cfg.workloads = vec!['B', 'C'];
        cfg.modes = vec![Mode::Durable, Mode::Buffered];
        cfg.threads = vec![16];
        cfg
    }

    #[test]
    fn a_failed_cell_writes_no_measured_value() {
        let s = Scratch::new("failed");
        let cfg = bounded(&s);
        let after = After {
            live: Some(5 << 30),
            oom: true,
            verdict: None,
        };
        let cell = Cell::failed("the server stopped answering".into());
        write_cell(
            "server-docker",
            Mode::Buffered,
            &WORKLOADS[1],
            16,
            1_000,
            &cell,
            1.0,
            &cfg,
            1,
            &after,
        );
        let mut got: Vec<(String, String)> =
            s.lines("lm1").into_iter().map(|(_, m, v)| (m, v)).collect();
        got.sort();
        // Neither `ops_s 0` nor the live bytes: a median would take them.
        assert_eq!(
            got,
            [("failed".into(), "1".into()), ("oom".into(), "1".into())]
        );
    }

    #[test]
    fn fill_hands_the_environments_cells_to_it() {
        let s = Scratch::new("fill");
        let cfg = bounded(&s);
        // B measured; C failed as a full disk fails a server; neither of
        // the durable mode's cells (B) written.
        fn values<'a>(v: &[(&'a str, &str)]) -> Vec<(&'a str, String)> {
            v.iter().map(|(k, x)| (*k, x.to_string())).collect()
        }
        let b = "server-docker:buffered:B:16";
        let c = "server-docker:buffered:C:16";
        append_values(
            &cfg.out,
            &cfg.family,
            "lm1",
            b,
            &values(&[("ops_s", "900"), ("failed", "0")]),
        );
        append_values(&cfg.out, &cfg.family, "lm1", c, &values(&[("failed", "1")]));
        fill(&cfg, false, true);
        let env: Vec<String> = s
            .lines("lm1")
            .into_iter()
            .filter(|(_, m, v)| m == "env_failed" && v == "1")
            .map(|(cell, _, _)| cell)
            .collect();
        assert!(!env.contains(&b.to_string()), "{env:?}");
        assert!(env.contains(&c.to_string()), "{env:?}");
        assert!(env.contains(&"server-docker:durable:B:16".to_string()));
        // A process that ended is the system's: a cell written once stays.
        let s = Scratch::new("fill-ended");
        let cfg = bounded(&s);
        append_values(&cfg.out, &cfg.family, "lm1", c, &values(&[("failed", "1")]));
        fill(&cfg, true, false);
        let lines = s.lines("lm1");
        assert_eq!(lines.iter().filter(|l| l.0 == c).count(), 1);
        assert!(lines.contains(&(
            "server-docker:durable:B:16".into(),
            "oom".into(),
            "1".into()
        )));
    }

    #[test]
    fn a_run_counts_only_as_a_measurement_of_the_regime() {
        let run = |v: &[(&str, f64)]| {
            let v: Vec<(String, f64)> = v.iter().map(|(k, x)| (k.to_string(), *x)).collect();
            Standing::of(|c| v.iter().find(|(k, _)| k == c).map(|(_, x)| *x), true)
        };
        let gb = (1u64 << 30) as f64;
        let ok = [("failed", 0.0), ("live", 4.5 * gb), ("limit", 2.0 * gb)];
        assert_eq!(run(&ok), Standing::Counts);
        // 1.92 times the limit, as 4 000 000 records of 1 026 bytes are.
        let small = [("failed", 0.0), ("live", 3.84 * gb), ("limit", 2.0 * gb)];
        assert_eq!(run(&small), Standing::Small);
        // Neither said: not shown to be the regime.
        assert_eq!(run(&[("failed", 0.0)]), Standing::Small);
        assert_eq!(run(&[("failed", 1.0), ("oom", 1.0)]), Standing::Failed);
        // A full disk's server error is the environment's.
        assert_eq!(run(&[("failed", 1.0), ("env_failed", 1.0)]), Standing::Env);
        // Outside `--memory`'s files no limit is asked for.
        assert_eq!(
            Standing::of(|c| (c == "ops_s").then_some(1.0), false),
            Standing::Counts
        );
    }

    /// A system whose live bytes and cgroup's limit are what a test says.
    struct Held(Option<u64>, Option<u64>);

    impl System for Held {
        fn name(&self) -> &'static str {
            "held"
        }
        fn load(&mut self, _: u64) {}
        fn set_mode(&mut self, _: Mode) {}
        fn client(&self) -> Box<dyn Client> {
            Box::new(Stalling(0, 100))
        }
        fn cgroup(&self) -> Option<Cgroup> {
            Some(Cgroup {
                limit: self.1,
                ..Cgroup::default()
            })
        }
        fn live(&self) -> Option<u64> {
            self.0
        }
    }

    #[test]
    fn a_run_is_refused_under_twice_the_cgroups_limit() {
        let limit = 2u64 << 30;
        assert!(twice_the_limit(&Held(Some(4_822_200_000), Some(limit))).is_ok());
        let why = twice_the_limit(&Held(Some(4_104_000_000), Some(limit))).unwrap_err();
        // Both numbers named.
        assert!(
            why.contains("4104000000") && why.contains("2147483648"),
            "{why}"
        );
        assert!(twice_the_limit(&Held(Some(1 << 40), None)).is_err());
        assert!(twice_the_limit(&Held(None, Some(limit))).is_err());
    }

    #[test]
    fn the_gate_answers_each_step() {
        let s = Scratch::new("gate");
        let mut cfg = bounded(&s);
        cfg.gate = Some(s.0.clone());
        let dir = s.0.clone();
        // The script's part: an ok to the first question, a stop to the
        // second.
        let keeper = std::thread::spawn(move || {
            let mut answered = 0;
            while answered < 2 {
                for e in std::fs::read_dir(&dir).unwrap() {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    let Some(id) = name.strip_prefix("ask.") else {
                        continue;
                    };
                    let (ok, stop) = (dir.join(format!("ok.{id}")), dir.join(format!("stop.{id}")));
                    if ok.exists() || stop.exists() {
                        continue;
                    }
                    match answered {
                        0 => std::fs::write(ok, "").unwrap(),
                        _ => std::fs::write(stop, "the host has 10 KB free\n").unwrap(),
                    }
                    answered += 1;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        assert_eq!(ask_gate(&cfg, "server-docker", "load"), Ok(()));
        assert_eq!(
            ask_gate(&cfg, "server-docker", "cell"),
            Err("the host has 10 KB free".to_string())
        );
        keeper.join().unwrap();
        // No gate, no question.
        cfg.gate = None;
        assert_eq!(ask_gate(&cfg, "server-docker", "cell"), Ok(()));
    }
}
