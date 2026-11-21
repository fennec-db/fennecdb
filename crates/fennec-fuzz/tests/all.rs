//! The fuzzer's targets built as a plain build makes them -- no sanitizer,
//! no coverage -- and run over their seeds and a few hundred of their
//! mutations: what CI's test job runs, beside the `fuzz-smoke` job's count
//! of runs of each under AddressSanitizer.

use fennec_fuzz::targets::TARGETS;
use fennec_fuzz::{guard, Inputs, Options, Outcome, Recipe, Target};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The fuzzer keeps the run in flight in the process's own state (what an
/// abort's handler reads), and its panic hook notes any thread's panic as
/// the run's: one test at a time, every test of the file, so that one
/// failing beside a fuzzing test is never taken for that test's finding.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fennec-fuzz-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn options(crashes: PathBuf, runs: u64) -> Options {
    Options {
        seconds: None,
        runs: Some(runs),
        seed: 7,
        corpus: None,
        crashes,
        timeout: Duration::from_secs(120),
        cap: None,
        max_alloc: 2048 << 20,
        quiet: true,
        private: false,
    }
}

#[test]
fn every_target_runs_its_seeds_and_their_mutations_clean() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let crashes = scratch("crashes");
    for t in TARGETS {
        let seeds = (t.seeds)();
        assert!(!seeds.is_empty(), "{}: no seeds", t.name);
        let runs = seeds.len() as u64 + 300;
        match fennec_fuzz::fuzz(t, options(crashes.clone(), runs)) {
            Outcome::Clean { runs: done, .. } => assert!(done >= runs, "{}: {done} runs", t.name),
            found => panic!("{}: {found:?}", t.name),
        }
    }
    std::fs::remove_dir_all(&crashes).unwrap();
    fennec_fuzz::targets::clean_work();
}

static BOOM: Target = Target {
    name: "boom",
    about: "panics on an input holding `boom`",
    max_len: 64,
    smoke: 1000,
    seeds: || vec![b"bo".to_vec(), b"om".to_vec(), b"boom".to_vec()],
    dictionary: &[],
    run: |b| {
        if b.windows(4).any(|w| w == b"boom") {
            panic!("boom");
        }
    },
};

/// A run that panics ends the fuzzer, its input written where it says.
#[test]
fn a_panic_ends_the_run_and_its_input_is_kept() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let crashes = scratch("boom");
    match fennec_fuzz::fuzz(&BOOM, options(crashes.clone(), 1000)) {
        Outcome::Panicked { message, input, .. } => {
            assert!(message.contains("boom"), "{message}");
            assert_eq!(std::fs::read(&input).unwrap(), b"boom");
        }
        clean => panic!("{clean:?}"),
    }
    assert_eq!(fennec_fuzz::replay(&BOOM, vec![b"xx".to_vec()]), None);
    assert!(fennec_fuzz::replay(&BOOM, vec![b"boom".to_vec()]).is_some());
    std::fs::remove_dir_all(&crashes).unwrap();
}

static HALVES: Target = Target {
    name: "halves",
    about: "panics on an input holding `boom`, seeded with its halves",
    max_len: 64,
    smoke: 1000,
    seeds: || vec![b"bo".to_vec(), b"om".to_vec(), b"a longer seed".to_vec()],
    dictionary: &[],
    run: |b| {
        if b.windows(4).any(|w| w == b"boom") {
            panic!("boom");
        }
    },
};

