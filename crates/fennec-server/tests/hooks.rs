//! Webhook consumers on a file's server (`docs/design/named-statements.md`,
//! *Webhook consumers (F4)*): `--hooks` read and refused at the start,
//! `POST /_hooks/<name>` verified before anything of it is read -- every
//! refusal before the one 401, its check named in the log, the audit line
//! and `/_metrics` alone -- and a verified event taken once by its key by
//! the statements defined `on hook`, answered by what it came to, as a
//! scoped caller is told: the binary as a deployment runs it, each event
//! signed as its sender signs it.

use crate::support::*;
use fennec_http::crypto::{b64_encode, b64url_encode, hmac_sha256, sha256};
use std::path::PathBuf;

use std::time::Duration;

const STRIPE_SECRET: &str = "whsec_a_secret_stripe_shows";
const STRIPE_TEST_SECRET: &str = "whsec_the_test_mode_secret";
const GITHUB_SECRET: &str = "a word its user chose";
/// A Standard Webhooks secret's key, `whsec_` and its base64 in its file.
const STANDARD_KEY: &[u8] = b"twenty-four bytes of key";
const ROOT: &str = "root";
const JWT_SECRET: &str = "thirty-two bytes and a few more, for HS256";

const ORDERS: &str = "create collection orders (ref text @hash, status text, intent text, total \
                      int, currency text, paid_at timestamp)";
