//! The tokens a deployment keeps off the command line -- `ps` shows every
//! process's arguments to every user of the machine -- read from the
//! environment, as deploy/'s systemd units hand them over
//! (`EnvironmentFile=`): the admin token from FENNEC_ADMIN_TOKEN. A variable
//! set and empty, as an environment file leaves a line it does not use, is
//! no token; an empty one given as a flag is refused at the start, since
//! `/_admin/` reads a request with no `Authorization` as an empty token.

use crate::support::{start, start_env, tmp, try_start, try_start_env, Http};

#[test]
fn the_admin_token_is_read_from_the_environment() {
    let dir = tmp("tokens", "node");
    let server = start_env(
        &["--dir", dir.to_str().unwrap(), "--auth-delay", "0"],
        &[("FENNEC_ADMIN_TOKEN", "adm-from-env")],
    );
    assert_eq!(
        Http::open(server.port)
            .ask("GET", "/_admin/stats", "")
            .status,
        401
    );
    let mut admin = Http::open(server.port).with_token("adm-from-env");
    assert_eq!(admin.ask("GET", "/_admin/stats", "").status, 200);
    let made = admin.ask("PUT", "/_admin/tenants/acme", "");
    assert!(made.status / 100 == 2, "{} {}", made.status, made.body);
    assert!(
        admin
            .ask("GET", "/_admin/tenants", "")
            .body
            .contains("acme"),
        "the tenant the admin token made"
    );
}

#[test]
fn an_admin_token_set_empty_is_none() {
    // From the environment: /_admin/ is off, as with no token, never open.
    let dir = tmp("tokens", "empty-env");
    let server = start_env(
        &["--dir", dir.to_str().unwrap(), "--auth-delay", "0"],
        &[("FENNEC_ADMIN_TOKEN", "")],
    );
    let off = Http::open(server.port).ask("GET", "/_admin/stats", "");
    assert_eq!(off.status, 404, "{}", off.body);
    drop(server);

    // Given as a flag: refused before anything listens.
    let dir = tmp("tokens", "empty-flag");
    let log = match try_start(&["--dir", dir.to_str().unwrap(), "--admin-token", ""]) {
        Ok(server) => panic!(
            "an empty --admin-token was taken, /_admin/ open on port {}",
            server.port
        ),
        Err(log) => log,
    };
    assert!(log.contains("--admin-token is empty"), "{log}");
}

#[test]
fn every_token_given_empty_as_a_flag_is_refused() {
    // `--http-token "$TOKEN"` with TOKEN unset: an empty data token passed
    // the check that keeps a server off a public address and answered
    // nothing, an empty replication token turned replication off unsaid.
    let file = tmp("tokens", "flags.fennec");
    let file = file.to_str().unwrap();
    for flag in [
        "--http-token",
        "--replication-token",
        "--jwt-secret",
        "--jwt-keys",
    ] {
        let log = match try_start(&["--file", file, flag, ""]) {
            Ok(server) => panic!(
                "an empty {flag} was taken, the server on port {}",
                server.port
            ),
            Err(log) => log,
        };
        assert!(log.contains(&format!("{flag} is empty")), "{flag}: {log}");
    }
    // A secret's file holding nothing, likewise.
    let secret = tmp("tokens", "jwt.secret");
    std::fs::write(&secret, "\n").unwrap();
    let log = match try_start(&[
        "--file",
        file,
        "--jwt-secret-file",
        secret.to_str().unwrap(),
    ]) {
        Ok(server) => panic!("an empty secret's file was taken, port {}", server.port),
        Err(log) => log,
    };
    assert!(log.contains("is empty"), "{log}");
}

#[test]
fn a_secret_set_empty_in_the_environment_is_none() {
    // Left empty, FENNEC_JWT_SECRET and FENNEC_JWT_KEYS were taken for keys,
    // and the server refused to start for want of a --policy; unset, it
    // starts with no keys, no data token and no replication.
    let file = tmp("tokens", "env.fennec");
    let server = start_env(
        &["--file", file.to_str().unwrap()],
        &[
            ("FENNEC_JWT_SECRET", ""),
            ("FENNEC_JWT_KEYS", ""),
            ("FENNEC_REPLICATION_TOKEN", ""),
            ("FENNEC_HTTP_TOKEN", ""),
        ],
    );
    let status = Http::open(server.port)
        .ask("GET", "/_replication/status", "")
        .status;
    assert_ne!(status, 200, "replication on with an empty token");
}

#[test]
fn a_short_token_is_refused_outside_loopback() {
    // 31 bytes past loopback, each token as a flag and as its variable:
    // refused at the start, the flag and the minimum named, before the
    // store is made. The data and admin tokens are taken on --metrics'
    // listener too, and refused where that one alone is past loopback.
    let short = "s".repeat(31);
    let path = tmp("tokens", "short.fennec");
    let file = path.to_str().unwrap();
    let node_path = tmp("tokens", "short-node");
    let node = node_path.to_str().unwrap();
    let refused = |args: &[&str], env: &[(&str, &str)], flag: &str| {
        let log = match try_start_env(args, env) {
            Ok(server) => panic!(
                "{flag} of 31 bytes was taken past loopback, port {}",
                server.port
            ),
            Err(log) => log,
        };
        assert!(
            log.contains(&format!("{flag} (")) && log.contains("shorter than 32 bytes"),
            "{args:?} {env:?}: {log}"
        );
    };
    for (flag, var, store) in [
        ("--http-token", "FENNEC_HTTP_TOKEN", ["--file", file]),
        ("--admin-token", "FENNEC_ADMIN_TOKEN", ["--dir", node]),
        (
            "--replication-token",
            "FENNEC_REPLICATION_TOKEN",
            ["--file", file],
        ),
    ] {
        let given = [&store[..], &["--http", "0.0.0.0:0", flag, &short]].concat();
        refused(&given, &[], flag);
        let past = [&store[..], &["--http", "0.0.0.0:0"]].concat();
        refused(&past, &[(var, &short)], flag);
    }
    for flag in ["--http-token", "--admin-token"] {
        refused(
            &["--file", file, "--metrics", "0.0.0.0:0", flag, &short],
            &[],
            flag,
        );
    }
    assert!(!path.exists() && !node_path.exists(), "a store was opened");

    // 32 bytes pass the check past loopback: the server goes on to its
    // listener, here on an address of the documentation's, which no machine
    // holds -- or listens there, unreached, where the system binds
    // addresses it does not hold.
    let (data, admin, repl) = ("d".repeat(32), "a".repeat(32), "r".repeat(32));
    let args = [
        "--dir",
        node,
        "--http",
        "192.0.2.1:0",
        "--http-token",
        &data,
        "--admin-token",
        &admin,
        "--replication-token",
        &repl,
    ];
    if let Err(log) = try_start(&args) {
        assert!(
            log.contains("could not open the HTTP endpoint") && !log.contains("shorter than"),
            "{log}"
        );
    }

    // On loopback any length is taken: `make server TOKEN=secret`.
    let node_path = tmp("tokens", "short-loopback");
    let server = start(&[
        "--dir",
        node_path.to_str().unwrap(),
        "--http-token",
        "secret",
        "--admin-token",
        "adm",
        "--replication-token",
        "repl",
    ]);
    let mut admin = Http::open(server.port).with_token("adm");
    assert_eq!(admin.ask("GET", "/_admin/stats", "").status, 200);
}