/// A finding is made again from the recipe it says alone -- CI keeps no
/// input, its logs being public -- read as the recipe's own text, and
/// nothing but that recipe makes it: another start, or another hash, is
/// refused rather than handed back as some other input.
#[test]
fn a_finding_is_made_again_from_its_recipe() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let crashes = scratch("recipe");
    let (found, run) = match fennec_fuzz::fuzz(&HALVES, options(crashes.clone(), 1_000_000)) {
        Outcome::Panicked { input, run, .. } => (std::fs::read(&input).unwrap(), run),
        clean => panic!("{clean:?}"),
    };
    let starts = fennec_fuzz::start_of(&HALVES, None).len() as u64;
    assert!(run >= starts, "a mutation's, not a seed's: run {run}");
    // The recipe as a finding says it, its flags read back.
    let mut said = Vec::new();
    guard::recipe_into(b"a panic", &mut |b| said.extend_from_slice(b));
    let said = String::from_utf8(said).unwrap();
    let flags: Vec<String> = said
        .split_once("fennec-fuzz reproduce halves ")
        .unwrap_or_else(|| panic!("{said}"))
        .1
        .split_whitespace()
        .map(String::from)
        .collect();
    let (recipe, out) = Recipe::from_args(&flags).unwrap();
    assert_eq!(out, None);
    assert_eq!(
        recipe,
        Recipe {
            seed: 7,
            start: fennec_fuzz::fingerprint(&fennec_fuzz::start_of(&HALVES, None)),
            corpus: None,
            run,
            fnv: guard::fnv(&found),
            kept: Vec::new(),
        }
    );
    assert!(said.starts_with(&format!("fennec-fuzz: halves: a panic at run {run}, ")));
    assert_eq!(fennec_fuzz::reproduce(&HALVES, &recipe).unwrap(), found);
    for other in [
        Recipe {
            start: recipe.start ^ 1,
            ..recipe.clone()
        },
        Recipe {
            fnv: recipe.fnv ^ 1,
            ..recipe.clone()
        },
    ] {
        assert!(
            fennec_fuzz::reproduce(&HALVES, &other).is_err(),
            "{other:?}"
        );
    }
    std::fs::remove_dir_all(&crashes).unwrap();
}

static ABORTS: Target = Target {
    name: "aborts",
    about: "aborts on an input holding `boom`, seeded with its halves",
    max_len: 64,
    smoke: 1000,
    seeds: || vec![b"bo".to_vec(), b"om".to_vec(), b"a longer seed".to_vec()],
    dictionary: &[],
    run: |b| {
        if b.windows(4).any(|w| w == b"boom") {
            std::process::abort();
        }
    },
};

