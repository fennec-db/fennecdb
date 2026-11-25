//! fennecdb's fuzzer: every parser of bytes a process takes from outside --
//! an HTTP request's head and chunked body, JSON, FennecQL, a value's and a
//! type's encoding, a `.fennec` file's open and a replica's apply, a SQLite
//! file -- handed inputs its seeds are mutated into, each run caught where
//! it panics, aborts, hangs or asks for more memory than its input could
//! need (docs/design/conventions.md#fuzzing).
//!
//! The mutator, the coverage it is guided by and what keeps a crashing
//! input are this crate's own, with no dependency, so the zero-dependency
//! rule holds here too: no libFuzzer, no cargo-fuzz. `make fuzz` lists the
//! targets and runs one; `make fuzz-smoke` runs each for a count of runs its
//! own, about 30 s, built with AddressSanitizer and LLVM's coverage counters on
//! a nightly compiler pinned to a date -- CI's `fuzz-smoke` job -- and `fuzz.yml`
//! each week for half an hour. A build without them (`cargo test`) runs every
//! target over its seeds and a few hundred of their mutations.

pub mod cover;
pub mod guard;
pub mod mutate;
pub mod targets;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Allocations held to a cap (`--max-alloc-mb`), the input kept past it.
#[global_allocator]
static ALLOC: guard::Capped = guard::Capped;

/// A parser of untrusted bytes and what it is fed.
pub struct Target {
    pub name: &'static str,
    /// What it runs, in a line.
    pub about: &'static str,
    /// The longest input it is handed.
    pub max_len: usize,
    /// The runs `make fuzz-smoke` gives it (`--smoke`): a count, never a
    /// time, so a commit's smoke hands it the same inputs on any runner --
    /// about what 30 s of CI's runner ran under ASan when it was set --
    /// bounded by [`SMOKE_CAP`] of CPU time.
    pub smoke: u64,
    /// The inputs it starts from: built here, and read from the
    /// repository's fixtures where they are found.
    pub seeds: fn() -> Vec<Vec<u8>>,
    /// The tokens its inputs are spelled with, beside those of its seeds.
    pub dictionary: &'static [&'static [u8]],
    /// One run over an input: an answer or a refusal is a pass, whatever
    /// ends it otherwise a finding.
    pub run: fn(&[u8]),
}

/// How a fuzzer runs.
pub struct Options {
    /// How long, or how many runs, whichever comes first; neither, for
    /// ever.
    pub seconds: Option<u64>,
    pub runs: Option<u64>,
    pub seed: u64,
    /// Where the inputs that reached new coverage are kept, and read from
    /// at the start: a run goes on from the one before.
    pub corpus: Option<PathBuf>,
    /// Where an input that ends a run is written.
    pub crashes: PathBuf,
    /// A run longer than this is a finding.
    pub timeout: Duration,
    /// The CPU time every run together may take, past which the process
    /// ends failing, naming the target and the run it reached
    /// ([`guard::cap`]): `--smoke`'s [`SMOKE_CAP`]. A bound on a fuzzer
    /// whose runs all slowed, never what ends one that passes.
    pub cap: Option<Duration>,
    /// The most one allocation may ask for.
    pub max_alloc: usize,
    /// Progress lines, or none.
    pub quiet: bool,
    /// A finding says its recipe alone -- the target, the seed, the run and
    /// its input's hash ([`guard::say_recipe`]) -- and never its message
    /// or its backtrace: the weekly run's, whose log a public repository
    /// shows anyone (SECURITY.md asks a finding be told privately). A
    /// maintainer makes the input again from the recipe ([`reproduce`]).
    /// The smoke says its message: its inputs are its commit's, which
    /// anyone makes again with `make fuzz-smoke`.
    pub private: bool,
}

/// How a fuzzer's runs ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every run answered or refused: so many runs, the corpus at its end.
    Clean { runs: u64, corpus: usize },
    /// A run panicked: its message, its run, and where its input was
    /// written.
    Panicked {
        message: String,
        run: u64,
        input: PathBuf,
    },
}

