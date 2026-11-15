//! `integrations/api-golden/cli-fennec.json` is what the shell gives now:
//! every command's flags -- the shell's own and each subcommand's its helps
//! name -- asked of its parsers, and its exit codes (`src/exit.rs`), as
//! docs/design/conventions.md#api-baselines says. `make api-golden` writes
//! it again; tools/api_compat.py holds it to main's. The released binary's
//! features only: a build without one names its subcommand's absence.
#![cfg(all(feature = "import", feature = "backup", feature = "mcp"))]

// fennec-server's: the three binaries' dumps are made and held one way.
#[path = "../../fennec-server/tests/api_dump.rs"]
mod api_dump;
// The crate has no library: its codes are taken in by their file.
#[allow(dead_code)]
#[path = "../src/exit.rs"]
mod exit;

use std::path::Path;

#[test]
fn the_flags_and_exit_codes_are_the_dump() {
    let codes: Vec<(i32, &str, String)> = exit::Code::ALL
        .iter()
        .map(|c| (*c as i32, c.means(), format!("{c:?}")))
        .collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let exe = Path::new(env!("CARGO_BIN_EXE_fennec"));
    let (commands, dump) = api_dump::cli(exe, "fennec", &src, &codes);
    // fennec-import's options, read by the list fennec-server takes them
    // by too: each a flag of `fennec import`'s help, taking a value.
    let import = &commands["fennec import"];
    for flag in fennec_import::args::OPTIONS {
        assert_eq!(
            import.get(flag),
            Some(&api_dump::Takes::Value),
            "{flag}: fennec-import's option, which `fennec import --help` must show"
        );
    }
    // Each of the backup family names the flags it reads alone: a flag
    // another of them reads is unknown to it, never taken and ignored.
    let flags = |c: &str| {
        let mut f: Vec<&str> = commands[c]
            .keys()
            .map(String::as_str)
            .filter(|f| !matches!(*f, "-h" | "--help"))
            .collect();
        f.sort();
        f
    };
    assert_eq!(flags("fennec key"), Vec::<&str>::new());
    assert_eq!(flags("fennec verify"), ["--key-file", "--tenants"]);
    assert_eq!(flags("fennec prune"), ["--keep", "--key-file", "--tenants"]);
    assert_eq!(
        flags("fennec restore"),
        ["--force", "--key-file", "--tenants", "--to", "--to-change"]
    );
    assert_eq!(
        flags("fennec backup"),
        ["--force", "--key-file", "--tls-ca", "--token"]
    );
    assert_eq!(
        flags("fennec archive"),
        [
            "--image-every",
            "--keep",
            "--key-file",
            "--tenants",
            "--tls-ca",
            "--token"
        ]
    );
    api_dump::hold("cli-fennec.json", &dump);
}

/// The ways out of a process the check refuses, each seeded: an exit by
/// any path or name, an abort, an `ExitCode`, a `main` returning a value.
/// Comments, literals and longer names are none.
#[test]
fn every_way_out_but_exit_now_is_refused() {
    let refused = [
        ("fn f() { std::process::exit(1) }", "std::process::exit"),
        ("fn f() { process::exit(1) }", "std::process::exit"),
        (
            "use std::process::exit;\nfn f() { exit(1) }",
            "std::process::exit",
        ),
        (
            "use std::process::{self, exit};\nfn f() { exit(1) }",
            "taken in by a use",
        ),
        (
            "use std::{io, process::{exit, Command}};\nfn f() { exit(1) }",
            "taken in by a use",
        ),
        (
            "use std::process::exit as quit;\nfn f() { quit(1) }",
            "std::process::exit",
        ),
        (
            "use std::process as p;\nfn f() { p::exit(1) }",
            "renamed by a use",
        ),
        ("fn f() { std::process::abort() }", "std::process::abort"),
        (
            "use std::process::{abort, Command};\nfn f() { abort() }",
            "taken in by a use",
        ),
        (
            "use std::process::ExitCode;\nfn main() -> ExitCode { ExitCode::from(3) }",
            "an ExitCode",
        ),
        (
            "fn main()\n    -> Result<(), String>\n{\n    Err(\"no\".into())\n}",
            "a main returning a value",
        ),
    ];
    for (src, way) in refused {
        let ways = api_dump::other_exits(src);
        assert!(ways.iter().any(|w| w.contains(way)), "{src:?}: {ways:?}");
    }
    let passed = [
        "use crate::exit::{self, Code};\nfn main() { exit::now(Code::Failed) }",
        "// std::process::exit(1) once stood here\nfn f() {}",
        "/* process::abort() */ fn f() {}",
        "fn f() -> &'static str { \"std::process::exit(2)\" }",
        "fn f() -> &'static str { r#\"a main() -> ExitCode\"# }",
        "fn f() -> char { '\\'' } fn g() -> &'static str { \"process::exit\" }",
        "use std::process::{Command, Stdio};\nfn main() {}",
        "fn main_loop() -> u8 { 0 }\nfn main() { main_loop(); }",
        "fn reexit(code: u8) -> u8 { code }",
        "pub fn main(args: &[String]) -> Code { Code::Ok }",
    ];
    for src in passed {
        assert_eq!(api_dump::other_exits(src), Vec::<&str>::new(), "{src:?}");
    }
}

