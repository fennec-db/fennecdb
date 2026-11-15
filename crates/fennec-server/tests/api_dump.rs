//! The dumps of `integrations/api-golden/` the binaries give, made and held
//! one way for the three (docs/design/conventions.md#api-baselines):
//! fennec-server's, fennec-shard's and fennec-cli's `tests/api_golden.rs`
//! take this file in by its path.
//!
//! - A command's flags are the dash-led words of its `--help` its parser
//!   takes, each asked of the binary itself: `<flag> --help <no such flag>`
//!   is refused naming the flag where the parser does not know it, prints
//!   the help and exits 0 where it is a switch, and is refused at the flag
//!   after `--help` where the flag took `--help` for its value. A flag the
//!   crate's sources name that no help shows fails the test.
//! - The exit codes are the binary's `exit::Code`s with their meanings; a
//!   source of the crate that exits other than through `exit::now`, or
//!   names a code the list leaves out, fails the test.
//! - The metrics are the families scrapes show, each with its type and every
//!   label one of its samples carries; a family with no sample, whose labels
//!   no scrape shows, fails the test.
//!
//! Each dump is held to the committed file byte for byte; under
//! `FENNEC_API_GOLDEN=<dir>` (`make api-golden`) it is written there instead,
//! for `tools/api_compat.py --write --built <dir>` to judge and copy.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where `make api-golden` has the tests write their dumps.
pub const WRITE_TO: &str = "FENNEC_API_GOLDEN";

/// A flag no parser takes: what each probe ends with.
const NO_SUCH: &str = "--fennec-api-golden-no-such-flag";

/// The longest a probe or a server's start may take: they answer in
/// milliseconds, so only a hang meets it.
const WITHIN: Duration = Duration::from_secs(60);

/// The committed dumps.
pub fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/api-golden")
}