/// The CPU time a target's smoke may take (`--smoke`, [`Options::cap`]):
/// six minutes, twelve times the 30 s of CI's runner each count was set
/// from. Generous, so a machine slower than the runner never meets it --
/// an M-series Mac under ASan spent 104 s of CPU on `http`'s count at a
/// load near 15 and 110 s at one of 70 to 200, the most of any target --
/// and short enough that eight targets each just under it, 48 minutes, end
/// inside ci.yml's 57 with the build's minute and a half: a smoke slowed
/// past the job's timeout ends at a target's cap first, named, rather
/// than at the timeout, naming none.
pub const SMOKE_CAP: Duration = Duration::from_secs(360);

/// The stack a run is on: a server's connection thread's
/// (`fennec_http::CONNECTION_STACK`), which a request parses, plans and
/// reads on, so that what overflows here overflows there.
pub const RUN_STACK: usize = 8 << 20;

/// Runs `target` as `opts` say, on a thread of [`RUN_STACK`]. A task the
/// engine cuts in shares runs them on that thread ([`fennec_core::one_thread`]):
/// the coverage counters are bumped unlocked, and threads beside one
/// another lost counts as their scheduler had it, so that one process kept
/// an input another did not.
pub fn fuzz(target: &'static Target, opts: Options) -> Outcome {
    fennec_core::one_thread();
    let _stops = Stops;
    std::thread::Builder::new()
        .name(format!("fuzz-{}", target.name))
        .stack_size(RUN_STACK)
        .spawn(move || fuzz_here(target, &opts))
        .expect("the run's thread")
        .join()
        .unwrap_or_else(|_| panic!("the fuzzer itself panicked"))
}

/// Stops the timers a fuzzer sets -- its cap of CPU time, a run's
/// timeout -- as it is dropped: where [`fuzz`] returns, and where it
/// unwinds from its thread's panic, which skipped the call after the join
/// that stopped the cap. Left set, a test binary's tests after it ran
/// under a timer none of them set, whose handler ends the process.
struct Stops;

impl Drop for Stops {
    fn drop(&mut self) {
        guard::disarm();
        guard::cap(None, 0);
    }
}

/// What `fennec-fuzz` says of how it is run, after a flag that does not
/// read.
pub const USAGE: &str = "usage: fennec-fuzz list | names | run <target> [--seconds N] [--runs N] \
    [--smoke] [--seed N] [--corpus DIR|none] [--crashes DIR] [--timeout-ms N] [--max-alloc-mb N] [--quiet] \
    [--private] | replay <target> <file>... | reproduce <target> --seed N --start HASH \
    [--corpus DIR] --run N --fnv HASH --kept RUNS [--out FILE]";

/// The options `fennec-fuzz run <target> <args>` fuzzes `target` with, or
/// what to say of a flag that does not read. Here rather than in the
/// binary, so that a test holds `--smoke` -- the flag `make fuzz-smoke`
/// runs each target with -- to the count and the cap of CPU time it sets
/// ([`SMOKE_CAP`]).
pub fn run_options(target: &Target, args: &[String]) -> Result<Options, String> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/fuzz"));
    let mut opts = Options {
        seconds: None,
        runs: None,
        seed: 1,
        corpus: Some(root.join("corpus").join(target.name)),
        crashes: root.join("crashes"),
        // Wall time: a loop that never ends is caught at any bound, and at
        // 10 s a machine at a load of 40 took 8 000 numbers of 1e255, 2.5 s
        // of a quiet one's under ASan, for a hang.
        timeout: Duration::from_secs(30),
        cap: None,
        max_alloc: 2048 << 20,
        quiet: false,
        private: false,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} takes a value\n{USAGE}"))
        };
        let number = |v: String| -> Result<u64, String> {
            v.parse()
                .map_err(|_| format!("{flag} takes a number, not `{v}`"))
        };
        match flag.as_str() {
            "--seconds" => opts.seconds = Some(number(value()?)?),
            "--runs" => opts.runs = Some(number(value()?)?),
            "--smoke" => {
                opts.runs = Some(target.smoke);
                opts.cap = Some(SMOKE_CAP);
            }
            "--seed" => opts.seed = number(value()?)?,
            "--corpus" => {
                opts.corpus = match value()? {
                    v if v == "none" => None,
                    v => Some(PathBuf::from(v)),
                }
            }
            "--crashes" => opts.crashes = PathBuf::from(value()?),
            "--timeout-ms" => opts.timeout = Duration::from_millis(number(value()?)?),
            "--max-alloc-mb" => {
                opts.max_alloc = (number(value()?)? as usize).saturating_mul(1 << 20)
            }
            "--quiet" => opts.quiet = true,
            "--private" => opts.private = true,
            other => return Err(format!("unknown flag `{other}`\n{USAGE}")),
        }
    }
    Ok(opts)
}