const STRIPE_PAID: &str = "create statement stripe_paid(
    $intent text from event.data.object.id,
    $order text from event.data.object.metadata.order,
    $amount int from event.data.object.amount_received,
    $currency text from event.data.object.currency,
    $created int from event.created)
  on hook stripe
  where event.type = 'payment_intent.succeeded' and event.livemode = true
  as set orders {status: 'paid', intent: $intent, paid_at: $created * 1000}
     where ref = $order and total = $amount and currency = $currency
       and (status = 'open' or intent = $intent)
     require 1 else 'no open order of that total'";

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A file of `text` under this test's directory.
fn file(tag: &str, name: &str, text: &[u8]) -> String {
    let path = tmp(tag, name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_string()
}

/// A hooks file naming `stripe`, `stripe_test`, `github` (keyed by its
/// body), `delivered` (GitHub's, keyed by its delivery) and `standard`,
/// each secret in a file of its own, and the lines `more` beside them.
fn hooks_file(tag: &str, more: &str) -> String {
    let stripe = file(tag, "stripe.whsec", STRIPE_SECRET.as_bytes());
    let test = file(
        tag,
        "stripe-test.whsec",
        format!("{STRIPE_TEST_SECRET}\n").as_bytes(),
    );
    let github = file(tag, "github.secret", GITHUB_SECRET.as_bytes());
    let standard = file(
        tag,
        "standard.whsec",
        format!("whsec_{}\n", b64_encode(STANDARD_KEY)).as_bytes(),
    );
    file(
        tag,
        "hooks",
        format!(
            "# hook       scheme    secret files, then options\n\
             stripe       stripe    {stripe}   tolerance 5m\n\
             stripe_test  stripe    {test}\n\
             github       github    {github}\n\
             delivered    github    {github}   key delivery\n\
             standard     standard  {standard}\n\
             {more}"
        )
        .as_bytes(),
    )
}

/// A file's server taking the hooks above, its token `root`, every refusal
/// waiting nothing; the orders `o-1` (2000 eur) and `o-2` (500 usd) open,
/// and `stripe_paid` defined.
fn serve(tag: &str, flags: &[&str]) -> (Server, PathBuf) {
    serve_at(tag, &tmp(tag, "data.fennec"), flags)
}

/// [`serve`] over the file `path`.
fn serve_at(tag: &str, path: &std::path::Path, flags: &[&str]) -> (Server, PathBuf) {
    let hooks = hooks_file(tag, "");
    let audit = tmp(tag, "audit.log");
    let mut args = vec![
        "--file",
        path.to_str().unwrap(),
        "--http-token",
        ROOT,
        "--hooks",
        &hooks,
        "--auth-delay",
        "0",
        "--audit",
        audit.to_str().unwrap(),
    ];
    args.extend_from_slice(flags);
    let server = start(&args);
    let mut http = server.http().with_token(ROOT);
    http.run(ORDERS);
    http.run("put orders {ref: 'o-1', status: 'open', total: 2000, currency: 'eur'}");
    http.run("put orders {ref: 'o-2', status: 'open', total: 500, currency: 'usd'}");
    http.run(STRIPE_PAID);
    (server, audit)
}

/// A Stripe `payment_intent.succeeded` for `order`, as Stripe's Snapshot
/// events carry it.
fn paid(id: &str, intent: &str, order: &str, amount: i64, currency: &str) -> String {
    format!(
        r#"{{"id":"{id}","object":"event","type":"payment_intent.succeeded","livemode":true,"created":1711627200,"data":{{"object":{{"id":"{intent}","amount_received":{amount},"currency":"{currency}","metadata":{{"order":"{order}"}}}}}}}}"#
    )
}

/// `Stripe-Signature` for `body` signed at `t` under `secret`.
fn stripe_sig(secret: &str, t: u64, body: &str) -> String {
    let mac = hmac_sha256(secret.as_bytes(), format!("{t}.{body}").as_bytes());
    format!("t={t},v1={}", hex(&mac))
}

/// `body` sent to hook `hook` with `headers`, as JSON.
fn send(server: &Server, hook: &str, headers: &[(&str, &str)], body: &str) -> Answer {
    let mut h = vec![("Content-Type", "application/json")];
    h.extend_from_slice(headers);
    server
        .http()
        .ask_with("POST", &format!("/_hooks/{hook}"), body, &h)
}

/// `body` signed now as Stripe signs it, to `hook`.
fn stripe(server: &Server, hook: &str, body: &str) -> Answer {
    let sig = stripe_sig(STRIPE_SECRET, now_s(), body);
    send(server, hook, &[("Stripe-Signature", &sig)], body)
}

fn status_of(server: &Server, order: &str) -> String {
    let body = server
        .http()
        .with_token(ROOT)
        .run(&format!("get orders select status where ref = '{order}'"));
    column(&body, "status").join(",")
}

/// The `fennec_hook_*` samples a scrape holds, as `name{labels} value`.
fn hook_samples(server: &Server) -> Vec<String> {
    let a = server.http().with_token(ROOT).ask("GET", "/_metrics", "");
    assert_eq!(a.status, 200, "{}", a.body);
    a.body
        .lines()
        .filter(|l| l.starts_with("fennec_hook_"))
        .map(str::to_string)
        .collect()
}

fn sample(server: &Server, line: &str) -> u64 {
    let samples = hook_samples(server);
    samples
        .iter()
        .find_map(|l| l.strip_prefix(line)?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no {line} in {samples:?}"))
}

/// An event of Stripe's taken once by its key: its write and its key one
/// block, answered 200 with the key and how many statements took it; sent
/// again -- re-signed, as Stripe re-signs a retry -- answered as the first
/// with `Idempotent-Replayed`, nothing run again; sent with another body
/// under its key, answered as the first too, never run, logged and counted
/// a conflict. A number in seconds read into an `int` and written times
/// 1000 is the instant Stripe meant.
#[test]
fn a_stripe_event_is_taken_once_by_its_key() {
    let (server, _) = serve("taken", &[]);
    let body = paid("evt_1", "pi_1", "o-1", 2000, "eur");
    let a = stripe(&server, "stripe", &body);
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(a.body, r#"{"event":"evt_1","taken":1}"#);
    assert!(a.header("Fennec-Seq").is_some());
    assert!(a.header("Idempotent-Replayed").is_none());
    assert_eq!(status_of(&server, "o-1"), "paid");
    let at = server
        .http()
        .with_token(ROOT)
        .run("get orders select paid_at where ref = 'o-1'");
    assert!(at.contains("2024-03-28T12:00:00"), "{at}");

    // The order set back to open: a replay running again would pay it.
    server
        .http()
        .with_token(ROOT)
        .run("set orders {status: 'open'} where ref = 'o-1'");
    let again = stripe(&server, "stripe", &body);
    assert_eq!((again.status, &again.body), (200, &a.body));
    assert_eq!(again.header("Idempotent-Replayed"), Some("true"));
    assert_eq!(status_of(&server, "o-1"), "open", "the replay ran");

    let other = body.replace(
        "\"livemode\":true",
        "\"livemode\":true,\"api_version\":\"2026-10-28\"",
    );
    let conflict = stripe(&server, "stripe", &other);
    assert_eq!((conflict.status, &conflict.body), (200, &a.body));
    assert_eq!(conflict.header("Idempotent-Replayed"), Some("true"));
    assert!(server.logged(
        "event \"evt_1\" came again with another body",
        Duration::from_secs(10)
    ));
    assert_eq!(
        sample(&server, "fennec_hook_events_total{outcome=\"taken\"}"),
        1
    );
    assert_eq!(
        sample(&server, "fennec_hook_events_total{outcome=\"replayed\"}"),
        1
    );
    assert_eq!(
        sample(&server, "fennec_hook_events_total{outcome=\"conflict\"}"),
        1
    );
}

/// Every refusal before a verified signature is one 401 with one body --
/// a hook missing, a scheme not the hook's, a timestamp past the
/// tolerance, a body past the ceiling, a header sent twice, a signature
/// that does not verify -- its check named in the log, the audit's
/// `refused` line and `fennec_hook_unverified_total` alone. Any other
/// method is 405 on every path under `/_hooks/` alike.
#[test]
fn every_refusal_before_the_signature_is_one_401() {
    let (server, audit) = serve("unverified", &[]);
    let body = paid("evt_2", "pi_2", "o-1", 2000, "eur");
    let t = now_s();
    let good = stripe_sig(STRIPE_SECRET, t, &body);
    let gh = format!(
        "sha256={}",
        hex(&hmac_sha256(GITHUB_SECRET.as_bytes(), body.as_bytes()))
    );
    let big = format!(r#"{{"id":"evt_big","pad":"{}"}}"#, "x".repeat(1 << 20));
    let big_sig = stripe_sig(STRIPE_SECRET, t, &big);
    let stale = stripe_sig(STRIPE_SECRET, t - 301, &body);
    let wrong = stripe_sig(STRIPE_TEST_SECRET, t, &body);
    // A case: the hook, the headers sent, the body and the check named.
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str, &'a str);
    let cases: [Case; 7] = [
        ("nosuch", vec![("Stripe-Signature", &good)], &body, "hook"),
        (
            "stripe",
            vec![("X-Hub-Signature-256", &gh)],
            &body,
            "scheme",
        ),
        ("stripe", vec![("Stripe-Signature", &stale)], &body, "stale"),
        ("stripe", vec![("Stripe-Signature", &big_sig)], &big, "size"),
        (
            "stripe",
            vec![("Stripe-Signature", &good), ("Stripe-Signature", &good)],
            &body,
            "header",
        ),
        ("stripe", vec![], &body, "header"),
        (
            "stripe",
            vec![("Stripe-Signature", &wrong)],
            &body,
            "signature",
        ),
    ];
    let mut answers = Vec::new();
    for (hook, headers, body, check) in &cases {
        let a = send(&server, hook, headers, body);
        assert_eq!(a.status, 401, "{check}: {}", a.body);
        assert!(a.header("WWW-Authenticate").is_none(), "{check}");
        answers.push(a.body);
        assert!(
            server.logged(
                &format!("did not verify ({check})"),
                Duration::from_secs(10)
            ),
            "{check}"
        );
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:?}");
    assert_eq!(answers[0], r#"{"error":"the event does not verify"}"#);
    assert!(
        !answers[0].contains("stripe"),
        "the answer names no hook, no scheme, no check"
    );
    for (check, n) in [
        ("hook", 1),
        ("scheme", 1),
        ("stale", 1),
        ("size", 1),
        ("header", 2),
        ("signature", 1),
    ] {
        let line = format!("fennec_hook_unverified_total{{check=\"{check}\"}}");
        assert_eq!(sample(&server, &line), n, "{check}");
    }
    let audit = std::fs::read_to_string(audit).unwrap();
    let refused: Vec<&str> = audit
        .lines()
        .filter(|l| l.contains("\"refused\""))
        .collect();
    assert_eq!(refused.len(), 7, "{audit}");
    assert!(refused
        .iter()
        .any(|l| l.contains("outside its hook's tolerance")));
    assert!(!audit.contains(STRIPE_SECRET) && !audit.contains("evt_2"));
    let a = server.http().ask("GET", "/_hooks/stripe", "");
    assert_eq!((a.status, a.header("Allow")), (405, Some("POST")));
    let b = server.http().ask("PUT", "/_hooks/nosuch", "{}");
    assert_eq!((b.status, b.header("Allow")), (405, Some("POST")));
    assert_eq!(a.body, b.body);
    assert_eq!(
        sample(&server, "fennec_hook_events_total{outcome=\"taken\"}"),
        0
    );
}

/// What a verified event is answered, in the order the design sets: a
/// content type not JSON's 415, a held write's header 400, a body no
/// object 400, a Stripe event naming no `id` 400; a hook no statement
/// takes 409; an event no condition takes 200 `taken: 0` with nothing
/// written; a parameter of another type 400 naming the parameter and its
/// path and no statement; a guard unmet 412 with its reason, the order
/// paid at a retry once its row lands -- an event out of order.
#[test]
fn a_verified_event_is_answered_by_what_it_came_to() {
    let (server, _) = serve("answers", &[]);
    let body = paid("evt_3", "pi_3", "o-3", 900, "eur");
    let sig = stripe_sig(STRIPE_SECRET, now_s(), &body);
    let form = server.http().ask_with(
        "POST",
        "/_hooks/stripe",
        &body,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Stripe-Signature", &sig),
        ],
    );
    assert_eq!(form.status, 415, "{}", form.body);
    assert!(form.body.contains("application/json"), "{}", form.body);
    let held = send(
        &server,
        "stripe",
        &[("Stripe-Signature", &sig), ("Fennec-Wait", "5")],
        &body,
    );
    assert_eq!(held.status, 400, "{}", held.body);
    for (bad, says) in [("[1, 2]", "a JSON object"), (r#"{"type":"x"}"#, "`id`")] {
        let a = stripe(&server, "stripe", bad);
        assert_eq!(a.status, 400, "{}", a.body);
        assert!(a.body.contains(says), "{}", a.body);
    }
    let sig = stripe_sig(STRIPE_TEST_SECRET, now_s(), &body);
    let none = send(&server, "stripe_test", &[("Stripe-Signature", &sig)], &body);
    assert_eq!(none.status, 409, "{}", none.body);
    assert!(none.body.contains("no statement takes hook `stripe_test`"));

    let other = body.replace("payment_intent.succeeded", "charge.refunded");
    let a = stripe(&server, "stripe", &other);
    assert_eq!((a.status, a.body.as_str()), (200, r#"{"taken":0}"#));
    let test_mode = body.replace("\"livemode\":true", "\"livemode\":false");
    let a = stripe(&server, "stripe", &test_mode);
    assert_eq!((a.status, a.body.as_str()), (200, r#"{"taken":0}"#));

    let typed = body.replace("\"amount_received\":900", "\"amount_received\":\"900\"");
    let a = stripe(&server, "stripe", &typed);
    assert_eq!(a.status, 400, "{}", a.body);
    assert!(
        a.body.contains("`$amount` is int") && a.body.contains("event.data.object.amount_received"),
        "{}",
        a.body
    );
    assert!(
        !a.body.contains("stripe_paid"),
        "a statement's name: {}",
        a.body
    );

    let early = stripe(&server, "stripe", &body);
    assert_eq!(early.status, 412, "{}", early.body);
    assert!(early.body.contains("no open order of that total"));
    assert!(!early.body.contains("stripe_paid"), "{}", early.body);
    assert!(server.logged("event \"evt_3\" answered 412", Duration::from_secs(10)));
    server
        .http()
        .with_token(ROOT)
        .run("put orders {ref: 'o-3', status: 'open', total: 900, currency: 'eur'}");
    let retry = stripe(&server, "stripe", &body);
    assert_eq!(
        (retry.status, retry.body.as_str()),
        (200, r#"{"event":"evt_3","taken":1}"#)
    );
    assert_eq!(status_of(&server, "o-3"), "paid");
    let keys = server.http().with_token(ROOT).run("get _hook_events count");
    assert_eq!(count(&keys), 1, "a key for the taken event alone: {keys}");
}

/// Stripe may send two Event objects for one change: both are taken, the
/// second meeting `stripe_paid`'s guard on the intent and writing what is
/// there. Two statements on one hook take an event in name order, the
/// second reading the first's write, in one block with its key -- and a
/// binding its write wakes lands in it too.
#[test]
fn two_events_for_one_intent_and_two_statements_for_one_event() {
    let (server, _) = serve("two", &[]);
    let mut http = server.http().with_token(ROOT);
    http.run("create collection ledger (ref text @hash, what text)");
    http.run(
        "create statement booked after set orders where new.status = 'paid' and old.status = \
         'open' as insert ledger {ref: new.ref, what: 'booked'}",
    );
    http.run(
        "create statement stripe_receipt($order text from event.data.object.metadata.order) on \
         hook stripe where event.type = 'payment_intent.succeeded' as begin
           get orders select id where ref = $order and status = 'paid' require 1;
           insert ledger {ref: $order, what: 'receipt'}
         end",
    );
    let a = stripe(&server, "stripe", &paid("evt_4", "pi_4", "o-2", 500, "usd"));
    assert_eq!(
        (a.status, a.body.as_str()),
        (200, r#"{"event":"evt_4","taken":2}"#)
    );
    let b = stripe(&server, "stripe", &paid("evt_5", "pi_4", "o-2", 500, "usd"));
    assert_eq!(
        (b.status, b.body.as_str()),
        (200, r#"{"event":"evt_5","taken":2}"#)
    );
    let ledger = http.run("get ledger select what where ref = 'o-2' order what");
    assert_eq!(column(&ledger, "what"), ["booked", "receipt", "receipt"]);
}

/// GitHub's key: under `key body` a delivery sent again under another
/// `X-GitHub-Delivery` is answered from its key, and two dispatches of one
/// payload are one event; under `key delivery` the two dispatches are
/// taken twice, and a redelivery under its GUID answered from it.
/// `hook.event` is `X-GitHub-Event`.
#[test]
fn github_keys_its_events_by_body_or_by_delivery() {
    let (server, _) = serve("github", &[]);
    let mut http = server.http().with_token(ROOT);
    http.run("create collection pushes (via text, kind text, sha text)");
    for hook in ["github", "delivered"] {
        http.run(&format!(
            "create statement {hook}_push($kind text from hook.event, $sha text from \
             event.sha) on hook {hook} where hook.event = 'push' as insert pushes {{via: \
             '{hook}', kind: $kind, sha: $sha}}"
        ));
    }
    let body = r#"{"ref":"refs/heads/main","sha":"abc123"}"#;
    let sig = format!(
        "sha256={}",
        hex(&hmac_sha256(GITHUB_SECRET.as_bytes(), body.as_bytes()))
    );
    let deliver = |hook: &str, guid: &str| {
        send(
            &server,
            hook,
            &[
                ("X-Hub-Signature-256", &sig),
                ("X-GitHub-Event", "push"),
                ("X-GitHub-Delivery", guid),
            ],
            body,
        )
    };
    let digest = hex(&sha256(body.as_bytes()));
    let a = deliver("github", "guid-1");
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(a.body, format!(r#"{{"event":"{digest}","taken":1}}"#));
    let b = deliver("github", "guid-2");
    assert_eq!(
        (b.status, b.header("Idempotent-Replayed")),
        (200, Some("true"))
    );
    let c = deliver("delivered", "guid-1");
    assert_eq!(c.body, r#"{"event":"guid-1","taken":1}"#);
    let d = deliver("delivered", "guid-2");
    assert_eq!(d.body, r#"{"event":"guid-2","taken":1}"#);
    let e = deliver("delivered", "guid-2");
    assert_eq!(e.header("Idempotent-Replayed"), Some("true"));
    let rows = http.run("get pushes select via, kind order via");
    assert_eq!(column(&rows, "via"), ["delivered", "delivered", "github"]);
    assert_eq!(column(&rows, "kind"), ["push", "push", "push"]);
    let missing = send(&server, "delivered", &[("X-Hub-Signature-256", &sig)], body);
    assert_eq!(missing.status, 400, "{}", missing.body);
    assert!(missing.body.contains("X-GitHub-Delivery"));
}

/// A Standard Webhooks sender's event, keyed by its `webhook-id`, with
/// `hook.at` the instant its `webhook-timestamp` signs.
#[test]
fn a_standard_webhooks_event_is_keyed_by_its_id() {
    let (server, _) = serve("standard", &[]);
    let mut http = server.http().with_token(ROOT);
    http.run("create collection users (ext text @hash, at timestamp)");
    http.run(
        "create statement user_created($ext text from event.data.id, $at timestamp from \
         hook.at) on hook standard where event.type = 'user.created' as insert users {ext: \
         $ext, at: $at}",
    );
    let body = r#"{"type":"user.created","data":{"id":"user_1"}}"#;
    let t = now_s().to_string();
    let mac = hmac_sha256(STANDARD_KEY, format!("msg_1.{t}.{body}").as_bytes());
    let sig = format!("v1,{}", b64_encode(&mac));
    let headers = [
        ("webhook-id", "msg_1"),
        ("webhook-timestamp", t.as_str()),
        ("webhook-signature", sig.as_str()),
    ];
    let a = send(&server, "standard", &headers, body);
    assert_eq!(
        (a.status, a.body.as_str()),
        (200, r#"{"event":"msg_1","taken":1}"#)
    );
    let b = send(&server, "standard", &headers, body);
    assert_eq!(b.header("Idempotent-Replayed"), Some("true"));
    assert_eq!(count(&http.run("get users count")), 1);
}

/// An HS256 token for `claims`, an hour's `exp` added.
fn token(claims: &str) -> String {
    let exp = now_s() + 3600;
    let open = claims.strip_suffix('}').expect("a JSON object");
    let claims = format!("{open},\"exp\":{exp}}}");
    let head = b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let body = b64url_encode(claims.as_bytes());
    let sig = hmac_sha256(JWT_SECRET.as_bytes(), format!("{head}.{body}").as_bytes());
    format!("{head}.{body}.{}", b64url_encode(&sig))
}

/// The server's token calls a hook's statement with an event as its
/// argument, its condition asked and its parameters read as a delivery's,
/// keeping no key; called with none, it is refused. A scoped token -- even
/// one whose policy names the statement in a `call` line -- meets the 404
/// a missing statement is, at `/query`, `/batch` and `/_call`, and finds it
/// in no listing of the database's; the server says at its start that the
/// line grants nothing.
#[test]
fn only_the_servers_token_calls_a_hooks_statement() {
    let secret = file("call", "jwt.secret", JWT_SECRET.as_bytes());
    let policy = file("call", "policy", b"orders read\nstripe_paid call\n");
    let (server, _) = serve("call", &["--jwt-secret-file", &secret, "--policy", &policy]);
    let event = paid("evt_call", "pi_c", "o-2", 500, "usd");
    let mut root = server.http().with_token(ROOT);
    let a = root.ask(
        "POST",
        "/_call/stripe_paid",
        &format!(r#"{{"params":{event}}}"#),
    );
    assert_eq!(a.status, 200, "{}", a.body);
    assert_eq!(status_of(&server, "o-2"), "paid");
    match root.query("get _hook_events count") {
        Ok(b) => assert_eq!(count(&b), 0, "a call keeps no key"),
        Err((404, _)) => {}
        Err(e) => panic!("{e:?}"),
    }
    let passed = event.replace("payment_intent.succeeded", "charge.refunded");
    let refused = root.ask(
        "POST",
        "/_call/stripe_paid",
        &format!(r#"{{"params":{passed}}}"#),
    );
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert!(refused.body.contains("condition passes this event over"));
    let none = root.query("call stripe_paid").unwrap_err();
    assert!(none.1.contains("takes a hook's events"), "{none:?}");

    let user = token(r#"{"sub":"u1"}"#);
    let mut scoped = server.http().with_token(&user);
    let a = scoped.ask(
        "POST",
        "/_call/stripe_paid",
        &format!(r#"{{"params":{event}}}"#),
    );
    assert_eq!(a.status, 404, "{}", a.body);
    let (status, body) = scoped.query("call stripe_paid {id: 'x'}").unwrap_err();
    assert_eq!(status, 404, "{body}");
    let batch = scoped.ask(
        "POST",
        "/batch",
        r#"{"query": "call stripe_paid {id: 'x'}"}"#,
    );
    assert_eq!(batch.status, 404, "{}", batch.body);
    // The database's listings are the server's token's: a scoped token is
    // told what its policy grants by `/_whoami`, which echoes the policy.
    let (status, _) = scoped.query("statements").unwrap_err();
    assert_eq!(status, 403);
    let schema = scoped.ask("GET", "/_schema?as=fennecql", "");
    assert!(!schema.body.contains("stripe_paid"), "{}", schema.body);

    // Started again over the file holding it, the server says the policy's
    // `call` line grants nothing.
    server.terminate();
    let path = std::env::temp_dir()
        .join(format!("fennec-server-call-{}", std::process::id()))
        .join("data.fennec");
    let hooks = hooks_file("call", "");
    let again = start(&[
        "--file",
        path.to_str().unwrap(),
        "--http-token",
        ROOT,
        "--hooks",
        &hooks,
        "--jwt-secret-file",
        &secret,
        "--policy",
        &policy,
    ]);
    assert!(again.logged(
        "grants calling statement `stripe_paid`, which takes a hook's events: a token held to a \
         policy never calls it, so the line grants nothing",
        Duration::from_secs(10)
    ));
}

/// A server taking no writes, or one of its marked reads, answers a
/// verified event 403 -- and a forged one the 401 all the same.
#[test]
fn a_server_taking_no_writes_takes_no_event() {
    let path = tmp("readonly", "data.fennec");
    serve_at("readonly", &path, &[]).0.terminate();
    let hooks = hooks_file("readonly", "");
    let server = start(&[
        "--file",
        path.to_str().unwrap(),
        "--http-token",
        ROOT,
        "--hooks",
        &hooks,
        "--http-read-only",
        "--auth-delay",
        "0",
    ]);
    let body = paid("evt_ro", "pi_ro", "o-1", 2000, "eur");
    let a = stripe(&server, "stripe", &body);
    assert_eq!(a.status, 403, "{}", a.body);
    let forged = send(
        &server,
        "stripe",
        &[("Stripe-Signature", "t=1,v1=00")],
        &body,
    );
    assert_eq!(forged.status, 401);
    assert_eq!(status_of(&server, "o-1"), "open");
}

/// `--hooks` refused at the start, naming its line and never a secret: an
/// empty secret file, a scheme unknown, a hook named twice, a `key` on a
/// hook not GitHub's, a `keep` inside twice the tolerance and a minute, a
/// tolerance past an hour; and the flag beside `--dir`, or with no file.
#[test]
fn a_hooks_file_is_refused_at_the_start() {
    let path = tmp("refused", "data.fennec");
    let empty = file("refused", "empty.secret", b"\n");
    let secret = file("refused", "ok.secret", b"whsec_never_logged");
    for (n, (line, says)) in [
        (format!("a stripe {empty}"), "is empty"),
        (format!("a carrier {secret}"), "no scheme"),
        (
            format!("a stripe {secret}\na stripe {secret}"),
            "named twice",
        ),
        (format!("a stripe {secret} key delivery"), "`github` hook's"),
        (
            format!("a stripe {secret} tolerance 1h keep 2h"),
            "a day at least",
        ),
        (
            format!("a stripe {secret} tolerance 2h"),
            "a second to an hour",
        ),
    ]
    .iter()
    .enumerate()
    {
        let hooks = file("refused", &format!("hooks{n}"), line.as_bytes());
        let log = match try_start(&["--file", path.to_str().unwrap(), "--hooks", &hooks]) {
            Ok(s) => panic!("{line:?} was taken, port {}", s.port),
            Err(log) => log,
        };
        assert!(log.contains("hooks file line "), "{line:?}: {log}");
        assert!(log.contains(says), "{line:?}: {log}");
        assert!(!log.contains("never_logged"), "a secret logged: {log}");
    }
    let hooks = file("refused", "fine", format!("a stripe {secret}").as_bytes());
    let dir = tmp("refused", "tenants");
    std::fs::create_dir_all(&dir).unwrap();
    for (args, says) in [
        (
            vec!["--dir", dir.to_str().unwrap(), "--hooks", &hooks],
            "a node's tenants take none yet",
        ),
        (vec!["--hooks", &hooks], "give --file"),
    ] {
        let log = match try_start(&args) {
            Ok(s) => panic!("{args:?} was taken, port {}", s.port),
            Err(log) => log,
        };
        assert!(log.contains(says), "{args:?}: {log}");
    }
}

/// A server with no hooks file has no such route and reads no secret: a
/// signed event to `/_hooks/` is asked for a token as every unknown path
/// is, `/_metrics` writes none of the hooks' families, and a statement
/// taking a hook is logged as taking events no secret verifies.
#[test]
fn a_server_with_no_hook_reads_no_secret() {
    let path = tmp("nohooks", "data.fennec");
    let server = start(&["--file", path.to_str().unwrap(), "--http-token", ROOT]);
    let mut http = server.http().with_token(ROOT);
    http.run(ORDERS);
    http.run(STRIPE_PAID);
    assert!(server.logged(
        "statement `stripe_paid` takes hook `stripe`'s events, and this server has no hooks file",
        Duration::from_secs(10)
    ));
    let a = stripe(
        &server,
        "stripe",
        &paid("evt_n", "pi_n", "o-1", 2000, "eur"),
    );
    assert_eq!(a.status, 401, "{}", a.body);
    assert_eq!(
        a.header("WWW-Authenticate"),
        Some("Bearer"),
        "a token's 401"
    );
    assert!(!a.body.contains("does not verify"), "{}", a.body);
    assert!(
        hook_samples(&server).is_empty(),
        "{:?}",
        hook_samples(&server)
    );
}

/// A hook the hooks file names that no statement takes is logged as the
/// server starts, and a statement taking one it does not name as it is
/// defined; a definition is an audit line naming its hook, its quoted key
/// kept as written.
#[test]
fn hooks_and_statements_that_miss_each_other_are_logged() {
    let (server, audit) = serve("missed", &[]);
    assert!(server.logged(
        "hook `stripe_test` of the hooks file: no statement takes its events",
        Duration::from_secs(10)
    ));
    let mut http = server.http().with_token(ROOT);
    http.run(
        "create statement shop($o text from event.metadata.\"order-id\") on hook shopify as \
         del orders where ref = $o",
    );
    assert!(server.logged(
        "statement `shop` takes hook `shopify`'s events, which the hooks file does not name",
        Duration::from_secs(10)
    ));
    let lines = std::fs::read_to_string(&audit).unwrap();
    let line = lines
        .lines()
        .find(|l| l.contains("create statement shop"))
        .unwrap_or_else(|| panic!("{lines}"));
    assert!(line.contains(r#"\"order-id\""#), "{line}");
    assert!(line.contains(r#""hook":"shopify""#), "{line}");
    http.run("drop statement shop");
    let lines = std::fs::read_to_string(&audit).unwrap();
    let drop = lines
        .lines()
        .find(|l| l.contains("drop statement shop"))
        .unwrap_or_else(|| panic!("{lines}"));
    assert!(drop.contains(r#""hook":"shopify""#), "{drop}");
}

/// A hook's refusals wait by a count of their own, by the sender's address
/// and the hook's path: a verified event clears no token's count, a
/// token's verification clears no hook's, and a hostile sender's forged
/// events -- signed under its own secret, or none -- raise no token's wait
/// from the address they share. Read from below, and from above only
/// where the count is the property, with seconds to spare: four forged
/// events, counted as tokens' failures, would make a bad token's next wait
/// 4.8 s where its own count makes it 0.3.
#[test]
fn a_hooks_refusals_wait_by_a_count_of_their_own() {
    // `--auth-delay` given again after the one `serve` gives, which wins.
    let (server, _) = serve("waits", &["--auth-delay", "300"]);
    let base = Duration::from_millis(300);
    let timed = |status: &dyn Fn() -> u16| {
        let t = std::time::Instant::now();
        let s = status();
        (s, t.elapsed())
    };
    let bad_token = || {
        server
            .http()
            .with_token("wrong")
            .ask("GET", "/_whoami", "")
            .status
    };
    let good_token = || {
        server
            .http()
            .with_token(ROOT)
            .ask("GET", "/_whoami", "")
            .status
    };
    let body = paid("evt_w", "pi_w", "o-1", 2000, "eur");
    let forged = || {
        send(
            &server,
            "stripe",
            &[("Stripe-Signature", "t=1,v1=00")],
            &body,
        )
        .status
    };
    let n = std::cell::Cell::new(0);
    let verified = || {
        n.set(n.get() + 1);
        let id = format!("evt_v{}", n.get());
        stripe(&server, "stripe", &paid(&id, "pi_v", "o-2", 500, "usd")).status
    };

    // A verified event clears no token's count: the second bad token waits
    // as a second failure.
    assert_eq!(bad_token(), 401);
    assert_eq!(verified(), 200);
    let (status, took) = timed(&bad_token);
    assert_eq!(status, 401);
    assert!(
        took >= 2 * base,
        "a verified event cleared a token's count: {took:?}"
    );

    // A token's verification clears no hook's count.
    let (status, took) = timed(&forged);
    assert_eq!(status, 401);
    assert!(took >= base, "{took:?}");
    assert_eq!(good_token(), 200);
    let (status, took) = timed(&forged);
    assert_eq!(status, 401);
    assert!(
        took >= 2 * base,
        "a token's verification cleared a hook's count: {took:?}"
    );

    // A verified event of the hook clears its count; and the forged events
    // raised no token's -- the good token above cleared the token's count,
    // so a bad token now waits as a first failure, where the four forged
    // events since, counted as tokens' failures, would have made it the
    // fifth, 4.8 s.
    assert_eq!(verified(), 200);
    for _ in 0..3 {
        assert_eq!(forged(), 401);
    }
    let (status, took) = timed(&bad_token);
    assert_eq!(status, 401);
    assert!(took >= base, "{took:?}");
    assert!(
        took < Duration::from_secs(3),
        "the forged events raised the token's wait: {took:?}"
    );
}

/// An event's insert answers a worker's claim held until a job comes
/// (`Fennec-Wait`): the one-request queue fed by another service's events.
/// And an event examining more rows than `--call-max-rows` is refused
/// whole, its sender told the bound.
#[test]
fn an_events_insert_answers_a_held_claim_and_its_bound_is_told() {
    let (server, _) = serve("held", &["--call-max-rows", "2"]);
    let mut http = server.http().with_token(ROOT);
    http.run("create collection jobs (kind text, owner text, run_at timestamp @sorted)");
    http.run(
        "create statement enqueue($kind text from event.kind) on hook standard as insert jobs \
         {kind: $kind, run_at: now()}",
    );
    http.run("create collection notes (n int)");
    for n in 0..3 {
        http.run(&format!("put notes {{n: {n}}}"));
    }
    http.run(
        "create statement touch_all($n int from event.n) on hook standard where event.kind = \
         'sweep' scans as set notes {n: $n} where n >= 0",
    );
    let port = server.port;
    let claim = std::thread::spawn(move || {
        let body = format!(
            "{{\"query\": {}}}",
            json_string(
                "set jobs {owner: 'w1', run_at: now() + 60000} where run_at <= now() order \
                 run_at limit 1 returning kind"
            )
        );
        Http::open(port).with_token(ROOT).ask_with(
            "POST",
            "/query",
            &body,
            &[("Fennec-Wait", "20000")],
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let m = server
            .http()
            .with_token(ROOT)
            .ask("GET", "/_metrics", "")
            .body;
        if m.lines().any(|l| l == "fennec_held_requests 1") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the claim was never held"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let event = |id: &str, body: &str| {
        let t = now_s().to_string();
        let mac = hmac_sha256(STANDARD_KEY, format!("{id}.{t}.{body}").as_bytes());
        let sig = format!("v1,{}", b64_encode(&mac));
        let headers = [
            ("webhook-id", id),
            ("webhook-timestamp", t.as_str()),
            ("webhook-signature", sig.as_str()),
        ];
        send(&server, "standard", &headers, body)
    };
    let a = event("msg_job", r#"{"kind":"mail"}"#);
    assert_eq!(a.status, 200, "{}", a.body);
    let claimed = claim.join().unwrap();
    assert_eq!(claimed.status, 200, "{}", claimed.body);
    assert!(
        claimed.body.contains("\"kind\":\"mail\""),
        "{}",
        claimed.body
    );

    let a = event("msg_sweep", r#"{"kind":"sweep","n":9}"#);
    assert_eq!(a.status, 400, "{}", a.body);
    assert!(a.body.contains("examines at most 2 rows"), "{}", a.body);
    assert!(!a.body.contains("touch_all"), "{}", a.body);
    assert_eq!(count(&http.run("get notes count where n = 9")), 0);
}

/// The statements `site/content/docs/webhooks.html` shows, as it writes
/// them (its `clerk` hook this file's `standard`), each taking the event
/// its section sends: the page's Stripe payment, with its guard idempotent
/// by the intent, a second event for the same payment taken as a write of
/// what is there; its refund, `created` read as seconds into an instant;
/// its GitHub push and the `ping` no condition takes; its Standard
/// Webhooks user, read from a list by its place.
#[test]
fn the_reference_pages_statements_take_their_events() {
    let (server, _) = serve("page", &[]);
    let mut http = server.http().with_token(ROOT);
    for q in [
        "drop statement stripe_paid",
        "alter collection orders add field amount int",
        "create statement stripe_paid(
             $intent text from event.data.object.id,
             $order  text from event.data.object.metadata.order,
             $amount int  from event.data.object.amount_received,
             $paid   timestamp from hook.at)
           on hook stripe
           where event.type = 'payment_intent.succeeded' and event.livemode = true
         as set orders {status: 'paid', intent: $intent, amount: $amount, paid_at: $paid}
            where ref = $order and (status = 'open' or intent = $intent)
            require 1 else 'no open order for this payment yet'",
        "create collection refunds (charge text @unique, at timestamp)",
        "create statement stripe_refunded(
             $charge  text from event.data.object.id,
             $created int  from event.created)
           on hook stripe
           where event.type = 'charge.refunded'
         as put refunds {charge: $charge, at: $created * 1000} if absent",
        "create collection pushes (repo text, branch text, sha text, kind text)",
        "create statement pushed(
             $repo   text from event.repository.full_name,
             $branch text from event.ref,
             $sha    text from event.after,
             $kind   text from hook.event)
           on hook github
           where hook.event = 'push'
         as insert pushes {repo: $repo, branch: $branch, sha: $sha, kind: $kind}",
        "create collection users (clerk_id text @unique, email text)",
        "create statement user_created(
             $id    text from event.data.id,
             $email text from event.data.email_addresses.\"0\".email_address)
           on hook standard
           where event.type = 'user.created'
         as put users {clerk_id: $id, email: $email} if absent",
    ] {
        http.run(q);
    }

    // A payment before its order's row is told `not yet`; the second event
    // Stripe may send for one payment is taken as a write of what is there.
    let early = stripe(&server, "stripe", &paid("evt_e", "pi_e", "o-9", 900, "eur"));
    assert_eq!(early.status, 412, "{}", early.body);
    assert!(
        early.body.contains("no open order for this payment yet"),
        "{}",
        early.body
    );
    for id in ["evt_1", "evt_2"] {
        let a = stripe(&server, "stripe", &paid(id, "pi_1", "o-1", 2500, "eur"));
        assert_eq!(a.status, 200, "{}", a.body);
        assert_eq!(a.body, format!(r#"{{"event":"{id}","taken":1}}"#));
    }
    let row = http.run("get orders select status, amount where ref = 'o-1'");
    assert_eq!(column(&row, "status"), ["paid"]);
    assert_eq!(column(&row, "amount"), ["2500"]);

    let refund = r#"{"id":"evt_r","type":"charge.refunded","livemode":true,"created":1711627200,"data":{"object":{"id":"ch_1"}}}"#;
    assert_eq!(stripe(&server, "stripe", refund).status, 200);
    let at = http.run("get refunds select at where charge = 'ch_1'");
    assert!(at.contains("2024-03-28T12:00:00"), "{at}");

    let push = |event: &str, body: &str| {
        let sig = format!(
            "sha256={}",
            hex(&hmac_sha256(GITHUB_SECRET.as_bytes(), body.as_bytes()))
        );
        send(
            &server,
            "github",
            &[("X-Hub-Signature-256", &sig), ("X-GitHub-Event", event)],
            body,
        )
    };
    let ping = push(
        "ping",
        r#"{"zen":"Keep it logically awesome.","hook_id":1}"#,
    );
    assert_eq!((ping.status, ping.body.as_str()), (200, r#"{"taken":0}"#));
    let pushed = push(
        "push",
        r#"{"ref":"refs/heads/main","after":"9f2c","repository":{"full_name":"acme/shop"}}"#,
    );
    assert_eq!(pushed.status, 200, "{}", pushed.body);
    let row = http.run("get pushes select repo, kind");
    assert_eq!(column(&row, "repo"), ["acme/shop"]);
    assert_eq!(column(&row, "kind"), ["push"]);

    let body = r#"{"type":"user.created","data":{"id":"user_1","email_addresses":[{"email_address":"a@example.com"},{"email_address":"b@example.com"}]}}"#;
    let t = now_s().to_string();
    let mac = hmac_sha256(STANDARD_KEY, format!("msg_1.{t}.{body}").as_bytes());
    let sig = format!("v1,{}", b64_encode(&mac));
    let a = send(
        &server,
        "standard",
        &[
            ("webhook-id", "msg_1"),
            ("webhook-timestamp", t.as_str()),
            ("webhook-signature", sig.as_str()),
        ],
        body,
    );
    assert_eq!(a.status, 200, "{}", a.body);
    let row = http.run("get users select email");
    assert_eq!(column(&row, "email"), ["a@example.com"]);
}