/// Whatever ends a run says its recipe: an abort, from its handler with
/// system calls alone, and a panic; and a private run -- the weekly one's -- says the
/// recipe alone, never the panic's message. Each fuzzes in a child, this
/// test binary run again, its standard error read and its recipe made
/// into the input again.
#[test]
fn an_abort_and_a_private_panic_say_their_recipe() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const TEST: &str = "an_abort_and_a_private_panic_say_their_recipe";
    if let Ok(child) = std::env::var("FENNEC_FUZZ_CHILD") {
        let dir = PathBuf::from(std::env::var("FENNEC_FUZZ_CHILD_DIR").unwrap());
        let mut opts = options(dir, 1_000_000);
        opts.private = true;
        let target = if child == "aborts" { &ABORTS } else { &HALVES };
        let outcome = fennec_fuzz::fuzz(target, opts);
        assert!(child == "halves", "{child} went on: {outcome:?}");
        return;
    }
    for (target, code) in [(&ABORTS, 2), (&HALVES, 0)] {
        let dir = scratch(&format!("child-{}", target.name));
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env("FENNEC_FUZZ_CHILD", target.name)
            .env("FENNEC_FUZZ_CHILD_DIR", &dir)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{err}");
        // A private run's panic says no message: "boom" is the panic's.
        assert!(!err.contains("boom"), "{err}");
        let command = err
            .lines()
            .skip_while(|l| !l.ends_with("made again by"))
            .nth(1)
            .unwrap_or_else(|| panic!("no recipe: {err}"));
        let flags: Vec<String> = command
            .split_once(&format!("fennec-fuzz reproduce {} ", target.name))
            .unwrap_or_else(|| panic!("{command}"))
            .1
            .split_whitespace()
            .map(String::from)
            .collect();
        let (recipe, _) = Recipe::from_args(&flags).unwrap();
        let input = fennec_fuzz::reproduce(target, &recipe).unwrap();
        assert!(input.windows(4).any(|w| w == b"boom"), "{input:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Each target's start is the same bytes in every process -- no clock, no
/// path, no hash map's order in a seed, the `image` target's files made by
/// the engine among them -- or no recipe a CI run says would make its input
/// again elsewhere: two processes' fingerprints of each start agree.
#[test]
fn every_targets_start_is_the_same_in_another_process() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const TEST: &str = "every_targets_start_is_the_same_in_another_process";
    if std::env::var_os("FENNEC_FUZZ_STARTS").is_some() {
        for t in TARGETS {
            let start = fennec_fuzz::start_of(t, None);
            println!(
                "[start {} {:016x}]",
                t.name,
                fennec_fuzz::fingerprint(&start)
            );
        }
        fennec_fuzz::targets::clean_work();
        return;
    }
    // Each in brackets, wherever on a line: the first follows the test's
    // name on the line the harness began.
    let starts = || {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env("FENNEC_FUZZ_STARTS", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .split("[start ")
            .skip(1)
            .filter_map(|s| s.split_once(']').map(|(start, _)| start.to_string()))
            .collect::<Vec<_>>()
    };
    let first = starts();
    assert_eq!(first.len(), TARGETS.len(), "{first:?}");
    assert_eq!(first, starts());
}

/// What the corpus took is part of the recipe: a run's input is its seed's
/// and its start's, and the runs whose inputs the corpus took before it,
/// whatever coverage said so -- made again from those on a build that
/// counts none -- and another list of them makes others.
#[test]
fn a_runs_input_follows_the_runs_kept_before_it() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let start = fennec_fuzz::start_of(&HALVES, None);
    let mut inputs = Inputs::new(&HALVES, 11, start.clone());
    let (mut kept, mut seen) = (Vec::new(), Vec::new());
    for _ in 0..3000 {
        let run = inputs.run();
        seen.push(inputs.next_input().to_vec());
        if run % 7 == 1 && inputs.keep() {
            kept.push(run);
        }
    }
    // A seed is the corpus's whatever it reached, so no recipe names it:
    // run 1 is the second of three.
    assert_eq!(kept[..2], [8, 15]);
    let text = fennec_fuzz::kept_text(&kept);
    assert_eq!(fennec_fuzz::kept_from(&text).unwrap(), kept);
    assert_eq!(fennec_fuzz::kept_text(&[]), "-");
    assert_eq!(fennec_fuzz::kept_from("-").unwrap(), Vec::<u64>::new());
    let recipe = |run: u64, kept: Vec<u64>| Recipe {
        seed: 11,
        start: fennec_fuzz::fingerprint(&start),
        corpus: None,
        run,
        fnv: guard::fnv(&seen[run as usize]),
        kept,
    };
    for run in [0, 2, 3, 500, 2999] {
        assert_eq!(
            fennec_fuzz::reproduce(&HALVES, &recipe(run, kept.clone())).unwrap(),
            seen[run as usize],
            "run {run}"
        );
    }
    assert!(fennec_fuzz::reproduce(&HALVES, &recipe(2999, Vec::new())).is_err());
}

static HANGS: Target = Target {
    name: "hangs",
    about: "never returns on an input holding `boom`, seeded with its halves",
    max_len: 64,
    smoke: 1000,
    seeds: || vec![b"bo".to_vec(), b"om".to_vec(), b"a longer seed".to_vec()],
    dictionary: &[],
    run: |b| {
        if b.windows(4).any(|w| w == b"boom") {
            loop {
                std::thread::park();
            }
        }
    },
};

/// A run past its timeout ends the fuzzer, its input written by the
/// handler of the timer the run set -- no thread watches beside the runs,
/// whose code the coverage would count -- and its recipe said, which
/// makes the input again. The fuzzer runs in a child, this test binary run
/// again; the child's end is the event waited for.
#[test]
fn a_run_past_its_timeout_ends_the_fuzzer_and_its_input_is_kept() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const TEST: &str = "a_run_past_its_timeout_ends_the_fuzzer_and_its_input_is_kept";
    if std::env::var_os("FENNEC_FUZZ_CHILD").is_some() {
        let dir = PathBuf::from(std::env::var("FENNEC_FUZZ_CHILD_DIR").unwrap());
        let mut opts = options(dir, 1_000_000);
        opts.timeout = Duration::from_millis(200);
        let outcome = fennec_fuzz::fuzz(&HANGS, opts);
        panic!("the child went on: {outcome:?}");
    }
    let dir = scratch("child-hangs");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env("FENNEC_FUZZ_CHILD", "hangs")
        .env("FENNEC_FUZZ_CHILD_DIR", &dir)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("a run took more than 200 ms"), "{err}");
    let kept: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let input = std::fs::read(&kept[0]).unwrap();
    assert!(input.windows(4).any(|w| w == b"boom"), "{input:?}");
    let command = err
        .lines()
        .skip_while(|l| !l.ends_with("made again by"))
        .nth(1)
        .unwrap_or_else(|| panic!("no recipe: {err}"));
    let flags: Vec<String> = command
        .split_once("fennec-fuzz reproduce hangs ")
        .unwrap_or_else(|| panic!("{command}"))
        .1
        .split_whitespace()
        .map(String::from)
        .collect();
    let (recipe, _) = Recipe::from_args(&flags).unwrap();
    assert_eq!(fennec_fuzz::reproduce(&HANGS, &recipe).unwrap(), input);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Spends a fixed sum of work a run, whatever the input -- CPU time, not