/// What a run starts from: the target's seeds, and the inputs a corpus
/// directory holds, each cut to the target's longest, in order, once.
pub fn start_of(target: &Target, corpus: Option<&Path>) -> Vec<Vec<u8>> {
    let mut start = (target.seeds)();
    if let Some(dir) = corpus {
        let mut kept: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        kept.sort();
        start.extend(kept.iter().filter_map(|p| std::fs::read(p).ok()));
    }
    for s in &mut start {
        s.truncate(target.max_len);
    }
    start.sort();
    start.dedup();
    start
}

/// A start's fingerprint: what a recipe names it by, so that a run made
/// again from another start -- another checkout's seeds, another corpus --
/// is told apart rather than made of other inputs.
pub fn fingerprint(start: &[Vec<u8>]) -> u64 {
    start.iter().fold(guard::fnv(&[]), |h, s| {
        let h = (h ^ s.len() as u64).wrapping_mul(0x0000_0100_0000_01b3);
        s.iter().fold(h, |h, &x| {
            (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3)
        })
    })
}

/// The inputs a run hands its target, a run at a time: each of its start
/// once, then a mutation of one the corpus holds, the corpus taking each
/// input that reached new coverage ([`Inputs::keep`]). Nothing else is
/// read -- no clock, no coverage -- so a seed, a start and the runs kept
/// make the same inputs again, on any machine and in a build with or
/// without coverage: how a finding is made again from its recipe
/// ([`reproduce`]).
pub struct Inputs {
    mutator: mutate::Mutator,
    seed: u64,
    start: Vec<Vec<u8>>,
    corpus: Vec<Vec<u8>>,
    input: Vec<u8>,
    /// The run the next input is for.
    run: u64,
}

impl Inputs {
    pub fn new(target: &Target, seed: u64, start: Vec<Vec<u8>>) -> Inputs {
        let mut dictionary: Vec<Vec<u8>> = target.dictionary.iter().map(|t| t.to_vec()).collect();
        dictionary.extend(mutate::tokens_of(&start, 512));
        Inputs {
            mutator: mutate::Mutator::new(seed, dictionary, target.max_len),
            seed,
            corpus: Vec::with_capacity(start.len()),
            start,
            input: Vec::new(),
            run: 0,
        }
    }

    /// The run the next input is for, counted from 0.
    pub fn run(&self) -> u64 {
        self.run
    }

    /// Whether the input last handed came from the start, which the
    /// corpus takes whatever it reached.
    fn starting(&self) -> bool {
        self.run <= self.start.len() as u64
    }

    /// The next run's input.
    pub fn next_input(&mut self) -> &[u8] {
        let at = self.run as usize;
        self.run += 1;
        self.input.clear();
        if let Some(s) = self.start.get(at) {
            self.input.extend_from_slice(s);
            self.corpus.push(s.clone());
            return &self.input;
        }
        if self.corpus.is_empty() {
            self.corpus.push(Vec::new());
        }
        let parent = self.mutator.rng.below(self.corpus.len());
        self.input.extend_from_slice(&self.corpus[parent]);
        self.mutator.mutate(&mut self.input, &self.corpus);
        &self.input
    }

