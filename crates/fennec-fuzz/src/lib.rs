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
//! targets and runs one; `make fuzz-smoke` runs each for 30 s built with
//! AddressSanitizer and LLVM's coverage counters on a nightly compiler
//! pinned to a date -- CI's `fuzz-smoke` job, and `fuzz.yml` each week for
//! longer. A build without them (`cargo test`) runs every target over its
//! seeds and a few hundred of their mutations.

pub mod cover;
pub mod guard;
pub mod mutate;
pub mod targets;

use std::path::PathBuf;
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
    /// The most one allocation may ask for.
    pub max_alloc: usize,
    /// Progress lines, or none.
    pub quiet: bool,
}

/// How a fuzzer's runs ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every run answered or refused: so many runs, the corpus at its end.
    Clean { runs: u64, corpus: usize },
    /// A run panicked: its message, and where its input was written.
    Panicked { message: String, input: PathBuf },
}

/// The stack a run is on: a server's connection thread's
/// (`fennec_http::CONNECTION_STACK`), which a request parses, plans and
/// reads on, so that what overflows here overflows there.
pub const RUN_STACK: usize = 8 << 20;

/// Runs `target` as `opts` say, on a thread of [`RUN_STACK`].
pub fn fuzz(target: &'static Target, opts: Options) -> Outcome {
    std::thread::Builder::new()
        .name(format!("fuzz-{}", target.name))
        .stack_size(RUN_STACK)
        .spawn(move || fuzz_here(target, &opts))
        .expect("the run's thread")
        .join()
        .unwrap_or_else(|_| panic!("the fuzzer itself panicked"))
}

fn fuzz_here(target: &'static Target, opts: &Options) -> Outcome {
    std::fs::create_dir_all(&opts.crashes).expect("the crashes' directory");
    guard::install(&opts.crashes, target.name, opts.timeout, opts.max_alloc);
    // A panic before this fuzzer -- another's, a caller's -- is none of its
    // runs'.
    let _ = guard::take_panic();
    let mut seeds = (target.seeds)();
    if let Some(dir) = &opts.corpus {
        let _ = std::fs::create_dir_all(dir);
        let mut kept: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        kept.sort();
        seeds.extend(kept.iter().filter_map(|p| std::fs::read(p).ok()));
    }
    for s in &mut seeds {
        s.truncate(target.max_len);
    }
    seeds.sort();
    seeds.dedup();
    let mut dictionary: Vec<Vec<u8>> = target.dictionary.iter().map(|t| t.to_vec()).collect();
    dictionary.extend(mutate::tokens_of(&seeds, 512));
    let mut mutator = mutate::Mutator::new(opts.seed, dictionary, target.max_len);
    let mut cover = cover::Coverage::of_build();

    // The fuzzer's own code is instrumented beside the targets': its edges
    // are taken as seen before the first run, so a mutation taking another
    // path is no new coverage.
    if let Some(c) = cover.as_mut() {
        let mut scratch = seeds.first().cloned().unwrap_or_default();
        for _ in 0..2000 {
            mutator.mutate(&mut scratch, &seeds);
        }
        c.ignore_reached();
    }

    let started = Instant::now();
    let mut runs = 0u64;
    let mut corpus: Vec<Vec<u8>> = Vec::with_capacity(seeds.len());
    let mut last_report = Instant::now();
    let ended = |input: &[u8], message: String, runs: u64| -> Outcome {
        let name = format!("crash-{}-{:016x}", target.name, fnv(input));
        let path = opts.crashes.join(name);
        let _ = std::fs::write(&path, input);
        eprintln!(
            "fennec-fuzz: {} panicked after {runs} runs; its input is {}",
            target.name,
            path.display()
        );
        Outcome::Panicked {
            message,
            input: path,
        }
    };

    // Every seed once, each kept: the corpus starts from all of them.
    for s in seeds {
        if let Some(message) = run_one(target, &s) {
            return ended(&s, message, runs);
        }
        runs += 1;
        if let Some(c) = cover.as_mut() {
            c.take_new();
        }
        corpus.push(s);
    }
    if corpus.is_empty() {
        corpus.push(Vec::new());
    }
    if !opts.quiet {
        eprintln!(
            "fennec-fuzz: {}: {} seeds, {}",
            target.name,
            corpus.len(),
            match &cover {
                Some(c) => format!("{} of {} edges", c.edges, c.counters()),
                None => "a build without coverage: mutating unguided".into(),
            }
        );
    }

    let deadline = opts.seconds.map(|s| started + Duration::from_secs(s));
    let mut input = Vec::new();
    loop {
        if opts.runs.is_some_and(|n| runs >= n) || deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let parent = mutator.rng.below(corpus.len());
        input.clear();
        input.extend_from_slice(&corpus[parent]);
        // What the mutator reaches counts toward the run's scan below, and
        // is never new: its edges were taken as seen in every bucket.
        mutator.mutate(&mut input, &corpus);
        if let Some(message) = run_one(target, &input) {
            return ended(&input, message, runs);
        }
        runs += 1;
        if cover.as_mut().is_some_and(|c| c.take_new()) {
            if let Some(dir) = &opts.corpus {
                let _ = std::fs::write(dir.join(format!("{:016x}", fnv(&input))), &input);
            }
            corpus.push(input.clone());
        }
        if !opts.quiet && last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            let secs = started.elapsed().as_secs_f64();
            eprintln!(
                "fennec-fuzz: {}: {runs} runs, {:.0}/s, corpus {}{}",
                target.name,
                runs as f64 / secs,
                corpus.len(),
                cover
                    .as_ref()
                    .map(|c| format!(", {} edges", c.edges))
                    .unwrap_or_default()
            );
        }
    }
    if !opts.quiet {
        eprintln!(
            "fennec-fuzz: {}: done, {runs} runs in {:.1} s, corpus {}",
            target.name,
            started.elapsed().as_secs_f64(),
            corpus.len()
        );
    }
    Outcome::Clean {
        runs,
        corpus: corpus.len(),
    }
}

/// One run of `target` over `input`: the message of a panic in it, caught
/// or not, or `None`.
fn run_one(target: &Target, input: &[u8]) -> Option<String> {
    guard::arm(input);
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
    std::thread::Builder::new()
        .stack_size(RUN_STACK)
        .spawn(move || inputs.iter().find_map(|i| run_one(target, i)))
        .expect("the run's thread")
        .join()
        .unwrap_or_else(|_| Some("the replay itself panicked".into()))
}

/// The target named `name`.
pub fn target(name: &str) -> Option<&'static Target> {
    targets::TARGETS.iter().find(|t| t.name == name)
}

/// FNV-1a: an input's file name.
fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
