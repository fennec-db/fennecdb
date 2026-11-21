//! A release's server file served by this build's `fennec-server`: the file
//! the release's own server wrote (fennec-core's `tests/compat/`,
//! `tools/compat_fixtures.py`), started on as a deployment upgraded in
//! place starts -- the same flags, the same hooks file -- on a port of its
//! own, found by its `listening on:` line. Every recorded statement is
//! asked over HTTP and answered as the release's server answered it once
//! started again on that file (`server.http.json`), and the requests that
//! reach the server's own collections by their own paths are answered as
//! it answered them: a write sent again with its `Idempotency-Key`
//! (`_idempotency`), a webhook event sent again (`_hook_events`), the
//! consumers of the changes (`_consumers`), the schema and the plan of a
//! migration it ran (`_migrations`). Then stopped as a supervisor stops it,
//! started again, and asked again: an upgraded server's restart.

#[path = "../../fennec-core/tests/compat/answers.rs"]
mod answers;

use crate::support::*;
use answers::{set_name, sets, Scratch, ACCEPTED_REQUESTS};
use fennec_core::prelude::Value;

/// `member` of a JSON object, as `server.http.json` holds it.
fn member<'a>(v: &'a Value, name: &str) -> &'a Value {
    match v {
        Value::Object(o) => o
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("no `{name}` in server.http.json")),
        _ => panic!("`{name}`: not an object"),
    }
}

fn text(v: &Value) -> &str {
    match v {
        Value::Text(t) => t,
        v => panic!("not a text: {v:?}"),
    }
}

fn list(v: &Value) -> &[Value] {
    match v {
        Value::List(l) => l,
        v => panic!("not a list: {v:?}"),
    }
}

fn int(v: &Value) -> u16 {
    match v {
        Value::Int(n) => *n as u16,
        v => panic!("not a number: {v:?}"),
    }
}

/// Every recorded statement and request asked of the server at `port`, as
/// the release's server answered each: what differs.
fn asked(
    port: u16,
    token: &str,
    set: &str,
    recorded: &Value,
    statements: &[String],
    when: &str,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut http = Http::open(port).with_token(token);
    let queries = list(member(recorded, "queries"));
    assert_eq!(
        queries.len(),
        statements.len(),
        "{set}: a statement an answer"
    );
    for (q, theirs) in statements.iter().zip(queries) {
        let (status, body) = match http.query(q) {
            Ok(body) => (200, body),
            Err(e) => e,
        };
        let theirs = list(theirs);
        if status != int(&theirs[0]) || body != text(&theirs[1]) {
            found.push(format!(
                "{set}/server.fennec over HTTP ({when}): {q}\n  release: {} {}\n  this build: {status} {body}",
                int(&theirs[0]),
                text(&theirs[1])
            ));
        }
    }
    for r in list(member(recorded, "requests")) {
        let (method, path) = (text(member(r, "method")), text(member(r, "path")));
        let headers: Vec<(&str, &str)> = match member(r, "headers") {
            Value::Object(o) => o.iter().map(|(k, v)| (k.as_str(), text(v))).collect(),
            _ => Vec::new(),
        };
        let mut client = Http::open(port);
        if matches!(member(r, "token"), Value::Bool(true)) {
            client = client.with_token(token);
        }
        let a = client.ask_with(method, path, text(member(r, "body")), &headers);
        let key = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("idempotency-key"))
            .map(|&(_, v)| v);
        let accepted = ACCEPTED_REQUESTS
            .iter()
            .find(|x| x.set == set && x.method == method && x.path == path && x.key == key);
        let (status, answer) = match accepted {
            Some(x) => (x.status, x.body),
            None => (int(member(r, "status")), text(member(r, "answer"))),
        };
        if a.status != status || a.body != answer {
            found.push(format!(
                "{set}/server.fennec ({when}): {method} {path}\n  {}: {status} {answer}\n  this build: {} {}",
                if accepted.is_some() { "accepted" } else { "release" },
                a.status,
                a.body
            ));
        }
    }
    found
}

#[test]
fn a_release_s_server_file_is_served_and_answers_as_its_server_did() {
    let mut found = Vec::new();
    let mut served = 0;
    for set in sets() {
        let name = set_name(&set);
        let Ok(recorded) = std::fs::read_to_string(set.join("server.http.json")) else {
            continue;
        };
        let recorded = fennec_core::json::parse_json(&recorded).unwrap();
        let serve = member(&recorded, "serve");
        let token = text(member(serve, "token"));
        let scratch = Scratch::new(&format!("server-{name}"));
        let file = scratch.0.join("server.fennec");
        std::fs::copy(set.join("server.fennec"), &file).unwrap();
        let secret = scratch.0.join("hook.secret");
        std::fs::write(&secret, text(member(serve, "secret"))).unwrap();
        let hooks = scratch.0.join("hooks");
        let line = text(member(serve, "hooks")).replace("{secret}", secret.to_str().unwrap());
        std::fs::write(&hooks, format!("{line}\n")).unwrap();
        let mut args: Vec<&str> = list(member(serve, "args")).iter().map(text).collect();
        args.extend([
            "--file",
            file.to_str().unwrap(),
            "--hooks",
            hooks.to_str().unwrap(),
        ]);
        let statements = answers::statements(&set, "server");
        for when in ["started on the release's file", "stopped and started again"] {
            let server = start(&args);
            found.extend(asked(
                server.port,
                token,
                &name,
                &recorded,
                &statements,
                when,
            ));
            let (code, log) = server.terminate();
            assert_eq!(code, 0, "{name}: the server's stop ({when}):\n{log}");
        }
        served += 1;
    }
    assert!(served > 0, "no set holds server.http.json");
    assert!(
        found.is_empty(),
        "{} answers differ:\n{}",
        found.len(),
        found.join("\n")
    );
}
