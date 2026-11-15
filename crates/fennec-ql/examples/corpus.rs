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
//!   hash of its [`form`]: its pretty `Debug`, a named field holding an
//!   empty value left out, as the canonical form leaves one out.
//!
//! Run as `corpus forms`, each line holds the statements' forms instead of
//! their words, for the tool to print what changed in a text that failed.
//!
//! The canonical tree writes a field only where it holds something, and the
//! form leaves such a field's line out, so a statement compares across
//! trees one of which has fields the other lacks: what a `Debug` form,
//! comparing every field, could not. It reads nothing but what every tree
//! since v0.2.0 exports, since the same file is copied into BASE's to build
//! there.

use fennec_core::error::Result;
use fennec_core::named::Definition;
use fennec_core::query::Statement;
use std::fmt::Debug;
use std::io::{BufRead, Write};

fn one_line(s: String) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// FNV-1a: a form's hash, written here so the word is the same in every
/// tree whatever its crates hash with.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The values a named field's line is left out of a [`form`] for, as
/// `{:#?}` writes them: the canonical form's own rule (`named.rs`'s `text`,
/// `flag`, `num`, `list`, `opt_*` writers), so a field added that holds
/// nothing leaves the form as it was. A value going from one of these to
/// another -- `0` to `None`, a field's type changed under it -- is not
/// seen; nor is a field taken away that held one of them.
const EMPTY: [&str; 6] = ["None,", "false,", "0,", "[],", "\"\",", "{},"];

/// Whether `line` of a pretty `Debug` is a named field holding an empty
/// value: `name: None,` and the rest of [`EMPTY`]. A positional one -- a
/// tuple's `0,` -- keeps its line, its place being what it means.
fn empty_field(line: &str) -> bool {
    let Some((name, value)) = line.trim_start().split_once(": ") else {
        return false;
    };
    let name = name.strip_prefix("r#").unwrap_or(name);
    let ident = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ident && EMPTY.contains(&value)
}

