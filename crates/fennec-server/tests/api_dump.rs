//! The dumps of `integrations/api-golden/` the binaries give, made and held
//! one way for the three (docs/design/conventions.md#api-baselines):
//! fennec-server's, fennec-shard's and fennec-cli's `tests/api_golden.rs`
//! take this file in by its path.
//!
//! - A command's flags are the dash-led words of its `--help` its parser
//!   takes, each asked of the binary itself: `<flag> --help <no such flag>`
//!   is refused naming the flag where the parser does not know it, prints
//!   the help and exits 0 where it is a switch, and is refused at the flag
//!   after `--help` where the flag took `--help` for its value. Each probe
//!   runs in an empty directory of its own, which it must leave empty, and
//!   its answer must carry no system error: a binary reads its whole line,
//!   and refuses it, before it makes, opens or reads anything a flag names.
//!   A flag the crate's sources name that no help shows fails the test.
//! - The exit codes are the binary's `exit::Code`s with their meanings; a
//!   source of the crate with another way out of the process
//!   ([`other_exits`]), or naming a code the list leaves out, fails the
//!   test.
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

/// A directory of a test's own, removed when the guard goes -- a failed
/// assertion's unwinding too, which left it in the temporary directory.
pub struct Scratch(pub PathBuf);

impl Scratch {
    /// `dir`, made empty.
    pub fn new(dir: PathBuf) -> Scratch {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
/// another program's flags, or names it holds absent). Read past what is
/// not code ([`not_code`]), so a quote in a comment, a char or a raw string
/// pairs no literal and a brace in none counts.
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

pub fn literals_of(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let (mut i, n) = (0, b.len());
    let mut out = Vec::new();
    // The brace depth, and the depth a `#[cfg(test)]` item's body opened
    // at while in one: its literals are not kept.
    let (mut depth, mut test_at, mut pending) = (0usize, None::<usize>, false);
    while i < n {
        if let Some((end, text)) = not_code(src, i) {
            if let (Some(text), None) = (text, test_at) {
                out.push(src[text].to_string());
            }
            i = end;
        } else if b[i..].starts_with(b"#[cfg(test)]") {
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
        } else {
            i += 1;
        }
    }
    out
}

/// What is not code at `src[i]`, if anything starts there: a comment, a
/// string or a char literal -- its end, and a string's text (between its
/// quotes, escapes as written). Every form Rust has: `//`, a `/* */` that
/// nests, `"..."` and `b"..."`/`c"..."` (their prefix read as code, which
/// it harms nothing to be), a raw string `r"..."`, `br"..."` or `cr"..."`
/// with any number of `#`, in which a backslash escapes nothing, and a
/// char (`'"'`, `'\''`, `'é'`) where a lifetime (`'a`) is code. Read as a
/// plain string, `br"a\"` ran on to the next quote and hid the code after
/// it; `br#"..."#` holding one quote hid all of the file after it.
pub fn not_code(src: &str, i: usize) -> Option<(usize, Option<std::ops::Range<usize>>)> {
    let b = src.as_bytes();
    let n = b.len();
    if b[i..].starts_with(b"//") {
        return Some((src[i..].find('\n').map_or(n, |j| i + j), None));
    }
    if b[i..].starts_with(b"/*") {
        let (mut j, mut depth) = (i + 2, 1);
        while j < n && depth > 0 {
            if b[j..].starts_with(b"/*") {
                depth += 1;
                j += 2;
            } else if b[j..].starts_with(b"*/") {
                depth -= 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        return Some((j.min(n), None));
    }
    // A raw string's prefix stands at a word's start: `r`, `br` or `cr`.
    let word_start = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    let prefix = match (b[i], b.get(i + 1)) {
        (b'r', _) => 1,
        (b'b' | b'c', Some(b'r')) => 2,
        _ => 0,
    };
    if prefix > 0 && word_start {
        let hashes = b[i + prefix..].iter().take_while(|&&c| c == b'#').count();
        let open = i + prefix + hashes;
        // `r#name` is a raw identifier, code.
        if b.get(open) == Some(&b'"') {
            let close = format!("\"{}", "#".repeat(hashes));
            let end = src[open + 1..].find(&close).map_or(n, |j| open + 1 + j);
            return Some(((end + close.len()).min(n), Some(open + 1..end)));
        }
        return None;
    }
    if b[i] == b'"' {
        let mut j = i + 1;
        while j < n && b[j] != b'"' {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        return Some(((j + 1).min(n), Some(i + 1..j.min(n))));
    }
    if b[i] == b'\'' {
        let c = src[i + 1..].chars().next().map_or(0, char::len_utf8);
        // Its closing quote after the escaped character, which may be one:
        // `'\''`.
        if b.get(i + 1) == Some(&b'\\') {
            let end = (src.get(i + 3..).and_then(|r| r.find('\''))).map_or(n, |j| i + 3 + j + 1);
            return Some((end, None));
        }
        if b.get(i + 1 + c) == Some(&b'\'') {
            return Some((i + 2 + c, None));
        }
    }
    None
}

// ------------------------------------------------------------ processes

/// Runs `exe args` in `cwd`, stdin closed: its exit code and its stdout
/// then stderr. A probe that runs past [`WITHIN`] -- it started serving --
/// is killed and fails the test.
pub fn run(exe: &Path, args: &[&str], cwd: &Path) -> (i32, String) {
    run_command(Command::new(exe), exe, args, cwd)
}

/// [`run`] with none of this process's `FENNEC_` variables: a token or a
/// key file the environment gives would answer for what the line leaves
/// out.
pub fn run_clean(exe: &Path, args: &[&str], cwd: &Path) -> (i32, String) {
    let mut cmd = Command::new(exe);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("FENNEC_") {
            cmd.env_remove(k);
        }
    }
    run_command(cmd, exe, args, cwd)
}

fn run_command(mut cmd: Command, exe: &Path, args: &[&str], cwd: &Path) -> (i32, String) {
    let mut child = cmd
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
/// `cwd`, an empty directory of the probe's own, which it must leave
/// empty, and answered with no system error: a binary that acted on a
/// flag's value as it read it -- the router's `--audit` made a file named
/// `--help`, `--tls-ca` read one -- before the flag after it was refused
/// would run, not answer, a probe.
pub fn probe(exe: &Path, prefix: &[&str], flag: &str, refused: i32, cwd: &Path) -> Option<Takes> {
    let mut args = prefix.to_vec();
    args.extend([flag, "--help", NO_SUCH]);
    let (code, out) = run(exe, &args, cwd);
    touched_nothing(&args, cwd, &out);
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

/// Holds a probe run in `cwd`, which was empty, to what it left there and
/// what it answered, `out`: nothing made, and no error of the system's
/// (`(os error 2)`, a file named `--help` read), since a probe's line is
/// refused before anything a flag names is touched.
pub fn touched_nothing(args: &[&str], cwd: &Path, out: &str) {
    let left: Vec<String> = std::fs::read_dir(cwd)
        .unwrap_or_else(|e| panic!("{}: {e}", cwd.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.is_empty(),
        "{args:?} made {left:?} in its empty directory: a binary reads its whole line, and \
         refuses it, before it makes anything a flag names\n{out}"
    );
    assert!(
        !out.contains("(os error"),
        "{args:?} met the system over what a flag named before its line was read whole and \
         refused:\n{out}"
    );
}

/// `exe prefix --help`: its text, held to exit 0, in `cwd`, which was empty
/// and stays so.
pub fn help(exe: &Path, prefix: &[&str], cwd: &Path) -> String {
    let mut args = prefix.to_vec();
    args.push("--help");
    let (code, out) = run(exe, &args, cwd);
    assert_eq!(code, 0, "{args:?}:\n{out}");
    touched_nothing(&args, cwd, &out);
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
/// takes. Each probe is a process of its own, in an empty directory of its
/// own: they run eight at a time.
pub fn commands(exe: &Path, name: &str, refused: i32) -> BTreeMap<String, BTreeMap<String, Takes>> {
    let scratch = Scratch::new(
        std::env::temp_dir().join(format!("fennec-api-golden-{name}-{}", std::process::id())),
    );
    let helped = scratch.0.join("help");
    std::fs::create_dir(&helped).unwrap();
    let cwd = helped.as_path();
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
    let mut probes = 0usize;
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
                        probes += 1;
                        let own = scratch.0.join(probes.to_string());
                        std::fs::create_dir(&own).unwrap();
                        s.spawn(move || (w.clone(), probe(exe, prefix, w, refused, &own)))
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
/// name): no way out of the process but `exit.rs`'s ([`other_exits`]), and
/// no `Code::` the list leaves out.
pub fn exits_held(src: &Path, listed: &[String]) {
    let mut files = Vec::new();
    rust_files(src, &mut files);
    for path in files {
        if path.file_name().is_some_and(|f| f == "exit.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let ways = other_exits(&text);
        assert!(
            ways.is_empty(),
            "{} has a way out of the process other than exit::now -- {} -- which decides its \
             code where the dump does not see it: hand exit::now a Code",
            path.display(),
            ways.join("; ")
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

/// The ways out of the process the source `src` takes other than
/// `exit::now`, each named, read from its code alone ([`code_of`]: a
/// message naming `process::exit` is none) as tokens, so spacing hides
/// nothing (`process :: exit`):
///
/// - `process::exit` or `process::abort` by any path (`std::`, `::std::`,
///   a module renamed, `r#exit`);
/// - a `use` taking `exit` or `abort` in from `process`, under any name
///   (`exit`, `exit as quit`, `{self, exit}`, nested in `std::{...}`);
/// - a `use` taking in all of `process`'s names (`std::process::*`), or
///   the module under another name (`process as p`, `{self as p}`), under
///   which a call is no longer written `process::exit`;
/// - an `ExitCode`, and the binary's `main()` returning a value -- an
///   `Err` from one exits 1 with no `Code`.
pub fn other_exits(src: &str) -> Vec<&'static str> {
    let code = code_of(src);
    let t = tokens(&code);
    let mut out = Vec::new();
    let mut way = |w: &'static str| {
        if !out.contains(&w) {
            out.push(w);
        }
    };
    for w in t.windows(3) {
        match w {
            ["process", "::", "exit"] => way("std::process::exit"),
            ["process", "::", "abort"] => way("std::process::abort"),
            _ => {}
        }
    }
    if t.contains(&"ExitCode") {
        way("an ExitCode");
    }
    for k in (0..t.len()).filter(|&k| t[k] == "use") {
        let mut taken = Vec::new();
        use_tree(&t, &mut (k + 1), Vec::new(), &mut taken);
        for (path, name) in taken {
            let n = path.len();
            if n >= 2 && path[n - 2] == "process" && matches!(path[n - 1], "exit" | "abort") {
                way("std::process's exit or abort taken in by a use");
            } else if path.last() == Some(&"process") && name == "*" {
                way("std::process's names all taken in by a use (`process::*`)");
            } else if path.last() == Some(&"process") && name != "process" {
                way("std::process renamed by a use");
            }
        }
    }
    if t.windows(5).any(|w| w == ["fn", "main", "(", ")", "->"]) {
        way("a main returning a value");
    }
    out
}

/// The tokens of `code`: names (a raw one, `r#exit`, as its name), `::`,
/// `->`, and every other character on its own, spacing dropped.
fn tokens(code: &str) -> Vec<&str> {
    let b = code.as_bytes();
    let name = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if name(b[i]) {
            let at = i;
            while i < b.len() && name(b[i]) {
                i += 1;
            }
            out.push(&code[at..i]);
        } else if b[i..].starts_with(b"::") || b[i..].starts_with(b"->") {
            out.push(&code[i..i + 2]);
            i += 2;
        } else {
            out.push(&code[i..i + 1]);
            i += 1;
        }
    }
    // `r # name` is the name: a raw string was blanked as a literal.
    let mut named = Vec::with_capacity(out.len());
    let mut k = 0;
    while k < out.len() {
        let raw = out[k] == "r"
            && out.get(k + 1) == Some(&"#")
            && out.get(k + 2).is_some_and(|w| name(w.as_bytes()[0]));
        if raw {
            k += 2;
        }
        named.push(out[k]);
        k += 1;
    }
    named
}

/// The names the `use` tree at `t[*at]` takes in, each with the path it
/// names: `a::b::{c, d as e, self as f, *}` takes `c` as `c`, `d` as `e`,
/// `a::b` as `f`, and `a::b`'s every name (`*`). Read until what is not a
/// tree: a macro's `$crate` ends it.
fn use_tree<'a>(
    t: &[&'a str],
    at: &mut usize,
    mut path: Vec<&'a str>,
    out: &mut Vec<(Vec<&'a str>, &'a str)>,
) {
    let named = |w: &str| w.starts_with(|c: char| c.is_alphabetic() || c == '_' || !c.is_ascii());
    if t.get(*at) == Some(&"::") {
        *at += 1;
    }
    loop {
        match t.get(*at).copied() {
            Some("*") => {
                *at += 1;
                out.push((path, "*"));
                return;
            }
            Some("{") => {
                *at += 1;
                while let Some(&w) = t.get(*at) {
                    if w == "}" {
                        *at += 1;
                        return;
                    }
                    if w == "," {
                        *at += 1;
                        continue;
                    }
                    let before = *at;
                    use_tree(t, at, path.clone(), out);
                    if *at == before {
                        return;
                    }
                }
                return;
            }
            Some(w) if named(w) => {
                path.push(w);
                *at += 1;
                if t.get(*at) == Some(&"::") {
                    *at += 1;
                    continue;
                }
                // `{self}` and `{self as p}` name the module they are in.
                if path.len() > 1 && path.last() == Some(&"self") {
                    path.pop();
                }
                let name = if t.get(*at) == Some(&"as") {
                    *at += 1;
                    let name = t.get(*at).copied().unwrap_or("_");
                    *at += 1;
                    name
                } else {
                    path.last().copied().unwrap_or("_")
                };
                out.push((path, name));
                return;
            }
            _ => return,
        }
    }
}

/// `src` with what is not code ([`not_code`]) blanked, line breaks kept.
fn code_of(src: &str) -> String {
    let (mut out, mut i) = (String::with_capacity(src.len()), 0);
    while i < src.len() {
        match not_code(src, i) {
            Some((end, _)) => {
                out.extend(
                    src[i..end]
                        .chars()
                        .map(|c| if c == '\n' { '\n' } else { ' ' }),
                );
                i = end;
            }
            None => {
                let c = src[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
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

/// The commands of a binary, each with its flags.
pub type Commands = BTreeMap<String, BTreeMap<String, Takes>>;

/// Everything [`commands`], [`no_hidden_flags`] and [`exits_held`] hold of
/// the binary `exe`, called `name`, whose crate's sources are `src`: its
/// commands, and its dump; `codes` are its `Code::ALL` (code, meaning,
/// variant name).
pub fn cli(
    exe: &Path,
    name: &str,
    src: &Path,
    codes: &[(i32, &str, String)],
) -> (Commands, String) {
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
    let json = cli_json(&commands, &codes);
    (commands, json)
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
