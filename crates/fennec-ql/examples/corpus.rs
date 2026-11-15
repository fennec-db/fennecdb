//! FennecQL's corpus, parsed: `tools/fennecql_corpus.py` (`make
//! fennecql-corpus BASE=origin/main`) builds this in the tree it checks and
//! in BASE's, hands it every FennecQL text the repository holds, one JSON
//! string a line on stdin, and compares the lines it writes
//! (docs/design/fennecql.md#the-corpus).
//!
//! Its first line is `epoch <GRAMMAR_EPOCH>`. Then a line a text: its read
//! by `parse`, by `parse_exact` and by `stored_definition` -- the digest an
//! open holds a stored definition's record to -- tab-separated, each `ok`
//! and a word a statement, `err <the refusal>` or `panic`; the third is `-`
//! for a text that is no one definition. A word is a letter and the 16 hex
//! digits of a hash:
//!
//! - `c`: a statement a named statement's body may hold -- `get`, `put`,
//!   `insert`, `set`, `del` and SQL's spellings of them -- as the digest of
//!   a definition whose body it is alone (`Definition::digest`);
//! - `e`: an `explain`, its `get`'s digest so;
//! - `s`: a definition, its own digest -- what its record holds and an open
//!   holds its text to -- `r` after it for `create or replace`, which the
//!   digest leaves out;
//! - `d`: any other statement -- a schema change, a listing, `call` -- a
//!   hash of its `Debug` form, which a field added to its tree leaves as it
//!   was by writing itself only where it holds something (`Field`'s,
//!   `TextIndexSpec`'s).
//!
//! The canonical tree writes a field only where it holds something, so a
//! statement compares across trees one of which has fields the other lacks:
//! what a `Debug` form, comparing every field, could not. It reads nothing
//! but what every tree since v0.2.0 exports, since the same file is copied
//! into BASE's to build there.

use fennec_core::named::Definition;
use fennec_core::query::Statement;
use std::io::{BufRead, Write};

fn one_line(s: String) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// FNV-1a: a `Debug` form's hash, written here so the word is the same in
/// every tree whatever its crates hash with.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A definition's digest, `r` after it where it replaces.
fn defined(d: &Definition) -> String {
    match d.digest() {
        Ok(h) => format!("s{h:016x}{}", if d.replace { "r" } else { "" }),
        Err(_) => format!("d{:016x}", fnv(&format!("{d:?}"))),
    }
}

/// `body` as the digest of a definition whose body it is alone: the
/// template's name and nothing else around it, the same bytes in every
/// tree.
fn body(template: &Definition, letter: char, s: Statement) -> String {
    let mut d = template.clone();
    d.body = vec![s];
    match d.digest() {
        Ok(h) => format!("{letter}{h:016x}"),
        Err(_) => format!("d{:016x}", fnv(&format!("{:?}", d.body))),
    }
}

fn word(template: &Definition, s: Statement) -> String {
    match s {
        Statement::Select(_)
        | Statement::Put { .. }
        | Statement::Update { .. }
        | Statement::Delete { .. } => body(template, 'c', s),
        Statement::Explain(sel) => body(template, 'e', Statement::Select(sel)),
        Statement::CreateStatement(d) => defined(&d),
        other => format!("d{:016x}", fnv(&format!("{other:?}"))),
    }
}

/// One reading, a panic in the parser among the answers: a text it panics
/// on is named, not the run.
fn read(
    template: &Definition,
    f: impl FnOnce() -> fennec_core::error::Result<Vec<Statement>>,
) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(statements)) => {
            let mut out = String::from("ok");
            for s in statements {
                out.push(' ');
                out.push_str(&word(template, s));
            }
            out
        }
        Ok(Err(e)) => one_line(format!("err {e}")),
        Err(_) => "panic".into(),
    }
}

/// The text as an open reads a stored definition, where it is one alone.
fn stored(text: &str) -> String {
    let read = || {
        if !matches!(
            fennec_ql::parse(text).as_deref(),
            Ok([Statement::CreateStatement(_)])
        ) {
            return "-".into();
        }
        // The digest an open holds the record to, the quick read's; the
        // tree it runs, read as written, is `parse_exact`'s.
        match fennec_ql::stored_definition(text) {
            Ok((h, d)) => format!("ok s{h:016x}{}", if d.replace { "r" } else { "" }),
            Err(e) => one_line(format!("err {e}")),
        }
    };
    std::panic::catch_unwind(read).unwrap_or_else(|_| "panic".into())
}

fn run() {
    let template = match fennec_ql::parse_one("create statement corpus() as get corpus select id") {
        Ok(Statement::CreateStatement(d)) => *d,
        other => panic!("the template reads as no definition: {other:?}"),
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    writeln!(out, "epoch {}", fennec_ql::GRAMMAR_EPOCH).expect("stdout");
    for line in stdin.lock().lines() {
        let line = line.expect("a line of stdin");
        let text = match fennec_core::json::parse(&line) {
            Ok(fennec_core::value::Value::Text(t)) => t.to_string(),
            other => panic!("not a JSON string: {line}: {other:?}"),
        };
        let parsed = read(&template, || fennec_ql::parse(&text));
        let exact = read(&template, || fennec_ql::parse_exact(&text));
        writeln!(out, "{parsed}\t{exact}\t{}", stored(&text)).expect("stdout");
    }
}

fn main() {
    // Built without optimisation, the parser's frames are larger: the depth
    // test's 512 levels outgrew 8 MiB of a debug build's stack (fennecql.md,
    // *SQL spellings*).
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .expect("a thread")
        .join()
        .expect("the corpus");
}
