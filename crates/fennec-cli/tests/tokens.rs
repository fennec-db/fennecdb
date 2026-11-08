//! `fennec backup`, `archive`, `restore`, `prune` and `verify` given an empty
//! token or key: `--token "$REPL"` with REPL unset, an environment file's
//! line left empty. An empty FENNEC_REPLICATION_TOKEN is no token -- the
//! primary's ask for one is said before anything is sent -- and an empty
//! `--token` is refused; an empty key file, from the flag or FENNEC_KEY_FILE,
//! is refused rather than taken for none, which would write in the clear.

use std::process::{Command, Output};

fn fennec(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fennec"))
        .env_remove("FENNEC_REPLICATION_TOKEN")
        .env_remove("FENNEC_KEY_FILE")
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn said(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

// A port nothing listens on: a backup that got as far as connecting would
// say so, rather than what is asserted.
const PRIMARY: &str = "http://127.0.0.1:9";

#[test]
fn an_empty_replication_token_is_none_or_refused() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-tokens-{}", std::process::id()));
    let out_file = dir.join("out.fennec");
    let out_file = out_file.to_str().unwrap();

    let out = fennec(
        &["backup", PRIMARY, out_file],
        &[("FENNEC_REPLICATION_TOKEN", "")],
    );
    assert_eq!(out.status.code(), Some(2), "{}", said(&out));
    assert!(
        said(&out).contains("asks for its replication token"),
        "an empty FENNEC_REPLICATION_TOKEN was sent as a token: {}",
        said(&out)
    );

    let out = fennec(&["backup", PRIMARY, out_file, "--token", ""], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", said(&out));
    assert!(said(&out).contains("--token is empty"), "{}", said(&out));
}

/// `fennec key` writes a key its owner alone can read: mode 0600, and on
/// Windows, where a file inherits its directory's ACL, a protected DACL of
/// its owner alone, set before the key is written (docs/design/windows.md,
/// 7h). A key there already is never written over.
#[test]
fn a_new_key_is_its_owner_s_alone() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-new-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("archive.key");
    let _ = std::fs::remove_file(&key);
    let out = fennec(&["key", key.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    let hex = std::fs::read_to_string(&key).unwrap();
    assert_eq!(hex.trim_end().len(), 64, "{hex}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    #[cfg(windows)]
    {
        assert!(fennec_http::owner_only::owner_alone(&key).unwrap());
        assert_eq!(fennec_http::owner_only::open_to(&key).unwrap(), None);
    }
    let again = fennec(&["key", key.to_str().unwrap()], &[]);
    assert_eq!(again.status.code(), Some(2), "{}", said(&again));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), hex);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_empty_key_file_is_refused() {
    let dir = std::env::temp_dir().join(format!("fennec-cli-keys-{}", std::process::id()));
    let dir = dir.to_str().unwrap();
    for (args, env) in [
        (vec!["verify", dir], vec![("FENNEC_KEY_FILE", "")]),
        (vec!["verify", dir, "--key-file", ""], vec![]),
    ] {
        let out = fennec(&args, &env);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", said(&out));
        assert!(
            said(&out).contains("the key file is empty"),
            "{args:?}: {}",
            said(&out)
        );
    }
}
