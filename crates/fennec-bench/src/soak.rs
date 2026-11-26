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
//! replica. Both ask a token, the run's own, as a server reached past
//! loopback must (`Kit::start`), and a fault of the harness's own -- a
//! server refusing the run's token or its name, the replica not following
//! -- stops the run at once (`stop_run`), never carried for hours. The
//! load is open loop (`--rate`, one schedule the connections take their
//! operations from in turn, an operation's time from when it was due: a
//! stall counts the wait of every one queued behind it), or as fast as
//! answered for the peak (`--peak`).
//! Its mix, over a fixed set of connections:
//!
//!   36% read and 37% update of YCSB's records (A), its scrambled zipfian,
//!       and 4% read of them through the replica, sent after the latest
//!       write answered (`Fennec-After`) -- on connections and a schedule
//!       of their own (`--replica-threads`), so that the replica holding
//!       them holds them alone,
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
//! A server that dies on its own -- the kernel's OOM kill under its
//! `--memory`, an exit -- is found as Docker says it (`follow_events`) or
//! by its state at a minute's end or a kill, named on a line of its own,
//! timed by the burst of closes its exit made (`death_time`), the writes a
//! read then shows gone judged from that time, and started again; each
//! server's cgroup is read a second (`watch_memory`) and the open loop's
//! lag too (`seconds`), the minute's line taking their most. After each
//! restart, until the loop is back on its schedule and the replica holds
//! the change answered then (`catch_up`), a read through the replica held
//! past its wait is the restart's, and the lane is moved up.
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
    Hist, Rng, Scrambled, ALPINE, FIELDS, LOAD_BATCH, SEED,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::fs::File;
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
///
/// The rig measured `SYNC_OWN` on a GitHub runner with no compact running;
/// the soak kills on Docker Desktop's disk, every third kill in a compact,
/// and a write behind is passed over while an fsync holds the disk. So the
/// bound is a parameter (`--loss-ms`, this its default), and every read
/// after a kill that shows a write gone names how long before the kill it
/// was answered: the report prints the run's largest beside the bound, and
/// whether the run supports it.
const SYNC_INTERVAL_MS: u64 = 250;
const SYNC_OWN_MS: u64 = 200;

fn default_loss_ms(sync: &str) -> u64 {
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

/// The longest the run waits on one thing before it says which and ends:
/// a side job a kill waits for, a server's answer after its start, a docker
/// call the minute's loop makes. Under the ten minutes the script's guard
/// gives a soak that prints nothing (`ENV_SILENT`), so the soak names what
/// hung before the guard has to.
const WAIT_MAX: Duration = Duration::from_secs(540);

/// A shell command that prints 1 where the file it is handed is there and
/// was written in the last minute -- a compaction writing its side file
/// (`<file>.beside`) -- and 0 otherwise. A kill leaves the side file of a
/// compact it cut, and the server started again leaves it until its next
/// compact makes it anew: by its being there, the second dry run of
/// 2026-11-23 read the twelve minutes after its kill of minute 90 as a
/// compaction writing, none running, and judged no memory in them.
const WRITTEN_SINCE: &str = "sh -c '[ -e \"$1\" ] && [ $(( $(date +%s) - $(stat -c %Y \"$1\") )) -lt 60 ] && echo 1 || echo 0' sh";

/// The looks after a compaction of the replica's that its memory is not
/// judged on (`judge_run`): the allocator keeps the heap the compaction
/// let go of while the engine grows back into it, three or four minutes
/// on the third dry run of 2026-11-24.
const REPLICA_SETTLE: usize = 5;

/// How far behind its schedule the open loop may be and still be on it, in
/// seconds (`behind_s`): the report's "back on its schedule", and a kill
/// at which the loop was further behind is one whose operations wait out
/// the backlog, not the restart (`Shared::restart_behind`).
const ON_SCHEDULE_S: f64 = 1.0;

/// How often the replica's compaction thread is looked at
/// (`watch_replica`), and how long it must be at work -- or asleep again --
/// for a compaction of the replica's own to have begun or ended: its look
/// at the files every 5 s (fennec-http's `link`, `AUTO_COMPACT_EVERY`)
/// takes milliseconds, a compaction tens of seconds.
const R_WATCH_EVERY: u64 = 100_000_000;
const R_WATCH_SURE: u64 = 500_000_000;

/// The name of the thread a fennec-server compacts its files with on its
/// own (fennec-http's `link::keep_held`), which the watch looks for
/// (`watch_replica`); held to the source that names it by
/// `the_watch_reads_the_thread_fennec_http_names`. fennec-core's
/// `Compactor` names its thread the same -- the native library's, one a
/// file -- and waits between its looks on a condvar, a futex, where
/// fennec-http's sleeps: the watch tells them apart by that
/// (`CompactWatch`).
const R_COMPACT_THREAD: &str = "fennec-compact";

/// How soon the watch's container is started again: after it ended (the
/// replica started again, the thread not yet there), and after it found no
/// thread so named in the replica -- which no restart of the container
/// changes, so it is said once and looked for again slowly, not a
/// container every 5 s for the run's length.
const R_WATCH_AGAIN: Duration = Duration::from_secs(5);
const R_WATCH_ABSENT: Duration = Duration::from_secs(60);

/// How long the watch may be gone with a window of the replica's open
/// before the window is lost (`bound_r_window`): said, counted as lost
/// (`r_compacts_lost`), which no run passes the watch's criterion with,
/// and its operations counted as if no window had been. The watch's
/// container is started again 5 s after it ends (`R_WATCH_AGAIN`) and lists
/// the replica's threads at once, and a replica the run found stopped is
/// started at that minute's look and answers in seconds: gone a minute,
/// ten containers or more did not see the thread, and a minute's look while
/// it is down decides. Before, a window whose watch went in it stayed open to
/// the run's end -- every read through the replica in those hours the
/// compaction's, and the window's length the bound of their p99 -- and the
/// counter's count passed as the one open at the last look.
const R_WATCH_GONE: u64 = 60_000_000_000;

/// The longest a compaction of the replica's own can plausibly take, by
/// the replica's file at the latest look (`plausible_ms`): 500 s a GB --
/// three times the slowest any run measured, the third dry run of
/// 2026-11-24's replica compacting 2.06 GB in 344 s (167 s a GB) under
/// 512 MB, the loop behind and the primary compacting beside it, where the
/// fourth's took 21.5-25 s of the same size -- or four times the slowest
/// of the primary's compacts on the run, a GB of its file, where that is
/// more (the third dry run's replica's own took 262-344 s, the primary's
/// 114-322), and ten minutes at the least. A window
/// open past it is no compaction a server ends, but a thread that never
/// sleeps again: a FAIL of its own (`r_compacts_overlong`), its reads
/// never excused by it.
const R_COMPACT_MS_PER_GB: u64 = 500_000;
const R_COMPACT_PRIMARY_TIMES: u64 = 4;
const R_COMPACT_FLOOR_MS: u64 = 600_000;

/// The operations a worker holds for windows of the replica's not yet
/// settled (`Hists::held`) at the most, 32 bytes each (`Held`, its `Rec`
/// nested; a test holds the size): 4 MiB a worker. At
/// the soak's rates of the dry runs, 1 240-2 093 a second over 32
/// connections, it is 33-56 minutes of a window every operation of which a
/// worker owes, past the 20 minutes `plausible_ms` gives the largest file
/// the soak keeps (2.2 times its 1.1 GB live); the peak owes nothing, and
/// holds only its reads through the replica. Reached, the window is cut,
/// said, and its operations counted as if no window had been: before, a
/// window open for hours held 24 bytes an operation (round ten's layout),
/// some 280 MB an hour at 3 200 a second, rescanned whole at each. The
/// most any worker held is on each minute's line and the end line
/// (`r_held_max`), the run's distance from this.
const HELD_MAX: usize = 1 << 17;

/// The minutes of the peak's slowest stretch, which the soak's rate is a
/// fraction of (`slowest_stretch`): shorter than a compaction of the file
/// under the run's memory -- the third dry run of 2026-11-24's took 110 to
/// 344 s, the peak's own 252 -- so that the stretch can lie inside one, and
/// longer than a minute, whose noise alone never sets the rate.
const PEAK_STRETCH: usize = 3;

/// A step of the VM's clock against this host's, from one minute's look to
/// the next, past which the line marks it: a fifth of the `@ttl` slack's
/// second, well past the probe's own round trip of a few ms.
const CLOCK_STEP_MS: f64 = 200.0;

/// The share of the mix that reads through the replica: on its own
/// connections and its own schedule (`--replica-threads`), apart from the
/// pool every other operation shares.
const REPLICA_SHARE: f64 = 0.04;

/// The replica's `--slow-ms`: its slow log starts after a read's
/// `Fennec-After` wait (fennec-http's `after`), so it names what a read
/// through it spent past the primary's sync -- lock waits, faults -- the
/// part dry run 6's minute figures could not tell from the loop's queue.
const REPLICA_SLOW_MS: u64 = 100;

/// Both servers' `--max-connections`, past the default of 100: the
/// harness's own are the pool's 32, the replica lane's 128 -- each of
/// which reads from the primary from a kill until the replica is back --
/// the 64 subscriptions, the replica's follow, a backup's and the looks'
/// scrapes, some 235.
const MAX_CONNECTIONS: u64 = 320;

/// How long after a close the server may still be read answering before
/// that close is taken for no death (`Shared::closed`). An answer the
/// server wrote before it died can be read after another connection saw
/// it close -- each connection's bytes come through Docker's port
/// forwarding on a relay of its own, and the thread holding it waits for
/// a core among the harness's 250 -- but within milliseconds: the dry run
/// of 2026-11-26's three deaths had their first close 9-10 ms after the
/// last answer before it. A burst of closes the server answered past is no
/// death's -- a stream's end or a refused connect beside others -- and is
/// let go at the next close. Kept longer, it stands for a death that came
/// after it: a slop of a second took a close 0.9 s before a server that
/// then hung 30 s and was OOM-killed for its time, and excused writes
/// answered half a minute before it went. Too short a slop only moves a
/// death's time to the next burst, later: the judge stricter by an
/// answer's few ms, never more lenient (the writes a read shows gone are
/// excused from the death's time back, and `death_time` never takes one
/// before the last answer). 50 ms is five times the gap the dry run
/// measured, and a twentieth of the second.
const CLOSE_SLOP: u64 = 50_000_000;

/// The closes, within how long of the first of them, that are a death's
/// rather than one connection's (`Shared::closed`). A process's exit
/// closes every socket it holds in the kernel's one teardown, so a death
/// cuts the 64 streams, each blocked in its read, and the workers'
/// requests under way at once, and the harness reads them as fast as its
/// threads wake; a close that was no death's -- a stream the server ended,
/// a connection thread's panic, a refused connect -- is one connection's,
/// and the next close of a living server comes as another connection
/// happens to end, not within the span. One close timed a death that came
/// later where the server stopped answering between them: a stream ended
/// by the server, then a hang and an OOM kill, its writes excused from the
/// stream's end. Three within 20 ms, ten times the widest the dry runs of
/// 2026-11-26 through `run-soak.sh` measured: an OOM kill's three came
/// within 0.1 ms, an outside `docker kill`'s within 1.2, the planned
/// kill's within 2.1. A run whose death closes fewer -- no streams, an
/// idle pool -- is timed by Docker's own times, later, as before the
/// closes were read.
const BURST_CLOSES: u32 = 3;
const BURST_SPAN: u64 = 20_000_000;

/// How fast this process's clock (`Shared::ns`, macOS's `CLOCK_UPTIME_RAW`,
/// never slewed) and Docker's VM's, which follows the host's wall clock,
/// may part between two probes, in parts per million: NTP slews the wall
/// clock at 500 at the most. Measured on this Mac at -43 over three
/// minutes and +5.6 over two -- some 60 ms by the 90th minute of a run had
/// Docker's times been read through the wall clock at `t0`, which put a
/// planned kill's end, 20-200 ms after its send, before it.
const CLOCK_DRIFT_PPM: u64 = 500;

/// How long a side job that failed waits for a death to be found before it
/// counts its failure as its own (`cut_by`): the death is found from
/// Docker's `die` event at once, or from the container's state, which
/// Docker sets as it reaps the process -- milliseconds after its sockets
/// close, which is what the job saw.
const DEATH_WAIT: Duration = Duration::from_secs(5);

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
    /// The replica lane's connections: the reads through the replica on a
    /// schedule of their own (`REPLICA_SHARE` of the rate); 0 puts them in
    /// the pool every operation shares, as before the lanes.
    replica_threads: usize,
    subscriptions: usize,
    kill_every: u64,
    kill_in_compact_every: u64,
    compact_every: u64,
    backup_every: u64,
    backup_at: u64,
    ttl: u64,
    slack: u64,
    loss_ms: u64,
    restart_max: u64,
    replica_back_max: u64,
    lag_max: u64,
    catchup_max: u64,
    recall_min: f64,
    samples: u64,
    disk_floor: u64,
    replication_buffer: u64,
    out: PathBuf,
    run_id: String,
    image: String,
    tools: String,
    name: String,
    progress: Option<PathBuf>,
    kept: Option<PathBuf>,
    logs: Option<PathBuf>,
    load_before: f64,
}

const HELP: &str = "ycsb soak --sync 250|always (--rate <ops/s> [--label 50] | --peak)
    [--minutes 480] [--records 1000000] [--docs 20000] [--counters 1000] [--dim 16]
    [--memory 512m] [--threads 32] [--replica-threads 128] [--subscriptions 64] [--kill-every 30]
    [--kill-in-compact-every 90] [--compact-every 20] [--backup-every 120]
    [--backup-at 50] [--ttl 60] [--slack 1] [--loss-ms 650|0] [--restart-max 120]
    [--replica-back-max 60] [--lag-max 5] [--catchup-max 300] [--recall-min 0.925]
    [--samples 3] [--disk-floor 3g] [--replication-buffer 64] [--out soak.tsv] [--run-id <id>]
    [--image fennecdb-ycsb] [--tools p6-tools] [--name p6] [--progress <file>]
    [--kept <file>] [--logs <dir>] [--load-before <load>]
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
            // A read through the replica waits out the primary's next sync:
            // dry run 6's took 177-232 ms at the median. By Little's law the
            // lane holds the rate's 4% times that busy -- 24 at 3 000 a
            // second, 108 at the 9 000 of the lane's first dry run, a
            // 100 000-record file whose reads took 0.27 s -- and a replica's
            // compaction doubling it must find room, or the lane's figures
            // are its own queue at every second.
            replica_threads: 128,
            subscriptions: 64,
            kill_every: 30,
            kill_in_compact_every: 90,
            compact_every: 20,
            backup_every: 120,
            backup_at: 50,
            ttl: 60,
            // The server stamps `seen` and judges the expiry by its own
            // clock, so the VM's offset from this host cancels: only a
            // drift or a step of it within the ttl is left, which the
            // minute's clock probe marks.
            slack: 1,
            loss_ms: u64::MAX,
            restart_max: 120,
            replica_back_max: 60,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            samples: 3,
            disk_floor: 3 << 30,
            // fennec-server's own default (`replication::DEFAULT_BUFFER`),
            // passed so that the report knows the bound it holds memory by.
            replication_buffer: 64,
            out: PathBuf::from("soak.tsv"),
            run_id: String::new(),
            image: "fennecdb-ycsb".into(),
            tools: "p6-tools".into(),
            name: "p6".into(),
            progress: None,
            kept: None,
            logs: None,
            load_before: -1.0,
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
                "--replica-threads" => o.replica_threads = n() as usize,
                "--subscriptions" => o.subscriptions = n() as usize,
                "--kill-every" => o.kill_every = n(),
                "--kill-in-compact-every" => o.kill_in_compact_every = n(),
                "--compact-every" => o.compact_every = n(),
                "--backup-every" => o.backup_every = n(),
                "--backup-at" => o.backup_at = n(),
                "--ttl" => o.ttl = n(),
                "--slack" => o.slack = n(),
                "--loss-ms" => o.loss_ms = n(),
                "--restart-max" => o.restart_max = n(),
                "--replica-back-max" => o.replica_back_max = n(),
                "--lag-max" => o.lag_max = n(),
                "--catchup-max" => o.catchup_max = n(),
                "--recall-min" => o.recall_min = v.parse().expect("--recall-min <0..1>"),
                "--samples" => o.samples = n(),
                "--disk-floor" => {
                    o.disk_floor = super::bytes_of(&v).unwrap_or_else(|| panic!("--disk-floor {v}"))
                }
                "--replication-buffer" => o.replication_buffer = n(),
                "--out" => o.out = PathBuf::from(&v),
                "--run-id" => o.run_id = v.clone(),
                "--image" => o.image = v.clone(),
                "--tools" => o.tools = v.clone(),
                "--name" => o.name = v.clone(),
                "--progress" => o.progress = Some(PathBuf::from(&v)),
                "--kept" => o.kept = Some(PathBuf::from(&v)),
                "--logs" => o.logs = Some(PathBuf::from(&v)),
                "--load-before" => o.load_before = v.parse().expect("--load-before <load>"),
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
        if o.loss_ms == u64::MAX {
            o.loss_ms = default_loss_ms(&o.sync);
        }
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

/// The run's progress, on stderr and in `--progress`'s file: the script's
/// guard stops a soak whose file gained no line in ten minutes, so every
/// phase that runs long says something every few minutes, and a wait that
/// could outlast the guard is bounded and named here first (`WAIT_MAX`).
static PROGRESS: Mutex<Option<File>> = Mutex::new(None);

fn say(line: String) {
    eprintln!("soak: {line}");
    let mut p = PROGRESS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = p.as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

/// `docker <args>`, ended after `within`: whether it succeeded, and what it
/// printed -- or that it did not end, naming the call. Every docker call
/// the run makes goes through here: one that hangs (a daemon that stopped
/// answering, a container that will not die) would stop every line after
/// it, and the script's guard would find the soak silent with no word of
/// why. A container the call named is removed once it is given up on.
fn docker_within(within: Duration, args: &[&str]) -> (bool, String) {
    docker_env(within, &[], args)
}

/// [`docker_within`] with `env` in the docker CLI's environment: what a
/// `-e NAME` with no value hands the container, a run's secrets so --
/// never an argument, which `ps` shows on this host and in the VM, and a
/// call that does not end prints.
fn docker_env(within: Duration, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let spawned = Command::new("docker")
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut v);
            }
            v
        })
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let until = Instant::now() + within;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut text = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    text.push_str(&String::from_utf8_lossy(&err.join().unwrap_or_default()));
    match status {
        Some(st) => (st.success(), text),
        None => {
            if let Some(i) = args.iter().position(|a| *a == "--name") {
                if let Some(name) = args.get(i + 1) {
                    let _ = docker_within(Duration::from_secs(60), &["rm", "-f", name]);
                }
            }
            (
                false,
                format!(
                    "docker {} did not end within {} s, and was stopped. {text}",
                    args.join(" "),
                    within.as_secs()
                ),
            )
        }
    }
}

/// [`docker_within`] two minutes: every quick call's bound.
fn docker(args: &[&str]) -> (bool, String) {
    docker_within(Duration::from_secs(120), args)
}

/// The containers, volumes and network a run makes, every one named after
/// `--name` -- which the script makes the run's own, so that a run never
/// removes another's by a shared name -- and what the side thread needs of
/// them. [`Bench`] removes them when the run ends.
#[derive(Clone)]
struct Kit {
    name: String,
    image: String,
    tools: String,
    memory: String,
    sync: String,
    /// The servers' `--replication-token`, and below their `--http-token`:
    /// the run's own (`secret`), handed to each container in its
    /// environment (`secrets`).
    token: String,
    http_token: String,
    /// The servers' `--replication-buffer`, in MiB: what the report holds
    /// the primary's memory beside (`feed_held`).
    replication_buffer: u64,
    port: u16,
    replica_port: u16,
    /// Set once a verify failed or a mismatch was counted: the volumes
    /// outlive the run, for whoever reads why.
    keep: Arc<AtomicBool>,
    kept: Option<PathBuf>,
    /// Where the servers' logs are written as their containers go
    /// (`--logs`): the second dry run of 2026-11-23 counted 40 reads
    /// through the replica answered 504, and nothing of either server was
    /// left to say why.
    logs: Option<PathBuf>,
}

impl Kit {
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
    fn containers(&self) -> Vec<String> {
        [
            "server",
            "replica",
            "server-look",
            "replica-look",
            "verify",
            "backup",
            "restored",
            "sampler",
            "replica-watch",
            "server-mem",
            "replica-mem",
        ]
        .iter()
        .map(|c| format!("{}-{c}", self.name))
        .collect()
    }
    fn volumes(&self) -> Vec<String> {
        ["data", "replica-data", "backup-data"]
            .iter()
            .map(|v| self.volume(v))
            .collect()
    }

    /// Keeps the run's volumes past its end, saying why and where -- in
    /// `--kept`'s file too, whose volumes the script's cleanup leaves.
    fn keep_data(&self, why: &str) {
        if self.keep.swap(true, Ordering::SeqCst) {
            return;
        }
        let vols = self.volumes();
        say(format!(
            "KEPT the volumes {} past the run ({why}): `docker run --rm --pull never -v {}:/data {ALPINE} ls -l /data` lists the primary's, `docker volume rm {}` removes them",
            vols.join(", "),
            vols[0],
            vols.join(" ")
        ));
        if let Some(p) = &self.kept {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                for v in &vols {
                    let _ = writeln!(f, "{v}");
                }
            }
        }
    }

    fn clean(&self) {
        self.keep_logs();
        for c in self.containers() {
            docker(&["rm", "-f", &c]);
        }
        if !self.keep.load(Ordering::SeqCst) {
            for v in self.volumes() {
                docker(&["volume", "rm", "-f", &v]);
            }
        }
        docker(&["network", "rm", &self.net()]);
    }

    /// Each server's log, every life of it (`docker start` keeps the
    /// container's), its lines stamped by Docker, into `--logs` before the
    /// container is removed: `<name>-server.log`, `<name>-replica.log`.
    fn keep_logs(&self) {
        use std::process::{Command, Stdio};
        let Some(dir) = &self.logs else {
            return;
        };
        for c in [self.server(), self.replica()] {
            // None yet: the clean before a run starts.
            if !docker(&["container", "inspect", "-f", "{{.Id}}", &c]).0 {
                continue;
            }
            let path = dir.join(format!("{c}.log"));
            let Ok(f) = std::fs::File::create(&path) else {
                continue;
            };
            let Ok(e) = f.try_clone() else {
                continue;
            };
            let Ok(mut child) = Command::new("docker")
                .args(["logs", "--timestamps", &c])
                .stdin(Stdio::null())
                .stdout(f)
                .stderr(e)
                .spawn()
            else {
                continue;
            };
            let until = Instant::now() + Duration::from_secs(120);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
            say(format!("{c}'s log kept: {}", path.display()));
        }
    }

    /// The run's secrets as the containers read them (fennec-server's
    /// `FENNEC_HTTP_TOKEN` and `FENNEC_REPLICATION_TOKEN`, `fennec
    /// backup`'s the latter), for [`docker_env`].
    fn secrets(&self) -> [(&'static str, &str); 2] {
        [
            ("FENNEC_HTTP_TOKEN", &self.http_token),
            ("FENNEC_REPLICATION_TOKEN", &self.token),
        ]
    }

    /// `fennec-server` in a container on the run's network, its port on
    /// this machine's loopback, held to `--memory` with no swap past it.
    /// It asks a token, as a server reached past loopback must: with none
    /// it answers only a `Host` of this machine's, and the replica's follow
    /// and `fennec backup`, which reach the primary by its container's
    /// name, were refused 403 (the dry run of 2026-11-22). Both tokens go
    /// in its environment, never its arguments.
    fn start(&self, container: &str, port: u16, volume: &str, args: &[&str]) {
        let buffer = self.replication_buffer.to_string();
        let most = MAX_CONNECTIONS.to_string();
        let (ok, out) = docker_env(
            Duration::from_secs(120),
            &self.secrets(),
            &[
                &[
                    "run",
                    "-d",
                    "--pull",
                    "never",
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
                    "-e",
                    "FENNEC_HTTP_TOKEN",
                    "-e",
                    "FENNEC_REPLICATION_TOKEN",
                    &self.image,
                    "--http",
                    "0.0.0.0:8080",
                    "--replication-buffer",
                    &buffer,
                    "--max-connections",
                    &most,
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
        let slow = REPLICA_SLOW_MS.to_string();
        self.start(
            &self.replica(),
            self.replica_port,
            &self.volume("replica-data"),
            &[
                "--file",
                "/data/replica.fennec",
                "--replica-of",
                &primary,
                "--slow-ms",
                &slow,
            ],
        );
    }

    /// `script` in a container holding the tools' volume
    /// (`ycsb/linux-tools.sh`) and `volume` at `/data`, on the run's
    /// network, for `within` at the most: whether it exited 0, and what it
    /// printed.
    fn fennec(
        &self,
        container: &str,
        volume: &str,
        script: &str,
        within: Duration,
    ) -> (bool, String) {
        self.fennec_env(container, volume, script, within, &[])
    }

    /// [`Kit::fennec`] with `env` in the container's environment, by name
    /// (`-e NAME`), its values never in an argument.
    fn fennec_env(
        &self,
        container: &str,
        volume: &str,
        script: &str,
        within: Duration,
        env: &[(&str, &str)],
    ) -> (bool, String) {
        let mut args = vec![
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            container,
            "--network",
        ];
        let net = self.net();
        let tools = format!("{}:/tools:ro", self.tools);
        let data = format!("{volume}:/data");
        args.extend([net.as_str(), "-v", tools.as_str(), "-v", data.as_str()]);
        for &(name, _) in env {
            args.extend(["-e", name]);
        }
        args.extend([ALPINE, "sh", "-c", script]);
        docker_env(within, env, &args)
    }

    /// A server's process as a shell in its process namespace sees it: its
    /// cgroup, anonymous memory, threads, descriptors and major faults, its
    /// database file's length alone, whether a compact is writing its image
    /// beside the file (`<file>.beside`, the side file `compact` writes
    /// into, there from its start to its rename), the CPU its cgroup used,
    /// the VM's CPUs as `/proc/stat` counts them, and the free bytes of the
    /// disk under its volume -- the Docker VM's.
    fn look(&self, container: &str, file: &str) -> Option<Look> {
        let script = format!(
            "cd /proc/1/root/sys/fs/cgroup && cat memory.stat && \
             echo current $(cat memory.current) && grep oom_kill memory.events; \
             echo cpu_usec $(awk '/^usage_usec/ {{print $2}}' cpu.stat); \
             grep -E '^(Threads|RssAnon):' /proc/1/status; \
             echo fds $(ls /proc/1/fd | wc -l); \
             echo majflt $(cut -d' ' -f12 /proc/1/stat); \
             echo filesize $(stat -c %s /proc/1/root/data/{file}); \
             echo compacting $({WRITTEN_SINCE} /proc/1/root/data/{file}.beside); \
             head -1 /proc/stat; \
             echo vmfree $(df -kP /proc/1/root/data | awk 'NR == 2 {{print $4}}')"
        );
        let (ok, out) = docker_within(
            Duration::from_secs(60),
            &[
                "run",
                "--rm",
                "--pull",
                "never",
                "--name",
                &format!("{container}-look"),
                &format!("--pid=container:{container}"),
                // Reading another user's `/proc/<pid>/root` is a ptrace's right.
                "--cap-add",
                "SYS_PTRACE",
                ALPINE,
                "sh",
                "-c",
                &script,
            ],
        );
        if !ok {
            return None;
        }
        Look::parse(&out)
    }

    /// A container's state as Docker has it, with how its process ended
    /// (`Exit`); `None` where `docker inspect` did not answer.
    fn exit(&self, container: &str) -> Option<Exit> {
        let (ok, out) = docker(&[
            "inspect",
            "-f",
            "{{.State.Running}} {{.State.OOMKilled}} {{.State.ExitCode}} {{.State.FinishedAt}}",
            container,
        ]);
        ok.then(|| Exit::parse(&out)).flatten()
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

/// The run's [`Kit`], cleaned when the run ends -- a panic's too.
struct Bench(Kit);

impl std::ops::Deref for Bench {
    type Target = Kit;
    fn deref(&self) -> &Kit {
        &self.0
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.0.clean();
    }
}

/// One look at a server's process.
#[derive(Default, Clone, Copy, Debug)]
struct Look {
    cgroup: Cgroup,
    rss_anon: u64,
    threads: u64,
    fds: u64,
    majflt: u64,
    file: u64,
    vm_free: Option<u64>,
    /// A compact was writing its image beside the file at the look.
    compacting: bool,
    /// The CPU the server's cgroup has used, in µs.
    cpu_usec: Option<u64>,
    /// The VM's CPUs' busy time since it started, in ticks.
    vm_busy: Option<u64>,
}

impl Look {
    fn parse(out: &str) -> Option<Look> {
        let num = |key: &str| -> Option<u64> {
            out.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        };
        // `cpu  user nice system idle iowait irq softirq steal ...`: idle
        // and iowait are the CPUs doing nothing.
        let ticks: Option<Vec<u64>> = out.lines().find_map(|l| l.strip_prefix("cpu ")).map(|r| {
            r.split_whitespace()
                .take(8)
                .filter_map(|x| x.parse().ok())
                .collect()
        });
        let vm_busy = match ticks {
            Some(t) if t.len() == 8 => Some(t.iter().sum::<u64>() - t[3] - t[4]),
            _ => None,
        };
        Some(Look {
            cgroup: Cgroup::parse(out).unwrap_or_default(),
            rss_anon: num("RssAnon:").map(|kb| kb << 10)?,
            threads: num("Threads:")?,
            fds: num("fds ")?,
            majflt: num("majflt ")?,
            file: num("filesize ")?,
            vm_free: num("vmfree ").map(|kb| kb << 10),
            compacting: num("compacting ").is_some_and(|n| n > 0),
            cpu_usec: num("cpu_usec "),
            vm_busy,
        })
    }
}

/// How a container's process ended, as `docker inspect` has it: whether it
/// runs, whether the kernel killed it for its memory (`OOMKilled`), its
/// exit code, and when it was reaped (`FinishedAt`), unix ns on the clock
/// of Docker's VM -- `None` where it never ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Exit {
    running: bool,
    oom: bool,
    code: Option<i64>,
    finished: Option<u64>,
}

impl Exit {
    /// `{{.State.Running}} {{.State.OOMKilled}} {{.State.ExitCode}}
    /// {{.State.FinishedAt}}`'s line.
    fn parse(out: &str) -> Option<Exit> {
        let mut w = out.split_whitespace();
        Some(Exit {
            running: w.next()? == "true",
            oom: w.next()? == "true",
            code: w.next().and_then(|c| c.parse().ok()),
            finished: w.next().and_then(unix_ns),
        })
    }

    /// How it ended, in words: the kernel's OOM kill under the run's
    /// `memory`, a signal (an exit code past 128), or its own exit.
    fn said(&self, memory: &str) -> String {
        match (self.oom, self.code) {
            (true, Some(c)) => format!("OOM-killed under --memory {memory} (exit {c})"),
            (true, None) => format!("OOM-killed under --memory {memory}"),
            (false, Some(c)) if c > 128 => format!(
                "killed by signal {} (exit {c}), not by the kernel for its memory",
                c - 128
            ),
            (false, Some(c)) => format!("exited with code {c}"),
            (false, None) => "gone, its exit code unread".into(),
        }
    }
}

/// An RFC 3339 time as Docker writes it (`2026-11-26T00:56:47.123456789Z`)
/// in unix ns; `None` for one before 1970 -- Docker's
/// `0001-01-01T00:00:00Z` for a container that never ended -- or another
/// zone than UTC.
fn unix_ns(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || frac.len() > 9 {
        return None;
    }
    let ns: i64 = match frac {
        "" => 0,
        f => f.parse::<i64>().ok()? * 10i64.pow(9 - f.len() as u32),
    };
    // Days from 1970-01-01 to the civil date (H. Hinnant's
    // `days_from_civil`).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + da - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let secs = days * 86_400 + h * 3_600 + mi * 60 + se;
    u64::try_from(secs)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(ns as u64)
}

/// Unix ns as the time of day in UTC, to the ms: what a death's line names
/// beside Docker's own times and the servers' logs.
fn utc(ns: u64) -> String {
    let ms = ns / 1_000_000 % 86_400_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1_000 % 60,
        ms % 1_000
    )
}

/// Where a death's time was read from (`death_time`), the line's
/// `death_source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The burst of closes its exit made after its last answer
    /// (`BURST_CLOSES`), on this process's clock -- the one every write's
    /// answer is timed on.
    Close = 1,
    /// Docker's `die` event, on the VM's clock, by the probe kept.
    Event = 2,
    /// Docker's `State.FinishedAt`, likewise.
    Finished = 3,
    /// None of those: the minute's look before it, as before.
    Look = 4,
}

impl Source {
    fn said(self) -> &'static str {
        match self {
            Source::Close => "the burst of closes its exit made",
            Source::Event => "Docker's die event",
            Source::Finished => "Docker's FinishedAt",
            Source::Look => "the minute's look before it, nothing closing or Docker's times read",
        }
    }
}

/// The time a death of the primary's writes are judged from, in ns from the
/// run's start, where it was read from, and the last answer read before
/// that time.
///
/// The burst of closes the server's exit made after its last answer,
/// where there is one: the process's sockets close in its exit, before
/// Docker reaps it, and the soak holds 64 streams and its workers'
/// connections open, so it is the death to a few ms -- on the clock every
/// write's answer is timed on. `closed` is the burst's first close
/// (`Shared::closed`; one close alone is none), `answered` the last answer
/// read at all: a server read answering more than `CLOSE_SLOP` after a
/// burst was alive then, and that burst no death; one read within it was
/// written before the death, which is taken at that answer where it came
/// after the burst -- never before the last answer, as a time before the
/// real death excuses writes the server held for longer than
/// `--loss-ms`. Docker's times (`docker`: its `die` event, or
/// `FinishedAt`) are the VM's clock, which this process knows only by the
/// probe it keeps (`Shared::clock`), to that reading's error (`err`,
/// `Shared::clock_err`: its round trip, the drift since); and they are the
/// reap, after the process's teardown, its sockets' close among it: taken
/// where no burst was read -- at the latest the reading's error allows,
/// and never before the last answer, so that it is never before the death
/// it stands for -- and said beside the burst where one was. A probe the
/// minute took behind a restart's backlog answered in seconds (the dry run
/// of 2026-11-26's minute 4, its p99 3.4 s): read through it as it came, a
/// death with no burst was timed up to half that before its reap. With
/// neither, the minute's look before it (`look`), or the last answer
/// where that came after it.
fn death_time(
    closed: u64,
    answered: u64,
    docker: Option<(u64, Source)>,
    err: u64,
    look: u64,
) -> (u64, Source, Option<u64>) {
    let c = closed;
    let last = (answered > 0).then_some(answered);
    if c != u64::MAX && answered <= c.saturating_add(CLOSE_SLOP) {
        return (c.max(answered), Source::Close, last);
    }
    match docker {
        Some((d, s)) => (d.saturating_add(err).max(answered), s, last),
        None => (look.max(answered), Source::Look, last),
    }
}

/// How far before the kill due at minute `due` a death at `at` (ns from the
/// start) came, in words, and in seconds for the line: negative where the
/// kill was due first and waited for the side jobs before it -- the long
/// run of 2026-11-25's primary died in the compact the kill of minute 300
/// waited for.
fn before_kill(at: u64, due: Option<u64>) -> (String, Option<f64>) {
    let Some(m) = due else {
        return ("no kill due after it".into(), None);
    };
    let s = (m * 60) as f64 - at as f64 / 1e9;
    let said = match s >= 0.0 {
        true if s >= 120.0 => format!("{:.1} min before the kill due at minute {m}", s / 60.0),
        true => format!("{s:.1} s before the kill due at minute {m}"),
        false => format!(
            "{:.1} s after the kill due at minute {m} was due, as it waited for the side jobs",
            -s
        ),
    };
    (said, Some(s))
}

/// One look at a server's cgroup a second (`watch_memory`): its anonymous
/// memory, page cache, dirty and writeback pages, in bytes, and the major
/// faults its processes took so far (`memory.stat`'s `anon`, `file`,
/// `file_dirty`, `file_writeback`, `pgmajfault`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MemSample {
    anon: u64,
    file: u64,
    dirty: u64,
    writeback: u64,
    majflt: u64,
}

impl MemSample {
    /// The watch's line, `M <anon> <file> <dirty> <writeback> <majflt>`.
    fn parse(l: &str) -> Option<MemSample> {
        let mut w = l.strip_prefix("M ")?.split_whitespace();
        let mut n = || w.next()?.parse::<u64>().ok();
        Some(MemSample {
            anon: n()?,
            file: n()?,
            dirty: n()?,
            writeback: n()?,
            majflt: n()?,
        })
    }
}

/// A stretch's looks a second -- a minute's, a life's, a minute's seconds
/// in a window of the replica's -- as the line writes them: the most
/// anonymous memory and dirty pages, the least page cache, the major
/// faults between them, and how many looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemSpan {
    anon: u64,
    dirty: u64,
    file: u64,
    faults: u64,
    n: u64,
}

impl MemSpan {
    fn add(span: &mut Option<MemSpan>, s: MemSample, faults: u64) {
        let x = span.get_or_insert(MemSpan {
            anon: 0,
            dirty: 0,
            file: u64::MAX,
            faults: 0,
            n: 0,
        });
        x.anon = x.anon.max(s.anon);
        x.dirty = x.dirty.max(s.dirty);
        x.file = x.file.min(s.file);
        x.faults += faults;
        x.n += 1;
    }
}

/// A server's looks a second as the run keeps them: the latest and when,
/// the minute's since its line took them, the life's since its latest
/// start, and the minute's seconds a window of the replica's was open in.
/// The minute line's one look missed the compaction's jump of some 130 MB
/// that OOM-killed the long run's primary at minute 300.
#[derive(Default)]
struct Mem {
    last: Option<(u64, MemSample)>,
    minute: Option<MemSpan>,
    life: Option<MemSpan>,
    window: Option<MemSpan>,
}

impl Mem {
    /// A look at `t`, the replica's window open then or not: its faults
    /// are those since the last look of the same life.
    fn sample(&mut self, t: u64, s: MemSample, in_window: bool) {
        let faults = match self.last {
            Some((_, p)) if s.majflt >= p.majflt => s.majflt - p.majflt,
            _ => 0,
        };
        MemSpan::add(&mut self.minute, s, faults);
        MemSpan::add(&mut self.life, s, faults);
        if in_window {
            MemSpan::add(&mut self.window, s, faults);
        }
        self.last = Some((t, s));
    }

    /// A new life: the server started again, its cgroup's counters with it.
    fn restarted(&mut self) {
        self.life = None;
        self.last = None;
    }
}

/// The open loop's lag sampled each second (`seconds`), the most of each
/// lane's since the minute's line took them, and the most while a window
/// of the replica's was open, with how many seconds were.
#[derive(Default)]
struct Behind {
    main: Option<f64>,
    replica: Option<f64>,
    window_main: Option<f64>,
    window_replica: Option<f64>,
    window_s: u64,
}

/// `x` held as the most of `m`.
fn most(m: &mut Option<f64>, x: Option<f64>) {
    if let Some(x) = x {
        *m = Some(m.map_or(x, |y| y.max(x)));
    }
}

/// The minute line's names for each server's looks a second, the
/// primary's then the replica's: the looks, the most anonymous memory and
/// dirty pages, the least page cache, the life's most of the two, and in
/// the seconds a window of the replica's was open the faults and the least
/// page cache.
const MEM_KEYS: [[&str; 8]; 2] = [
    [
        "mem_n",
        "anon_peak",
        "dirty_peak",
        "file_min",
        "anon_peak_life",
        "dirty_peak_life",
        "rc_majflt",
        "rc_file_min",
    ],
    [
        "r_mem_n",
        "r_anon_peak",
        "r_dirty_peak",
        "r_file_min",
        "r_anon_peak_life",
        "r_dirty_peak_life",
        "rc_r_majflt",
        "rc_r_file_min",
    ],
];

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
        // The server's clock against this host's, a probe a minute
        // (`CLOCK_PROBE`): `now()` into an int is its milliseconds.
        "create collection clock (t int)".into(),
    ]
}

/// The probe the minute's loop sends to read the server's clock: `now()`
/// as the write ran, in ms since the epoch.
const CLOCK_PROBE: &str = "put clock {id: 1, t: now()} returning *";

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
            let mut said = Instant::now();
            while key <= n {
                if said.elapsed() >= Duration::from_secs(60) {
                    say(format!("loading {path}: {} of {n}", key - 1));
                    said = Instant::now();
                }
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
    say(format!(
        "{} records, {} docs and {} counters loaded in {:.0} s",
        o.records,
        o.docs,
        o.counters,
        t.elapsed().as_secs_f64()
    ));
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
    /// A read of a record: whether it was there, and its fields' digests;
    /// `replica`, one through the replica, which is sent after the writes
    /// answered before it began and may lag those answered since: held to
    /// the registers alone, never to the primary's reads before it.
    Row {
        key: u64,
        s: u64,
        e: u64,
        found: bool,
        got: [u64; FIELDS],
        replica: bool,
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
    /// A record as the file restored from backup `backup` holds it: a read
    /// over the backup's span, the moment its image was taken being in it.
    Restored {
        backup: u64,
        key: u64,
        s: u64,
        e: u64,
        found: bool,
        got: [u64; FIELDS],
    },
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
            | Ev::Count { s, e, .. }
            | Ev::Restored { s, e, .. } => Some((s, e)),
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
    /// The writes a read after a kill showed gone (`Registers::gone`),
    /// each counted once; how many since the minute's line took them, and
    /// the largest gap among them between the answer and its kill, in ms
    /// rounded up (`ms_up`).
    gone_seen: HashSet<(u64, u8, u64)>,
    lost_writes: u64,
    lost_ms_max: Option<i64>,
    /// Backups whose restored file held a record no write left.
    restored_bad: HashSet<u64>,
    backups_differ: u64,
}

/// A gap in ns as whole ms, rounded up: the loss bound is held in ns, so a
/// gap a fraction of a ms past it reads past it -- 650.4 ms as 651, never
/// as the 650 that would say the run supports a bound it broke.
fn ms_up(ns: i64) -> i64 {
    match ns > 0 {
        true => (ns + 999_999) / 1_000_000,
        // Toward zero, which is up for a gap below it.
        false => ns / 1_000_000,
    }
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
            gone_seen: HashSet::new(),
            lost_writes: 0,
            lost_ms_max: None,
            restored_bad: HashSet::new(),
            backups_differ: 0,
        }
    }

    fn wrong(&mut self, what: String) {
        self.mismatches += 1;
        if self.examples.len() < 20 {
            say(format!("mismatch: {what}"));
            self.examples.push(what);
        }
    }

    /// One register's value read over `[s, e]`, held as one of the reads'
    /// history (`pin`: a worker's or a sweep's read, given in the order the
    /// reads ended) or alone (a stream's event); `None` where it may hold
    /// it, else what to add to the mismatch's words. Where `gone`, the
    /// writes the read shows gone with a kill are counted and the largest
    /// gap between one's answer and its kill kept -- the figure the loss
    /// bound is read against -- and one past the bound named.
    fn held(
        &mut self,
        key: u64,
        f: u8,
        (s, e): (u64, u64),
        d: u64,
        pin: bool,
        gone: bool,
    ) -> Option<String> {
        let base = self.base(key, f);
        let ok = match pin {
            true => self.regs.read(key, f, s, e, d, base),
            false => self.regs.may_hold(key, f, s, e, d, base),
        };
        let mut past: Option<i64> = None;
        if gone {
            for (gap, certain, ws) in self.regs.gone(key, f, s, e, d) {
                let ms = ms_up(gap);
                if self.gone_seen.insert((key, f, ws)) {
                    self.lost_writes += 1;
                    self.lost_ms_max = Some(self.lost_ms_max.map_or(ms, |m| m.max(ms)));
                }
                if certain {
                    past = Some(past.map_or(ms, |p| p.max(ms)));
                }
            }
        }
        match ok {
            true => None,
            false => Some(match past {
                Some(ms) => format!(
                    "; a write answered {ms} ms before a kill or a death is gone, past the {} ms a kill may take",
                    self.regs.kills.loss / 1_000_000
                ),
                None => String::new(),
            }),
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
    /// `pin`: one of the workers' and sweeps' reads, given in the order
    /// they ended, which the reads after it are held to.
    fn hold_one(&mut self, ev: &Ev, safe: u64, pin: bool) -> bool {
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
                replica,
            } => {
                self.reads += 1;
                let pin = pin && !replica;
                if let Some(why) = self.held(key, EXISTS, (s, e), found as u64, pin, true) {
                    let what = if found { "found it" } else { "found nothing" };
                    self.wrong(format!("a read of {key} [{s}, {e}] ns {what}{why}"));
                    return true;
                }
                if found {
                    for f in 0..FIELDS as u8 {
                        if let Some(why) = self.held(key, f, (s, e), got[f as usize], pin, true) {
                            self.wrong(format!(
                                "a read of {key} [{s}, {e}] ns: field{f} holds no value written to it{why}"
                            ));
                            break;
                        }
                    }
                }
            }
            Ev::Session { id, s, e, found } => {
                self.reads += 1;
                // A session's expiry is a write of its absence over a span:
                // nothing a kill takes, so nothing is shown gone by it.
                if self
                    .held(id, SESSION, (s, e), found as u64, pin, false)
                    .is_some()
                {
                    self.wrong(format!(
                        "session {id} read over [{s}, {e}] ns: {}",
                        if found {
                            "there past its time, or back after a read found it gone"
                        } else {
                            "gone inside it, or gone again after a read found it"
                        }
                    ));
                }
            }
            Ev::Restored {
                backup,
                key,
                s,
                e,
                found,
                got,
            } => {
                self.reads += 1;
                let bad = !self.may_hold(key, EXISTS, s, e, found as u64)
                    || (found
                        && (0..FIELDS as u8)
                            .any(|f| !self.may_hold(key, f, s, e, got[f as usize])));
                if bad {
                    self.wrong(format!(
                        "the file restored from backup {backup} holds {key} as no write left it over [{s}, {e}] ns"
                    ));
                    if self.restored_bad.insert(backup) {
                        self.backups_differ += 1;
                    }
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
                        replica: false,
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
            // A deletion outside the range is the server's word that an id
            // the stream does not hold changed: a plain stream judges each
            // changed id by its filter alone, as the row stands, and tells
            // one that does not match as deleted whether or not it ever did
            // (fennec-core's `Database::changes_since`, "a deliberate
            // looseness"; a client deleting a row it lacks does nothing).
            // Nothing to hold, and no write of the range's -- the dry run of
            // 2026-11-22 counted every write outside each range as a key
            // sent twice and a mismatch, 23.5 million of each. A row put
            // outside it is still wrong (`row`).
            let dels: Vec<u64> = ev
                .dels
                .iter()
                .copied()
                .filter(|k| (from..to).contains(k))
                .collect();
            let mut keys: HashSet<u64> = ev.rows.iter().map(|r| r.0).collect();
            keys.extend(dels.iter().copied());
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
            // An event of ids outside the range alone holds nothing, so
            // nothing of it is late.
            if !ev.rows.is_empty() || !dels.is_empty() {
                if s < horizon {
                    faults.late += 1;
                } else {
                    for &(key, got) in &ev.rows {
                        row(key, s, true, got, &mut wrong);
                    }
                    for &key in &dels {
                        row(key, s, false, [0; FIELDS], &mut wrong);
                    }
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
            self.hold_one(&r, u64::MAX, false);
        }
    }

    /// Holds every read and stream event that ended before `safe`, judges
    /// the streams still open for silence (`open`: each one's connection
    /// opened at, `u64::MAX` closed, and the latest change it read), then
    /// lets go of what no read from `cutoff` on can be answered from.
    fn hold(&mut self, safe: u64, cutoff: u64, now: u64, open: &[(u64, u64)]) {
        let mut pending = std::mem::take(&mut self.pending);
        // In the order they ended: every read that ended before another
        // began is held before it (`Registers::read`).
        pending.sort_by_key(|ev| ev.span().map_or(0, |x| x.1));
        for ev in pending {
            if !self.hold_one(&ev, safe, true) {
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

/// The slowest `n` minutes in a row of the peak's (minute, operations,
/// seconds), as the minute they start at and their operations over their
/// time; `None` where there are fewer than `n`. What the soak's rate is a
/// fraction of: the open loop must hold it through the server's own
/// compactions, the slowest stretch of a cycle, or it falls behind each
/// cycle and drains the backlog only once the compacted file is fast again
/// -- if before the next.
fn slowest_stretch(peak: &[(u64, f64, f64)], n: usize) -> Option<(u64, f64)> {
    peak.windows(n.max(1))
        .map(|w| {
            let ops: f64 = w.iter().map(|x| x.1).sum();
            let secs: f64 = w.iter().map(|x| x.2).sum();
            (w[0].0, ops / secs.max(1.0))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Which of a worker's histograms an operation goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Steady,
    Compact,
    Restart,
    /// The replica's own compaction's: a histogram of its own
    /// (`Hists::r_compact`), out of the compacts' and the hour's figures --
    /// the replica's compactions are not on the schedule the last hour is
    /// held to its reference by -- and judged against the longest watched
    /// compaction alone (`judge_run`).
    ReplicaCompact,
}

/// The windows an operation's class is read from, each `(from, to)` in ns
/// from the start, `to` `u64::MAX` while one is open: the latest restart's
/// and the schedule's latest compact's, and the latest compaction the
/// replica ran on its own (`watch_replica`).
#[derive(Clone, Copy)]
struct Windows {
    restart: (u64, u64),
    compact: (u64, u64),
    replica: (u64, u64),
}

/// An operation's class: a restart's from a second before its kill until
/// the server answered again, and after that until its worker is back on
/// its schedule -- the open loop's backlog drains for minutes at 80%, its
/// waits the restart's, not the steady tail's -- and a compact's alike;
/// the steady otherwise. `at` is when it was due, `done` when it was
/// answered, `via_replica` whether it was a read through the replica,
/// `on_time` whether the worker reached it before it was due (it waited
/// for it), `owed` the worker's own: the window it is still draining.
///
/// A read through the replica waiting -- for its worker or its answer --
/// at any moment of a compaction of the replica's own is that
/// compaction's, and its worker's backlog after it: the fourth dry run of
/// 2026-11-24 had reads through the replica answered in 2.7 s at the p99
/// of its minute 118, the replica compacting, and every connection held
/// on one -- the minute's p99 from the schedule 2.2 s, what the servers
/// answered in 261 ms, and all of it steady. The compaction's is taken
/// before a restart's window, and before a restart its worker still owes:
/// at its kill of minute 60 the replica had begun compacting half a
/// second before the kill was announced, its stream's end read 4.2 s
/// after the kill, and the operations due in the restart waited for a
/// worker until it answered -- their p99 5.1 s against the restart's
/// bound of 4.9, the servers answering in 249 ms. And reads went through
/// it again 8.1 s after the kill, its compaction 15 s from its end: one
/// sent by a worker still behind with the restart's backlog was held by
/// the compaction, not the restart, and taken as the restart's it was
/// judged against the replica's return (10.1 s) with the backlog's lag
/// and the compaction's hold in it.
fn class(
    at: u64,
    done: u64,
    via_replica: bool,
    on_time: bool,
    w: &Windows,
    owed: &mut Option<Class>,
) -> Class {
    if on_time {
        *owed = None;
    }
    let within = |w: (u64, u64)| at >= w.0 && at <= w.1;
    let c = if *owed == Some(Class::ReplicaCompact)
        || via_replica && at <= w.replica.1 && done >= w.replica.0
    {
        Class::ReplicaCompact
    } else if within(w.restart) || *owed == Some(Class::Restart) {
        Class::Restart
    } else if within(w.compact) || *owed == Some(Class::Compact) {
        Class::Compact
    } else {
        Class::Steady
    };
    if c != Class::Steady {
        *owed = Some(c);
    }
    c
}

/// A worker's classes as its operations end (`class`): the window it
/// still owes, and the same were the replica's compactions the watch has
/// not settled none (`otherwise`), with the number of the replica's
/// compaction it owes. The watch opens a window wherever the replica's
/// compaction thread is at work for half a second, and its 5 s look at
/// the files can wait that long on the replica's lock -- a seed landing in
/// one block after a reconnect -- so a window is a compaction only where
/// the replica's compaction counter moved in it (`replica_compaction`):
/// the operations of one not yet settled are held (`Hists::held`) with
/// the class they would have otherwise, and a worker owing one that was
/// none goes back to the chain it would have had.
#[derive(Default)]
struct Classes {
    owed: Option<Class>,
    otherwise: Option<Class>,
    window: u64,
}

impl Classes {
    /// An operation's class, `class`'s arguments with `id` the number of
    /// the replica's latest window (`w.replica`) and `verdict` each one's
    /// as it settled: where it is a replica's compaction's the watch has
    /// not settled, the window's number and the class it has otherwise.
    #[allow(clippy::too_many_arguments)]
    fn of(
        &mut self,
        at: u64,
        done: u64,
        via_replica: bool,
        on_time: bool,
        w: &Windows,
        id: u64,
        verdict: &dyn Fn(u64) -> Option<bool>,
    ) -> (Class, Option<(u64, Class)>) {
        if self.owed == Some(Class::ReplicaCompact) && verdict(self.window) == Some(false) {
            self.owed = self.otherwise;
        }
        let owing = self.owed == Some(Class::ReplicaCompact) && !on_time;
        let c = class(at, done, via_replica, on_time, w, &mut self.owed);
        let none = Windows {
            replica: (u64::MAX, u64::MAX),
            ..*w
        };
        let otherwise = class(at, done, via_replica, on_time, &none, &mut self.otherwise);
        if c != Class::ReplicaCompact {
            self.otherwise = self.owed;
            return (c, None);
        }
        if !owing {
            self.window = id;
        }
        match verdict(self.window) {
            Some(true) => {
                self.otherwise = self.owed;
                (c, None)
            }
            Some(false) => {
                self.owed = self.otherwise;
                (otherwise, None)
            }
            None => (c, Some((self.window, otherwise))),
        }
    }
}

/// A compaction of the replica's own as its compaction thread's watch
/// tells it (`watch_replica`): `A <arch>` once, `T <tid>...` the threads
/// named `R_COMPACT_THREAD`, and `S <tid> <syscall>` at each change of
/// what one of them is in. fennec-http's sleeps between the server's looks
/// at its files and is at work in anything else -- `running`, `-1` outside
/// a call, a read, a lock's or a join's futex -- through every part of a
/// compaction, its graphs rebuilt before its side file is written too.
/// fennec-core's `Compactor`, the native library's, is so named as well
/// and waits between its looks on a futex, never in a sleep: the thread
/// watched is the first seen in a sleep call (`plain_sleep`), whichever
/// the order the replica's threads are listed in, and none is watched
/// before one is -- taking the last so named, a replica running both
/// would have read as one compaction the run long. Either state is taken
/// once held `R_WATCH_SURE`, dated from its first look: a compaction from
/// a poll and a little before it, its end from the first look asleep.
#[derive(Default)]
struct CompactWatch {
    arch: String,
    /// The thread watched: fennec-http's, seen asleep in a sleep call.
    tid: Option<String>,
    /// Whether this container listed the threads so named (`T`).
    listed: bool,
    /// The thread settled at work: a compaction under way.
    open: bool,
    /// The first look at the other state, not yet held long enough.
    since: Option<u64>,
    /// A compaction a watch before this one saw begin, its container gone
    /// in it: its end is seen only where this one sees the thread at work
    /// first, and is lost otherwise -- it ended unseen (`Edge::Lost`).
    resumed: bool,
}

/// A stretch of the run, `(from, to)` in ns from its start.
type Span = (u64, u64);

/// A change `CompactWatch` settled on, at its time in ns from the start.
#[derive(Debug, PartialEq, Eq)]
enum Edge {
    Began(u64),
    /// Its end seen: the thread back asleep.
    Ended(u64),
    /// Its end not seen: the thread gone with the replica's process, or
    /// the run ended first. Never counted as an end (`r_compacts_cut`).
    Cut(u64),
    /// Its end not seen, the watch lost it: the thread found asleep by a
    /// watch after the one that saw it begin. Cut, and lost besides
    /// (`r_compacts_lost`): a window the watch lost while it was open is
    /// never a pass of the watch's criterion -- a thread that never slept
    /// again, its counter unmoved, agreed with the watch that saw nothing.
    Lost(u64),
}

/// Whether the replica's compaction thread is asleep in `n`, the syscall
/// `/proc/<task>/syscall` names, by the VM's architecture: nanosleep,
/// clock_nanosleep and restart_syscall (a sleep a signal cut, resumed).
/// `running`, and `-1` for a thread waiting outside a call, are at work.
fn asleep(arch: &str, n: &str) -> bool {
    let Ok(n) = n.parse::<u64>() else {
        return false;
    };
    match arch {
        "x86_64" => [35, 230, 219].contains(&n),
        _ => [101, 115, 128].contains(&n),
    }
}

/// Whether `n` is a sleep call itself -- nanosleep, clock_nanosleep --
/// which fennec-http's compaction thread is in between its looks and
/// fennec-core's `Compactor` never is: it waits on a condvar, a futex,
/// whose timed wait a signal also resumes through restart_syscall.
fn plain_sleep(arch: &str, n: &str) -> bool {
    let Ok(n) = n.parse::<u64>() else {
        return false;
    };
    match arch {
        "x86_64" => [35, 230].contains(&n),
        _ => [101, 115].contains(&n),
    }
}

impl CompactWatch {
    /// The watch after `prev`, whose container ended: the thread it
    /// watched, and a compaction it saw begin, still open.
    fn after(prev: &CompactWatch) -> CompactWatch {
        CompactWatch {
            tid: prev.tid.clone(),
            open: prev.open,
            resumed: prev.open,
            ..CompactWatch::default()
        }
    }

    /// Whether it is looking at the replica's compaction thread: one it
    /// watches, listed by this container -- one started again says nothing
    /// before it lists the threads, and a `docker run` that hangs never
    /// does, where the thread kept from the last read as watched.
    fn watching(&self) -> bool {
        self.tid.is_some() && self.listed
    }

    /// A line of the watch's, read at `t`.
    fn line(&mut self, l: &str, t: u64) -> Option<Edge> {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["A", a] => {
                self.arch = a.to_string();
                None
            }
            ["T", tids @ ..] => {
                self.listed = true;
                // The thread watched is not among them: the replica's
                // process is another, a compaction it was in cut with it.
                if self.tid.as_deref().is_some_and(|x| !tids.contains(&x)) {
                    self.tid = None;
                    self.since = None;
                    self.resumed = false;
                    if std::mem::take(&mut self.open) {
                        return Some(Edge::Cut(t));
                    }
                }
                None
            }
            ["S", tid, n] => {
                if self.tid.is_none() && plain_sleep(&self.arch, n) {
                    self.tid = Some(tid.to_string());
                }
                if self.tid.as_deref() != Some(*tid) {
                    return None;
                }
                let busy = !asleep(&self.arch, n);
                if busy && self.open {
                    self.resumed = false;
                }
                if busy == self.open {
                    self.since = None;
                } else if self.since.is_none() {
                    self.since = Some(t);
                }
                self.tick(t)
            }
            _ => None,
        }
    }

    /// Time gone by to `t` with no change said: the other state, held long
    /// enough, is taken.
    fn tick(&mut self, t: u64) -> Option<Edge> {
        let s = self.since?;
        if t.saturating_sub(s) < R_WATCH_SURE {
            return None;
        }
        self.since = None;
        self.open = !self.open;
        Some(match (self.open, std::mem::take(&mut self.resumed)) {
            (true, _) => Edge::Began(s.saturating_sub(2 * R_WATCH_EVERY)),
            (false, false) => Edge::Ended(s),
            (false, true) => Edge::Lost(s),
        })
    }

    /// The run's end with a compaction open: cut, its end unseen.
    fn stop(&mut self, t: u64) -> Option<Edge> {
        self.since = None;
        self.resumed = false;
        std::mem::take(&mut self.open).then_some(Edge::Cut(t))
    }
}

/// How far the open loop is behind its schedule at `now` (ns from the
/// start), in seconds: the median connection's lag behind the operation it
/// waits for or works on, none where it is ahead -- `None` at the peak,
/// which keeps no schedule. A server that answers under the rate leaves
/// every connection behind and the lag growing: the second dry run of
/// 2026-11-23 fell behind at minute 13 and never caught up, 1 656 s behind
/// at its end, and from minute 28 every operation was a restart's or a
/// compact's backlog (`class`), its steady p99 written as 0 for none.
fn behind_s(slots: &[Slot], now: u64) -> Option<f64> {
    let lags: Vec<f64> = slots
        .iter()
        .map(|s| s.due.load(Ordering::Relaxed))
        .filter(|&d| d != u64::MAX)
        .map(|d| now.saturating_sub(d) as f64 / 1e9)
        .collect();
    (!lags.is_empty()).then(|| median(&lags))
}

/// A worker's latencies: an operation is timed from when it was due, and
/// counted apart where it was a restart's or a compact's (`class`) -- a
/// restart's where the loop was behind its schedule at the kill apart
/// again (`restart_behind`): those wait out the backlog, not the restart;
/// and the reads through the replica among a restart's apart from the
/// primary's operations (`restart_replica`): none goes through it from
/// the kill's announcement until it is back and the restart's grace is
/// over, so one due in a restart waited out the replica's return, which
/// the report bounds apart. Beside them each operation's own time, from
/// its send to its answer (`own`), and that of the reads through the
/// replica alone (`replica`): what the servers answered in, without the
/// schedule's wait. And the operations of a compaction of the replica's
/// own (`r_compact`), those due in a restart's window among them
/// (`restart_r_compact`) -- out of the compacts': the fourth dry run of
/// 2026-11-24 had the replica compact in its lines 61 and 118, which no
/// schedule puts in an hour, and the last hour's compacts held to its
/// reference's with one in either would fail a right server or hide a
/// slower compact of the primary's. And those of a compaction the watch
/// has not settled yet (`held`), in the order they ended: one its counter
/// did not move in was none (`settle`); and the most it held since the
/// minute's line took its figures (`held_max`), the most of every worker's
/// on the line (`r_held_max`): how near the run came to `HELD_MAX`. And
/// the reads through the replica among a compaction's of its own, from
/// their send (`r_compact_own`): what the replica answered them in, beside
/// the class's time from the schedule -- dry run 6's class p99 of 5.5 s was
/// the loop's queue, the replica's own answers 0.44-0.72 s at the p99 of
/// those minutes, and no line could say so.
struct Hists {
    steady: Hist,
    restart: Hist,
    restart_replica: Hist,
    restart_behind: Hist,
    compact: Hist,
    own: Hist,
    replica: Hist,
    r_compact: Hist,
    r_compact_own: Hist,
    restart_r_compact: u64,
    held: VecDeque<Held>,
    held_max: usize,
}

/// An operation of a replica's compaction the watch has not settled
/// (`Shared::r_verdicts`), kept until it has: the window it was counted
/// in, the class it has if that was no compaction, and `Hists::record`'s
/// own arguments (`Rec`).
#[derive(Clone, Copy, Debug)]
struct Held {
    window: u64,
    otherwise: Class,
    rec: Rec,
}

/// What `Hists::record` takes of an operation beside its class:
/// `via_replica` a read through the replica, `behind` the latest kill met
/// with the loop behind its schedule (`Shared::restart_behind`),
/// `in_restart` due in a restart's window, `took` its time from when it
/// was due, `own_us` from its send to its answer in µs -- a `u32`, 71
/// minutes, so that a held operation stays 32 bytes (`HELD_MAX`).
#[derive(Clone, Copy, Debug)]
struct Rec {
    via_replica: bool,
    behind: bool,
    in_restart: bool,
    took: u64,
    own_us: u32,
}

impl Hists {
    fn new() -> Hists {
        Hists {
            steady: Hist::new(),
            restart: Hist::new(),
            restart_replica: Hist::new(),
            restart_behind: Hist::new(),
            compact: Hist::new(),
            own: Hist::new(),
            replica: Hist::new(),
            r_compact: Hist::new(),
            r_compact_own: Hist::new(),
            restart_r_compact: 0,
            held: VecDeque::new(),
            held_max: 0,
        }
    }
    /// `o`'s latencies added in, and the most it held; what `o` holds
    /// unsettled is its own.
    fn merge(&mut self, o: &Hists) {
        self.held_max = self.held_max.max(o.held_max);
        self.steady.merge(&o.steady);
        self.restart.merge(&o.restart);
        self.restart_replica.merge(&o.restart_replica);
        self.restart_behind.merge(&o.restart_behind);
        self.compact.merge(&o.compact);
        self.own.merge(&o.own);
        self.replica.merge(&o.replica);
        self.r_compact.merge(&o.r_compact);
        self.r_compact_own.merge(&o.r_compact_own);
        self.restart_r_compact += o.restart_r_compact;
    }

    /// [`Hists::record`] of an operation's `Rec`: a read through the
    /// replica among a compaction's of its own counted by its own answer
    /// time too (`r_compact_own`).
    fn record_rec(&mut self, c: Class, r: Rec) {
        self.record(c, r.via_replica, r.behind, r.in_restart, r.took);
        if c == Class::ReplicaCompact && r.via_replica {
            self.r_compact_own.record(r.own_us as u64 * 1_000);
        }
    }

    /// An operation of class `c` that took `took` from when it was due:
    /// `via_replica` a read through the replica, `behind` the latest kill
    /// met with the loop behind its schedule (`Shared::restart_behind`),
    /// `in_restart` due in a restart's window.
    fn record(&mut self, c: Class, via_replica: bool, behind: bool, in_restart: bool, took: u64) {
        match c {
            Class::Restart if behind => self.restart_behind.record(took),
            Class::Restart if via_replica => self.restart_replica.record(took),
            Class::Restart => self.restart.record(took),
            Class::Compact => self.compact.record(took),
            Class::ReplicaCompact => {
                self.r_compact.record(took);
                self.restart_r_compact += in_restart as u64;
            }
            Class::Steady => self.steady.record(took),
        }
    }

    /// The operations held for windows `verdict` now settles, from the
    /// oldest on: a compaction's where it says one ran, their own class
    /// where it says none did; the rest kept from the first it says
    /// nothing of yet. A worker's windows are numbered in the order they
    /// open, and each is settled before the next opens (its end, or a cut),
    /// so the oldest held settles first: a look asks of the oldest's window
    /// alone, and an operation is moved once, as it settles. Before, every
    /// operation a worker ended rescanned and moved all it held, under its
    /// slot's lock -- quadratic in a window's length.
    fn settle(&mut self, verdict: &dyn Fn(u64) -> Option<bool>) {
        let mut known: Option<(u64, bool)> = None;
        while let Some(&x) = self.held.front() {
            let was = match known {
                Some((w, was)) if w == x.window => was,
                _ => match verdict(x.window) {
                    Some(was) => {
                        known = Some((x.window, was));
                        was
                    }
                    None => break,
                },
            };
            self.held.pop_front();
            let c = if was {
                Class::ReplicaCompact
            } else {
                x.otherwise
            };
            self.record_rec(c, x.rec);
        }
    }

    /// An operation answered, of class `c` (`Classes::of`): recorded, or
    /// held for the replica's window it names (`held`), appended in the
    /// order it ended -- the operations held before it settled first as
    /// far as their windows are. At `HELD_MAX` held, the window held for
    /// is handed back to be cut (`cut_r_window`) and settled after as none.
    fn took(
        &mut self,
        c: Class,
        held: Option<(u64, Class)>,
        r: Rec,
        verdict: &dyn Fn(u64) -> Option<bool>,
    ) -> Option<u64> {
        if !self.held.is_empty() {
            self.settle(verdict);
        }
        match held {
            None => {
                self.record_rec(c, r);
                None
            }
            Some((window, otherwise)) => {
                self.held.push_back(Held {
                    window,
                    otherwise,
                    rec: r,
                });
                self.held_max = self.held_max.max(self.held.len());
                (self.held.len() >= HELD_MAX).then_some(window)
            }
        }
    }
}

/// Whether minute `m`'s line is the run's last: its planned `minutes`, or
/// the environment ending it there (`--disk-floor`, `env_failed`). No
/// line after settles what the workers hold (`flush_held`).
fn last_line(m: u64, minutes: u64, env_failed: bool) -> bool {
    m >= minutes || env_failed
}

/// What every worker holds for windows of the replica's (`Hists::held`),
/// settled into the minute's `h` by `verdict`, which says of each: the
/// run's last line, which no later one settles.
fn flush_held(slots: &[Slot], h: &mut Hists, verdict: &dyn Fn(u64) -> Option<bool>) {
    for s in slots {
        let mut held = std::mem::take(&mut s.hists.lock().unwrap().held);
        h.held.append(&mut held);
        h.settle(verdict);
    }
}

/// The latencies a run keeps across its minutes: each of the last 60
/// minutes' steady, compacts' and replica compactions' operations, for
/// the hour's figures, and the restarts' and the replica compactions'
/// over the run, for the end line's, with the most a worker held for a
/// window of the replica's (`Hists::held_max`).
struct Kept {
    hours: VecDeque<(Hist, Hist, Hist)>,
    restart: Hist,
    restart_replica: Hist,
    restart_behind: Hist,
    r_compact: Hist,
    r_compact_own: Hist,
    held_max: usize,
}

/// `p99` of each histogram that holds an operation, named: one that holds
/// none has no quantile, and written as 0 it read as the run's best --
/// the primary's restarts passed on a p99 of 0 with no operation due in
/// one, which a replica's compaction holding every worker through a kill
/// makes.
fn p99s<'a>(figs: &[(&'a str, &Hist)]) -> Vec<(&'a str, String)> {
    figs.iter()
        .filter(|(_, h)| h.n > 0)
        .map(|(k, h)| (*k, format!("{:.0}", h.pct(0.99))))
        .collect()
}

impl Kept {
    fn new() -> Kept {
        Kept {
            hours: VecDeque::new(),
            restart: Hist::new(),
            restart_replica: Hist::new(),
            restart_behind: Hist::new(),
            r_compact: Hist::new(),
            r_compact_own: Hist::new(),
            held_max: 0,
        }
    }

    /// The minute's latencies `h` taken in, and its line's figures of
    /// them: the operations of each class, and their quantiles where there
    /// were any -- a minute every connection spent behind its schedule has
    /// no steady operation, and its quantile written as 0 read as the
    /// run's best minute (the second dry run of 2026-11-23's every minute
    /// from 28 to 120) -- with the hour's to it, and what the servers
    /// answered in from each operation's send: beside the times from when
    /// it was due, whether the server or the schedule kept it waiting.
    fn minute(&mut self, h: &Hists) -> Vec<(&'static str, String)> {
        self.restart.merge(&h.restart);
        self.restart_replica.merge(&h.restart_replica);
        self.restart_behind.merge(&h.restart_behind);
        self.r_compact.merge(&h.r_compact);
        self.r_compact_own.merge(&h.r_compact_own);
        self.held_max = self.held_max.max(h.held_max);
        if self.hours.len() == 60 {
            self.hours.pop_front();
        }
        let (mut s, mut c, mut r) = (Hist::new(), Hist::new(), Hist::new());
        s.merge(&h.steady);
        c.merge(&h.compact);
        r.merge(&h.r_compact);
        self.hours.push_back((s, c, r));
        let (mut hour, mut chour, mut rhour) = (Hist::new(), Hist::new(), Hist::new());
        for x in &self.hours {
            hour.merge(&x.0);
            chour.merge(&x.1);
            rhour.merge(&x.2);
        }
        let mut v: Vec<(&'static str, String)> = vec![
            ("steady_ops", h.steady.n.to_string()),
            ("restart_ops", h.restart.n.to_string()),
            ("restart_replica_ops", h.restart_replica.n.to_string()),
            ("restart_behind_ops", h.restart_behind.n.to_string()),
            ("compact_ops", h.compact.n.to_string()),
            ("r_compact_ops", h.r_compact.n.to_string()),
            ("restart_r_compact_ops", h.restart_r_compact.to_string()),
            ("r_held_max", h.held_max.to_string()),
        ];
        if h.steady.n > 0 {
            v.extend([
                ("p50", format!("{:.0}", h.steady.pct(0.5))),
                ("p999", format!("{:.0}", h.steady.pct(0.999))),
                ("max", format!("{:.0}", h.steady.max as f64 / 1e3)),
            ]);
        }
        v.extend(p99s(&[
            ("p99", &h.steady),
            ("p99_hour", &hour),
            ("p99_restart", &h.restart),
            ("p99_restart_replica", &h.restart_replica),
            ("p99_restart_behind", &h.restart_behind),
            ("p99_compact", &h.compact),
            ("p99_compact_hour", &chour),
            // The replica's own compactions', the minute's and the hour's:
            // said, never held to another hour's (`p99_compact_hour` is).
            ("p99_r_compact", &h.r_compact),
            ("p99_r_compact_hour", &rhour),
        ]));
        v.push(("r_compact_own_n", h.r_compact_own.n.to_string()));
        for (k50, k99, x) in [
            ("own_p50", "own_p99", &h.own),
            ("replica_p50", "replica_p99", &h.replica),
            // The replica's own answers to the reads its compaction held,
            // from their send: beside `p99_r_compact`, from the schedule.
            ("r_compact_own_p50", "r_compact_own_p99", &h.r_compact_own),
        ] {
            if x.n > 0 {
                v.push((k50, format!("{:.0}", x.pct(0.5))));
                v.push((k99, format!("{:.0}", x.pct(0.99))));
            }
        }
        v
    }

    /// The end line's figures of the run's restarts and of the replica's
    /// own compactions: the operations of each, and their p99 where there
    /// were any; and the most a worker held for one.
    fn end(&self) -> Vec<(&'static str, String)> {
        let mut v: Vec<(&'static str, String)> = vec![
            ("restart_ops", self.restart.n.to_string()),
            ("restart_replica_ops", self.restart_replica.n.to_string()),
            ("restart_behind_ops", self.restart_behind.n.to_string()),
            ("r_compact_ops", self.r_compact.n.to_string()),
            ("r_compact_own_n", self.r_compact_own.n.to_string()),
            ("r_held_max", self.held_max.to_string()),
        ];
        v.extend(p99s(&[
            ("p99_restart", &self.restart),
            ("p99_restart_replica", &self.restart_replica),
            ("p99_restart_behind", &self.restart_behind),
            ("p99_r_compact", &self.r_compact),
            ("p99_r_compact_own", &self.r_compact_own),
        ]));
        v
    }
}

/// A worker's own: its latencies since the minute's line took them, its
/// log since the checker did, the start of what it has in flight, the
/// operations it ended in the minute, and when the operation it waits for
/// or works on was due (`u64::MAX` at the peak, which keeps no schedule):
/// how far the open loop is behind (`behind_s`).
struct Slot {
    hists: Mutex<Hists>,
    log: Mutex<Vec<Ev>>,
    inflight: AtomicU64,
    done: AtomicU64,
    due: AtomicU64,
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

/// The primary's closes as they come (`Shared::closed`): the run of them
/// under way -- its first and last close, how many, each within
/// `BURST_SPAN` of the first -- and, once it is a burst, how far apart its
/// `BURST_CLOSES` came, ns.
#[derive(Clone, Copy, Debug, Default)]
struct Closes {
    first: u64,
    last: u64,
    n: u32,
    spread: u64,
}

/// A restart's catch-up (`catch_up`): from its window's start, through the
/// backlog the open loop sends the primary once it answers again, until the
/// loop is back on its schedule and the replica holds the newest change
/// answered then. A read through the replica sent in it that the replica
/// held past its wait and answered 504 is the restart's, `down`
/// (`Shared::catch_504`), and the lane's schedule is moved up meanwhile
/// (`lane_due`). The dry run of 2026-11-26 counted 185 such 504s in the
/// minute after its planned kill, every error it had there: the replica was
/// back 517 ms after the server answered, at the change the kill left,
/// then some 7 000 changes behind the writes of the pool's backlog -- past
/// the 5 s a read waits -- while the lane sent its own backlog of reads,
/// 128 at a time, each waiting for the newest change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Catch {
    /// The restart's minute, and its window's start: the first send whose
    /// 504 is the restart's.
    minute: u64,
    from: u64,
    /// The server answering again, `u64::MAX` before.
    back: u64,
    /// The loop back on its schedule once reads went through the replica
    /// again, and the newest change answered then: the change the replica
    /// must hold.
    on: Option<(u64, u64)>,
    /// Its end, `u64::MAX` while it is open, and how it ended.
    to: u64,
    end: CatchEnd,
    /// The reads through the replica a 504 answered in it, and the lane's
    /// reads its moves skipped, in thousandths (`dropped_milli`).
    down: u64,
    moved_milli: u64,
    /// Whether its line was said (`catch_up`).
    said: bool,
}

/// How a restart's catch-up ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatchEnd {
    Open,
    /// The replica held the change answered as the loop came back.
    Caught,
    /// `--replica-back-max` passed after the loop came back, the replica
    /// short of that change: a 504 after it is an error.
    Bound,
    /// The next restart began first.
    Cut,
    /// The run ended first.
    RunEnd,
}

impl Catch {
    fn new(minute: u64, from: u64) -> Catch {
        Catch {
            minute,
            from,
            back: u64::MAX,
            on: None,
            to: u64::MAX,
            end: CatchEnd::Open,
            down: 0,
            moved_milli: 0,
            said: false,
        }
    }

    /// At `now`, the loop `behind` its schedule (`behind_s`, `None` at the
    /// peak, which keeps none) and reads going through the replica again
    /// or not (`through`): the first look on its schedule with them going
    /// through takes the newest change answered (`newest`) as the one the
    /// replica must hold. Before reads go through it again its backlog is
    /// no reader's, and the loop may be on its schedule within the grace,
    /// the pool's writes still landing on the replica.
    fn on_schedule(&mut self, now: u64, behind: Option<f64>, through: bool, newest: u64) {
        if self.to == u64::MAX
            && self.on.is_none()
            && through
            && behind.is_none_or(|b| b <= ON_SCHEDULE_S)
        {
            self.on = Some((now, newest));
        }
    }

    /// The replica read holding the change `held` at `now` (`None` where
    /// it could not be read): whether that ends the catch-up -- the change
    /// the loop's return named held, or `bound` (`--replica-back-max`)
    /// past that return without it.
    fn settle(&mut self, now: u64, held: Option<u64>, bound: u64) -> bool {
        let Some((at, target)) = self.on else {
            return false;
        };
        if self.to != u64::MAX {
            return false;
        }
        self.end = match held.is_some_and(|h| h >= target) {
            true => CatchEnd::Caught,
            false if now.saturating_sub(at) >= bound => CatchEnd::Bound,
            false => return false,
        };
        self.to = now;
        true
    }
}

/// The VM's clock as a probe read it (`clock_probe`, `CLOCK_PROBE`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Probe {
    /// The VM's clock less this host's wall clock, ms, from the middle of
    /// the request: the minute's `clock_off_ms`, and what a step is told
    /// by (`clock_stepped`).
    off_ms: f64,
    /// The request's round trip, ms and ns: what bounds the offsets' error.
    rtt_ms: f64,
    rtt_ns: u64,
    /// The VM's clock, unix ns, less this process's (`Shared::ns`) at the
    /// middle of the request, and that middle: what Docker's times are
    /// read on this process's clock by (`Shared::vm_ns`).
    vm_less_ns: i64,
    at: u64,
}

impl Probe {
    /// The VM's `now()`, `t_ms`, read between `wall` (ms, this host's wall
    /// clock) and `ns` (this process's clock) before and after.
    fn read(t_ms: i64, wall: (f64, f64), ns: (u64, u64)) -> Probe {
        let mid = ns.0 + (ns.1 - ns.0) / 2;
        Probe {
            off_ms: t_ms as f64 - (wall.0 + wall.1) / 2.0,
            rtt_ms: wall.1 - wall.0,
            rtt_ns: ns.1 - ns.0,
            vm_less_ns: (t_ms as i128 * 1_000_000 - mid as i128) as i64,
            at: mid,
        }
    }
}

/// What the workers, the subscribers and the minute's look share.
struct Shared {
    t0: Instant,
    stop: AtomicBool,
    addr: String,
    raddr: String,
    rate: f64,
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
    /// The latest compaction the replica ran on its own, as its
    /// compaction thread's watch saw it (`watch_replica`): its number, from
    /// 1, and its window (`to` `u64::MAX` while it is open, both while it
    /// was found to be none); and each one's verdict as it ended, its
    /// number's less one -- whether its counter moved in it (or the watch's
    /// word, where its end was not seen or the counter could not be read).
    r_window: Mutex<(u64, u64, u64)>,
    r_verdicts: Mutex<Vec<bool>>,
    /// When the watch saw the latest window begin, `u64::MAX` once it saw
    /// it end or cut it: what its length is held to its bound by
    /// (`bound_r_window`), whether or not its reads' window was settled
    /// before -- the watch's compaction open while it is not `u64::MAX`;
    /// and the numbers of the latest found open past its bound and of the
    /// latest its watch lost, each counted once.
    r_since: AtomicU64,
    r_overlong: AtomicU64,
    r_lost: AtomicU64,
    /// Whether the watch is looking at that thread now, and since when it
    /// is not (`u64::MAX` while it is); and since the minute's line took
    /// them, the compactions whose end it saw, the longest of them in ms,
    /// those whose end it did not see (`Edge::Cut`, `Edge::Lost`), those
    /// it lost while they were open, the windows its counter did not move
    /// in, those found open past their bound, and those its counter judged
    /// across a reset (the replica started again in it), on a base read
    /// before the window began (`stale`), or not at all (`unread`).
    r_watched: AtomicBool,
    r_down_since: AtomicU64,
    r_compacts_seen: AtomicU64,
    r_compact_ms: AtomicU64,
    r_compacts_cut: AtomicU64,
    r_compacts_lost: AtomicU64,
    r_compacts_false: AtomicU64,
    r_compacts_overlong: AtomicU64,
    r_compacts_reset: AtomicU64,
    r_compacts_stale: AtomicU64,
    r_compacts_unread: AtomicU64,
    /// The latest restart's span as the reads through the replica see it
    /// -- from its kill's announcement until they go through it again --
    /// and the latest window of the replica's seen to end as a compaction,
    /// each `(from, to)` in ns once it is over (`restart_met`); and since
    /// the minute's line took it, the longest such window a restart met,
    /// its length and the restart's span together, in ms
    /// (`r_compact_kill_ms`): what its reads are bounded by.
    r_met: Mutex<(Option<Span>, Option<Span>)>,
    r_kill_ms: AtomicU64,
    /// The longest a compaction of the replica's can plausibly take at the
    /// latest look (`plausible_ms`), from the replica's file then and the
    /// slowest of the primary's compacts on the run, a GB of the primary's
    /// file at the look before it (`p_file`).
    r_bound_ms: AtomicU64,
    p_file: AtomicU64,
    p_compact_ms_per_gb: AtomicU64,
    /// Whether the open loop was behind its schedule (`ON_SCHEDULE_S`)
    /// as the latest kill was announced: its operations then wait out the
    /// backlog before the restart, and are counted apart from those of a
    /// kill the loop met on its schedule (`Hists::restart_behind`).
    restart_behind: AtomicBool,
    /// The open loop's next operation: the schedule is one, the `n`th
    /// operation due `n / rate` seconds from `t0`, and each connection
    /// takes the next as it is free (`work`). A schedule a connection
    /// queued each operation behind its own connection's: the third dry
    /// run of 2026-11-24, at 2 093 a second on 32 connections, had a
    /// median operation 135 ms late in its first minute and a p99 of 1.4
    /// to 4.7 s on its schedule through its eleventh, the peak's own p99
    /// 227 ms on the same file -- 4% of the mix are reads through the
    /// replica, which wait out the primary's next sync (`Fennec-After`
    /// under `--sync 250`), and every operation of their connection
    /// waited behind them while the others stood idle.
    next: AtomicU64,
    /// The latest change a write was answered as: what a read through the
    /// replica is sent after.
    max_seq: AtomicU64,
    /// The primary's lives: turned as a killed or dead server, gone and
    /// its file verified, is started again (`restart`). A kill takes the
    /// writes answered from its buffer (`--loss-ms`), and the server started
    /// again gives their change numbers to the writes after it: the checker
    /// knows a change by its life and number (`in_life`), the life of the
    /// connection it came on, read as that connection was first answered
    /// -- no server answers between the death and the start, and a reader
    /// waits on its first answer. The short run of 2026-11-22 held a stream's change from
    /// before a kill to the write that took its number after it -- a read
    /// spanning back in time, "field9 holds no value written to it" -- and
    /// counted 7 writes after the kill missing from the events before it.
    life: AtomicU64,
    /// Whether reads may go through the replica: not from a kill until the
    /// replica says it is connected again and past the newest change
    /// answered before it (`restart`) -- before that it waits out the
    /// primary's absence and its own reconnect, and answers 504.
    replica_ok: AtomicBool,
    /// The change the replica must hold before reads go through it again.
    replica_target: AtomicU64,
    /// When the side thread's compact was sent, `u64::MAX` while none is
    /// out: what a kill meant for a compact waits for.
    compact_since: AtomicU64,
    /// The side thread's job, for a wait on it that runs out to name.
    doing: Mutex<String>,
    /// Inserted keys: the next to hand out, and those inserted and not yet
    /// deleted, oldest first.
    fifo: Mutex<(u64, VecDeque<u64>)>,
    /// Sessions inserted, with when, kept past their time for reads.
    sessions: Mutex<VecDeque<(u64, u64)>>,
    next_session: AtomicU64,
    /// Docs written: id and span, for the minute's sample to leave out --
    /// each from before its request is sent (its end `u64::MAX` until it
    /// is answered): a write lands, and is read, before its answer.
    doc_writes: Mutex<Vec<(u64, u64, u64)>>,
    slots: Vec<Slot>,
    readers: Vec<Reader>,
    streams: Vec<Stream>,
    notes: Mutex<Vec<String>>,
    /// Why the run must stop now, the harness's own setting at fault -- a
    /// server that refused the run's token or its name, a backup refused
    /// so -- which every minute after would meet again: the first said,
    /// and the minute's loop ends the run on it (`stop_run`).
    fault: Mutex<Option<String>>,
    /// The replication token, which `/_replication/status` asks: read on
    /// both servers as a read through the replica answers 504
    /// (`replication_said`).
    repl_token: std::sync::OnceLock<String>,
    /// The first slot of the replica lane (`--replica-threads`): the slots
    /// before it are the pool's, the rest the lane's, none where the reads
    /// through the replica share the pool. The lane's schedule is its own:
    /// the next one due, in ns from `t0`, and the time between two, which at
    /// the peak follows the pool's answers each second (`seconds`) -- the
    /// lane's share of what the pool did -- and the pool's operations ended,
    /// which it follows.
    lane: usize,
    r_due: AtomicU64,
    r_every: AtomicU64,
    pool_ops: AtomicU64,
    /// The lane's reads answered since the minute's line took them, and,
    /// at the peak, those its schedule dropped -- in thousandths of a
    /// read: a lane more than a second behind is moved up to a second
    /// behind, the reads it skips never sent (`work`).
    lane_ops: AtomicU64,
    lane_dropped_milli: AtomicU64,
    /// `t0` on the wall clock, unix ns: what a time of the run's is named
    /// by on a line (`utc`), never what Docker's are read by -- the wall
    /// clock is slewed, `t0`'s `Instant` is not.
    t0_wall: u64,
    /// The VM's clock less this process's (`ns`) at the probe kept
    /// (`Probe`, `clock`), ns, that probe's round trip and when it was
    /// taken (0 before the first): what
    /// Docker's times, the VM's, are read on this process's clock by
    /// (`vm_ns`), and the error that reading carries (`clock_err`).
    clock_vm: std::sync::atomic::AtomicI64,
    clock_rtt: AtomicU64,
    clock_at: AtomicU64,
    /// The latest a primary's connection was read answering -- a worker's
    /// answer, a stream's event -- and the first close of the burst of
    /// closes since (`u64::MAX` while none): a death's time (`death_time`).
    /// The closes as they come, under way to a burst or not
    /// (`Shared::closed`).
    answered: AtomicU64,
    closed: AtomicU64,
    closes: Mutex<Closes>,
    /// When each connection that failed outside a restart's window failed
    /// since the minute's line took the errors: those from a death on are
    /// its, `down` and not errors, once it is found (`death_errors`).
    cut_errors: Mutex<Vec<u64>>,
    /// The deaths found, by their time: what a side job cut by one asks
    /// (`cut_by`).
    deaths: Mutex<Vec<u64>>,
    /// When the last kill the schedule sent was sent (0 before any): a
    /// compact cut after it is the kill's, one cut before it -- in the
    /// announced second, the server dead before the kill -- the death's.
    /// The restart's window cannot tell them: the announcement opens it.
    /// Stored before `docker kill` is sent, so that a job its cut ends
    /// finds it; and a death in the moment before the send, which Docker's
    /// inspect showed running, is found only after Docker's wait
    /// (`gone_before`): until the kill sent at `kill_sent` is judged
    /// (`kill_judged`, its time once judged either way; `kill_death`, its
    /// time once judged a death's, stored first), a job it cut waits to
    /// say whose (`cut_by`) -- a death's timed before the job was asked
    /// among them, which the deaths found cannot name.
    kill_sent: AtomicU64,
    kill_judged: AtomicU64,
    kill_death: AtomicU64,
    /// Each restart's catch-up (`Catch`), the latest last, and whether
    /// the latest is open: the lane's schedule is moved up while it is
    /// (`lane_due`).
    catches: Mutex<Vec<Catch>>,
    catching: AtomicBool,
    /// Each server's cgroup a second (`watch_memory`): the primary's, then
    /// the replica's.
    mem: [Mutex<Mem>; 2],
    /// The open loop's lag each second (`seconds`).
    behind: Mutex<Behind>,
}

impl Shared {
    fn new(o: &Opts, addr: String, raddr: String, ranges: &[(u64, u64)]) -> Shared {
        let t0 = Instant::now();
        let t0_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        // The lane's share of the rate; at the peak a first guess until the
        // pool's first second says.
        let r_rate = match o.rate > 0.0 {
            true => o.rate * REPLICA_SHARE,
            false => 100.0,
        };
        Shared {
            t0,
            stop: AtomicBool::new(false),
            addr,
            raddr,
            rate: o.rate,
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
            r_window: Mutex::new((0, u64::MAX, u64::MAX)),
            r_verdicts: Mutex::new(Vec::new()),
            r_since: AtomicU64::new(u64::MAX),
            r_overlong: AtomicU64::new(0),
            r_lost: AtomicU64::new(0),
            r_watched: AtomicBool::new(false),
            r_down_since: AtomicU64::new(0),
            r_compacts_seen: AtomicU64::new(0),
            r_compact_ms: AtomicU64::new(0),
            r_compacts_cut: AtomicU64::new(0),
            r_compacts_lost: AtomicU64::new(0),
            r_compacts_false: AtomicU64::new(0),
            r_compacts_overlong: AtomicU64::new(0),
            r_compacts_reset: AtomicU64::new(0),
            r_compacts_stale: AtomicU64::new(0),
            r_compacts_unread: AtomicU64::new(0),
            r_met: Mutex::new((None, None)),
            r_kill_ms: AtomicU64::new(0),
            r_bound_ms: AtomicU64::new(R_COMPACT_FLOOR_MS),
            p_file: AtomicU64::new(0),
            p_compact_ms_per_gb: AtomicU64::new(0),
            restart_behind: AtomicBool::new(false),
            next: AtomicU64::new(0),
            max_seq: AtomicU64::new(0),
            life: AtomicU64::new(0),
            replica_ok: AtomicBool::new(true),
            replica_target: AtomicU64::new(0),
            compact_since: AtomicU64::new(u64::MAX),
            doing: Mutex::new(String::new()),
            fifo: Mutex::new((o.records, VecDeque::new())),
            sessions: Mutex::new(VecDeque::new()),
            next_session: AtomicU64::new(1),
            doc_writes: Mutex::new(Vec::new()),
            slots: (0..o.threads + o.replica_threads)
                .map(|_| Slot {
                    hists: Mutex::new(Hists::new()),
                    log: Mutex::new(Vec::new()),
                    inflight: AtomicU64::new(u64::MAX),
                    done: AtomicU64::new(0),
                    due: AtomicU64::new(u64::MAX),
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
            fault: Mutex::new(None),
            repl_token: std::sync::OnceLock::new(),
            lane: o.threads,
            r_due: AtomicU64::new(0),
            r_every: AtomicU64::new((1e9 / r_rate) as u64),
            pool_ops: AtomicU64::new(0),
            lane_ops: AtomicU64::new(0),
            lane_dropped_milli: AtomicU64::new(0),
            t0_wall,
            // Until the first probe, the wall clock at `t0`: off by the
            // drift since, which `clock_err` counts.
            clock_vm: std::sync::atomic::AtomicI64::new(t0_wall as i64),
            clock_rtt: AtomicU64::new(0),
            clock_at: AtomicU64::new(0),
            answered: AtomicU64::new(0),
            closed: AtomicU64::new(u64::MAX),
            closes: Mutex::new(Closes::default()),
            cut_errors: Mutex::new(Vec::new()),
            kill_sent: AtomicU64::new(0),
            kill_judged: AtomicU64::new(0),
            kill_death: AtomicU64::new(0),
            catches: Mutex::new(Vec::new()),
            catching: AtomicBool::new(false),
            deaths: Mutex::new(Vec::new()),
            mem: [Mutex::new(Mem::default()), Mutex::new(Mem::default())],
            behind: Mutex::new(Behind::default()),
        }
    }

    /// The pool's slots, and the replica lane's.
    fn pool_slots(&self) -> &[Slot] {
        &self.slots[..self.lane]
    }
    fn lane_slots(&self) -> &[Slot] {
        &self.slots[self.lane..]
    }

    /// A primary's connection read answering at `t`.
    fn answered(&self, t: u64) {
        self.answered.fetch_max(t, Ordering::SeqCst);
    }

    /// A primary's connection found closed -- cut, refused, a stream ended
    /// -- at `t`. A death is told from a close that was no death's by how
    /// many close together: `BURST_CLOSES` within `BURST_SPAN` of the
    /// first of them are its exit's, and that first close is kept
    /// (`closed`, `death_time`) while the server is not read answering
    /// past it by more than `CLOSE_SLOP`; a close alone, or closes spread
    /// wider, are each one connection's, and time nothing. A burst, or a
    /// run of closes toward one, the server was read answering past by
    /// more than the slop is let go -- it was no death's -- and the closes
    /// from this one counted afresh.
    fn closed(&self, t: u64) {
        let a = self.answered.load(Ordering::SeqCst);
        let mut run = self.closes.lock().unwrap();
        let burst = self.closed.load(Ordering::SeqCst);
        if burst != u64::MAX {
            if a <= burst.saturating_add(CLOSE_SLOP) {
                return;
            }
            self.closed.store(u64::MAX, Ordering::SeqCst);
        }
        // Each close's time is taken before this lock, on its own thread:
        // the run's first is the earliest, whichever came in first.
        let joins = run.n > 0
            && t.abs_diff(run.first) <= BURST_SPAN
            && a <= run.first.saturating_add(CLOSE_SLOP);
        match joins {
            true => {
                run.n += 1;
                run.first = run.first.min(t);
                run.last = run.last.max(t);
            }
            false => {
                *run = Closes {
                    first: t,
                    last: t,
                    n: 1,
                    spread: 0,
                }
            }
        }
        if run.n >= BURST_CLOSES {
            run.spread = run.last - run.first;
            self.closed.store(run.first, Ordering::SeqCst);
        }
    }

    /// The death's burst of closes (`closed`) -- `u64::MAX` where none
    /// was read, or the server was read answering past it by more than
    /// `CLOSE_SLOP` -- and how far apart its `BURST_CLOSES` came, ns.
    fn burst(&self) -> (u64, u64) {
        let run = self.closes.lock().unwrap();
        let c = self.closed.load(Ordering::SeqCst);
        let a = self.answered.load(Ordering::SeqCst);
        match c != u64::MAX && a <= c.saturating_add(CLOSE_SLOP) {
            true => (c, run.spread),
            false => (u64::MAX, 0),
        }
    }

    /// The server answering again at `t` after a restart: its life's
    /// answers and closes counted from here. Without it a life that died
    /// before a worker answered was timed by the last death's burst, or by
    /// the refused connects of its restart's, and every failure since
    /// moved to `down`; and its kill judged by the last life's last answer.
    fn back(&self, t: u64) {
        let mut run = self.closes.lock().unwrap();
        *run = Closes::default();
        self.closed.store(u64::MAX, Ordering::SeqCst);
        self.answered.store(t, Ordering::SeqCst);
    }

    /// The VM's clock read by a probe: from here Docker's times are read
    /// on this process's clock by it (`vm_ns`) -- unless the probe kept
    /// reads them closer, its error now (`clock_err`) under this one's,
    /// and the two agree within both errors. The minute's probe waits
    /// behind whatever the server is answering: after a restart, behind
    /// the loop's backlog, its round trip seconds where the last took a
    /// few ms. Two that disagree -- the VM's clock stepped, or drifted
    /// past the bound -- take the new one. Whether it was taken.
    fn clock(&self, p: &Probe) -> bool {
        let at = self.clock_at.load(Ordering::SeqCst);
        if at > 0 {
            let kept = self.clock_err(p.at);
            let new = p.rtt_ns + 1_000_000;
            let off = self.clock_vm.load(Ordering::SeqCst);
            let agree = (p.vm_less_ns as i128 - off as i128).unsigned_abs() <= (kept + new) as u128;
            if agree && kept < new {
                return false;
            }
        }
        self.clock_vm.store(p.vm_less_ns, Ordering::SeqCst);
        self.clock_rtt.store(p.rtt_ns, Ordering::SeqCst);
        self.clock_at.store(p.at, Ordering::SeqCst);
        true
    }

    /// A time Docker gives, unix ns on the VM's clock, in ns from `t0` on
    /// this process's, by the kept probe's offset against this process's
    /// clock itself (`ns`) -- never through the wall clock at `t0`, which
    /// parts from it by tens of ms an hour; `None` before `t0`.
    fn vm_ns(&self, vm: u64) -> Option<u64> {
        let off = self.clock_vm.load(Ordering::SeqCst);
        u64::try_from(vm as i128 - off as i128).ok()
    }

    /// How far a time read through `vm_ns` at `t` may be off: the kept
    /// probe's round trip (`clock`), the ms `now()` is read to, and the
    /// drift the two clocks may have taken since (`CLOCK_DRIFT_PPM`).
    fn clock_err(&self, t: u64) -> u64 {
        let since = t.saturating_sub(self.clock_at.load(Ordering::SeqCst));
        self.clock_rtt.load(Ordering::SeqCst)
            + 1_000_000
            + (since as u128 * CLOCK_DRIFT_PPM as u128 / 1_000_000) as u64
    }

    /// The failures counted as errors from `at` on -- a death's, cut by it
    /// before the run knew -- moved to `down`; how many.
    fn death_errors(&self, at: u64) -> u64 {
        let mut v = self.cut_errors.lock().unwrap();
        let n = v.iter().filter(|&&t| t >= at).count() as u64;
        v.retain(|&t| t < at);
        let _ = self
            .errors
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |e| {
                Some(e.saturating_sub(n))
            });
        self.down.fetch_add(n, Ordering::SeqCst);
        n
    }

    /// The minute's errors and `down`, taken for its line.
    fn take_errors(&self) -> (u64, u64) {
        let mut v = self.cut_errors.lock().unwrap();
        v.clear();
        (
            self.errors.swap(0, Ordering::SeqCst),
            self.down.swap(0, Ordering::SeqCst),
        )
    }

    /// Both servers' `/_replication/status` as they are now: the replica's
    /// connection, change, lag, images and reconnects, the primary's change
    /// and durable one -- said beside a read the replica answered 504, the
    /// second dry run of 2026-11-23's 40 errors, which nothing of the run
    /// could say the cause of.
    fn replication_said(&self) -> String {
        let Some(token) = self.repl_token.get() else {
            return String::new();
        };
        let auth = format!("Authorization: Bearer {token}\r\n");
        let look = |addr: &str| -> String {
            let Ok(mut c) = http::Http::try_connect(addr, Duration::from_secs(5)) else {
                return "no connection".into();
            };
            match c.try_request_with("GET", "/_replication/status", "text/plain", &auth, b"") {
                Ok((200, body)) => {
                    let t = String::from_utf8_lossy(body);
                    t.chars().take(400).collect()
                }
                Ok((status, _)) => format!("{status}"),
                Err(e) => e.to_string(),
            }
        };
        format!(
            "; the primary then {}; the replica {}",
            look(&self.addr),
            look(&self.raddr)
        )
    }

    /// The run stops at the minute's end for `why`, the first fault said.
    fn fault(&self, why: String) {
        let mut f = self.fault.lock().unwrap_or_else(|e| e.into_inner());
        if f.is_none() {
            say(format!("the harness's fault, the run stops: {why}"));
            *f = Some(why);
        }
    }

    fn faulted(&self) -> Option<String> {
        self.fault.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn ns(&self) -> u64 {
        self.t0.elapsed().as_nanos() as u64
    }

    /// Whether the replica's compaction window `id` was one, as its end
    /// settled it (`replica_compaction`); `None` while it is open.
    fn r_verdict(&self, id: u64) -> Option<bool> {
        let i = id.checked_sub(1)? as usize;
        self.r_verdicts.lock().unwrap().get(i).copied()
    }

    /// The watch looking at the replica's compaction thread or not, as of
    /// now: since when it is not, for `bound_r_window`. Whether it was.
    fn watched(&self, on: bool) -> bool {
        let was = self.r_watched.swap(on, Ordering::SeqCst);
        if was != on {
            let since = if on { u64::MAX } else { self.ns() };
            self.r_down_since.store(since, Ordering::SeqCst);
        }
        was
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
            say(what.clone());
            n.push(what);
        }
    }

    /// When a read sent after the latest write answered begins, and the
    /// change it is sent after: its start taken first, so that every write
    /// answered before the read began is in the change it names (a write
    /// notes its change before it takes its end). `load` reads the change.
    fn sent_after(&self, load: impl FnOnce() -> u64) -> (u64, u64) {
        let s = self.ns();
        (s, load())
    }

    /// A doc's write about to be sent: in the sample's leave-out from now,
    /// its end unknown until `doc_ended`.
    fn doc_begun(&self, id: u64) -> u64 {
        let s = self.ns();
        self.doc_writes.lock().unwrap().push((id, s, u64::MAX));
        s
    }

    fn doc_ended(&self, id: u64, s: u64) {
        let e = self.ns();
        let mut w = self.doc_writes.lock().unwrap();
        if let Some(x) = w
            .iter_mut()
            .rev()
            .find(|x| x.0 == id && x.1 == s && x.2 == u64::MAX)
        {
            x.2 = e;
        }
    }

    /// Counts what met a restart -- a connection refused or cut, a 503
    /// saying when to come back -- inside a restart's window as `down`, and
    /// anything else, or the same outside one, as an error.
    fn failed(&self, t: u64, restart_kind: bool, what: impl FnOnce() -> String) {
        if restart_kind && self.restarting(t) {
            self.down.fetch_add(1, Ordering::Relaxed);
        } else {
            // Under the lock the minute's line takes the errors with, so a
            // death found later moves no more than were counted.
            {
                let mut v = self.cut_errors.lock().unwrap();
                if restart_kind && v.len() < 100_000 {
                    v.push(t);
                }
                self.errors.fetch_add(1, Ordering::SeqCst);
            }
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

    /// The catch-up of the restart of minute `m` opened, from its window's
    /// start: one still open is cut by it, its own 504s counted to it.
    fn catch_open(&self, m: u64, from: u64) {
        let mut c = self.catches.lock().unwrap();
        if let Some(x) = c.last_mut().filter(|x| x.to == u64::MAX) {
            x.to = from;
            x.end = CatchEnd::Cut;
        }
        c.push(Catch::new(m, from));
        self.catching.store(true, Ordering::SeqCst);
    }

    /// The server answering again at `back`, for the catch-up's line.
    fn catch_back(&self, back: u64) {
        if let Some(x) = self.catches.lock().unwrap().last_mut() {
            x.back = back;
        }
    }

    /// A read through the replica sent at `s` that the replica held past
    /// its wait and answered 504: the restart's where `s` falls in a
    /// catch-up (`Catch`) -- counted `down` and to it -- and an error
    /// otherwise, the replica caught up or no restart behind it. By its
    /// send, not its answer: the replica answers a 504 the full wait after
    /// it, and those of reads sent as the catch-up ended come after its end.
    fn catch_504(&self, s: u64) -> bool {
        let mut c = self.catches.lock().unwrap();
        match c.iter_mut().rev().find(|x| s >= x.from && s <= x.to) {
            Some(x) => {
                x.down += 1;
                self.down.fetch_add(1, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// The lane's reads a move of its schedule skipped in an open
    /// catch-up, in thousandths of a read.
    fn catch_moved(&self, milli: u64) {
        if let Some(x) = self
            .catches
            .lock()
            .unwrap()
            .last_mut()
            .filter(|x| x.to == u64::MAX)
        {
            x.moved_milli += milli;
        }
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
    /// The primary's life `c` answers in (`Shared::life`): read at its
    /// first answer, `None` before it.
    life: Option<u64>,
    rc: Option<http::Http>,
    rng: Rng,
    /// Whether the operation under way was a read through the replica: its
    /// class and its restart's part are the replica's (`class`).
    via_replica: bool,
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
            // The request's send: what tells a replica's 504 of a
            // restart's catch-up from one after it (`catch_504`).
            let sent = self.sh.ns();
            // A connect's own failure, kept: one refused is a close, one
            // that timed out is not (`a_close`) -- the server may be
            // alive and slow, and a close taken for its death's time.
            let mut unconnected = None;
            if c.is_none() {
                match http::Http::try_connect(addr, Duration::from_secs(60)) {
                    Ok(x) => *c = Some(x),
                    Err(e) => unconnected = Some(e),
                }
                if !replica {
                    self.life = None;
                }
            }
            let got = match c.as_mut() {
                Some(c) => match c.try_query_with(text, params, headers) {
                    Ok((status, body)) => {
                        let body = body.to_vec();
                        Ok((status, body, c.seq(), c.retry_after()))
                    }
                    Err(e) => Err(e),
                },
                None => Err(unconnected.unwrap_or_else(|| std::io::Error::other("no connection"))),
            };
            let t = self.sh.ns();
            // A connection's server is the one that answered it first,
            // whose life is the one turning at that answer -- not at the
            // connect: Docker's forwarded port takes a connection before
            // the server behind it is there to answer.
            if !replica && got.is_ok() && self.life.is_none() {
                self.life = Some(self.sh.life.load(Ordering::SeqCst));
            }
            // What a death's time is read from (`death_time`): the
            // primary answering, and its connections closing.
            if !replica {
                match &got {
                    Ok(_) => self.sh.answered(t),
                    Err(e) if a_close(e) => self.sh.closed(t),
                    Err(_) => {}
                }
            }
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
                    let said = || {
                        format!(
                            "{text}: {status} {}",
                            String::from_utf8_lossy(&body[..body.len().min(200)])
                        )
                    };
                    // Nothing the mix sends is refused for its token or its
                    // address by a server the run set up right: the run's
                    // own setting is, at every request after.
                    if status == 401 || status == 403 {
                        self.sh.fault(format!(
                            "the {} refused {}",
                            if replica { "replica" } else { "primary" },
                            said()
                        ));
                    }
                    // A read the replica held past its wait while it caught
                    // up after a restart -- the writes of the loop's
                    // backlog ahead of it -- is the restart's.
                    if replica && status == 504 && self.sh.catch_504(sent) {
                        return Answer::Refused(status);
                    }
                    // Any other: what each server said of replication as
                    // it answered.
                    let sh = self.sh;
                    self.sh.failed(t, false, || match replica && status == 504 {
                        true => format!("{}{}", said(), sh.replication_said()),
                        false => said(),
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
                    // Its first answer, the server's life with it: kept
                    // from the connection before, every write after the
                    // first kill was the life before's (4 148 and 4 464
                    // events counted a key sent twice, in two runs).
                    self.life = Some(self.sh.life.load(Ordering::SeqCst));
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
        let sh = self.sh;
        self.via_replica = replica;
        let (s, after) = sh.sent_after(|| sh.max_seq.load(Ordering::SeqCst));
        let headers = match replica {
            true => format!("Fennec-After: {after}\r\n"),
            false => String::new(),
        };
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
        if replica {
            self.slot.hists.lock().unwrap().replica.record(e - s);
        }
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
            replica,
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
                // The checker's change: the server's, of the life that
                // answered it.
                let life = self.life.unwrap_or(0);
                Some((false, affected(&body), seq.map(|q| in_life(life, q))))
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

    /// A read of a record through the replica, the mix's 4%.
    fn replica_read(&mut self, zipf: &mut Scrambled, lane: bool) -> bool {
        let key = zipf.next(&mut self.rng, self.sh.records) + 1;
        // Not across a restart: the replica would wait out the primary's
        // absence for the write it is sent after, and its own reconnect
        // after it. To the primary meanwhile.
        let replica =
            self.sh.replica_ok.load(Ordering::SeqCst) && !self.sh.restarting(self.sh.ns());
        // The lane's connection to the primary is for a restart alone:
        // let go once the replica is read again, not held idle for the
        // half hour to the next, where the server's idle bound closes it.
        if lane && replica {
            self.c = None;
        }
        self.read(key, replica)
    }

    /// One operation of the mix; `lanes`, the reads through the replica on
    /// a lane of their own (`--replica-threads`): the pool's mix is then
    /// the rest of it, each operation's share as before.
    fn op(&mut self, value: &mut String, zipf: &mut Scrambled, lanes: bool) -> bool {
        let u = pool_u(self.rng.f64(), lanes);
        let records = self.sh.records;
        match u {
            u if u < 0.36 => {
                let key = zipf.next(&mut self.rng, records) + 1;
                self.read(key, false)
            }
            u if u < 0.40 => self.replica_read(zipf, false),
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
                self.doc_write(id, &params)
            }
        }
    }

    /// A doc updated: in the minute's sample's leave-out from before it is
    /// sent until it is answered, answered or not -- under `--sync always`
    /// it is visible from when it lands, before its fsync and its answer.
    fn doc_write(&mut self, id: u64, params: &str) -> bool {
        let s = self.sh.doc_begun(id);
        let done = self.written("set docs {body: $2, v: $3} where id = $1", params);
        self.sh.doc_ended(id, s);
        matches!(done, Some((false, ..)))
    }
}

/// The pool's draw `r` in [0, 1) as a point of the mix (`Worker::op`):
/// itself where the reads through the replica share the pool, and where
/// they have a lane of their own the rest of the mix, each operation's
/// share of the pool's in the proportion it was -- [0.36, 0.40), the
/// replica's, never drawn.
fn pool_u(r: f64, lanes: bool) -> f64 {
    match lanes {
        false => r,
        true => {
            let u = r * (1.0 - REPLICA_SHARE);
            // Past the replica's share from its end, not by adding it:
            // 0.36 + 0.04 is 0.39999999999999997 in an f64, the replica's.
            if u < 0.36 {
                u
            } else {
                (u - 0.36) + 0.40
            }
        }
    }
}

/// Whether a request's failure is its connection gone -- closed, cut or
/// refused -- rather than a wait that ran out: what a death does to every
/// connection at once. A connect that timed out is no close: the server
/// may be alive and slow, and the close would be taken for a death's time
/// (`Shared::closed`).
fn a_close(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::UnexpectedEof
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::ConnectionRefused
            | K::BrokenPipe
            | K::NotConnected
    )
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
        life: None,
        rc: None,
        rng: Rng::new(SEED ^ 0x534F_414B ^ (w as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        via_replica: false,
    };
    let mut zipf = Scrambled::new();
    let mut value = String::with_capacity(super::FIELD_LEN);
    // The replica lane: the reads through the replica on connections and
    // a schedule of their own, so that a replica holding its reads -- its
    // own compaction, dry run 6's -- holds them and not the pool every
    // other operation shares. In one pool, each such read waits out the
    // primary's next sync (`Fennec-After`, ~200 ms): 4% of the operations
    // held some 17 of the 32 connections all the time, and a slowdown of
    // 10-15% for 30 s queued every operation for seconds (dry run 6's
    // `p99_r_compact` of 5.5 s, the replica answering in 0.44-0.72 s).
    let lanes = sh.lane < sh.slots.len();
    let in_lane = w >= sh.lane;
    // Open loop: one schedule, an operation every `every`, each connection
    // taking the next as it is free (`Shared::next`) -- an operation waits
    // for a connection only while every one is busy, never behind its own
    // connection's slow one. The pool's rate is the run's less the lane's.
    let pool_rate = match lanes {
        true => sh.rate * (1.0 - REPLICA_SHARE),
        false => sh.rate,
    };
    let every = (sh.rate > 0.0).then(|| Duration::from_secs_f64(1.0 / pool_rate));
    let window = |from: &AtomicU64, to: &AtomicU64| {
        (from.load(Ordering::Relaxed), to.load(Ordering::Relaxed))
    };
    let verdict = |id: u64| sh.r_verdict(id);
    let mut classes = Classes::default();
    while !sh.stop.load(Ordering::Relaxed) {
        let (due, on_time) = match (in_lane, every) {
            (true, _) => {
                let d = lane_due(sh, sh.ns());
                slot.due.store(d, Ordering::Relaxed);
                let due = sh.t0 + Duration::from_nanos(d);
                let now = Instant::now();
                std::thread::sleep(due.saturating_duration_since(now));
                (due, due > now)
            }
            (false, Some(e)) => {
                let n = sh.next.fetch_add(1, Ordering::Relaxed);
                let due = sh.t0 + e.mul_f64(n as f64);
                slot.due.store(
                    due.saturating_duration_since(sh.t0).as_nanos() as u64,
                    Ordering::Relaxed,
                );
                let now = Instant::now();
                std::thread::sleep(due.saturating_duration_since(now));
                (due, due > now)
            }
            (false, None) => (Instant::now(), true),
        };
        // Every write that began before this one is logged before the
        // checker reads past it (`safe`).
        let sent = Instant::now();
        slot.inflight.store(sh.ns(), Ordering::SeqCst);
        me.via_replica = false;
        let ok = match in_lane {
            true => me.replica_read(&mut zipf, true),
            false => me.op(&mut value, &mut zipf, lanes),
        };
        slot.inflight.store(u64::MAX, Ordering::SeqCst);
        slot.done.fetch_add(1, Ordering::Relaxed);
        if !in_lane {
            sh.pool_ops.fetch_add(1, Ordering::Relaxed);
        } else if ok {
            sh.lane_ops.fetch_add(1, Ordering::Relaxed);
        }
        let at = due.saturating_duration_since(sh.t0).as_nanos() as u64;
        let (id, from, to) = *sh.r_window.lock().unwrap();
        let w = Windows {
            restart: window(&sh.restart_from, &sh.restart_to),
            compact: window(&sh.compact_from, &sh.compact_to),
            replica: (from, to),
        };
        let (c, held) = classes.of(at, sh.ns(), me.via_replica, on_time, &w, id, &verdict);
        if ok {
            let own = sent.elapsed().as_nanos() as u64;
            let r = Rec {
                via_replica: me.via_replica,
                behind: sh.restart_behind.load(Ordering::Relaxed),
                in_restart: at >= w.restart.0 && at <= w.restart.1,
                took: due.elapsed().as_nanos() as u64,
                own_us: (own / 1_000).min(u32::MAX as u64) as u32,
            };
            let mut h = slot.hists.lock().unwrap();
            h.own.record(own);
            if let Some(window) = h.took(c, held, r, &verdict) {
                cut_r_window(sh, window, sh.ns(), Cut::Held);
                h.settle(&verdict);
            }
            drop(h);
            sh.ops.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The replica lane's next read, due in ns from `t0`: its own schedule, a
/// read every `r_every`. At the peak it follows what the pool answers
/// (`seconds`), and one more than a second behind `now` is moved up: its
/// share of the pool's operations, never a backlog of its own that a
/// slower pool would see it burst through. So too from a restart until
/// its catch-up ends (`Shared::catching`): the reads due while the
/// primary was gone, and those the replica's 504s held the lane behind
/// by, sent at once -- 128 connections each waiting for the newest change
/// on a replica the pool's backlog of writes has put behind -- were 504s
/// in their turn. The reads a move skips are counted: dropped, never sent,
/// the lane short of its share by them, and in a catch-up said on its line.
fn lane_due(sh: &Shared, now: u64) -> u64 {
    let step = sh.r_every.load(Ordering::Relaxed);
    let catching = sh.catching.load(Ordering::Relaxed);
    if sh.rate == 0.0 || catching {
        let floor = now.saturating_sub(1_000_000_000);
        let was = sh.r_due.fetch_max(floor, Ordering::Relaxed);
        if was < floor {
            let milli = dropped_milli(floor - was, step);
            sh.lane_dropped_milli.fetch_add(milli, Ordering::Relaxed);
            if catching {
                sh.catch_moved(milli);
            }
        }
    }
    sh.r_due.fetch_add(step, Ordering::Relaxed)
}

/// The reads a move of the lane's schedule by `skipped` ns skips, one due
/// every `step` ns, in thousandths of a read: the moves are many and small,
/// a whole read's count of each would round most to none.
fn dropped_milli(skipped: u64, step: u64) -> u64 {
    (skipped as u128 * 1000 / step.max(1) as u128).min(u64::MAX as u128) as u64
}

/// An SSE `data:` line of a subscription: its change, rows and deletions.
/// A change as the checker knows it: the primary's life (`Shared::life`)
/// above the server's own number, which a server started after a kill
/// gives out again from where its file ends -- 2^40 changes a life, some
/// 300 years at the soak's rates.
fn in_life(life: u64, seq: u64) -> u64 {
    (life << 40) | seq
}

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
        let mut life = 0;
        let opened = (|| -> std::io::Result<std::net::TcpStream> {
            let to = sh.addr.parse().map_err(std::io::Error::other)?;
            let mut s = std::net::TcpStream::connect_timeout(&to, Duration::from_secs(10))?;
            // The stop is looked at between reads.
            s.set_read_timeout(Some(Duration::from_secs(2)))?;
            write!(
                s,
                "GET /usertable/changes?where={query} HTTP/1.1\r\nHost: localhost\r\n{}\r\n",
                http::auth()
            )?;
            Ok(s)
        })();
        let mut ended = String::from("refused");
        // Its connection closed or refused, rather than answered with a
        // status: what a death's time is read from (`death_time`). A
        // connect that timed out is none (`a_close`).
        let mut gone = matches!(&opened, Err(e) if a_close(e));
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
                        gone = true;
                        break;
                    }
                    Ok(_) if head => {
                        sh.answered(sh.ns());
                        // Its server's life, the one turning as it answers
                        // (`Worker::life`).
                        life = sh.life.load(Ordering::SeqCst);
                        if !line.contains(" 200") {
                            ended = line.trim().to_string();
                            if line.contains(" 401") || line.contains(" 403") {
                                sh.fault(format!("subscription {i} was refused: {ended}"));
                            }
                            break;
                        }
                        head = false;
                    }
                    Ok(_) => {
                        if let Some(name) = line.strip_prefix("event: ") {
                            event = name.trim().to_string();
                        } else if let Some(data) = line.strip_prefix("data: ") {
                            let e = sh.ns();
                            sh.answered(e);
                            match stream_data(data.trim(), event == "seed") {
                                Some(mut ev) => {
                                    ev.seq = in_life(life, ev.seq);
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
                        gone = a_close(&e);
                        break;
                    }
                }
            }
            me.open_since.store(u64::MAX, Ordering::SeqCst);
        }
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }
        let t = sh.ns();
        if gone {
            sh.closed(t);
        }
        sh.failed(t, true, || format!("subscription {i}: {ended}"));
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
    /// Backups taken, restored into a file that verified, held at least
    /// the primary's change at the backup's start, and whose sample of
    /// records the checker reads (`backup_differs` on a later line where
    /// one is not as written); `backup_failed` any other.
    backups: AtomicU64,
    restores: AtomicU64,
    backup_failed: AtomicU64,
    /// Restored files short of the primary's change at the backup's start.
    backup_stale: AtomicU64,
    compacts: AtomicU64,
    compact_failed: AtomicU64,
    /// Compacts refused because one was writing its file beside the
    /// database already -- the server's own (`--auto-compact`): the file
    /// was being compacted, so no failure and nothing owed again.
    compact_busy: AtomicU64,
    /// Compacts a kill cut: asked, under way, the server killed.
    compact_killed: AtomicU64,
    /// Compacts and backups a death of the primary cut (`cut_by`):
    /// the death's, counted with it, never a failure of their own.
    compact_died: AtomicU64,
    backup_died: AtomicU64,
    restart_ms: AtomicU64,
    swept: AtomicU64,
    /// Keys a restart's sweep was handed, and sweeps that could not read
    /// them.
    sweep_owed: AtomicU64,
    sweep_failed: AtomicU64,
    /// The figures of the minute's kill, said on its line as they are.
    figures: Mutex<Vec<(&'static str, String)>>,
}

impl Doings {
    fn figure(&self, name: &'static str, value: String) {
        self.figures.lock().unwrap().push((name, value));
    }
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

/// The CPU this host's other processes used, in cores: every process but
/// this one and Docker's (its VM, whose CPUs the look counts apart), by
/// `ps`'s `%cpu` -- on macOS a decaying average of about the last minute,
/// on Linux a process's own lifetime's, so there a rough sign alone.
///
/// With it the three of them that took the most, by name and in cores:
/// said on a minute they took a core or more. The second dry run of
/// 2026-11-23 counted a core or more in 53 of its minutes, up to 2.9, and
/// nothing named whose.
fn others_named() -> Option<(f64, Vec<(String, f64)>)> {
    let me = std::process::id();
    let o = std::process::Command::new("ps")
        .args(["-A", "-o", "pcpu=,pid=,comm="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stdout);
    let mut sum = 0.0;
    let mut top: Vec<(String, f64)> = Vec::new();
    for l in text.lines() {
        let mut w = l.split_whitespace();
        let (Some(cpu), Some(pid)) = (w.next(), w.next()) else {
            continue;
        };
        let comm: String = w.collect::<Vec<_>>().join(" ");
        let docker = [
            "docker",
            "Docker",
            "Virtualization",
            "qemu",
            "krun",
            "fennec-server",
        ]
        .iter()
        .any(|n| comm.contains(n));
        if pid.parse::<u32>().ok() == Some(me) || docker {
            continue;
        }
        let cpu = cpu.parse::<f64>().unwrap_or(0.0);
        sum += cpu;
        // Its name without its directories.
        let name = comm.rsplit('/').next().unwrap_or(&comm).to_string();
        match top.iter_mut().find(|(n, _)| *n == name) {
            Some(x) => x.1 += cpu / 100.0,
            None => top.push((name, cpu / 100.0)),
        }
    }
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    top.truncate(3);
    Some((sum / 100.0, top))
}

/// A job beside the load, one at a time in the order asked, each with the
/// minute it was asked at, which a wait on it names.
enum Job {
    /// `compact usertable`; `for_kill`, the one a kill is to land in,
    /// asked again while the server's own compact holds the file.
    Compact {
        minute: u64,
        for_kill: bool,
    },
    Backup {
        minute: u64,
        id: u64,
    },
    /// Read these records back, after a restart.
    Sweep {
        minute: u64,
        keys: Vec<u64>,
    },
    /// Answered once every job before it has ended.
    Fence(mpsc::Sender<()>),
}

/// Every side job asked before now waited for -- by a kill before it lands,
/// and by the end before it reads every key back -- `within` at the most:
/// one that hangs is named (`doing`, what the side thread was doing) and
/// ends the run, never waited on forever, as is a side thread that ended
/// before its queue did (its panic above). `what` is the waiter.
fn fence(jobs: &mpsc::Sender<Job>, doing: &Mutex<String>, within: Duration, what: &str) {
    let (done, wait) = mpsc::channel();
    let _ = jobs.send(Job::Fence(done));
    let t = Instant::now();
    match wait.recv_timeout(within) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Copied out first: a panic holding the lock would poison it.
            let job = doing.lock().unwrap().clone();
            panic!(
                "{what} waited {:.0} s for the side job, {job}, which did not end",
                t.elapsed().as_secs_f64()
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("{what} found the side jobs' thread ended (its panic is above)")
        }
    }
}

/// A backup's verdict from its parts -- taken, restored and verified, the
/// restored file's change number (`seq`) against the primary's at the
/// backup's start (`seq0`), its records read back -- as whether it counts
/// and whether it is stale. An empty or stale archive restores and
/// verifies as well as a whole one: only the change number tells them
/// apart, and a number not read proves nothing, so the backup counts only
/// where both were read and the restored one is at least the primary's.
fn backup_held(
    backed: bool,
    restored: bool,
    seq0: Option<u64>,
    seq: Option<u64>,
    sampled: usize,
) -> (bool, bool) {
    let fresh = matches!((seq0, seq), (Some(a), Some(b)) if b >= a);
    (
        backed && restored && fresh && sampled > 0,
        restored && !fresh,
    )
}

/// What the side thread needs of the run.
struct Side {
    sh: Arc<Shared>,
    doings: Arc<Doings>,
    kit: Kit,
}

/// What cut a side job that failed (`cut_by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutBy {
    /// The run's kill, which the schedule lands in a compact: no failure.
    Kill,
    /// A death of the primary's: the death's, never the job's own.
    Death,
}

/// How long a side job cut after the run's kill was sent waits for that
/// kill to be judged its or a death's (`Shared::kill_judged`): the kill's
/// `docker kill` and `docker wait` are bounded at 60 s and 120 s, the
/// inspect after them is one more call.
const KILL_JUDGE_WAIT: Duration = Duration::from_secs(200);

/// What cut a side job of `[s, ...]` that failed, `close` where its
/// connection went rather than a wait running out: a death found since
/// `s` (`Shared::deaths`); the run's kill, sent since, once the kill's
/// path judged it (`judge_kill`): its own, or a death's (`kill_death`) --
/// a death in the moment before the send is found only after Docker's
/// wait (`gone_before`), and the job, cut by it, saw its connection close
/// after `kill_sent` was stored all the same, the death timed before `s`
/// where the server was gone before the job was asked; with no kill sent
/// since, a closed connection and the
/// server not running with no restart's window open (`stopped`: Docker's
/// word), looked for `death_wait` at the most, as the job sees its
/// connection close before the run finds the death; and in a restart's
/// window with nothing else found, the kill's, as before the deaths were
/// told apart. `None`: a failure of the job's own. The long run's compact
/// of minute 300 counted as a failure of its own, the death that cut it
/// its cause.
fn cut_by(
    sh: &Shared,
    s: u64,
    close: bool,
    stopped: &dyn Fn() -> bool,
    death_wait: Duration,
    kill_wait: Duration,
) -> Option<CutBy> {
    let t = Instant::now();
    loop {
        if sh.deaths.lock().unwrap().iter().any(|&d| d >= s) {
            return Some(CutBy::Death);
        }
        let sent = sh.kill_sent.load(Ordering::SeqCst);
        if sent >= s {
            if sh.kill_judged.load(Ordering::SeqCst) >= sent {
                return Some(match sh.kill_death.load(Ordering::SeqCst) >= sent {
                    true => CutBy::Death,
                    false => CutBy::Kill,
                });
            }
            if t.elapsed() >= kill_wait {
                return Some(CutBy::Kill);
            }
        } else {
            if !close || t.elapsed() >= death_wait {
                break;
            }
            if !sh.restarting(sh.ns()) && stopped() {
                return Some(CutBy::Death);
            }
        }
        std::thread::sleep(Duration::from_millis(50).min(death_wait));
    }
    sh.restarting(sh.ns()).then_some(CutBy::Kill)
}

impl Side {
    /// What cut a job of `[s, ...]` that failed (`cut_by`), Docker asked
    /// whether the server runs.
    fn cut_by(&self, s: u64, close: bool) -> Option<CutBy> {
        let stopped = || {
            self.kit
                .exit(&self.kit.server())
                .is_some_and(|x| !x.running)
        };
        cut_by(&self.sh, s, close, &stopped, DEATH_WAIT, KILL_JUDGE_WAIT)
    }

    fn compact(&self, for_kill: bool) {
        let sh = &self.sh;
        sh.compact_to.store(u64::MAX, Ordering::SeqCst);
        sh.compact_from.store(sh.ns(), Ordering::SeqCst);
        let first = Instant::now();
        let asked = sh.ns();
        loop {
            sh.compact_since.store(sh.ns(), Ordering::SeqCst);
            let (t, file) = (Instant::now(), sh.p_file.load(Ordering::SeqCst));
            let done =
                http::Http::try_connect(&sh.addr, Duration::from_secs(3600)).and_then(|mut c| {
                    c.try_query("compact usertable", "")
                        .map(|(st, body)| (st, String::from_utf8_lossy(body).into_owned()))
                });
            sh.compact_since.store(u64::MAX, Ordering::SeqCst);
            match done {
                Ok((200, _)) => {
                    self.doings.compacts.fetch_add(1, Ordering::Relaxed);
                    // Its time, and a GB of the file at the look before
                    // it: what the replica's own may plausibly take
                    // (`plausible_ms`).
                    let ms = t.elapsed().as_millis() as u64;
                    self.doings.figure("compact_ms", ms.to_string());
                    if file > 0 {
                        let per_gb = (ms as u128 * 1_000_000_000 / file as u128) as u64;
                        sh.p_compact_ms_per_gb.fetch_max(per_gb, Ordering::SeqCst);
                    }
                }
                // fennec-server compacts on its own (`--auto-compact`), and
                // under the run's memory it does at a third dead: a second
                // compact is refused while that one writes its file
                // (engine/maintenance.rs). The file is being compacted:
                // no failure. The compact a kill is to land in waits its
                // turn and is asked again.
                Ok((_, body)) if body.contains("beside the database already") => {
                    if for_kill && first.elapsed() < WAIT_MAX {
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    self.doings.compact_busy.fetch_add(1, Ordering::Relaxed);
                    say("compact refused: the server's own was writing its file".into());
                }
                Err(e) => match self.cut_by(asked, a_close(&e)) {
                    // Cut by a kill the schedule landed in it: what the run
                    // means it to meet, not a failure -- the file must
                    // verify after.
                    Some(CutBy::Kill) => {
                        self.doings.compact_killed.fetch_add(1, Ordering::Relaxed);
                    }
                    // Cut by a death of the primary's: the death's -- before
                    // the kill's window, which the announcement opens a
                    // second before the kill, or in the moment before the
                    // kill's send: the dry run's death in that second was
                    // counted as the kill's cut.
                    Some(CutBy::Death) => {
                        say(format!("compact cut by the primary's death: {e}"));
                        self.doings.compact_died.fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        sh.note(format!("compact failed: {e}"));
                        self.doings.compact_failed.fetch_add(1, Ordering::Relaxed);
                    }
                },
                other => {
                    sh.note(format!("compact failed: {other:?}"));
                    self.doings.compact_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            break;
        }
        sh.compact_to.store(sh.ns(), Ordering::SeqCst);
    }

    /// A backup over the replication stream into an archive, restored into
    /// a file and the file verified, each step said on its own; then the
    /// file served read-only, its change number held to the primary's at
    /// the backup's start and a sample of its records logged for the
    /// checker as reads over the backup's span -- an empty or stale archive
    /// verifies as well as a whole one. A backup that fails any of it keeps
    /// the run's volumes, the restored file among them.
    fn backup(&self, id: u64) {
        let sh = &self.sh;
        let kit = &self.kit;
        let me = &sh.readers[0];
        let s = sh.ns();
        // The registers the sample is held to are kept from here.
        me.inflight.store(s, Ordering::SeqCst);
        let seq0 = super::metrics_page(&sh.addr)
            .map(|p| metric_sum(p.as_bytes(), "fennec_change_sequence"));
        // The token from the container's environment (`fennec backup`
        // reads FENNEC_REPLICATION_TOKEN), never the script's words: `ps`
        // shows them. The archive's directory made first: into a path that
        // is none, a backup writes a database file of its own, which
        // `fennec restore` takes for a sealed one and refuses without its
        // key -- the short run of 2026-11-22's backup.
        let script = format!(
            "rm -rf /data/archive /data/restored.fennec; mkdir /data/archive && \
             /tools/fennec backup http://{}:8080 /data/archive && echo BACKUP-OK && \
             /tools/fennec restore /data/archive /data/restored.fennec && echo RESTORE-OK && \
             /tools/fennec verify /data/restored.fennec && echo VERIFY-OK; \
             rm -rf /data/archive",
            kit.server(),
        );
        let t = Instant::now();
        let (_, out) = kit.fennec_env(
            &format!("{}-backup", kit.name),
            &kit.volume("backup-data"),
            &script,
            Duration::from_secs(1800),
            &kit.secrets()[1..],
        );
        let e = sh.ns();
        // Refused by the primary for its token or its name: the run's own
        // setting, which every backup after would meet -- the run stops at
        // the minute's end rather than carry it for hours.
        if let Some(why) = refused(&out) {
            sh.fault(format!("fennec backup was refused: {why}"));
        }
        let backed = out.contains("BACKUP-OK");
        let restored = out.contains("RESTORE-OK") && out.contains("VERIFY-OK");
        let (seq, sampled) = match restored {
            true => self.read_restored(id, s, e),
            false => (None, 0),
        };
        me.inflight.store(u64::MAX, Ordering::SeqCst);
        let (ok, stale) = backup_held(backed, restored, seq0, seq, sampled);
        say(format!(
            "backup {id} {}, restored and verified {}, its change {} against the primary's {} at its start, {sampled} records read from it; {:.0} s",
            if backed { "taken" } else { "FAILED" },
            if restored { "clean" } else { "FAILED" },
            seq.map_or("unread".into(), |x| x.to_string()),
            seq0.map_or("unread".into(), |x| x.to_string()),
            t.elapsed().as_secs_f64()
        ));
        if ok {
            self.doings.backups.fetch_add(1, Ordering::Relaxed);
            self.doings.restores.fetch_add(1, Ordering::Relaxed);
            kit.fennec(
                &format!("{}-backup", kit.name),
                &kit.volume("backup-data"),
                "rm -f /data/restored.fennec",
                Duration::from_secs(120),
            );
        } else if !backed && self.cut_by(s, true) == Some(CutBy::Death) {
            // Its stream from the primary cut by the primary's death: the
            // death's, not the backup's.
            say(format!("backup {id} cut by the primary's death"));
            self.doings.backup_died.fetch_add(1, Ordering::Relaxed);
        } else {
            say(out);
            self.doings.backup_failed.fetch_add(1, Ordering::Relaxed);
            if stale {
                self.doings.backup_stale.fetch_add(1, Ordering::Relaxed);
            }
            kit.keep_data(&format!(
                "backup {id} failed; its restored file is restored.fennec in {}",
                kit.volume("backup-data")
            ));
        }
    }

    /// The restored file served read-only beside the run: its change
    /// number, and 1 000 of the records written so far read from it and
    /// logged as reads over the backup's span `[s, e]`; how many.
    fn read_restored(&self, id: u64, s: u64, e: u64) -> (Option<u64>, usize) {
        let sh = &self.sh;
        let kit = &self.kit;
        let port = http::free_port();
        // The run's token, as the requests of this process carry it.
        let (ok, out) = docker_env(
            Duration::from_secs(120),
            &kit.secrets()[..1],
            &[
                "run",
                "-d",
                "--pull",
                "never",
                "--name",
                &format!("{}-restored", kit.name),
                "--network",
                &kit.net(),
                "-p",
                &format!("127.0.0.1:{port}:8080"),
                "-v",
                &format!("{}:/data", kit.volume("backup-data")),
                "--memory",
                &kit.memory,
                "--memory-swap",
                &kit.memory,
                "-e",
                "FENNEC_HTTP_TOKEN",
                &kit.image,
                "--http",
                "0.0.0.0:8080",
                "--file",
                "/data/restored.fennec",
                "--http-read-only",
                "--auto-compact",
                "off",
            ],
        );
        let mut got = (None, 0);
        if !ok {
            sh.note(format!("the restored file's server did not start: {out}"));
        } else if kit.wait_health(port, Duration::from_secs(300)) {
            let addr = format!("127.0.0.1:{port}");
            let seq = super::metrics_page(&addr)
                .map(|p| metric_sum(p.as_bytes(), "fennec_change_sequence"));
            let last = sh.fifo.lock().unwrap().0;
            let mut rng = Rng::new(SEED ^ 0xBAC0 ^ id);
            let mut keys: Vec<u64> = (0..1_000).map(|_| rng.below(last) + 1).collect();
            keys.sort_unstable();
            keys.dedup();
            let list = keys
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let read = http::Http::try_connect(&addr, Duration::from_secs(60)).and_then(|mut c| {
                c.try_query("get usertable where id in $1", &format!("[{list}]"))
                    .map(|(st, body)| (st, json_rows(body)))
            });
            match read {
                Ok((200, rows)) => {
                    let evs: Vec<Ev> =
                        page_reads(&rows, keys.iter().copied(), s, e, "the restored file")
                            .into_iter()
                            .map(|ev| match ev {
                                Ev::Row {
                                    key, found, got, ..
                                } => Ev::Restored {
                                    backup: id,
                                    key,
                                    s,
                                    e,
                                    found,
                                    got,
                                },
                                other => other,
                            })
                            .collect();
                    sh.readers[0].log.lock().unwrap().extend(evs);
                    got = (seq, keys.len());
                }
                other => sh.note(format!(
                    "the restored file's records: {:?}",
                    other.map(|x| x.0)
                )),
            }
        } else {
            sh.note("the restored file's server did not answer within 300 s".into());
        }
        docker(&["rm", "-f", &format!("{}-restored", kit.name)]);
        got
    }

    /// Reads `keys` back, a thousand a request, each held as a read; one
    /// that cannot is counted, never passed over.
    fn sweep(&self, keys: Vec<u64>) {
        let sh = &self.sh;
        let me = &sh.readers[0];
        self.doings
            .sweep_owed
            .fetch_add(keys.len() as u64, Ordering::Relaxed);
        let Ok(mut c) = http::Http::try_connect(&sh.addr, Duration::from_secs(60)) else {
            self.doings.sweep_failed.fetch_add(1, Ordering::Relaxed);
            sh.failed(sh.ns(), false, || {
                format!(
                    "the sweep of {} keys after a restart could not connect",
                    keys.len()
                )
            });
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
                other => {
                    self.doings.sweep_failed.fetch_add(1, Ordering::Relaxed);
                    sh.failed(e, false, || {
                        format!("the sweep's read: {:?}", other.map(|x| x.0))
                    })
                }
            }
            drop(log);
            me.inflight.store(u64::MAX, Ordering::SeqCst);
        }
    }

    fn run(self, jobs: mpsc::Receiver<Job>) {
        for job in jobs {
            let what = match &job {
                Job::Compact { minute, for_kill } => format!(
                    "the compact asked at minute {minute}{}",
                    if *for_kill {
                        " for the kill to land in"
                    } else {
                        ""
                    }
                ),
                Job::Backup { minute, id } => format!("backup {id}, asked at minute {minute}"),
                Job::Sweep { minute, keys } => {
                    format!("the sweep of {} keys asked at minute {minute}", keys.len())
                }
                Job::Fence(_) => String::new(),
            };
            *self.sh.doing.lock().unwrap() = what;
            match job {
                Job::Compact { for_kill, .. } => self.compact(for_kill),
                Job::Backup { id, .. } => self.backup(id),
                Job::Sweep { keys, .. } => self.sweep(keys),
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
            replica: false,
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
    loss_ms: u64,
    restart_max: u64,
    replica_back_max: u64,
    lag_max: u64,
    catchup_max: u64,
    recall_min: f64,
    subscriptions: u64,
    threads: u64,
    /// The replica lane's connections (`--replica-threads`): 0 where the
    /// reads through the replica share the pool, a start line from before
    /// the lanes among them.
    replica_threads: u64,
    /// The servers' `--replication-buffer`, MiB: 64, the server's own
    /// default, where a start line from before it does not say.
    replication_buffer: u64,
    /// The CPUs of Docker's VM (`docker info`'s `NCPU`): what a task the
    /// engine cuts in shares spreads over (`fennec_core::threads`), a
    /// thread a CPU but the one that asked. 0 where unknown.
    cpus: u64,
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
            loss_ms: o.loss_ms,
            restart_max: o.restart_max,
            replica_back_max: o.replica_back_max,
            lag_max: o.lag_max,
            catchup_max: o.catchup_max,
            recall_min: o.recall_min,
            subscriptions: o.subscriptions as u64,
            threads: o.threads as u64,
            replica_threads: o.replica_threads as u64,
            replication_buffer: o.replication_buffer,
            cpus: 0,
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
            ("loss_ms", self.loss_ms.to_string()),
            ("restart_max", self.restart_max.to_string()),
            ("replica_back_max", self.replica_back_max.to_string()),
            ("lag_max", self.lag_max.to_string()),
            ("catchup_max", self.catchup_max.to_string()),
            ("recall_min", format!("{}", self.recall_min)),
            ("subscriptions", self.subscriptions.to_string()),
            ("threads", self.threads.to_string()),
            ("replica_threads", self.replica_threads.to_string()),
            ("replication_buffer", self.replication_buffer.to_string()),
            ("cpus", self.cpus.to_string()),
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
            loss_ms: g("loss_ms")? as u64,
            restart_max: g("restart_max")? as u64,
            replica_back_max: g("replica_back_max")? as u64,
            lag_max: g("lag_max")? as u64,
            catchup_max: g("catchup_max")? as u64,
            recall_min: g("recall_min")?,
            subscriptions: g("subscriptions")? as u64,
            threads: g("threads")? as u64,
            replica_threads: g("replica_threads").map_or(0, |x| x as u64),
            replication_buffer: g("replication_buffer").map_or(64, |x| x as u64),
            cpus: g("cpus").map_or(0, |x| x as u64),
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
    let mut c = http::Http::try_connect(addr, WAIT_MAX).ok()?;
    let mut digests = [0u64; 3];
    // A line a minute: the end's read of a million records under the
    // cgroup's limit is minutes, which the guard must not take for a hang.
    let mut said = Instant::now();
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
                // `order id`: a page in storage order would differ between
                // two files holding the same rows, after either compacted.
                .try_query(
                    &format!("get {name} where id >= $1 and id < $2 order id"),
                    &format!("{from},{to}"),
                )
                .ok()?;
            let e = sh.ns();
            if status != 200 {
                return None;
            }
            if said.elapsed() >= Duration::from_secs(60) {
                say(format!("reading {name} on {addr}: {} of {last}", to - 1));
                said = Instant::now();
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

/// The server's clock against this host's wall clock and this process's
/// own (`sh.ns`), by a write of `now()` (`CLOCK_PROBE`), each from the
/// middle of the request (`Probe`).
fn clock_probe(addr: &str, sh: &Shared) -> Option<Probe> {
    let wall = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1e3)
    };
    let mut c = http::Http::try_connect(addr, Duration::from_secs(10)).ok()?;
    let (before, b_ns) = (wall(), sh.ns());
    let (status, body) = c.try_query(CLOCK_PROBE, "").ok()?;
    let (after, a_ns) = (wall(), sh.ns());
    if status != 200 {
        return None;
    }
    let t = *http::ints(body, "t").first()?;
    Some(Probe::read(t, (before, after), (b_ns, a_ns)))
}

/// Whether the clock moved against this host's from one probe to the next
/// by more than a probe's error can explain: a step the `@ttl` slack must
/// take (`CLOCK_STEP_MS`).
fn clock_stepped(prev: Probe, now: Probe) -> bool {
    (now.off_ms - prev.off_ms).abs() - (prev.rtt_ms + now.rtt_ms) / 2.0 > CLOCK_STEP_MS
}

/// A secret of the run's: 32 bytes of the system's randomness, as hex --
/// the servers' tokens, held in memory and handed to each container in its
/// environment, never written to a file, an argument or the log.
fn secret() -> String {
    use std::io::Read;
    let mut b = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A refusal a server answered the run for its token or its address, as
/// `fennec backup` and a replica's status word one (`the primary answered
/// 403: ...`), from its status on: the run's own setting, which every
/// request after meets again. `None` for anything else -- a 5xx, a cut
/// connection -- which a restart explains and the run counts.
fn refused(text: &str) -> Option<String> {
    ["answered 401", "answered 403"].iter().find_map(|k| {
        let at = text.find(k)?;
        // Its first line, unescaped where a status's JSON quoted it.
        let words: String = text[at..].lines().next()?.chars().take(300).collect();
        let words = words.replace("\\\"", "\"");
        Some(words.trim_end_matches(['"', '}']).to_string())
    })
}

/// Ends the run at once on `why` -- the harness's own fault, or a server
/// gone before the load began -- never carrying for hours what every
/// minute meets again: the load stopped, the run's
/// volumes kept for whoever reads why (`--kept`), its containers removed,
/// a `:stopped` line beside its others, and exit 4 -- the script's word for
/// the harness's own failure, apart from a report's (1) and the
/// environment's (2, 3).
fn stop_run(bench: &Kit, sh: Option<&Shared>, o: &Opts, why: &str) -> ! {
    if let Some(sh) = sh {
        sh.stop.store(true, Ordering::SeqCst);
    }
    bench.keep_data(&format!("the run stopped at once: {why}"));
    say(format!(
        "STOPPED: {why} -- every minute after would meet it again, and no run counts until it is fixed"
    ));
    append_values(
        &o.out,
        "soak",
        &o.run_id,
        &format!("{}:{}:stopped", o.sync, o.label),
        &[("stopped", "1".to_string())],
    );
    bench.clean();
    std::process::exit(4);
}

/// The CPUs of Docker's VM, 0 where `docker info` does not say.
fn vm_cpus() -> u64 {
    let (ok, out) = docker_within(Duration::from_secs(60), &["info", "--format", "{{.NCPU}}"]);
    match ok {
        true => out.trim().parse().unwrap_or(0),
        false => 0,
    }
}

/// Whether the replica says it is connected and holds the change `target`.
fn replica_ready(kit: &Kit, target: u64) -> bool {
    kit.status(kit.replica_port).is_some_and(|st| {
        st.contains("\"connected\":true") && json_num(&st, "seq").is_some_and(|q| q >= target)
    })
}

/// A watch on the primary's main thread -- its syncer under `--sync <ms>`
/// (fennec-server's `run_syncer` runs on it) -- from a container in its
/// process namespace, which the kill ends with the namespace: what it last
/// saw is what the thread was doing at the kill.
struct Sampler {
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    name: String,
}

/// The syscalls that wait for the disk, by the VM's architecture: fsync,
/// fdatasync, sync_file_range, msync, sync and syncfs.
fn syncing(arch: &str, n: u64) -> bool {
    match arch {
        "x86_64" => [74, 75, 277, 26, 162, 306].contains(&n),
        _ => [82, 83, 84, 227, 81, 267].contains(&n),
    }
}

impl Sampler {
    /// Started and seen running, or `None`: the thread's syscall is not to
    /// be read, and the kill's line says it is unknown.
    fn start(kit: &Kit) -> Option<Sampler> {
        use std::process::{Command, Stdio};
        let name = format!("{}-sampler", kit.name);
        let script = "echo \"A $(uname -m)\"; prev=; i=0; \
            while read -r n _ < /proc/1/task/1/syscall; do \
              if [ \"$n\" != \"$prev\" ]; then read -r up _ < /proc/uptime; echo \"S $up $n\"; prev=$n; fi; \
              i=$((i + 1)); if [ $((i % 20)) = 0 ]; then read -r up _ < /proc/uptime; echo \"T $up\"; fi; \
              usleep 5000 2>/dev/null || sleep 0.005; \
            done";
        let mut child = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--pull",
                "never",
                "--name",
                &name,
                &format!("--pid=container:{}", kit.server()),
                "--cap-add",
                "SYS_PTRACE",
                ALPINE,
                "sh",
                "-c",
                script,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let mut s = Sampler { child, lines, name };
        let until = Instant::now() + Duration::from_secs(15);
        let mut first = Vec::new();
        while let Ok(l) = s
            .lines
            .recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            let seen = l.starts_with("S ");
            first.push(l);
            if seen {
                // Put back for `finish`, through a channel of its own.
                let (tx, rx) = mpsc::channel();
                for x in first {
                    let _ = tx.send(x);
                }
                let rest = std::mem::replace(&mut s.lines, rx);
                std::thread::spawn(move || {
                    for x in rest {
                        if tx.send(x).is_err() {
                            break;
                        }
                    }
                });
                return Some(s);
            }
        }
        s.stop();
        None
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        docker(&["rm", "-f", &self.name]);
    }

    /// After the kill: whether the thread was in a call that waits for the
    /// disk, and for how long it had been, in ms (0: it was not).
    fn finish(mut self) -> Option<u64> {
        let until = Instant::now() + Duration::from_secs(30);
        let mut lines = Vec::new();
        while let Ok(l) = self
            .lines
            .recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            lines.push(l);
        }
        self.stop();
        in_sync(&lines)
    }
}

/// What a sampler's lines say of the thread at their end: in a call that
/// waits for the disk, for so many ms, or 0 for not; `None` for not seen.
/// `A <arch>` once, `S <uptime> <syscall>` at each change, `T <uptime>`
/// every twentieth look.
fn in_sync(lines: &[String]) -> Option<u64> {
    let (mut arch, mut last, mut end) = ("", None::<(f64, u64)>, 0f64);
    for l in lines {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["A", a] => arch = a,
            ["S", up, n] => {
                let up: f64 = up.parse().unwrap_or(0.0);
                // `running`, or -1 outside a syscall: neither waits.
                last = Some((up, n.parse().unwrap_or(u64::MAX)));
                end = end.max(up);
            }
            ["T", up] => end = end.max(up.parse().unwrap_or(0.0)),
            _ => {}
        }
    }
    let (since, n) = last?;
    Some(match syncing(arch, n) {
        true => ((end - since) * 1e3).max(1.0) as u64,
        false => 0,
    })
}

/// The watch's script, over the process whose `/proc` directory is
/// `proc` (`/proc/1` in the replica's namespace): the VM's architecture,
/// every thread named `R_COMPACT_THREAD` -- each listed, whatever the
/// order the kernel lists them in, and the replica's told apart by what
/// it waits in (`CompactWatch`) -- and each one's syscall as it changes;
/// `N` where there is none. It ends once a thread it watches is gone.
fn watch_script(proc: &str) -> String {
    format!(
        "echo \"A $(uname -m)\"; ts=; \
         for d in {proc}/task/*; do read -r c < \"$d/comm\" && [ \"$c\" = {R_COMPACT_THREAD} ] && ts=\"$ts ${{d##*/}}\"; done; \
         [ -n \"$ts\" ] || {{ echo N; exit 0; }}; echo \"T$ts\"; \
         while :; do \
           for t in $ts; do \
             read -r n _ < \"{proc}/task/$t/syscall\" || exit 0; \
             eval \"p=\\${{p$t-}}\"; \
             if [ \"$n\" != \"$p\" ]; then echo \"S $t $n\"; eval \"p$t=\\$n\"; fi; \
           done; \
           usleep {} 2>/dev/null || sleep 0.1; \
         done",
        R_WATCH_EVERY / 1_000
    )
}

/// The watch's container, `name`, in the process namespace of `replica`:
/// reading another user's task's syscall is a ptrace's right, and it reads
/// `/proc` alone, so it is on no network.
fn watch_args(name: &str, replica: &str, script: &str) -> Vec<String> {
    let pid = format!("--pid=container:{replica}");
    [
        "run",
        "--rm",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "none",
        &pid,
        "--cap-add",
        "SYS_PTRACE",
        ALPINE,
        "sh",
        "-c",
        script,
    ]
    .map(String::from)
    .to_vec()
}

/// Why the watch is not looking: its container could not start or ended,
/// or the replica has no thread so named -- said once, and looked for
/// again slowly (`R_WATCH_ABSENT`).
const WATCH_FAILED: &str = "docker run failed";
const WATCH_ENDED: &str = "its container ended";
const WATCH_ABSENT: &str = "no fennec-compact thread in the replica";

/// How long the watch waits before its next container, after it went for
/// `why`.
fn watch_again(why: &str) -> Duration {
    match why {
        WATCH_ABSENT => R_WATCH_ABSENT,
        _ => R_WATCH_AGAIN,
    }
}

/// The replica's compactions on its own so far
/// (`fennec_auto_compactions_total`), on a connection of its own bounded
/// at 5 s: the watch's thread waits on it, and a server's `/_metrics`
/// waits on its database's lock.
fn compactions(addr: &str) -> Option<u64> {
    let mut c = http::Http::try_connect(addr, Duration::from_secs(5)).ok()?;
    let (status, body) = c.try_request("GET", "/_metrics", "text/plain", b"").ok()?;
    (status == 200).then(|| metric_sum(body, "fennec_auto_compactions_total"))
}

/// Watches the replica's compaction thread (fennec-http's `link`, named
/// `fennec-compact`) from a container in its process namespace, as the
/// sampler watches the primary's syncer, for the run's length: each
/// compaction the replica runs on its own opens a window from about its
/// start to its end (`Shared::r_window`, `CompactWatch`), which the reads
/// through the replica are classed by as they are answered (`class`). Its
/// metrics say only that one ended -- its `fennec_auto_compactions_total`
/// counted, its `fennec_reclaimable_bytes` fallen at the swap -- and its
/// log the same, the compaction's time with it: a read held at the start
/// is answered seconds before either, and the minute's figures are
/// written as each minute ends. Its side file (`<file>.beside`, the
/// minute's `r_compacting`) is written only once the graphs holding
/// tombstones are rebuilt and swapped in under the write lock
/// (fennec-core's `compact_online`), and a stall there would come before
/// it. But its counter settles each window as it ends: read as one
/// begins and as it ends, a window it did not move in was no compaction
/// (`Counter`, `replica_compaction`). Started again 5 s after the
/// container ends -- the replica started again after a death, a thread
/// not yet there -- a compaction under way left open for the next to see
/// whether its thread is still at work (`CompactWatch::after`), and 60 s
/// after one found no thread so named, until the run stops; said once each
/// time it goes, and as it comes back. A window whose watch stays gone is
/// lost, and one that stays open past what its file plausibly takes is
/// past its bound, each found at a minute's look and its reads' window cut
/// (`bound_r_window`).
fn watch_replica(sh: &Shared, kit: &Kit) {
    use std::process::{Command, Stdio};
    let name = format!("{}-replica-watch", kit.name);
    let args = watch_args(&name, &kit.replica(), &watch_script("/proc/1"));
    let mut w = CompactWatch::default();
    let mut counter = Counter::default();
    let mut read = || compactions(&sh.raddr);
    // Why the watch went, once said: said again only after it watched.
    let mut down: Option<&str> = None;
    while !sh.stop.load(Ordering::Relaxed) {
        docker(&["rm", "-f", &name]);
        let child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        w = CompactWatch::after(&w);
        let mut why = WATCH_FAILED;
        if let Ok(mut child) = child {
            let (tx, lines) = mpsc::channel::<(u64, String)>();
            if let Some(out) = child.stdout.take() {
                let t0 = sh.t0;
                std::thread::spawn(move || {
                    for l in BufReader::new(out).lines().map_while(Result::ok) {
                        if tx.send((t0.elapsed().as_nanos() as u64, l)).is_err() {
                            break;
                        }
                    }
                });
            }
            why = WATCH_ENDED;
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    break;
                }
                let edge = match lines.recv_timeout(Duration::from_nanos(R_WATCH_EVERY)) {
                    Ok((t, l)) => {
                        if l == "N" {
                            why = WATCH_ABSENT;
                        }
                        w.line(&l, t)
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => w.tick(sh.ns()),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let watching = w.watching();
                if !sh.watched(watching) && watching && down.take().is_some() {
                    say("the replica's compaction thread watched again".into());
                }
                counter.edge(sh, edge, &mut read);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        sh.watched(false);
        docker(&["rm", "-f", &name]);
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }
        let again = watch_again(why);
        if down != Some(why) {
            say(format!(
                "the replica's compactions are not watched: {why}; looked at again every {} s",
                again.as_secs()
            ));
            down = Some(why);
        }
        let until = Instant::now() + again;
        while Instant::now() < until {
            if sh.stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // A compaction still open as the run ends: cut, its end unseen.
    counter.edge(sh, w.stop(sh.ns()), &mut read);
}

/// A server's memory watch's script (`watch_memory`): its cgroup's
/// `memory.stat` once a second, read through the server's own root -- the
/// shell's built-ins alone, a fork a second for `sleep`. A `docker exec` a
/// look is no option: a sizing loop of one every 100 ms slowed a server
/// 30-57%. It runs in a container of its own, charged to its own cgroup,
/// and ends with the server's processes (its process namespace's).
const MEM_SCRIPT: &str = "cd /proc/1/root/sys/fs/cgroup || exit 0; \
    while :; do a=0; f=0; d=0; w=0; j=0; \
      while read -r k v; do case $k in anon) a=$v;; file) f=$v;; \
        file_dirty) d=$v;; file_writeback) w=$v;; pgmajfault) j=$v;; esac; \
      done < memory.stat || exit 0; \
      echo \"M $a $f $d $w $j\"; sleep 1; \
    done";

/// The memory watch's container on `server`: in its process namespace,
/// reading another user's `/proc/1/root` a ptrace's right, on no network.
fn mem_args(name: &str, server: &str) -> Vec<String> {
    let pid = format!("--pid=container:{server}");
    [
        "run",
        "--rm",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        "none",
        &pid,
        "--cap-add",
        "SYS_PTRACE",
        ALPINE,
        "sh",
        "-c",
        MEM_SCRIPT,
    ]
    .map(String::from)
    .to_vec()
}

/// Watches a server's cgroup a second for the run's length (`which` 0 the
/// primary, 1 the replica), into `Shared::mem`: the minute's, the life's
/// and a replica's window's most anonymous memory and dirty pages, least
/// page cache and faults. One process the run long, as `ycsb`'s
/// larger-than-memory cells watch theirs (`Watch`), started again a second
/// after it ends -- the server killed, its namespace gone with it -- until
/// the server runs again: the look a minute read 304 MB at the long run's
/// minute 300, and the compaction that then took 130 MB more in seconds
/// OOM-killed it between two looks.
fn watch_memory(sh: &Shared, kit: &Kit, which: usize) {
    use std::process::{Command, Stdio};
    let server = match which {
        0 => kit.server(),
        _ => kit.replica(),
    };
    let name = format!("{server}-mem");
    let args = mem_args(&name, &server);
    while !sh.stop.load(Ordering::Relaxed) {
        docker(&["rm", "-f", &name]);
        let child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            let (tx, lines) = mpsc::channel::<String>();
            if let Some(out) = child.stdout.take() {
                std::thread::spawn(move || {
                    for l in BufReader::new(out).lines().map_while(Result::ok) {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                });
            }
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    break;
                }
                match lines.recv_timeout(Duration::from_millis(500)) {
                    Ok(l) => {
                        if let Some(s) = MemSample::parse(&l) {
                            let open = sh.r_since.load(Ordering::SeqCst) != u64::MAX;
                            sh.mem[which].lock().unwrap().sample(sh.ns(), s, open);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        docker(&["rm", "-f", &name]);
        // The server being started again: looked for each second.
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until && !sh.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Each second of the run: the open loop's lag in each lane (`behind_s`),
/// the most of the minute's and of its seconds in a window of the
/// replica's (`Behind`); at the peak, the replica lane's schedule set to its
/// share of what the pool answered in the second; and a line of
/// `<run>-seconds.tsv` in `--logs` -- the lags, the window, both servers'
/// memory, page cache and faults at their latest look -- which the minute's
/// line sums up: dry run 6 had `behind_s` once a minute, and its one look
/// inside a replica's compaction, 16 s into it, could not say how its
/// queue grew.
fn seconds(sh: &Shared, log: Option<PathBuf>) {
    let mut f = log.and_then(|p| File::create(p).ok());
    if let Some(f) = f.as_mut() {
        let _ = writeln!(
            f,
            "s\tutc\tbehind_s\tr_behind_s\tr_compacting\tanon_mb\tdirty_mb\tfile_mb\tmajflt\tr_anon_mb\tr_dirty_mb\tr_file_mb\tr_majflt"
        );
    }
    let lanes = sh.lane < sh.slots.len();
    let mut pool_before = sh.pool_ops.load(Ordering::Relaxed);
    let mut next = Instant::now() + Duration::from_secs(1);
    let mut last = Instant::now();
    while !sh.stop.load(Ordering::Relaxed) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        next += Duration::from_secs(1);
        let now = sh.ns();
        let b = behind_s(sh.pool_slots(), now);
        let rb = match lanes {
            true => behind_s(sh.lane_slots(), now),
            false => None,
        };
        let open = sh.r_since.load(Ordering::SeqCst) != u64::MAX;
        {
            let mut x = sh.behind.lock().unwrap();
            most(&mut x.main, b);
            most(&mut x.replica, rb);
            if open {
                most(&mut x.window_main, b);
                most(&mut x.window_replica, rb);
                x.window_s += 1;
            }
        }
        if sh.rate == 0.0 && lanes {
            let p = sh.pool_ops.load(Ordering::Relaxed);
            let secs = last.elapsed().as_secs_f64().max(0.1);
            last = Instant::now();
            let rate =
                p.saturating_sub(pool_before) as f64 / secs * REPLICA_SHARE / (1.0 - REPLICA_SHARE);
            pool_before = p;
            sh.r_every
                .store((1e9 / rate.max(1.0)) as u64, Ordering::Relaxed);
        }
        let Some(f) = f.as_mut() else {
            continue;
        };
        let look = |i: usize| {
            let m = sh.mem[i].lock().unwrap();
            match m.last {
                Some((t, s)) if now.saturating_sub(t) < 2_000_000_000 => format!(
                    "{}\t{}\t{}\t{}",
                    s.anon >> 20,
                    s.dirty >> 20,
                    s.file >> 20,
                    s.majflt
                ),
                _ => "\t\t\t".into(),
            }
        };
        let lag = |x: Option<f64>| x.map_or(String::new(), |x| format!("{x:.2}"));
        let _ = writeln!(
            f,
            "{:.0}\t{}\t{}\t{}\t{}\t{}\t{}",
            now as f64 / 1e9,
            utc(sh.t0_wall + now),
            lag(b),
            lag(rb),
            open as u8,
            look(0),
            look(1)
        );
    }
}

/// How often a restart's catch-up is looked at (`catch_up`): the replica's
/// status asked four times a second while the replica is short of the
/// change, a 504 of a read sent in the quarter second after it held it
/// still the restart's.
const CATCH_EVERY: Duration = Duration::from_millis(250);

/// Each restart's catch-up (`Catch`) followed to its end at a look every
/// `CATCH_EVERY` (`catch_look`), and said once it is over: the replica's
/// change read from its `/_replication/status`.
fn catch_up(sh: &Shared, kit: &Kit, doings: &Doings, bound: u64) {
    let wait = fennec_http::AFTER_WAIT.as_nanos() as u64 + 1_000_000_000;
    let mut held = || {
        kit.status(kit.replica_port)
            .filter(|st| st.contains("\"connected\":true"))
            .and_then(|st| json_num(&st, "seq"))
    };
    loop {
        let stop = sh.stop.load(Ordering::Relaxed);
        for x in catch_look(sh, doings, bound, wait, stop, &mut held) {
            say(catch_said(&x, bound));
        }
        if stop {
            break;
        }
        std::thread::sleep(CATCH_EVERY);
    }
}

/// One look at the latest restart's catch-up (`catch_up`): the loop's lag
/// and whether reads go through the replica again, then -- the loop back
/// on its schedule -- the replica's change (`held`), read outside the lock
/// a 504 is judged under. How long the replica took from the loop's return
/// is written as it ends (`catchup_replica_ms`, -1 where `bound`,
/// `--replica-back-max`, passed first). Handed back to be said, with the
/// 504s it took and the lane's reads it moved up (`catchup_down`,
/// `catchup_moved`): each ended catch-up once every read sent in it has had
/// `wait` -- the replica's wait, `fennec_http::AFTER_WAIT`, and a second --
/// to come back; at the run's end (`stop`) each at once, one still open
/// cut by it.
fn catch_look(
    sh: &Shared,
    doings: &Doings,
    bound: u64,
    wait: u64,
    stop: bool,
    held: &mut dyn FnMut() -> Option<u64>,
) -> Vec<Catch> {
    let now = sh.ns();
    let through = sh.replica_ok.load(Ordering::SeqCst) && !sh.restarting(now);
    let behind = behind_s(sh.pool_slots(), now);
    let newest = sh.max_seq.load(Ordering::SeqCst);
    let open = {
        let mut c = sh.catches.lock().unwrap();
        c.last_mut().filter(|x| x.to == u64::MAX).and_then(|x| {
            x.on_schedule(now, behind, through, newest);
            x.on.map(|_| x.from)
        })
    };
    if let (Some(from), false) = (open, stop) {
        let held = held();
        let now = sh.ns();
        let mut c = sh.catches.lock().unwrap();
        if let Some(x) = c.last_mut().filter(|x| x.from == from) {
            if x.settle(now, held, bound) {
                sh.catching.store(false, Ordering::SeqCst);
                let ms = match (x.end, x.on) {
                    (CatchEnd::Caught, Some((on, _))) => ((x.to - on) / 1_000_000) as i64,
                    _ => -1,
                };
                doings.figure("catchup_replica_ms", ms.to_string());
                if let Some((on, _)) = x.on {
                    doings.figure(
                        "catchup_on_ms",
                        (on.saturating_sub(x.back) / 1_000_000).to_string(),
                    );
                }
            }
        }
    }
    let now = sh.ns();
    let mut c = sh.catches.lock().unwrap();
    if stop {
        for x in c.iter_mut().filter(|x| x.to == u64::MAX) {
            x.to = now;
            x.end = CatchEnd::RunEnd;
        }
        sh.catching.store(false, Ordering::SeqCst);
    }
    let said: Vec<Catch> = c
        .iter_mut()
        .filter(|x| !x.said && x.to != u64::MAX && (stop || now >= x.to.saturating_add(wait)))
        .map(|x| {
            x.said = true;
            *x
        })
        .collect();
    drop(c);
    for x in &said {
        doings.figure("catchup_down", x.down.to_string());
        doings.figure("catchup_moved", (x.moved_milli / 1000).to_string());
    }
    said
}

/// A restart's catch-up on its line (`catch_up`): how long the loop took to
/// be back on its schedule from the server answering again, and the
/// replica to hold the change answered then from there -- or that it did
/// not within `bound` (`--replica-back-max`), its 504s errors from there,
/// or the next restart or the run's end came first -- and the reads
/// through the replica a 504 answered in it, the restart's, and the
/// lane's it moved up.
fn catch_said(x: &Catch, bound: u64) -> String {
    let s = |a: u64, b: u64| a.saturating_sub(b) as f64 / 1e9;
    let back = match x.back {
        u64::MAX => "the server not answering again".to_string(),
        b => match x.on {
            Some((on, _)) => format!(
                "the loop back on its schedule {:.1} s after the server answered again",
                s(on, b)
            ),
            None => format!(
                "the loop not back on its schedule, with reads through the replica again, in the {:.1} s after the server answered again",
                s(x.to, b)
            ),
        },
    };
    let end = match x.end {
        CatchEnd::Cut => "cut by the next restart",
        _ => "cut by the run's end",
    };
    let how = match (x.end, x.on) {
        (CatchEnd::Caught, Some((on, seq))) => format!(
            "over: {back}, the replica holding change {seq}, the newest answered then, {:.1} s after that",
            s(x.to, on)
        ),
        (CatchEnd::Bound, Some((_, seq))) => format!(
            "NOT over: {back}, the replica short of change {seq}, the newest answered then, {} s after that (--replica-back-max): a 504 from there an error",
            bound / 1_000_000_000
        ),
        (_, Some((on, seq))) => format!(
            "{end}: {back}, the replica short of change {seq}, the newest answered then, {:.1} s after that",
            s(x.to, on)
        ),
        (_, None) => format!("{end}: {back}"),
    };
    format!(
        "minute {}: the restart's catch-up {how}; {} reads through the replica a 504 answered in it, the restart's, counted as down; {} of the lane's due in it moved up, never sent",
        x.minute,
        x.down,
        x.moved_milli / 1000
    )
}

/// Docker's word on the primary's process as it ended: its `die`, with
/// its exit code, or the kernel's `oom` kill of a process of its cgroup,
/// at `vm` (unix ns on the VM's clock), read at `at` (ns from `t0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DockerEv {
    at: u64,
    vm: u64,
    die: bool,
    exit: Option<i64>,
}

impl DockerEv {
    /// `docker events`' line as `follow_events` asks it, `{{.TimeNano}}
    /// {{.Action}} {{.Actor.Attributes.name}} {{index .Actor.Attributes
    /// "exitCode"}}`, of the container `name` alone: Docker's `container=`
    /// filter may take a name's prefix too, and the server's own watches
    /// -- `<server>-mem`, `<server>-look`, which ends every minute --
    /// begin with its name.
    fn parse(l: &str, at: u64, name: &str) -> Option<DockerEv> {
        let mut w = l.split_whitespace();
        let vm = w.next()?.parse().ok()?;
        let die = match w.next()? {
            "die" => true,
            "oom" => false,
            _ => return None,
        };
        if w.next()? != name {
            return None;
        }
        Some(DockerEv {
            at,
            vm,
            die,
            exit: w.next().and_then(|c| c.parse().ok()),
        })
    }
}

/// `program` with `args` under a shell that ends it once this process
/// lets go of the shell's input -- dropped, or this process gone, killed
/// or interrupted as it may be: a `docker events` follows forever and
/// writes only on an event, so with nothing to end it, it outlived a
/// killed `ycsb` until its next line found no reader. Only the program
/// holds the output: it ending closes it, the reader sees the end, and the
/// shell is let go. The shell is a process group of its own: Ctrl-C in a
/// terminal signals the foreground group, which held `ycsb`, the shell
/// and the program -- the shell died of it before it could `kill $p`, and
/// the program, an asynchronous list's, ignores SIGINT, so it lived on.
/// Out of that group, the shell outlives `ycsb` and ends the program.
fn tied(program: &str, args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.args([
        "-c",
        "\"$@\" </dev/null & p=$!; exec >/dev/null; read -r x; kill $p 2>/dev/null; wait $p",
        "sh",
        program,
    ])
    .args(args)
    .stdin(std::process::Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    c
}

/// Follows Docker's events of the primary's container for the run's
/// length -- one `docker events` process, no polling -- and hands each
/// `die` and `oom` to the minute's loop (`events`): a death between two
/// minutes is found as it happens, not at the next minute's look, which
/// the failures it cut meanwhile were counted as errors before. Started
/// again 5 s after it ends; ended with this process (`tied`).
fn follow_events(sh: &Shared, kit: &Kit, events: mpsc::Sender<DockerEv>) {
    use std::process::Stdio;
    let server = kit.server();
    let filter = format!("container={server}");
    while !sh.stop.load(Ordering::Relaxed) {
        let child = tied(
            "docker",
            &[
                "events",
                "--filter",
                &filter,
                "--filter",
                "event=die",
                "--filter",
                "event=oom",
                "--format",
                "{{.TimeNano}} {{.Action}} {{.Actor.Attributes.name}} {{index .Actor.Attributes \"exitCode\"}}",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
        if let Ok(mut child) = child {
            let (tx, lines) = mpsc::channel::<(u64, String)>();
            if let Some(out) = child.stdout.take() {
                let t0 = sh.t0;
                std::thread::spawn(move || {
                    for l in BufReader::new(out).lines().map_while(Result::ok) {
                        if tx.send((t0.elapsed().as_nanos() as u64, l)).is_err() {
                            break;
                        }
                    }
                });
            }
            loop {
                if sh.stop.load(Ordering::Relaxed) {
                    break;
                }
                match lines.recv_timeout(Duration::from_millis(500)) {
                    Ok((at, l)) => {
                        if let Some(ev) = DockerEv::parse(&l, at, &server) {
                            let _ = events.send(ev);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            // Its input let go: the shell ends `docker events`, then itself.
            drop(child.stdin.take());
            let _ = child.wait();
        }
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && !sh.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The primary as Docker has it now (`look`, `docker inspect`), Docker's
/// events read so far kept in `evs` -- a death's `die` and `oom` for its
/// line: its `Exit` where it is not running -- a death, the run's kill
/// not sent. Docker sets the state as it reaps the process, after its
/// sockets close: a death of the moment before a look is found after it
/// (`gone_before`).
fn dead(
    look: impl FnOnce() -> Option<Exit>,
    events: &mpsc::Receiver<DockerEv>,
    evs: &mut Vec<DockerEv>,
) -> Option<Exit> {
    evs.extend(events.try_iter());
    look().filter(|x| !x.running)
}

/// The run's kill of minute `m`, sent at `k`: noted first (`kill_sent`),
/// so that a side job its end cuts finds it, then `docker kill` and
/// Docker's wait for the end. Hands back when the wait ended, the server's
/// state then (`docker inspect`), and whether `docker kill` found it not
/// running: what tells the kill's end from a death of the moment before
/// it (`gone_before`).
fn send_kill(bench: &Kit, sh: &Shared, m: u64, k: u64) -> (u64, Option<Exit>, bool) {
    // What the replica must hold before reads go through it again
    // (`restart`): the newest change answered before the kill.
    sh.replica_target
        .store(sh.max_seq.load(Ordering::SeqCst), Ordering::SeqCst);
    sh.kill_sent.store(k.max(1), Ordering::SeqCst);
    let (ok, out) = docker_within(
        Duration::from_secs(60),
        &["kill", "--signal", "KILL", &bench.server()],
    );
    let not_running = !ok && out.to_ascii_lowercase().contains("is not running");
    if !ok {
        sh.note(format!("docker kill at minute {m}: {out}"));
    }
    let (ok, out) = docker_within(Duration::from_secs(120), &["wait", &bench.server()]);
    if !ok {
        sh.note(format!("docker wait at minute {m}: {out}"));
    }
    let killed = sh.ns();
    (killed, bench.exit(&bench.server()), not_running)
}

/// Whether the server the run's kill, sent at `k`, found gone had died
/// before it was sent: a death, not the kill -- its writes judged from its
/// own time (`died`), earlier than `k`, and the run failed for it. The
/// looks before the send (`dead`) read Docker's state, which Docker sets
/// as it reaps the process, after its sockets close: a death in the moment
/// before the send -- an OOM kill in the compact the kill was to land in
/// -- is still running to them, and `docker kill` then finds it not
/// running, or kills nothing. So after Docker's wait: the kernel's OOM
/// kill (`OOMKilled`, which Docker sets from the same event as its `oom`
/// and clears at a start), or `docker kill` finding it not running
/// (`not_running`); else on this process's own clock, the one `k` is
/// read on, where it read the server: its last answer (`answered`) more
/// than `CLOSE_SLOP` before the send, and its exit's burst of closes
/// (`closed`, `u64::MAX` where none was read) after that answer and
/// before the send. A server read answering within the slop of the send
/// or after it is the kill's -- a planned kill's last answers come up to
/// its landing, tens of ms after `k` -- and a burst after the send is the
/// kill's own, a server that stopped answering before it hung, not dead.
/// Only where neither an answer within the slop nor a burst was read,
/// Docker's end (`FinishedAt`, `finished` on this process's clock by the
/// kept probe's offset against it, `Shared::vm_ns`) before the send by
/// more than that reading's error (`err`, `Shared::clock_err`). Read
/// through the wall clock at `t0` instead, a planned kill's end, 20-200
/// ms after its send, came before it once the two clocks had parted by
/// more than that -- some 60 ms by the 90th minute on this Mac -- and the
/// kill was judged a death; a death just before the send, the other way,
/// the kill.
fn gone_before(
    exit: &Exit,
    not_running: bool,
    answered: u64,
    closed: u64,
    finished: Option<u64>,
    err: u64,
    k: u64,
) -> bool {
    if exit.running {
        return false;
    }
    if exit.oom || not_running {
        return true;
    }
    if answered > 0 && answered.saturating_add(CLOSE_SLOP) >= k {
        return false;
    }
    if closed != u64::MAX {
        return closed < k;
    }
    finished.is_some_and(|f| f.saturating_add(err) < k)
}

/// The run's kill sent at `k` judged after Docker's wait, from the
/// server's state then (`exit`) and whether `docker kill` found it not
/// running: the state where it was a death of the moment before the send
/// (`gone_before`), read on what the run read of the server
/// (`Shared::answered`, `Shared::burst`) and Docker's end by the latest
/// probe. Stored as judged either way (`kill_judged`, a death's in
/// `kill_death` first), so that a side job the kill's end cut says whose
/// its cut was at once (`cut_by`): a death's timed before the job was
/// asked, never among the deaths it looks for, waited out `KILL_JUDGE_WAIT`
/// and was counted the kill's.
fn judge_kill(sh: &Shared, exit: Option<Exit>, not_running: bool, k: u64) -> Option<Exit> {
    let before = exit.filter(|x| {
        gone_before(
            x,
            not_running,
            sh.answered.load(Ordering::SeqCst),
            sh.burst().0,
            x.finished.and_then(|v| sh.vm_ns(v)),
            sh.clock_err(k),
            k,
        )
    });
    if before.is_some() {
        sh.kill_death.store(k.max(1), Ordering::SeqCst);
    }
    sh.kill_judged.store(k.max(1), Ordering::SeqCst);
    before
}

/// The kill of minute `m`, sent at `k`, judged its own (`judge_kill`), in
/// words: the server's last answer and its exit's burst of closes read
/// against the send -- a planned kill's come after it.
fn kill_said(sh: &Shared, m: u64, k: u64) -> String {
    let ms = |t: u64| (t as f64 - k as f64) / 1e6;
    let a = sh.answered.load(Ordering::SeqCst);
    let burst = match sh.burst() {
        (u64::MAX, _) => "no burst of closes read".to_string(),
        (c, spread) => format!(
            "its burst of closes {:+.0} ms ({BURST_CLOSES} within {:.1} ms)",
            ms(c),
            spread as f64 / 1e6
        ),
    };
    format!(
        "minute {m}: the kill judged the kill's: its server last read answering {:+.0} ms of the send, {burst}",
        ms(a)
    )
}

/// The first minute from `m` on with a kill on the run's schedule: the
/// kill a death came before.
fn next_kill(m: u64, o: &Opts) -> Option<u64> {
    (m.max(1)..o.minutes).find(|&k| {
        schedule(
            k,
            o.minutes,
            o.kill_every,
            o.kill_in_compact_every,
            o.compact_every,
            o.backup_every,
            o.backup_at,
        )
        .iter()
        .any(|d| matches!(d, Doing::Kill { .. }))
    })
}

/// Why the wait for the compact a kill is to land in ended (`compact_wait`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompactWait {
    /// Docker's `die` of the primary: the dry run's second death, sent a
    /// second into the kill's compact, held the run here for `WAIT_MAX`,
    /// the compact's cut counted as the kill's, no compact left under way
    /// to end.
    Died,
    /// The compact under way a second: the kill lands in it.
    UnderWay,
    /// It ended within the second: killed without.
    Ended,
    /// None of its own under way within `WAIT_MAX`: killed without.
    Timeout,
}

/// Whether the wait for the compact the kill is to land in ends, and why:
/// `evs` Docker's events read since it began, `since` when the compact
/// under way was asked (`u64::MAX` while none is), `now`, whether it
/// ended (`ended`) and how long the wait has been.
fn compact_wait(
    evs: &[DockerEv],
    since: u64,
    now: u64,
    ended: bool,
    waited: Duration,
) -> Option<CompactWait> {
    if evs.iter().any(|e| e.die) {
        return Some(CompactWait::Died);
    }
    if since != u64::MAX && now.saturating_sub(since) >= 1_000_000_000 {
        return Some(CompactWait::UnderWay);
    }
    if ended {
        return Some(CompactWait::Ended);
    }
    (waited >= WAIT_MAX).then_some(CompactWait::Timeout)
}

/// A death of the primary the run did not cause, found now: its time read
/// (`death_time`) -- the burst of closes its exit made, else Docker's
/// `die` (`evs`) or `FinishedAt` by the clock probe kept, else the minute's
/// look before (`look`) -- and said on a line of its own: how it ended,
/// its minute, how long before the kill due (`kill_due`, the minute's
/// where it was found as that kill was to be sent), and its memory at its
/// last look a second. The failures it cut before it was found are moved
/// from the errors to `down`; the server is started again as after a kill
/// (`restart`), its window from the death -- or from the kill's
/// announcement, `announced`, where it was found in it -- and its kill
/// time the death's, so a write a read later shows gone is judged from
/// when the server went, still held to `--loss-ms`. The long run of
/// 2026-11-25 sent its kill of minute 300 to a primary OOM-killed in the
/// compact that kill waited for, timed the death by its own kill, sent
/// after its announced second, and counted seven writes answered 1.2 s
/// before its kill -- all but a fraction of a second of it the harness's
/// own wait -- as gone past the 650 ms a kill may take.
#[allow(clippy::too_many_arguments)]
fn died(
    bench: &Kit,
    sh: &Shared,
    doings: &Doings,
    o: &Opts,
    exit: Exit,
    evs: &mut Vec<DockerEv>,
    look: u64,
    kill_due: Option<u64>,
    announced: Option<u64>,
    sampler: Option<Sampler>,
    longest: &mut u64,
) -> u64 {
    let (from, at, m) = found_dead(sh, doings, o, exit, evs, look, kill_due, announced);
    restart(bench, sh, doings, (from, at, m), None, sampler, o, longest)
}

/// [`died`] but its restart, which is Docker's: the death timed, said and
/// counted, its restart's window open from `from` with its time `at` for
/// the kill's -- what the checker judges the writes a read later shows
/// gone from (`Shared::kills`) -- and its minute. Hands back the three.
#[allow(clippy::too_many_arguments)]
fn found_dead(
    sh: &Shared,
    doings: &Doings,
    o: &Opts,
    exit: Exit,
    evs: &mut Vec<DockerEv>,
    look: u64,
    kill_due: Option<u64>,
    announced: Option<u64>,
) -> (u64, u64, u64) {
    let event = evs.iter().rev().find(|e| e.die).map(|e| e.vm);
    let oom_event = evs
        .iter()
        .rev()
        .find(|e| !e.die)
        .and_then(|e| sh.vm_ns(e.vm));
    evs.clear();
    let docker = match event.and_then(|v| sh.vm_ns(v)) {
        Some(t) => Some((t, Source::Event)),
        None => exit
            .finished
            .and_then(|v| sh.vm_ns(v))
            .map(|t| (t, Source::Finished)),
    };
    let (closed, spread) = sh.burst();
    let err = docker.map_or(0, |(t, _)| sh.clock_err(t));
    let (at, source, answered) = death_time(
        closed,
        sh.answered.load(Ordering::SeqCst),
        docker,
        err,
        look,
    );
    // An answer read after the burst it is timed by, within the slop: how
    // late -- what `CLOSE_SLOP` is measured against.
    let late = match (source, answered) {
        (Source::Close, Some(a)) if a > closed => Some(a - closed),
        _ => None,
    };
    sh.deaths.lock().unwrap().push(at);
    // Its restart's window opened first -- from the kill's announcement
    // where it was found in it, the window that opened, from whichever
    // came first -- so that a failure from here on is `down`, then the
    // failures it cut before it was found moved there.
    let from = match announced {
        Some(a) => a.min(at),
        None => at,
    };
    {
        let mut w = sh.windows.lock().unwrap();
        match w.last_mut() {
            Some(x) if announced == Some(x.0) && x.2 == u64::MAX => *x = (from, at, u64::MAX),
            _ => w.push((from, at, u64::MAX)),
        }
    }
    sh.restart_to.store(u64::MAX, Ordering::SeqCst);
    sh.restart_from.store(from, Ordering::SeqCst);
    let moved = sh.death_errors(at);
    let m = at / 60_000_000_000 + 1;
    let due = kill_due.or_else(|| next_kill(m, o));
    let (before, before_s) = before_kill(at, due);
    // Found in the kill's announced second -- its compact, its wait -- the
    // kill not sent: said so, not as the side jobs the kill waited for.
    let in_kill = announced.map(|a| at.saturating_sub(a));
    let before = match in_kill {
        Some(d) => format!(
            "{:.1} s after the kill due at minute {} was announced, before it was sent",
            d as f64 / 1e9,
            due.unwrap_or(m)
        ),
        None => before,
    };
    let (last, life) = {
        let x = sh.mem[0].lock().unwrap();
        (x.last.map(|l| l.1), x.life)
    };
    let ms = |a: u64, b: u64| (a as f64 - b as f64) / 1e6;
    say(format!(
        "the server DIED in minute {m}, at {} ({:.1} s into it): {}; {before}; its time {}{}{}{}{}{}{}",
        utc(sh.t0_wall + at),
        (at % 60_000_000_000) as f64 / 1e9,
        exit.said(&o.memory),
        source.said(),
        // How close together the burst's closes came: what `BURST_SPAN`
        // is measured against.
        match source {
            Source::Close => format!(
                " ({BURST_CLOSES} closes within {:.1} ms)",
                spread as f64 / 1e6
            ),
            // Docker's, read through the clock's probe: at the latest
            // that reading allows (`death_time`).
            Source::Event | Source::Finished => {
                format!(", {:.0} ms past its reading, the probe's error", err as f64 / 1e6)
            }
            Source::Look => String::new(),
        },
        match (answered, late) {
            (Some(_), Some(l)) => format!(
                ", its last answer read {:.0} ms after the burst's first close",
                l as f64 / 1e6
            ),
            (Some(a), None) => format!(", {:.0} ms after its last answer", ms(at, a)),
            (None, _) => String::new(),
        },
        match (source, docker) {
            (Source::Close, Some((t, _))) => format!(
                "; Docker's {} {:+.0} ms of it",
                if event.is_some() { "die" } else { "FinishedAt" },
                ms(t, at)
            ),
            _ => String::new(),
        },
        match oom_event {
            Some(t) => format!(", its oom {:+.0} ms", ms(t, at)),
            None => String::new(),
        },
        match (last, life) {
            (Some(l), Some(p)) => format!(
                "; its last look a second anon {} MB and dirty {} MB, its life's most {} MB and {} MB",
                l.anon >> 20,
                l.dirty >> 20,
                p.anon >> 20,
                p.dirty >> 20
            ),
            _ => "; its memory not watched".into(),
        },
        match moved {
            0 => String::new(),
            n => format!("; {n} failures it cut before it was found counted as down, not errors"),
        }
    ));
    doings.deaths.fetch_add(1, Ordering::Relaxed);
    if exit.oom {
        doings.oom.fetch_add(1, Ordering::Relaxed);
    }
    for (k, v) in [
        ("death_oom", (exit.oom as u8).to_string()),
        (
            "death_exit",
            exit.code.map_or("-1".into(), |c| c.to_string()),
        ),
        ("death_at_s", format!("{:.3}", at as f64 / 1e9)),
        ("death_source", (source as u8).to_string()),
        ("death_down", moved.to_string()),
    ] {
        doings.figure(k, v);
    }
    if let Some(s) = before_s {
        doings.figure("death_kill_due_s", format!("{s:.1}"));
    }
    if let Some(d) = in_kill {
        doings.figure("death_announced_ms", (d / 1_000_000).to_string());
    }
    if let Some(a) = answered {
        doings.figure("death_answer_ms", format!("{:.0}", ms(at, a)));
    }
    if let Some(l) = late {
        doings.figure("death_late_ms", format!("{:.0}", l as f64 / 1e6));
    }
    if let Some((t, _)) = docker {
        doings.figure("death_docker_ms", format!("{:.0}", ms(t, at)));
    }
    if let Some(l) = last {
        doings.figure("death_anon", l.anon.to_string());
        doings.figure("death_dirty", l.dirty.to_string());
    }
    sh.restart_behind.store(
        behind_s(sh.pool_slots(), sh.ns()).is_some_and(|b| b > ON_SCHEDULE_S),
        Ordering::SeqCst,
    );
    (from, at, m)
}

/// The replica's compaction counter (`fennec_auto_compactions_total`) as
/// the watch reads it at each window's edges (`compactions`): as last
/// read and when, as the open window began, and whether that read failed
/// and the last one stood for it (`stale`).
#[derive(Default)]
struct Counter {
    last: Option<(u64, u64)>,
    base: Option<u64>,
    stale: bool,
}

/// What the replica's counter said of a window as it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Count {
    /// Read as it began and as it ended: `moved` whether it rose. `stale`
    /// where the read as it began failed and the last one before stood for
    /// it: a compaction ended between the two -- one the watch missed --
    /// read as this window's.
    Read { moved: bool, stale: bool },
    /// Lower at its end than at its start: the replica started again in
    /// it, its compaction thread numbered as before -- judged by the count
    /// since it started (`since`), all of it the window's. Taken as moved
    /// (`n != b`), it turned a window none into a compaction.
    Reset { since: u64 },
    /// Not read at its end, or not at all: the watch's word.
    Unread,
}

impl Count {
    /// Whether the window was a compaction: the counter's word where it
    /// has one, the watch's where it has none.
    fn was(self) -> bool {
        match self {
            Count::Read { moved, .. } => moved,
            Count::Reset { since } => since > 0,
            Count::Unread => true,
        }
    }
}

impl Counter {
    /// An edge of the watch's taken, `read` reading the counter. A window
    /// opens before its base is read: the read waits on the replica's
    /// lock (`/_metrics`), 5 s and 10 s with its connect, and a read
    /// through the replica answered meanwhile took the class it would have
    /// had without the window -- steady or a restart's, the fourth dry
    /// run's minute 118 again. The counter counts at a compaction's end
    /// (fennec-http's `link::compact`), so the base is the same either way.
    /// A window's end is judged by the counter's rise (`Count`) -- a
    /// counter lower than the base said as a reset, never as a rise -- and
    /// a cut takes the watch's word, as before.
    fn edge(&mut self, sh: &Shared, edge: Option<Edge>, read: &mut dyn FnMut() -> Option<u64>) {
        let minute = |t: u64| t / 60_000_000_000 + 1;
        match edge {
            None => {}
            Some(Edge::Began(from)) => {
                replica_compaction(sh, edge, Count::Unread);
                let n = read();
                self.stale = n.is_none();
                self.base = n.or(self.last.map(|x| x.0));
                match (n, self.last) {
                    (Some(n), _) => self.last = Some((n, sh.ns())),
                    (None, Some((_, at))) => say(format!(
                        "the replica's compaction counter not read as its window of minute {} opened: the read of minute {} stands for its base",
                        minute(from),
                        minute(at)
                    )),
                    (None, None) => say(format!(
                        "the replica's compaction counter not read as its window of minute {} opened, nor before: the watch's word will settle it",
                        minute(from)
                    )),
                }
            }
            Some(Edge::Ended(_)) => {
                let n = read();
                let count = match (self.base, n) {
                    (Some(b), Some(n)) if n < b => Count::Reset { since: n },
                    (Some(b), Some(n)) => Count::Read {
                        moved: n > b,
                        stale: self.stale,
                    },
                    _ => Count::Unread,
                };
                if let Some(n) = n {
                    self.last = Some((n, sh.ns()));
                }
                replica_compaction(sh, edge, count);
            }
            Some(Edge::Cut(_) | Edge::Lost(_)) => replica_compaction(sh, edge, Count::Unread),
        }
    }
}

/// Why the reads' window of the replica's was cut before the watch saw
/// its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    /// A worker held `HELD_MAX` operations for it.
    Held,
    /// Its watch gone `R_WATCH_GONE` and more at a minute's look: lost.
    Unwatched,
    /// Open past what a compaction of the replica's file plausibly takes
    /// (`plausible_ms`).
    Overlong,
}

/// The reads' window of the replica's `id` cut at `t` for `why`, unless it
/// is settled already or another has begun: said, and its operations
/// counted as if no window had been -- each its class otherwise
/// (`Classes`, `Hists::settle`). Whether it was. It counts nothing: the
/// compaction's end is still the watch's to see and the counter's to
/// judge (`replica_compaction`), and its caller counts what it found --
/// past its bound (`r_compacts_overlong`), or lost (`r_compacts_lost`).
/// Before, every cut counted as one whose end was not seen
/// (`r_compacts_cut`): a window that ended past its bound, its end seen,
/// failed the watch's criterion as "cut before their end was seen".
fn cut_r_window(sh: &Shared, id: u64, t: u64, why: Cut) -> bool {
    let minute = |t: u64| t / 60_000_000_000 + 1;
    let mut win = sh.r_window.lock().unwrap();
    let (now, from, _) = *win;
    if id == 0 || now != id || sh.r_verdict(id).is_some() {
        return false;
    }
    *win = (id, u64::MAX, u64::MAX);
    {
        let mut v = sh.r_verdicts.lock().unwrap();
        if v.len() < id as usize {
            v.resize(id as usize, false);
        }
    }
    drop(win);
    let since = match sh.r_since.load(Ordering::SeqCst) {
        u64::MAX => from,
        s => s,
    };
    let on = t.saturating_sub(since) as f64 / 1e9;
    say(format!(
        "the replica's compaction window of minute {} cut in minute {}, {on:.1} s on: {}; its operations counted as if no window had been",
        minute(since),
        minute(t),
        match why {
            Cut::Held => format!("a worker held {HELD_MAX} operations for it, the most it holds"),
            Cut::Unwatched => format!(
                "its watch gone {:.0} s, LOST",
                t.saturating_sub(sh.r_down_since.load(Ordering::SeqCst)) as f64 / 1e9
            ),
            Cut::Overlong => format!(
                "OPEN PAST the {:.0} s a compaction of the replica's file plausibly takes",
                sh.r_bound_ms.load(Ordering::SeqCst) as f64 / 1e3
            ),
        }
    ));
    true
}

/// The longest a compaction of the replica's own can plausibly take, in
/// ms, its file `r_file` bytes and the slowest of the primary's compacts
/// on the run `p_ms_per_gb` ms a GB of the primary's file (0 for none):
/// `R_COMPACT_MS_PER_GB` a GB, or `R_COMPACT_PRIMARY_TIMES` the primary's
/// where that is more, and `R_COMPACT_FLOOR_MS` at the least.
fn plausible_ms(r_file: u64, p_ms_per_gb: u64) -> u64 {
    let per_gb = R_COMPACT_MS_PER_GB.max(R_COMPACT_PRIMARY_TIMES.saturating_mul(p_ms_per_gb));
    let ms = r_file as u128 * per_gb as u128 / 1_000_000_000;
    (ms.min(u64::MAX as u128) as u64).max(R_COMPACT_FLOOR_MS)
}

/// The replica's latest compaction held to its bounds at the minute's look
/// `now`, `watched` whether the watch looks at its thread. Open past what
/// a compaction of its file plausibly takes (`Shared::r_bound_ms`), the
/// watch looking, it is no compaction a server ends but a thread that
/// never sleeps again -- counted (`r_compacts_overlong`), a FAIL of its
/// own, and its reads' window cut, never an excuse for them. Open with
/// the watch gone `R_WATCH_GONE` and more, it is lost (`r_compacts_lost`),
/// which no run passes the watch's criterion with, and its reads' window
/// cut. Both are judged by the watch's compaction (`r_since`), not its
/// reads' window: a thread found past its bound while watched, or one a
/// worker's held had cut, then its watch gone with it never sleeping
/// again, was never lost -- its counter unmoved, nothing seen, both
/// criteria passed.
fn bound_r_window(sh: &Shared, now: u64, watched: bool) {
    // The two read together: a window begins and ends under the lock.
    let (id, since) = {
        let w = sh.r_window.lock().unwrap();
        (w.0, sh.r_since.load(Ordering::SeqCst))
    };
    if id == 0 || since == u64::MAX {
        return;
    }
    let minute = |t: u64| t / 60_000_000_000 + 1;
    let bound = sh
        .r_bound_ms
        .load(Ordering::SeqCst)
        .saturating_mul(1_000_000);
    if watched && now.saturating_sub(since) > bound {
        if sh.r_overlong.swap(id, Ordering::SeqCst) != id {
            sh.r_compacts_overlong.fetch_add(1, Ordering::Relaxed);
            // Its reads' window cut before -- a worker's held at the most:
            // past its bound all the same.
            if !cut_r_window(sh, id, now, Cut::Overlong) {
                say(format!(
                    "the replica's compaction thread at work {:.0} s in minute {}, OPEN PAST the {:.0} s a compaction of its file plausibly takes",
                    now.saturating_sub(since) as f64 / 1e9,
                    minute(now),
                    bound as f64 / 1e9
                ));
            }
        }
        return;
    }
    let down = sh.r_down_since.load(Ordering::SeqCst);
    if !watched
        && down != u64::MAX
        && now.saturating_sub(down) >= R_WATCH_GONE
        && sh.r_lost.swap(id, Ordering::SeqCst) != id
    {
        sh.r_compacts_lost.fetch_add(1, Ordering::Relaxed);
        if !cut_r_window(sh, id, now, Cut::Unwatched) {
            say(format!(
                "the replica's compaction thread at work from minute {}, its watch gone {:.0} s in minute {}: LOST, its reads' window cut before",
                minute(since),
                now.saturating_sub(down) as f64 / 1e9,
                minute(now)
            ));
        }
    }
}

/// A restart's span as the reads through the replica see it, or a window
/// of the replica's seen to end as a compaction, done: where the latest of
/// each overlap, the window's reads waited out both -- a kill landing in
/// one, its workers held by the compaction and then by the primary's
/// restart and the replica's return, or the other way round -- and their
/// bound is the two together (`r_compact_kill_ms`), not the compaction's
/// alone. Whichever is done second pairs them. The fifth dry run of
/// 2026-11-24's kill of minute 60 landed in the replica's compaction of
/// that minute -- it compacts every 20 minutes or so, the kills come every
/// 30 -- its 13 888 operations due in the restart all the compaction's.
fn restart_met(sh: &Shared, restart: Option<Span>, window: Option<Span>) {
    let mut m = sh.r_met.lock().unwrap();
    if restart.is_some() {
        m.0 = restart;
    }
    if window.is_some() {
        m.1 = window;
    }
    if let (Some(r), Some(w)) = *m {
        if r.0 <= w.1 && w.0 <= r.1 {
            let ns = (w.1 - w.0).saturating_add(r.1 - r.0);
            sh.r_kill_ms
                .fetch_max(ns.div_ceil(1_000_000), Ordering::Relaxed);
        }
    }
}

/// A compaction of the replica's own begun or ended, as its window for
/// the reads through it, and said on the log. One whose end was seen is
/// judged by its counter (`Count::was`) -- or, where it could not say, by
/// the watch's word: a compaction where it moved, its length kept
/// (`r_compact_ms`), and none where it did not, its window closed to
/// nothing and the operations held for it counted as they would be
/// otherwise (`Classes`, `Hists::settle`); one judged across a reset, on a
/// stale base or on no read is counted so on the minute's line and said.
/// One that ended past what its file plausibly takes is counted past its
/// bound (`r_compacts_overlong`) and its reads' window cut, as a look
/// would have (`bound_r_window`); its end, seen, is judged by the counter
/// all the same, and so is that of one whose reads' window was cut before
/// -- a worker's held, or a look finding it past its bound -- neither
/// excusing its reads: before, both were counted as cut before their end
/// was seen (`r_compacts_cut`), and the watch's criterion failed on an end
/// it saw. One its watch lost at a look (`r_lost`) has its end said and
/// nothing more. One whose end was not seen keeps the watch's word for its
/// reads and is counted as cut, never as an end -- a watch that went in
/// the middle of one and saw it at work again counted it twice, and the
/// run's end one more -- and as lost where the watch lost it
/// (`Edge::Lost`).
fn replica_compaction(sh: &Shared, edge: Option<Edge>, count: Count) {
    let minute = |t: u64| t / 60_000_000_000 + 1;
    let mut win = sh.r_window.lock().unwrap();
    let (id, from, _) = *win;
    let (to, unseen, lost) = match edge {
        None => return,
        Some(Edge::Began(from)) => {
            *win = (id + 1, from, u64::MAX);
            sh.r_since.store(from, Ordering::SeqCst);
            drop(win);
            say(format!(
                "the replica began a compaction of its own in minute {}",
                minute(from)
            ));
            return;
        }
        Some(Edge::Ended(to)) => (to, false, false),
        Some(Edge::Cut(to)) => (to, true, false),
        Some(Edge::Lost(to)) => (to, true, true),
    };
    let since = match id {
        0 => u64::MAX,
        _ => sh.r_since.swap(u64::MAX, Ordering::SeqCst),
    };
    if since == u64::MAX {
        return;
    }
    let took = to.saturating_sub(since);
    // Its reads' window cut before its end: a worker's held, a look past
    // its bound or with its watch gone.
    let settled = sh.r_verdict(id).is_some();
    let verdict = |was: bool| {
        let mut v = sh.r_verdicts.lock().unwrap();
        if v.len() < id as usize {
            v.resize(id as usize, was);
        }
    };
    if unseen {
        sh.r_compacts_cut.fetch_add(1, Ordering::Relaxed);
        if lost && sh.r_lost.swap(id, Ordering::SeqCst) != id {
            sh.r_compacts_lost.fetch_add(1, Ordering::Relaxed);
        }
        if !settled {
            *win = (id, from, to);
            verdict(true);
        }
        drop(win);
        say(format!(
            "the replica's compaction of its own from minute {} {} in minute {}, {:.1} s on, its end not seen: not counted{}",
            minute(since),
            if lost { "LOST by its watch" } else { "cut" },
            minute(to),
            took as f64 / 1e9,
            if settled { ", its reads' window cut before" } else { "" }
        ));
        return;
    }
    if sh.r_lost.load(Ordering::SeqCst) == id {
        drop(win);
        say(format!(
            "the replica's compaction thread asleep again in minute {}, {:.1} s after it began in minute {}: its watch lost it before, not counted",
            minute(to),
            took as f64 / 1e9,
            minute(since)
        ));
        return;
    }
    let bound = sh
        .r_bound_ms
        .load(Ordering::SeqCst)
        .saturating_mul(1_000_000);
    let overlong = took > bound;
    if overlong && sh.r_overlong.swap(id, Ordering::SeqCst) != id {
        sh.r_compacts_overlong.fetch_add(1, Ordering::Relaxed);
    }
    let was = count.was();
    // Its reads the compaction's: an end within its bound, its window not
    // cut before.
    let held = was && !overlong && !settled;
    if !settled && !overlong {
        *win = match was {
            true => (id, from, to),
            false => (id, u64::MAX, u64::MAX),
        };
        verdict(was);
    }
    drop(win);
    if overlong && !settled {
        cut_r_window(sh, id, to, Cut::Overlong);
    }
    let judged = match count {
        Count::Read { stale: false, .. } => "",
        Count::Read { stale: true, .. } => {
            sh.r_compacts_stale.fetch_add(1, Ordering::Relaxed);
            ", judged on its counter's read before it began, the read as it began failed"
        }
        Count::Reset { .. } => {
            sh.r_compacts_reset.fetch_add(1, Ordering::Relaxed);
            ", its counter lower than as it began -- the replica started again in it -- judged by its count since"
        }
        Count::Unread => {
            sh.r_compacts_unread.fetch_add(1, Ordering::Relaxed);
            ", its counter not read: the watch's word taken"
        }
    };
    let reads = match (overlong, settled) {
        (false, false) => "",
        (true, _) => ", PAST what a compaction of its file plausibly takes: its reads counted as if no window had been",
        (false, true) => ", its reads' window cut before: its reads counted as if no window had been",
    };
    let said = match was {
        true => {
            sh.r_compacts_seen.fetch_add(1, Ordering::Relaxed);
            if held {
                sh.r_compact_ms
                    .fetch_max(took.div_ceil(1_000_000), Ordering::Relaxed);
                restart_met(sh, None, Some((since, to)));
            }
            format!(
                "the replica's compaction of its own ended in minute {}, {:.1} s{judged}{reads}",
                minute(to),
                took as f64 / 1e9,
            )
        }
        false => {
            sh.r_compacts_false.fetch_add(1, Ordering::Relaxed);
            format!(
                "the replica's compaction thread at work from minute {} for {:.1} s, its compaction counter counting none in it{judged}: no compaction, its reads counted as they would be{}",
                minute(since),
                took as f64 / 1e9,
                if overlong { ", PAST what a compaction of its file plausibly takes" } else { "" }
            )
        }
    };
    say(said);
}

/// The file descriptors the run holds at the most: two a connection of
/// `http::Http`'s (its socket and the reader's clone of it); the pool's
/// one each, the replica lane's two -- to the replica, and to the primary
/// from a kill until the replica is back -- or, with no lane, the pool's
/// two; one a subscription's stream; and `FDS_BESIDE` for the rest. The
/// lane took the run from about 200 to some 380, and 640 at a restart,
/// past macOS's default soft limit of 256: a connect past it fails
/// (`EMFILE`), counted as the server's error.
fn fds_needed(o: &Opts) -> u64 {
    let https = match o.replica_threads {
        0 => 2 * o.threads,
        r => o.threads + 2 * r,
    };
    2 * https as u64 + o.subscriptions as u64 + FDS_BESIDE
}

/// What else the run holds beside its connections: the standard three, the
/// side job's connection, the minute's scrapes and probes, the watches'
/// and `docker`'s pipes, the seconds' log and the results' appends.
const FDS_BESIDE: u64 = 64;

/// This process's soft limit on open files, as a shell it starts inherits
/// it (`ulimit -n`): `None` where none, or unread.
fn open_files_limit() -> Option<u64> {
    let out = std::process::Command::new("sh")
        .args(["-c", "ulimit -n"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn run(o: &Opts) {
    // Held to the limit before anything starts: past it, every connect
    // fails as the servers' error, the run hours on (run-soak.sh raises
    // it).
    let need = fds_needed(o);
    if let Some(have) = open_files_limit().filter(|&have| have < need) {
        eprintln!(
            "the soft limit on open files is {have}, under the {need} the run holds at a restart: raise it (`ulimit -n {need}`; run-soak.sh does)"
        );
        std::process::exit(2);
    }
    if let Some(p) = &o.progress {
        *PROGRESS.lock().unwrap() = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok();
    }
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let http_token = secret();
    // Every request this process sends carries it, the load's first.
    http::bearer(&http_token);
    let bench = Bench(Kit {
        name: o.name.clone(),
        image: o.image.clone(),
        tools: o.tools.clone(),
        memory: o.memory.clone(),
        sync: o.sync.clone(),
        token: secret(),
        http_token,
        replication_buffer: o.replication_buffer,
        port: http::free_port(),
        replica_port: http::free_port(),
        keep: Arc::new(AtomicBool::new(false)),
        kept: o.kept.clone(),
        logs: o.logs.clone(),
    });
    if let Some(dir) = &o.logs {
        let _ = std::fs::create_dir_all(dir);
    }
    bench.clean();
    let (ok, out) = docker(&["network", "create", &bench.net()]);
    assert!(ok, "docker network create: {out}");
    bench.start_primary();
    say(format!("{} started", bench.server()));
    assert!(
        bench.wait_health(bench.port, WAIT_MAX),
        "fennec-server did not answer within {} s of its start",
        WAIT_MAX.as_secs()
    );
    let addr = format!("127.0.0.1:{}", bench.port);
    load(&addr, o);
    bench.start_replica();
    say(format!("{} started", bench.replica()));
    assert!(
        bench.wait_health(bench.replica_port, WAIT_MAX),
        "the replica did not answer within {} s of its start",
        WAIT_MAX.as_secs()
    );
    let raddr = format!("127.0.0.1:{}", bench.replica_port);
    let seq = |a: &str| {
        super::metrics_page(a).map(|p| metric_sum(p.as_bytes(), "fennec_change_sequence"))
    };
    // The replica's own word on its follow (`/_replication/status`'s
    // `error`): what a wait says, and a refusal ends the run at once --
    // the dry run of 2026-11-22 waited its whole half hour, twice, on a
    // follow its primary had refused from the first second.
    let follow_said = || {
        let st = bench.status(bench.replica_port).unwrap_or_default();
        let error = st
            .find("\"error\":")
            .map(|at| st[at + 8..].trim_end_matches('}').to_string());
        (refused(&st), error)
    };
    // `setup`, the first follow: a server gone then -- the short run of
    // 2026-11-22's primary, OOM-killed the moment its replica first asked
    // it for an image -- ends the run at once, as no load has begun; the
    // end's catch-up waits its bound and says what it found.
    let caught_up = |within: Duration, what: &str, setup: bool| -> Option<f64> {
        let t = Instant::now();
        let mut said = Instant::now();
        let mut looked = Instant::now();
        loop {
            let (p, r) = (seq(&addr), seq(&raddr));
            if p.is_some() && p == r {
                return Some(t.elapsed().as_secs_f64());
            }
            if t.elapsed() >= within {
                return None;
            }
            let (refusal, error) = follow_said();
            if let Some(why) = refusal {
                stop_run(
                    &bench,
                    None,
                    o,
                    &format!("{what}: the replica's primary {why}"),
                );
            }
            if setup && looked.elapsed() >= Duration::from_secs(5) {
                looked = Instant::now();
                for (who, c) in [("primary", bench.server()), ("replica", bench.replica())] {
                    let (up, oom) = bench.state(&c);
                    if !up {
                        stop_run(
                            &bench,
                            None,
                            o,
                            &format!(
                                "{what}: the {who} is not running{}{}",
                                if oom {
                                    ", OOM-killed under --memory "
                                } else {
                                    ""
                                },
                                if oom { o.memory.as_str() } else { "" }
                            ),
                        );
                    }
                }
            }
            if said.elapsed() >= Duration::from_secs(60) {
                say(format!(
                    "{what}: the replica at change {r:?}, the primary at {p:?}, {:.0} s{}",
                    t.elapsed().as_secs_f64(),
                    error.map_or(String::new(), |e| format!("; the replica says {e}"))
                ));
                said = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    // The replica's first follow copies the loaded file: bounded by half
    // an hour, which a gigabyte over the VM's network takes a fraction of.
    let follow_max = 1_800;
    let follow = caught_up(Duration::from_secs(follow_max), "the first follow", true);
    say(format!(
        "the replica {}; {} connections and {} on the replica's lane at {}, {} subscriptions, --sync {}, --memory {}",
        match follow {
            Some(s) => format!("followed in {s:.0} s"),
            None => format!("did NOT catch up in {follow_max} s"),
        },
        o.threads,
        o.replica_threads,
        match o.rate > 0.0 {
            true => format!("{:.0} ops/s", o.rate),
            false => "the peak".into(),
        },
        o.subscriptions,
        o.sync,
        o.memory
    ));
    let mut settings = Settings::of(o);
    settings.cpus = vm_cpus();
    let mut v: Vec<(&str, String)> = settings.values();
    v.extend([
        ("started", started.to_string()),
        ("ttl", o.ttl.to_string()),
        ("slack", o.slack.to_string()),
        ("records", o.records.to_string()),
        ("rate", format!("{:.0}", o.rate)),
        ("follow_s", follow.unwrap_or(-1.0).to_string()),
        ("follow_max", follow_max.to_string()),
        ("load_before", format!("{:.2}", o.load_before)),
    ]);
    append_values(
        &o.out,
        "soak",
        &o.run_id,
        &format!("{}:{}:start", o.sync, o.label),
        &v,
    );
    // A run whose replica never followed reads nothing through it, waits
    // out every read sent there and compares nothing at its end: no soak.
    if follow.is_none() {
        let (_, error) = follow_said();
        stop_run(
            &bench,
            None,
            o,
            &format!(
                "the replica did not follow within {follow_max} s{}",
                error.map_or("; it said no error".into(), |e| format!("; it says {e}"))
            ),
        );
    }

    let ranges: Vec<(u64, u64)> = (0..o.subscriptions)
        .map(|i| {
            let mut rng = Rng::new(SEED ^ 0x5355_4253 ^ i as u64);
            let from = Scrambled::new().next(&mut rng, o.records.saturating_sub(64)) + 1;
            (from, from + 64)
        })
        .collect();
    let sh = Arc::new(Shared::new(o, addr.clone(), raddr.clone(), &ranges));
    let _ = sh.repl_token.set(bench.token.clone());
    // The VM's clock against this host's before the first death can need
    // it (`vm_ns`).
    if let Some(p) = clock_probe(&addr, &sh) {
        sh.clock(&p);
    }
    let mut handles = Vec::new();
    for w in 0..o.threads + o.replica_threads {
        let sh = Arc::clone(&sh);
        handles.push(std::thread::spawn(move || work(&sh, w)));
    }
    for which in 0..2 {
        let (sh, kit) = (Arc::clone(&sh), bench.0.clone());
        handles.push(std::thread::spawn(move || watch_memory(&sh, &kit, which)));
    }
    {
        let sh = Arc::clone(&sh);
        let log = o
            .logs
            .as_ref()
            .map(|d| d.join(format!("{}-seconds.tsv", o.name)));
        handles.push(std::thread::spawn(move || seconds(&sh, log)));
    }
    let (events_tx, events) = mpsc::channel::<DockerEv>();
    {
        let (sh, kit) = (Arc::clone(&sh), bench.0.clone());
        handles.push(std::thread::spawn(move || {
            follow_events(&sh, &kit, events_tx)
        }));
    }
    // Docker's events of the primary read and not yet a death's.
    let mut evs: Vec<DockerEv> = Vec::new();
    for i in 0..o.subscriptions {
        let sh = Arc::clone(&sh);
        handles.push(std::thread::spawn(move || subscribe(&sh, i)));
    }
    {
        let (sh, kit) = (Arc::clone(&sh), bench.0.clone());
        handles.push(std::thread::spawn(move || watch_replica(&sh, &kit)));
    }
    let doings = Arc::new(Doings::default());
    {
        let (sh, kit, doings) = (Arc::clone(&sh), bench.0.clone(), Arc::clone(&doings));
        let bound = o.replica_back_max.saturating_mul(1_000_000_000);
        handles.push(std::thread::spawn(move || {
            catch_up(&sh, &kit, &doings, bound)
        }));
    }
    let (jobs, side_jobs) = mpsc::channel::<Job>();
    let side = Side {
        sh: Arc::clone(&sh),
        doings: Arc::clone(&doings),
        kit: bench.0.clone(),
    };
    let side = std::thread::spawn(move || side.run(side_jobs));

    let loss = o.loss_ms * 1_000_000;
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
    // The last hour's latencies, and the restarts' and the replica's
    // compactions' over the run: those of the kills the loop met on its
    // schedule, which the report judges -- the primary's and the reads
    // through the replica apart -- and the others.
    let mut kept = Kept::new();
    let mut prev_majflt = None;
    let mut r_prev_majflt = None;
    // Each server's minute of looks a second, for the minute's say line.
    let mut peaks: [Option<MemSpan>; 2] = [None, None];
    let mut prev_oom = 0;
    // Each server's own compactions (`fennec_auto_compactions_total`), its
    // count kept across the primary's restarts.
    let mut auto = [(0u64, 0u64); 2];
    let mut totals = (0u64, 0u64, 0u64);
    let mut peak = Vec::new();
    // The peak's reads through the replica lane, of every operation, and
    // those its schedule dropped (in thousandths), over the minutes its
    // mean is taken over.
    let mut peak_lane = (0u64, 0u64, 0u64);
    let mut last = sh.t0;
    let mut life = 0u64;
    let mut env_failed = None;
    let mut faults_before = StreamFaults::default();
    let mut minutes_run = 0;
    let mut restart_ms_max = 0u64;
    let mut backup_id = 0u64;
    let mut clock_prev: Option<Probe> = None;
    // The CPU the two servers' cgroups and the VM's CPUs had used at the
    // last look.
    let mut cpu_prev: Option<(u64, u64, u64)> = None;
    let (mut lost_before, mut differ_before) = (0u64, 0u64);
    // Kills the restart's sweep has not read back after yet.
    let mut swept_after = 0usize;
    let floor = o.disk_floor;
    // The kills the schedule meant that found the server dead, the death
    // standing for them (`kill_died`).
    let kill_died = AtomicU64::new(0);
    for m in 1..=o.minutes {
        let at = sh.t0 + Duration::from_secs(60 * m);
        // The minute waited out on Docker's events: a death of the
        // primary between two minutes is found as it happens -- its `die`
        // -- and the server started again at once.
        loop {
            let left = at.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match events.recv_timeout(left) {
                Ok(ev) => {
                    evs.push(ev);
                    if !ev.die {
                        continue;
                    }
                    if let Some(x) = dead(|| bench.exit(&bench.server()), &events, &mut evs) {
                        let look = (m - 1) * 60_000_000_000;
                        let back = died(
                            &bench,
                            &sh,
                            &doings,
                            o,
                            x,
                            &mut evs,
                            look,
                            None,
                            None,
                            None,
                            &mut restart_ms_max,
                        );
                        evs.extend(events.try_iter());
                        evs.retain(|e| e.at > back);
                        life += 1;
                        prev_majflt = None;
                        prev_oom = 0;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(left);
                    break;
                }
            }
        }
        // And by its state, where Docker's events said nothing: died
        // between the minute's looks, timed by what closed.
        if let Some(x) = dead(|| bench.exit(&bench.server()), &events, &mut evs) {
            let look = (m - 1) * 60_000_000_000;
            let back = died(
                &bench,
                &sh,
                &doings,
                o,
                x,
                &mut evs,
                look,
                None,
                None,
                None,
                &mut restart_ms_max,
            );
            evs.extend(events.try_iter());
            evs.retain(|e| e.at > back);
            life += 1;
            prev_majflt = None;
            prev_oom = 0;
        }
        minutes_run = m;
        let now = Instant::now();
        let secs = now.duration_since(last).as_secs_f64();
        last = now;
        // The latencies of the minute, and of the hour to it: the steady
        // operations apart from the restarts' and the compacts' (`class`).
        let loop_behind = behind_s(sh.pool_slots(), sh.ns());
        let lane_behind = match o.replica_threads {
            0 => None,
            _ => behind_s(sh.lane_slots(), sh.ns()),
        };
        let mut h = Hists::new();
        let mut idle = 0;
        // The operations held for a replica's compaction its end settled
        // are counted as it said, the others kept for a later minute's --
        // but the last line's, which settles every one (`flush_held`).
        //
        // Settled here, before the watch's counters are read for the line
        // (`r_compacts_watched` and the rest, below): an end seen between
        // the two is on this line, its operations on the next. The order
        // stays, as no other closes it. A worker settles what it holds as
        // it ends its next operation (`Hists::took`), a few ms after the
        // end, into the figures the next line takes, so settling after the
        // counters would leave the same end's operations on the next line
        // still; only the verdicts, the counters and every worker's settle
        // under one lock would, on the workers' path. And no verdict pairs
        // a line's operations with its count: the criteria take the run's
        // operations (`r_compact_ops`, `p99_r_compact` on the end line)
        // and the longest window of any line, and a window across a
        // minute's end has its operations on both lines anyway -- the fifth
        // dry run's of minute 20 ended on its line 20 and all 144 302 of
        // its operations were on line 21.
        let verdict = |id: u64| sh.r_verdict(id);
        for s in &sh.slots {
            let mut g = s.hists.lock().unwrap();
            g.settle(&verdict);
            let held = std::mem::take(&mut g.held);
            let x = std::mem::replace(&mut *g, Hists::new());
            g.held = held;
            // The next line's most held begins at what is held now.
            g.held_max = g.held.len();
            drop(g);
            h.merge(&x);
            idle += (s.done.swap(0, Ordering::Relaxed) == 0) as u64;
        }
        let ops = sh.ops.swap(0, Ordering::Relaxed);
        let lane_ops = sh.lane_ops.swap(0, Ordering::Relaxed);
        let lane_dropped = sh.lane_dropped_milli.swap(0, Ordering::Relaxed);
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
        if mismatches > 0 {
            bench.keep_data(&format!("{mismatches} mismatches at minute {m}"));
        }
        let f = checker.faults;
        let faults = StreamFaults {
            missing: f.missing - faults_before.missing,
            twice: f.twice - faults_before.twice,
            order: f.order - faults_before.order,
            late: f.late - faults_before.late,
            silent: f.silent - faults_before.silent,
        };
        faults_before = f;
        let lost = checker.lost_writes - lost_before;
        lost_before = checker.lost_writes;
        let differ = checker.backups_differ - differ_before;
        differ_before = checker.backups_differ;
        // The keys a kill since the last sweep may have touched, read back
        // now that the writes around it are in.
        let kills_now = sh.windows.lock().unwrap().len();
        if kills_now > swept_after {
            swept_after = kills_now;
            let keys: Vec<u64> = std::mem::take(&mut checker.touched).into_iter().collect();
            if !keys.is_empty() {
                let _ = jobs.send(Job::Sweep { minute: m, keys });
            }
        } else {
            checker.touched.clear();
        }
        let (errors, down) = sh.take_errors();
        totals.0 += ops;
        totals.1 += errors;
        totals.2 += mismatches;
        // The servers, as their processes and their metrics have them.
        let look = bench.look(&bench.server(), "soak.fennec");
        let rlook = bench.look(&bench.replica(), "replica.fennec");
        let page = super::metrics_page(&addr).unwrap_or_default();
        let rpage = super::metrics_page(&raddr).unwrap_or_default();
        let status = bench.status(bench.replica_port).unwrap_or_default();
        if let Some(why) = refused(&status) {
            sh.fault(format!("the replica's primary {why}"));
        }
        // The compactions each server ran on its own in the minute: its
        // `fennec_auto_compactions_total`, which starts again with it.
        let mut own = [0u64; 2];
        for (i, p) in [&page, &rpage].into_iter().enumerate() {
            if p.is_empty() {
                continue;
            }
            let n = metric_sum(p.as_bytes(), "fennec_auto_compactions_total");
            let (before, last) = &mut auto[i];
            own[i] = match n >= *last {
                true => n - *last,
                false => {
                    *before += *last;
                    n
                }
            };
            *last = n;
        }
        let compactions = auto[0].0 + auto[0].1 + doings.compacts.load(Ordering::Relaxed);
        let behind = json_num(&status, "behind");
        // About half the mix writes: the replica's lag in seconds of them.
        let write_rate = ops as f64 / secs * 0.5;
        let lag_s = behind.map_or(-1.0, |b| b as f64 / write_rate.max(1.0));
        let (load1, load5) = load_average();
        let clock = clock_probe(&addr, &sh);
        let stepped = match (clock_prev, clock) {
            (Some(a), Some(b)) => clock_stepped(a, b),
            _ => false,
        };
        if let Some(p) = clock {
            clock_prev = clock;
            sh.clock(&p);
        }
        // What the VM's CPUs did beside the two servers' cgroups -- the
        // run's own look, verify and backup containers among it -- in
        // cores, and this host's other processes.
        let cpu_now = match (look, rlook) {
            (Some(a), Some(b)) => match (a.cpu_usec, b.cpu_usec, a.vm_busy) {
                (Some(x), Some(y), Some(busy)) => Some((x, y, busy)),
                _ => None,
            },
            _ => None,
        };
        let others_vm = match (cpu_prev, cpu_now) {
            (Some(p), Some(n)) if n.0 >= p.0 && n.1 >= p.1 && n.2 >= p.2 => {
                let ours = (n.0 - p.0 + n.1 - p.1) as f64 / 1e6;
                ((n.2 - p.2) as f64 / 100.0 - ours).max(0.0) / secs
            }
            _ => -1.0,
        };
        cpu_prev = cpu_now;
        let named = others_named();
        let others_host = named.as_ref().map_or(-1.0, |x| x.0);
        if let Some((cores, top)) = named.filter(|x| x.0 >= 1.0) {
            say(format!(
                "minute {m}: other processes on this host took {cores:.1} cores, the most {}",
                top.iter()
                    .map(|(n, c)| format!("{n} {c:.2}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let sample = match o.samples > 0 && !o.peak {
            true => sample(&sh, &mut sample_rng, o.samples),
            false => None,
        };
        sh.doc_writes
            .lock()
            .unwrap()
            .retain(|w| w.2.saturating_add(120_000_000_000) > now_ns);
        let hfree = host_free(&o.out);
        let vfree = look.and_then(|l| l.vm_free);
        // The disk on both sides: one short of the floor ends the run as
        // the environment's, before a write fails for it.
        let short = [("this host", hfree), ("Docker's VM", vfree)]
            .into_iter()
            .find(|(_, free)| free.is_some_and(|b| b < floor));
        // The replica's window held to what its file plausibly takes and
        // to its watch's absence (`bound_r_window`), the watch as the line
        // says it.
        if let Some(l) = look {
            sh.p_file.store(l.file, Ordering::SeqCst);
        }
        if let Some(l) = rlook {
            let per_gb = sh.p_compact_ms_per_gb.load(Ordering::SeqCst);
            sh.r_bound_ms
                .store(plausible_ms(l.file, per_gb), Ordering::SeqCst);
        }
        let watched = sh.r_watched.load(Ordering::SeqCst);
        bound_r_window(&sh, sh.ns(), watched);
        // The run's last line -- its planned minutes, or the environment
        // ending it -- settles every operation still held: an open window's
        // by the watch's word where it looks, as if no window had been where
        // it does not. Before, the last line was the planned minutes' alone,
        // and an end the environment made lost the held.
        if last_line(m, o.minutes, short.is_some()) {
            flush_held(&sh.slots, &mut h, &|id| sh.r_verdict(id).or(Some(watched)));
        }
        let figures = kept.minute(&h);
        // The replica's compaction open at the look, and how long so far:
        // what the counter may have counted and the watch not yet ended.
        let open_ms = {
            let (_, from, to) = *sh.r_window.lock().unwrap();
            (from != u64::MAX && to == u64::MAX).then(|| now_ns.saturating_sub(from) / 1_000_000)
        };
        let mut v: Vec<(&str, String)> = vec![
            ("started", started.to_string()),
            ("life", life.to_string()),
            ("secs", format!("{secs:.1}")),
            ("ops", ops.to_string()),
            ("ops_s", format!("{:.0}", ops as f64 / secs)),
            ("rate", format!("{:.0}", o.rate)),
            ("idle_workers", idle.to_string()),
            ("lane_ops", lane_ops.to_string()),
            ("lane_dropped", (lane_dropped / 1000).to_string()),
            (
                "r_compacts_watched",
                sh.r_compacts_seen.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_cut",
                sh.r_compacts_cut.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_lost",
                sh.r_compacts_lost.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_false",
                sh.r_compacts_false.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_overlong",
                sh.r_compacts_overlong
                    .swap(0, Ordering::Relaxed)
                    .to_string(),
            ),
            (
                "r_compacts_reset",
                sh.r_compacts_reset.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_stale",
                sh.r_compacts_stale.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compacts_unread",
                sh.r_compacts_unread.swap(0, Ordering::Relaxed).to_string(),
            ),
            (
                "r_compact_bound_ms",
                sh.r_bound_ms.load(Ordering::SeqCst).to_string(),
            ),
            ("r_compact_open", (open_ms.is_some() as u8).to_string()),
            ("r_watch", (watched as u8).to_string()),
            ("errors", errors.to_string()),
            ("refused", sh.refused.swap(0, Ordering::Relaxed).to_string()),
            ("down", down.to_string()),
            ("mismatches", mismatches.to_string()),
            ("lost_writes", lost.to_string()),
            ("backup_differs", differ.to_string()),
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
            ("auto_compacts", own[0].to_string()),
            ("r_auto_compacts", own[1].to_string()),
            (
                "memory_bytes",
                metric_sum(page.as_bytes(), "fennec_memory_bytes").to_string(),
            ),
            (
                "r_memory_bytes",
                metric_sum(rpage.as_bytes(), "fennec_memory_bytes").to_string(),
            ),
            // The primary's change counter: the records its replication
            // feed holds, whose index the report counts as held (`feed_held`).
            (
                "seq",
                metric_sum(page.as_bytes(), "fennec_change_sequence").to_string(),
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
            ("others_host", format!("{others_host:.2}")),
            ("others_vm", format!("{others_vm:.2}")),
            ("clock_step", (stepped as u8).to_string()),
        ];
        v.extend(figures);
        if let Some(b) = loop_behind {
            v.push(("behind_s", format!("{b:.1}")));
        }
        match sh.r_compact_ms.swap(0, Ordering::Relaxed) {
            0 => {}
            ms => v.push(("r_compact_ms", ms.to_string())),
        }
        match sh.r_kill_ms.swap(0, Ordering::Relaxed) {
            0 => {}
            ms => v.push(("r_compact_kill_ms", ms.to_string())),
        }
        if let Some(ms) = open_ms {
            v.push(("r_compact_open_ms", ms.to_string()));
        }
        if let Some(ms) = checker.lost_ms_max.take() {
            v.push(("lost_ms_max", ms.to_string()));
        }
        if let Some(p) = clock {
            v.push(("clock_off_ms", format!("{:.1}", p.off_ms)));
            v.push(("clock_rtt_ms", format!("{:.1}", p.rtt_ms)));
        }
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
                ("compacting", (l.compacting as u8).to_string()),
            ]);
        }
        if let Some(l) = rlook {
            // Its faults and page cache too: dry run 6 could not tell a
            // replica faulting in its compaction from the loop's queue.
            let majflt = match r_prev_majflt {
                Some(p) if l.majflt >= p => l.majflt - p,
                _ => l.majflt,
            };
            r_prev_majflt = Some(l.majflt);
            v.extend([
                ("r_file", l.file.to_string()),
                ("r_rss_anon", l.rss_anon.to_string()),
                ("r_threads", l.threads.to_string()),
                ("r_fds", l.fds.to_string()),
                ("r_compacting", (l.compacting as u8).to_string()),
                ("r_majflt", majflt.to_string()),
                ("r_cg_file", l.cgroup.file.to_string()),
            ]);
        }
        // Each server's looks a second in the minute (`watch_memory`): the
        // most anonymous memory and dirty pages, the least page cache and
        // the faults, the life's most, and the seconds a window of the
        // replica's was open in.
        for (i, k) in MEM_KEYS.iter().enumerate() {
            let (minute, life, window) = {
                let mut x = sh.mem[i].lock().unwrap();
                (x.minute.take(), x.life, x.window.take())
            };
            v.push((k[0], minute.map_or(0, |s| s.n).to_string()));
            if let Some(s) = minute {
                v.extend([
                    (k[1], s.anon.to_string()),
                    (k[2], s.dirty.to_string()),
                    (k[3], s.file.to_string()),
                ]);
            }
            if let Some(s) = life {
                v.extend([(k[4], s.anon.to_string()), (k[5], s.dirty.to_string())]);
            }
            if let Some(s) = window {
                v.extend([(k[6], s.faults.to_string()), (k[7], s.file.to_string())]);
            }
            peaks[i] = minute;
        }
        // The open loop's lag each second of the minute, each lane's most,
        // and in a window of the replica's.
        {
            let b = std::mem::take(&mut *sh.behind.lock().unwrap());
            for (k, x) in [
                ("behind_max_s", b.main),
                ("r_behind_max_s", b.replica),
                ("rc_behind_max_s", b.window_main),
                ("rc_r_behind_max_s", b.window_replica),
            ] {
                if let Some(x) = x {
                    v.push((k, format!("{x:.2}")));
                }
            }
            if b.window_s > 0 {
                v.push(("rc_seconds", b.window_s.to_string()));
            }
        }
        if let Some(b) = lane_behind {
            v.push(("r_behind_s", format!("{b:.1}")));
        }
        v.push((
            "kill_died",
            kill_died.swap(0, Ordering::Relaxed).to_string(),
        ));
        let (rup, _) = bench.state(&bench.replica());
        for (name, n) in [
            ("kill", &doings.kills),
            ("died", &doings.deaths),
            ("oom", &doings.oom),
            ("verify", &doings.verifies),
            ("verify_failed", &doings.verify_failed),
            ("backup", &doings.backups),
            ("restore", &doings.restores),
            ("backup_failed", &doings.backup_failed),
            ("backup_stale", &doings.backup_stale),
            ("compact", &doings.compacts),
            ("compact_failed", &doings.compact_failed),
            ("compact_busy", &doings.compact_busy),
            ("compact_killed", &doings.compact_killed),
            ("compact_died", &doings.compact_died),
            ("backup_died", &doings.backup_died),
            ("restart_ms", &doings.restart_ms),
            ("swept", &doings.swept),
            ("sweep_owed", &doings.sweep_owed),
            ("sweep_failed", &doings.sweep_failed),
        ] {
            v.push((name, n.swap(0, Ordering::Relaxed).to_string()));
        }
        // The kill's figures: one kill a minute at the most, a death beside
        // it the rarer still -- the last said of each kept.
        let figures = std::mem::take(&mut *doings.figures.lock().unwrap());
        for (name, x) in figures {
            v.retain(|(k, _)| *k != name);
            v.push((name, x));
        }
        v.push(("r_died", (!rup as u8).to_string()));
        // The replica, kept from reads after a restart until it said it
        // held the newest change: looked at again each minute.
        if !sh.replica_ok.load(Ordering::SeqCst)
            && replica_ready(&bench, sh.replica_target.load(Ordering::SeqCst))
        {
            sh.replica_ok.store(true, Ordering::SeqCst);
            say(format!("the replica is back at minute {m}"));
        }
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
        let tail = match (h.steady.n, loop_behind) {
            (0, Some(b)) => format!("no operation on its schedule, the loop {b:.0} s behind"),
            (0, None) => "no steady operation".to_string(),
            (_, b) => format!(
                "p99 {:.0} us, p99.9 {:.0}, max {:.0} ms{}",
                h.steady.pct(0.99),
                h.steady.pct(0.999),
                h.steady.max as f64 / 1e6,
                match b {
                    Some(b) if b >= 1.0 => format!(", the loop {b:.0} s behind"),
                    _ => String::new(),
                }
            ),
        };
        // What the servers answered in, from the send: beside a tail from
        // the schedule, whose wait it was.
        let tail = match (h.own.n, h.replica.n) {
            (0, _) => tail,
            (_, 0) => format!("{tail} (answered in p99 {:.0} ms)", h.own.pct(0.99) / 1e3),
            _ => format!(
                "{tail} (answered in p99 {:.0} ms, through the replica {:.0})",
                h.own.pct(0.99) / 1e3,
                h.replica.pct(0.99) / 1e3
            ),
        };
        // The replica lane's lag, and its reads' own answers in a window of
        // the replica's, beside the class's from the schedule.
        let tail = match lane_behind {
            Some(b) if b >= 1.0 => format!("{tail}, the replica lane {b:.0} s behind"),
            _ => tail,
        };
        // At the peak the lane follows the pool, and what it could not
        // keep up with it dropped: its share of what was answered, which
        // the soak's rate asks 4% of.
        let tail = match (o.peak && o.replica_threads > 0, ops) {
            (true, 1..) => format!(
                "{tail}; the replica lane {:.1}% of them{}",
                lane_ops as f64 * 100.0 / ops as f64,
                match lane_dropped / 1000 {
                    0 => String::new(),
                    d => format!(", {d} of its reads dropped, more than a second behind"),
                }
            ),
            _ => tail,
        };
        let tail = match h.r_compact_own.n {
            0 => tail,
            _ => format!(
                "{tail}; in the replica's compaction p99 {:.0} ms from the schedule, the replica answering in {:.0}",
                h.r_compact.pct(0.99) / 1e3,
                h.r_compact_own.pct(0.99) / 1e3
            ),
        };
        let peak_said = |p: Option<MemSpan>| match p {
            Some(p) => format!(" (most {}, dirty {})", p.anon >> 20, p.dirty >> 20),
            None => String::new(),
        };
        say(format!(
            "{m:>4} min: {:.0} ops/s, {tail}; {errors} errors, {down} at a restart, {mismatches} mismatches; file {} MB, live {} MB, anon {} MB{}, the replica's {} MB{}, lag {lag_s:.1} s, load {load1:.1}",
            ops as f64 / secs,
            look.map(|l| l.file >> 20).unwrap_or(0),
            metric_sum(page.as_bytes(), "fennec_data_bytes") >> 20,
            look.map(|l| l.rss_anon >> 20).unwrap_or(0),
            peak_said(peaks[0]),
            rlook.map(|l| l.rss_anon >> 20).unwrap_or(0),
            peak_said(peaks[1]),
        ));
        if let Some(why) = &env_failed {
            say(format!("the environment failed the run: {why}"));
            break;
        }
        if let Some(why) = sh.faulted() {
            stop_run(&bench, Some(&sh), o, &format!("minute {m}: {why}"));
        }
        if o.peak && m > 1 {
            peak.push((m, ops as f64, secs));
            peak_lane.0 += lane_ops;
            peak_lane.1 += ops;
            peak_lane.2 += lane_dropped;
        }
        if !rup {
            sh.note(format!("the replica was found stopped at minute {m}"));
            // Its looks a second, the next life's from here: its life's
            // most is its own, never the one before's.
            sh.mem[1].lock().unwrap().restarted();
            let _ = docker(&["start", &bench.replica()]);
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
                    let _ = jobs.send(Job::Compact {
                        minute: m,
                        for_kill: false,
                    });
                }
                Doing::Backup => {
                    backup_id += 1;
                    let _ = jobs.send(Job::Backup {
                        minute: m,
                        id: backup_id,
                    });
                }
                Doing::Kill { in_compact } => {
                    // One at a time: a kill waits for what is queued.
                    fence(
                        &jobs,
                        &sh.doing,
                        WAIT_MAX,
                        &format!("the kill due at minute {m}"),
                    );
                    // A server that died on its own while the kill waited
                    // -- the long run's primary, OOM-killed in the compact
                    // its kill of minute 300 waited for -- is a death,
                    // timed by itself and never by the kill, which is
                    // not sent.
                    if let Some(x) = dead(|| bench.exit(&bench.server()), &events, &mut evs) {
                        let back = died(
                            &bench,
                            &sh,
                            &doings,
                            o,
                            x,
                            &mut evs,
                            m * 60_000_000_000,
                            Some(m),
                            None,
                            None,
                            &mut restart_ms_max,
                        );
                        evs.extend(events.try_iter());
                        evs.retain(|e| e.at > back);
                        kill_died.fetch_add(1, Ordering::Relaxed);
                        life += 1;
                        prev_majflt = None;
                        prev_oom = 0;
                        continue;
                    }
                    say(format!(
                        "the kill due at minute {m}{}: announced",
                        if in_compact { ", in a compact" } else { "" }
                    ));
                    let from = sh.ns();
                    // How far behind the loop is as the kill is announced
                    // -- after the queue it waited for, which may have
                    // been minutes (the third dry run's kill of minute 60
                    // waited out a compact of five minutes, the loop 114 s
                    // behind at the minute's look and 166 s at the kill).
                    let lag = behind_s(sh.pool_slots(), from);
                    sh.restart_behind
                        .store(lag.is_some_and(|b| b > ON_SCHEDULE_S), Ordering::SeqCst);
                    if let Some(b) = lag {
                        doings.figure("kill_behind_s", format!("{b:.1}"));
                    }
                    sh.windows.lock().unwrap().push((from, u64::MAX, u64::MAX));
                    sh.restart_to.store(u64::MAX, Ordering::SeqCst);
                    sh.restart_from.store(from, Ordering::SeqCst);
                    sh.replica_ok.store(false, Ordering::SeqCst);
                    // The primary's main thread watched to the kill: an
                    // fsync in flight then is said on the kill's line.
                    let sampler = match o.sync.as_str() {
                        "always" => None,
                        _ => Sampler::start(&bench),
                    };
                    if in_compact {
                        // The compact the kill lands in: asked, and under
                        // way a second -- asked again while the server's
                        // own holds the file -- the kill after.
                        let ended = || {
                            doings.compacts.load(Ordering::SeqCst)
                                + doings.compact_busy.load(Ordering::SeqCst)
                                + doings.compact_failed.load(Ordering::SeqCst)
                                + doings.compact_died.load(Ordering::SeqCst)
                                + doings.compact_killed.load(Ordering::SeqCst)
                        };
                        let before = ended();
                        let _ = jobs.send(Job::Compact {
                            minute: m,
                            for_kill: true,
                        });
                        let t = Instant::now();
                        let seen = evs.len();
                        loop {
                            evs.extend(events.try_iter());
                            let wait = compact_wait(
                                &evs[seen..],
                                sh.compact_since.load(Ordering::SeqCst),
                                sh.ns(),
                                ended() != before,
                                t.elapsed(),
                            );
                            match wait {
                                None => {
                                    std::thread::sleep(Duration::from_millis(20));
                                    continue;
                                }
                                Some(CompactWait::Ended) => sh.note(format!(
                                    "the compact the kill at minute {m} was to land in ended within a second: killed without"
                                )),
                                Some(CompactWait::Timeout) => sh.note(format!(
                                    "the kill at minute {m} found no compact of its own under way within {} s: killed without",
                                    WAIT_MAX.as_secs()
                                )),
                                Some(CompactWait::Died | CompactWait::UnderWay) => {}
                            }
                            break;
                        }
                    }
                    // The announced second: operations due from `from` on
                    // are the restart's.
                    let k = (from + 1_000_000_000).max(sh.ns());
                    std::thread::sleep(Duration::from_nanos(k.saturating_sub(sh.ns())));
                    // Dead in that second, or in the compact the kill was
                    // to land in: the death's, the kill not sent.
                    if let Some(x) = dead(|| bench.exit(&bench.server()), &events, &mut evs) {
                        let back = died(
                            &bench,
                            &sh,
                            &doings,
                            o,
                            x,
                            &mut evs,
                            from,
                            Some(m),
                            Some(from),
                            sampler,
                            &mut restart_ms_max,
                        );
                        evs.extend(events.try_iter());
                        evs.retain(|e| e.at > back);
                        kill_died.fetch_add(1, Ordering::Relaxed);
                        life += 1;
                        prev_majflt = None;
                        prev_oom = 0;
                        continue;
                    }
                    let k = sh.ns();
                    let (killed, exit, not_running) = send_kill(&bench, &sh, m, k);
                    // Dead in the moment before the send, which Docker's
                    // state showed running to the look above: told after
                    // Docker's wait (`gone_before`), the death's, timed by
                    // itself, the kill not counted -- judged either way
                    // for the side job it cut (`judge_kill`).
                    if let Some(x) = judge_kill(&sh, exit, not_running, k) {
                        sh.note(format!(
                            "the kill at minute {m} found its server gone before it was sent: a death"
                        ));
                        evs.extend(events.try_iter());
                        let back = died(
                            &bench,
                            &sh,
                            &doings,
                            o,
                            x,
                            &mut evs,
                            from,
                            Some(m),
                            Some(from),
                            sampler,
                            &mut restart_ms_max,
                        );
                        evs.extend(events.try_iter());
                        evs.retain(|e| e.at > back);
                        kill_died.fetch_add(1, Ordering::Relaxed);
                        life += 1;
                        prev_majflt = None;
                        prev_oom = 0;
                        continue;
                    }
                    // The kill's own: a side job it cut is the kill's
                    // (`cut_by`), stored so by `judge_kill`.
                    say(kill_said(&sh, m, k));
                    let back = restart(
                        &bench,
                        &sh,
                        &doings,
                        (from, k, m),
                        Some(killed),
                        sampler,
                        o,
                        &mut restart_ms_max,
                    );
                    // Docker's `die` of the kill's: the run's own.
                    evs.extend(events.try_iter());
                    evs.retain(|e| e.at > back);
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
    // The side jobs still queued -- a backup is minutes -- waited for as a
    // kill waits for them: bounded and named. The join after it is the
    // thread's end once its queue is closed.
    say(format!(
        "the load stopped; the side jobs still queued next, {} s at the most",
        WAIT_MAX.as_secs()
    ));
    fence(&jobs, &sh.doing, WAIT_MAX, "the end");
    drop(jobs);
    side.join().unwrap();
    say("the side jobs ended; every read held next, then every key read back".into());
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
    // The last minutes' reads and events, held only now: the run's, said
    // apart from what the end's read finds.
    let tail = checker.mismatches - before;
    let before = checker.mismatches;
    let t = Instant::now();
    let primary = match env_failed {
        None => read_all(&addr, &sh, o, Some(&mut checker)),
        Some(_) => None,
    };
    let swept_s = t.elapsed().as_secs_f64();
    let late = checker.mismatches - before;
    let catchup = match env_failed {
        None => caught_up(
            Duration::from_secs(o.catchup_max),
            "the end's catch-up",
            false,
        ),
        Some(_) => None,
    };
    let replica = match (catchup, &primary) {
        (Some(_), Some(_)) => read_all(&raddr, &sh, o, None),
        _ => None,
    };
    let same = primary.is_some() && primary == replica;
    if !same {
        say(format!(
            "the replica's rows {} the primary's (usertable, counters, docs: {replica:?} against {primary:?})",
            if replica.is_none() { "were not read beside" } else { "DIFFER from" }
        ));
    }
    // Each stop said apart: two bounded at 330 s would outlast the guard.
    for c in [bench.server(), bench.replica()] {
        let t = Instant::now();
        let (ok, _) = docker_within(Duration::from_secs(330), &["stop", "-t", "300", &c]);
        say(format!(
            "{c} {} in {:.0} s",
            if ok { "stopped" } else { "did NOT stop" },
            t.elapsed().as_secs_f64()
        ));
    }
    // A line before each verify and after it: the two, each bounded at
    // `WAIT_MAX`, would outlast the guard's ten minutes between two lines.
    let verify = |volume: &str, file: &str| {
        say(format!(
            "{file} verified next, {} s at the most",
            WAIT_MAX.as_secs()
        ));
        let t = Instant::now();
        let (ok, out) = bench.fennec(
            &format!("{}-verify", bench.name),
            volume,
            &format!("/tools/fennec verify /data/{file}"),
            WAIT_MAX,
        );
        if !ok {
            say(out);
        }
        say(format!(
            "{file} {} in {:.0} s",
            if ok {
                "verified clean"
            } else {
                "FAILED its verify"
            },
            t.elapsed().as_secs_f64()
        ));
        ok
    };
    let primary_ok = verify(&bench.volume("data"), "soak.fennec");
    let replica_ok = verify(&bench.volume("replica-data"), "replica.fennec");
    if !primary_ok || !replica_ok || tail > 0 || late > 0 || !same {
        bench.keep_data(&format!(
            "at the end: the primary's file {}, the replica's {}, {tail} mismatches in the last minutes' reads and events, {late} in the end's read, the replica's rows {}",
            if primary_ok { "verified" } else { "FAILED verify" },
            if replica_ok { "verified" } else { "FAILED verify" },
            if same { "the same" } else { "different or not compared" }
        ));
    }
    let cell = format!("{}:{}:end", o.sync, o.label);
    let mut v: Vec<(&str, String)> = vec![
        ("started", started.to_string()),
        ("minutes", minutes_run.to_string()),
        ("ops", totals.0.to_string()),
        ("errors", totals.1.to_string()),
        ("mismatches", (totals.2 + tail + late).to_string()),
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
        ("restart_ms_max", restart_ms_max.to_string()),
        ("verify_failed", (!primary_ok as u8).to_string()),
        ("replica_verify_failed", (!replica_ok as u8).to_string()),
        ("replica_differs", (!same as u8).to_string()),
        (
            "lost_writes",
            (checker.lost_writes - lost_before).to_string(),
        ),
        (
            "backup_differs",
            (checker.backups_differ - differ_before).to_string(),
        ),
        (
            "memory_limit",
            super::bytes_of(&o.memory).unwrap_or(0).to_string(),
        ),
        ("records", o.records.to_string()),
        ("env_failed", (env_failed.is_some() as u8).to_string()),
    ];
    v.extend(kept.end());
    if let Some(ms) = checker.lost_ms_max.take() {
        v.push(("lost_ms_max", ms.to_string()));
    }
    // The peak a soak's rate is a fraction of is its slowest stretch
    // (`slowest_stretch`), the mean over every minute but the first beside
    // it: under the run's memory the file doubles with the updates and the
    // server compacts it on its own, and a rate above what it answers while
    // it compacts leaves the open loop behind every cycle. The second dry
    // run of 2026-11-23 took the median of four minutes of a fresh file, 3
    // 214 a second, and fell behind at minute 13 for good; the third, of
    // 2026-11-24, the mean of thirty, 2 617 -- 3 228 a second at a 1 GB
    // file, 2 749 at 2 GB, 1 955-2 117 while the server compacted -- and
    // its 80%, 2 093, was behind from minute 46 to 97, the loop falling up
    // to 15 s a minute behind through each compaction (the primary's own
    // of 114-322 s, the replica's of 262-344 s, the backup's 132 s) and
    // never draining the backlog before the next.
    if o.peak && !peak.is_empty() {
        let (ops, secs) = peak
            .iter()
            .fold((0.0, 0.0), |a: (f64, f64), x| (a.0 + x.1, a.1 + x.2));
        let mean = ops / secs.max(1.0);
        let (at, slowest) = slowest_stretch(&peak, PEAK_STRETCH).unwrap_or((2, mean));
        v.push(("peak_ops_s", format!("{mean:.0}")));
        v.push(("peak_slowest_ops_s", format!("{slowest:.0}")));
        v.push(("peak_slowest_at", at.to_string()));
        let own = auto[0].0 + auto[0].1;
        v.push(("peak_auto_compacts", own.to_string()));
        // The lane's share of the peak, beside the 4% the soak's rate
        // asks of it: a peak whose lane dropped reads it could not serve
        // measured fewer of them, and its rate's fraction asks more.
        if o.replica_threads > 0 {
            let share = peak_lane.0 as f64 * 100.0 / peak_lane.1.max(1) as f64;
            v.push(("peak_lane_share", format!("{share:.2}")));
            v.push(("peak_lane_dropped", (peak_lane.2 / 1000).to_string()));
            say(format!(
                "the peak: {mean:.0} a second, its slowest {PEAK_STRETCH} minutes {slowest:.0}; the replica lane {share:.1}% of it, its share {:.0}%, {} of its reads dropped, more than a second behind",
                REPLICA_SHARE * 100.0,
                peak_lane.2 / 1000
            ));
        }
        println!("peak {slowest:.0}");
        println!("peak_mean {mean:.0}");
        println!("peak_slowest_at {at}");
        println!("peak_compacts {own}");
    }
    append_values(&o.out, "soak", &o.run_id, &cell, &v);
    say(format!(
        "{minutes_run} minutes, {} operations, {} errors, {} mismatches ({} reads and {} writes held), the files {} and {}, the replica {}{}",
        totals.0,
        totals.1,
        totals.2 + tail + late,
        checker.reads,
        checker.writes,
        if primary_ok { "clean" } else { "NOT CLEAN" },
        if replica_ok { "clean" } else { "NOT CLEAN" },
        if same { "the same" } else { "DIFFERENT or not compared" },
        match &env_failed {
            Some(why) => format!("; the ENVIRONMENT failed it: {why}"),
            None => String::new(),
        }
    ));
    drop(bench);
}

/// The server killed (`kill -9`) -- or found dead -- its file verified,
/// started again and waited for: the restart's window closed when it
/// answers. `at` is when the window began (a second before the kill, or
/// the last look before a death), the kill, and its minute. Each part is
/// timed and said on the minute's line -- docker's kill and wait
/// (`kill_ms`), the file's verify (`verify_ms`), and the server's own part,
/// `docker start` to answering (`start_ms`) -- with whether a compact was
/// writing at the kill (its side file left behind: `kill_compacting`) and
/// for how long the primary's syncer had been in an fsync then
/// (`kill_sync_ms`, -1 where it could not be seen). A file that fails its
/// verify is copied aside before the server opens it again, and the run's
/// volumes kept. After it, no read goes through the replica until the
/// replica is connected and holds the newest change answered before the
/// kill (`replica_back_ms`, bounded by `--replica-back-max`). `kill`, the
/// run's own kill, sent at `k` (`send_kill`), and when Docker's wait for
/// its end ended; a death (`died`) is gone already, its `k` its time.
/// Hands back when the server was started again, in ns: Docker's events of
/// the process before are the kill's or the death's.
#[allow(clippy::too_many_arguments)]
fn restart(
    bench: &Kit,
    sh: &Shared,
    doings: &Doings,
    at: (u64, u64, u64),
    kill: Option<u64>,
    sampler: Option<Sampler>,
    o: &Opts,
    longest: &mut u64,
) -> u64 {
    let (from, k, m) = at;
    {
        // The kill's window, opened as it was announced: its time the
        // send's. A death's is opened by `found_dead`, its time there.
        let mut w = sh.windows.lock().unwrap();
        match w.last_mut() {
            Some(x) if x.0 == from && x.2 == u64::MAX => {
                if kill.is_some() {
                    x.1 = k;
                }
            }
            _ => w.push((from, k, u64::MAX)),
        }
    }
    sh.restart_to.store(u64::MAX, Ordering::SeqCst);
    sh.restart_from.store(from, Ordering::SeqCst);
    sh.replica_ok.store(false, Ordering::SeqCst);
    // Its catch-up from the window's start (`catch_up`): the replica's
    // 504s the restart's, the lane moved up, until the loop is back on its
    // schedule and the replica holds the change answered then.
    sh.catch_open(m, from);
    // The newest change answered before the kill was sent (`send_kill`
    // read it then), or before the death.
    if kill.is_none() {
        sh.replica_target
            .store(sh.max_seq.load(Ordering::SeqCst), Ordering::SeqCst);
    }
    let target = sh.replica_target.load(Ordering::SeqCst);
    let killed = kill.unwrap_or_else(|| sh.ns());
    let kill = kill.is_some();
    if kill {
        say(format!(
            "minute {m}: the server gone {:.1} s after the kill was sent; its file verified next",
            (killed - k) as f64 / 1e9
        ));
    }
    // A death found in the kill's announced second had the syncer watched
    // to it; one found otherwise has no watch, and no kill of the run's to
    // time.
    let sync_ms = sampler.and_then(Sampler::finish);
    if kill {
        doings.figure("kill_ms", ((killed - k) / 1_000_000).to_string());
        doings.figure(
            "kill_sync_ms",
            sync_ms.map_or("-1".into(), |x| x.to_string()),
        );
    } else if let Some(x) = sync_ms {
        doings.figure("death_sync_ms", x.to_string());
    }
    let t = Instant::now();
    let copy = format!("failed-m{m:04}.fennec");
    let (_, out) = bench.fennec(
        &format!("{}-verify", bench.name),
        &bench.volume("data"),
        &format!(
            "[ \"$({WRITTEN_SINCE} /data/soak.fennec.beside)\" = 1 ] && echo BESIDE; \
             if /tools/fennec verify /data/soak.fennec; then echo VERIFY-OK; \
             else cp /data/soak.fennec /data/{copy} && echo KEPT; exit 1; fi"
        ),
        WAIT_MAX,
    );
    let verified = out.contains("VERIFY-OK");
    let beside = out.contains("BESIDE");
    doings.figure("verify_ms", t.elapsed().as_millis().to_string());
    doings.figure("kill_compacting", (beside as u8).to_string());
    say(format!(
        "{} at minute {m}{}{}; fennec verify {} in {:.0} s",
        if kill { "killed" } else { "died" },
        if beside { ", a compact writing" } else { "" },
        match sync_ms {
            Some(0) => ", no fsync in flight".to_string(),
            Some(x) => format!(", an fsync {x} ms in flight"),
            None => String::new(),
        },
        if verified { "clean" } else { "FAILED" },
        t.elapsed().as_secs_f64()
    ));
    if !verified {
        say(out);
        doings.verify_failed.fetch_add(1, Ordering::Relaxed);
        bench.keep_data(&format!(
            "the file failed its verify after the kill at minute {m}: its copy is {copy} in {}",
            bench.volume("data")
        ));
    }
    doings.verifies.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let started_ns = sh.ns();
    // A connection first answered from here is the next server's: the
    // last was gone, and nothing answered since.
    sh.life.fetch_add(1, Ordering::SeqCst);
    // Its memory a second, the next life's from here: the watch of the
    // last ended with its process.
    sh.mem[0].lock().unwrap().restarted();
    let (ok, out) = docker(&["start", &bench.server()]);
    assert!(ok, "docker start after the kill at minute {m}: {out}");
    // A line between docker's start and the wait for an answer: the two
    // bounds together outlast the guard's ten minutes.
    say(format!("minute {m}: {} started again", bench.server()));
    // Bounded under the script's guard; the report holds the restart to
    // `--restart-max`, which is shorter.
    assert!(
        bench.wait_health(bench.port, WAIT_MAX),
        "fennec-server did not answer within {} s of its start after the kill at minute {m}",
        WAIT_MAX.as_secs()
    );
    let back = sh.ns();
    // This life's answers and closes from here: the last life's burst and
    // its restart's refused connects time none of its deaths.
    sh.back(back);
    sh.catch_back(back);
    sh.restart_to.store(back, Ordering::SeqCst);
    sh.windows.lock().unwrap().last_mut().unwrap().2 = back;
    let ms = (back - k) / 1_000_000;
    doings.restart_ms.fetch_max(ms, Ordering::Relaxed);
    doings.figure("start_ms", started.elapsed().as_millis().to_string());
    *longest = (*longest).max(ms);
    say(format!(
        "answering again {:.1} s after the {}, {:.1} s after its start",
        ms as f64 / 1e3,
        if kill { "kill" } else { "death" },
        started.elapsed().as_secs_f64()
    ));
    let t = Instant::now();
    loop {
        if replica_ready(bench, target) {
            sh.replica_ok.store(true, Ordering::SeqCst);
            doings.figure("replica_back_ms", t.elapsed().as_millis().to_string());
            break;
        }
        if t.elapsed() >= Duration::from_secs(o.replica_back_max) {
            // Past its bound, which the report fails: the minute's loop
            // looks again each minute and lets reads through once it is.
            doings.figure("replica_back_ms", t.elapsed().as_millis().to_string());
            sh.note(format!(
                "the replica was not connected at change {target} within {} s of the restart at minute {m}",
                o.replica_back_max
            ));
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // The restart as the reads through the replica see it: none is sent
    // through it from the announcement until it is back and the grace
    // after the server answered is over (`restarting`). A compaction of
    // the replica's it met held its reads the longer by it (`restart_met`).
    restart_met(
        sh,
        Some((from, sh.ns().max(back.saturating_add(GRACE)))),
        None,
    );
    started_ns
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
    /// The figures the run gives beside its verdicts: each kill's state,
    /// the two bounds against what the run measured, the clocks, the load.
    notes: Vec<String>,
}

pub const CRITERIA: [&str; 23] = [
    "8 hours and an end line",
    "ran its planned minutes",
    "every minute has operations from every connection",
    "last hour's p99, steady and in compacts, <= 1.25x its schedule's",
    "file <= 2.2x live every minute",
    "primary's memory beyond what it holds < +5% within each life",
    "replica's memory beyond its engine's < +5% after the first hour",
    "threads less connections within 2, a life's too",
    "descriptors less connections within 2, a life's too",
    "no error",
    "no mismatch",
    "every stream held",
    "the replica followed, came back, caught up and holds the rows",
    "every restart within its bound",
    "the schedule's kills, compacts, backups and restores",
    "every verify, backup and restore clean",
    "match and near answer as a scan does",
    "every key read back, after each restart and at the end",
    "no server died, none OOM-killed",
    "the open loop back on its schedule within every kill interval",
    "reads held by the replica's own compactions within their bound",
    "the replica's compactions watched as its counter counted them",
    "no replica compaction open past what its file plausibly takes",
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

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s.get(s.len() / 2).copied().unwrap_or(0.0)
}

/// The doings the lines `[a, a + 59]` hold, as offsets from `a`: a
/// minute's doings show on the next line.
fn hour_doings(a: u64, n: u64, s: &Settings) -> Vec<(u64, Doing)> {
    let mut out = Vec::new();
    for i in 0..60 {
        let m = (a + i).saturating_sub(1);
        for d in schedule(
            m,
            n,
            s.kill_every,
            s.kill_in_compact_every,
            s.compact_every,
            s.backup_every,
            s.backup_at,
        ) {
            out.push((i, d));
        }
    }
    out
}

/// The first line of the earliest hour before the last, and apart from
/// it, that holds the last hour's schedule -- the same kills, the same
/// compacts in them, the same backups, at the same minutes of the hour: the
/// hour the last is compared with, so that a backlog a restart leaves and
/// a compact's tail weigh alike in both. Lines 61-120 of an 8-hour run.
fn same_hour(n: u64, s: &Settings) -> Option<u64> {
    if n < 120 {
        return None;
    }
    let last = n - 59;
    let want = hour_doings(last, n, s);
    (1..=last - 60).find(|&a| hour_doings(a, n, s) == want)
}

/// A cell's lines as one append wrote them: run, cell, metrics.
type Block = (String, String, Vec<(String, f64)>);

/// A minute's line as the report reads it: its minute and its values.
type Line<'a> = (u64, &'a HashMap<String, f64>);

/// What the primary's replication feed holds at each line of a life, in
/// bytes: the records it appended since it opened, up to the feed's bound
/// (`cap`, its `--replication-buffer`) -- the feed starts empty at each
/// open and keeps the last `cap` bytes of them -- and the index the feed
/// keeps beside each record held (`Chunk`'s `ends`, `lasts` and `times`:
/// 24 bytes, in vectors grown by doubling, so 48 at the most), its records
/// counted by the change counter (`seq`) where the lines have it, and none
/// where they do not. A minute's appends are its file's growth; one whose
/// file did not grow, or that compacted, counts the life's median minute,
/// as does its first line. `loaded`, the life the run loaded its records
/// in: they fill the feed at once (the file, every byte of it appended).
fn feed_held(xs: &[Line], cap: f64, loaded: bool) -> Vec<f64> {
    let g = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
    // Each minute's appended bytes and records, where it grew and nothing
    // compacted.
    let steps: Vec<Option<(f64, Option<f64>)>> = xs
        .iter()
        .enumerate()
        .map(|(i, (_, m))| {
            let (_, p) = xs.get(i.checked_sub(1)?)?;
            let grew = g(m, "file")? - g(p, "file")?;
            let records = match (g(m, "seq"), g(p, "seq")) {
                (Some(a), Some(b)) if a >= b => Some(a - b),
                _ => None,
            };
            (grew > 0.0 && quiet_look(m, false)).then_some((grew, records))
        })
        .collect();
    let grown: Vec<f64> = steps.iter().flatten().map(|s| s.0).collect();
    let per_minute = median(&grown);
    let recs: Vec<f64> = steps.iter().flatten().filter_map(|s| s.1).collect();
    // Records a byte of the file, over the minutes both are known for.
    let per_byte = match recs.len() == grown.len() && !recs.is_empty() {
        true => recs.iter().sum::<f64>() / grown.iter().sum::<f64>().max(1.0),
        false => 0.0,
    };
    let mut appended = match loaded {
        true => xs.first().and_then(|(_, m)| g(m, "file")).unwrap_or(0.0),
        false => 0.0,
    };
    steps
        .iter()
        .map(|s| {
            appended += s.map_or(per_minute, |x| x.0);
            let held = appended.min(cap);
            held + held * per_byte * 48.0
        })
        .collect()
}

/// Judges each run in a results file's text (or the one named) by the
/// soak's exit criterion, each attempt of a run id apart and the latest
/// judged.
fn judge(text: &str, only: Option<&str>) -> Vec<Judged> {
    // run -> started -> cell -> metric -> value
    type Cells = HashMap<String, HashMap<String, f64>>;
    let mut runs: Vec<String> = Vec::new();
    let mut data: HashMap<String, BTreeMap<u64, Cells>> = HashMap::new();
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
        // The script's marks -- the environment ended a run (`:env`), its
        // guard stopped a soak that printed nothing (`:hung`), `ycsb mark`
        // both -- and the soak's own stop on the harness's fault
        // (`:stopped`) are the attempt's that wrote the lines before them.
        let started = match metrics.iter().find(|(k, _)| k == "started") {
            Some(x) => x.1 as u64,
            None if cell.ends_with(":env")
                || cell.ends_with(":hung")
                || cell.ends_with(":stopped") =>
            {
                latest.get(&run).copied().unwrap_or(0)
            }
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

/// What a server's looks a second (`watch_memory`, `p` "" the primary's
/// and "r_" the replica's) found at their most over the run, said beside
/// its memory criterion: the most anonymous memory and dirty pages in a
/// second, and the minute whose two came nearest `--memory` (`limit`, the
/// end line's) -- their sum, the two maybe a second apart. Not judged: the
/// criterion holds a life's growth, a server the limit killed fails its
/// own (`CRITERIA[18]`), and no run has measured a bound. The look a
/// minute missed the compaction's jump of some 130 MB that killed the long
/// run's primary at minute 300, read at 304 MB a minute before its death.
fn peaks(minutes: &[Line], p: &str, limit: Option<f64>) -> String {
    let get = |m: &HashMap<String, f64>, k: &str| m.get(&format!("{p}{k}")).copied();
    let most = |k: &str| {
        minutes
            .iter()
            .filter_map(|(n, m)| Some((*n, get(m, k)?)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    };
    let Some((am, a)) = most("anon_peak") else {
        return String::new();
    };
    let mb = |x: f64| x / 1048576.0;
    let dirty = most("dirty_peak").map_or(String::new(), |(dm, d)| {
        format!(", dirty pages {:.0} MB (minute {dm})", mb(d))
    });
    let near = limit.and_then(|l| {
        minutes
            .iter()
            .filter_map(|(n, m)| {
                Some((
                    *n,
                    get(m, "anon_peak")? + get(m, "dirty_peak").unwrap_or(0.0),
                ))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, x)| {
                format!(
                    "; the two {:.0} MB of --memory's {:.0} at minute {n}",
                    mb(x),
                    mb(l)
                )
            })
    });
    format!(
        "; a second at a time the most anonymous memory {:.0} MB (minute {am}){dirty}{}, said, not judged",
        mb(a),
        near.unwrap_or_default()
    )
}

/// What the replica's own compactions held, said beside their criterion's
/// p99 from the schedule: the reads through it among them as it answered
/// them, from their send (`p99_r_compact_own`); the most each lane was
/// behind its schedule in a second of a window (`rc_r_behind_max_s`, the
/// replica lane's, and `rc_behind_max_s`, the pool's, which a lane of its
/// own leaves out of the replica's stall); and the replica's major faults
/// and least page cache in those seconds -- dry run 6 could tell none of
/// "the server held the read" from "the loop's queue". Nothing where the
/// lines have none of it, a run from before.
fn rc_figures(minutes: &[Line], end: Option<&HashMap<String, f64>>) -> String {
    let get = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
    let most = |k: &str| {
        minutes
            .iter()
            .filter_map(|(_, m)| get(m, k))
            .reduce(f64::max)
    };
    let least = |k: &str| {
        minutes
            .iter()
            .filter_map(|(_, m)| get(m, k))
            .reduce(f64::min)
    };
    let total = |k: &str| {
        minutes
            .iter()
            .filter_map(|(_, m)| get(m, k))
            .reduce(|a, b| a + b)
    };
    let mut s = String::new();
    if let Some(p) = end.and_then(|m| get(m, "p99_r_compact_own")) {
        write!(
            s,
            "; the replica answered the reads through it among them in {:.2} s at the p99 from their send ({:.0} reads)",
            p / 1e6,
            end.and_then(|m| get(m, "r_compact_own_n")).unwrap_or(0.0)
        )
        .unwrap();
    }
    if let Some(secs) = total("rc_seconds") {
        write!(s, "; in their {secs:.0} seconds").unwrap();
        for (k, what) in [
            ("rc_r_behind_max_s", "the replica lane"),
            ("rc_behind_max_s", "the pool"),
        ] {
            if let Some(b) = most(k) {
                write!(s, ", {what} {b:.1} s behind at the most").unwrap();
            }
        }
        if let Some(f) = total("rc_r_majflt") {
            write!(s, ", the replica's major faults {f:.0}").unwrap();
        }
        if let Some(c) = least("rc_r_file_min") {
            write!(s, ", its page cache {:.0} MB at the least", c / 1048576.0).unwrap();
        }
    }
    s
}

/// Each death of the primary's the lines name (`died`), as the death
/// criterion says it: its minute, how it ended, how long before the kill
/// due, what timed it, and the mismatches and the writes a read showed gone
/// from its line to the next restart's -- judged from the death's time, so
/// each is a loss past what `--sync` allows, never the harness's clock.
fn deaths_said(minutes: &[Line]) -> String {
    let get = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
    let restarts: Vec<u64> = minutes
        .iter()
        .filter(|(_, m)| {
            get(m, "kill").is_some_and(|x| x > 0.0) || get(m, "died").is_some_and(|x| x > 0.0)
        })
        .map(|(k, _)| *k)
        .collect();
    let each: Vec<String> = minutes
        .iter()
        .filter(|(_, m)| get(m, "died").is_some_and(|x| x > 0.0))
        .map(|(k, m)| {
            let next = restarts
                .iter()
                .find(|&&r| r > *k)
                .copied()
                .unwrap_or(u64::MAX);
            let after: Vec<&HashMap<String, f64>> = minutes
                .iter()
                .filter(|(x, _)| *x >= *k && *x < next)
                .map(|x| x.1)
                .collect();
            let total = |key: &str| after.iter().filter_map(|m| get(m, key)).sum::<f64>();
            let lost_ms = after
                .iter()
                .filter_map(|m| get(m, "lost_ms_max"))
                .reduce(f64::max);
            let minute = get(m, "death_at_s").map_or(*k, |s| (s / 60.0) as u64 + 1);
            let how = match (get(m, "death_oom"), get(m, "death_exit")) {
                (Some(o), _) if o > 0.0 => "OOM-killed".to_string(),
                (_, Some(c)) if c >= 0.0 => format!("exit {c:.0}"),
                _ => "how it ended unread".to_string(),
            };
            let due = match (get(m, "death_kill_due_s"), get(m, "death_announced_ms")) {
                (_, Some(a)) => format!(
                    ", {:.1} s after the kill due was announced, before it was sent",
                    a / 1e3
                ),
                (Some(s), None) if s >= 0.0 => format!(", {s:.0} s before the kill due"),
                (Some(s), None) => format!(
                    ", {:.0} s after the kill due, in the side jobs it waited for",
                    -s
                ),
                (None, None) => String::new(),
            };
            let by = match get(m, "death_source").map(|x| x as u8) {
                // The burst of closes, as every line says it: an older
                // run's lines wrote its first close under the same number.
                Some(1) => ", timed by the burst of closes its exit made",
                Some(2) => ", timed by Docker's die event",
                Some(3) => ", timed by Docker's FinishedAt",
                Some(_) => ", timed by the minute's look before it",
                None => "",
            };
            format!(
                "minute {minute}: {how}{due}{by}; {:.0} mismatches and {:.0} writes gone with it in the {} minutes from its line{}",
                total("mismatches"),
                total("lost_writes"),
                after.len(),
                lost_ms.map_or(String::new(), |x| format!(", the earliest answered {x:.0} ms before it"))
            )
        })
        .collect();
    match each.is_empty() {
        true => String::new(),
        false => format!(
            "; the primary's deaths, each a failure of its own: {}",
            each.join("; ")
        ),
    }
}

/// A line of the primary's in a compaction: its side file written at the
/// look, or one the server or the run ended in the minute.
fn compacted(m: &HashMap<String, f64>) -> bool {
    let g = |k: &str| m.get(k).copied().unwrap_or(0.0);
    g("compacting") > 0.0 || g("auto_compacts") > 0.0 || g("compact") > 0.0
}

/// A memory look that says something of a leak: none with a compact
/// writing at the look (`begin_beside` clones each store), none in a
/// minute the server or the run compacted.
fn quiet_look(m: &HashMap<String, f64>, replica: bool) -> bool {
    let g = |k: &str| m.get(k).copied().unwrap_or(0.0);
    match replica {
        true => g("r_compacting") == 0.0 && g("r_auto_compacts") == 0.0,
        false => {
            g("compacting") == 0.0
                && g("auto_compacts") == 0.0
                && g("compact_ops") == 0.0
                && g("compact") == 0.0
        }
    }
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
        notes: Vec::new(),
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
    // A count the end line adds to the minutes' (what the end's read found).
    let total = |k: &str| sum(k) + end.and_then(|m| get(m, k)).unwrap_or(0.0);
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
    // Stopped at once (`stop_run`): its log's STOPPED line says why.
    let stopped = find(":stopped").is_some_and(|m| get(m, "stopped") == Some(1.0));
    let why_short = match (end.is_none(), stopped) {
        (_, true) => ", STOPPED at once (its log's STOPPED line says why)",
        (true, false) => ", no end line",
        (false, false) => "",
    };
    v.push((
        CRITERIA[1],
        settings.is_some() && n >= planned && end.is_some() && !stopped,
        match settings {
            None => format!("no start line: its settings unknown{why_short}"),
            Some(_) => format!("{n} of {planned} minutes{why_short}"),
        },
    ));
    // The dead-man: a minute nothing was answered in, a connection that
    // answered nothing all minute -- a worker stuck or dead -- or a soak
    // the script's guard stopped for printing nothing.
    let dead = broke(&|m| {
        get(m, "ops").is_none_or(|x| x == 0.0) || get(m, "idle_workers").is_none_or(|x| x > 0.0)
    });
    let hung = find(":hung").is_some_and(|m| get(m, "hung") == Some(1.0));
    v.push((
        CRITERIA[2],
        n > 0 && dead.is_empty() && !hung,
        format!(
            "{n} minutes{}{}",
            minutes_said(&dead),
            match hung {
                true => format!(
                    "; the script's guard stopped it after minute {}, silent ten minutes",
                    minutes.last().map_or(0, |x| x.0)
                ),
                false => String::new(),
            }
        ),
    ));
    // The last hour against the earliest one holding its schedule: the
    // restarts' and compacts' operations apart from the steady ones, each
    // held to the same hour's, so that neither a schedule's backlog nor a
    // compact's tail decides the verdict.
    let line = |k: u64| minutes.iter().find(|(m, _)| *m == k).map(|x| x.1);
    let same = settings.and_then(|s| same_hour(n, &s));
    let pair = |k: &str| -> Option<(f64, f64)> {
        let a = same?;
        Some((get(line(a + 59)?, k)?, get(minutes.last()?.1, k)?))
    };
    // An hour with no compact's operation has no compacts' p99 (written as
    // 0 before): the compacts are not judged where either hour has none --
    // read as 0, a last hour with no compact's operation passed the half
    // against any -- the steady half judged as ever.
    let compacts = same.and_then(|a| {
        let p = |m: &HashMap<String, f64>| get(m, "p99_compact_hour").filter(|x| *x > 0.0);
        Some((p(line(a + 59)?), p(minutes.last()?.1)))
    });
    match (same, pair("p99_hour"), compacts) {
        // An hour with no operation on its schedule has no steady p99 --
        // the loop behind it, every operation a restart's or a compact's
        // backlog -- and is not held to one: the line wrote none (0 before,
        // which a last hour behind its schedule passed against any).
        (Some(a), None, _) => v.push((
            CRITERIA[3],
            false,
            format!(
                "not judged: no operation on its schedule in minutes {a}-{} or in the last hour, the open loop behind it",
                a + 59
            ),
        )),
        (Some(a), Some((s0, s1)), Some(c)) if s0 > 0.0 && s1 > 0.0 => {
            let (c_ok, c_said) = match c {
                (Some(c0), Some(c1)) => (
                    c1 <= 1.25 * c0,
                    format!("in compacts {c0:.0} us to {c1:.0} us ({:.2}x)", c1 / c0),
                ),
                (c0, c1) => (
                    true,
                    format!(
                        "compacts not judged: no compact's operation {}",
                        match (c0, c1) {
                            (None, None) => "in either hour".to_string(),
                            (None, _) => format!("in minutes {a}-{}", a + 59),
                            _ => "in the last hour".to_string(),
                        }
                    ),
                ),
            };
            v.push((
                CRITERIA[3],
                s1 <= 1.25 * s0 && c_ok,
                format!(
                    "minutes {a}-{} against the last hour: steady {s0:.0} us to {s1:.0} us ({:.2}x), {c_said}{}",
                    a + 59,
                    s1 / s0,
                    // The replica's own compactions are on no schedule:
                    // said, never held to another hour's.
                    minutes
                        .last()
                        .and_then(|x| get(x.1, "p99_r_compact_hour"))
                        .map_or(String::new(), |r| format!(
                            "; the replica's own compactions' {r:.0} us in the last hour, apart and not held to it"
                        ))
                ),
            ));
        }
        _ => v.push((
            CRITERIA[3],
            false,
            format!("not judged: no hour of {n} minutes before the last holds the last's schedule"),
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
    // Each life of the primary, from its fifth line: the line's place in
    // its life, the line and its values.
    let mut lives: BTreeMap<u64, Vec<Line>> = BTreeMap::new();
    for (k, m) in &minutes {
        if let Some(life) = get(m, "life") {
            lives.entry(life as u64).or_default().push((*k, m));
        }
    }
    /// A life's lines from its fifth: a process just started has not
    /// settled.
    fn settled<'a, 'b>(xs: &'b [Line<'a>]) -> &'b [Line<'a>] {
        &xs[xs.len().min(4)..]
    }
    // The primary is killed every half hour, so a leak is looked for
    // within each life: its anonymous memory beyond what the server holds
    // by its own account -- the engine's tables and indexes
    // (`memory_bytes`) and its replication feed (`feed_held`) -- from the
    // median of its first five quiet looks from its fifth line to the
    // median of its last five, against 5% of its anonymous memory at the
    // first five: one look in a compact, or a minute's noise, moves
    // neither, and what it holds may grow. The feed starts empty at each
    // open and fills to its bound (64 MiB) over the life's first 20 minutes
    // at 80% of the peak, longer at 50%: the dry run of 2026-11-22, its
    // anonymous memory held to a flat line, failed every life on it, the
    // most +75.9%, its memory beyond the feed and the engine's own flat
    // within a megabyte once the feed was full. And it is judged between
    // compactions, each stretch of ten quiet looks or more from its first
    // five to its last five, as a life is between kills: a compaction
    // drops what the engine held of the old file and builds it again, and
    // the heap it let go of is the allocator's to keep or hand back. The
    // second dry run of 2026-11-23 failed one life at +9.4%, its memory
    // beyond flat at 12-16 MB through minutes 65-80, then 26 MB from the
    // compaction of minutes 81-86 -- the engine's own down from some 60 MB
    // to 40-48 at it -- and flat again; the other lives' compactions moved
    // it by 5 MB at most. A leak grows inside a stretch; the steps across
    // compactions are said beside the verdict.
    let cap = settings.map_or(64, |s| s.replication_buffer) as f64 * 1048576.0;
    let mut grew = Vec::new();
    let mut most = 0.0f64;
    let mut judged_lives = 0;
    // Each life's looks: quiet, settled, and those the server's own
    // compactions took (no compact of the run's in the minute) -- what a
    // life too few looks were left in says.
    let mut looks = Vec::new();
    // The steps across compactions, said beside the verdict: the most, in
    // bytes, and its minute.
    let mut step: Option<(f64, u64)> = None;
    for (life, xs) in &lives {
        let feed = feed_held(xs, cap, *life == 0);
        // Each settled quiet look: the stretch between compactions it is
        // in, its anonymous memory, that less what the server holds, and
        // its minute. A compaction -- the run's or the server's own,
        // writing at a look or ended in a minute -- starts a stretch.
        let mut stretch = 0usize;
        let quiet: Vec<(usize, f64, f64, u64)> = xs
            .iter()
            .zip(&feed)
            .skip(xs.len() - settled(xs).len())
            .filter_map(|((k, m), f)| {
                if compacted(m) {
                    stretch += 1;
                }
                if !quiet_look(m, false) {
                    return None;
                }
                let anon = get(m, "rss_anon")?;
                Some((
                    stretch,
                    anon,
                    anon - get(m, "memory_bytes").unwrap_or(0.0) - f,
                    *k,
                ))
            })
            .collect();
        // What the memory beyond what is held did across each compaction:
        // the median of a stretch's last three quiet looks to the next's
        // first three.
        let parts: Vec<&[(usize, f64, f64, u64)]> = quiet.chunk_by(|a, b| a.0 == b.0).collect();
        for w in parts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a.len() < 3 || b.len() < 3 {
                continue;
            }
            let beyond =
                |xs: &[(usize, f64, f64, u64)]| median(&xs.iter().map(|x| x.2).collect::<Vec<_>>());
            let d = beyond(&b[..3]) - beyond(&a[a.len() - 3..]);
            if step.is_none_or(|s| d > s.0) {
                step = Some((d, b[0].3));
            }
        }
        let own = settled(xs)
            .iter()
            .filter(|(_, m)| {
                let g = |k: &str| get(m, k).unwrap_or(0.0);
                !quiet_look(m, false) && g("compact") == 0.0 && g("compact_ops") == 0.0
            })
            .count();
        let long = parts.iter().map(|p| p.len()).max().unwrap_or(0);
        looks.push(format!(
            "life {life} {} of {} ({own} in the server's own, {long} between two compactions at the most)",
            quiet.len(),
            settled(xs).len()
        ));
        let at = |xs: &[(usize, f64, f64, u64)], k: usize| -> f64 {
            median(
                &xs.iter()
                    .map(|x| if k == 0 { x.1 } else { x.2 })
                    .collect::<Vec<_>>(),
            )
        };
        let mut judged = false;
        for p in parts.iter().filter(|p| p.len() >= 10) {
            judged = true;
            let (first, last) = (&p[..5], &p[p.len() - 5..]);
            let g = (at(last, 1) - at(first, 1)) / at(first, 0).max(1.0);
            most = most.max(g);
            if g >= 0.05 && !grew.contains(life) {
                grew.push(*life);
            }
        }
        judged_lives += judged as usize;
    }
    let limit = end
        .and_then(|m| get(m, "memory_limit"))
        .filter(|x| *x > 0.0);
    let peaks_said = |p: &str| peaks(&minutes, p, limit);
    v.push((
        CRITERIA[5],
        judged_lives > 0 && grew.is_empty(),
        match judged_lives {
            // Not a leak and not a pass: the run's data left nothing to
            // judge -- under the run's memory the server compacts on its
            // own, and a life whose minutes are mostly compactions has no
            // ten quiet looks between two. Said so, each life's looks
            // beside it.
            0 => format!(
                "NOT JUDGED, too few quiet looks: no life of the primary had ten between two compactions from its fifth line, its other looks in a compaction -- quiet of settled: {}{}",
                match looks.is_empty() {
                    true => "no line names its life".to_string(),
                    false => looks.join(", "),
                },
                peaks_said("")
            ),
            _ => format!(
                "{judged_lives} of {} lives of ten quiet looks between two compactions or more, the most {:+.1}% of a life's anonymous memory grown beyond what the server holds (its engine's own and its replication feed's, {:.0} MiB at most), from the median of a stretch's first five to its last five's{}{}{}",
                lives.len(),
                most * 100.0,
                cap / 1048576.0,
                match grew.is_empty() {
                    true => String::new(),
                    false => format!("; lives {}", list(&grew)),
                },
                match step {
                    Some((d, k)) if d > 0.0 => format!(
                        "; across a compaction it stepped {:+.1} MB at most (minute {k})",
                        d / 1048576.0
                    ),
                    _ => String::new(),
                },
                peaks_said("")
            ),
        },
    ));
    // The replica is never killed, and its feed is full by the first
    // hour's end: the median of its first five settled quiet looks from
    // minute 60 against the median of every five after, five in a row
    // between two of its compactions, its engine's own memory
    // (`r_memory_bytes`, growing with the rows) taken out of both and the
    // growth held to 5% of its anonymous memory at the first five. A
    // compaction drops what the engine held of the old file and builds it
    // again, and the allocator keeps the heap it let go of while the engine
    // grows back into it: a look is settled from the sixth after one
    // (`REPLICA_SETTLE`). The third dry run of 2026-11-24's replica stood
    // 65-72 MB beyond its engine's through the second hour, 73-75 MB in the
    // two to four looks after each of its three compactions, its engine's
    // own down from 50-60 MB to 26-31 at each; five looks taken across one
    // -- minutes 104-107 and 114-116, 105 among them, where its engine's
    // own dipped 10 MB for a minute -- failed it at +6.6%, its last five
    // settled looks at +2.5%, the most of five in a row between two
    // compactions +4.9%.
    let mut stretch = 0usize;
    let mut since = 0usize;
    let rq: Vec<(usize, u64, f64, f64)> = minutes
        .iter()
        .filter_map(|(k, m)| {
            if !quiet_look(m, true) {
                stretch += 1;
                since = 0;
                return None;
            }
            since += 1;
            if *k < 60 || since <= REPLICA_SETTLE {
                return None;
            }
            let anon = get(m, "r_rss_anon")?;
            Some((
                stretch,
                *k,
                anon,
                anon - get(m, "r_memory_bytes").unwrap_or(0.0),
            ))
        })
        .collect();
    let windows: Vec<&[(usize, u64, f64, f64)]> = match rq.len() >= 10 {
        true => rq[5..]
            .chunk_by(|a, b| a.0 == b.0)
            .flat_map(|p| p.windows(5))
            .collect(),
        false => Vec::new(),
    };
    match windows.is_empty() {
        false => {
            let first: Vec<f64> = rq[..5].iter().map(|x| x.2).collect();
            let b = median(&first);
            let base = median(&rq[..5].iter().map(|x| x.3).collect::<Vec<_>>());
            let mut over = Vec::new();
            let mut most = 0.0f64;
            for w in &windows {
                let xs: Vec<f64> = w.iter().map(|x| x.3).collect();
                let g = (median(&xs) - base) / b.max(1.0);
                most = most.max(g);
                if g >= 0.05 {
                    over.push(w[4].1);
                }
            }
            v.push((
                CRITERIA[6],
                over.is_empty(),
                format!(
                    "{:.0} MB from minute {}, the most {:+.1}% after{}; {} settled quiet looks, the {REPLICA_SETTLE} after each of its compactions left out{}",
                    b / 1048576.0,
                    rq[0].1,
                    most * 100.0,
                    minutes_said(&over),
                    rq.len(),
                    peaks_said("r_")
                ),
            ));
        }
        true => v.push((
            CRITERIA[6],
            false,
            format!(
                "not judged: {} settled quiet looks after the first hour, ten needed, five in a row between two compactions after the first five{}",
                rq.len(),
                peaks_said("r_")
            ),
        )),
    }
    // Threads and descriptors less the connections open as they were
    // looked at: each connection is a thread and a socket, and the
    // harness's own -- its workers', subscriptions', the scrape's -- move
    // with its doings. Each server within 2 of its run's median every
    // minute; and the primary, which starts again every half hour, within
    // 2 from the median of a life's first five looks to its last five --
    // a leak of a few a life never leaves a run-wide band. A task the
    // engine cuts in shares runs them on a thread a CPU of the VM but the
    // one that asked, joined before it returns (`vector::spread`, under
    // `std::thread::scope`: an image's index, a graph's lists, an exact
    // scan): a look that lands in one sees up to the CPUs less one more,
    // and the next look none of them. Such a look is said apart, never
    // failed; a thread left over is at the next look too. The second dry
    // run of 2026-11-23 failed the primary on one look, minute 17, 7 more
    // on the VM's 8 CPUs, back at 6 the next minute, 0 within each life.
    let cpus = settings.map_or(0, |s| s.cpus) as f64;
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
            let sorted: Vec<f64> = net.iter().map(|x| x.1).collect();
            if sorted.is_empty() {
                ok = false;
                write!(detail, "{who}: none; ").unwrap();
                continue;
            }
            let mid = median(&sorted);
            let within = |x: f64| (x - mid).abs() <= 2.0;
            // A look over the band by no more than a task in shares adds,
            // the next look back within it.
            let shares = |i: usize| {
                let x = net[i].1;
                k.ends_with("threads")
                    && x > mid
                    && x - mid <= 2.0 + (cpus - 1.0).max(0.0)
                    && net.get(i + 1).is_some_and(|n| within(n.1))
            };
            let (mut off, mut spread) = (Vec::new(), Vec::new());
            for (i, x) in net.iter().enumerate() {
                match (within(x.1), shares(i)) {
                    (true, _) => {}
                    (false, true) => spread.push(x.0),
                    (false, false) => off.push(x.0),
                }
            }
            ok &= off.is_empty();
            let (lo, hi) = sorted
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
            write!(
                detail,
                "{who}: median {mid:.0}, {lo:.0} to {hi:.0}{}{}",
                minutes_said(&off),
                match spread.is_empty() {
                    true => String::new(),
                    false => format!(
                        "; a task on the VM's {cpus:.0} CPUs at minutes {}, gone at the next look",
                        list(&spread)
                    ),
                }
            )
            .unwrap();
            if who == "the primary" {
                let mut moved = Vec::new();
                let mut most = 0.0f64;
                for (life, xs) in &lives {
                    let net: Vec<f64> = settled(xs)
                        .iter()
                        .filter_map(|(_, m)| Some(get(m, k)? - get(m, cn)?))
                        .collect();
                    if net.len() < 10 {
                        continue;
                    }
                    let d = median(&net[net.len() - 5..]) - median(&net[..5]);
                    most = most.max(d.abs());
                    if d.abs() > 2.0 {
                        moved.push(*life);
                    }
                }
                ok &= moved.is_empty();
                write!(
                    detail,
                    ", within a life {most:.0} at most{}",
                    match moved.is_empty() {
                        true => String::new(),
                        false => format!(" (lives {})", list(&moved)),
                    }
                )
                .unwrap();
            }
            detail.push_str("; ");
        }
        v.push((c, ok, detail.trim_end_matches("; ").to_string()));
    }
    let steps = broke(&|m| get(m, "clock_step") == Some(1.0));
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
                "{:.0}{}{}{}{}{}",
                total + at_end,
                minutes_said(&b),
                match at_end > 0.0 {
                    true => format!("; {at_end:.0} found at the end"),
                    false => String::new(),
                },
                match key == "mismatches" && !steps.is_empty() {
                    true => format!("; the VM's clock stepped at minutes {}", list(&steps)),
                    false => String::new(),
                },
                // A death's cut connections are its own (`CRITERIA[18]`):
                // said, counted as `down` rather than here.
                match (key, sum("death_down")) {
                    ("errors", d) if d > 0.0 => format!(
                        "; {d:.0} connections a death of the primary cut before it was found counted with it, as down"
                    ),
                    _ => String::new(),
                },
                // A restart's catch-up's 504s are its own (`Catch`), said.
                match (key, sum("catchup_down")) {
                    ("errors", d) if d > 0.0 => format!(
                        "; {d:.0} reads through the replica a 504 answered in a restart's catch-up counted with it, as down"
                    ),
                    _ => String::new(),
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
    let back_max = settings.map_or(0.0, |s| s.replica_back_max as f64 * 1e3);
    let late_back = broke(&|m| get(m, "replica_back_ms").is_some_and(|x| x > back_max));
    let back_most = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "replica_back_ms"))
        .fold(0.0f64, f64::max);
    // And caught up with the writes of the loop's backlog after it, held
    // to the same bound from the loop's return (`Catch`): -1 where it was
    // still short of the change answered then as the bound passed.
    let short = broke(&|m| get(m, "catchup_replica_ms").is_some_and(|x| x < 0.0));
    let caught_most = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "catchup_replica_ms"))
        .fold(0.0f64, f64::max);
    let follow = start.and_then(|m| Some((get(m, "follow_s")?, get(m, "follow_max")?)));
    let catchup = end.and_then(|m| get(m, "catchup_s"));
    let catchup_max = settings.map_or(0.0, |s| s.catchup_max as f64);
    let differs = end.and_then(|m| get(m, "replica_differs"));
    v.push((
        CRITERIA[12],
        lagging.is_empty()
            && late_back.is_empty()
            && short.is_empty()
            && follow.is_some_and(|(s, max)| s >= 0.0 && s <= max)
            && catchup.is_some_and(|s| s >= 0.0 && s <= catchup_max)
            && differs == Some(0.0),
        format!(
            "connected and within {lag_max:.0} s of the primary{}; followed in {}; back after a restart within {:.1} s at most, the bound {:.0} s{}, and holding the change answered as the loop was back on its schedule within {:.1} s of it at most{}; caught up at the end in {}; {}",
            minutes_said(&lagging),
            follow.map_or("?".into(), |(s, _)| format!("{s:.0} s")),
            back_most / 1e3,
            back_max / 1e3,
            match late_back.is_empty() {
                true => String::new(),
                false => format!(" (past it at minutes {})", list(&late_back)),
            },
            caught_most / 1e3,
            match short.is_empty() {
                true => String::new(),
                false => format!(" (NOT within the bound at minutes {})", list(&short)),
            },
            catchup.map_or("? (no end line)".into(), |s| format!("{s:.0} s of {catchup_max:.0}")),
            match differs {
                Some(0.0) => "the same rows",
                Some(_) => "DIFFERENT rows",
                None => "rows not compared",
            }
        ),
    ));
    // A restart's own time is bounded, and so is the wait of an operation
    // due in it: no longer than the longest restart it could have been due
    // in and two seconds -- one held longer was held by something else than
    // the restart. The wait is judged on the kills the loop met on its
    // schedule: at one it met behind, every operation due waits out the
    // backlog first, which is the open loop's verdict to judge, not the
    // restart's. The third dry run of 2026-11-24 held its p99 of 145 s to
    // the backlog at the look before its kill of minute 60, 114 s, and
    // failed -- the kill waited five minutes for a compact, the loop 166 s
    // behind when it came. A read through the replica due in one waits
    // longer by design: none is sent through it from the kill's
    // announcement until it says it is back (`replica_back_ms`, from the
    // server's answering again) and the restart's grace is over, so its
    // bound is the longest of a restart and that wait, and two seconds. A
    // line written before the two were apart holds them together in
    // `p99_restart`, held to the primary's.
    //
    // A kill none of whose operations the two figures hold sets neither
    // bound: one met behind the schedule, whose operations go to the
    // backlog's figure (`restart_behind`) or the replica's compaction's,
    // and one met on it whose every operation was the replica's
    // compaction's, its workers held by reads through it as the kill came.
    // Its restart bounds no operation judged, and only loosened the bound
    // of the others': the fifth dry run of 2026-11-24's kill of minute 60
    // was both, the loop 15.9 s behind with the replica compacting and its
    // 13 888 operations all the compaction's, and its restart of 6.0 s set
    // the bound of 8.0 s the other two kills' operations were held to,
    // their restarts 3.6 and 3.5 s. Its operations are said, with their
    // count, as the compaction's.
    let restart_max = settings.map_or(0, |s| s.restart_max) as f64;
    let slow = broke(&|m| get(m, "restart_ms").is_some_and(|x| x > restart_max * 1e3));
    let p99r = end.and_then(|m| get(m, "p99_restart"));
    let r_ops = end.and_then(|m| get(m, "restart_ops"));
    let p99rr = end.and_then(|m| get(m, "p99_restart_replica"));
    let rr_ops = end.and_then(|m| get(m, "restart_replica_ops"));
    let grace_ms = GRACE as f64 / 1e6;
    let held_by_replica = sum("restart_r_compact_ops");
    let longest = end
        .and_then(|m| get(m, "restart_ms_max"))
        .unwrap_or(0.0)
        .max(
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, "restart_ms"))
                .fold(0.0, f64::max),
        );
    let parts = |k: &str| -> Vec<f64> { minutes.iter().filter_map(|(_, m)| get(m, k)).collect() };
    let starts = parts("start_ms");
    let start_most = starts.iter().copied().fold(0.0f64, f64::max);
    // Each kill, and how far behind its schedule the loop was as it was
    // announced (`kill_behind_s`, on the kill's line; for a run before it
    // was written, the look before the kill's line, `behind_s`).
    let kills_seen: Vec<(u64, f64)> = minutes
        .iter()
        .filter(|(_, m)| get(m, "kill").is_some_and(|x| x > 0.0))
        .map(|(k, m)| {
            let before = || line(k.checked_sub(1)?).and_then(|m| get(m, "behind_s"));
            (*k, get(m, "kill_behind_s").or_else(before).unwrap_or(0.0))
        })
        .collect();
    let behind_at: Vec<(u64, f64)> = kills_seen
        .iter()
        .copied()
        .filter(|x| x.1 > ON_SCHEDULE_S)
        .collect();
    let on_time = kills_seen.len() - behind_at.len();
    // A kill's own operations: those on its lines, from the line before
    // its own to the line before the next kill's -- a restart is answered
    // in seconds, the kills half an hour apart -- `None` where its lines
    // have no such figure (a run's from before it was written).
    let of_kill = |k: u64, key: &str| -> Option<f64> {
        let next = kills_seen
            .iter()
            .map(|x| x.0)
            .find(|&j| j > k)
            .unwrap_or(u64::MAX);
        let xs: Vec<f64> = minutes
            .iter()
            .filter(|(j, _)| *j + 1 >= k && *j + 1 < next)
            .filter_map(|(_, m)| get(m, key))
            .collect();
        (!xs.is_empty()).then(|| xs.iter().sum())
    };
    let judged_in = |k: u64| {
        Some(of_kill(k, "restart_ops")? + of_kill(k, "restart_replica_ops").unwrap_or(0.0))
    };
    let rc_of = |ks: &[u64]| -> f64 {
        ks.iter()
            .filter_map(|&k| of_kill(k, "restart_r_compact_ops"))
            .sum()
    };
    // The kills met on the schedule none of whose operations is judged.
    let none_judged: Vec<u64> = kills_seen
        .iter()
        .filter(|x| x.1 <= ON_SCHEDULE_S && judged_in(x.0) == Some(0.0))
        .map(|x| x.0)
        .collect();
    let unbounding: Vec<u64> = behind_at
        .iter()
        .map(|x| x.0)
        .chain(none_judged.iter().copied())
        .collect();
    let bounding = minutes
        .iter()
        .filter(|(k, _)| !unbounding.contains(k))
        .map(|x| x.1)
        .collect::<Vec<_>>();
    let longest_of = |ms: &[&HashMap<String, f64>],
                      f: &dyn Fn(&HashMap<String, f64>) -> Option<f64>| {
        ms.iter().filter_map(|m| f(m)).fold(0.0f64, f64::max)
    };
    let all: Vec<&HashMap<String, f64>> = minutes.iter().map(|x| x.1).collect();
    let back_of = |m: &HashMap<String, f64>| {
        Some(get(m, "restart_ms")? + get(m, "replica_back_ms")?.max(grace_ms))
    };
    // None left to bound -- every kill behind or none judged -- the
    // longest of all, as before: no operation is judged by it then.
    let bound = match longest_of(&bounding, &|m| get(m, "restart_ms")) {
        0.0 => longest,
        x => x,
    } / 1e3
        + 2.0;
    let rbound = match longest_of(&bounding, &back_of) {
        0.0 => longest_of(&all, &back_of),
        x => x,
    } / 1e3
        + 2.0;
    let p99b = end.and_then(|m| get(m, "p99_restart_behind"));
    let ids = |ks: &[u64]| -> (&str, &str, String) {
        match ks.len() {
            1 => ("", "it", list(ks)),
            _ => ("s", "them", list(ks)),
        }
    };
    let behind_rc = rc_of(&behind_at.iter().map(|x| x.0).collect::<Vec<_>>());
    let apart = match behind_at.is_empty() {
        true => String::new(),
        false => {
            let ks: Vec<u64> = behind_at.iter().map(|x| x.0).collect();
            let (_, it, at) = ids(&ks);
            format!(
                "; {} met behind it, at minutes {at} ({:.0} s at the most), {}",
                behind_at.len(),
                behind_at.iter().map(|x| x.1).fold(0.0f64, f64::max),
                match (p99b, behind_rc > 0.0) {
                    (Some(x), false) => format!(
                        "whose operations' p99 {:.1} s is the backlog's, said apart",
                        x / 1e6
                    ),
                    (Some(x), true) => format!(
                        "whose operations' p99 {:.1} s is the backlog's, said apart, and {behind_rc:.0} due in {it} the replica's own compaction's, judged with it",
                        x / 1e6
                    ),
                    (None, true) => format!(
                        "every operation due in {it} gone to the replica's own compaction, {behind_rc:.0} of them, judged with it, and none to the backlog's figure"
                    ),
                    (None, false) => format!(
                        "no figure of the backlog's for the operations due in {it} on this run's lines"
                    ),
                }
            )
        }
    };
    let none_rc = rc_of(&none_judged);
    let none_said = match none_judged.is_empty() {
        true => String::new(),
        false => {
            let (s, it, at) = ids(&none_judged);
            format!(
                "; at minute{s} {at}, met on it, every operation due in {it} gone to the replica's own compaction, {none_rc:.0} of them, judged with it"
            )
        }
    };
    let unbounding_said = match unbounding.is_empty() {
        true => String::new(),
        false => {
            let mut ks = unbounding.clone();
            ks.sort_unstable();
            let (s, it, at) = ids(&ks);
            format!(
                "; the restart{s} at minute{s} {at} set{} no bound, none of the operations due in {it} among them",
                if s.is_empty() { "s" } else { "" }
            )
        }
    };
    // A line before the reads through the replica were apart has no
    // figure of their own; one of a run that sent none through it during
    // a restart, no p99 (written as 0 before).
    let (through_replica, replica_ok) = match (rr_ops, p99rr) {
        (None, _) => (
            ", the reads through the replica among them (no figure of their own on this run's lines)"
                .to_string(),
            true,
        ),
        (Some(n), Some(y)) if n > 0.0 => (
            format!(
                " at the primary and {:.1} s of {rbound:.1} through the replica",
                y / 1e6
            ),
            y <= rbound * 1e6,
        ),
        (Some(n), None) if n > 0.0 => (
            format!(" at the primary and NO figure for the {n:.0} through the replica"),
            false,
        ),
        (Some(_), _) => (
            format!(" at the primary and none through the replica (its bound {rbound:.1})"),
            true,
        ),
    };
    // A kill met on its schedule owes operations due in it to judge: a
    // run whose every one was a replica compaction's -- its workers held
    // as the kill came -- judged none, and passed on a p99 of 0 written
    // for none.
    let judged_ops = kills_seen.is_empty() || r_ops.is_some_and(|n| n > 0.0);
    let judged_kills = on_time - none_judged.len();
    let held_rest = (held_by_replica - behind_rc - none_rc).max(0.0);
    v.push((
        CRITERIA[13],
        settings.is_some()
            && slow.is_empty()
            && (kills_seen.is_empty() || on_time > 0)
            && judged_ops
            && (kills_seen.is_empty() || p99r.is_some_and(|x| x <= bound * 1e6))
            && replica_ok,
        format!(
            "the longest {:.1} s of {restart_max:.0}{}; the server's own part, docker start to answering, {:.1} s at the median and {:.1} s at most; {}{none_said}{apart}{}; {:.0} met a restart",
            longest / 1e3,
            minutes_said(&slow),
            median(&starts) / 1e3,
            start_most / 1e3,
            match (end.is_some(), kills_seen.is_empty() || on_time > 0, judged_ops, p99r) {
                (false, _, _, _) => "p99 of the operations due in one ? (no end line)".to_string(),
                (true, false, _, _) => format!(
                    "the operations due in one NOT JUDGED: the loop behind its schedule at each of the {} kills",
                    kills_seen.len()
                ),
                (true, true, false, _) | (true, true, true, None) if !kills_seen.is_empty() => format!(
                    "NO operation due in one judged, over the {on_time} kills the loop met on its schedule"
                ),
                (true, true, _, None) => "no kill, no operation due in one".to_string(),
                (true, true, _, Some(x)) => format!(
                    "p99 of the operations due in one {:.1} s of {bound:.1}{through_replica}{}{unbounding_said}",
                    x / 1e6,
                    match kills_seen.is_empty() {
                        true => ", no kill".to_string(),
                        false => format!(", over the {judged_kills} kills the loop met on its schedule"),
                    }
                ),
            },
            match held_rest > 0.0 {
                true => format!(
                    "; {held_rest:.0} due in one the replica's own compaction's, their workers held by reads through it as it compacted, judged with it"
                ),
                false => String::new(),
            },
            sum("down")
        ),
    ));
    let (kills, in_compact, compacts, backups) = settings.map_or((0, 0, 0, 0), |s| owed(n, &s));
    let got = |k: &str| sum(k) as u64;
    let differ = total("backup_differs") as u64;
    // A death is a failure of its own (`CRITERIA[18]`), apart from the
    // planned kills: a kill it found the server dead at, and a compact or
    // a backup it cut, are its, counted with it here -- the long run's
    // death at its kill of minute 300 counted as a kill, timed by it.
    // Every restart verifies the file: a death's as a kill's.
    let (kill_died, died) = (got("kill_died"), got("died"));
    let mut short = Vec::new();
    for (what, have, want, exact) in [
        ("kills", got("kill") + kill_died, kills, true),
        (
            "verifies",
            got("verify"),
            (kills + died).saturating_sub(kill_died),
            true,
        ),
        // One the server's own compact turned away was compacted all the
        // same.
        (
            "compacts",
            got("compact") + got("compact_busy") + got("compact_died"),
            compacts,
            false,
        ),
        (
            "kills that cut a compact",
            got("compact_killed"),
            in_compact.saturating_sub(kill_died.min(in_compact)),
            false,
        ),
        (
            "backups",
            got("backup").saturating_sub(differ) + got("backup_died"),
            backups,
            false,
        ),
        (
            "restores",
            got("restore") + got("backup_died"),
            backups,
            false,
        ),
    ] {
        if (exact && have != want) || have < want {
            short.push(format!("{what} {have} of {want}"));
        }
    }
    // A kill meant to land in a compact must find one writing its file
    // (its side file left behind: `kill_compacting`, on the line after the
    // kill's minute). A compact request the restart cut (`compact_killed`)
    // alone does not say so: a compact that ended just before the kill
    // landed leaves its request cut all the same.
    if let Some(s) = settings {
        for m in 1..n {
            let due = schedule(
                m,
                n,
                s.kill_every,
                s.kill_in_compact_every,
                s.compact_every,
                s.backup_every,
                s.backup_at,
            );
            if due.contains(&Doing::Kill { in_compact: true })
                && line(m + 1).and_then(|x| get(x, "kill_compacting")) != Some(1.0)
                && !line(m + 1).is_some_and(|x| get(x, "kill_died").is_some_and(|d| d > 0.0))
            {
                short.push(format!("the kill at minute {m} found no compact writing"));
            }
        }
    }
    v.push((
        CRITERIA[14],
        settings.is_some() && short.is_empty(),
        match short.is_empty() {
            true => format!(
                "{kills} kills ({in_compact} cutting a compact) and verifies, {} compacts ({} turned away by the server's own), {} backups and restores{}",
                got("compact") + got("compact_busy"),
                got("compact_busy"),
                got("backup"),
                match (kill_died, got("compact_died") + got("backup_died")) {
                    (0, 0) => String::new(),
                    (k, c) => format!(
                        "; {k} kills found the server dead and {c} compacts and backups were cut by a death, each the death's"
                    ),
                }
            ),
            false => short.join(", "),
        },
    ));
    let bad = sum("verify_failed") + sum("backup_failed") + sum("compact_failed") + differ as f64;
    let end_bad = end.map_or(1.0, |m| {
        get(m, "verify_failed").unwrap_or(1.0) + get(m, "replica_verify_failed").unwrap_or(1.0)
    });
    v.push((
        CRITERIA[15],
        bad == 0.0 && end_bad == 0.0,
        format!(
            "{:.0} at restarts, {:.0} restores; {:.0} verifies failed, {:.0} backups ({:.0} of them stale, {differ} holding a record no write left), {:.0} compacts; at the end {}",
            sum("verify"),
            sum("restore"),
            sum("verify_failed"),
            sum("backup_failed") + differ as f64,
            sum("backup_stale"),
            sum("compact_failed"),
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
    let (owed_back, back, sweeps_failed) = (sum("sweep_owed"), sum("swept"), sum("sweep_failed"));
    v.push((
        CRITERIA[17],
        swept.is_some_and(|(s, k)| s >= k && k > 0.0) && back >= owed_back && sweeps_failed == 0.0,
        format!(
            "{}; after the restarts {back:.0} of {owed_back:.0} keys read back{}",
            match swept {
                None => "no end line".to_string(),
                Some((s, k)) => format!("{s:.0} of {k:.0} keys at the end"),
            },
            match sweeps_failed > 0.0 {
                true => format!(", {sweeps_failed:.0} sweeps FAILED to read"),
                false => String::new(),
            }
        ),
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
            "{:.0} deaths, {:.0} OOM kills, the replica stopped {:.0} times{}{}",
            sum("died"),
            sum("oom"),
            sum("r_died"),
            minutes_said(&gone),
            deaths_said(&minutes)
        ),
    ));
    // The open loop's rate is the run's premise: a server that answers
    // under it leaves every connection behind its schedule, and from then
    // on each latency the run writes is the queue's. A compact's or a
    // restart's backlog drains -- at 80% of the peak a kill's in four
    // times as long as the server was gone -- so the loop must be back on
    // its schedule (the median connection within a second of it) at some
    // look of every stretch as long as the kill interval; one behind
    // longer was asked for more than the server answers. The second dry run of 2026-11-23,
    // at 80% of a peak measured over four minutes of a fresh file, fell
    // behind at minute 13 as the file passed 1.8 GB under the run's 512 MB
    // and never came back, 1 656 s behind at its end.
    let every = settings.map_or(30, |s| match s.kill_every {
        0 => 30,
        k => k,
    });
    // Each lane alike: the pool's (`behind_s`) on every line, and the
    // replica lane's (`r_behind_s`) where the run had one -- its 4% of the
    // rate on a schedule of its own, which a replica that never keeps up
    // with them leaves behind without the pool's lag showing it.
    // Each line's lag, and each long stretch of lines behind: its first
    // and last minute and the most it was behind.
    type Lags = (Vec<(u64, f64)>, Vec<(u64, u64, f64)>);
    let lag = |name: &str| -> Lags {
        let looked: Vec<(u64, f64)> = minutes
            .iter()
            .filter_map(|(k, m)| Some((*k, get(m, name)?)))
            .collect();
        // Each stretch of lines behind (over a second), from its first to
        // its last, and the most it was behind.
        let mut stretches: Vec<(u64, u64, f64)> = Vec::new();
        for &(k, b) in &looked {
            if b <= ON_SCHEDULE_S {
                continue;
            }
            match stretches.last_mut() {
                Some(x) if x.1 + 1 == k => {
                    x.1 = k;
                    x.2 = x.2.max(b);
                }
                _ => stretches.push((k, k, b)),
            }
        }
        stretches.retain(|x| x.1 - x.0 + 1 >= every);
        (looked, stretches)
    };
    let (looked, long) = lag("behind_s");
    let (r_looked, r_long) = lag("r_behind_s");
    let most = |l: &[(u64, f64)]| l.iter().map(|x| x.1).fold(0.0f64, f64::max);
    let behind = |lane: &str, long: &[(u64, u64, f64)]| {
        long.iter()
            .map(|x| {
                format!(
                    "{lane} from minute {} to {}, {:.0} s at the most",
                    x.0, x.1, x.2
                )
            })
            .collect::<Vec<_>>()
    };
    v.push((
        CRITERIA[19],
        !looked.is_empty() && looked.len() as u64 == n && long.is_empty() && r_long.is_empty(),
        match (looked.is_empty(), long.is_empty() && r_long.is_empty()) {
            (true, _) => "not judged: no line says how far behind its schedule the loop was".into(),
            (false, true) => format!(
                "back on it within every {every} minutes, {:.1} s behind at the most{}{}",
                most(&looked),
                match r_looked.is_empty() {
                    true => String::new(),
                    false => format!(", the replica lane {:.1} s", most(&r_looked)),
                },
                match looked.len() as u64 == n {
                    true => String::new(),
                    false => format!("; {} of {n} lines say how far", looked.len()),
                }
            ),
            (false, false) => {
                let mut said = behind("behind", &long);
                said.extend(behind("the replica lane behind", &r_long));
                said.join("; ")
            }
        },
    ));
    // The reads held by the replica's own compactions, and their workers'
    // backlog after (`Class::ReplicaCompact`): none waits longer than the
    // compaction holding it, and two seconds -- one held past it was held
    // by something else. The bound is the longest the watch saw end, or
    // the one still open at the last look, so long so far, where the
    // watch was looking then: one whose watch had gone at the last look is
    // no measure -- a watch gone in it held it open to the run's end, its
    // length so far hours. The stall itself, the server's (a compaction's
    // reads waiting on the replica), is said, not judged here. A
    // compaction a kill's restart met held its reads the longer by the
    // restart -- its workers held by it, then by the primary's restart and
    // the replica's return, or the other way round -- and its bound is the
    // two together (`r_compact_kill_ms`, `restart_met`): the fifth dry run
    // of 2026-11-24's kill of minute 60 landed in the replica's compaction
    // of that minute, 33.5 s, and its restart held reads through the
    // replica 18 s and more. The replica compacts every 20 minutes or so
    // and the kills come every 30, so an eight-hour run meets it again.
    let last = minutes.last().map(|x| x.1);
    let alive_at_end = last.is_some_and(|m| get(m, "r_watch") == Some(1.0));
    let open_ms = last.and_then(|m| get(m, "r_compact_open_ms"));
    let unwatched_open = match (open_ms, alive_at_end) {
        (Some(_), false) => "; the one open at the last look not counted, its watch gone",
        _ => "",
    };
    let rc_ops = end.and_then(|m| get(m, "r_compact_ops"));
    let rc_p99 = end.and_then(|m| get(m, "p99_r_compact"));
    let rc_alone = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "r_compact_ms"))
        .chain(open_ms.filter(|_| alive_at_end))
        .fold(0.0f64, f64::max);
    let rc_kill = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "r_compact_kill_ms"))
        .fold(0.0f64, f64::max);
    let rc_longest = rc_alone.max(rc_kill);
    let rc_bound = rc_longest / 1e3 + 2.0;
    let rc_by = match rc_kill > rc_alone {
        false => format!("the longest watched, {:.1} s, and two", rc_alone / 1e3),
        true => format!(
            "the longest watched with the restart of a kill that landed in it, {:.1} s, and two; the longest alone {:.1} s",
            rc_kill / 1e3,
            rc_alone / 1e3
        ),
    };
    let (rc_ok, rc_said) = match (rc_ops, rc_p99) {
        (Some(n), Some(p)) if n > 0.0 && rc_longest > 0.0 => (
            p <= rc_bound * 1e6,
            format!(
                "p99 of the {n:.0} operations they held {:.1} s of {rc_bound:.1} ({rc_by}); {:.0} of them due in a restart{unwatched_open}",
                p / 1e6,
                sum("restart_r_compact_ops")
            ),
        ),
        (Some(n), Some(p)) if n > 0.0 => (
            false,
            format!(
                "p99 of the {n:.0} operations they held {:.1} s, and NO compaction the watch saw end to bound them{unwatched_open}",
                p / 1e6
            ),
        ),
        (Some(n), None) if n > 0.0 => (false, format!("NO figure for the {n:.0} operations they held")),
        (Some(_), _) => (
            true,
            "no operation held by one: no figure, nothing judged".to_string(),
        ),
        (None, _) => (
            true,
            "no figure of their own on this run's lines: nothing judged".to_string(),
        ),
    };
    v.push((CRITERIA[20], rc_ok, rc_said + &rc_figures(&minutes, end)));
    // The watch on the replica's compaction thread against its counter
    // (`fennec_auto_compactions_total`): a compaction is counted where its
    // end was seen as the thread going back to sleep and the counter moved
    // in it, so the two agree but for one still open at the last look,
    // which the counter may have counted and the watch not yet ended --
    // where the watch was looking then: one whose watch had gone may be
    // anything. One the counter saw and the watch did not -- not watched
    // then, or cut by the watch's own going -- or the watch saw and the
    // counter did not, fails: a read through the replica in it was classed
    // by a window that was not there. Those the counter judged across a
    // reset, on a base read before they began, or not at all are said. A
    // window the watch lost while it was open -- gone a minute at a look,
    // or the thread found asleep by the next container (`r_compacts_lost`)
    // -- or open at the last look with the watch gone, is unseen, and
    // fails whatever the counter says: a thread that never slept again,
    // its watch gone with it, left the counter unmoved and the two agreed.
    // One past its bound is the next criterion's: its end, seen, is judged
    // by the counter like any other's, and never counted as cut -- before,
    // it failed here too, "cut before their end was seen".
    let watch_said = minutes.iter().any(|(_, m)| get(m, "r_watch").is_some());
    let watched_any = minutes.iter().any(|(_, m)| get(m, "r_watch") == Some(1.0));
    let counted = sum("r_auto_compacts");
    let watched = sum("r_compacts_watched");
    let open_last = last.is_some_and(|m| get(m, "r_compact_open") == Some(1.0));
    let open_at_end = open_last && alive_at_end;
    let unwatched: Vec<u64> = minutes
        .iter()
        .filter(|(_, m)| get(m, "r_watch") == Some(0.0))
        .map(|(k, _)| *k)
        .collect();
    let agree = watched == counted || open_at_end && counted == watched + 1.0;
    let lost = sum("r_compacts_lost");
    let lost_at_end = open_last && !alive_at_end;
    let many = |n: f64, one: &'static str, more: &'static str| if n == 1.0 { one } else { more };
    v.push((
        CRITERIA[21],
        watched_any && agree && lost == 0.0 && !lost_at_end,
        match (watch_said, watched_any) {
            (false, _) => format!(
                "not watched: no line says whether the watch looked; {counted:.0} by its counter"
            ),
            (true, false) => format!(
                "not watched at any look; {counted:.0} by its counter"
            ),
            (true, true) => format!(
                "{counted:.0} by its counter, {watched:.0} seen to end at its compaction thread{}{}{}{}{}{}",
                match (open_last, alive_at_end) {
                    (true, true) => "; one open at the last look",
                    (true, false) => "; one open at the last look, its watch GONE: unseen, no allowance",
                    (false, _) => "",
                },
                match lost {
                    0.0 => String::new(),
                    l => format!(
                        "; {l:.0} LOST by the watch while open, {} end unseen",
                        many(l, "its", "their")
                    ),
                },
                match sum("r_compacts_cut") {
                    0.0 => String::new(),
                    c => format!(
                        "; {c:.0} cut before {} end was seen, not counted",
                        many(c, "its", "their")
                    ),
                },
                match sum("r_compacts_false") {
                    0.0 => String::new(),
                    f => format!(
                        "; {f:.0} {} its counter did not move in, no compaction",
                        many(f, "window", "windows")
                    ),
                },
                [
                    ("r_compacts_reset", "across a reset of its counter, the replica started again in it"),
                    ("r_compacts_stale", "on a read of its counter from before it began, the read as it began failed"),
                    ("r_compacts_unread", "on no read of its counter at its end, the watch's word"),
                ]
                .iter()
                .filter_map(|(k, how)| match sum(k) {
                    0.0 => None,
                    x => Some(format!("; {x:.0} judged {how}")),
                })
                .collect::<String>(),
                match unwatched.is_empty() {
                    true => String::new(),
                    false => format!("; not watched at minutes {}", list(&unwatched)),
                }
            ),
        },
    ));
    // Each window of the replica's held to what a compaction of its file
    // plausibly takes (`plausible_ms`, `r_compact_bound_ms` at each look):
    // one open past it -- a thread that never sleeps again -- is a failure
    // of its own, counted as the run found it (`r_compacts_overlong`) and
    // cut, never the open window that excuses a count or bounds the reads.
    let bounds: Vec<f64> = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "r_compact_bound_ms"))
        .collect();
    let overlong: Vec<u64> = minutes
        .iter()
        .filter(|(_, m)| get(m, "r_compacts_overlong").is_some_and(|x| x > 0.0))
        .map(|(k, _)| *k)
        .collect();
    // One open at the last look, the watch looking: past its bound it
    // fails, where the run's look at it came too late to cut it.
    let open_past = last
        .and_then(|m| Some((get(m, "r_compact_open_ms")?, get(m, "r_compact_bound_ms")?)))
        .filter(|(o, b)| alive_at_end && o > b);
    let longest = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "r_compact_ms"))
        .chain(open_ms)
        .fold(0.0f64, f64::max);
    let (lo, hi) = bounds
        .iter()
        .fold((f64::MAX, 0.0f64), |a, b| (a.0.min(*b), a.1.max(*b)));
    v.push((
        CRITERIA[22],
        overlong.is_empty() && open_past.is_none(),
        match (bounds.is_empty(), overlong.is_empty(), open_past) {
            (true, _, _) => "no bound on this run's lines: nothing judged".to_string(),
            (false, true, None) => format!(
                "the longest {:.1} s, the bound {:.0} to {:.0} s (the replica's file at {} s a GB, or {}x the primary's slowest compact a GB, {} s at the least)",
                longest / 1e3,
                lo / 1e3,
                hi / 1e3,
                R_COMPACT_MS_PER_GB / 1_000,
                R_COMPACT_PRIMARY_TIMES,
                R_COMPACT_FLOOR_MS / 1_000
            ),
            (false, _, past) => format!(
                "{}{}",
                match overlong.is_empty() {
                    true => String::new(),
                    false => format!(
                        "{:.0} OPEN PAST their bound, found at minutes {}, cut",
                        sum("r_compacts_overlong"),
                        list(&overlong)
                    ),
                },
                match past {
                    Some((o, b)) => format!(
                        "{}one open {:.0} s at the last look, PAST its bound of {:.0} s",
                        if overlong.is_empty() { "" } else { "; " },
                        o / 1e3,
                        b / 1e3
                    ),
                    None => String::new(),
                }
            ),
        },
    ));
    judged.notes = notes(&minutes, start, end, settings);
    judged
}

/// The figures a run gives beside its verdicts, a line each: each kill's
/// state, the two bounds set without a measured run against what this
/// one measured -- whether it supports each, or what it suggests -- the
/// VM's clock, and what else used the machine.
fn notes(
    minutes: &[(u64, &HashMap<String, f64>)],
    start: Option<&HashMap<String, f64>>,
    end: Option<&HashMap<String, f64>>,
    settings: Option<Settings>,
) -> Vec<String> {
    let get = |m: &HashMap<String, f64>, k: &str| m.get(k).copied();
    let mut out = Vec::new();
    // A kill's figures are on the line after its minute.
    let kills: Vec<String> = minutes
        .iter()
        .filter(|(_, m)| get(m, "kill").is_some_and(|x| x > 0.0))
        .map(|(k, m)| {
            format!(
                "minute {} ({}, {})",
                k - 1,
                match get(m, "kill_compacting") {
                    Some(1.0) => "a compact writing",
                    Some(_) => "no compact writing",
                    None => "compact unknown",
                },
                match get(m, "kill_sync_ms") {
                    Some(x) if x > 0.0 => format!("an fsync {x:.0} ms in flight"),
                    Some(0.0) => "no fsync in flight".into(),
                    _ => "the syncer not seen".into(),
                }
            )
        })
        .collect();
    if !kills.is_empty() {
        out.push(format!("the kills: {}", kills.join("; ")));
    }
    let Some(s) = settings else {
        return out;
    };
    let lost: f64 = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "lost_writes"))
        .sum::<f64>()
        + end.and_then(|m| get(m, "lost_writes")).unwrap_or(0.0);
    let lost_most = minutes
        .iter()
        .map(|x| x.1)
        .chain(end)
        .filter_map(|m| get(m, "lost_ms_max"))
        .fold(f64::MIN, f64::max);
    out.push(match lost > 0.0 {
        false => format!(
            "what a kill may take, --loss-ms {}: no write a read showed gone with a kill -- the run says nothing of the bound",
            s.loss_ms
        ),
        true if lost_most <= s.loss_ms as f64 => format!(
            "what a kill may take, --loss-ms {}: {lost:.0} writes a read showed gone with a kill, the earliest answered {lost_most:.0} ms before it -- the run supports the bound",
            s.loss_ms
        ),
        true => format!(
            "what a kill may take, --loss-ms {}: {lost:.0} writes a read showed gone with a kill, the earliest answered {lost_most:.0} ms before it, past the bound -- the run suggests --loss-ms {}, or a loss the server owes",
            s.loss_ms,
            (lost_most / 50.0).ceil() as u64 * 50
        ),
    });
    let restarts: Vec<f64> = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "restart_ms"))
        .filter(|x| *x > 0.0)
        .collect();
    if !restarts.is_empty() {
        let most = restarts.iter().copied().fold(0.0f64, f64::max) / 1e3;
        let part = |k: &str| -> f64 {
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, k))
                .fold(0.0f64, f64::max)
                / 1e3
        };
        let starts: Vec<f64> = minutes
            .iter()
            .filter_map(|(_, m)| get(m, "start_ms"))
            .collect();
        let twice = 2.0 * most;
        out.push(format!(
            "a restart, --restart-max {}: the longest {most:.1} s -- docker's kill and wait {:.1} s at most, the verify {:.1} s, the server's own part (docker start to answering) {:.1} s at the median and {:.1} s at most -- {}",
            s.restart_max,
            part("kill_ms"),
            part("verify_ms"),
            median(&starts) / 1e3,
            part("start_ms"),
            match twice <= s.restart_max as f64 {
                true => format!("the run supports the bound (twice the longest is {twice:.0} s)"),
                false => format!(
                    "the run suggests --restart-max {} (twice the longest)",
                    (twice / 10.0).ceil() as u64 * 10
                ),
            }
        ));
    }
    // The replica's own compactions, as its counter has them and as its
    // compaction thread's watch saw them: the reads through it waiting in
    // one, and the backlog of the workers they held (`class`), judged
    // apart (`judge_run`). A run before the watch has none of it.
    if minutes.iter().any(|(_, m)| get(m, "r_watch").is_some()) {
        let total = |k: &str| -> f64 {
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, k))
                .fold(0.0, |a, x| a + x)
        };
        let unwatched: Vec<u64> = minutes
            .iter()
            .filter(|(_, m)| get(m, "r_watch") == Some(0.0))
            .map(|(k, _)| *k)
            .collect();
        // The most a worker held for a window before its verdict, of the
        // most it may (`HELD_MAX`): the end line's, or the lines'.
        let held_max = end.and_then(|m| get(m, "r_held_max")).or_else(|| {
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, "r_held_max"))
                .reduce(f64::max)
        });
        out.push(format!(
            "the replica's own compactions: {:.0} by its counter, {:.0} seen at its compaction thread{}; {:.0} operations held by them, {:.0} of them due in a restart{}",
            total("r_auto_compacts"),
            total("r_compacts_watched"),
            match unwatched.is_empty() {
                true => String::new(),
                false => format!(" (not watched at minutes {})", list(&unwatched)),
            },
            total("r_compact_ops"),
            total("restart_r_compact_ops"),
            match held_max {
                Some(x) => format!(
                    "; a worker held {x:.0} operations for one before its verdict at the most, of the {HELD_MAX} it may ({:.1}%)",
                    x * 100.0 / HELD_MAX as f64
                ),
                None => String::new(),
            }
        ));
    }
    // Where the reads through the replica ran: a lane of their own, its
    // lag, or the pool every operation shares.
    out.push(match s.replica_threads {
        0 => "the reads through the replica: in the pool every operation shares (no lane of their own)".into(),
        t => format!(
            "the reads through the replica: a lane of {t} connections on a schedule of its own at {:.0}% of the rate{}",
            REPLICA_SHARE * 100.0,
            minutes
                .iter()
                .filter_map(|(_, m)| get(m, "r_behind_max_s"))
                .reduce(f64::max)
                .map_or(String::new(), |b| format!(", {b:.1} s behind it at the most"))
        ),
    });
    let offs: Vec<f64> = minutes
        .iter()
        .filter_map(|(_, m)| get(m, "clock_off_ms"))
        .collect();
    if !offs.is_empty() {
        let steps: Vec<u64> = minutes
            .iter()
            .filter(|(_, m)| get(m, "clock_step") == Some(1.0))
            .map(|(k, _)| *k)
            .collect();
        let (lo, hi) = offs
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
        out.push(format!(
            "the server's clock against this host's: {lo:.0} to {hi:.0} ms over the run; {}",
            match steps.is_empty() {
                true => format!("no step past {CLOCK_STEP_MS:.0} ms from one minute to the next"),
                false => format!(
                    "a step past {CLOCK_STEP_MS:.0} ms at minutes {} -- a session's verdict near one is the clock's",
                    list(&steps)
                ),
            }
        ));
    }
    // This host's other processes are every one but the run's and
    // Docker's. Docker's VM has no such line to draw: what its CPUs did
    // beside the two servers' cgroups is the run's own first -- Docker's
    // forwarding of the run's connections to the published ports, which
    // its `initd` does in the VM (2.6 times the server's own CPU, its
    // cgroup's, for the same `/_health` requests, measured beside one),
    // the kernel's vsock work, and the run's look, verify and
    // backup containers -- about a core all through the third dry run of
    // 2026-11-24, which named 77 of its minutes as others taking a core or
    // more. So a minute is said there only a core or more over the run's
    // own median.
    let busy = |k: &str, over: f64| -> Vec<u64> {
        minutes
            .iter()
            .filter(|(_, m)| get(m, k).is_some_and(|x| x >= over + 1.0))
            .map(|(k, _)| *k)
            .collect()
    };
    let vm_own = median(
        &minutes
            .iter()
            .filter_map(|(_, m)| get(m, "others_vm").filter(|x| *x >= 0.0))
            .collect::<Vec<_>>(),
    );
    let (host, vm) = (busy("others_host", 0.0), busy("others_vm", vm_own));
    out.push(format!(
        "what else used the machine: the load {} before the run; other processes took a core or more in {} minutes on this host{}; Docker's VM did {vm_own:.1} cores beside the two servers at the median -- its forwarding of the run's connections and the run's own containers among it -- and a core more in {} minutes{}",
        start
            .and_then(|m| get(m, "load_before"))
            .filter(|x| *x >= 0.0)
            .map_or("unknown".into(), |x| format!("{x:.1}")),
        host.len(),
        match host.is_empty() {
            true => String::new(),
            false => format!(" ({})", list(&host)),
        },
        vm.len(),
        match vm.is_empty() {
            true => String::new(),
            false => format!(" ({})", list(&vm)),
        }
    ));
    out
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
                "  {:<66} {:<4} {detail}",
                what,
                if *ok { "PASS" } else { "FAIL" }
            );
            if !ok {
                code = code.max(1);
            }
        }
        for n in &j.notes {
            println!("  {n}");
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
            replica: false,
        }
    }

    fn absent(key: u64, s: u64, e: u64) -> Ev {
        Ev::Row {
            key,
            s,
            e,
            found: false,
            got: [0; FIELDS],
            replica: false,
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
        // Ids outside the range told as deleted, beside a write of the
        // range's and alone -- the server's word that an id the stream
        // does not hold changed (`Database::changes_since`), every write
        // outside the range told so: no fault, no mismatch.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![
            write(10, 0, 42, 20, 30, Some(5)),
            write(50, 0, 7, 31, 32, Some(6)),
        ]);
        c.spending.push((0, seed(1, 4, 10)));
        let mut ch = change(1, 5, 40, vec![(10, with(10, 0, 42))]);
        ch.dels = vec![3, 90];
        c.spending.push((0, ch));
        let mut ch = change(1, 6, 41, vec![]);
        ch.dels = vec![50];
        c.spending.push((0, ch));
        c.hold(100, 0, 100, &[(1, 6)]);
        assert_eq!((c.faults, c.mismatches), (StreamFaults::default(), 0));
        // A row put outside the range is wrong.
        let mut c = Checker::new(100, kills(), 60, 5, &range);
        c.take(vec![write(50, 0, 7, 20, 30, Some(5))]);
        c.spending.push((0, seed(1, 4, 10)));
        c.spending
            .push((0, change(1, 5, 40, vec![(50, with(50, 0, 7))])));
        c.hold(100, 0, 100, &[(1, 5)]);
        assert_eq!(c.mismatches, 1);
    }

    #[test]
    fn a_change_number_a_kill_gave_out_again_is_known_by_its_life() {
        // The write to 10 was answered 5 ns before the kill (`--loss-ms`
        // 10): its change, 5, went out to the stream, read as the kill
        // landed, and the kill took it. The server started again gave 5 to
        // the write to 11, and the event from before the kill is judged
        // once both are logged.
        let range = [(10, 12)];
        let lost = || Kills {
            at: vec![(45, 60)],
            loss: 10,
        };
        let run = |old: u64, new: u64| {
            let mut c = Checker::new(100, lost(), 60, 5, &range);
            // After the restart 10 is written again, before 11.
            c.take(vec![
                write(10, 0, 42, 20, 40, Some(in_life(old, 5))),
                write(10, 0, 55, 62, 68, Some(in_life(new, 3))),
                write(11, 0, 77, 70, 80, Some(in_life(new, 5))),
            ]);
            c.spending.push((
                0,
                SEv {
                    conn: 1,
                    seed: true,
                    seq: in_life(old, 4),
                    opened: 9,
                    e: 10,
                    rows: vec![(10, base_digests(10)), (11, base_digests(11))],
                    ..SEv::default()
                },
            ));
            c.spending.push((
                0,
                change(1, in_life(old, 5), 50, vec![(10, with(10, 0, 42))]),
            ));
            c.spending.push((
                0,
                SEv {
                    conn: 2,
                    seed: true,
                    seq: in_life(new, 4),
                    opened: 64,
                    e: 65,
                    rows: vec![(10, base_digests(10)), (11, base_digests(11))],
                    ..SEv::default()
                },
            ));
            let mut ch = change(2, in_life(new, 5), 85, vec![(11, with(11, 0, 77))]);
            ch.opened = 64;
            c.spending.push((0, ch));
            c.hold(100, 0, 100, &[(64, in_life(new, 5))]);
            (c.faults, c.mismatches)
        };
        // Known by its life alone, each event holds its own write.
        assert_eq!(run(0, 1), (StreamFaults::default(), 0));
        // By its number, the event before the kill owed the write after
        // it, and was read back from that write's start: the short run's
        // missing writes and its read "holding no value written".
        let (faults, mismatches) = run(0, 0);
        assert_eq!(faults.missing, 1);
        assert!(mismatches > 0);
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
            loss_ms: 650,
            restart_max: 120,
            replica_back_max: 60,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            subscriptions: 64,
            threads: 32,
            replica_threads: 0,
            replication_buffer: 64,
            cpus: 8,
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
            loss_ms: 650,
            restart_max: 120,
            replica_back_max: 60,
            lag_max: 5,
            catchup_max: 300,
            recall_min: 0.925,
            subscriptions: 64,
            threads: 32,
            replica_threads: 0,
            replication_buffer: 64,
            cpus: 8,
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
        start.insert("load_before", 1.0);
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
                        x.insert("kill_ms", 400.0);
                        x.insert("verify_ms", 9_000.0);
                        x.insert("start_ms", 6_000.0);
                        x.insert("kill_sync_ms", 0.0);
                        x.insert("replica_back_ms", 3_000.0);
                        x.insert("kill_compacting", *in_compact as u8 as f64);
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
                ("steady_ops", 50_000.0),
                ("behind_s", 0.0),
                ("p99_hour", 2_000.0),
                ("p99_compact_hour", 5_000.0),
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
                ("others_host", 0.2),
                ("others_vm", 0.1),
                ("clock_off_ms", 3.0),
                ("clock_rtt_ms", 2.0),
                ("clock_step", 0.0),
                ("r_watch", 1.0),
                ("r_compact_bound_ms", 1_200_000.0),
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
            ("restart_ops", 5_000.0),
            ("restart_replica_ops", 0.0),
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
    fn the_peak_is_its_slowest_stretch() {
        // The third dry run of 2026-11-24's peak, minutes 2-30: 3 191 a
        // second at a 1 GB file, 2 749 at 2 GB, 1 955-2 117 while the
        // server compacted on its own (minutes 14-17), 2 892 after it.
        let rates = [
            3191, 3148, 3150, 3058, 3046, 2974, 2926, 2951, 2894, 2880, 2822, 2749, 2117, 2085,
            1994, 1955, 2410, 2892, 2777, 2620, 2501, 2413, 2445, 2425, 2358, 2255, 2312, 2298,
            2260,
        ];
        let peak: Vec<(u64, f64, f64)> = rates
            .iter()
            .enumerate()
            .map(|(i, r)| (i as u64 + 2, *r as f64 * 60.0, 60.0))
            .collect();
        let (at, r) = slowest_stretch(&peak, PEAK_STRETCH).unwrap();
        assert_eq!(at, 15);
        assert_eq!(r.round(), 2011.0);
        // Its 80%, under what the server answered through its compaction
        // -- the mean's, 2 093, was not.
        assert!(r * 0.8 < 1955.0);
        // A minute of its own: the slowest minute.
        assert_eq!(slowest_stretch(&peak, 1), Some((17, 1955.0)));
        // Fewer minutes than a stretch: none.
        assert_eq!(slowest_stretch(&peak[..2], PEAK_STRETCH), None);
        // A minute's seconds count: one cut short weighs less.
        let short = [(2, 3000.0, 60.0), (3, 100.0, 1.0), (4, 3000.0, 60.0)];
        assert_eq!(slowest_stretch(&short, 3).unwrap().1.round(), 50.0);
    }

    #[test]
    fn the_report_passes_a_good_run_and_fails_each_criterion_alone() {
        let none: &dyn Fn(u64, &mut HashMap<&'static str, f64>) = &|_, _| {};
        assert_eq!(failing(&synthetic(480, none)), Vec::<&str>::new());
        // Two hours, all else held: the eight hours fail, and the last
        // hour's tail is not judged -- no hour before it holds its schedule
        // -- what the dry run's report must show.
        assert_eq!(
            failing(&synthetic(120, none)),
            vec![CRITERIA[0], CRITERIA[3]]
        );
        // The hour the last is held to: lines 61-120 of 480, the same kills
        // (a plain one, one in a compact) and compacts at the same minutes.
        let s = Settings::read(
            &synthetic(480, none)
                .lines()
                .filter(|l| l.contains(":start\t"))
                .map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    (f[6].to_string(), f[7].parse().unwrap())
                })
                .collect(),
        )
        .unwrap();
        assert_eq!(same_hour(480, &s), Some(61));
        assert_eq!(same_hour(120, &s), None);
        type Tweak = Box<dyn Fn(u64, &mut HashMap<&'static str, f64>)>;
        let cases: Vec<(&str, Tweak, Vec<&str>)> = vec![
            (
                "a planned run of ten hours that ran eight",
                Box::new(|m, x| {
                    if m == 0 {
                        x.insert("minutes", 600.0);
                    }
                }),
                vec![CRITERIA[1]],
            ),
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
                // The loop behind its schedule all the last hour: no steady
                // operation, no steady p99 -- written as 0, it passed.
                "no operation on its schedule in the last hour",
                Box::new(|m, x| {
                    if (421..=480).contains(&m) {
                        x.remove("p99_hour");
                        x.insert("steady_ops", 0.0);
                    }
                }),
                vec![CRITERIA[3]],
            ),
            (
                "the compacts' tail drifting",
                Box::new(|m, x| {
                    if m > 420 && m < u64::MAX {
                        x.insert("p99_compact_hour", 7_000.0);
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
                "the same threads at two looks",
                Box::new(|m, x| {
                    if m == 200 || m == 201 {
                        x.insert("threads", 147.0);
                    }
                }),
                vec![CRITERIA[7]],
            ),
            (
                "more threads than a task in shares makes",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("threads", 150.0);
                    }
                }),
                vec![CRITERIA[7]],
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
                "a thread more every 8 minutes of each life, given back at each restart",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("threads", 140.0 + ((m - 1) % 30 / 8) as f64);
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
                "the replica slow to come back after a restart",
                Box::new(|m, x| {
                    if m == 61 {
                        x.insert("replica_back_ms", 90_000.0);
                    }
                }),
                vec![CRITERIA[12]],
            ),
            (
                "the replica short of the loop's writes after a restart past its bound",
                Box::new(|m, x| {
                    if m == 62 {
                        x.insert("catchup_replica_ms", -1.0);
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
                "the operations due in a restart held past it",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("p99_restart", 30e6);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                // Each restart 20 s, the replica back 3 s after it, within
                // the 5 s grace: 25 s from a kill to the reads through it
                // again, and two -- 27 s.
                "the reads through the replica due in a restart held past its return",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("p99_restart_replica", 30e6);
                        x.insert("restart_replica_ops", 40.0);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                // Every kill met with the loop behind: no restart's own
                // operations to judge, only the backlog's.
                "the loop behind its schedule at every kill",
                Box::new(|m, x| {
                    if m > 1 && m < u64::MAX && (m - 1) % 30 == 0 {
                        x.insert("kill_behind_s", 90.0);
                    }
                    if m == u64::MAX {
                        x.remove("p99_restart");
                        x.insert("restart_ops", 0.0);
                        x.insert("p99_restart_behind", 110e6);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                // Asked more than the server answers: 40 minutes behind,
                // the kill of minute 60 among them -- its restart judged by
                // the loop's lag as it was announced (`kill_behind_s`).
                "the loop behind its schedule for 40 minutes",
                Box::new(|m, x| {
                    if (31..=70).contains(&m) {
                        x.insert("behind_s", 100.0);
                    }
                }),
                vec![CRITERIA[19]],
            ),
            (
                // The pool on its schedule, the replica lane's 4% never
                // kept up with: its own lag fails it.
                "the replica lane behind its schedule from minute 31 on",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("r_behind_s", if m > 30 { 40.0 } else { 0.0 });
                    }
                }),
                vec![CRITERIA[19]],
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
                "a kill that cut no compact",
                Box::new(|m, x| {
                    if m == 91 {
                        x.remove("compact_killed");
                    }
                }),
                vec![CRITERIA[14]],
            ),
            (
                "a kill in a compact that found none writing, its request cut all the same",
                Box::new(|m, x| {
                    if m == 91 {
                        x.insert("kill_compacting", 0.0);
                    }
                }),
                vec![CRITERIA[14]],
            ),
            (
                "a stale backup",
                Box::new(|m, x| {
                    if m == 51 {
                        x.remove("backup");
                        x.remove("restore");
                        x.insert("backup_failed", 1.0);
                        x.insert("backup_stale", 1.0);
                    }
                }),
                vec![CRITERIA[14], CRITERIA[15]],
            ),
            (
                "a backup whose restored records are not as written",
                Box::new(|m, x| {
                    if m == 53 {
                        x.insert("backup_differs", 1.0);
                    }
                }),
                vec![CRITERIA[14], CRITERIA[15]],
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
                "a restart's sweep that could not read",
                Box::new(|m, x| {
                    if m == 32 {
                        x.insert("sweep_owed", 100.0);
                        x.insert("sweep_failed", 1.0);
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
            (
                // Every operation of the kills met on schedule a replica's
                // compaction's: none of the restart's to judge. Written as
                // a p99 of 0, it passed.
                "no operation due in a restart judged",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.remove("p99_restart");
                        x.insert("restart_ops", 0.0);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                "a p99 of 0 written for no operation due in a restart",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.insert("p99_restart", 0.0);
                        x.insert("restart_ops", 0.0);
                    }
                }),
                vec![CRITERIA[13]],
            ),
            (
                // The replica's compaction of 21.5 s, its reads held 25 s.
                "reads held past the replica's compaction",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_compact_ms", 21_500.0);
                        x.insert("r_auto_compacts", 1.0);
                        x.insert("r_compacts_watched", 1.0);
                    }
                    if m == u64::MAX {
                        x.insert("r_compact_ops", 4_000.0);
                        x.insert("p99_r_compact", 25e6);
                    }
                }),
                vec![CRITERIA[20]],
            ),
            (
                "reads held by a replica's compaction no watch saw end",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_compacts_cut", 1.0);
                    }
                    if m == u64::MAX {
                        x.insert("r_compact_ops", 4_000.0);
                        x.insert("p99_r_compact", 2e6);
                    }
                }),
                vec![CRITERIA[20]],
            ),
            (
                "a replica's compaction its counter saw and the watch did not",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_auto_compacts", 1.0);
                    }
                }),
                vec![CRITERIA[21]],
            ),
            (
                // Its container gone in the compaction, the next finding
                // the thread asleep: its end unseen.
                "a replica's compaction cut by the watch's own going",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_auto_compacts", 1.0);
                        x.insert("r_compacts_cut", 1.0);
                    }
                }),
                vec![CRITERIA[21]],
            ),
            (
                "a compaction the watch saw end and the counter did not",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_compacts_watched", 1.0);
                    }
                }),
                vec![CRITERIA[21]],
            ),
            (
                // No thread so named in the replica: never watched.
                "the replica's compactions not watched",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        x.insert("r_watch", 0.0);
                    }
                }),
                vec![CRITERIA[21]],
            ),
            (
                // The watch's container gone in a compaction and never
                // back: the window open to the run's end, its length the
                // reads' bound and the counter's count its excuse -- both
                // passed it.
                "a replica's compaction open at the last look, its watch gone",
                Box::new(|m, x| {
                    if m == 480 {
                        x.insert("r_watch", 0.0);
                        x.insert("r_auto_compacts", 1.0);
                        x.insert("r_compact_open", 1.0);
                        x.insert("r_compact_open_ms", 12_000.0);
                    }
                    if m == u64::MAX {
                        x.insert("r_compact_ops", 300.0);
                        x.insert("p99_r_compact", 12e6);
                    }
                }),
                vec![CRITERIA[20], CRITERIA[21]],
            ),
            (
                // A thread that never slept again, found and cut by a look.
                "a replica's compaction window open past its bound",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("r_compacts_overlong", 1.0);
                    }
                }),
                vec![CRITERIA[22]],
            ),
            (
                "a replica's compaction open at the last look past its bound",
                Box::new(|m, x| {
                    if m == 480 {
                        x.insert("r_compact_open", 1.0);
                        x.insert("r_compact_open_ms", 1_300_000.0);
                    }
                }),
                vec![CRITERIA[22]],
            ),
        ];
        for (name, tweak, want) in cases {
            assert_eq!(failing(&synthetic(480, &*tweak)), want, "{name}");
        }
        // What a right server does and the report must pass.
        let passes: Vec<(&str, Tweak)> = vec![
            (
                // A task in shares on the VM's 8 CPUs at one look: 7 more,
                // none at the next.
                "a task's threads at one look",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("threads", 147.0);
                    }
                }),
            ),
            (
                // The heap a compaction let go of, kept: a step at the
                // compaction of minute 100, flat on both sides of it.
                "a step at a compaction, flat between",
                Box::new(|m, x| {
                    if (101..=120).contains(&m) {
                        x.insert("rss_anon", 115e6);
                    }
                }),
            ),
            (
                "the loop back on its schedule within each kill interval",
                Box::new(|m, x| {
                    if (31..=55).contains(&m) || (61..=85).contains(&m) {
                        x.insert("behind_s", 100.0);
                    }
                }),
            ),
            (
                // The open loop's backlog after each restart in the hour's
                // tail: two kills an hour cost more than one, and the last
                // hour has two, the first one.
                "a tail that follows the schedule's kills",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX {
                        let kills = (m.saturating_sub(60)..m)
                            .filter(|k| *k > 0 && k % 30 == 0)
                            .count();
                        x.insert("p99_hour", 2_000.0 + 1_000.0 * kills as f64);
                    }
                }),
            ),
            (
                "a compact turned away while the server's own wrote the file",
                Box::new(|m, x| {
                    if m == 41 {
                        x.remove("compact");
                        x.insert("compact_busy", 1.0);
                    }
                }),
            ),
            (
                "a look at the primary in a compact, and one look apart",
                Box::new(|m, x| {
                    if m == 60 {
                        x.insert("rss_anon", 108e6);
                        x.insert("compacting", 1.0);
                    }
                    if m == 90 {
                        x.insert("rss_anon", 108e6);
                    }
                }),
            ),
            (
                "a look at the replica in its own compact",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("r_rss_anon", 98e6);
                        x.insert("r_compacting", 1.0);
                    }
                }),
            ),
            (
                // The allocator keeping the heap a compaction let go of,
                // four looks after each of the replica's own: the third
                // dry run of 2026-11-24's, read across one, failed.
                "the replica's heap a step over its engine's after each of its compactions",
                Box::new(|m, x| {
                    if m > 0 && m < u64::MAX && m % 40 == 0 {
                        x.insert("r_compacting", 1.0);
                    }
                    if m > 0 && m < u64::MAX && (1..=4).contains(&(m % 40)) {
                        x.insert("r_rss_anon", 99e6);
                    }
                }),
            ),
            (
                // Behind its schedule at the kill of minute 60 alone: the
                // backlog's wait said apart, the other kills judged.
                "a kill met behind its schedule, its operations the backlog's",
                Box::new(|m, x| {
                    if m == 61 {
                        x.insert("kill_behind_s", 120.0);
                    }
                    if m == u64::MAX {
                        x.insert("p99_restart_behind", 140e6);
                    }
                }),
            ),
            (
                "writes a kill took within the bound",
                Box::new(|m, x| {
                    if m == 31 {
                        x.insert("lost_writes", 3.0);
                        x.insert("lost_ms_max", 412.0);
                    }
                }),
            ),
            (
                // A line from before the reads through the replica were
                // apart: held to the primary's bound together.
                "a run's lines with no figure for the replica's part",
                Box::new(|m, x| {
                    if m == u64::MAX {
                        x.remove("p99_restart_replica");
                        x.remove("restart_replica_ops");
                    }
                }),
            ),
            (
                // The replica's compaction of 21.5 s, its reads held 22 s.
                "reads held by the replica's compaction within it",
                Box::new(|m, x| {
                    if m == 118 {
                        x.insert("r_compact_ms", 21_500.0);
                        x.insert("r_auto_compacts", 1.0);
                        x.insert("r_compacts_watched", 1.0);
                    }
                    if m == u64::MAX {
                        x.insert("r_compact_ops", 4_000.0);
                        x.insert("p99_r_compact", 22e6);
                    }
                }),
            ),
            (
                // Counted, its end not yet seen at the run's last look: held
                // to how long it had run.
                "a replica's compaction open at the run's last look",
                Box::new(|m, x| {
                    if m == 480 {
                        x.insert("r_auto_compacts", 1.0);
                        x.insert("r_compact_open", 1.0);
                        x.insert("r_compact_open_ms", 12_000.0);
                    }
                    if m == u64::MAX {
                        x.insert("r_compact_ops", 300.0);
                        x.insert("p99_r_compact", 12e6);
                    }
                }),
            ),
            (
                // The watch's look waiting on the replica's lock: a window
                // its counter did not move in, no compaction.
                "a window the replica's counter did not move in",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("r_compacts_false", 1.0);
                    }
                }),
            ),
            (
                // Gone a minute: nothing counted then.
                "the watch gone a minute the replica did not compact in",
                Box::new(|m, x| {
                    if m == 200 {
                        x.insert("r_watch", 0.0);
                    }
                }),
            ),
        ];
        for (name, tweak) in passes {
            assert_eq!(
                failing(&synthetic(480, &*tweak)),
                Vec::<&str>::new(),
                "{name}"
            );
        }
        // Docker's VM a core busy beside the servers every minute -- its
        // forwarding of the run's own connections, never another's -- and
        // a minute a core over that: the minute alone said.
        let vm = synthetic(480, &|m, x| {
            if m > 0 && m < u64::MAX {
                x.insert("others_vm", if m == 200 { 2.3 } else { 1.1 });
            }
        });
        let j = judge(&vm, None);
        let said = j[0]
            .notes
            .iter()
            .find(|n| n.starts_with("what else used the machine"))
            .unwrap();
        assert!(
            said.contains("Docker's VM did 1.1 cores beside the two servers at the median")
                && said.contains("a core more in 1 minutes (200)"),
            "{said}"
        );
        // The rule the first round had: the first hour against the last
        // fails the right server whose tail follows its schedule.
        let text = synthetic(480, &|m, x| {
            if m > 0 && m < u64::MAX {
                let kills = (m.saturating_sub(60)..m)
                    .filter(|k| *k > 0 && k % 30 == 0)
                    .count();
                x.insert("p99_hour", 2_000.0 + 1_000.0 * kills as f64);
            }
        });
        let first = text
            .lines()
            .find(|l| l.contains(":m0060\tp99_hour\t"))
            .and_then(|l| l.rsplit('\t').next()?.parse::<f64>().ok())
            .unwrap();
        assert_eq!(first, 3_000.0);
        // A soak its script's guard stopped as silent: the dead-man.
        let hung = format!(
            "{}t1\tc\tmac\t2026-11-15\tsoak\t250:50:hung\thung\t1\n",
            synthetic(480, none)
                .lines()
                .filter(|l| l.split('\t').nth(5).is_some_and(|c| c < "250:50:m0101"))
                .map(|l| format!("{l}\n"))
                .collect::<String>()
        );
        let f = failing(&hung);
        assert!(f.contains(&CRITERIA[2]), "{f:?}");
        let j = judge(&hung, None);
        assert!(j[0].verdicts[2]
            .2
            .contains("guard stopped it after minute 100"));
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
        // The digest's pages, in id order whatever a compact did.
        let r = run(
            &mut db,
            "get usertable where id >= $1 and id < $2 order id",
            &[Value::Int(1), Value::Int(10)],
        );
        assert_eq!(ids(&r), vec![5]);
        // The clock's probe answers the server's `now()` in ms.
        let r = run(&mut db, CLOCK_PROBE, &[]);
        let rows = r.rows().unwrap();
        let t = rows.columns.iter().position(|c| c == "t").unwrap();
        match rows.rows[0].values[t] {
            Value::Int(ms) => assert!(ms > 1_700_000_000_000, "{ms}"),
            ref other => panic!("the clock's probe answered {other:?}"),
        }
    }

    /// A server answering each request with `answer(path, body)`, a
    /// connection at a time, until the test binary ends.
    fn fake(answer: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> String {
        use std::io::Read;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { break };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut w = c;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let mut len = 0;
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 {
                            break;
                        }
                        let h = h.trim_end().to_ascii_lowercase();
                        if h.is_empty() {
                            break;
                        }
                        if let Some(v) = h.strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; len];
                    if r.read_exact(&mut body).is_err() {
                        break;
                    }
                    let (status, out) = answer(&path, &String::from_utf8_lossy(&body));
                    let _ = write!(
                        w,
                        "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n\r\n{out}",
                        out.len()
                    );
                }
            }
        });
        addr
    }

    fn opts() -> Opts {
        Opts::parse(&["--sync".into(), "250".into(), "--rate".into(), "100".into()])
    }

    #[test]
    fn a_doc_write_is_left_out_of_the_sample_from_before_it_is_sent() {
        let seen = Arc::new(AtomicBool::new(false));
        let shared: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
        let (cell, flag) = (Arc::clone(&shared), Arc::clone(&seen));
        let addr = fake(move |_, body| {
            if body.contains("set docs") {
                let sh = cell.lock().unwrap().clone().unwrap();
                // The server has the write: the sample must leave it out.
                flag.store(written_docs(&sh, 0).contains(&7), Ordering::SeqCst);
            }
            (200, "{\"affected\":1}".into())
        });
        let sh = Arc::new(Shared::new(&opts(), addr.clone(), addr, &[]));
        *shared.lock().unwrap() = Some(Arc::clone(&sh));
        let mut w = Worker {
            sh: &sh,
            slot: &sh.slots[0],
            c: None,
            life: None,
            rc: None,
            rng: Rng::new(1),
            via_replica: false,
        };
        assert!(w.doc_write(7, "7,\"w1\",[0.5]"));
        assert!(
            seen.load(Ordering::SeqCst),
            "the doc was not in the sample's leave-out while its write was at the server"
        );
        assert!(sh
            .doc_writes
            .lock()
            .unwrap()
            .iter()
            .all(|x| x.2 != u64::MAX));
    }

    #[test]
    fn a_read_through_the_replica_is_sent_after_every_write_answered_before_it_began() {
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        sh.max_seq.store(41, Ordering::SeqCst);
        let mut write = (0, 0);
        let (s, after) = sh.sent_after(|| {
            let q = sh.max_seq.load(Ordering::SeqCst);
            // A write answered just as the change is read: its change
            // noted, then its end taken.
            sh.max_seq.fetch_max(42, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1));
            write = (42, sh.ns());
            q
        });
        assert!(
            write.1 >= s || after >= write.0,
            "a write answered before the read began ({} < {s}) is past the change it was sent after ({after})",
            write.1
        );
    }

    fn kit(replica_port: u16) -> Kit {
        Kit {
            name: "t".into(),
            image: String::new(),
            tools: String::new(),
            memory: String::new(),
            sync: "250".into(),
            token: "x".into(),
            http_token: "y".into(),
            replication_buffer: 64,
            port: 1,
            replica_port,
            keep: Arc::new(AtomicBool::new(false)),
            kept: None,
            logs: None,
        }
    }

    /// The servers ask a token, and every request carries it: the head a
    /// request writes, the bearer set once, an `Authorization` of its own
    /// sent in its place (`Kit::status`'s, the replication token's).
    #[test]
    fn every_request_carries_the_runs_token_and_only_one() {
        use std::io::Read;
        http::bearer("t0k");
        assert_eq!(http::auth(), "Authorization: Bearer t0k\r\n");
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let heads = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for _ in 0..2 {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap();
                heads.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
            heads
        });
        let mut c = http::Http::try_connect(&addr, Duration::from_secs(10)).unwrap();
        c.try_query("get usertable", "").unwrap();
        let mut c = http::Http::try_connect(&addr, Duration::from_secs(10)).unwrap();
        c.try_request_with(
            "GET",
            "/_replication/status",
            "text/plain",
            "Authorization: Bearer r3p\r\n",
            b"",
        )
        .unwrap();
        let heads = heads.join().unwrap();
        assert_eq!(
            heads[0].matches("Authorization:").count(),
            1,
            "{}",
            heads[0]
        );
        assert!(
            heads[0].contains("Authorization: Bearer t0k\r\n"),
            "{}",
            heads[0]
        );
        assert_eq!(
            heads[1].matches("Authorization:").count(),
            1,
            "{}",
            heads[1]
        );
        assert!(
            heads[1].contains("Authorization: Bearer r3p\r\n"),
            "{}",
            heads[1]
        );
    }

    /// A refusal for the run's token or its name -- the primary's 403 to a
    /// replica or a backup reaching it by its container's name, the dry
    /// run of 2026-11-22's -- is the harness's own fault, read from its
    /// words; anything a restart explains is not. A worker answered one
    /// says it, and the run stops at the minute's end.
    #[test]
    fn a_refusal_of_the_runs_token_or_name_is_the_harness_s_fault() {
        let backup = "fennec backup: the primary answered 403: {\"error\":\"Host `p6-server:8080` is not this machine's, and this server asks for no token: reach it as localhost or by its address, or give it --http-token\"}\n";
        let status = "{\"role\":\"replica\",\"seq\":0,\"connected\":false,\"error\":\"the primary answered 403: {\\\"error\\\":\\\"Host `p6-server:8080` is not this machine's, and this server asks for no token: reach it as localhost or by its address, or give it --http-token\\\"}\"}";
        for text in [backup, status] {
            let why = refused(text).unwrap_or_else(|| panic!("not read: {text}"));
            assert!(
                why.starts_with("answered 403: {\"error\":\"Host `p6-server:8080` is not"),
                "{why}"
            );
            assert!(why.ends_with("give it --http-token"), "{why}");
        }
        assert!(refused("fennec backup: the primary answered 401: wrong token").is_some());
        for text in [
            "the primary answered 503: {\"error\":\"starting\"}",
            "{\"role\":\"replica\",\"connected\":false,\"error\":\"Connection refused (os error 111)\"}",
            "",
        ] {
            assert_eq!(refused(text), None, "{text}");
        }
        for (status, fault) in [(403, true), (401, true), (500, false), (404, false)] {
            let addr = fake(move |_, _| (status, "{\"error\":\"no\"}".into()));
            let sh = Shared::new(&opts(), addr.clone(), addr, &[]);
            let mut w = Worker {
                sh: &sh,
                slot: &sh.slots[0],
                c: None,
                life: None,
                rc: None,
                rng: Rng::new(1),
                via_replica: false,
            };
            assert!(!w.read(5, false));
            assert_eq!(sh.faulted().is_some(), fault, "{status}");
            if fault {
                let why = sh.faulted().unwrap();
                assert!(
                    why.contains(&format!("refused get usertable where id = $1: {status} ")),
                    "{why}"
                );
            }
        }
    }

    #[test]
    fn the_loop_is_behind_by_its_median_connections_lag() {
        let slot = |due: u64| Slot {
            hists: Mutex::new(Hists::new()),
            log: Mutex::new(Vec::new()),
            inflight: AtomicU64::new(u64::MAX),
            done: AtomicU64::new(0),
            due: AtomicU64::new(due),
        };
        let s = 1_000_000_000;
        // At the peak no connection keeps a schedule.
        assert_eq!(behind_s(&[slot(u64::MAX), slot(u64::MAX)], 10 * s), None);
        // Ahead of it -- waiting for an operation due later -- is not
        // behind; one stuck connection does not move the median.
        let slots = [slot(11 * s), slot(10 * s), slot(9 * s), slot(2 * s)];
        assert_eq!(behind_s(&slots, 10 * s), Some(1.0));
        let slots = [slot(s), slot(2 * s), slot(3 * s)];
        assert_eq!(behind_s(&slots, 10 * s), Some(8.0));
    }

    #[test]
    fn the_replica_is_read_again_once_connected_and_past_the_newest_change() {
        let status = Arc::new(Mutex::new(String::new()));
        let s2 = Arc::clone(&status);
        let addr = fake(move |path, _| match path {
            "/_replication/status" => (200, s2.lock().unwrap().clone()),
            _ => (404, String::new()),
        });
        let k = kit(addr.rsplit(':').next().unwrap().parse().unwrap());
        let say = |seq: u64, connected: bool| {
            *status.lock().unwrap() = format!(
                "{{\"role\":\"replica\",\"seq\":{seq},\"history\":\"0\",\"primary\":\"x\",\"connected\":{connected},\"primary_durable\":{seq},\"behind\":0}}"
            );
        };
        say(41, true);
        assert!(!replica_ready(&k, 42), "short of the newest change");
        say(42, false);
        assert!(!replica_ready(&k, 42), "not connected");
        say(42, true);
        assert!(replica_ready(&k, 42));
    }

    #[test]
    fn a_restart_s_catch_up_ends_with_the_loop_on_its_schedule_and_the_replica_holding_the_change_then(
    ) {
        let s = 1_000_000_000u64;
        let bound = 60 * s;
        let mut x = Catch::new(3, 100 * s);
        x.back = 104 * s;
        // Within the grace the reads go to the primary: the loop on its
        // schedule names no change yet.
        x.on_schedule(105 * s, Some(0.5), false, 900);
        assert_eq!(x.on, None);
        // Through the replica again, the loop 4 s behind with the
        // restart's backlog: not yet; and nothing to hold before it.
        x.on_schedule(110 * s, Some(4.0), true, 1_000);
        assert_eq!(x.on, None);
        assert!(!x.settle(110 * s, Some(2_000), bound));
        // Back on its schedule: the newest change answered then is the
        // replica's to hold, a later look never moving it.
        x.on_schedule(118 * s, Some(0.8), true, 8_000);
        assert_eq!(x.on, Some((118 * s, 8_000)));
        x.on_schedule(119 * s, Some(0.1), true, 9_000);
        assert_eq!(x.on, Some((118 * s, 8_000)));
        // Short of it, then not read, then holding it: over.
        assert!(!x.settle(119 * s, Some(7_999), bound));
        assert!(!x.settle(120 * s, None, bound));
        assert!(x.settle(121 * s, Some(8_000), bound));
        assert_eq!((x.end, x.to), (CatchEnd::Caught, 121 * s));
        assert!(!x.settle(122 * s, Some(8_000), bound));
        x.down = 185;
        x.moved_milli = 1_203_400;
        assert_eq!(
            catch_said(&x, bound),
            "minute 3: the restart's catch-up over: the loop back on its schedule 14.0 s after the server answered again, the replica holding change 8000, the newest answered then, 3.0 s after that; 185 reads through the replica a 504 answered in it, the restart's, counted as down; 1203 of the lane's due in it moved up, never sent"
        );
        // Short of it `--replica-back-max` after the loop's return: over,
        // a 504 from there an error. At the peak the loop keeps no
        // schedule: on it as soon as reads go through the replica.
        let mut y = Catch::new(4, 200 * s);
        y.back = 203 * s;
        y.on_schedule(210 * s, None, true, 5);
        assert!(!y.settle(269 * s, Some(4), bound));
        assert!(y.settle(270 * s, Some(4), bound));
        assert_eq!((y.end, y.to), (CatchEnd::Bound, 270 * s));
        assert!(catch_said(&y, bound).starts_with(
            "minute 4: the restart's catch-up NOT over: the loop back on its schedule 7.0 s after the server answered again, the replica short of change 5, the newest answered then, 60 s after that (--replica-back-max): a 504 from there an error; 0 reads"
        ));
        // A loop never back on its schedule leaves it open -- the open
        // loop's own verdict fails that -- to the next restart.
        let mut z = Catch::new(5, 300 * s);
        z.back = 304 * s;
        z.on_schedule(400 * s, Some(30.0), true, 5);
        assert!(!z.settle(1_000 * s, None, bound));
        z.to = 1_500 * s;
        z.end = CatchEnd::Cut;
        assert!(catch_said(&z, bound).starts_with(
            "minute 5: the restart's catch-up cut by the next restart: the loop not back on its schedule, with reads through the replica again, in the 1196.0 s after the server answered again; "
        ));
    }

    #[test]
    fn a_replica_s_504_in_a_restart_s_catch_up_is_the_restart_s_and_one_after_it_an_error() {
        // The replica holding every read past its wait; one key's answer
        // coming 300 ms after it was sent.
        let addr = fake(|_, body| {
            if body.contains("77") {
                std::thread::sleep(Duration::from_millis(300));
            }
            (
                504,
                "{\"error\":\"change 9 has not reached this server in the wait\"}".into(),
            )
        });
        let sh = Shared::new(&opts(), addr.clone(), addr, &[]);
        let mut w = Worker {
            sh: &sh,
            slot: &sh.slots[0],
            c: None,
            life: None,
            rc: None,
            rng: Rng::new(1),
            via_replica: false,
        };
        let counts = || {
            (
                sh.errors.load(Ordering::SeqCst),
                sh.down.load(Ordering::SeqCst),
            )
        };
        // No restart behind it: an error.
        assert!(!w.read(5, true));
        assert_eq!(counts(), (1, 0));
        // A kill's restart, the server back: its catch-up open from the
        // window's start, the replica behind the backlog's writes -- the
        // restart's, `down`, counted to it.
        let from = sh.ns();
        sh.catch_open(3, from);
        sh.catch_back(from + 1);
        assert!(!w.read(5, true));
        assert!(!w.read(6, true));
        assert_eq!(counts(), (1, 2));
        // A read sent in it whose 504 comes after it ended, by the wait:
        // the restart's still, by its send.
        std::thread::scope(|scope| {
            let late = scope.spawn(|| w.read(77, true));
            std::thread::sleep(Duration::from_millis(100));
            let mut c = sh.catches.lock().unwrap();
            let x = c.last_mut().unwrap();
            x.on_schedule(sh.ns(), Some(0.2), true, 40);
            assert!(x.settle(sh.ns(), Some(40), 60_000_000_000));
            drop(c);
            assert!(!late.join().unwrap());
        });
        assert_eq!(counts(), (1, 3));
        assert_eq!(sh.catches.lock().unwrap()[0].down, 3);
        // After it, the replica caught up: an error.
        assert!(!w.read(7, true));
        assert_eq!(counts(), (2, 3));
        // A 504 of the primary's is never a catch-up's. (The fake serves a
        // connection at a time: the replica's let go.)
        let from = sh.ns();
        sh.catch_open(4, from);
        w.rc = None;
        assert!(!w.read(8, false));
        assert_eq!(counts(), (3, 3));
        assert_eq!(sh.catches.lock().unwrap()[1].down, 0);
    }

    #[test]
    fn a_restart_s_catch_up_is_looked_at_once_reads_go_through_the_replica_and_said_once_over() {
        let mut sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        // A run 100 s in: a restart's grace can be over.
        sh.t0 = sh.t0.checked_sub(Duration::from_secs(100)).unwrap();
        let sh = sh;
        let doings = Doings::default();
        let s = 1_000_000_000u64;
        let bound = 60 * s;
        let figures = || std::mem::take(&mut *doings.figures.lock().unwrap());
        let reads = std::cell::Cell::new(0);
        let look = |wait: u64, stop: bool, held: u64| {
            catch_look(&sh, &doings, bound, wait, stop, &mut || {
                reads.set(reads.get() + 1);
                Some(held)
            })
        };
        // The kill's window open: its catch-up from the window's start, the
        // lane moved up.
        let from = sh.ns();
        sh.windows.lock().unwrap().push((from, from, u64::MAX));
        sh.catch_open(3, from);
        assert!(sh.catching.load(Ordering::SeqCst));
        // Down, then back within the grace, the loop on its schedule (no
        // connection behind it): the replica not asked.
        assert!(look(0, false, 0).is_empty());
        let back = sh.ns();
        sh.windows.lock().unwrap()[0].2 = back;
        sh.catch_back(back);
        assert!(look(0, false, 0).is_empty());
        // Past the grace, the pool 3 s behind with its backlog: not yet.
        sh.windows.lock().unwrap()[0].2 = back - GRACE - s;
        let lagging = sh.ns().saturating_sub(3 * s);
        for slot in sh.pool_slots() {
            slot.due.store(lagging, Ordering::SeqCst);
        }
        assert!(look(0, false, 0).is_empty());
        // On it, but the replica kept from reads since the kill: not yet.
        for slot in sh.pool_slots() {
            slot.due.store(sh.ns(), Ordering::SeqCst);
        }
        sh.replica_ok.store(false, Ordering::SeqCst);
        assert!(look(0, false, 0).is_empty());
        assert_eq!(reads.get(), 0);
        // Reads through it again: the newest change answered then named,
        // the replica short of it, then holding it -- said only once the
        // reads sent in it had their wait.
        sh.replica_ok.store(true, Ordering::SeqCst);
        sh.max_seq.store(8_000, Ordering::SeqCst);
        assert!(look(0, false, 7_000).is_empty());
        sh.max_seq.store(9_000, Ordering::SeqCst);
        assert!(look(3_600 * s, false, 8_000).is_empty());
        assert!(!sh.catching.load(Ordering::SeqCst));
        assert!(look(3_600 * s, false, 8_000).is_empty());
        assert_eq!(reads.get(), 2);
        let x = sh.catches.lock().unwrap()[0];
        assert_eq!((x.end, x.on.map(|o| o.1)), (CatchEnd::Caught, Some(8_000)));
        let f = figures();
        assert!(
            f.iter()
                .any(|(k, v)| *k == "catchup_replica_ms" && v != "-1"),
            "{f:?}"
        );
        assert!(f.iter().any(|(k, _)| *k == "catchup_on_ms"), "{f:?}");
        assert_eq!(look(0, false, 0).len(), 1);
        assert!(look(0, false, 0).is_empty());
        assert!(figures().iter().any(|(k, _)| *k == "catchup_down"));
        // The next restart's, the replica short of the change past the
        // bound -- a nanosecond here: -1; and one open at the run's end
        // cut by it, both said at once.
        sh.catch_open(4, sh.ns());
        sh.catches.lock().unwrap()[1].on = Some((sh.ns(), 10_000));
        std::thread::sleep(Duration::from_millis(1));
        assert!(catch_look(&sh, &doings, 1, 3_600 * s, false, &mut || Some(9_999)).is_empty());
        assert_eq!(sh.catches.lock().unwrap()[1].end, CatchEnd::Bound);
        let f = figures();
        assert!(
            f.iter()
                .any(|(k, v)| *k == "catchup_replica_ms" && v == "-1"),
            "{f:?}"
        );
        sh.catch_open(5, sh.ns());
        let said = look(3_600 * s, true, 0);
        assert_eq!(
            said.iter().map(|x| (x.minute, x.end)).collect::<Vec<_>>(),
            vec![(4, CatchEnd::Bound), (5, CatchEnd::RunEnd)]
        );
        assert!(!sh.catching.load(Ordering::SeqCst));
    }

    #[test]
    fn the_lane_is_moved_up_from_a_restart_until_its_catch_up_ends() {
        // 100 a second, the lane's 4: a read every 250 ms.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let s = 1_000_000_000u64;
        let step = s / 4;
        assert_eq!(sh.r_every.load(Ordering::SeqCst), step);
        // Off the peak and no restart: the lane's own backlog, sent.
        sh.r_due.store(10 * s, Ordering::SeqCst);
        assert_eq!(lane_due(&sh, 100 * s), 10 * s);
        assert_eq!(sh.lane_dropped_milli.load(Ordering::SeqCst), 0);
        // From a restart: a second behind at the most, the reads skipped
        // dropped, and said as the catch-up's.
        sh.catch_open(3, 50 * s);
        assert_eq!(lane_due(&sh, 100 * s), 99 * s);
        let skipped = (99 * s - (10 * s + step)) * 1000 / step;
        assert_eq!(sh.lane_dropped_milli.load(Ordering::SeqCst), skipped);
        assert_eq!(sh.catches.lock().unwrap()[0].moved_milli, skipped);
        assert_eq!(lane_due(&sh, 100 * s), 99 * s + step);
        // Over: its backlog its own again.
        {
            let mut c = sh.catches.lock().unwrap();
            c[0].to = 101 * s;
            c[0].end = CatchEnd::Caught;
        }
        sh.catching.store(false, Ordering::SeqCst);
        assert_eq!(lane_due(&sh, 200 * s), 99 * s + 2 * step);
        assert_eq!(sh.lane_dropped_milli.load(Ordering::SeqCst), skipped);
    }

    #[test]
    fn an_operation_is_the_restarts_until_its_worker_is_back_on_schedule() {
        let mut owed = None;
        let (r, none) = ((100, 200), (u64::MAX, u64::MAX));
        let w = |compact| Windows {
            restart: r,
            compact,
            replica: none,
        };
        let class = |at, on_time, w: &Windows, owed: &mut Option<Class>| {
            class(at, at + 5, false, on_time, w, owed)
        };
        assert_eq!(class(150, false, &w(none), &mut owed), Class::Restart);
        // Due after the server answered again, its worker still behind
        // with the backlog the restart left: the restart's.
        assert_eq!(class(210, false, &w(none), &mut owed), Class::Restart);
        assert_eq!(class(230, false, &w(none), &mut owed), Class::Restart);
        // Back on its schedule: steady from there.
        assert_eq!(class(240, true, &w(none), &mut owed), Class::Steady);
        assert_eq!(class(250, false, &w(none), &mut owed), Class::Steady);
        // A compact's alike.
        let c = (300, 400);
        assert_eq!(class(350, true, &w(c), &mut owed), Class::Compact);
        assert_eq!(class(410, false, &w(c), &mut owed), Class::Compact);
        assert_eq!(class(420, true, &w(c), &mut owed), Class::Steady);
    }

    #[test]
    fn a_restarts_reads_through_the_replica_are_held_to_the_replicas_bound() {
        let none = (u64::MAX, u64::MAX);
        let w = Windows {
            restart: (100, 200),
            compact: none,
            replica: none,
        };
        let mut h = Hists::new();
        // A read through the replica due in the restart, sent once the
        // replica was back -- its worker behind the restart's backlog --
        // and a primary operation due in it: the restart's, each its part.
        let mut owed = None;
        let c = class(150, 9_000, true, false, &w, &mut owed);
        assert_eq!(c, Class::Restart);
        h.record(c, true, false, true, 8_850);
        let mut owed = None;
        let c = class(160, 2_000, false, true, &w, &mut owed);
        assert_eq!(c, Class::Restart);
        h.record(c, false, false, true, 1_840);
        assert_eq!((h.restart_replica.n, h.restart.n), (1, 1));
        assert_eq!((h.restart_replica.max, h.restart.max), (8_850, 1_840));
        // A kill met behind its schedule: the backlog's, wherever the
        // operation went, as before.
        h.record(Class::Restart, true, true, true, 1);
        h.record(Class::Restart, false, true, true, 1);
        assert_eq!(
            (h.restart_behind.n, h.restart_replica.n, h.restart.n),
            (2, 1, 1)
        );
        // Merged as the minute's line merges them.
        let mut all = Hists::new();
        all.merge(&h);
        all.merge(&h);
        assert_eq!((all.restart_replica.n, all.restart.n), (2, 2));
    }

    #[test]
    fn a_read_through_the_compacting_replica_is_the_compactions_and_so_is_its_backlog() {
        let none = (u64::MAX, u64::MAX);
        // The replica compacting from 1 000 on, its end not yet seen.
        let w = Windows {
            restart: none,
            compact: none,
            replica: (1_000, u64::MAX),
        };
        let mut owed = None;
        // A primary operation due in it: not the replica's compaction's.
        assert_eq!(
            class(1_100, 1_110, false, true, &w, &mut owed),
            Class::Steady
        );
        // A read through the replica due in it: the compaction's.
        assert_eq!(
            class(1_200, 1_400, true, true, &w, &mut owed),
            Class::ReplicaCompact
        );
        assert_eq!(
            class(1_500, 1_510, false, true, &w, &mut owed),
            Class::Steady
        );
        // One sent before it began and held into it: the compaction's too,
        // and every operation its worker takes behind its schedule after.
        assert_eq!(
            class(900, 3_000, true, true, &w, &mut owed),
            Class::ReplicaCompact
        );
        assert_eq!(
            class(1_600, 3_010, false, false, &w, &mut owed),
            Class::ReplicaCompact
        );
        // Back on its schedule: steady.
        assert_eq!(
            class(3_100, 3_110, false, true, &w, &mut owed),
            Class::Steady
        );
        // One answered before it began, and one due after it ended: not.
        assert_eq!(class(800, 950, true, true, &w, &mut owed), Class::Steady);
        let ended = Windows {
            replica: (1_000, 5_000),
            ..w
        };
        assert_eq!(
            class(5_100, 5_300, true, true, &ended, &mut owed),
            Class::Steady
        );
        // The fourth dry run's minute 61: a worker held by a read through
        // the compacting replica takes the operations due in the restart
        // behind it -- the compaction's, not the restart's; one that was
        // not held, the restart's.
        let both = Windows {
            restart: (2_000, 4_000),
            compact: none,
            replica: (1_000, u64::MAX),
        };
        let mut held = None;
        assert_eq!(
            class(1_500, 6_000, true, true, &both, &mut held),
            Class::ReplicaCompact
        );
        assert_eq!(
            class(2_100, 6_010, false, false, &both, &mut held),
            Class::ReplicaCompact
        );
        let mut free = None;
        assert_eq!(
            class(2_200, 4_100, false, true, &both, &mut free),
            Class::Restart
        );
        // A histogram of their own, out of the compacts'; those due in a
        // restart's window counted apart again.
        let mut h = Hists::new();
        h.record(Class::ReplicaCompact, true, false, false, 4_500);
        h.record(Class::ReplicaCompact, false, false, true, 3_910);
        assert_eq!(
            (h.compact.n, h.r_compact.n, h.restart_r_compact, h.steady.n),
            (0, 2, 1, 0)
        );
        assert_eq!(h.r_compact.max, 4_500);
    }

    #[test]
    fn the_replicas_compaction_thread_says_when_it_compacts() {
        let s = 1_000_000_000u64;
        let ms = 1_000_000u64;
        let mut w = CompactWatch::default();
        assert_eq!(w.line("A aarch64", 0), None);
        assert_eq!(w.line("T 41", 0), None);
        // Not watched before it is seen asleep in a sleep call.
        assert!(!w.watching());
        assert_eq!(w.line("S 41 101", s), None);
        assert!(w.watching());
        // The server's look at its files every 5 s: at work a moment,
        // asleep again -- no compaction.
        assert_eq!(w.line("S 41 running", 5 * s), None);
        assert_eq!(w.line("S 41 101", 5 * s + 100 * ms), None);
        assert_eq!(w.tick(7 * s), None);
        // A compaction: at work, in one call and another, held past the
        // bound -- dated from its first look, and a little before it.
        assert_eq!(w.line("S 41 98", 10 * s), None);
        assert_eq!(w.line("S 41 -1", 10 * s + 200 * ms), None);
        assert_eq!(w.tick(10 * s + 400 * ms), None);
        assert_eq!(
            w.tick(10 * s + 500 * ms),
            Some(Edge::Began(10 * s - 200 * ms))
        );
        assert_eq!(w.tick(15 * s), None);
        // A moment asleep within it ends nothing.
        assert_eq!(w.line("S 41 115", 20 * s), None);
        assert_eq!(w.line("S 41 running", 20 * s + 100 * ms), None);
        assert_eq!(w.tick(21 * s), None);
        // Its end: asleep, held -- dated from its first look asleep.
        assert_eq!(w.line("S 41 115", 35 * s), None);
        assert_eq!(w.tick(35 * s + 600 * ms), Some(Edge::Ended(35 * s)));
        assert_eq!(w.tick(40 * s), None);
        // The sleeps by the VM's architecture; at work otherwise.
        assert!(asleep("x86_64", "35") && asleep("x86_64", "230") && asleep("x86_64", "219"));
        assert!(!asleep("x86_64", "101") && !asleep("x86_64", "0"));
        assert!(asleep("aarch64", "101") && asleep("aarch64", "115") && asleep("aarch64", "128"));
        assert!(
            !asleep("aarch64", "running") && !asleep("aarch64", "-1") && !asleep("aarch64", "63")
        );
        // A sleep call itself, which a timed futex a signal cut is not.
        assert!(plain_sleep("x86_64", "35") && plain_sleep("x86_64", "230"));
        assert!(!plain_sleep("x86_64", "219") && !plain_sleep("x86_64", "202"));
        assert!(plain_sleep("aarch64", "101") && plain_sleep("aarch64", "115"));
        assert!(!plain_sleep("aarch64", "128") && !plain_sleep("aarch64", "98"));
    }

    #[test]
    fn the_watch_reads_fennec_https_thread_whatever_the_order() {
        let s = 1_000_000_000u64;
        let ms = 1_000_000u64;
        // fennec-core's `Compactor` (9) beside fennec-http's thread (7),
        // both so named: the Compactor waits between its looks on a futex
        // (202 on x86_64), fennec-http's sleeps (230). Listed either way,
        // the one watched is the sleeper, and the Compactor's wait is no
        // compaction -- the script that took the last so named read the
        // Compactor wherever the kernel listed it last, one window the
        // run long.
        for order in ["T 7 9", "T 9 7"] {
            let mut w = CompactWatch::default();
            w.line("A x86_64", 0);
            w.line(order, 0);
            assert_eq!(w.line("S 9 202", s), None);
            assert_eq!(w.tick(3 * s), None, "{order}");
            assert!(!w.watching(), "{order}");
            assert_eq!(w.line("S 7 230", 3 * s), None);
            assert!(w.watching(), "{order}");
            // The Compactor at work and in its futex for minutes: nothing.
            assert_eq!(w.line("S 9 running", 4 * s), None);
            assert_eq!(w.line("S 9 202", 5 * s), None);
            assert_eq!(w.tick(100 * s), None, "{order}");
            // fennec-http's at work -- a join's futex within it at work too.
            assert_eq!(w.line("S 7 running", 101 * s), None);
            assert_eq!(w.line("S 7 202", 101 * s + 100 * ms), None);
            assert_eq!(
                w.tick(102 * s),
                Some(Edge::Began(101 * s - 200 * ms)),
                "{order}"
            );
            assert_eq!(w.line("S 7 230", 130 * s), None);
            assert_eq!(w.tick(131 * s), Some(Edge::Ended(130 * s)), "{order}");
        }
        // A Compactor alone: never watched, its futex never a window.
        let mut w = CompactWatch::default();
        w.line("A x86_64", 0);
        w.line("T 9", 0);
        assert_eq!(w.line("S 9 202", s), None);
        assert_eq!(w.tick(600 * s), None);
        assert!(!w.watching());
    }

    #[test]
    fn the_watch_reads_the_thread_fennec_http_names() {
        let link = include_str!("../../fennec-http/src/link.rs");
        let garbage = include_str!("../../fennec-core/src/engine/garbage.rs");
        let part = |src: &'static str, from: &str, to: &str| -> &'static str {
            let a = src.find(from).unwrap_or_else(|| panic!("{from}"));
            let b = a + src[a..].find(to).unwrap_or_else(|| panic!("{to}"));
            &src[a..b]
        };
        let named = |part: &str| -> String {
            let at = part.find(".name(\"").expect("a named thread") + 7;
            part[at..at + part[at..].find('"').unwrap()].to_string()
        };
        // The thread a server compacts its files on, as the watch looks
        // for it -- a thread's name is 15 bytes in `comm` -- and asleep
        // between its looks.
        let server = part(link, "COMPACTER.call_once", "KEEPER.call_once");
        assert_eq!(named(server), R_COMPACT_THREAD);
        assert!(R_COMPACT_THREAD.len() <= 15);
        assert!(server.contains("std::thread::sleep("), "{server}");
        // fennec-core's, the native library's: so named, and waiting on a
        // condvar, never in a sleep -- what tells the two apart.
        let core = part(garbage, "impl Compactor", "impl Drop for Compactor");
        assert_eq!(named(core), R_COMPACT_THREAD);
        assert!(core.contains("wait_timeout_while") && !core.contains("thread::sleep"));
        // And the watch's script asks for that name.
        assert!(watch_script("/proc/1").contains(&format!("[ \"$c\" = {R_COMPACT_THREAD} ]")));
    }

    #[test]
    fn the_watch_is_on_no_network_and_looks_for_a_missing_thread_slowly() {
        let a = watch_args("w", "r", "true");
        assert!(a.windows(2).any(|x| x == ["--network", "none"]), "{a:?}");
        assert!(a.iter().any(|x| x == "--pid=container:r"), "{a:?}");
        assert_eq!(watch_again(WATCH_ABSENT), Duration::from_secs(60));
        assert_eq!(watch_again(WATCH_ENDED), Duration::from_secs(5));
        assert_eq!(watch_again(WATCH_FAILED), Duration::from_secs(5));
    }

    #[test]
    fn the_watch_script_lists_every_thread_so_named_and_ends_with_them() {
        use std::process::{Command, Stdio};
        /// The script's shell, killed on the way out whatever happens.
        struct Killed(std::process::Child);
        impl Drop for Killed {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = std::env::temp_dir().join(format!("soak-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let task = |tid: &str, comm: &str, call: &str| {
            let d = dir.join("task").join(tid);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(d.join("syscall"), format!("{call} 0x1 0x2\n")).unwrap();
        };
        task("7", R_COMPACT_THREAD, "230");
        task("8", "fennec-http", "232");
        task("9", R_COMPACT_THREAD, "202");
        let mut child = Killed(
            Command::new("sh")
                .arg("-c")
                .arg(watch_script(dir.to_str().unwrap()))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let (tx, lines) = mpsc::channel();
        let out = child.0.stdout.take().unwrap();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        let next = || lines.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(next().starts_with("A "));
        assert_eq!(next(), "T 7 9");
        let mut said = [next(), next()];
        said.sort();
        assert_eq!(said, ["S 7 230", "S 9 202"]);
        // Said again only as it changes -- the file replaced whole, as the
        // kernel's is read: a read of one cut short ends the script.
        std::fs::write(dir.join("task/7/syscall.new"), "running\n").unwrap();
        std::fs::rename(dir.join("task/7/syscall.new"), dir.join("task/7/syscall")).unwrap();
        assert_eq!(next(), "S 7 running");
        // Fed to the watch, the thread watched is the one asleep in a
        // sleep call, whatever the order the two were said in.
        let mut w = CompactWatch::default();
        for l in ["A x86_64", "T 7 9", "S 9 202", "S 7 230"] {
            w.line(l, 0);
        }
        assert_eq!(w.tid.as_deref(), Some("7"));
        // Its threads gone -- the replica's process -- the script ends.
        std::fs::remove_dir_all(&dir).unwrap();
        let t = Instant::now();
        while child.0.try_wait().unwrap().is_none() {
            assert!(t.elapsed() < Duration::from_secs(10), "the script went on");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_watch_started_again_in_a_compaction_counts_it_once() {
        let s = 1_000_000_000u64;
        let ms = 1_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        // Each edge as the watch's loop takes it, the counter moved at
        // each end seen.
        let feed = |w: &mut CompactWatch, l: Option<&str>, t: u64| {
            let e = match l {
                Some(l) => w.line(l, t),
                None => w.tick(t),
            };
            let count = match e {
                Some(Edge::Ended(_)) => Count::Read {
                    moved: true,
                    stale: false,
                },
                _ => Count::Unread,
            };
            replica_compaction(&sh, e, count);
        };
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        let mut w = CompactWatch::default();
        for l in ["A x86_64", "T 7", "S 7 230"] {
            feed(&mut w, Some(l), s);
        }
        feed(&mut w, Some("S 7 running"), 10 * s);
        feed(&mut w, None, 11 * s);
        assert_eq!(
            *sh.r_window.lock().unwrap(),
            (1, 10 * s - 200 * ms, u64::MAX)
        );
        // Its container gone in the compaction: the next finds the thread
        // still at work and goes on with it -- no new window, and one end.
        // Before, the first ended it there and the next began another: one
        // compaction counted twice. Watching again once it lists the
        // threads, not before: a `docker run` that hangs lists none.
        let mut w = CompactWatch::after(&w);
        assert!(!w.watching());
        feed(&mut w, Some("A x86_64"), 16 * s);
        assert!(!w.watching());
        for l in ["T 7", "S 7 running"] {
            feed(&mut w, Some(l), 16 * s);
        }
        assert!(w.watching());
        feed(&mut w, None, 20 * s);
        feed(&mut w, Some("S 7 230"), 35 * s);
        feed(&mut w, None, 36 * s);
        assert_eq!(*sh.r_window.lock().unwrap(), (1, 10 * s - 200 * ms, 35 * s));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compacts_cut)), (1, 0));
        assert_eq!(n(&sh.r_compact_ms), 25_200);
        // Gone in the next, whose successor finds the thread asleep: it
        // ended unseen -- cut, never counted as an end, its reads the
        // compaction's still.
        feed(&mut w, Some("S 7 running"), 50 * s);
        feed(&mut w, None, 51 * s);
        let mut w = CompactWatch::after(&w);
        for l in ["A x86_64", "T 7", "S 7 230"] {
            feed(&mut w, Some(l), 70 * s);
        }
        feed(&mut w, None, 71 * s);
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compacts_cut)), (1, 1));
        assert_eq!(sh.r_verdict(2), Some(true));
        assert_eq!(*sh.r_window.lock().unwrap(), (2, 50 * s - 200 * ms, 70 * s));
        // The replica started again in one: its thread another, the
        // compaction cut with its process.
        feed(&mut w, Some("S 7 running"), 80 * s);
        feed(&mut w, None, 81 * s);
        let mut w = CompactWatch::after(&w);
        feed(&mut w, Some("A x86_64"), 90 * s);
        feed(&mut w, Some("T 12"), 90 * s);
        assert!(!w.watching());
        assert_eq!(n(&sh.r_compacts_cut), 2);
        // And the run's end in one.
        for l in ["S 12 230", "S 12 running"] {
            feed(&mut w, Some(l), 100 * s);
        }
        feed(&mut w, None, 101 * s);
        replica_compaction(&sh, w.stop(110 * s), Count::Unread);
        assert_eq!(w.stop(111 * s), None);
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compacts_cut)), (1, 3));
        assert_eq!(sh.r_verdicts.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_replicas_compaction_is_settled_by_its_counter() {
        let s = 1_000_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        let moved = |moved| Count::Read {
            moved,
            stale: false,
        };
        // No edge, or an end with none begun: nothing.
        replica_compaction(&sh, None, Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(5 * s)), moved(true));
        assert_eq!(n(&sh.r_compacts_seen), 0);
        assert!(sh.r_verdicts.lock().unwrap().is_empty());
        // Begun: open and numbered, not yet settled.
        replica_compaction(&sh, Some(Edge::Began(10 * s)), Count::Unread);
        assert_eq!(*sh.r_window.lock().unwrap(), (1, 10 * s, u64::MAX));
        assert_eq!(sh.r_verdict(1), None);
        // Its end seen, its counter moved: a compaction, its length kept
        // in whole ms rounded up.
        replica_compaction(&sh, Some(Edge::Ended(35 * s + 400)), moved(true));
        assert_eq!(*sh.r_window.lock().unwrap(), (1, 10 * s, 35 * s + 400));
        assert_eq!(sh.r_verdict(1), Some(true));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compact_ms)), (1, 25_001));
        // The thread at work as long, its counter unmoved -- its look at
        // the files waiting on the replica's lock: none, its window
        // closed to nothing.
        replica_compaction(&sh, Some(Edge::Began(100 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(101 * s)), moved(false));
        assert_eq!(*sh.r_window.lock().unwrap(), (2, u64::MAX, u64::MAX));
        assert_eq!(sh.r_verdict(2), Some(false));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compacts_false)), (1, 1));
        // Its counter not read: the watch's word, and said so.
        replica_compaction(&sh, Some(Edge::Began(200 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(230 * s)), Count::Unread);
        assert_eq!(sh.r_verdict(3), Some(true));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compact_ms)), (2, 30_000));
        assert_eq!(n(&sh.r_compacts_unread), 1);
        // Cut: the watch's word for its reads, never an end.
        replica_compaction(&sh, Some(Edge::Began(300 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Cut(310 * s)), moved(false));
        assert_eq!(sh.r_verdict(4), Some(true));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compacts_cut)), (2, 1));
        assert_eq!(sh.r_verdict(5), None);
        assert_eq!(sh.r_verdict(0), None);
    }

    #[test]
    fn a_window_its_counter_did_not_move_in_leaves_its_reads_their_class() {
        let none = (u64::MAX, u64::MAX);
        let w = |replica, compact| Windows {
            restart: none,
            compact,
            replica,
        };
        let open = w((1_000, u64::MAX), (1_100, 1_250));
        let unsettled = |_: u64| None;
        let mut c = Classes::default();
        // A read through the replica in window 1, not yet settled: held,
        // with the class it has otherwise -- the compact's, due in one --
        // and its worker's next, behind it, the compact's backlog.
        assert_eq!(
            c.of(1_200, 1_400, true, true, &open, 1, &unsettled),
            (Class::ReplicaCompact, Some((1, Class::Compact)))
        );
        assert_eq!(
            c.of(1_300, 1_500, false, false, &open, 1, &unsettled),
            (Class::ReplicaCompact, Some((1, Class::Compact)))
        );
        // Settled as none: the worker owing it goes back to the chain it
        // would have had, the compact's backlog until back on schedule.
        let gone = w(none, (1_100, 1_250));
        let was_none = |id: u64| (id == 1).then_some(false);
        assert_eq!(
            c.of(1_350, 1_600, false, false, &gone, 1, &was_none),
            (Class::Compact, None)
        );
        assert_eq!(
            c.of(1_700, 1_710, false, true, &gone, 1, &was_none),
            (Class::Steady, None)
        );
        // Owing one found to be none as the next begins: a read held in
        // the next is the next's, held for it -- not the first's, as none.
        let mut c = Classes::default();
        assert_eq!(
            c.of(
                1_200,
                1_400,
                true,
                true,
                &w((1_000, u64::MAX), none),
                1,
                &unsettled
            ),
            (Class::ReplicaCompact, Some((1, Class::Steady)))
        );
        let next = w((2_000, u64::MAX), none);
        assert_eq!(
            c.of(1_300, 2_500, true, false, &next, 2, &was_none),
            (Class::ReplicaCompact, Some((2, Class::Steady)))
        );
        // One settled as a compaction: the compaction's at once, its
        // worker's backlog too.
        let mut c = Classes::default();
        let ended = w((1_000, 5_000), none);
        let was = |_: u64| Some(true);
        assert_eq!(
            c.of(1_200, 6_000, true, true, &ended, 1, &was),
            (Class::ReplicaCompact, None)
        );
        assert_eq!(
            c.of(1_300, 6_010, false, false, &ended, 1, &was),
            (Class::ReplicaCompact, None)
        );
        // The held operations as their window settles: the compaction's
        // where it was one, their own class where it was none, kept where
        // it is open still -- until the run's last minute, which takes
        // the watch's word.
        let held = |window, otherwise, took| Held {
            window,
            otherwise,
            rec: Rec {
                via_replica: false,
                behind: false,
                in_restart: window == 2,
                took,
                own_us: 0,
            },
        };
        let mut h = Hists::new();
        h.held = VecDeque::from([
            held(1, Class::Steady, 200),
            held(2, Class::Restart, 300),
            held(3, Class::Compact, 400),
        ]);
        h.settle(&|id| match id {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        });
        assert_eq!(
            (h.steady.n, h.r_compact.n, h.restart_r_compact, h.restart.n),
            (1, 1, 1, 0)
        );
        assert_eq!(h.held.len(), 1);
        // Merged as the minute's line merges them: the held stay the
        // worker's.
        let mut all = Hists::new();
        all.merge(&h);
        assert!(all.held.is_empty());
        h.settle(&|_| Some(true));
        assert_eq!((h.r_compact.n, h.compact.n, h.held.len()), (2, 0, 0));
    }

    #[test]
    fn the_watch_opens_a_window_before_it_reads_its_counter_and_says_how_it_judged_it() {
        use std::cell::RefCell;
        let s = 1_000_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        // The counter as each read finds it, a window's start and end in
        // turn, and the window as each read was made.
        let answers = RefCell::new(VecDeque::from([
            // A compaction: risen.
            Some(3),
            Some(4),
            // The replica started again in it, its thread numbered as
            // before: lower, none counted since -- no compaction, where
            // `n != b` read it as one.
            Some(4),
            Some(0),
            // Again, one counted since its start: a compaction.
            Some(5),
            Some(1),
            // The read as it began failed: the last read stands for its
            // base, and the window is said as judged on it.
            None,
            Some(2),
            // Its end not read: the watch's word.
            Some(2),
            None,
        ]));
        let windows = RefCell::new(Vec::new());
        let mut read = || {
            windows.borrow_mut().push(*sh.r_window.lock().unwrap());
            answers.borrow_mut().pop_front().unwrap()
        };
        let mut c = Counter::default();
        for k in 0..5u64 {
            let t = (100 + 100 * k) * s;
            c.edge(&sh, Some(Edge::Began(t)), &mut read);
            c.edge(&sh, Some(Edge::Ended(t + 30 * s)), &mut read);
        }
        // Each window open before its base was read: the read waits on the
        // replica's lock up to 10 s, and a read through the replica
        // answered meanwhile took the class it had without the window.
        let seen = windows.borrow();
        for k in 0..5u64 {
            let t = (100 + 100 * k) * s;
            assert_eq!(
                seen[2 * k as usize],
                (k + 1, t, u64::MAX),
                "window {}",
                k + 1
            );
            assert_eq!(seen[2 * k as usize + 1], (k + 1, t, u64::MAX));
        }
        assert_eq!(
            (1..=5).map(|id| sh.r_verdict(id)).collect::<Vec<_>>(),
            [Some(true), Some(false), Some(true), Some(true), Some(true)]
        );
        assert_eq!(
            (
                n(&sh.r_compacts_seen),
                n(&sh.r_compacts_false),
                n(&sh.r_compacts_reset),
                n(&sh.r_compacts_stale),
                n(&sh.r_compacts_unread)
            ),
            (4, 1, 2, 1, 1)
        );
        // A counter never read: the watch's word, said as unread.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let mut c = Counter::default();
        let mut never = || -> Option<u64> { None };
        c.edge(&sh, Some(Edge::Began(10 * s)), &mut never);
        c.edge(&sh, Some(Edge::Ended(40 * s)), &mut never);
        assert_eq!(sh.r_verdict(1), Some(true));
        assert_eq!(n(&sh.r_compacts_unread), 1);
        // A cut reads nothing.
        let mut none = || -> Option<u64> { panic!("a cut read the counter") };
        c.edge(&sh, Some(Edge::Began(50 * s)), &mut || Some(7u64));
        c.edge(&sh, Some(Edge::Cut(60 * s)), &mut none);
        assert_eq!(n(&sh.r_compacts_cut), 1);
    }

    #[test]
    fn a_window_whose_watch_stays_gone_or_that_outlives_its_file_is_cut() {
        let s = 1_000_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        let moved = Count::Read {
            moved: true,
            stale: false,
        };
        // What a compaction of the replica's file plausibly takes: 500 s a
        // GB, four times the primary's slowest a GB where that is more,
        // ten minutes at the least.
        assert_eq!(plausible_ms(0, 0), 600_000);
        assert_eq!(plausible_ms(2_400_000_000, 0), 1_200_000);
        assert_eq!(plausible_ms(2_400_000_000, 400_000), 3_840_000);
        assert_eq!(plausible_ms(1_000_000_000, 100_000), 600_000);
        // A window open, its watch gone from 20 s: a minute's look 30 s
        // after keeps it, one 61 s after finds it lost -- counted so, once,
        // its reads as if no window had been, never an end.
        replica_compaction(&sh, Some(Edge::Began(10 * s)), Count::Unread);
        sh.r_down_since.store(20 * s, Ordering::SeqCst);
        bound_r_window(&sh, 50 * s, false);
        assert_eq!(sh.r_verdict(1), None);
        bound_r_window(&sh, 81 * s, false);
        bound_r_window(&sh, 141 * s, false);
        assert_eq!(sh.r_verdict(1), Some(false));
        assert_eq!(*sh.r_window.lock().unwrap(), (1, u64::MAX, u64::MAX));
        assert_eq!(
            (
                n(&sh.r_compacts_lost),
                n(&sh.r_compacts_cut),
                n(&sh.r_compacts_seen)
            ),
            (1, 0, 0)
        );
        // The watch back, the thread asleep: its end said, not counted.
        replica_compaction(&sh, Some(Edge::Ended(190 * s)), moved);
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compact_ms)), (0, 0));
        assert_eq!(sh.r_since.load(Ordering::SeqCst), u64::MAX);
        // The watch gone with no window open: nothing lost.
        bound_r_window(&sh, 300 * s, false);
        assert_eq!(n(&sh.r_compacts_lost), 1);
        // A window the watch sees open past the bound, ten minutes for a
        // small file: a thread that never sleeps again -- counted once,
        // its reads' window cut, and never an excuse for its reads; never
        // counted as cut before its end was seen, its end still the
        // watch's to see.
        sh.r_down_since.store(u64::MAX, Ordering::SeqCst);
        replica_compaction(&sh, Some(Edge::Began(1_000 * s)), Count::Unread);
        bound_r_window(&sh, 1_550 * s, true);
        assert_eq!(sh.r_verdict(2), None);
        bound_r_window(&sh, 1_601 * s, true);
        assert_eq!(sh.r_verdict(2), Some(false));
        bound_r_window(&sh, 1_700 * s, true);
        assert_eq!((n(&sh.r_compacts_overlong), n(&sh.r_compacts_cut)), (1, 0));
        // Its end seen: judged by its counter like any other's -- the two
        // agree -- its reads still as if no window had been. Before, it
        // was counted as cut before its end was seen, and the watch's
        // criterion failed beside the bound's.
        replica_compaction(&sh, Some(Edge::Ended(1_900 * s)), moved);
        assert_eq!(
            (
                n(&sh.r_compacts_seen),
                n(&sh.r_compacts_cut),
                n(&sh.r_compact_ms)
            ),
            (1, 0, 0)
        );
        // One that ended past it between two looks: the same, past its
        // bound once, its end counted, never cut.
        replica_compaction(&sh, Some(Edge::Began(3_000 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(3_700 * s)), moved);
        assert_eq!(sh.r_verdict(3), Some(false));
        assert_eq!(*sh.r_window.lock().unwrap(), (3, u64::MAX, u64::MAX));
        assert_eq!(
            (
                n(&sh.r_compacts_overlong),
                n(&sh.r_compacts_seen),
                n(&sh.r_compacts_cut),
                n(&sh.r_compact_ms)
            ),
            (2, 2, 0, 0)
        );
        // Its counter unmoved: no compaction, past its bound all the same.
        replica_compaction(&sh, Some(Edge::Began(3_800 * s)), Count::Unread);
        replica_compaction(
            &sh,
            Some(Edge::Ended(4_500 * s)),
            Count::Read {
                moved: false,
                stale: false,
            },
        );
        assert_eq!(
            (n(&sh.r_compacts_overlong), n(&sh.r_compacts_false)),
            (3, 1)
        );
        // Its reads' window cut before, by a worker's held: past its bound
        // all the same, at a look or at its end between two.
        replica_compaction(&sh, Some(Edge::Began(5_000 * s)), Count::Unread);
        assert!(cut_r_window(&sh, 5, 5_100 * s, Cut::Held));
        bound_r_window(&sh, 5_700 * s, true);
        assert_eq!(n(&sh.r_compacts_overlong), 4);
        replica_compaction(&sh, Some(Edge::Ended(5_800 * s)), moved);
        replica_compaction(&sh, Some(Edge::Began(5_900 * s)), Count::Unread);
        assert!(cut_r_window(&sh, 6, 5_910 * s, Cut::Held));
        replica_compaction(&sh, Some(Edge::Ended(6_550 * s)), moved);
        assert_eq!(
            (
                n(&sh.r_compacts_overlong),
                n(&sh.r_compacts_seen),
                n(&sh.r_compacts_cut)
            ),
            (5, 4, 0)
        );
        // Within its bound, watched: kept, and counted as it ends.
        replica_compaction(&sh, Some(Edge::Began(7_000 * s)), Count::Unread);
        bound_r_window(&sh, 7_020 * s, true);
        replica_compaction(&sh, Some(Edge::Ended(7_025 * s)), moved);
        assert_eq!(sh.r_verdict(7), Some(true));
        assert_eq!((n(&sh.r_compacts_seen), n(&sh.r_compact_ms)), (5, 25_000));
        assert_eq!(n(&sh.r_compacts_lost), 1);
    }

    #[test]
    fn a_window_its_watch_lost_while_open_is_never_a_pass() {
        let s = 1_000_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        // A compaction thread that never sleeps again, found past its
        // bound by a look while watched -- its reads' window cut -- and
        // its watch gone with it after: lost at the next look a minute
        // on, which the watch's criterion fails on. Before, judged by its
        // reads' window alone, cut already, it was never lost, its counter
        // never moved, and the two criteria passed beside the overlong.
        sh.watched(true);
        replica_compaction(&sh, Some(Edge::Began(10 * s)), Count::Unread);
        bound_r_window(&sh, 700 * s, true);
        assert_eq!(n(&sh.r_compacts_overlong), 1);
        sh.watched(false);
        let down = sh.r_down_since.load(Ordering::SeqCst);
        bound_r_window(&sh, down + 30 * s, false);
        assert_eq!(n(&sh.r_compacts_lost), 0);
        bound_r_window(&sh, down + R_WATCH_GONE, false);
        bound_r_window(&sh, down + 2 * R_WATCH_GONE, false);
        assert_eq!((n(&sh.r_compacts_lost), n(&sh.r_compacts_overlong)), (1, 1));
        // The same thread's watch gone from the start, never past its
        // bound while watched: lost, its reads' window cut.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        sh.watched(true);
        replica_compaction(&sh, Some(Edge::Began(10 * s)), Count::Unread);
        sh.watched(false);
        let down = sh.r_down_since.load(Ordering::SeqCst);
        bound_r_window(&sh, down + R_WATCH_GONE, false);
        assert_eq!(n(&sh.r_compacts_lost), 1);
        assert_eq!(sh.r_verdict(1), Some(false));
        // Its container gone in one and the next finding the thread
        // asleep: cut, its end unseen, and lost; a watch finding it gone
        // with the replica's process: cut alone.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let mut w = CompactWatch::default();
        let feed = |w: &mut CompactWatch, l: Option<&str>, t: u64| {
            let e = match l {
                Some(l) => w.line(l, t),
                None => w.tick(t),
            };
            replica_compaction(&sh, e, Count::Unread);
        };
        for l in ["A x86_64", "T 7", "S 7 230", "S 7 running"] {
            feed(&mut w, Some(l), s);
        }
        feed(&mut w, None, 2 * s);
        let mut w = CompactWatch::after(&w);
        for l in ["A x86_64", "T 7", "S 7 230"] {
            feed(&mut w, Some(l), 20 * s);
        }
        assert_eq!(w.tick(21 * s), Some(Edge::Lost(20 * s)));
        replica_compaction(&sh, Some(Edge::Lost(20 * s)), Count::Unread);
        feed(&mut w, Some("S 7 running"), 30 * s);
        feed(&mut w, None, 31 * s);
        let mut w = CompactWatch::after(&w);
        for l in ["A x86_64", "T 9"] {
            feed(&mut w, Some(l), 40 * s);
        }
        assert_eq!((n(&sh.r_compacts_cut), n(&sh.r_compacts_lost)), (2, 1));
        // On the lines: a window lost while open fails the watch's
        // criterion though the counter and the watch agree, and so does
        // one open at the last look with its watch gone.
        let verdict = |text: &str| -> (bool, String) {
            let j = judge(text, None);
            let v = j[0].verdicts.iter().find(|v| v.0 == CRITERIA[21]).unwrap();
            (v.1, v.2.clone())
        };
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if m == 200 {
                x.insert("r_compacts_lost", 1.0);
                x.insert("r_compacts_overlong", 1.0);
            }
        }));
        assert!(!ok, "{said}");
        assert!(
            said.contains("; 1 LOST by the watch while open, its end unseen"),
            "{said}"
        );
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if m == 480 {
                x.insert("r_watch", 0.0);
                x.insert("r_compact_open", 1.0);
                x.insert("r_compact_open_ms", 12_000.0);
            }
        }));
        assert!(!ok, "{said}");
        // An overlong window whose end was seen, its counter moved: the
        // bound's criterion fails, the watch's passes, saying nothing of a
        // cut.
        let text = synthetic(480, &|m, x| {
            if m == 200 {
                x.insert("r_compacts_overlong", 1.0);
                x.insert("r_compacts_watched", 1.0);
                x.insert("r_auto_compacts", 1.0);
            }
        });
        assert_eq!(failing(&text), vec![CRITERIA[22]]);
        let (_, said) = verdict(&text);
        assert!(!said.contains("cut"), "{said}");
        // And one window its counter did not move in is one.
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if m == 200 {
                x.insert("r_compacts_false", 1.0);
            }
        }));
        assert!(
            ok && said.contains("; 1 window its counter did not move in"),
            "{said}"
        );
    }

    #[test]
    fn a_worker_settles_its_held_from_the_oldest_and_cuts_a_window_it_holds_the_most_of() {
        use std::cell::Cell;
        let s = 1_000_000_000u64;
        let rec = |took| Rec {
            via_replica: true,
            behind: false,
            in_restart: false,
            took,
            own_us: 0,
        };
        // A thousand operations held for window 1, open: each operation
        // asks of the oldest's window alone -- the rescan asked of every one
        // held, half a million times, and moved them all.
        let calls = Cell::new(0u64);
        let state = Cell::new(None::<bool>);
        let verdict = |id: u64| {
            calls.set(calls.get() + 1);
            (id == 1).then(|| state.get()).flatten()
        };
        let mut h = Hists::new();
        for i in 0..1_000 {
            let back = h.took(
                Class::ReplicaCompact,
                Some((1, Class::Steady)),
                rec(i),
                &verdict,
            );
            assert_eq!(back, None);
        }
        assert_eq!(h.held.len(), 1_000);
        assert!(calls.get() < 1_000, "{}", calls.get());
        // Settled as none: the next operation settles every one, as soon
        // as it is known, not at the minute's line.
        state.set(Some(false));
        h.took(Class::Steady, None, rec(5), &verdict);
        assert!(h.held.is_empty());
        assert_eq!((h.steady.n, h.r_compact.n), (1_001, 0));
        // A worker holding the most for window 2: the window handed back,
        // cut -- said, as if no window had been -- and settled at once.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        sh.r_verdicts.lock().unwrap().push(true);
        *sh.r_window.lock().unwrap() = (2, s, u64::MAX);
        let verdict = |id: u64| sh.r_verdict(id);
        let mut h = Hists::new();
        let mut back = None;
        for i in 0..HELD_MAX {
            back = h.took(
                Class::ReplicaCompact,
                Some((2, Class::Steady)),
                rec(i as u64),
                &verdict,
            );
            assert!(back.is_none() || i == HELD_MAX - 1, "{i}");
        }
        assert_eq!(back, Some(2));
        assert!(cut_r_window(&sh, 2, 9 * s, Cut::Held));
        h.settle(&verdict);
        assert!(h.held.is_empty());
        assert_eq!((h.steady.n as usize, h.r_compact.n), (HELD_MAX, 0));
        assert_eq!(sh.r_verdict(2), Some(false));
        assert_eq!(*sh.r_window.lock().unwrap(), (2, u64::MAX, u64::MAX));
        // Its reads' window cut, its end still the watch's to see: no
        // count of an end unseen.
        assert_eq!(sh.r_compacts_cut.load(Ordering::Relaxed), 0);
        // The most it held, for the minute's line: all of it, a figure
        // that stays once the held are settled -- and what each line
        // writes, the most of every worker's since the last, and the end
        // line the run's.
        assert_eq!(h.held_max, HELD_MAX);
        let mut kept = Kept::new();
        let mut minute = Hists::new();
        minute.merge(&Hists::new());
        minute.merge(&h);
        let line = kept.minute(&minute);
        let fig = |v: &[(&str, String)], k: &str| v.iter().find(|x| x.0 == k).map(|x| x.1.clone());
        assert_eq!(fig(&line, "r_held_max"), Some(HELD_MAX.to_string()));
        let line = kept.minute(&Hists::new());
        assert_eq!(fig(&line, "r_held_max"), Some("0".into()));
        assert_eq!(fig(&kept.end(), "r_held_max"), Some(HELD_MAX.to_string()));
        // Settled already, or another window's begun: no cut.
        assert!(!cut_r_window(&sh, 2, 10 * s, Cut::Held));
        assert!(!cut_r_window(&sh, 1, 10 * s, Cut::Held));
    }

    #[test]
    fn the_runs_last_line_settles_what_every_worker_holds() {
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let hold = || {
            for (i, slot) in sh.slots.iter().take(2).enumerate() {
                let mut g = slot.hists.lock().unwrap();
                for k in 0..3 {
                    g.held.push_back(Held {
                        window: 1,
                        otherwise: [Class::Steady, Class::Restart][i],
                        rec: Rec {
                            via_replica: i == 0,
                            behind: false,
                            in_restart: i == 1,
                            took: 1_000 + k,
                            own_us: 0,
                        },
                    });
                }
            }
        };
        let held = || -> usize {
            sh.slots
                .iter()
                .map(|s| s.hists.lock().unwrap().held.len())
                .sum()
        };
        hold();
        // A line before the last, the window open: kept for a later one.
        for slot in &sh.slots {
            slot.hists.lock().unwrap().settle(&|id| sh.r_verdict(id));
        }
        assert_eq!(held(), 6);
        // The last, the watch looking: its word, the compaction's.
        let mut h = Hists::new();
        flush_held(&sh.slots, &mut h, &|id| sh.r_verdict(id).or(Some(true)));
        assert_eq!((held(), h.r_compact.n, h.restart_r_compact), (0, 6, 3));
        // The watch gone at it: as if no window had been.
        hold();
        let mut h = Hists::new();
        flush_held(&sh.slots, &mut h, &|id| sh.r_verdict(id).or(Some(false)));
        assert_eq!(
            (held(), h.r_compact.n, h.steady.n, h.restart.n),
            (0, 0, 3, 3)
        );
        // The last line: the planned minutes', or the one the environment
        // ended the run at -- whose held were lost.
        assert!(last_line(120, 120, false));
        assert!(last_line(7, 120, true));
        assert!(!last_line(7, 120, false));
    }

    #[test]
    fn an_hour_with_no_compacts_operation_leaves_the_compacts_not_judged() {
        let verdict = |text: &str| -> (bool, String) {
            let j = judge(text, None);
            let v = j[0].verdicts.iter().find(|v| v.0 == CRITERIA[3]).unwrap();
            (v.1, v.2.clone())
        };
        // The last hour with no compact's operation: read as 0, it passed
        // the compacts against any.
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if (421..=480).contains(&m) {
                x.remove("p99_compact_hour");
            }
        }));
        assert!(
            ok && said.contains("compacts not judged: no compact's operation in the last hour"),
            "{said}"
        );
        // The steady half judged as ever.
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if (421..=480).contains(&m) {
                x.remove("p99_compact_hour");
                x.insert("p99_hour", 3_000.0);
            }
        }));
        assert!(!ok && said.contains("compacts not judged"), "{said}");
        // The reference hour's an older line's 0: none, not judged.
        let (ok, said) = verdict(&synthetic(480, &|m, x| {
            if (61..=120).contains(&m) {
                x.insert("p99_compact_hour", 0.0);
            }
        }));
        assert!(
            ok && said.contains("compacts not judged: no compact's operation in minutes 61-120"),
            "{said}"
        );
        // Both: judged.
        let (ok, said) = verdict(&synthetic(480, &|_, _| {}));
        assert!(
            ok && said.contains("in compacts 5000 us to 5000 us"),
            "{said}"
        );
    }

    #[test]
    fn a_read_a_restarts_backlog_sends_into_the_compacting_replica_is_the_compactions() {
        // The fourth dry run's kill of minute 60, from its timeline (ms past
        // 20:10:00): the replica compacting from 9 300 to 34 300, the kill
        // announced at 9 800 and sent at 10 800, the server answering
        // again 2.0 s after it, and reads through the replica again at
        // kill + 8.1 s.
        let ms = 1_000_000u64;
        let w = Windows {
            restart: (9_800 * ms, 12_800 * ms),
            compact: (u64::MAX, u64::MAX),
            replica: (9_300 * ms, 34_300 * ms),
        };
        // A worker still behind with the restart's backlog sends one of
        // them into the compaction, held to its end: the compaction's, and
        // so is its next behind it. Taken as the restart's -- the order
        // before -- it was judged against the replica's return, 10.1 s,
        // with the backlog's lag and the compaction's hold in it.
        let mut owed = Some(Class::Restart);
        assert_eq!(
            class(18_900 * ms, 34_300 * ms, true, false, &w, &mut owed),
            Class::ReplicaCompact
        );
        assert_eq!(
            class(19_000 * ms, 34_310 * ms, false, false, &w, &mut owed),
            Class::ReplicaCompact
        );
        // A primary operation of the backlog, its worker held by none: the
        // restart's.
        let mut owed = Some(Class::Restart);
        assert_eq!(
            class(18_900 * ms, 19_100 * ms, false, false, &w, &mut owed),
            Class::Restart
        );
        // And a read through the replica after its compaction: the
        // restart's still.
        assert_eq!(
            class(35_000 * ms, 35_400 * ms, true, false, &w, &mut owed),
            Class::Restart
        );
    }

    #[test]
    fn the_replicas_compactions_leave_the_compacts_figures_and_an_empty_one_writes_none() {
        let mut kept = Kept::new();
        let mut h = Hists::new();
        for _ in 0..100 {
            h.record(Class::Compact, false, false, false, 5_000_000);
            h.record(Class::Steady, false, false, false, 2_000_000);
        }
        // The replica's compaction holding its reads 25 s.
        for _ in 0..50 {
            h.record(Class::ReplicaCompact, true, false, false, 25_000_000_000);
        }
        let f: HashMap<&str, String> = kept.minute(&h).into_iter().collect();
        let x = |f: &HashMap<&str, String>, k: &str| f[k].parse::<f64>().unwrap();
        assert_eq!(
            (x(&f, "compact_ops"), x(&f, "r_compact_ops")),
            (100.0, 50.0)
        );
        assert!(x(&f, "p99_compact") <= 5_000.0 && x(&f, "p99_compact_hour") <= 5_000.0);
        assert!(x(&f, "p99_r_compact") > 24e6 && x(&f, "p99_r_compact_hour") > 24e6);
        // No restart in the minute: its counts, and no quantile.
        assert_eq!(x(&f, "restart_ops"), 0.0);
        for k in ["p99_restart", "p99_restart_replica", "p99_restart_behind"] {
            assert!(!f.contains_key(k), "{k}");
        }
        // A minute with nothing: no quantile of its own, the hour's kept.
        let f: HashMap<&str, String> = kept.minute(&Hists::new()).into_iter().collect();
        for k in [
            "p99",
            "p99_compact",
            "p99_r_compact",
            "own_p99",
            "p50",
            "max",
        ] {
            assert!(!f.contains_key(k), "{k}");
        }
        assert_eq!(x(&f, "steady_ops"), 0.0);
        assert!(f.contains_key("p99_hour") && f.contains_key("p99_compact_hour"));
        // The end line alike.
        let e: HashMap<&str, String> = kept.end().into_iter().collect();
        assert_eq!((x(&e, "restart_ops"), x(&e, "r_compact_ops")), (0.0, 50.0));
        assert!(!e.contains_key("p99_restart") && !e.contains_key("p99_restart_replica"));
        assert!(x(&e, "p99_r_compact") > 24e6);
        // The hour's figures drop a minute after sixty.
        for _ in 0..60 {
            kept.minute(&Hists::new());
        }
        let f: HashMap<&str, String> = kept.minute(&Hists::new()).into_iter().collect();
        assert!(!f.contains_key("p99_compact_hour") && !f.contains_key("p99_r_compact_hour"));
    }

    /// A worker's operation: `(at, done, via_replica, on_time, took)`.
    type Op = (u64, u64, bool, bool, u64);

    /// The end line's figures of workers' operations, as a run writes them:
    /// each worker's in turn, classed as it ended (`Classes`) while the
    /// replica's window number 1 is not yet settled -- an operation of it
    /// held (`Hists::took`), as `work` holds it -- and settled at the
    /// minute's line as its counter says, `was` one or none; kept over the
    /// run (`Kept`).
    fn written(workers: &[Vec<Op>], w: &Windows, was: bool) -> Vec<(&'static str, f64)> {
        let mut h = Hists::new();
        let unsettled = |_: u64| None;
        for ops in workers {
            let mut classes = Classes::default();
            let mut mine = Hists::new();
            for &(at, done, via, on_time, took) in ops {
                let (c, held) = classes.of(at, done, via, on_time, w, 1, &unsettled);
                let r = Rec {
                    via_replica: via,
                    behind: false,
                    in_restart: at >= w.restart.0 && at <= w.restart.1,
                    took,
                    own_us: 0,
                };
                assert_eq!(mine.took(c, held, r, &unsettled), None);
            }
            mine.settle(&|_| Some(was));
            assert!(mine.held.is_empty());
            h.merge(&mine);
        }
        let mut kept = Kept::new();
        kept.minute(&h);
        kept.end()
            .into_iter()
            .map(|(k, v)| (k, v.parse().unwrap()))
            .collect()
    }

    #[test]
    fn a_restarts_operations_are_judged_by_what_held_them() {
        let s = 1_000_000_000u64;
        let ms = 1_000_000u64;
        let verdict = |text: &str, c: &str| -> (bool, String) {
            let j = judge(text, None);
            let v = j[0].verdicts.iter().find(|v| v.0 == c).unwrap();
            (v.1, v.2.clone())
        };
        let with = |figures: Vec<(&'static str, f64)>, minute: Vec<(&'static str, f64)>| {
            synthetic(480, &move |m, x| {
                if m == u64::MAX {
                    for (k, v) in &figures {
                        x.insert(k, *v);
                    }
                }
                if m == 4 {
                    for (k, v) in &minute {
                        x.insert(k, *v);
                    }
                }
            })
        };
        // Each restart 20 s, the replica back 3 s after it, within the 5 s
        // grace: the primary's bound 22 s, the replica's 27 s. Reads
        // through the replica due in one, held until it was back, 26 s;
        // the primary's 21 s. One figure for both, as before the two were
        // apart, failed it.
        let restart = Windows {
            restart: (100 * s, 120 * s),
            compact: (u64::MAX, u64::MAX),
            replica: (u64::MAX, u64::MAX),
        };
        let primary: Vec<_> = (0..200)
            .map(|i| (101 * s + i * ms, 122 * s, false, true, 21 * s))
            .collect();
        let reads: Vec<_> = (0..40)
            .map(|i| (101 * s + i * ms, 127 * s, true, false, 26 * s))
            .collect();
        let text = with(written(&[primary, reads], &restart, true), vec![]);
        let (ok, said) = verdict(&text, CRITERIA[13]);
        assert!(ok, "{said}");
        assert!(
            said.contains(" s of 22.0 at the primary and ")
                && said.contains(" s of 27.0 through the replica"),
            "{said}"
        );
        // The replica compacting from 95 s to 135 s as a kill comes: 30
        // workers held by reads through it before the announcement take
        // the operations due in the restart behind them, answered as it
        // ends, 33-36 s from when they were due; ten not held answer the
        // restart's in 2.5 s. The compaction's, held to its 40 s and two:
        // as the restart's, before the replica's compactions were apart,
        // they failed its 22 s.
        let both = Windows {
            restart: (100 * s, 103 * s),
            compact: (u64::MAX, u64::MAX),
            replica: (95 * s, 135 * s),
        };
        let mut workers: Vec<Vec<Op>> = (0..30)
            .map(|_| {
                let mut ops = vec![(99 * s, 135 * s, true, true, 36 * s)];
                ops.extend((0..5).map(|i| {
                    let at = 100 * s + 500 * ms * (i + 1);
                    (at, 135 * s + 10 * ms, false, false, 135 * s - at)
                }));
                ops.push((136 * s, 136 * s + ms, false, true, ms));
                ops
            })
            .collect();
        workers.extend((0..10).map(|i| {
            let at = 100 * s + 200 * ms * (i + 1);
            vec![(at, at + 2_500 * ms, false, true, 2_500 * ms)]
        }));
        let figures = written(&workers, &both, true);
        let get = |k: &str| figures.iter().find(|x| x.0 == k).map(|x| x.1);
        assert_eq!(
            (get("r_compact_ops"), get("restart_ops")),
            (Some(180.0), Some(10.0))
        );
        // Held until the replica's counter settles the window: settled as
        // none -- its counter unmoved -- each is its class otherwise, the
        // reads before the announcement steady and the backlog behind
        // them the restart's.
        let none = written(&workers, &both, false);
        let got = |k: &str| none.iter().find(|x| x.0 == k).map(|x| x.1);
        assert_eq!(
            (
                got("r_compact_ops"),
                got("restart_ops"),
                got("restart_replica_ops")
            ),
            (Some(0.0), Some(160.0), Some(0.0))
        );
        let watched = vec![
            ("r_compact_ms", 40_000.0),
            ("r_auto_compacts", 1.0),
            ("r_compacts_watched", 1.0),
            ("restart_r_compact_ops", 150.0),
        ];
        let text = with(figures, watched);
        for c in [CRITERIA[13], CRITERIA[20], CRITERIA[21]] {
            let (ok, said) = verdict(&text, c);
            assert!(ok, "{c}: {said}");
        }
        let (_, said) = verdict(&text, CRITERIA[20]);
        assert!(
            said.contains("p99 of the 180 operations they held ")
                && said.contains(" s of 42.0 (the longest watched, 40.0 s, and two); 150 of them due in a restart"),
            "{said}"
        );
        let (_, said) = verdict(&text, CRITERIA[13]);
        assert!(
            said.contains("p99 of the operations due in one 2.5 s of 22.0"),
            "{said}"
        );
    }

    #[test]
    fn a_session_is_held_to_its_time_within_a_second() {
        let o = opts();
        assert_eq!(o.slack, 1);
        let s = 1_000_000_000_000;
        let mut c = Checker::new(
            100,
            kills(),
            o.ttl * 1_000_000_000,
            o.slack * 1_000_000_000,
            &[],
        );
        c.take(vec![Ev::SessionPut {
            id: 1,
            s,
            e: s + 10_000_000,
            unanswered: false,
            seq: None,
        }]);
        // Found two seconds past its time: the clocks' drift is a step a
        // minute's probe would mark, not five seconds of slack.
        let past = s + 10_000_000 + (o.ttl + 2) * 1_000_000_000;
        c.take(vec![Ev::Session {
            id: 1,
            s: past,
            e: past + 10_000_000,
            found: true,
        }]);
        c.hold(u64::MAX, 0, u64::MAX, &[]);
        assert_eq!(c.mismatches, 1);
        let p = |off_ms, rtt_ms| Probe {
            off_ms,
            rtt_ms,
            rtt_ns: 0,
            vm_less_ns: 0,
            at: 0,
        };
        assert!(!clock_stepped(p(3.0, 2.0), p(3.5, 2.0)));
        assert!(clock_stepped(p(3.0, 2.0), p(400.0, 2.0)));
        // A round trip as long as the move: no step it can tell.
        assert!(!clock_stepped(p(3.0, 2.0), p(300.0, 400.0)));
    }

    #[test]
    fn a_look_and_a_sampler_are_read_as_written() {
        let out = "anon 1000\nfile 2000\npgmajfault 3\ncurrent 4000\noom_kill 0\n\
                   cpu_usec 123456\nThreads:\t41\nRssAnon:\t  2048 kB\nfds 77\nmajflt 5\n\
                   filesize 999\ncompacting 1\ncpu  10 1 20 300 9 1 2 0 0 0\nvmfree 4096\n";
        let l = Look::parse(out).unwrap();
        assert_eq!(
            (
                l.threads,
                l.rss_anon,
                l.fds,
                l.file,
                l.compacting,
                l.cpu_usec
            ),
            (41, 2 << 20, 77, 999, true, Some(123_456))
        );
        assert_eq!(l.vm_busy, Some(34));
        let lines = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            in_sync(&lines(&[
                "A aarch64",
                "S 100.00 98",
                "S 100.50 82",
                "T 100.70"
            ])),
            Some(200)
        );
        assert_eq!(
            in_sync(&lines(&["A x86_64", "S 5.00 74", "S 5.10 running"])),
            Some(0)
        );
        assert_eq!(in_sync(&lines(&["A aarch64"])), None);
    }

    #[test]
    fn a_read_after_a_kill_names_how_long_before_it_a_write_it_lost_was_answered() {
        let ms = 1_000_000;
        let kills = Kills {
            at: vec![(5_000 * ms, 9_000 * ms)],
            loss: 650 * ms,
        };
        // Answered 400 ms before the kill and gone: within the bound,
        // counted and its gap kept.
        let mut c = Checker::new(100, kills.clone(), 60_000 * ms, 1_000 * ms, &[]);
        c.take(vec![write(7, 3, 42, 4_000 * ms, 4_600 * ms, None)]);
        c.take(vec![row(7, 10_000 * ms, 10_100 * ms, base_digests(7))]);
        c.hold(20_000 * ms, 0, 20_000 * ms, &[]);
        assert_eq!(
            (c.mismatches, c.lost_writes, c.lost_ms_max),
            (0, 1, Some(400))
        );
        // Answered 1 000 ms before it and gone: past the bound, a mismatch
        // that says by how much.
        let mut c = Checker::new(100, kills.clone(), 60_000 * ms, 1_000 * ms, &[]);
        c.take(vec![write(7, 3, 42, 3_900 * ms, 4_000 * ms, None)]);
        c.take(vec![row(7, 10_000 * ms, 10_100 * ms, base_digests(7))]);
        c.hold(20_000 * ms, 0, 20_000 * ms, &[]);
        assert_eq!(c.mismatches, 1);
        assert!(
            c.examples[0]
                .contains("answered 1000 ms before a kill or a death is gone, past the 650 ms"),
            "{}",
            c.examples[0]
        );
        // A fraction of a ms past the bound is past it, and reads so: the
        // gap rounded up, never down to the bound itself -- which the
        // report would have read as a run supporting it.
        let mut c = Checker::new(100, kills.clone(), 60_000 * ms, 1_000 * ms, &[]);
        let answered = 5_000 * ms - 650 * ms - 400_000;
        c.take(vec![write(7, 3, 42, 4_000 * ms, answered, None)]);
        c.take(vec![row(7, 10_000 * ms, 10_100 * ms, base_digests(7))]);
        c.hold(20_000 * ms, 0, 20_000 * ms, &[]);
        assert_eq!((c.mismatches, c.lost_ms_max), (1, Some(651)));
        assert!(
            c.examples[0]
                .contains("answered 651 ms before a kill or a death is gone, past the 650 ms"),
            "{}",
            c.examples[0]
        );
        // At the bound exactly: within it, and 650.
        let mut c = Checker::new(100, kills, 60_000 * ms, 1_000 * ms, &[]);
        c.take(vec![write(7, 3, 42, 4_000 * ms, 4_350 * ms, None)]);
        c.take(vec![row(7, 10_000 * ms, 10_100 * ms, base_digests(7))]);
        c.hold(20_000 * ms, 0, 20_000 * ms, &[]);
        assert_eq!((c.mismatches, c.lost_ms_max), (0, Some(650)));
        let n = judge(
            &synthetic(480, &|m, x| {
                if m == 31 {
                    x.insert("lost_writes", 1.0);
                    x.insert("lost_ms_max", 651.0);
                }
            }),
            None,
        )[0]
        .notes
        .join("\n");
        assert!(n.contains("651 ms before it, past the bound"), "{n}");
    }

    #[test]
    fn the_primarys_reads_never_go_back_and_the_replicas_may_lag_them() {
        // A write in flight: the primary answered its value, then the old
        // one -- a read going back, which an atomic register never does.
        let mut c = Checker::new(100, kills(), 60, 1, &[]);
        c.take(vec![
            write(7, 3, 42, 10, 100, None),
            row(7, 20, 30, with(7, 3, 42)),
            row(7, 40, 50, base_digests(7)),
        ]);
        c.hold(200, 0, 200, &[]);
        assert_eq!(c.mismatches, 1);
        // The replica, sent after the writes answered before it began, may
        // not hold this one yet: held to the registers alone.
        let mut c = Checker::new(100, kills(), 60, 1, &[]);
        let through = Ev::Row {
            key: 7,
            s: 40,
            e: 50,
            found: true,
            got: base_digests(7),
            replica: true,
        };
        c.take(vec![
            write(7, 3, 42, 10, 100, None),
            row(7, 20, 30, with(7, 3, 42)),
            through,
        ]);
        c.hold(200, 0, 200, &[]);
        assert_eq!(c.mismatches, 0);
    }

    #[test]
    fn a_backup_whose_restored_file_holds_a_record_no_write_left_is_counted() {
        let mut c = Checker::new(100, kills(), 60, 1, &[]);
        c.take(vec![write(7, 3, 42, 10, 20, None)]);
        let restored = |got| Ev::Restored {
            backup: 1,
            key: 7,
            s: 30,
            e: 40,
            found: true,
            got,
        };
        c.take(vec![
            restored(with(7, 3, 42)),
            restored(base_digests(7)),
            restored(base_digests(7)),
        ]);
        c.hold(100, 100, 100, &[]);
        assert_eq!((c.mismatches, c.backups_differ), (2, 1));
    }

    #[test]
    fn a_fence_ends_the_run_naming_the_side_job_it_waited_for() {
        let doing = Mutex::new("the compact asked at minute 40".to_string());
        let panicked = |f: &dyn Fn()| -> String {
            let e = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_err();
            e.downcast::<String>().map(|s| *s).unwrap_or_default()
        };
        // A side thread that answers: the fence returns.
        let (jobs, rx) = mpsc::channel::<Job>();
        let side = std::thread::spawn(move || {
            for job in rx {
                if let Job::Fence(done) = job {
                    let _ = done.send(());
                }
            }
        });
        fence(&jobs, &doing, Duration::from_secs(60), "the end");
        drop(jobs);
        side.join().unwrap();
        // One whose job does not end: the waiter and the job named, within
        // the bound.
        let (jobs, rx) = mpsc::channel::<Job>();
        let t = Instant::now();
        let said = panicked(&|| fence(&jobs, &doing, Duration::from_millis(50), "the end"));
        assert!(t.elapsed() < Duration::from_secs(30));
        assert!(
            said.starts_with(
                "the end waited 0 s for the side job, the compact asked at minute 40,"
            ),
            "{said}"
        );
        drop(rx);
        // A side thread that ended before its queue: said so, never waited
        // on.
        let said = panicked(&|| fence(&jobs, &doing, Duration::from_secs(60), "the end"));
        assert!(said.contains("the side jobs' thread ended"), "{said}");
    }

    #[test]
    fn a_backup_counts_only_where_its_restored_change_is_the_primarys_at_its_start() {
        let (taken, restored) = (true, true);
        // At or past the primary's change when the backup began: counted.
        for seq in [100, 250] {
            assert_eq!(
                backup_held(taken, restored, Some(100), Some(seq), 1_000),
                (true, false)
            );
        }
        // Behind it -- a stale archive, an empty one -- or either number
        // unread: restored and verified all the same, stale and failed.
        for (seq0, seq) in [
            (Some(100), Some(99)),
            (Some(100), Some(0)),
            (None, Some(100)),
            (Some(100), None),
        ] {
            assert_eq!(
                backup_held(taken, restored, seq0, seq, 1_000),
                (false, true),
                "{seq0:?} {seq:?}"
            );
        }
        // Not restored: failed, and no question of stale; nothing read
        // back from it: failed.
        assert_eq!(
            backup_held(taken, false, Some(100), None, 0),
            (false, false)
        );
        assert_eq!(
            backup_held(taken, restored, Some(100), Some(100), 0),
            (false, false)
        );
        assert_eq!(
            backup_held(false, restored, Some(100), Some(100), 1_000),
            (false, false)
        );
    }

    #[test]
    fn a_primary_whose_lives_are_mostly_compactions_is_not_judged_and_says_so() {
        // The server's own compactions in two minutes of three: no life
        // keeps ten quiet looks from its fifth line.
        let text = synthetic(480, &|m, x| {
            if m > 0 && m < u64::MAX && m % 3 != 0 {
                x.insert("auto_compacts", 1.0);
            }
        });
        let j = judge(&text, None);
        let (_, pass, said) = &j[0].verdicts[5];
        assert!(!pass);
        assert!(
            said.starts_with("NOT JUDGED, too few quiet looks: no life of the primary had ten"),
            "{said}"
        );
        assert!(said.contains("life 0 "), "{said}");
        assert!(said.contains("in the server's own, "), "{said}");
        // A run with them judged says how many of how many.
        let j = judge(&synthetic(480, &|_, _| {}), None);
        let (_, pass, said) = &j[0].verdicts[5];
        assert!(pass, "{said}");
        assert!(
            said.starts_with("16 of 16 lives of ten quiet looks"),
            "{said}"
        );
    }

    /// The primary's memory beside what it holds: its replication feed,
    /// empty at each open, filling with the file's appends -- 3 MB a
    /// minute, records of 800 bytes -- to its 64 MiB and its index, and its
    /// engine's own, a sawtooth. The life the records were loaded in holds
    /// it full from its first line. Held to a flat line, as the dry run of
    /// 2026-11-22 was, a life grew some +100%; beyond what it holds,
    /// nothing -- and a leak of half a MB a minute on top of it fails.
    #[test]
    fn a_primarys_memory_is_judged_beyond_what_it_holds() {
        let cap = 64.0 * 1048576.0;
        let run = |leak: f64| {
            synthetic(480, &move |m, x| {
                if m == 0 || m == u64::MAX {
                    return;
                }
                // Lines 30k + 1 to 30k + 30 are life k's (`synthetic`).
                let (life, i) = ((m - 1) / 30, ((m - 1) % 30) as f64);
                let compacts = ((m - 1) / 20) as f64;
                x.insert("file", 1.0e9 + 3e6 * m as f64 - 50e6 * compacts);
                x.insert("seq", 3750.0 * m as f64);
                let engine = 30e6 + 2e6 * ((m % 4) as f64);
                x.insert("memory_bytes", engine);
                let held = match life {
                    0 => cap,
                    _ => (3e6 * (i + 1.0)).min(cap),
                };
                let feed = held * (1.0 + 3750.0 / 3e6 * 48.0);
                x.insert("rss_anon", 15e6 + engine + feed + leak * i);
            })
        };
        let j = judge(&run(0.0), None);
        let (_, pass, said) = &j[0].verdicts[5];
        assert!(pass, "{said}");
        assert!(said.starts_with("16 of 16 lives"), "{said}");
        // Its anonymous memory alone grew far past 5% in a life.
        let text = run(0.0);
        let anon = |m: u64| -> f64 {
            let cell = format!("\t250:50:m{m:04}\trss_anon\t");
            let l = text.lines().find(|l| l.contains(&cell)).unwrap();
            l.rsplit('\t').next().unwrap().parse().unwrap()
        };
        assert!(anon(58) > 1.5 * anon(36), "{} {}", anon(36), anon(58));
        let j = judge(&run(0.5e6), None);
        let (_, pass, said) = &j[0].verdicts[5];
        assert!(!pass, "{said}");
        // What the feed holds, line by line: its appends up to its bound,
        // a compacted minute at the life's median, and its index.
        let lines: Vec<HashMap<String, f64>> = [
            (3e6, 0.0, 0.0),
            (6e6, 3750.0, 0.0),
            (9e6, 7500.0, 0.0),
            (5e6, 11250.0, 1.0),
            (8e6, 15000.0, 0.0),
        ]
        .iter()
        .map(|&(file, seq, compact)| {
            [("file", file), ("seq", seq), ("compact", compact)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect()
        })
        .collect();
        let xs: Vec<Line> = lines
            .iter()
            .enumerate()
            .map(|(i, m)| (i as u64, m))
            .collect();
        let index = 1.0 + 3750.0 / 3e6 * 48.0;
        let held = feed_held(&xs, 1e9, false);
        for (got, want) in held.iter().zip([3e6, 6e6, 9e6, 12e6, 15e6]) {
            assert!((got - want * index).abs() < 1.0, "{held:?}");
        }
        // Bounded, and full at once where the records were loaded.
        assert!(feed_held(&xs, 7e6, false)[4] <= 7e6 * index + 1.0);
        assert!((feed_held(&xs, 2e6, true)[0] - 2e6 * index).abs() < 1.0);
    }

    #[test]
    fn the_report_says_what_the_run_suggests_of_each_bound() {
        let notes = |tweak: &dyn Fn(u64, &mut HashMap<&'static str, f64>)| {
            judge(&synthetic(480, tweak), None)[0].notes.join("\n")
        };
        let n = notes(&|_, _| {});
        assert!(n.contains("no write a read showed gone"), "{n}");
        assert!(
            n.contains("the run supports the bound (twice the longest is 40 s)"),
            "{n}"
        );
        assert!(
            n.contains("minute 90 (a compact writing, no fsync in flight)"),
            "{n}"
        );
        assert!(n.contains("the load 1.0 before the run"), "{n}");
        let n = notes(&|m, x| {
            if m == 31 {
                x.insert("lost_writes", 2.0);
                x.insert("lost_ms_max", 412.0);
            }
            if m == 61 {
                x.insert("restart_ms", 70_000.0);
            }
        });
        assert!(
            n.contains("the earliest answered 412 ms before it -- the run supports the bound"),
            "{n}"
        );
        assert!(n.contains("the run suggests --restart-max 140"), "{n}");
        let n = notes(&|m, x| {
            if m == 31 {
                x.insert("lost_writes", 1.0);
                x.insert("lost_ms_max", 870.0);
            }
        });
        assert!(n.contains("the run suggests --loss-ms 900"), "{n}");
        let n = notes(&|m, x| {
            if m == 118 {
                x.insert("r_auto_compacts", 1.0);
                x.insert("r_compacts_watched", 1.0);
                x.insert("r_compact_ops", 4_200.0);
            }
            if m == 200 {
                x.insert("r_watch", 0.0);
            }
        });
        assert!(
            n.contains("the replica's own compactions: 1 by its counter, 1 seen at its compaction thread (not watched at minutes 200); 4200 operations held by them, 0 of them due in a restart"),
            "{n}"
        );
    }

    /// The fourth dry run of 2026-11-24, 120 minutes at 1 240 a second
    /// under `--sync 250`, its kills' figures as its lines wrote them: the
    /// restarts 2.9, 2.0 and 2.5 s, the replica back 4.4, 6.1 and 5.4 s
    /// after each, the loop 0.0, 0.4 and 0.0 s behind at each announcement,
    /// and its end line's `p99_restart` of 5.07 s over 19 719 operations.
    fn fourth_dry_run(split: Option<(f64, f64, f64)>) -> String {
        let tweak = move |m: u64, x: &mut HashMap<&'static str, f64>| {
            let kill = |x: &mut HashMap<&'static str, f64>, f: [f64; 6]| {
                for (k, v) in [
                    "restart_ms",
                    "start_ms",
                    "kill_ms",
                    "verify_ms",
                    "replica_back_ms",
                    "kill_behind_s",
                ]
                .into_iter()
                .zip(f)
                {
                    x.insert(k, v);
                }
            };
            match m {
                31 => kill(x, [2_912.0, 621.0, 319.0, 1_945.0, 4_399.0, 0.0]),
                61 => kill(x, [2_004.0, 275.0, 310.0, 1_394.0, 6_083.0, 0.4]),
                91 => kill(x, [2_512.0, 519.0, 330.0, 1_630.0, 5_448.0, 0.0]),
                u64::MAX => {
                    x.insert("p99_restart", 5_066_719.0);
                    x.insert("restart_ops", 19_719.0);
                    x.insert("p99_restart_behind", 0.0);
                    x.insert("restart_behind_ops", 0.0);
                    x.insert("restart_ms_max", 2_912.0);
                    x.remove("p99_restart_replica");
                    x.remove("restart_replica_ops");
                    if let Some((p, rp, rn)) = split {
                        x.insert("p99_restart", p);
                        x.insert("p99_restart_replica", rp);
                        x.insert("restart_replica_ops", rn);
                    }
                }
                _ => {}
            }
        };
        synthetic(120, &tweak)
    }

    #[test]
    fn the_fourth_dry_runs_restarts_under_the_split_verdict() {
        let restart = |text: &str| -> (bool, String) {
            let j = judge(text, None);
            let v = j[0].verdicts.iter().find(|v| v.0 == CRITERIA[13]).unwrap();
            (v.1, v.2.clone())
        };
        // Its own end line: one figure for every operation due in a
        // restart, the reads through the replica among them, held to the
        // primary's bound -- the longest restart, 2.9 s, and two. Still a
        // failure: its minute 61's operations waited for the replica's
        // compaction, which only a run that classes them so can say.
        let (ok, said) = restart(&fourth_dry_run(None));
        assert!(!ok, "{said}");
        assert!(
            said.contains("p99 of the operations due in one 5.1 s of 4.9, the reads through the replica among them (no figure of their own on this run's lines), over the 3 kills the loop met on its schedule"),
            "{said}"
        );
        // The replica's bound: from each kill until reads go through it
        // again -- its restart and the longer of the replica's return and
        // the grace, 2.0 + 6.1 s at minute 61 -- and two.
        let (ok, said) = restart(&fourth_dry_run(Some((2_868_904.0, 0.0, 0.0))));
        assert!(ok, "{said}");
        assert!(
            said.contains("p99 of the operations due in one 2.9 s of 4.9 at the primary and none through the replica (its bound 10.1)"),
            "{said}"
        );
        // Reads through the replica due in a restart held 9.5 s: within
        // its bound, past the primary's; 10.5 s past both.
        let (ok, said) = restart(&fourth_dry_run(Some((2_868_904.0, 9_500_000.0, 25.0))));
        assert!(ok, "{said}");
        assert!(
            said.contains("and 9.5 s of 10.1 through the replica"),
            "{said}"
        );
        let (ok, said) = restart(&fourth_dry_run(Some((2_868_904.0, 10_500_000.0, 25.0))));
        assert!(!ok, "{said}");
        // The primary's past its bound fails whatever the replica's.
        let (ok, _) = restart(&fourth_dry_run(Some((5_066_719.0, 0.0, 0.0))));
        assert!(!ok);
        // Its lines are from before the watch on the replica's compaction
        // thread: the replica compacted in its lines 61 and 118 by its
        // counter, and no line says whether a watch looked -- not watched,
        // and no figure for the reads its compactions held.
        let before_the_watch = synthetic(120, &|m, x| {
            x.remove("r_watch");
            if m == 61 || m == 118 {
                x.insert("r_auto_compacts", 1.0);
            }
        });
        let j = judge(&before_the_watch, None);
        let said = |c: &str| j[0].verdicts.iter().find(|v| v.0 == c).unwrap().clone();
        let (_, ok, why) = said(CRITERIA[20]);
        assert!(
            ok && why == "no figure of their own on this run's lines: nothing judged",
            "{why}"
        );
        let (_, ok, why) = said(CRITERIA[21]);
        assert!(!ok, "{why}");
        assert_eq!(
            why,
            "not watched: no line says whether the watch looked; 2 by its counter"
        );
    }

    #[test]
    fn a_held_operation_is_32_bytes() {
        // `HELD_MAX`'s comment and the design file count 4 MiB a worker.
        assert_eq!(std::mem::size_of::<Held>(), 32);
    }

    #[test]
    fn a_restart_that_meets_a_replicas_compaction_is_paired_with_it() {
        let s = 1_000_000_000u64;
        let ms = 1_000_000u64;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let n = |x: &AtomicU64| x.load(Ordering::Relaxed);
        let moved = |moved| Count::Read {
            moved,
            stale: false,
        };
        // The fifth dry run's minute 60: the replica compacting from 13 s
        // before the kill, its restart over -- reads through the replica
        // again -- 19 s after its announcement, the compaction 1.5 s
        // later: paired as the window ends, its length and the restart's.
        replica_compaction(&sh, Some(Edge::Began(3_587 * s)), Count::Unread);
        restart_met(&sh, Some((3_600 * s, 3_619 * s)), None);
        assert_eq!(n(&sh.r_kill_ms), 0);
        replica_compaction(&sh, Some(Edge::Ended(3_620 * s + 500 * ms)), moved(true));
        assert_eq!((n(&sh.r_compact_ms), n(&sh.r_kill_ms)), (33_500, 52_500));
        // A window over before the restart is: paired as the restart ends.
        sh.r_kill_ms.store(0, Ordering::Relaxed);
        replica_compaction(&sh, Some(Edge::Began(5_400 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(5_420 * s)), moved(true));
        assert_eq!(n(&sh.r_kill_ms), 0);
        restart_met(&sh, Some((5_410 * s, 5_440 * s)), None);
        assert_eq!(n(&sh.r_kill_ms), 50_000);
        // A restart after the latest window, a window after the latest
        // restart, and a window its counter did not move in: none.
        sh.r_kill_ms.store(0, Ordering::Relaxed);
        restart_met(&sh, Some((5_500 * s, 5_520 * s)), None);
        replica_compaction(&sh, Some(Edge::Began(5_600 * s)), Count::Unread);
        replica_compaction(&sh, Some(Edge::Ended(5_610 * s)), moved(true));
        replica_compaction(&sh, Some(Edge::Began(5_700 * s)), Count::Unread);
        restart_met(&sh, Some((5_705 * s, 5_720 * s)), None);
        replica_compaction(&sh, Some(Edge::Ended(5_730 * s)), moved(false));
        assert_eq!(n(&sh.r_kill_ms), 0);
    }

    /// The fifth dry run of 2026-11-24's lines as they bear on its
    /// restarts and the replica's own compactions, the rest the synthetic
    /// run's: its kills of minutes 30, 60 and 90 -- restarts of 3.6, 6.0
    /// and 3.5 s, the replica back 3.5, 11.9 and 7.8 s after each, the loop
    /// 0.1, 15.9 and 0.0 s behind at each announcement, the replica
    /// compacting through the second, all 13 888 of its operations the
    /// compaction's -- and the replica's compactions ending on its lines 20,
    /// 41, 61, 80 and 109 (24.7, 43.4, 33.5, 23.9 and 22.5 s), and a window
    /// its counter did not move in on line 91. Its end line's `p99_restart`
    /// was 3.57 s over 27 785 operations, the compactions' p99 20.8 s over
    /// 539 816. `kill_ms` is the figure a run writes now for the window
    /// the kill of minute 60 landed in (`r_compact_kill_ms`), its length
    /// and the restart's: its lines have none.
    fn fifth_dry_run(p99_restart: f64, p99_rc: f64, kill_ms: Option<f64>) -> String {
        let tweak = move |m: u64, x: &mut HashMap<&'static str, f64>| {
            if m > 0 && m < u64::MAX {
                for k in [
                    "restart_ops",
                    "restart_replica_ops",
                    "restart_behind_ops",
                    "restart_r_compact_ops",
                ] {
                    x.insert(k, 0.0);
                }
            }
            let kill = |x: &mut HashMap<&'static str, f64>, f: [f64; 5]| {
                for (k, v) in [
                    "restart_ms",
                    "replica_back_ms",
                    "kill_behind_s",
                    "restart_ops",
                    "restart_r_compact_ops",
                ]
                .into_iter()
                .zip(f)
                {
                    x.insert(k, v);
                }
            };
            let window = |x: &mut HashMap<&'static str, f64>, ms: f64| {
                x.insert("r_compact_ms", ms);
                x.insert("r_auto_compacts", 1.0);
                x.insert("r_compacts_watched", 1.0);
            };
            match m {
                20 => window(x, 24_701.0),
                31 => kill(x, [3_626.0, 3_521.0, 0.1, 13_704.0, 0.0]),
                41 => window(x, 43_410.0),
                61 => {
                    kill(x, [5_971.0, 11_907.0, 15.9, 0.0, 13_888.0]);
                    window(x, 33_502.0);
                    if let Some(k) = kill_ms {
                        x.insert("r_compact_kill_ms", k);
                    }
                }
                80 => window(x, 23_858.0),
                91 => {
                    kill(x, [3_504.0, 7_770.0, 0.0, 14_081.0, 0.0]);
                    x.insert("r_compacts_false", 1.0);
                }
                109 => window(x, 22_480.0),
                u64::MAX => {
                    x.insert("p99_restart", p99_restart);
                    x.insert("restart_ops", 27_785.0);
                    x.insert("restart_behind_ops", 0.0);
                    x.insert("restart_ms_max", 5_971.0);
                    x.insert("r_compact_ops", 539_816.0);
                    x.insert("p99_r_compact", p99_rc);
                    x.insert("r_held_max", 2_310.0);
                }
                _ => {}
            }
        };
        synthetic(120, &tweak)
    }

    #[test]
    fn the_fifth_dry_runs_kill_in_the_replicas_compaction() {
        let said = |text: &str, c: &str| -> (bool, String) {
            let j = judge(text, None);
            let v = j[0].verdicts.iter().find(|v| v.0 == c).unwrap();
            (v.1, v.2.clone())
        };
        // The kill of minute 60, met behind the schedule, every operation
        // due in it the compaction's: said so, with their count, where its
        // line had "p99 ?"; and its restart of 6.0 s sets no bound, the
        // others' operations held to their own restarts' -- 3.6 s and two,
        // the replica's 3.5 and 7.8 s and two -- not to 8.0 and 19.9.
        let run = fifth_dry_run(3_573_547.0, 20_803_748.0, None);
        let (ok, why) = said(&run, CRITERIA[13]);
        assert!(ok, "{why}");
        assert!(
            why.contains("p99 of the operations due in one 3.6 s of 5.6 at the primary and none through the replica (its bound 13.3), over the 2 kills the loop met on its schedule; the restart at minute 61 sets no bound, none of the operations due in it among them"),
            "{why}"
        );
        assert!(
            why.contains("; 1 met behind it, at minutes 61 (16 s at the most), every operation due in it gone to the replica's own compaction, 13888 of them, judged with it, and none to the backlog's figure; "),
            "{why}"
        );
        assert!(
            !why.contains("p99 ?") && !why.contains("due in one the replica's own"),
            "{why}"
        );
        // A p99 of 6.0 s: past the others' restarts and two, which 8.0
        // passed.
        let (ok, why) = said(&fifth_dry_run(6e6, 20_803_748.0, None), CRITERIA[13]);
        assert!(!ok, "{why}");
        // The compactions' reads, on its own lines: held to the longest
        // watched, 43.4 s, and two.
        let (ok, why) = said(&run, CRITERIA[20]);
        assert!(ok, "{why}");
        assert!(
            why.contains("p99 of the 539816 operations they held 20.8 s of 45.4 (the longest watched, 43.4 s, and two); 13888 of them due in a restart"),
            "{why}"
        );
        // Reads held 50 s: past it; but the window the kill landed in held
        // them 33.5 s and the restart's 18.9 s more, until reads went
        // through the replica again -- 52.4 s, and two, where the run says
        // so.
        let (ok, _) = said(&fifth_dry_run(3_573_547.0, 50e6, None), CRITERIA[20]);
        assert!(!ok);
        let (ok, why) = said(
            &fifth_dry_run(3_573_547.0, 50e6, Some(52_380.0)),
            CRITERIA[20],
        );
        assert!(ok, "{why}");
        assert!(
            why.contains("50.0 s of 54.4 (the longest watched with the restart of a kill that landed in it, 52.4 s, and two; the longest alone 43.4 s)"),
            "{why}"
        );
        // The watch: five by its counter, five seen, one window its
        // counter did not move in; and the most a worker held, of the most
        // it may.
        let (ok, why) = said(&run, CRITERIA[21]);
        assert!(ok, "{why}");
        assert_eq!(
            why,
            "5 by its counter, 5 seen to end at its compaction thread; 1 window its counter did not move in, no compaction"
        );
        let j = judge(&run, None);
        assert!(
            j[0].notes.iter().any(|n| n.ends_with(
                "; a worker held 2310 operations for one before its verdict at the most, of the 131072 it may (1.8%)"
            )),
            "{:?}",
            j[0].notes
        );
    }

    #[test]
    fn dockers_times_and_exits_are_read_as_written() {
        let s = 1_000_000_000u64;
        // FinishedAt to the ns, and the long run's death as Docker's
        // events said it, to the tenth.
        assert_eq!(
            unix_ns("2026-11-26T00:56:47.123456789Z"),
            Some(1_795_654_607 * s + 123_456_789)
        );
        assert_eq!(
            unix_ns("2026-11-26T00:56:47.1Z"),
            Some(1_795_654_607 * s + 100_000_000)
        );
        assert_eq!(unix_ns("2000-02-29T23:59:59Z"), Some(951_868_799 * s));
        assert_eq!(unix_ns("1970-01-01T00:00:00+00:00"), Some(0));
        // A container that never ended, and a zone not UTC's: none.
        assert_eq!(unix_ns("0001-01-01T00:00:00Z"), None);
        assert_eq!(unix_ns("2026-11-26T01:56:47+01:00"), None);
        assert_eq!(unix_ns("2026-13-26T00:56:47Z"), None);
        assert_eq!(utc(1_795_654_607 * s + 123_456_789), "00:56:47.123Z");
        // `docker inspect`'s line for the long run's primary, and the
        // words its death's line names it by.
        let x = Exit::parse("false true 137 2026-11-26T00:56:47.1Z\n").unwrap();
        assert_eq!(
            x,
            Exit {
                running: false,
                oom: true,
                code: Some(137),
                finished: Some(1_795_654_607 * s + 100_000_000),
            }
        );
        assert_eq!(x.said("512m"), "OOM-killed under --memory 512m (exit 137)");
        let killed = Exit::parse("false false 137 2026-11-26T00:56:47Z").unwrap();
        assert_eq!(
            killed.said("512m"),
            "killed by signal 9 (exit 137), not by the kernel for its memory"
        );
        let exited = Exit::parse("false false 101 2026-11-26T00:56:47Z").unwrap();
        assert_eq!(exited.said("512m"), "exited with code 101");
        let up = Exit::parse("true false 0 0001-01-01T00:00:00Z").unwrap();
        assert!(up.running && up.finished.is_none());
        assert_eq!(Exit::parse(""), None);
        // Docker's events as `follow_events` asks them, the server's alone.
        let server = "p6-50-server";
        assert_eq!(
            DockerEv::parse("1795654607100000000 die p6-50-server 137", 5, server),
            Some(DockerEv {
                at: 5,
                vm: 1_795_654_607 * s + 100_000_000,
                die: true,
                exit: Some(137),
            })
        );
        assert_eq!(
            DockerEv::parse("1795654607090000000 oom p6-50-server ", 5, server)
                .map(|e| (e.die, e.exit)),
            Some((false, None))
        );
        assert_eq!(
            DockerEv::parse("1795654607090000000 start p6-50-server ", 5, server),
            None
        );
        // Its watch, whose name a filter of the server's takes too.
        assert_eq!(
            DockerEv::parse("1795654607090000000 die p6-50-server-mem 137", 5, server),
            None
        );
    }

    #[test]
    fn a_death_is_timed_by_its_burst_of_closes() {
        let ms = 1_000_000u64;
        let d = 18_012_300 * ms;
        let docker = Some((d + 15 * ms, Source::Event));
        // The long run's shape: the last answer 3 ms before its burst's
        // first close, nothing answered after it: the burst, Docker's die
        // a reap later beside it.
        let err = 5 * ms;
        assert_eq!(
            death_time(d, d - 3 * ms, docker, err, 0),
            (d, Source::Close, Some(d - 3 * ms))
        );
        // An answer the server wrote before it died, read a few ms after
        // the burst began: the death taken at that answer,
        // never before it -- a time before the last answer would excuse
        // writes the server held longer than `--loss-ms`.
        assert_eq!(
            death_time(d, d + 10 * ms, docker, err, 0),
            (d + 10 * ms, Source::Close, Some(d + 10 * ms))
        );
        for a in [d - 3 * ms, d, d + ms, d + CLOSE_SLOP] {
            let (t, source, _) = death_time(d, a, docker, err, 0);
            assert_eq!(source, Source::Close);
            assert!(t >= a, "a death at {t} before its last answer at {a}");
        }
        // Read answering past the slop -- tens of ms: the review's case,
        // answers 0.9 s after a close that was no death's -- alive then,
        // that close no death's: Docker's time is taken, never before
        // the last answer.
        assert!(CLOSE_SLOP <= 100 * ms);
        for a in [d + CLOSE_SLOP + ms, d + 900 * ms] {
            assert_eq!(
                death_time(d, a, docker, err, 0),
                (a, Source::Event, Some(a))
            );
        }
        // Nothing closed: Docker's, at the latest its reading through the
        // probe allows -- read through a probe the minute took behind a
        // restart's backlog, its round trip 3.4 s, the reading can be 1.7
        // s before the reap: taken as read, a death with no burst was
        // timed before it went, the writes it held excused from there.
        let reap = d + 400 * ms;
        let slow = 3_400 * ms + ms;
        for read in [reap - 1_700 * ms, reap, reap + 1_700 * ms] {
            let (t, source, last) = death_time(u64::MAX, d, Some((read, Source::Event)), slow, 7);
            assert_eq!((source, last), (Source::Event, Some(d)));
            assert!(t >= reap, "a death at {t} before its reap at {reap}");
            assert_eq!(t, read + slow);
        }
        // Its last answer after the reading's latest: that answer.
        assert_eq!(
            death_time(u64::MAX, d, Some((d - 9 * ms, Source::Finished)), err, 7),
            (d, Source::Finished, Some(d))
        );
        // Nor Docker's: the minute's look before, or the last answer
        // after it.
        assert_eq!(death_time(u64::MAX, 0, None, 0, 7), (7, Source::Look, None));
        assert_eq!(
            death_time(u64::MAX, d, None, 0, 7),
            (d, Source::Look, Some(d))
        );
        // How long before the kill due: the long run's came 12.3 s after
        // its kill of minute 300 was due, in the compact it waited for.
        let (said, s) = before_kill(d, Some(300));
        assert_eq!(s.map(|x| (x * 10.0).round() / 10.0), Some(-12.3));
        assert_eq!(
            said,
            "12.3 s after the kill due at minute 300 was due, as it waited for the side jobs"
        );
        assert_eq!(
            before_kill(287 * 60_000 * ms, Some(300)).0,
            "13.0 min before the kill due at minute 300"
        );
        assert_eq!(
            before_kill(17_970_000 * ms, Some(300)).0,
            "30.0 s before the kill due at minute 300"
        );
        assert_eq!(before_kill(d, None).0, "no kill due after it");
    }

    #[test]
    fn a_death_is_its_burst_of_closes_and_one_close_is_none() {
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let ms = 1_000_000u64;
        let c = 60_000 * ms;
        let burst = || sh.burst().0;
        assert_eq!(burst(), u64::MAX);
        // One close is no death's, nor two.
        sh.answered(c - 5 * ms);
        sh.closed(c);
        assert_eq!(burst(), u64::MAX);
        sh.closed(c + 2 * ms);
        assert_eq!(burst(), u64::MAX);
        // The third within the span: a burst, timed by its first, read
        // in any order its threads took the lock in.
        sh.closed(c + ms);
        assert_eq!(sh.burst(), (c, 2 * ms));
        // An answer written before the death, read within the slop, and
        // the death's other closes: the first kept.
        sh.answered(c + 10 * ms);
        sh.closed(c + 12 * ms);
        sh.closed(c + 40 * ms);
        assert_eq!(burst(), c);
        // Read answering past the slop: alive then, the burst no death's,
        // and none read -- let go at the next close.
        sh.answered(c + 900 * ms);
        assert_eq!(burst(), u64::MAX);
        assert_eq!(sh.closed.load(Ordering::SeqCst), c);

        // The review's case: the server's last answer, a stream the
        // server ended 100 ms after it, then a hang of 30 s and an OOM
        // kill. The stream's close alone timed the death, and excused
        // the writes answered 30 s before it went.
        let last = c + 2_000 * ms;
        sh.answered(last);
        sh.closed(last + 100 * ms);
        assert_eq!(burst(), u64::MAX);
        let death = last + 30_000 * ms;
        for t in [death, death + 3 * ms, death + 4 * ms] {
            sh.closed(t);
        }
        assert_eq!(burst(), death);
        assert_eq!(
            death_time(burst(), sh.answered.load(Ordering::SeqCst), None, 0, 0),
            (death, Source::Close, Some(last))
        );
        // Closes spread wider than the span -- a reconnect's refusals, a
        // stream ending now and then -- are each one connection's.
        sh.answered(death + 9_000 * ms);
        let mut t = death + 10_000 * ms;
        for _ in 0..10 {
            sh.closed(t);
            t += BURST_SPAN + ms;
        }
        assert_eq!(burst(), u64::MAX);
        // A close of the living server's just before a hang, within the
        // span of the death's burst: the burst timed by it, but never
        // before the last answer.
        let a = t + 60_000 * ms;
        sh.answered(a);
        for t in [a + 10 * ms, a + 25 * ms, a + 26 * ms] {
            sh.closed(t);
        }
        assert_eq!(burst(), a + 10 * ms);
        assert!(death_time(burst(), a, None, 0, 0).0 >= a);
    }

    #[test]
    fn a_life_s_answers_and_closes_count_from_its_restart() {
        // A death, its burst read, the restart's refused connects as it
        // was down, the server back at `back`; then dead again 8 s after,
        // before any worker answered, a failure past the restart's grace
        // counted between.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let (doings, o) = (Doings::default(), opts());
        let ms = 1_000_000u64;
        let gone = Exit {
            running: false,
            oom: true,
            code: Some(137),
            finished: None,
        };
        let death = 600_000 * ms;
        sh.answered(death - 3 * ms);
        for t in [death, death + ms, death + 2 * ms] {
            sh.closed(t);
        }
        let (_, at, _) = found_dead(&sh, &doings, &o, gone, &mut Vec::new(), 0, None, None);
        assert_eq!(at, death);
        for t in [death + 1_000 * ms, death + 1_001 * ms, death + 1_002 * ms] {
            sh.closed(t);
        }
        let back = death + 4_000 * ms;
        sh.back(back);
        sh.windows.lock().unwrap().last_mut().unwrap().2 = back;
        assert_eq!(sh.burst().0, u64::MAX);
        assert_eq!(sh.answered.load(Ordering::SeqCst), back);
        sh.failed(back + GRACE + 1_000 * ms, true, || {
            "a stream refused".into()
        });
        assert_eq!(sh.errors.load(Ordering::SeqCst), 1);
        let again = back + GRACE + 3_000 * ms;
        for t in [again, again + ms, again + 2 * ms] {
            sh.closed(t);
        }
        // Timed by its own burst, its answer the restart's; the failure
        // before it its own, still an error. Timed by the last death's
        // burst it moved every failure since that death to `down`.
        let (_, at, _) = found_dead(&sh, &doings, &o, gone, &mut Vec::new(), back, None, None);
        assert_eq!(at, again);
        assert_eq!(sh.errors.load(Ordering::SeqCst), 1);
        assert_eq!(sh.down.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_close_is_a_connection_gone_not_a_wait_run_out() {
        // A connect that timed out is no close; one refused, or a
        // connection cut, is.
        use std::io::{Error, ErrorKind};
        assert!(!a_close(&Error::from(ErrorKind::TimedOut)));
        assert!(!a_close(&Error::from(ErrorKind::WouldBlock)));
        assert!(!a_close(&Error::other("no connection")));
        assert!(a_close(&Error::from(ErrorKind::ConnectionRefused)));
        assert!(a_close(&Error::from(ErrorKind::UnexpectedEof)));
    }

    /// A Checker with one kill or death, `at` to `back`, a write answered
    /// at `answered` and a read at `back` showing it gone.
    fn judged(kills: Vec<(u64, u64)>, answered: u64, back: u64) -> Checker {
        let ms = 1_000_000u64;
        let mut c = Checker::new(
            100,
            Kills {
                at: kills,
                loss: 650 * ms,
            },
            60_000 * ms,
            1_000 * ms,
            &[],
        );
        c.take(vec![write(7, 3, 42, answered - 2 * ms, answered, None)]);
        c.take(vec![row(
            7,
            back + 100 * ms,
            back + 101 * ms,
            base_digests(7),
        )]);
        c.hold(60_000 * ms, 0, 60_000 * ms, &[]);
        c
    }

    #[test]
    fn a_death_s_lost_writes_are_judged_from_its_time() {
        let ms = 1_000_000u64;
        // The long run's shape, through what `died` does but its restart
        // (`found_dead`): the primary gone at `death`, OOM-killed, its last
        // answer 3 ms before; found as the kill due at minute 1 was to be
        // sent 1.1 s after -- its fence, then the announced second -- the
        // minute's look 50 s before; the server back 4 s after that.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let (doings, o) = (Doings::default(), opts());
        let death = 50_000 * ms;
        let (look, k) = (death - 50_000 * ms, death + 1_100 * ms);
        let back = k + 4_000 * ms;
        sh.answered(death - 3 * ms);
        for t in [death, death + 2 * ms, death + 5 * ms] {
            sh.closed(t);
        }
        let exit = Exit {
            running: false,
            oom: true,
            code: Some(137),
            finished: None,
        };
        let (from, at, m) =
            found_dead(&sh, &doings, &o, exit, &mut Vec::new(), look, Some(1), None);
        assert_eq!((from, at, m), (death, death, 1));
        assert_eq!(doings.deaths.load(Ordering::SeqCst), 1);
        assert_eq!(doings.oom.load(Ordering::SeqCst), 1);
        // A failure from the death on is `down` (its window open), the
        // restart closes the window as the server answers again.
        assert!(sh.restarting(death));
        sh.windows.lock().unwrap().last_mut().unwrap().2 = back;
        let kills = sh.kills();
        assert_eq!(kills, vec![(death, back)]);
        // Answered 150 ms before the death and gone: within what `--sync
        // 250` allows, judged from the death.
        let c = judged(kills.clone(), death - 150 * ms, back);
        assert_eq!(
            (c.mismatches, c.lost_writes, c.lost_ms_max),
            (0, 1, Some(150))
        );
        // Still held to `--sync`'s bound: one answered 700 ms before the
        // death and gone is a loss past it -- excused were the death timed
        // by the minute's look before it.
        let c = judged(kills, death - 700 * ms, back);
        assert_eq!((c.mismatches, c.lost_ms_max), (1, Some(700)));
        // Judged from the kill the run had due, as the long run judged its
        // seven: 1 250 ms, past the 650 a kill may take -- the harness's
        // own wait counted as the server's loss.
        let c = judged(vec![(k, back)], death - 150 * ms, back);
        assert_eq!(c.mismatches, 1);
        assert!(
            c.examples[0]
                .contains("answered 1250 ms before a kill or a death is gone, past the 650 ms"),
            "{}",
            c.examples[0]
        );
    }

    #[test]
    fn a_kill_that_finds_its_server_gone_before_its_send_is_a_death() {
        let ms = 1_000_000u64;
        let k = 600_000 * ms;
        let err = 5 * ms;
        let none = u64::MAX;
        let gone = |oom| Exit {
            running: false,
            oom,
            code: Some(137),
            finished: None,
        };
        // OOM-killed in the moment before the send, Docker's state still
        // running to the look before it: after the wait, `OOMKilled`,
        // whatever was read.
        assert!(gone_before(
            &gone(true),
            false,
            k + 30 * ms,
            k + 40 * ms,
            Some(k + 200 * ms),
            err,
            k
        ));
        // `docker kill` found it not running.
        assert!(gone_before(&gone(false), true, k, none, None, err, k));
        // On this process's clock: its last answer more than the slop
        // before the send and its exit's burst after that and before the
        // send -- whatever Docker's end says, a step of the VM's clock
        // putting it after the send.
        assert!(gone_before(
            &gone(false),
            false,
            k - 125 * ms,
            k - 120 * ms,
            Some(k + 50 * ms),
            err,
            k
        ));
        // A planned kill: read answering up to its landing, after the
        // send, its burst after -- the kill's, Docker's end before the
        // send by a clock's step or not.
        for finished in [Some(k + 180 * ms), Some(k - 500 * ms), None] {
            assert!(!gone_before(
                &gone(false),
                false,
                k + 20 * ms,
                k + 40 * ms,
                finished,
                err,
                k
            ));
        }
        // Read answering within the slop before the send: the kill's --
        // what the harness cannot tell from it.
        assert!(!gone_before(
            &gone(false),
            false,
            k - CLOSE_SLOP,
            k - 20 * ms,
            Some(k - 40 * ms),
            err,
            k
        ));
        // Silent more than the slop before the send, its burst the kill's
        // own after it: a server the kill found hung, not dead.
        assert!(!gone_before(
            &gone(false),
            false,
            k - 300 * ms,
            k + 40 * ms,
            Some(k - 100 * ms),
            err,
            k
        ));
        // Silent, and no burst read: Docker's end, held to its reading's
        // error.
        let quiet = k - 300 * ms;
        assert!(gone_before(
            &gone(false),
            false,
            quiet,
            none,
            Some(k - 100 * ms),
            err,
            k
        ));
        assert!(!gone_before(
            &gone(false),
            false,
            quiet,
            none,
            Some(k - 4 * ms),
            err,
            k
        ));
        assert!(!gone_before(&gone(false), false, quiet, none, None, err, k));
        // Nothing read at all: likewise.
        assert!(gone_before(
            &gone(false),
            false,
            0,
            none,
            Some(k - 100 * ms),
            err,
            k
        ));
        assert!(!gone_before(&gone(false), false, 0, none, None, err, k));
        // Running still: no end at all.
        let up = Exit {
            running: true,
            ..gone(true)
        };
        assert!(!gone_before(
            &up,
            true,
            0,
            k - 120 * ms,
            Some(k - 500 * ms),
            err,
            k
        ));

        // The review's case through the kill's path (`judge_kill`):
        // announced at `from`, the kill's compact asked 10 ms later, the
        // server OOM-killed in it 950 ms in, the kill sent 50 ms after
        // that: a death, timed by its burst, its window the
        // announcement's.
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        let (doings, o) = (Doings::default(), opts());
        let from = k - 1_000 * ms;
        sh.windows.lock().unwrap().push((from, u64::MAX, u64::MAX));
        let death = from + 950 * ms;
        sh.answered(death - 4 * ms);
        for c in [death, death + ms, death + 2 * ms] {
            sh.closed(c);
        }
        sh.kill_sent.store(k, Ordering::SeqCst);
        let x = gone(true);
        assert_eq!(judge_kill(&sh, Some(x), false, k), Some(x));
        let (_, at, _) = found_dead(
            &sh,
            &doings,
            &o,
            x,
            &mut Vec::new(),
            from,
            Some(10),
            Some(from),
        );
        assert_eq!(at, death);
        sh.windows.lock().unwrap().last_mut().unwrap().2 = k + 4_000 * ms;
        assert_eq!(sh.kills(), vec![(death, k + 4_000 * ms)]);
        assert_eq!(sh.windows.lock().unwrap().len(), 1);
        // The compact it cut, asked before the send and its connection
        // seen to close after `kill_sent` was stored: the death's, the
        // kill never judged its own.
        let quick = Duration::from_millis(20);
        let long = Duration::from_secs(2);
        assert_eq!(
            cut_by(&sh, from + 10 * ms, true, &|| false, quick, long),
            Some(CutBy::Death)
        );
        // One asked after the death -- its connect refused -- and before
        // the send: the death's at once, though no death found is since
        // its ask -- the kill judged a death's (`kill_death`). Unjudged,
        // it waited out the kill's bound and was counted the kill's.
        assert_eq!(
            cut_by(&sh, death + 30 * ms, true, &|| false, quick, long),
            Some(CutBy::Death)
        );
    }

    #[test]
    fn a_kill_is_judged_on_this_process_s_clock_and_docker_s_end_through_a_probe_of_it() {
        let ms = 1_000_000u64;
        let min = 60_000 * ms;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        // The review's run: this host's wall clock, which Docker's VM
        // follows, 60 ms behind this process's by minute 90 -- some -11
        // ppm; this Mac measured -43 over three minutes -- the minute's
        // probe 3 s before each kill is sent, its round trip 4 ms; the
        // planned kill sent at minute 90 and its end, Docker's FinishedAt,
        // 30 ms after.
        let vm = |t: u64| sh.t0_wall + t - t / 90_000;
        let ms_of = |ns: u64| ns as f64 / 1e6;
        let probe = |k: u64| {
            let p = k - 3_000 * ms;
            sh.clock(&Probe::read(
                (vm(p) / ms) as i64,
                (ms_of(vm(p) - 2 * ms), ms_of(vm(p) + 2 * ms)),
                (p - 2 * ms, p + 2 * ms),
            ));
        };
        let k = 90 * min;
        probe(k);
        let ended = |t: u64| Exit {
            running: false,
            oom: false,
            code: Some(137),
            finished: Some(vm(t)),
        };
        // Its end read through the probe, on this process's clock: after
        // the send, within the reading's error -- the round trip, `now()`'s
        // ms, 3 s of drift at 500 ppm.
        let end = sh.vm_ns(vm(k + 30 * ms)).unwrap();
        assert!(
            end > k && end.abs_diff(k + 30 * ms) <= sh.clock_err(k),
            "{end}"
        );
        assert!(sh.clock_err(k) <= 10 * ms);
        // Nothing read of the server -- Docker's end is all there is --
        // and the kill's: read through the wall clock at `t0` it was 30 ms
        // before the send, and a death.
        sh.kill_sent.store(k, Ordering::SeqCst);
        assert_eq!(judge_kill(&sh, Some(ended(k + 30 * ms)), false, k), None);
        assert_eq!(sh.kill_judged.load(Ordering::SeqCst), k);
        assert_eq!(sh.kill_death.load(Ordering::SeqCst), 0);
        let quick = Duration::from_millis(20);
        let long = Duration::from_secs(2);
        assert_eq!(
            cut_by(&sh, k - 500 * ms, true, &|| false, quick, long),
            Some(CutBy::Kill)
        );
        // A death 200 ms before the next kill, at minute 120, nothing read
        // either: Docker's end before the send past the error, a death's.
        let k = 120 * min;
        probe(k);
        sh.kill_sent.store(k, Ordering::SeqCst);
        let x = ended(k - 200 * ms);
        assert_eq!(judge_kill(&sh, Some(x), false, k), Some(x));
        assert_eq!(sh.kill_death.load(Ordering::SeqCst), k);
        assert_eq!(sh.kill_judged.load(Ordering::SeqCst), k);
        assert_eq!(
            cut_by(&sh, k - 500 * ms, true, &|| false, quick, long),
            Some(CutBy::Death)
        );
        // Read of the server, its clock decides, Docker's not asked: a
        // planned kill read answering to 20 ms after the send, its burst
        // after -- a step of the VM's clock putting its end half a second
        // before the send -- the kill's.
        let k = 150 * min;
        probe(k);
        sh.kill_sent.store(k, Ordering::SeqCst);
        sh.answered(k + 20 * ms);
        for c in [k + 40 * ms, k + 41 * ms, k + 43 * ms] {
            sh.closed(c);
        }
        assert_eq!(judge_kill(&sh, Some(ended(k - 500 * ms)), false, k), None);
        assert!(kill_said(&sh, 5, k).ends_with(
            "its server last read answering +20 ms of the send, its burst of closes +40 ms (3 within 3.0 ms)"
        ));
        assert_eq!(
            cut_by(&sh, k - 500 * ms, true, &|| false, quick, long),
            Some(CutBy::Kill)
        );
        // Back, then dead 120 ms before the next send, its last answer 5
        // ms before its burst -- Docker's end, by a step the other way,
        // after the send: the death's.
        sh.back(k + 4_000 * ms);
        let k = 180 * min;
        probe(k);
        sh.kill_sent.store(k, Ordering::SeqCst);
        sh.answered(k - 125 * ms);
        for c in [k - 120 * ms, k - 119 * ms, k - 118 * ms] {
            sh.closed(c);
        }
        let x = ended(k + 50 * ms);
        assert_eq!(judge_kill(&sh, Some(x), false, k), Some(x));
        assert_eq!(
            cut_by(&sh, k - 500 * ms, true, &|| false, quick, long),
            Some(CutBy::Death)
        );
    }

    #[test]
    fn a_reading_through_the_probe_kept_covers_the_drift_since_and_the_tighter_probe_is_kept() {
        let ms = 1_000_000u64;
        let min = 60_000 * ms;
        let sh = Shared::new(&opts(), "127.0.0.1:1".into(), "127.0.0.1:1".into(), &[]);
        // The VM's clock parting from this process's at -43 ppm, as this
        // Mac measured over three minutes; `step` moves it at once.
        let vm = |t: u64| sh.t0_wall + t - t * 43 / 1_000_000;
        let ms_of = |ns: u64| ns as f64 / 1e6;
        let probe = |t: u64, rtt: u64, step: i64| {
            let v = (vm(t) as i64 + step) as u64;
            Probe::read(
                (v / ms) as i64,
                (ms_of(vm(t) - rtt / 2), ms_of(vm(t) + rtt / 2)),
                (t - rtt / 2, t + rtt / 2),
            )
        };
        // A probe at minute 60, its round trip 4 ms, and none after it to
        // the kill of minute 90: the minute's probes failed.
        assert!(sh.clock(&probe(60 * min, 4 * ms, 0)));
        let k = 90 * min;
        // The planned kill's end, 30 ms after its send, read through it
        // 47 ms before the send: the 77 ms the clocks parted since, which
        // the probe's round trip and `now()`'s ms do not cover, and the
        // drift term does -- the kill's, never a death's.
        let end = sh.vm_ns(vm(k + 30 * ms)).unwrap();
        assert!(end + 40 * ms < k, "{end} {k}");
        assert!(end.abs_diff(k + 30 * ms) <= sh.clock_err(k));
        assert!(sh.clock_err(k) > 77 * ms + 5 * ms);
        let ended = |t: u64| Exit {
            running: false,
            oom: false,
            code: Some(137),
            finished: Some(vm(t)),
        };
        sh.kill_sent.store(k, Ordering::SeqCst);
        assert_eq!(judge_kill(&sh, Some(ended(k + 30 * ms)), false, k), None);
        // A death with nothing closing, its last answer 2 s before Docker
        // reaped it: Docker's die, read 77 ms early through the same probe,
        // timed at the latest its reading allows -- never before the reap.
        let (doings, o) = (Doings::default(), opts());
        let reap = k + 5_000 * ms;
        sh.answered(reap - 2_000 * ms);
        let mut evs = vec![DockerEv {
            at: reap + 10 * ms,
            vm: vm(reap),
            die: true,
            exit: Some(137),
        }];
        let (_, at, _) = found_dead(&sh, &doings, &o, ended(reap), &mut evs, k, None, None);
        assert!(sh.vm_ns(vm(reap)).unwrap() < reap);
        assert!(at >= reap, "a death at {at}, before its reap at {reap}");
        assert!(at <= reap + 2 * sh.clock_err(reap));

        // A minute's probe behind a restart's backlog, its round trip
        // 3.4 s, agreeing with the one kept: that one kept, its error
        // its 4 ms and the drift its 32 minutes may hold, under a second.
        let t = 92 * min;
        assert!(!sh.clock(&probe(t, 3_400 * ms, 0)));
        assert_eq!(sh.clock_at.load(Ordering::SeqCst), 60 * min);
        assert!(sh.clock_err(t) < 1_000 * ms, "{}", sh.clock_err(t));
        // A tighter one is taken.
        assert!(sh.clock(&probe(t + min, 2 * ms, 0)));
        assert_eq!(sh.clock_rtt.load(Ordering::SeqCst), 2 * ms);
        // One that disagrees past both errors -- the VM's clock stepped 5
        // s -- is taken, slow as it is: the one kept is wrong now.
        let t = t + 2 * min;
        let step = 5_000 * ms as i64;
        assert!(sh.clock(&probe(t, 3_400 * ms, step)));
        let read = sh.vm_ns(vm(t) + step as u64).unwrap();
        assert!(read.abs_diff(t) <= sh.clock_err(t), "{read} {t}");
    }

    #[test]
    fn a_job_cut_after_the_kill_was_sent_is_the_kill_s_once_the_kill_is_judged_its_own() {
        let sh = Arc::new(Shared::new(
            &opts(),
            "127.0.0.1:1".into(),
            "127.0.0.1:1".into(),
            &[],
        ));
        let ms = 1_000_000u64;
        let quick = Duration::from_millis(20);
        let (running, stopped) = (|| false, || true);
        // A compact asked at `s`, the kill sent after it -- `kill_sent`
        // stored before `docker kill` -- and judged its own: the kill's.
        let s = 5_000 * ms;
        sh.kill_sent.store(s + 1_000 * ms, Ordering::SeqCst);
        sh.kill_judged.store(s + 1_000 * ms, Ordering::SeqCst);
        assert_eq!(
            cut_by(&sh, s, true, &running, quick, quick),
            Some(CutBy::Kill)
        );
        // Not judged yet: the job waits for its verdict, a death found
        // meanwhile -- the moment before the send -- the death's.
        let s = 10_000 * ms;
        sh.kill_sent.store(s + 1_000 * ms, Ordering::SeqCst);
        let t = {
            let sh = Arc::clone(&sh);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                sh.deaths.lock().unwrap().push(s + 900 * ms);
            })
        };
        let long = Duration::from_secs(30);
        assert_eq!(
            cut_by(&sh, s, true, &running, quick, long),
            Some(CutBy::Death)
        );
        t.join().unwrap();
        // Judged the kill's meanwhile: the kill's.
        let s = 20_000 * ms;
        sh.kill_sent.store(s + 1_000 * ms, Ordering::SeqCst);
        let t = {
            let sh = Arc::clone(&sh);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                sh.kill_judged.store(s + 1_000 * ms, Ordering::SeqCst);
            })
        };
        assert_eq!(
            cut_by(&sh, s, true, &running, quick, long),
            Some(CutBy::Kill)
        );
        t.join().unwrap();
        // Never judged within its bound: the kill's, as before.
        let s = 22_000 * ms;
        sh.kill_sent.store(s + 1_000 * ms, Ordering::SeqCst);
        assert_eq!(
            cut_by(&sh, s, true, &running, quick, quick),
            Some(CutBy::Kill)
        );
        // No kill since: a death found since, the job's connection closed
        // or not, is the death's.
        sh.deaths.lock().unwrap().push(25_000 * ms);
        assert_eq!(
            cut_by(&sh, 24_000 * ms, false, &running, quick, quick),
            Some(CutBy::Death)
        );
        // None found: the server not running with no window open, the
        // death not found yet -- where its connection closed; a wait that
        // ran out is nobody's, Docker not asked.
        let s = 30_000 * ms;
        assert_eq!(
            cut_by(&sh, s, true, &stopped, quick, quick),
            Some(CutBy::Death)
        );
        assert_eq!(cut_by(&sh, s, true, &running, quick, quick), None);
        assert_eq!(cut_by(&sh, s, false, &stopped, quick, quick), None);
        // In a restart's window with nothing found: the kill's, the
        // server down for it whatever Docker says.
        sh.windows.lock().unwrap().push((0, u64::MAX, u64::MAX));
        assert_eq!(
            cut_by(&sh, s, true, &stopped, quick, quick),
            Some(CutBy::Kill)
        );
        assert_eq!(
            cut_by(&sh, s, false, &running, quick, quick),
            Some(CutBy::Kill)
        );
    }

    #[test]
    fn a_look_finds_a_death_where_docker_says_not_running_and_keeps_its_events() {
        let (tx, rx) = mpsc::channel();
        let mut evs = Vec::new();
        let oom = DockerEv {
            at: 5,
            vm: 7,
            die: false,
            exit: None,
        };
        tx.send(oom).unwrap();
        let up = Exit {
            running: true,
            ..Exit::default()
        };
        // Running, or unread: none; the events read all the same, for the
        // death's line once there is one.
        assert_eq!(dead(|| Some(up), &rx, &mut evs), None);
        assert_eq!(evs, vec![oom]);
        assert_eq!(dead(|| None, &rx, &mut evs), None);
        let gone = Exit {
            running: false,
            oom: true,
            code: Some(137),
            finished: Some(9),
        };
        let die = DockerEv {
            die: true,
            exit: Some(137),
            ..oom
        };
        tx.send(die).unwrap();
        assert_eq!(dead(|| Some(gone), &rx, &mut evs), Some(gone));
        assert_eq!(evs, vec![oom, die]);
    }

    #[test]
    fn the_wait_for_a_kill_s_compact_ends_on_a_die() {
        let s = 1_000_000_000u64;
        let die = DockerEv {
            at: 1,
            vm: 1,
            die: true,
            exit: Some(137),
        };
        let oom = DockerEv {
            die: false,
            exit: None,
            ..die
        };
        let zero = Duration::ZERO;
        assert_eq!(compact_wait(&[], u64::MAX, 5 * s, false, zero), None);
        // An oom alone: its die is what ends the process.
        assert_eq!(compact_wait(&[oom], u64::MAX, 5 * s, false, zero), None);
        // The die: at once, whatever else -- the dry run's second death
        // held the run here for `WAIT_MAX`.
        assert_eq!(
            compact_wait(&[oom, die], u64::MAX, 5 * s, false, zero),
            Some(CompactWait::Died)
        );
        assert_eq!(
            compact_wait(&[die], 4 * s, 5 * s, true, WAIT_MAX),
            Some(CompactWait::Died)
        );
        // Under way a second: the kill lands in it.
        assert_eq!(
            compact_wait(&[], 4 * s, 5 * s, false, zero),
            Some(CompactWait::UnderWay)
        );
        assert_eq!(compact_wait(&[], 4 * s + 1, 5 * s, false, zero), None);
        assert_eq!(
            compact_wait(&[], u64::MAX, 5 * s, true, zero),
            Some(CompactWait::Ended)
        );
        assert_eq!(
            compact_wait(&[], u64::MAX, 5 * s, false, WAIT_MAX),
            Some(CompactWait::Timeout)
        );
    }

    #[test]
    fn a_tied_child_ends_once_its_input_is_let_go() {
        use std::process::Stdio;
        let mut c = tied("sh", &["-c", "echo $$; exec sleep 60"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(c.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid = line.trim().to_string();
        let alive = |pid: &str| {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(alive(&pid));
        // As this process ending lets go of it.
        drop(c.stdin.take());
        c.wait().unwrap();
        assert!(!alive(&pid), "the tied child {pid} outlived its shell");
    }

    /// `ycsb` in a terminal for `a_tied_child_ends_with_the_run_on_ctrl_c`:
    /// a tied child standing for `docker events`, its pid printed, then a
    /// wait for the Ctrl-C. Run alone, nothing.
    #[test]
    #[ignore = "run by a_tied_child_ends_with_the_run_on_ctrl_c"]
    fn tied_under_a_terminal() {
        use std::process::Stdio;
        if std::env::var_os("SOAK_TIED_UNDER_A_TERMINAL").is_none() {
            return;
        }
        let mut c = tied("sh", &["-c", "echo $$; exec sleep 60"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(c.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        println!("tied {}", line.trim());
        std::io::stdout().flush().unwrap();
        // Its input held, as `ycsb` holds it the run long -- a wait lets
        // go of it first -- until the Ctrl-C.
        let _input = c.stdin.take();
        let _ = c.wait();
    }

    #[cfg(unix)]
    #[test]
    fn a_tied_child_ends_with_the_run_on_ctrl_c() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        // This test's binary again for `ycsb`, the foreground process
        // group a terminal's Ctrl-C signals: a group of its own here.
        let mut run = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "soak::tests::tied_under_a_terminal",
                "--nocapture",
            ])
            .env("SOAK_TIED_UNDER_A_TERMINAL", "1")
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let out = BufReader::new(run.stdout.take().unwrap());
        let pid = out
            .lines()
            .map_while(Result::ok)
            .find_map(|l| l.strip_prefix("tied ").map(str::to_string))
            .expect("the run's tied child");
        let alive = |pid: &str| {
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(alive(&pid));
        // Ctrl-C: SIGINT to the run's group.
        let group = format!("-{}", run.id());
        assert!(Command::new("kill")
            .args(["-INT", "--", &group])
            .status()
            .unwrap()
            .success());
        run.wait().unwrap();
        // The child ignores SIGINT, as an asynchronous list's does: what
        // ends it is the tied shell, which outlived the run. Waited for,
        // bounded; let go of whatever the verdict.
        let t = Instant::now();
        while alive(&pid) && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let outlived = alive(&pid);
        if outlived {
            let _ = Command::new("kill").args(["-9", &pid]).status();
        }
        assert!(!outlived, "the tied child {pid} outlived the run's Ctrl-C");
    }

    #[test]
    fn the_run_s_open_files_are_counted_from_its_connections() {
        let with = |r: &str| {
            Opts::parse(&[
                "--sync".into(),
                "250".into(),
                "--rate".into(),
                "100".into(),
                "--replica-threads".into(),
                r.into(),
            ])
        };
        // The pool's 32 one connection each, the lane's 128 two at a
        // restart, two files a connection, the 64 streams and 64 beside.
        assert_eq!(fds_needed(&with("128")), 2 * (32 + 2 * 128) + 64 + 64);
        assert_eq!(fds_needed(&with("0")), 2 * (2 * 32) + 64 + 64);
        // Past macOS's default of 256 either way with the lane: run-soak.sh
        // raises it.
        assert!(fds_needed(&with("128")) > 256);
        assert!(open_files_limit().is_none_or(|n| n > 0));
    }

    #[test]
    fn the_peak_s_lane_counts_the_reads_its_move_drops() {
        let s = 1_000_000_000u64;
        // A read every 10 ms: a move of 2.5 s drops 250, one of 3 ms a
        // thousandth of each of 300.
        assert_eq!(dropped_milli(5 * s / 2, s / 100), 250_000);
        assert_eq!(dropped_milli(3_000_000, s / 100), 300);
        assert_eq!(dropped_milli(s, 0), 1_000_000_000_000);
    }

    #[test]
    fn a_server_s_looks_a_second_keep_the_minute_s_the_life_s_and_the_window_s_most() {
        let mb = 1u64 << 20;
        assert_eq!(
            MemSample::parse(&format!(
                "M {} {} {} {} 9",
                437 * mb,
                15 * mb,
                80 * mb,
                2 * mb
            )),
            Some(MemSample {
                anon: 437 * mb,
                file: 15 * mb,
                dirty: 80 * mb,
                writeback: 2 * mb,
                majflt: 9,
            })
        );
        assert_eq!(MemSample::parse("M 1 2 3"), None);
        assert_eq!(MemSample::parse("S 1 2"), None);
        let at = |anon: u64, file: u64, dirty: u64, majflt: u64| MemSample {
            anon: anon * mb,
            file: file * mb,
            dirty: dirty * mb,
            writeback: 0,
            majflt,
        };
        let mut m = Mem::default();
        // The long run's minute 300 as a second's looks would have seen it:
        // 304 MB at the minute's look, the compaction's jump between two.
        m.sample(1, at(304, 200, 0, 100), false);
        m.sample(2, at(427, 15, 80, 400), true);
        m.sample(3, at(310, 40, 10, 450), true);
        let minute = m.minute.take().unwrap();
        assert_eq!(
            minute,
            MemSpan {
                anon: 427 * mb,
                dirty: 80 * mb,
                file: 15 * mb,
                faults: 350,
                n: 3,
            }
        );
        // The window's two seconds: their faults and their least cache.
        let w = m.window.take().unwrap();
        assert_eq!((w.faults, w.file, w.n), (350, 15 * mb, 2));
        // The next minute starts empty; the life keeps its most.
        m.sample(4, at(300, 50, 5, 460), false);
        assert_eq!(m.minute.unwrap().anon, 300 * mb);
        assert_eq!(m.life.unwrap().anon, 427 * mb);
        assert!(m.window.is_none());
        // A new life: its own most, and its faults counted from its first
        // look -- the cgroup's counters start again with the container.
        m.restarted();
        m.sample(5, at(120, 50, 0, 3), false);
        assert_eq!(m.life.map(|l| (l.anon, l.faults)), Some((120 * mb, 0)));
    }

    #[test]
    fn the_pool_draws_the_mix_less_the_lane_s_reads_and_the_lane_counts_its_own() {
        // In one pool the draw is the mix itself.
        assert_eq!(pool_u(0.38, false), 0.38);
        // With a lane: never the replica's 4%, each other share kept in
        // proportion -- the reads 0.36 of 0.96, the updates 0.37.
        let n = 96_000;
        let us: Vec<f64> = (0..n).map(|i| pool_u(i as f64 / n as f64, true)).collect();
        assert!(us.iter().all(|u| !(0.36..0.40).contains(u) && *u < 1.0));
        let share = |lo: f64, hi: f64| {
            us.iter().filter(|u| (lo..hi).contains(*u)).count() as f64 / n as f64
        };
        assert!((share(0.0, 0.36) - 0.36 / 0.96).abs() < 1e-3);
        assert!((share(0.40, 0.77) - 0.37 / 0.96).abs() < 1e-3);
        assert!((share(0.98, 1.0) - 0.02 / 0.96).abs() < 1e-3);
        // A read through the replica held by its compaction is counted by
        // its own answer too; an operation of the class that is not one --
        // a worker's backlog after it -- by its time from the schedule
        // alone.
        let mut h = Hists::new();
        let rec = |via_replica, took, own_us| Rec {
            via_replica,
            behind: false,
            in_restart: false,
            took,
            own_us,
        };
        h.record_rec(Class::ReplicaCompact, rec(true, 5_400_000_000, 640_000));
        h.record_rec(Class::ReplicaCompact, rec(false, 5_300_000_000, 3_000));
        h.record_rec(Class::Steady, rec(true, 300_000_000, 200_000));
        assert_eq!((h.r_compact.n, h.r_compact_own.n), (2, 1));
        assert!((h.r_compact_own.pct(0.99) / 1e3 - 640.0).abs() < 10.0);
        // Still 32 bytes a held operation, its own time a `u32` of µs.
        assert_eq!(std::mem::size_of::<Held>(), 32);
    }

    #[test]
    fn a_death_fails_its_own_criterion_and_names_what_followed_it() {
        // The long run's death, judged by the new lines: found as the kill
        // due at minute 300 was to be sent, OOM-killed 12.3 s after that
        // kill was due, timed by the burst of closes its exit made; the kill
        // not sent, the death standing for it; the 96 connections it cut
        // counted with it; one write gone, answered 150 ms before it.
        let run = synthetic(480, &|m, x| {
            if m == 301 {
                x.remove("kill");
                x.remove("kill_ms");
                x.remove("kill_sync_ms");
                x.remove("kill_compacting");
                for (k, v) in [
                    ("kill_died", 1.0),
                    ("died", 1.0),
                    ("oom", 1.0),
                    ("death_oom", 1.0),
                    ("death_exit", 137.0),
                    ("death_at_s", 18_012.3),
                    ("death_source", 1.0),
                    ("death_kill_due_s", -12.3),
                    ("death_down", 96.0),
                    ("catchup_down", 185.0),
                    ("compact_died", 1.0),
                    ("lost_writes", 1.0),
                    ("lost_ms_max", 150.0),
                ] {
                    x.insert(k, v);
                }
            }
            if m == 300 {
                x.insert("anon_peak", 427.0 * 1048576.0);
                x.insert("dirty_peak", 80.0 * 1048576.0);
            }
            if (1..=480).contains(&m) {
                x.entry("anon_peak").or_insert(200.0 * 1048576.0);
                x.entry("dirty_peak").or_insert(10.0 * 1048576.0);
            }
            if m == u64::MAX {
                x.insert("memory_limit", 512.0 * 1048576.0);
            }
        });
        // Its own criterion alone: the kills, the verifies, the compacts
        // and the errors hold, the death standing for the kill it met.
        assert_eq!(failing(&run), vec![CRITERIA[18]]);
        let j = judge(&run, None);
        let said = |c: &str| {
            j[0].verdicts
                .iter()
                .find(|v| v.0 == c)
                .map(|v| v.2.clone())
                .unwrap()
        };
        assert_eq!(
            said(CRITERIA[18]),
            "1 deaths, 1 OOM kills, the replica stopped 0 times; minutes 301; the primary's deaths, each a failure of its own: minute 301: OOM-killed, 12 s after the kill due, in the side jobs it waited for, timed by the burst of closes its exit made; 0 mismatches and 1 writes gone with it in the 30 minutes from its line, the earliest answered 150 ms before it"
        );
        assert!(
            said(CRITERIA[14]).ends_with(
                "; 1 kills found the server dead and 1 compacts and backups were cut by a death, each the death's"
            ),
            "{}",
            said(CRITERIA[14])
        );
        assert!(
            said(CRITERIA[9]).ends_with(
                "; 96 connections a death of the primary cut before it was found counted with it, as down; 185 reads through the replica a 504 answered in a restart's catch-up counted with it, as down"
            ),
            "{}",
            said(CRITERIA[9])
        );
        assert!(
            said(CRITERIA[5]).ends_with(
                "; a second at a time the most anonymous memory 427 MB (minute 300), dirty pages 80 MB (minute 300); the two 507 MB of --memory's 512 at minute 300, said, not judged"
            ),
            "{}",
            said(CRITERIA[5])
        );
        // Dead in the kill's announced second -- the harness's dry run's,
        // a second into the compact the kill was to land in -- said as
        // that, not as the side jobs the kill waited for.
        let in_kill = synthetic(480, &|m, x| {
            if m == 301 {
                x.remove("kill");
                for (k, v) in [
                    ("kill_died", 1.0),
                    ("died", 1.0),
                    ("death_oom", 0.0),
                    ("death_exit", 137.0),
                    ("death_at_s", 18_003.8),
                    ("death_source", 1.0),
                    ("death_kill_due_s", -3.8),
                    ("death_announced_ms", 812.0),
                    ("compact_died", 1.0),
                ] {
                    x.insert(k, v);
                }
            }
        });
        let j = judge(&in_kill, None);
        let v = &j[0]
            .verdicts
            .iter()
            .find(|v| v.0 == CRITERIA[18])
            .unwrap()
            .2;
        assert!(
            v.contains(
                "minute 301: exit 137, 0.8 s after the kill due was announced, before it was sent, timed by the burst of closes its exit made"
            ),
            "{v}"
        );
        // A kill met dead counts with the death, never as the kill: one
        // the death did not stand for is short.
        let short = synthetic(480, &|m, x| {
            if m == 301 {
                x.remove("kill");
                x.insert("died", 1.0);
            }
        });
        assert_eq!(failing(&short), vec![CRITERIA[14], CRITERIA[18]]);
    }

    #[test]
    fn the_replicas_compaction_criterion_names_what_the_replica_answered_in() {
        let run = synthetic(480, &|m, x| {
            if m == 19 {
                for (k, v) in [
                    ("r_compacts_watched", 1.0),
                    ("r_auto_compacts", 1.0),
                    ("r_compact_ms", 33_600.0),
                    ("r_compact_ops", 4_200.0),
                    ("rc_seconds", 34.0),
                    ("rc_r_behind_max_s", 0.6),
                    ("rc_behind_max_s", 0.0),
                    ("rc_r_majflt", 105_800.0),
                    ("rc_r_file_min", 210.0 * 1048576.0),
                ] {
                    x.insert(k, v);
                }
            }
            if m == u64::MAX {
                x.insert("r_compact_ops", 4_200.0);
                x.insert("p99_r_compact", 1.1e6);
                x.insert("p99_r_compact_own", 0.72e6);
                x.insert("r_compact_own_n", 4_200.0);
            }
        });
        let j = judge(&run, None);
        let v = j[0].verdicts.iter().find(|v| v.0 == CRITERIA[20]).unwrap();
        assert!(v.1, "{}", v.2);
        assert!(
            v.2.ends_with(
                "; the replica answered the reads through it among them in 0.72 s at the p99 from their send (4200 reads); in their 34 seconds, the replica lane 0.6 s behind at the most, the pool 0.0 s behind at the most, the replica's major faults 105800, its page cache 210 MB at the least"
            ),
            "{}",
            v.2
        );
        // The lane, and how far behind its own schedule it fell, among the
        // notes; a run from before the lanes says its reads shared the pool.
        let laned = synthetic(480, &|m, x| match m {
            0 => {
                x.insert("replica_threads", 64.0);
            }
            7 => {
                x.insert("r_behind_max_s", 1.4);
            }
            _ => {}
        });
        let notes = judge(&laned, None)[0].notes.join("\n");
        assert!(
            notes.contains(
                "the reads through the replica: a lane of 64 connections on a schedule of its own at 4% of the rate, 1.4 s behind it at the most"
            ),
            "{notes}"
        );
        let notes = judge(&synthetic(480, &|_, _| {}), None)[0].notes.join("\n");
        assert!(
            notes.contains("in the pool every operation shares"),
            "{notes}"
        );
    }
}