/// A dump that is not the committed one fails naming the one command that
/// writes it again, and passes once that wrote it: an addition is stale
/// until then, never let through unseen nor refused for good.
#[test]
fn a_dump_is_stale_until_written_again() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-api-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cli-x.json");
    let was = api_dump::cli_json(
        &[(
            "x".to_string(),
            [("--a".to_string(), api_dump::Takes::Switch)].into(),
        )]
        .into(),
        &[(0, "done")],
    );
    std::fs::write(&path, &was).unwrap();
    assert_eq!(api_dump::held(&path, &was), Ok(()));
    let grown = api_dump::cli_json(
        &[(
            "x".to_string(),
            [
                ("--a".to_string(), api_dump::Takes::Switch),
                ("--b".to_string(), api_dump::Takes::Value),
            ]
            .into(),
        )]
        .into(),
        &[(0, "done")],
    );
    let why = api_dump::held(&path, &grown).unwrap_err();
    assert!(why.contains("run `make api-golden`"), "{why}");
    assert!(why.contains("\"--b\": \"value\""), "{why}");
    std::fs::write(&path, &grown).unwrap();
    assert_eq!(api_dump::held(&path, &grown), Ok(()));
    // The form Python's json.dumps(indent=1) writes, the other baselines'.
    assert_eq!(
        grown,
        "{\n \"commands\": {\n  \"x\": {\n   \"--a\": \"switch\",\n   \"--b\": \"value\"\n  }\n },\n \
         \"exit codes\": {\n  \"0\": \"done\"\n }\n}\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `fennec args` in `cwd`: its code and what it said.
fn fennec(args: &[&str], cwd: &Path) -> (i32, String) {
    api_dump::run(Path::new(env!("CARGO_BIN_EXE_fennec")), args, cwd)
}

/// 2 is "refused before anything ran", as the dump says: a flag the
/// command does not read -- another of the backup family's -- or one
/// wanting a value it lacks is refused with the line, naming it, before
/// any file is made, read or opened, and the helps name each command's own
/// flags alone.
#[test]
fn a_line_is_refused_whole_before_anything_runs() {
    let scratch = api_dump::Scratch::new(
        std::env::temp_dir().join(format!("fennec-cli-refused-{}", std::process::id())),
    );
    let cwd = scratch.0.as_path();
    for (args, unknown) in [
        (&["key", "--to-change", "5", "k"][..], "--to-change"),
        (&["key", "--force", "k"], "--force"),
        (&["key", "--key-file", "other", "k"], "--key-file"),
        (&["verify", "--token", "t", "archive"], "--token"),
        (&["verify", "--force", "archive"], "--force"),
        (
            &["prune", "--tls-ca", "ca.pem", "archive", "--keep", "1d"],
            "--tls-ca",
        ),
        (
            &["backup", "--tenants", "http://127.0.0.1:1", "out"],
            "--tenants",
        ),
        (
            &[
                "backup",
                "--to",
                "2026-01-01T00:00:00Z",
                "http://127.0.0.1:1",
                "out",
            ],
            "--to",
        ),
        (
            &["archive", "--force", "http://127.0.0.1:1", "out"],
            "--force",
        ),
        (
            &["restore", "--token", "t", "archive", "out.fennec"],
            "--token",
        ),
        (
            &["restore", "--image-every", "1h", "archive", "out.fennec"],
            "--image-every",
        ),
    ] {
        let (code, said) = fennec(args, cwd);
        assert_eq!(code, 2, "{args:?}: {said}");
        assert!(
            said.contains(&format!("unknown option: {unknown}")),
            "{args:?}: {said}"
        );
        api_dump::touched_nothing(args, cwd, &said);
    }
    for args in [&["-c"][..], &["data.fennec", "--command"]] {
        let (code, said) = fennec(args, cwd);
        assert_eq!(code, 2, "{args:?}: {said}");
        assert!(said.contains("expects a statement"), "{args:?}: {said}");
        api_dump::touched_nothing(args, cwd, &said);
    }
    let (code, help) = fennec(&["key", "--help"], cwd);
    assert_eq!(code, 0, "{help}");
    for other in ["--force", "--key-file", "--to", "--tenants", "--token"] {
        assert!(
            !help.contains(other),
            "`fennec key --help` names {other}:\n{help}"
        );
    }
    let (code, help) = fennec(&["verify", "--help"], cwd);
    assert_eq!(code, 0, "{help}");
    for other in [
        "--force",
        "--to",
        "--token",
        "--tls-ca",
        "--image-every",
        "--keep",
    ] {
        assert!(
            !help.contains(other),
            "`fennec verify --help` names {other}:\n{help}"
        );
    }
    // A change is one database's: refused with the line, before the
    // directory to restore into is made.
    let args = ["restore", "--tenants", "root", "out", "--to-change", "5"];
    let (code, said) = fennec(&args, cwd);
    assert_eq!(code, 2, "{said}");
    api_dump::touched_nothing(&args, cwd, &said);
}

/// A node's archives that cannot be read are the work failing, 1, as one
/// archive that cannot be verified, pruned or restored is: the line was
/// not refused.
#[test]
fn a_tenants_root_that_cannot_be_read_is_1() {
    let scratch = api_dump::Scratch::new(
        std::env::temp_dir().join(format!("fennec-cli-unread-{}", std::process::id())),
    );
    let cwd = scratch.0.as_path();
    for args in [
        &["verify", "--tenants", "missing"][..],
        &["prune", "--tenants", "missing", "--keep", "1d"],
        &[
            "restore",
            "--tenants",
            "missing",
            "out",
            "--to",
            "2026-01-01T00:00:00Z",
        ],
    ] {
        let (code, said) = fennec(args, cwd);
        assert_eq!(code, 1, "{args:?}: {said}");
    }
}
