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
    api_dump::hold(
        "cli-fennec.json",
        &api_dump::cli(exe, "fennec", &src, &codes),
    );
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