/// `x`'s pretty `Debug`, each named field holding an empty value left out
/// ([`empty_field`]), and a struct whose every field was left out closed
/// as its name alone -- `Collections {` and its `}` as `Collections` -- so a
/// unit variant given a field that holds nothing reads as it did. What a
/// `d` word hashes and a failure prints.
fn form(x: &impl Debug) -> String {
    let pretty = format!("{x:#?}");
    let mut out: Vec<String> = Vec::new();
    for line in pretty.lines() {
        if empty_field(line) {
            continue;
        }
        let close = line.trim_start();
        if close == "}" || close == "}," {
            let indent = &line[..line.len() - close.len()];
            if let Some(open) = out.last_mut() {
                if open.ends_with(" {") && open.len() - open.trim_start().len() == indent.len() {
                    open.truncate(open.len() - 2);
                    if close == "}," {
                        open.push(',');
                    }
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

fn hashed(x: &impl Debug) -> String {
    format!("d{:016x}", fnv(&form(x)))
}

/// A definition's digest, `r` after it where it replaces.
fn defined(d: &Definition) -> String {
    match d.digest() {
        Ok(h) => format!("s{h:016x}{}", if d.replace { "r" } else { "" }),
        Err(_) => hashed(d),
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
        Err(_) => hashed(&d.body),
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
        other => hashed(&other),
    }
}

/// A statement's form, as `word` reads it: an `explain`'s `get`, a
/// definition whole, any other as it stands.
fn shown(s: Statement) -> String {
    match s {
        Statement::Explain(sel) => form(&sel),
        Statement::CreateStatement(d) => form(&d),
        other => form(&other),
    }
}

/// How a line writes each statement read: its word, or (`corpus forms`) its
/// form, the forms of one reading apart by `\x1e`, which no `Debug` writes
/// raw.
#[derive(Clone, Copy)]
enum How {
    Words,
    Forms,
}

/// One reading, a panic in the parser among the answers: a text it panics
/// on is named, not the run.
fn read(template: &Definition, how: How, f: impl FnOnce() -> Result<Vec<Statement>>) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(statements)) => {
            let mut out = String::from("ok");
            for s in statements {
                match how {
                    How::Words => {
                        out.push(' ');
                        out.push_str(&word(template, s));
                    }
                    How::Forms => {
                        out.push('\x1e');
                        out.push_str(&shown(s));
                    }
                }
            }
            one_line(out)
        }
        Ok(Err(e)) => one_line(format!("err {e}")),
        Err(_) => "panic".into(),
    }
}

/// The text as an open reads a stored definition: where `parse` reads it as
/// one definition, and where `parse` refuses it -- a text refused afresh
/// that the stored reader still reads, as one stored before the refusal is
/// read (`stored_definition`'s own reason to differ). A refusal there is
/// `-`, a text no definition; one `parse` read as a definition and the
/// stored reader refuses, `err`.
fn stored(
    text: &str,
    how: How,
    parse: impl FnOnce(&str) -> Result<Vec<Statement>>,
    stored: impl FnOnce(&str) -> Result<(u64, Definition)>,
) -> String {
    let read = move || {
        let refused = match parse(text).as_deref() {
            Ok([Statement::CreateStatement(_)]) => false,
            Ok(_) => return "-".into(),
            Err(_) => true,
        };
        // The digest an open holds the record to, the quick read's; the
        // tree it runs, read as written, is `parse_exact`'s.
        match (stored(text), how) {
            (Ok((h, d)), How::Words) => {
                format!("ok s{h:016x}{}", if d.replace { "r" } else { "" })
            }
            (Ok((_, d)), How::Forms) => one_line(format!("ok\x1e{}", form(&d))),
            (Err(_), _) if refused => "-".into(),
            (Err(e), _) => one_line(format!("err {e}")),
        }
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).unwrap_or_else(|_| "panic".into())
}

/// The definition a body is digested in alone, the same bytes in every
/// tree.
fn template() -> Definition {
    match fennec_ql::parse_one("create statement corpus() as get corpus select id") {
        Ok(Statement::CreateStatement(d)) => *d,
        other => panic!("the template reads as no definition: {other:?}"),
    }
}

fn run(how: How) {
    let template = template();
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
        let parsed = read(&template, how, || fennec_ql::parse(&text));
        let exact = read(&template, how, || fennec_ql::parse_exact(&text));
        let kept = stored(&text, how, fennec_ql::parse, fennec_ql::stored_definition);
        writeln!(out, "{parsed}\t{exact}\t{kept}").expect("stdout");
    }
}

fn main() {
    let how = match std::env::args().nth(1).as_deref() {
        None => How::Words,
        Some("forms") => How::Forms,
        Some(other) => panic!("corpus takes `forms` or nothing, not {other}"),
    };
    // Built without optimisation, the parser's frames are larger: the depth
    // test's 512 levels outgrew 8 MiB of a debug build's stack (fennecql.md,
    // *SQL spellings*).
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || run(how))
        .expect("a thread")
        .join()
        .expect("the corpus");
}

#[cfg(test)]
mod tests {
    use super::*;
    use fennec_core::error::Error;

    const DEFINED: &str = "create statement q() as get t where a = 1 select id";

    fn refused(_: &str) -> Result<Vec<Statement>> {
        Err(Error::Query("refused afresh".into()))
    }

    /// A text `parse` now refuses that the stored reader still reads -- a
    /// definition stored before the refusal, read as it was -- keeps its
    /// stored reading, so a refusal on purpose that leaves stored
    /// definitions as they were is told apart from one that breaks them.
    #[test]
    fn a_text_refused_afresh_keeps_the_stored_reading_it_has() {
        let held = stored(
            DEFINED,
            How::Words,
            fennec_ql::parse,
            fennec_ql::stored_definition,
        );
        assert!(held.starts_with("ok s"), "{held}");
        let afresh = stored(DEFINED, How::Words, refused, fennec_ql::stored_definition);
        assert_eq!(afresh, held);
        // Refused both ways: no definition's text at all.
        assert_eq!(
            stored(
                "get t where",
                How::Words,
                fennec_ql::parse,
                fennec_ql::stored_definition
            ),
            "-"
        );
        // Read as no definition: none either.
        assert_eq!(
            stored(
                "get t",
                How::Words,
                fennec_ql::parse,
                fennec_ql::stored_definition
            ),
            "-"
        );
        // A definition `parse` reads that the stored reader refuses.
        let broken = |_: &str| -> Result<(u64, Definition)> { Err(Error::Query("no".into())) };
        assert_eq!(
            stored(DEFINED, How::Words, fennec_ql::parse, broken),
            "err query error: no"
        );
    }