/// The repository's `crates/`.
pub fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Holds the dump `name` to the committed file, or writes it where
/// `make api-golden` asked.
pub fn hold(name: &str, text: &str) {
    if let Some(dir) = std::env::var_os(WRITE_TO) {
        let path = Path::new(&dir).join(name);
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    if let Err(why) = held(&golden().join(name), text) {
        panic!("{why}");
    }
}

/// Whether the file at `path` is `text`; else what differs, naming the
/// one command that writes it again.
pub fn held(path: &Path, text: &str) -> Result<(), String> {
    let committed = std::fs::read_to_string(path).unwrap_or_default();
    if committed == text {
        return Ok(());
    }
    Err(format!(
        "{} is not what the binary gives now: run `make api-golden` and commit it \
         (tools/api_compat.py then holds it to main's); the file (-) against the binary's (+):\n{}",
        path.display(),
        diff(&committed, text)
    ))
}

/// The lines of `a` and `b` that differ, `-` and `+`, each under the
/// object it is in -- the flag's command, the metric -- and an added one
/// with its line in `b`: a longest common subsequence of lines, the dumps
/// being a few hundred.
fn diff(a: &str, b: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = match a[i] == b[j] {
                true => lcs[i + 1][j + 1] + 1,
                false => lcs[i + 1][j].max(lcs[i][j + 1]),
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, String::new());
    let (mut under, mut said) = ("", None);
    while i < a.len() || j < b.len() {
        let line = if i < a.len() && j < b.len() && a[i] == b[j] {
            if a[i].ends_with('{') {
                under = a[i];
            }
            i += 1;
            j += 1;
            continue;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            j += 1;
            format!("    + {} (line {j})\n", b[j - 1])
        } else {
            i += 1;
            format!("    - {}\n", a[i - 1])
        };
        if said != Some(under) {
            out.push_str(&format!("  {under}\n"));
            said = Some(under);
        }
        out.push_str(&line);
    }
    out
}

/// The committed dump `name`, or the one `make api-golden` wrote before
/// this test where it did.
pub fn current(name: &str) -> String {
    if let Some(dir) = std::env::var_os(WRITE_TO) {
        if let Ok(text) = std::fs::read_to_string(Path::new(&dir).join(name)) {
            return text;
        }
    }
    std::fs::read_to_string(golden().join(name)).unwrap_or_default()
}

// ------------------------------------------------------------ JSON

/// A dump's values: text, a list of texts, or an object (keys sorted).
pub enum J {
    S(String),
    L(Vec<String>),
    M(BTreeMap<String, J>),
}

/// `v` as Python's `json.dumps(v, indent=1, ensure_ascii=False)` writes
/// it, and a newline: the other baselines' form.
pub fn json(v: &J) -> String {
    let mut out = String::new();
    write_json(&mut out, v, 0);
    out.push('\n');
    out
}

fn write_json(out: &mut String, v: &J, depth: usize) {
    let pad = |out: &mut String, d: usize| out.extend(std::iter::repeat_n(' ', d));
    match v {
        J::S(s) => quote(out, s),
        J::L(items) if items.is_empty() => out.push_str("[]"),
        J::M(m) if m.is_empty() => out.push_str("{}"),
        J::L(items) => {
            out.push_str("[\n");
            for (i, s) in items.iter().enumerate() {
                pad(out, depth + 1);
                quote(out, s);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            pad(out, depth);
            out.push(']');
        }
        J::M(m) => {
            out.push_str("{\n");
            for (i, (k, v)) in m.iter().enumerate() {
                pad(out, depth + 1);
                quote(out, k);
                out.push_str(": ");
                write_json(out, v, depth + 1);
                out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
            }
            pad(out, depth);
            out.push('}');
        }
    }
}

fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The keys of a dump's top-level object, as [`json`] writes it: a key is
/// a line at one space's indent.
pub fn top_keys(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| {
            l.strip_prefix(" \"")?
                .split_once("\": ")
                .map(|(k, _)| k.to_string())
        })
        .collect()
}

// ------------------------------------------------------------ the sources

/// The string literals of every `.rs` file under `dir`, with the file each
/// is in, an item under `#[cfg(test)]` left out (a unit test's literals are
/// another program's flags, or names it holds absent). A lexer of its own:
/// line and block comments and char literals are passed over, raw strings
/// read whole, so a quote in any of them pairs no literal and a brace in
/// none counts.
pub fn literals(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        for lit in literals_of(&src) {
            out.push((path.clone(), lit));
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn literals_of(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let (mut i, n) = (0, b.len());
    let mut out = Vec::new();
    // The brace depth, and the depth a `#[cfg(test)]` item's body opened
    // at while in one: its literals are not kept.
    let (mut depth, mut test_at, mut pending) = (0usize, None::<usize>, false);
    let keep = |lit: String, test_at: Option<usize>, out: &mut Vec<String>| {
        if test_at.is_none() {
            out.push(lit);
        }
    };
    while i < n {
        if b[i..].starts_with(b"#[cfg(test)]") {
            pending = true;
            i += "#[cfg(test)]".len();
        } else if b[i] == b'{' {
            if pending && test_at.is_none() {
                test_at = Some(depth);
            }
            pending = false;
            depth += 1;
            i += 1;
        } else if b[i] == b'}' {
            depth = depth.saturating_sub(1);
            if test_at == Some(depth) {
                test_at = None;
            }
            i += 1;
        } else if b[i] == b';' {
            pending = false;
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i = src[i + 2..].find("*/").map_or(n, |j| i + 2 + j + 2);
        } else if b[i] == b'r'
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
            && matches!(b.get(i + 1), Some(b'"' | b'#'))
        {
            let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
            let open = i + 1 + hashes;
            if b.get(open) != Some(&b'"') {
                i += 1;
                continue;
            }
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[open + 1..].find(&close).map_or(n, |j| open + 1 + j);
            keep(src[open + 1..end].to_string(), test_at, &mut out);
            i = end + close.len();
        } else if b[i] == b'\'' {
            // A char literal ('"', '\'', '\u{..}') or a lifetime ('a).
            if b.get(i + 1) == Some(&b'\\') {
                i = src[i + 2..].find('\'').map_or(n, |j| i + 2 + j + 1);
            } else if b.get(i + 2) == Some(&b'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else if b[i] == b'"' {
            let mut j = i + 1;
            let mut lit = String::new();
            while j < n && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            lit.push_str(&src[i + 1..j.min(n)]);
            keep(lit, test_at, &mut out);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------ processes

/// Runs `exe args` in `cwd`, stdin closed: its exit code and its stdout
/// then stderr. A probe that runs past [`WITHIN`] -- it started serving --
/// is killed and fails the test.
pub fn run(exe: &Path, args: &[&str], cwd: &Path) -> (i32, String) {
    let mut child = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );
    let t = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if t.elapsed() > WITHIN {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{} {args:?} did not end: a probe must never serve",
                exe.display()
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let text = out.join().unwrap() + &err.join().unwrap();
    (status.code().unwrap_or(-1), text)
}

/// A server or a router started for a scrape, listening on a port of the
/// system's (its `listening on: http://` line names it); killed when
/// dropped.
pub struct Running {
    child: Child,
    pub port: u16,
    pub log: Arc<Mutex<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `exe args`, which must name port 0, and waits for its listener.
pub fn listening(exe: &Path, args: &[&str]) -> Running {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let log = Arc::new(Mutex::new(String::new()));
    let mut port = None;
    while port.is_none() {
        let mut line = String::new();
        if err.read_line(&mut line).unwrap_or(0) == 0 {
            let _ = child.wait();
            panic!(
                "{} {args:?} ended before it listened:\n{}",
                exe.display(),
                log.lock().unwrap()
            );
        }
        if let Some((_, rest)) = line.split_once("listening on: http://") {
            let addr: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            port = addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok());
        }
        log.lock().unwrap().push_str(&line);
    }
    let sink = Arc::clone(&log);
    std::thread::spawn(move || {
        let mut line = String::new();
        while err.read_line(&mut line).unwrap_or(0) > 0 {
            sink.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    Running {
        child,
        port: port.unwrap(),
        log,
    }
}

/// `GET path` of the server at `port` with `token`: the status and the body.
pub fn get(port: u16, path: &str, token: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(WITHIN)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0);
    let chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    if !chunked {
        return (status, body.to_string());
    }
    let (mut out, mut rest) = (String::new(), body);
    while let Some((size, after)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&after[..n]);
        rest = &after[n + 2..];
    }
    (status, out)
}

/// Asks `scrape` again until `found` holds of what it answers, within
/// [`WITHIN`]: the event a configuration's family waits for.
pub fn scrape_until(
    what: &str,
    scrape: impl Fn() -> String,
    found: impl Fn(&str) -> bool,
) -> String {
    let t = Instant::now();
    loop {
        let text = scrape();
        if found(&text) {
            return text;
        }
        assert!(t.elapsed() < WITHIN, "never {what}:\n{text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ------------------------------------------------------------ the CLI

/// The dash-led words of `text`: a dash at a word's start (`fennec-server`
/// holds none), one or two, then a letter.
pub fn dashed_words(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < b.len() {
        let starts = b[i] == b'-'
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-' || b[i - 1] == b'_'));
        if !starts {
            i += 1;
            continue;
        }
        let dashes = if b.get(i + 1) == Some(&b'-') { 2 } else { 1 };
        let mut j = i + dashes;
        if !b.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
            i = j;
            continue;
        }
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        out.insert(text[i..j].trim_end_matches('-').to_string());
        i = j;
    }
    out
}

/// What a probe made of a flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Takes {
    Switch,
    Value,
}

/// Asks `exe prefix flag --help <no such flag>` what `flag` is to the
/// command: not one (None), a switch, or a flag taking a value. A refusal
/// is the binary's `refused` code, which the probe holds it to. Run in
/// `cwd`, a directory of the test's own: a flag may act on its value as it
/// is read (the router's `--audit` opens its file), and there makes a file
/// named `--help`.
pub fn probe(exe: &Path, prefix: &[&str], flag: &str, refused: i32, cwd: &Path) -> Option<Takes> {
    let mut args = prefix.to_vec();
    args.extend([flag, "--help", NO_SUCH]);
    let (code, out) = run(exe, &args, cwd);
    let unknown = |f: &str| {
        out.lines().any(|l| {
            l.split_once("unknown option").is_some_and(|(_, rest)| {
                let word = rest.trim_start_matches([':', ' ', '`']);
                word.split(['`', ' ', '\n']).next() == Some(f)
            })
        })
    };
    if unknown(flag) {
        assert_eq!(
            code, refused,
            "{args:?} refused the flag with {code}:\n{out}"
        );
        return None;
    }
    if code == 0 {
        return Some(Takes::Switch);
    }
    assert_eq!(
        code, refused,
        "{args:?}: neither a switch nor a value:\n{out}"
    );
    Some(Takes::Value)
}

/// `exe prefix --help`: its text, held to exit 0.
pub fn help(exe: &Path, prefix: &[&str], cwd: &Path) -> String {
    let mut args = prefix.to_vec();
    args.push("--help");
    let (code, out) = run(exe, &args, cwd);
    assert_eq!(code, 0, "{args:?}:\n{out}");
    out
}

/// The words `name <word>` at a line's start, or after `usage:`, in `help`:
/// the subcommands a help names.
fn subcommands(help: &str, name: &str) -> BTreeSet<String> {
    let lead = format!("{name} ");
    help.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let l = l.strip_prefix("usage:").map_or(l, str::trim_start);
            let word = l.strip_prefix(&lead)?.split_whitespace().next()?;
            word.chars()
                .all(|c| c.is_ascii_lowercase())
                .then(|| word.to_string())
        })
        .collect()
}

/// Every command of the binary `exe`, called `name` -- itself and each
/// subcommand its helps name, found from help to help -- with each flag it
/// takes. Each probe is a process of its own: they run eight at a time.
pub fn commands(exe: &Path, name: &str, refused: i32) -> BTreeMap<String, BTreeMap<String, Takes>> {
    let cwd = std::env::temp_dir().join(format!("fennec-api-golden-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cwd);
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.as_path();
    let top = help(exe, &[], cwd);
    let mut helps = BTreeMap::from([(String::new(), top.clone())]);
    let mut todo: Vec<String> = subcommands(&top, name).into_iter().collect();
    while let Some(sub) = todo.pop() {
        if helps.contains_key(&sub) {
            continue;
        }
        let text = help(exe, &[&sub], cwd);
        assert_ne!(
            text, top,
            "`{name} {sub}` is named in a help but is no command: its --help is the binary's"
        );
        todo.extend(subcommands(&text, name));
        helps.insert(sub, text);
    }
    let mut out = BTreeMap::new();
    for (sub, text) in &helps {
        let prefix: Vec<&str> = if sub.is_empty() {
            vec![]
        } else {
            vec![sub.as_str()]
        };
        let mut words = dashed_words(text);
        words.extend(["-h", "--help", "-V", "--version"].map(String::from));
        let words: Vec<String> = words.into_iter().collect();
        let mut flags = BTreeMap::new();
        for chunk in words.chunks(8) {
            let got: Vec<(String, Option<Takes>)> = std::thread::scope(|s| {
                let asks: Vec<_> = chunk
                    .iter()
                    .map(|w| {
                        let prefix = &prefix;
                        s.spawn(move || (w.clone(), probe(exe, prefix, w, refused, cwd)))
                    })
                    .collect();
                asks.into_iter().map(|a| a.join().unwrap()).collect()
            });
            flags.extend(got.into_iter().filter_map(|(w, t)| Some((w, t?))));
        }
        let line = if sub.is_empty() {
            name.to_string()
        } else {
            format!("{name} {sub}")
        };
        out.insert(line, flags);
    }
    let _ = std::fs::remove_dir_all(cwd);
    out
}

/// Holds the crate's sources to the commands: every literal there that is
/// a flag's whole spelling is a flag of some command's dump -- shown by its
/// help and taken by its parser -- so none is taken unseen by the dump.
pub fn no_hidden_flags(src: &Path, commands: &BTreeMap<String, BTreeMap<String, Takes>>) {
    let shown: BTreeSet<&str> = commands
        .values()
        .flat_map(|f| f.keys().map(String::as_str))
        .collect();
    let mut hidden = BTreeSet::new();
    for (path, lit) in literals(src) {
        let dashes = lit.len() - lit.trim_start_matches('-').len();
        let rest = &lit[dashes..];
        let flag_like = (1..=2).contains(&dashes)
            && rest.starts_with(|c: char| c.is_ascii_alphabetic())
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !rest.ends_with('-');
        if flag_like && !shown.contains(lit.as_str()) {
            hidden.insert(format!("{lit} ({})", path.display()));
        }
    }
    assert!(
        hidden.is_empty(),
        "flags the sources name that no command's dump holds -- no --help shows it, or the \
         parser does not take it -- so a dump would miss them: name each in its command's usage \
         text, or drop it from the sources\n  {}",
        hidden.into_iter().collect::<Vec<_>>().join("\n  ")
    );
}

/// Holds the crate's sources to the codes `listed` (each code's variant
/// name): no exit but `exit.rs`'s, and no `Code::` the list leaves out.
pub fn exits_held(src: &Path, listed: &[String]) {
    let mut files = Vec::new();
    rust_files(src, &mut files);
    for path in files {
        if path.file_name().is_some_and(|f| f == "exit.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("process::exit("),
            "{} exits other than through exit::now: give it a Code",
            path.display()
        );
        for (_, rest) in text
            .match_indices("Code::")
            .map(|(i, _)| text.split_at(i + 6))
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            // A variant, not a constant (`Code::ALL`) nor a function.
            let variant = name.starts_with(|c: char| c.is_ascii_uppercase())
                && name.chars().any(|c| c.is_ascii_lowercase());
            if variant {
                assert!(
                    listed.contains(&name),
                    "{} names Code::{name}, which Code::ALL leaves out",
                    path.display()
                );
            }
        }
    }
}

/// The CLI dump of a binary: its commands' flags and its exit codes (code,
/// meaning).
pub fn cli_json(
    commands: &BTreeMap<String, BTreeMap<String, Takes>>,
    codes: &[(i32, &str)],
) -> String {
    let commands = commands
        .iter()
        .map(|(c, flags)| {
            let flags = flags
                .iter()
                .map(|(f, t)| {
                    let t = match t {
                        Takes::Switch => "switch",
                        Takes::Value => "value",
                    };
                    (f.clone(), J::S(t.into()))
                })
                .collect();
            (c.clone(), J::M(flags))
        })
        .collect();
    let codes = codes
        .iter()
        .map(|(c, m)| (c.to_string(), J::S(m.to_string())))
        .collect();
    json(&J::M(BTreeMap::from([
        ("commands".into(), J::M(commands)),
        ("exit codes".into(), J::M(codes)),
    ])))
}

/// Everything [`commands`], [`no_hidden_flags`] and [`exits_held`] hold of
/// the binary `exe`, called `name`, whose crate's sources are `src`, as its
/// dump; `codes` are its `Code::ALL` (code, meaning, variant name).
pub fn cli(exe: &Path, name: &str, src: &Path, codes: &[(i32, &str, String)]) -> String {
    let refused = codes
        .iter()
        .find(|(_, _, v)| v == "Refused")
        .map(|(c, _, _)| *c)
        .expect("a Refused code");
    let commands = commands(exe, name, refused);
    no_hidden_flags(src, &commands);
    exits_held(
        src,
        &codes.iter().map(|(_, _, v)| v.clone()).collect::<Vec<_>>(),
    );
    let codes: Vec<(i32, &str)> = codes.iter().map(|(c, m, _)| (*c, *m)).collect();
    cli_json(&commands, &codes)
}

// ------------------------------------------------------------ metrics

/// A metric family as scrapes showed it.
#[derive(Default, Debug)]
pub struct Family {
    pub kind: String,
    pub labels: BTreeSet<String>,
    pub samples: usize,
}

/// The families of a Prometheus text exposition: each `# TYPE`, and the
/// labels its samples carry (a histogram's `le` is its own, not a label).
pub fn families(text: &str) -> BTreeMap<String, Family> {
    let mut out: BTreeMap<String, Family> = BTreeMap::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap_or_else(|| panic!("{line}"));
            let f = out.entry(name.to_string()).or_default();
            assert!(f.kind.is_empty(), "two # TYPE lines for {name}");
            f.kind = kind.trim().to_string();
        }
    }
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let end = line.find(['{', ' ']).unwrap_or(line.len());
        let sample = &line[..end];
        let (name, histogram_part) = if out.contains_key(sample) {
            (sample.to_string(), false)
        } else {
            let base = ["_bucket", "_sum", "_count"]
                .iter()
                .find_map(|s| sample.strip_suffix(s))
                .filter(|b| out.get(*b).is_some_and(|f| f.kind == "histogram"))
                .unwrap_or_else(|| panic!("a sample of no family: {line}"));
            (base.to_string(), true)
        };
        let f = out.get_mut(&name).unwrap();
        f.samples += 1;
        for label in label_names(&line[end..]) {
            if !(histogram_part && label == "le") {
                f.labels.insert(label);
            }
        }
    }
    out
}

/// The label names of a sample's `{k="v",...}` (or none), the values read
/// past their escapes.
fn label_names(rest: &str) -> Vec<String> {
    let Some(inner) = rest.strip_prefix('{') else {
        return Vec::new();
    };
    let b = inner.as_bytes();
    let (mut i, mut out) = (0, Vec::new());
    while i < b.len() && b[i] != b'}' {
        let eq = inner[i..]
            .find('=')
            .map(|j| i + j)
            .expect("a label with no value");
        out.push(inner[i..eq].trim_start_matches(',').to_string());
        assert_eq!(
            b.get(eq + 1),
            Some(&b'"'),
            "an unquoted label value: {rest}"
        );
        let mut j = eq + 2;
        while j < b.len() && b[j] != b'"' {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        i = j + 1;
        if b.get(i) == Some(&b',') {
            i += 1;
        }
    }
    out
}

/// `scrape`'s families added to `into`: one name keeps one type.
pub fn merge(into: &mut BTreeMap<String, Family>, scrape: &str) {
    for (name, f) in families(scrape) {
        let at = into.entry(name.clone()).or_default();
        assert!(
            at.kind.is_empty() || at.kind == f.kind,
            "{name} is a {} in one scrape and a {} in another",
            at.kind,
            f.kind
        );
        at.kind = f.kind;
        at.labels.extend(f.labels);
        at.samples += f.samples;
    }
}

/// The metrics dump: every family with its type and labels. A family no
/// scrape showed a sample of fails: its labels are not known.
pub fn metrics_json(families: &BTreeMap<String, Family>) -> String {
    let unsampled: Vec<&str> = families
        .iter()
        .filter(|(_, f)| f.samples == 0)
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(
        unsampled.is_empty(),
        "families no scrape showed a sample of, so their labels are not known: give the test a \
         configuration that does\n  {}",
        unsampled.join("\n  ")
    );
    json(&J::M(
        families
            .iter()
            .map(|(n, f)| {
                let m = BTreeMap::from([
                    ("type".to_string(), J::S(f.kind.clone())),
                    (
                        "labels".to_string(),
                        J::L(f.labels.iter().cloned().collect()),
                    ),
                ]);
                (n.clone(), J::M(m))
            })
            .collect(),
    ))
}

/// Every metric name the sources under `dirs` declare: each literal that is
/// a whole `fennec_` name. A literal building one (`"fennec_{x}"`) fails
/// the test, as no scan could hold what it makes.
pub fn declared_metrics(dirs: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for dir in dirs {
        for (path, lit) in literals(dir) {
            let Some(rest) = lit.strip_prefix("fennec_") else {
                continue;
            };
            if rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                out.insert(lit);
            } else {
                assert!(
                    !lit.contains('{'),
                    "{}: {lit:?} builds a name the scan cannot hold: write the metric's name whole",
                    path.display()
                );
            }
        }
    }
    out
}
