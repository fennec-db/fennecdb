//! `fennec import` from a PostgreSQL stand-in: what the terminal is shown
//! of a source's own words. A column's name is the server's to choose --
//! or whoever answers in its place under `prefer` or `require`, or a SQLite
//! file's writer -- and the plan's warnings and `--dry-run`'s fields carry
//! it: printed raw, a name holding an escape sequence reached the terminal
//! (retitled it, rewrote what it showed). Every such line is escaped now
//! (`said` in `src/import.rs`).

#![cfg(feature = "import")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_fennec");

/// A column's name holding a terminal's escape sequences: OSC 0, which
/// retitles the window, and CSI 2J, which clears the screen.
const NAME: &str = "\x1b]0;owned\x07\x1b[2Jnote";

/// A directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d =
            std::env::temp_dir().join(format!("fennec-cli-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// A `RowDescription` of text-format columns, each a name and a type oid.
fn columns(cols: &[(&str, i32)]) -> Vec<u8> {
    let mut b = (cols.len() as i16).to_be_bytes().to_vec();
    for (name, oid) in cols {
        b.extend_from_slice(name.as_bytes());
        b.push(0);
        b.extend_from_slice(&0i32.to_be_bytes()); // table oid
        b.extend_from_slice(&0i16.to_be_bytes()); // column number
        b.extend_from_slice(&oid.to_be_bytes());
        b.extend_from_slice(&(-1i16).to_be_bytes()); // type length
        b.extend_from_slice(&(-1i32).to_be_bytes()); // type modifier
        b.extend_from_slice(&0i16.to_be_bytes()); // text
    }
    framed(b'T', &b)
}

fn done(tag: &str) -> Vec<u8> {
    let mut out = framed(b'C', format!("{tag}\0").as_bytes());
    out.extend_from_slice(&framed(b'Z', b"I"));
    out
}

/// One message, `None` at the close.
fn message(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head).ok()?;
    let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    let mut body = vec![0u8; len.checked_sub(4)?];
    s.read_exact(&mut body).ok()?;
    Some((head[0], body))
}

/// One connection: trust, then the reader's queries -- the pgvector types
/// (none here), the table's columns, its COPY -- table `t` holding an
/// `int8` id and a `uuid` column named [`NAME`].
fn connection(mut s: TcpStream) {
    s.set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    let mut len = [0u8; 4];
    if s.read_exact(&mut len).is_err() {
        return;
    }
    let mut startup = vec![0u8; i32::from_be_bytes(len) as usize - 4];
    if s.read_exact(&mut startup).is_err() {
        return;
    }
    let mut out = framed(b'R', &0i32.to_be_bytes());
    out.extend_from_slice(&framed(b'S', b"server_version\x0017.0\x00"));
    out.extend_from_slice(&framed(b'Z', b"I"));
    let _ = s.write_all(&out);
    while let Some((tag, body)) = message(&mut s) {
        if tag != b'Q' {
            return;
        }
        let sql = String::from_utf8_lossy(&body);
        let mut out = Vec::new();
        if sql.contains("from pg_type") {
            out.extend_from_slice(&columns(&[("typname", 19), ("oid", 26)]));
            out.extend_from_slice(&done("SELECT 0"));
        } else if sql.starts_with("select * from") {
            out.extend_from_slice(&columns(&[("id", 20), (NAME, 2950)]));
            out.extend_from_slice(&done("SELECT 0"));
        } else if sql.starts_with("copy") {
            out.extend_from_slice(&framed(b'H', &[0, 0, 2, 0, 0, 0, 0]));
            out.extend_from_slice(&framed(b'd', b"1\t0b3c5d7e-1f2a-4b6c-8d9e-0a1b2c3d4e5f\n"));
            out.extend_from_slice(&framed(b'c', b""));
            out.extend_from_slice(&done("COPY 1"));
        } else {
            return;
        }
        if s.write_all(&out).is_err() {
            return;
        }
    }
}

/// The stand-in's port: every connection served on a thread of its own.
fn stand_in() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(s) = s else { return };
            std::thread::spawn(move || connection(s));
        }
    });
    port
}

/// `fennec import <url> <args>`, with no variable of libpq's this process
/// inherited: the URL says all.
fn import(port: u16, args: &[&str]) -> Output {
    let url = format!("postgres://u@127.0.0.1:{port}/d?sslmode=disable");
    Command::new(BIN)
        .arg("import")
        .arg(&url)
        .args(args)
        .env_remove("PGPASSWORD")
        .env_remove("PGSSLMODE")
        .env_remove("PGSSLROOTCERT")
        .env_remove("PGUSER")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// A column named with escape sequences reaches neither stdout nor stderr
/// raw -- not as a field of `--dry-run`'s plan, nor in its warning (a
/// `uuid` taken as text, naming the column), nor in the import's own
/// warning line -- and is shown escaped. Before, both printed it as the
/// server sent it.
#[test]
fn a_columns_name_reaches_the_terminal_escaped() {
    let port = stand_in();
    let dir = Dir::new("escaped");
    let file = dir.0.join("t.fennec");
    let file = file.to_str().unwrap();
    for (what, args) in [
        (
            "--dry-run",
            vec!["--table", "t", "--file", file, "--dry-run"],
        ),
        ("the import", vec!["--table", "t", "--file", file]),
    ] {
        let out = import(port, &args);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(out.status.success(), "{what}: {stdout}{stderr}");
        for (name, text) in [("stdout", &stdout), ("stderr", &stderr)] {
            assert!(
                !text.contains('\x1b') && !text.contains('\x07'),
                "{what}: a raw escape on {name}: {}",
                text.escape_debug()
            );
        }
        let shown = "\\u{1b}]0;owned\\u{7}\\u{1b}[2Jnote";
        assert!(
            stdout.contains(&format!("warning  {shown}: uuid taken as `text`")),
            "{what}: {stdout}"
        );
        if what == "--dry-run" {
            assert!(stdout.contains(&format!("  {shown}  text")), "{stdout}");
        } else {
            assert!(stdout.contains("1 records"), "{stdout}");
        }
    }
}