    // A node's fields read through `Debug` alone, as the trees' are here.
    #[allow(dead_code)]
    mod before {
        #[derive(Debug)]
        pub enum Statement {
            Listing,
            Drop {
                name: String,
                if_exists: bool,
            },
            Call {
                name: String,
                args: Vec<u32>,
                at: Option<(u32, u32)>,
            },
        }
    }

    #[allow(dead_code)]
    mod after {
        #[derive(Debug)]
        pub enum Statement {
            Listing {
                pattern: Option<String>,
            },
            Drop {
                name: String,
                if_exists: bool,
                temporary: bool,
                at: u64,
            },
            Call {
                name: String,
                args: Vec<u32>,
                at: Option<(u32, u32)>,
                claims: Option<Vec<u32>>,
                note: String,
                hooks: std::collections::BTreeMap<u32, u32>,
            },
        }
    }

    /// A field added that holds nothing -- `None`, `false`, `0`, `[]`,
    /// `""`, `{}` -- leaves a `d` word as it was, a unit variant given one
    /// too; one that holds something moves it.
    #[test]
    fn a_field_added_that_holds_nothing_leaves_the_form_as_it_was() {
        use std::collections::BTreeMap;
        let pairs = [
            (
                form(&before::Statement::Listing),
                form(&after::Statement::Listing { pattern: None }),
            ),
            (
                form(&before::Statement::Drop {
                    name: "t".into(),
                    if_exists: true,
                }),
                form(&after::Statement::Drop {
                    name: "t".into(),
                    if_exists: true,
                    temporary: false,
                    at: 0,
                }),
            ),
            (
                form(&before::Statement::Call {
                    name: "f".into(),
                    args: vec![0, 1],
                    at: Some((0, 2)),
                }),
                form(&after::Statement::Call {
                    name: "f".into(),
                    args: vec![0, 1],
                    at: Some((0, 2)),
                    claims: None,
                    note: String::new(),
                    hooks: BTreeMap::new(),
                }),
            ),
        ];
        for (was, is) in &pairs {
            assert_eq!(was, is);
        }
        assert_eq!(pairs[0].0, "Listing");
        // A positional empty value keeps its line: `Some((0, 2))` is not
        // `Some((2))`.
        assert!(pairs[2].0.contains("            0,\n"), "{}", pairs[2].0);
        // Holding something, it is seen.
        let held = after::Statement::Drop {
            name: "t".into(),
            if_exists: true,
            temporary: true,
            at: 0,
        };
        assert_ne!(form(&held), pairs[1].0);
        let held = after::Statement::Listing {
            pattern: Some(String::new()),
        };
        assert_ne!(form(&held), pairs[0].0);
        // And a value that was not empty and is now is seen.
        let was = before::Statement::Drop {
            name: "t".into(),
            if_exists: true,
        };
        let is = before::Statement::Drop {
            name: "t".into(),
            if_exists: false,
        };
        assert_ne!(form(&was), form(&is));
    }

    /// The lines left out, exactly: a named field, then one of the empty
    /// values and its comma, nothing else.
    #[test]
    fn the_lines_left_out() {
        for line in [
            "    name: None,",
            "if_exists: false,",
            "  count: 0,",
            "    args: [],",
            "    text: \"\",",
            "    hooks: {},",
            "    r#type: None,",
            "    _x: 0,",
        ] {
            assert!(empty_field(line), "{line}");
        }
        for line in [
            "    0,",
            "    None,",
            "    name: \"\\\"\",",
            "    count: 0.0,",
            "    count: 00,",
            "    at: Some(",
            "    name: None",
            "    \"key\": None,",
            "    a b: None,",
            "    name: \"a: None,\",",
            "    flag: true,",
        ] {
            assert!(!empty_field(line), "{line}");
        }
    }

    /// The `d` word of a schema statement holds what it says: `if exists`
    /// read or not are two words.
    #[test]
    fn a_d_word_is_its_statements_form() {
        let words = |text: &str| read(&template(), How::Words, || fennec_ql::parse(text));
        let (a, b) = (
            words("drop collection t"),
            words("drop collection if exists t"),
        );
        assert!(a.starts_with("ok d") && b.starts_with("ok d"), "{a} {b}");
        assert_ne!(a, b);
        assert_eq!(words("collections"), words("COLLECTIONS"));
        let forms = read(&template(), How::Forms, || {
            fennec_ql::parse("drop collection if exists t")
        });
        assert!(forms.starts_with("ok\x1eDropCollection {"), "{forms}");
        assert!(forms.contains("if_exists: true"), "{forms}");
    }
}