    /// The input last handed reached new coverage: the corpus takes it, a
    /// parent and a splice's other half from the next run on. Whether it
    /// did -- a mutation's, not one of the start -- the corpus took it.
    pub fn keep(&mut self) -> bool {
        if self.starting() {
            return false;
        }
        self.corpus.push(self.input.clone());
        true
    }

    /// The fuzzer's own code is instrumented beside the targets': its
    /// edges are taken as seen before the first run, by mutations of a
    /// mutator of its own, so that the runs' are the seed's alone.
    fn warm_up(&self) {
        let mut warm = mutate::Mutator::new(
            !self.seed,
            self.mutator.dictionary.clone(),
            self.mutator.max_len,
        );
        let mut scratch = self.start.first().cloned().unwrap_or_default();
        for _ in 0..2000 {
            warm.mutate(&mut scratch, &self.start);
        }
    }
}

/// What a finding's recipe says ([`guard::say_recipe`]), to make its input
/// again: `fennec-fuzz reproduce <target> --seed .. --start .. --run ..
/// --fnv .. --kept ..`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub seed: u64,
    /// The start's [`fingerprint`].
    pub start: u64,
    /// The corpus directory the start read, where it read one.
    pub corpus: Option<PathBuf>,
    /// The run that found it, and its input's [`guard::fnv`].
    pub run: u64,
    pub fnv: u64,
    /// The runs whose inputs the corpus took, ascending.
    pub kept: Vec<u64>,
}

impl Recipe {
    /// A recipe from the flags a finding says it with -- `--seed N --start
    /// HASH [--corpus DIR] --run N --fnv HASH --kept RUNS` -- and where to
    /// write the input made again, `--out FILE`, where given.
    pub fn from_args(args: &[String]) -> Result<(Recipe, Option<PathBuf>), String> {
        let mut r = Recipe {
            seed: 0,
            start: 0,
            corpus: None,
            run: 0,
            fnv: 0,
            kept: Vec::new(),
        };
        let (mut out, mut named) = (None, 0u8);
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it.next().ok_or_else(|| format!("{flag} takes a value"))?;
            let number = |radix: u32| u64::from_str_radix(value, radix).ok();
            let read = match flag.as_str() {
                "--seed" => number(10).map(|v| r.seed = v).map(|_| 1),
                "--start" => number(16).map(|v| r.start = v).map(|_| 2),
                "--run" => number(10).map(|v| r.run = v).map(|_| 4),
                "--fnv" => number(16).map(|v| r.fnv = v).map(|_| 8),
                "--kept" => kept_from(value).map(|v| r.kept = v).map(|_| 16),
                "--corpus" => {
                    r.corpus = Some(PathBuf::from(value));
                    Some(0)
                }
                "--out" => {
                    out = Some(PathBuf::from(value));
                    Some(0)
                }
                other => return Err(format!("unknown flag `{other}`")),
            };
            named |= read.ok_or_else(|| format!("{flag} does not read `{value}`"))?;
        }
        match named {
            31 => Ok((r, out)),
            _ => Err("a recipe names --seed, --start, --run, --fnv and --kept".into()),
        }
    }
}

/// The runs kept, as a recipe writes them: each the step from the one
/// before in base 36, joined by dots, `-` for none.
pub fn kept_text(kept: &[u64]) -> String {
    let mut out = Vec::new();
    let mut was = 0;
    for &k in kept {
        guard::push_kept(&mut out, k - was);
        was = k;
    }
    match out.is_empty() {
        true => "-".into(),
        false => String::from_utf8(out).expect("base 36 and dots"),
    }
}

/// [`kept_text`] read back.
pub fn kept_from(text: &str) -> Option<Vec<u64>> {
    if text == "-" {
        return Some(Vec::new());
    }
    let mut was = 0u64;
    text.split('.')
        .map(|step| {
            was = was.checked_add(u64::from_str_radix(step, 36).ok()?)?;
            Some(was)
        })
        .collect()
}

