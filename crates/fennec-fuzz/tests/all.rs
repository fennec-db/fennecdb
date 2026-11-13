//! The fuzzer's targets built as a plain build makes them -- no sanitizer,
//! no coverage -- and run over their seeds and a few hundred of their
//! mutations: what CI's test job runs, beside the `fuzz-smoke` job's half
//! minute of each under AddressSanitizer.

use fennec_fuzz::targets::TARGETS;
use fennec_fuzz::{Options, Outcome, Target};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The fuzzer keeps the run in flight in the process's own state (what an
/// abort's handler reads): one fuzzing test at a time.
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
        Outcome::Panicked { message, input } => {
            assert!(message.contains("boom"), "{message}");
            assert_eq!(std::fs::read(&input).unwrap(), b"boom");
        }
        clean => panic!("{clean:?}"),
    }
    assert_eq!(fennec_fuzz::replay(&BOOM, vec![b"xx".to_vec()]), None);
    assert!(fennec_fuzz::replay(&BOOM, vec![b"boom".to_vec()]).is_some());
    std::fs::remove_dir_all(&crashes).unwrap();
}

/// The weekly run fuzzes every target: its matrix names each, and only
/// those.
#[test]
fn the_weekly_workflow_runs_every_target() {
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
