//! `fennec-fuzz`: the fuzzer's command line (docs/design/conventions.md#fuzzing).
//!
//!     fennec-fuzz list                       the targets, a line each
//!     fennec-fuzz names                      their names alone
//!     fennec-fuzz run <target> [options]     fuzz one: until a finding, or as long as asked
//!     fennec-fuzz replay <target> <file>...  run one over inputs, as a finding is reproduced
//!     fennec-fuzz reproduce <target> <recipe>  a finding's input made again from its recipe, and run
//!
//! run's options: --seconds N, --runs N, --seed N, --corpus DIR (kept
//! inputs, read at the start; default target/fuzz/corpus/<target>, `none`
//! for none), --crashes DIR (default target/fuzz/crashes), --timeout-ms N
//! (default 30000), --max-alloc-mb N (default 2048), --quiet, --private (a
//! finding says its recipe alone: CI's).
//!
//! A finding says its recipe -- `--seed N --start HASH [--corpus DIR] --run
//! N --fnv HASH --kept RUNS` -- which `reproduce` takes, writing the input
//! made again into the crashes' directory (`--out FILE` elsewhere) before
//! it runs it.
//!
//! The exit status is 0 when every run answered or refused, 1 on a panic,
//! 2 on an abort (a stack overflow, a failed allocation, AddressSanitizer's
//! report), 3 on a run past the timeout: the input is written either way.
//! `reproduce` exits 65 where the recipe makes no input of its hash.

use fennec_fuzz::targets::TARGETS;
use fennec_fuzz::{Options, Outcome};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: fennec-fuzz list | names | run <target> [--seconds N] [--runs N] \
    [--seed N] [--corpus DIR|none] [--crashes DIR] [--timeout-ms N] [--max-alloc-mb N] [--quiet] \
    [--private] | replay <target> <file>... | reproduce <target> --seed N --start HASH \
    [--corpus DIR] --run N --fnv HASH --kept RUNS [--out FILE]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("list") => {
            for t in TARGETS {
                println!("{:<10} {}", t.name, t.about);
            }
            ExitCode::SUCCESS
        }
        Some("names") => {
            for t in TARGETS {
                println!("{}", t.name);
            }
            ExitCode::SUCCESS
        }
        Some("run") => run(&args[1..]),
        Some("replay") => replay(&args[1..]),
        Some("reproduce") => reproduce(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    };
    fennec_fuzz::targets::clean_work();
    code
}

fn target(name: Option<&String>) -> Result<&'static fennec_fuzz::Target, ExitCode> {
    let Some(name) = name else {
        eprintln!("{USAGE}");
        return Err(ExitCode::from(64));
    };
    fennec_fuzz::target(name).ok_or_else(|| {
        let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
        eprintln!("fennec-fuzz: no target `{name}`: {}", names.join(", "));
        ExitCode::from(64)
    })
}

fn run(args: &[String]) -> ExitCode {
    let t = match target(args.first()) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/fuzz"));
    let mut opts = Options {
        seconds: None,
        runs: None,
        seed: 1,
        corpus: Some(root.join("corpus").join(t.name)),
        crashes: root.join("crashes"),
        // Wall time: a loop that never ends is caught at any bound, and at
        // 10 s a machine at a load of 40 took 8 000 numbers of 1e255, 2.5 s
        // of a quiet one's under ASan, for a hang.
        timeout: Duration::from_secs(30),
        max_alloc: 2048 << 20,
        quiet: false,
        private: false,
    };
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next().cloned().unwrap_or_else(|| {
                eprintln!("fennec-fuzz: {flag} takes a value\n{USAGE}");
                std::process::exit(64)
            })
        };
        let number = |v: String| -> u64 {
            v.parse().unwrap_or_else(|_| {
                eprintln!("fennec-fuzz: {flag} takes a number, not `{v}`");
                std::process::exit(64)
            })
        };
        match flag.as_str() {
            "--seconds" => opts.seconds = Some(number(value())),
            "--runs" => opts.runs = Some(number(value())),
            "--seed" => opts.seed = number(value()),
            "--corpus" => {
                opts.corpus = match value() {
                    v if v == "none" => None,
                    v => Some(PathBuf::from(v)),
                }
            }
            "--crashes" => opts.crashes = PathBuf::from(value()),
            "--timeout-ms" => opts.timeout = Duration::from_millis(number(value())),
            "--max-alloc-mb" => opts.max_alloc = (number(value()) as usize).saturating_mul(1 << 20),
            "--quiet" => opts.quiet = true,
            "--private" => opts.private = true,
            other => {
                eprintln!("fennec-fuzz: unknown flag `{other}`\n{USAGE}");
                return ExitCode::from(64);
            }
        }
    }
    match fennec_fuzz::fuzz(t, opts) {
        Outcome::Clean { .. } => ExitCode::SUCCESS,
        Outcome::Panicked { .. } => ExitCode::from(1),
    }
}

fn replay(args: &[String]) -> ExitCode {
    let t = match target(args.first()) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let mut inputs = Vec::new();
    for f in &args[1..] {
        match std::fs::read(f) {
            Ok(b) => inputs.push(b),
            Err(e) => {
                eprintln!("fennec-fuzz: {f}: {e}");
                return ExitCode::from(66);
            }
        }
    }
    replay_inputs(t, inputs)
}

/// A finding's input made again from the recipe it said, written where
/// `--out` says or into the crashes' directory, and run as `replay` runs
/// one -- under the build's AddressSanitizer where it carries it.
fn reproduce(args: &[String]) -> ExitCode {
    let t = match target(args.first()) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let (recipe, out) = match fennec_fuzz::Recipe::from_args(&args[1..]) {
        Ok(read) => read,
        Err(why) => {
            eprintln!("fennec-fuzz: {why}\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    let input = match fennec_fuzz::reproduce(t, &recipe) {
        Ok(input) => input,
        Err(why) => {
            eprintln!("fennec-fuzz: {}: {why}", t.name);
            return ExitCode::from(65);
        }
    };
    let path = out.unwrap_or_else(|| {
        let dir = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/fuzz/crashes"
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("reproduced-{}-{:016x}", t.name, recipe.fnv))
    });
    if let Err(e) = std::fs::write(&path, &input) {
        eprintln!("fennec-fuzz: {}: {e}", path.display());
        return ExitCode::from(73);
    }
    println!(
        "fennec-fuzz: {}: run {}'s input, {} bytes, is {}",
        t.name,
        recipe.run,
        input.len(),
        path.display()
    );
    replay_inputs(t, vec![input])
}

fn replay_inputs(t: &'static fennec_fuzz::Target, inputs: Vec<Vec<u8>>) -> ExitCode {
    // An abort or a hang ends a replay as it ends a run; what it would keep
    // is the input given, so it goes in a directory of its own, removed.
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-replay-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    fennec_fuzz::guard::install(&dir, t.name, Duration::from_secs(60), usize::MAX, false);
    let found = fennec_fuzz::replay(t, inputs);
    let _ = std::fs::remove_dir_all(&dir);
    match found {
        None => {
            println!("fennec-fuzz: {}: every input answered or refused", t.name);
            ExitCode::SUCCESS
        }
        Some(message) => {
            eprintln!("fennec-fuzz: {}: {message}", t.name);
            ExitCode::from(1)
        }
    }
}
