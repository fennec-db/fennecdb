//! The fuzzer's targets built as a plain build makes them -- no sanitizer,
//! no coverage -- and run over their seeds and a few hundred of their
//! mutations: what CI's test job runs, beside the `fuzz-smoke` job's half
//! minute of each under AddressSanitizer.

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
    seeds: || vec![b"bo".to_vec(), b"om".to_vec(), b"a longer seed".to_vec()],
    dictionary: &[],
    run: |b| {
        if b.windows(4).any(|w| w == b"boom") {
            std::process::abort();
        }
    },
};

/// Whatever ends a run says its recipe: an abort, from its handler with
/// system calls alone, and a panic; and a private run -- CI's -- says the
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
