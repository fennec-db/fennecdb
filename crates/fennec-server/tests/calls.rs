//! A policy's `call` lines as the server meets them
//! (`docs/design/named-statements.md`, *Authority*): `* call` is refused at
//! the start, and a line naming a statement the database does not hold is
//! logged -- as the server starts, and as a `drop` takes one away -- since
//! a typo grants nothing, silently, otherwise.

use crate::support::*;
use std::time::Duration;

const SECRET: &str = "thirty-two bytes and a few more, for HS256";

fn policy(tag: &str, text: &str) -> (String, String) {
    let secret = tmp("calls", &format!("{tag}.secret"));
    std::fs::write(&secret, SECRET).unwrap();
    let policy = tmp("calls", &format!("{tag}.policy"));
    std::fs::write(&policy, text).unwrap();
    (
        secret.to_str().unwrap().to_string(),
        policy.to_str().unwrap().to_string(),
    )
}

#[test]
fn star_call_is_refused_at_the_start() {
    let file = tmp("calls", "star.fennec");
    for (n, (text, why)) in [
        ("* call\n", "`* call` grants no statement"),
        ("* call for admin\n", "`* call` grants no statement"),
        ("transfer read,call\n", "`call` stands alone"),
        ("transfer call where $from = 1\n", "`call` takes no filter"),
    ]
    .iter()
    .enumerate()
    {
        let (secret, policy) = policy(&format!("star{n}"), text);
        let log = match try_start(&[
            "--file",
            file.to_str().unwrap(),
            "--http-token",
            "root",
            "--jwt-secret-file",
            &secret,
            "--policy",
            &policy,
        ]) {
            Ok(s) => panic!("{text:?} was taken, port {}", s.port),
            Err(log) => log,
        };
        assert!(
            log.contains(&format!("policy line 1: {why}")),
            "{text:?}: {log}"
        );
    }
}

#[test]
fn a_call_line_naming_no_statement_is_logged() {
    let file = tmp("calls", "logged.fennec");
    let f = file.to_str().unwrap();
    // `mine` defined before the policy is read.
    {
        let server = start(&["--file", f, "--http-token", "root"]);
        let mut http = server.http().with_token("root");
        http.run("create collection notes (owner text @hash, body text)");
        http.run(
            "create statement mine() claims (sub text) as get notes select body where owner = \
             $jwt.sub",
        );
        server.terminate();
    }
    let (secret, policy) = policy("logged", "ghost call\nmine call for reader\n");
    let server = start(&[
        "--file",
        f,
        "--http-token",
        "root",
        "--jwt-secret-file",
        &secret,
        "--policy",
        &policy,
    ]);
    let ghost = "policy line 1 grants calling statement `ghost`";
    let mine = "policy line 2 grants calling statement `mine`";
    assert!(server.logged(ghost, Duration::from_secs(10)));
    assert!(!server.log.lock().unwrap().contains(mine));
    server.http().with_token("root").run("drop statement mine");
    assert!(server.logged(mine, Duration::from_secs(10)));
}

/// A `call` line naming a scheduled statement is logged, as one naming a
/// statement bound to writes is: the clock runs it, and no call of a
/// token's does. So is a `wake` line naming it, which no write wakes, and
/// either line naming a webhook consumer, whose events run it. The fixture
/// a server's build wrote holds `lapse`, run every 15 minutes, and
/// `stripe_paid`, taking hook `stripe`'s events.
#[test]
fn a_call_line_naming_a_scheduled_or_a_hook_s_statement_is_logged() {
    let file = tmp("calls", "scheduled.fennec");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fennec-core/tests/statements.fennec"
        ),
        &file,
    )
    .unwrap();
    let (secret, policy) = policy(
        "scheduled",
        "lapse call\nlapse wake\nstripe_paid call\nstripe_paid wake\n",
    );
    let server = start(&[
        "--file",
        file.to_str().unwrap(),
        "--http-token",
        "root",
        "--jwt-secret-file",
        &secret,
        "--policy",
        &policy,
    ]);
    for line in [
        "policy line 1 grants calling statement `lapse`, which runs by the clock",
        "policy line 2 grants waking statement `lapse`, which runs by the clock",
        "policy line 3 grants calling statement `stripe_paid`, which takes a hook's events",
        "policy line 4 grants waking statement `stripe_paid`, which takes a hook's events",
    ] {
        assert!(server.logged(line, Duration::from_secs(10)), "{line}");
    }
}