/// the wall's, so a loaded machine spends the same -- as a commit that
/// slowed every run would.
static SLOW: Target = Target {
    name: "slow",
    about: "a few milliseconds of work a run, whatever the input",
    max_len: 64,
    smoke: 1000,
    seeds: || vec![b"x".to_vec()],
    dictionary: &[],
    run: |_| {
        let mut h = 0u64;
        for i in 0..2_000_000u64 {
            h = std::hint::black_box(h.wrapping_mul(31).wrapping_add(i));
        }
        std::hint::black_box(h);
    },
};

/// A fuzzer past its cap of CPU time ends failing, by the handler of a
/// timer of the process's CPU time -- no thread watches beside the runs --
/// saying the target and the run it reached of its count, where a count
/// of runs alone ran on to its job's timeout, naming no target. The cap is
/// a bound, never a pass: without it the child would end its 1 000 runs
/// and say it went on. The child's end is the event waited for.
#[test]
fn a_fuzzer_past_its_cap_of_cpu_time_ends_naming_the_target_and_its_run() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    const TEST: &str = "a_fuzzer_past_its_cap_of_cpu_time_ends_naming_the_target_and_its_run";
    if std::env::var_os("FENNEC_FUZZ_CHILD").is_some() {
        let dir = PathBuf::from(std::env::var("FENNEC_FUZZ_CHILD_DIR").unwrap());
        let mut opts = options(dir, 1000);
        opts.cap = Some(Duration::from_millis(300));
        let outcome = fennec_fuzz::fuzz(&SLOW, opts);
        panic!("the child went on: {outcome:?}");
    }
    let dir = scratch("child-slow");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env("FENNEC_FUZZ_CHILD", "slow")
        .env("FENNEC_FUZZ_CHILD_DIR", &dir)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "{err}");
    let said = err
        .lines()
        .find_map(|l| {
            l.strip_prefix("fennec-fuzz: slow: past its cap of 300 ms of CPU time at run ")
        })
        .unwrap_or_else(|| panic!("no cap said: {err}"));
    let (run, rest) = said.split_once(" of ").unwrap_or_else(|| panic!("{said}"));
    let run: u64 = run.parse().unwrap_or_else(|_| panic!("{said}"));
    assert!(run < 1000 && rest.starts_with("1000: "), "{said}");
    // No input kept: the run in flight is one of many that slowed.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Reads what is left of the cap of CPU time as it runs: the most any of
