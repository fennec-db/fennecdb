//! `fennec import` of a SQLite file whose names hold escape sequences: the
//! lines the SQLite reader makes of the file's text -- a VIRTUAL generated
//! column's warning, a refused default's error -- reach the terminal
//! escaped. A file's writer names its columns and writes its defaults, and
//! printed as they were an `ESC ] 0 ;` retitled the terminal and an `ESC [
//! 2 J` cleared it.
#![cfg(feature = "import")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_fennec");

/// A directory of the test's own, removed at the end.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `escapes.sqlite`, which SQLite wrote for fennec-import's tests: its
/// table `t` has a VIRTUAL column named `ESC[31mred BEL` and a column added
/// later, named `ESC]0;title BEL U+202E`, whose default `(-'ESC[2J')` no
/// import reads (SQLite reads it by the text's numeric prefix), for the
/// row written before it.
#[test]
fn a_sqlite_files_names_reach_the_terminal_escaped() {
    let dir =
        Dir(std::env::temp_dir().join(format!("fennec-cli-sqlite-import-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let kept = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fennec-import/tests/sqlite-files/escapes.sqlite");
    std::fs::copy(kept, dir.0.join("escapes.sqlite")).unwrap();
    let out = Command::new(BIN)
        .current_dir(&dir.0)
        .args([
            "import",
            "escapes.sqlite",
            "--table",
            "t",
            "--file",
            "e.fennec",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    for (name, text) in [("stdout", &stdout), ("stderr", &stderr)] {
        assert!(
            !text.contains(['\u{1b}', '\u{7}', '\u{202e}']),
            "{name}: {text:?}"
        );
    }
    assert!(
        stdout.contains(r"warning  `\u{1b}[31mred\u{7}` is a VIRTUAL generated column"),
        "{stdout}"
    );
    assert!(
        stderr.contains(r"`\u{1b}]0;title\u{7}\u{202e}`") && stderr.contains(r"(-'\u{1b}[2J')"),
        "{stderr}"
    );
}