/// The input a finding's recipe names, made again: its start read as the
/// run read it and checked against the fingerprint, then the runs handed
/// as the run handed them up to the one it names, its hash checked. What
/// a CI run's finding is reproduced from, since it keeps no input.
pub fn reproduce(target: &Target, recipe: &Recipe) -> Result<Vec<u8>, String> {
    let start = start_of(target, recipe.corpus.as_deref());
    let found = fingerprint(&start);
    if found != recipe.start {
        return Err(format!(
            "this start is {found:016x}, the run's {:016x}: check out the commit the run tested, \
             run `make fuzz-seeds`, and give it the run's corpus where it read one",
            recipe.start
        ));
    }
    let mut inputs = Inputs::new(target, recipe.seed, start);
    let mut kept = recipe.kept.iter().peekable();
    loop {
        let run = inputs.run();
        let input = inputs.next_input();
        if run == recipe.run {
            let input = input.to_vec();
            let fnv = guard::fnv(&input);
            if fnv != recipe.fnv {
                return Err(format!(
                    "run {run} made an input of hash {fnv:016x}, not {:016x}",
                    recipe.fnv
                ));
            }
            return Ok(input);
        }
        if kept.next_if_eq(&&run).is_some() {
            inputs.keep();
        }
    }
}

fn fuzz_here(target: &'static Target, opts: &Options) -> Outcome {
    std::fs::create_dir_all(&opts.crashes).expect("the crashes' directory");
    guard::install(
        &opts.crashes,
        target.name,
        opts.timeout,
        opts.max_alloc,
        opts.private,
    );
    // A panic before this fuzzer -- another's, a caller's -- is none of its
    // runs'.
    let _ = guard::take_panic();
    if let Some(dir) = &opts.corpus {
        let _ = std::fs::create_dir_all(dir);
    }
    let start = start_of(target, opts.corpus.as_deref());
    let starts = start.len() as u64;
    // The corpus directory is the recipe's where it gave the start more
    // than the seeds.
    let read_corpus = opts
        .corpus
        .as_deref()
        .filter(|_| start_of(target, None).len() as u64 != starts);
    guard::recipe(target.name, opts.seed, fingerprint(&start), read_corpus);
    let mut inputs = Inputs::new(target, opts.seed, start);
    let mut cover = cover::Coverage::of_build();
    if let Some(c) = cover.as_mut() {
        inputs.warm_up();
        c.ignore_reached();
        // This thread's keys drawn, whoever drew them.
        let _ = std::hash::RandomState::new();
        if !guard::KEYS_FIXED.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "fennec-fuzz: {}: std's hash keys were drawn at random, so what a run reaches \
                 -- and the inputs after it -- is this process's alone: a count of runs makes \
                 other inputs elsewhere (crates/fennec-fuzz/src/main.rs)",
                target.name
            );
        }
    }

    // From the first run on, the seeds made and taken in: what the runs
    // spend.
    guard::cap(opts.cap, opts.runs.unwrap_or(0));
    let started = Instant::now();
    let mut last_report = Instant::now();
    // Where a count of runs bounds the run, progress is said by the count:
    // the clock read between runs would be counted in the next one's
    // coverage -- `Duration`'s comparison is code the counters count --
    // and a process the clock reads otherwise would keep another input.
    let report_every = match (opts.runs, opts.seconds) {
        (Some(n), None) => Some((n / 6).max(1)),
        _ => None,
    };
    let ended = |input: &[u8], message: String, runs: u64| -> Outcome {
        let name = format!("crash-{}-{:016x}", target.name, guard::fnv(input));
        let path = opts.crashes.join(name);
        let _ = std::fs::write(&path, input);
        eprintln!(
            "fennec-fuzz: {} panicked after {runs} runs; its input is {}",
            target.name,
            path.display()
        );
        guard::say_recipe(b"a panic");
        Outcome::Panicked {
            message,
            run: runs,
            input: path,
        }
    };

    loop {
        let runs = inputs.run();
        if runs >= starts
            && (opts.runs.is_some_and(|n| runs >= n)
                || opts
                    .seconds
                    .is_some_and(|s| started.elapsed() >= Duration::from_secs(s)))
        {
            break;
        }
        if runs == starts && !opts.quiet {
            eprintln!(
                "fennec-fuzz: {}: {} seeds, {}",
                target.name,
                starts,
                match &cover {
                    Some(c) => format!("{} of {} edges", c.edges, c.counters()),
                    None => "a build without coverage: mutating unguided".into(),
                }
            );
        }
        let input = inputs.next_input();
        if let Some(message) = run_one(target, runs, input) {
            let input = input.to_vec();
            return ended(&input, message, runs);
        }
        // A seed's coverage is taken in, the seed kept whatever it reached;
        // what the mutator reached counts toward the run's scan, and is
        // never new: its edges were taken as seen in every bucket.
        if cover.as_mut().is_some_and(|c| c.take_new()) && inputs.keep() {
            guard::kept(runs);
            if let Some(dir) = &opts.corpus {
                let input = inputs.input.clone();
                let _ = std::fs::write(dir.join(format!("{:016x}", guard::fnv(&input))), &input);
            }
        }
        let report = !opts.quiet
            && match report_every {
                Some(every) => (runs + 1).is_multiple_of(every),
                None => last_report.elapsed() >= Duration::from_secs(5),
            };
        if report {
            last_report = Instant::now();
            let secs = started.elapsed().as_secs_f64();
            eprintln!(
                "fennec-fuzz: {}: {} runs, {:.0}/s, corpus {}{}",
                target.name,
                runs + 1,
                (runs + 1) as f64 / secs,
                inputs.corpus.len(),
                cover
                    .as_ref()
                    .map(|c| format!(", {} edges", c.edges))
                    .unwrap_or_default()
            );
        }
    }
    let runs = inputs.run();
    if !opts.quiet {
        // The time on a line of its own: the last says what the runs were
        // -- the same line from a commit wherever a count bounded them
        // (`make fuzz-smoke`), on every machine of a platform.
        eprintln!(
            "fennec-fuzz: {}: done, {runs} runs in {:.1} s",
            target.name,
            started.elapsed().as_secs_f64(),
        );
        eprintln!(
            "fennec-fuzz: {}: {runs} runs from seed {}, corpus {} of fingerprint {:016x}{}",
            target.name,
            opts.seed,
            inputs.corpus.len(),
            fingerprint(&inputs.corpus),
            cover
                .as_ref()
                .map(|c| format!(", {} edges", c.edges))
                .unwrap_or_default()
        );
    }
    Outcome::Clean {
        runs,
        corpus: inputs.corpus.len(),
    }
}

/// One run of `target` over `input`: the message of a panic in it, caught
/// or not, or `None`.
fn run_one(target: &Target, run: u64, input: &[u8]) -> Option<String> {
    guard::arm(run, input);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (target.run)(input)));
    guard::disarm();
    let noted = guard::take_panic();
    match caught {
        Ok(()) => noted.map(|m| format!("a panic the code caught and went on from: {m}")),
        Err(_) => Some(noted.unwrap_or_else(|| "a panic".into())),
    }
}

/// Runs `target` once over each input, as a crash is reproduced: the
/// message of the first that panics.
pub fn replay(target: &'static Target, inputs: Vec<Vec<u8>>) -> Option<String> {
    fennec_core::one_thread();
    std::thread::Builder::new()
        .stack_size(RUN_STACK)
        .spawn(move || inputs.iter().find_map(|i| run_one(target, 0, i)))
        .expect("the run's thread")
        .join()
        .unwrap_or_else(|_| Some("the replay itself panicked".into()))
}

/// The target named `name`.
pub fn target(name: &str) -> Option<&'static Target> {
    targets::TARGETS.iter().find(|t| t.name == name)
}