/// its runs read.
static PEEK: Target = Target {
    name: "peek",
    about: "reads the cap of CPU time its runs are under",
    max_len: 64,
    smoke: 3,
    seeds: || vec![b"x".to_vec()],
    dictionary: &[],
    run: |_| {
        let left = guard::cap_left().as_micros() as u64;
        PEEKED.fetch_max(left, std::sync::atomic::Ordering::Relaxed);
    },
};
static PEEKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `make fuzz-smoke` runs each target with `--smoke`, which gives it its
/// count of runs and arms the cap of CPU time its runs are under, read
/// off the timer by a run: the child test above holds the timer to the
/// process's end, this one the smoke to the timer. Stopped once the
/// fuzzer returns.
#[test]
fn the_smoke_runs_each_target_its_count_under_its_cap_of_cpu_time() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let make = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Makefile");
    let make = std::fs::read_to_string(make).unwrap();
    let recipe: Vec<&str> = make
        .lines()
        .skip_while(|l| !l.starts_with("fuzz-smoke:"))
        .take_while(|l| !l.is_empty())
        .collect();
    assert!(
        recipe.iter().any(|l| l.contains(",--smoke) --corpus none")),
        "{recipe:?}"
    );
    let flags = |more: &[&str]| -> Vec<String> {
        ["--smoke", "--corpus", "none"]
            .iter()
            .chain(more)
            .map(|f| f.to_string())
            .collect()
    };
    for t in TARGETS {
        let opts = fennec_fuzz::run_options(t, &flags(&[])).unwrap();
        assert_eq!(
            (opts.runs, opts.seconds, opts.cap, opts.corpus),
            (Some(t.smoke), None, Some(fennec_fuzz::SMOKE_CAP), None),
            "{}",
            t.name
        );
    }
    let dir = scratch("peek");
    let crashes = dir.to_str().unwrap();
    let opts = fennec_fuzz::run_options(&PEEK, &flags(&["--crashes", crashes, "--quiet"])).unwrap();
    assert_eq!(
        fennec_fuzz::fuzz(&PEEK, opts),
        Outcome::Clean { runs: 3, corpus: 1 }
    );
    let peeked = Duration::from_micros(PEEKED.load(std::sync::atomic::Ordering::Relaxed));
    // Linux adds a tick (1/HZ, 1 to 10 ms) to a CPU timer it is handed, so
    // that a clock it reads a tick at a time never fires it sooner than
    // asked (`set_cpu_itimer`): read back at once, the timer stands above
    // the cap by that tick, less the CPU time since -- 360.001 s on both
    // Linux runners, where macOS reads 360 s or less.
    let tick = Duration::from_millis(10);
    assert!(
        peeked > fennec_fuzz::SMOKE_CAP / 2 && peeked <= fennec_fuzz::SMOKE_CAP + tick,
        "{peeked:?}"
    );
    assert_eq!(guard::cap_left(), Duration::ZERO);
    // Without it, no cap.
    let opts = fennec_fuzz::run_options(&PEEK, &["--runs".to_string(), "3".to_string()]).unwrap();
    assert_eq!((opts.runs, opts.cap), (Some(3), None));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Panics as it is dropped: a run's panic carrying it is caught, and
/// dropping it after panics outside the catch, as a bug of the fuzzer's
/// own would -- on its thread, past the point its cap was set.
struct Bomb;

impl Drop for Bomb {
    fn drop(&mut self) {
        panic!("a panic's payload that panics as it is dropped");
    }
}

static BOMB: Target = Target {
    name: "bomb",
    about: "a panic the fuzzer's own thread does not catch",
    max_len: 64,
    smoke: 10,
    seeds: || vec![b"x".to_vec()],
    dictionary: &[],
    run: |_| std::panic::panic_any(Bomb),
};

/// A fuzzer whose own thread panics stops the timers it set as it unwinds:
/// the call after its thread's join that stopped its cap was skipped, and
/// a test binary's tests after it ran under a cap none of them set, which
/// ends the process naming that fuzzer's target. An hour here, so that a
/// timer left set fails this test, not the binary.
#[test]
fn a_fuzzer_whose_own_thread_panics_stops_its_cap() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("bomb");
    let mut opts = options(dir.clone(), 10);
    opts.cap = Some(Duration::from_secs(3600));
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fennec_fuzz::fuzz(&BOMB, opts)
    }));
    assert!(ended.is_err(), "{ended:?}");
    assert_eq!(guard::cap_left(), Duration::ZERO);
    let _ = guard::take_panic();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The weekly run fuzzes every target: its matrix names each, and only
/// those.
#[test]
fn the_weekly_workflow_runs_every_target() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/fuzz.yml"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("target: ["))
        .expect("fuzz.yml's matrix line, `target: [...]`");
    let listed: Vec<&str> = line
        .split_once('[')
        .and_then(|(_, r)| r.split_once(']'))
        .map(|(names, _)| names.split(',').map(str::trim).collect())
        .unwrap_or_default();
    let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
    assert_eq!(listed, names);
    // `make fuzz` lists at least the five the plan asks of 1.0.
    assert!(names.len() >= 5);
}
