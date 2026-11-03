//! Scheduled statements' syntax (`docs/design/named-statements.md`, *F3 in
//! slices*, slice 1): `create statement <name> (every <interval> | at
//! '<cron>') [in '<zone>'] as <body>` read into its tree -- `bucket`'s
//! interval, or cron's five fields as masks -- `slot()` read in a scheduled
//! body alone, the refusals the text alone answers, the schedule in the
//! canonical tree -- and every way of running one refusing it until a
//! server runs them, so no schedule is defined that no one runs.

use fennec_core::named::{
    Cron, Definition, Schedule, When, SCHEDULED_AND_BOUND, SCHEDULED_UNRUN, SCHEDULED_VALUES,
};
use fennec_core::prelude::*;
use fennec_core::query::Interval;
use fennec_ql::{parse, parse_one};

/// The design's reaper: a schedule's every part but the zone.
const LAPSE: &str = "create statement lapse_holds
  every 1m
  comment 'Lapses each hold whose time is up, a thousand a minute'
as set holds {state: 'lapsed', lapsed_at: now()}
   where state = 'held' and until < now() order until limit 1000";

/// The design's monthly reset: a cron expression on a zone's clock, and
/// `slot()`.
const MONTHLY: &str = "create statement monthly_reset
  at '0 0 1 * *' in 'Europe/Berlin'
  scans
as set plans {used: 0, period: slot()} where used > 0";

const DAY: i64 = 86_400_000;

fn definition(text: &str) -> Definition {
    match parse_one(text) {
        Ok(Statement::CreateStatement(d)) => *d,
        other => panic!("{text}: {other:?}"),
    }
}

fn schedule(text: &str) -> Schedule {
    *definition(text).schedule.expect("a schedule")
}

/// `every <interval>` before a body that writes.
fn every(interval: &str) -> When {
    schedule(&format!(
        "create statement s every {interval} as del t where a = 1"
    ))
    .when
}

fn cron(text: &str) -> Cron {
    Cron::read(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// `text` refused in words holding `words`, placed at the first `at` in it,
/// in characters as the parser counts: the word refused is not consumed.
fn refused_at(text: &str, words: &str, at: &str) {
    let e = match parse(text) {
        Err(e) => e.to_string(),
        Ok(s) => panic!("{text} parsed: {s:?}"),
    };
    assert!(e.contains(words), "{text}: {e}");
    let place = text[..text.find(at).expect("the word is in the text")]
        .chars()
        .count();
    assert!(
        e.contains(&format!("position {place}:")),
        "{text}: {e}, not at {place}"
    );
}

/// A schedule refused in words holding `words`, at the first `at` of
/// `create statement s <clause> as del t where a = 1`.
fn clause_refused(clause: &str, words: &str, at: &str) {
    refused_at(
        &format!("create statement s {clause} as del t where a = 1"),
        words,
        at,
    );
}

fn slot() -> Expr {
    Expr::Call("slot".into(), Vec::new())
}

#[test]
fn a_schedule_reads_into_its_tree() {
    let d = definition(LAPSE);
    assert_eq!(d.name, "lapse_holds");
    assert_eq!(
        d.schedule.as_deref(),
        Some(&Schedule {
            when: When::Every(Interval::Ms(60_000, 0)),
            zone: None,
        })
    );
    assert_eq!(
        d.comment.as_deref(),
        Some("Lapses each hold whose time is up, a thousand a minute")
    );
    assert!(!d.scans && !d.public && !d.replace && !d.block && d.after.is_none());
    assert!(d.params.is_empty() && d.claims.is_empty() && d.check.is_none());
    assert_eq!(d.body.len(), 1);
    assert_eq!(d.text, LAPSE);
    // A text read back is its tree, as an open reads a stored one.
    assert_eq!(fennec_ql::definition(&d.text).unwrap(), d);

    let d = definition(MONTHLY);
    assert_eq!(
        d.schedule.as_deref(),
        Some(&Schedule {
            when: When::At(Cron {
                minutes: 1,
                hours: 1,
                days: 1,
                months: 0xfff,
                weekdays: 0x7f,
            }),
            zone: Some("Europe/Berlin".into()),
        })
    );
    assert!(d.scans);
    // `slot()` is the call it is on main, read in the body as written.
    assert!(format!("{:?}", d.body[0]).contains(&format!("{:?}", slot())));

    // Every unit `bucket` takes from a minute up, from the epoch or a
    // week's Monday, and calendar months.
    assert_eq!(every("1m"), When::Every(Interval::Ms(60_000, 0)));
    assert_eq!(every("15m"), When::Every(Interval::Ms(900_000, 0)));
    assert_eq!(every("1h"), When::Every(Interval::Ms(3_600_000, 0)));
    assert_eq!(every("2H"), When::Every(Interval::Ms(7_200_000, 0)));
    assert_eq!(every("1 d"), When::Every(Interval::Ms(DAY, 0)));
    assert_eq!(every("1w"), When::Every(Interval::Ms(7 * DAY, 4 * DAY)));
    assert_eq!(every("1mo"), When::Every(Interval::Months(1)));
    assert_eq!(every("1y"), When::Every(Interval::Months(12)));
    // Whole minutes in another unit are minutes.
    assert_eq!(every("120s"), every("2m"));

    // A zone by its name in any case, held as the table spells it, or an
    // offset, held as `+hh:mm`, UTC's none at all; `create or replace`; a
    // body of several.
    for (zone, read) in [
        ("Europe/Berlin", Some("Europe/Berlin")),
        ("europe/berlin", Some("Europe/Berlin")),
        ("EUROPE/BERLIN", Some("Europe/Berlin")),
        ("+03:00", Some("+03:00")),
        ("+0300", Some("+03:00")),
        ("+03", Some("+03:00")),
        ("-0530", Some("-05:30")),
        ("UTC", None),
        ("utc", None),
        ("Z", None),
        ("+00:00", None),
        ("-00", None),
    ] {
        let s = schedule(&format!(
            "create statement s every 1d in '{zone}' as del t where a = 1"
        ));
        assert_eq!(s.zone.as_deref(), read, "{zone}");
    }
    let d = definition(
        "create or replace statement purge at '30 3 * * mon-fri' in 'Europe/Istanbul' comment \
         'x' scans as begin get t select id where a = 1 limit 1 require 1 else 'none'; del t \
         where b < now(); end",
    );
    assert!(d.replace && d.block && d.scans && d.body.len() == 2);
    assert_eq!(fennec_ql::definition(&d.text).unwrap(), d);
}

#[test]
fn cron_reads_every_form() {
    let all = cron("* * * * *");
    assert_eq!(
        all,
        Cron {
            minutes: (1 << 60) - 1,
            hours: (1 << 24) - 1,
            days: (1 << 31) - 1,
            months: 0xfff,
            weekdays: 0x7f,
        }
    );
    assert_eq!(
        cron("*/15 * * * *").minutes,
        1 | 1 << 15 | 1 << 30 | 1 << 45
    );
    assert_eq!(cron("*/15 * * * *"), cron("0,15,30,45 * * * *"));
    assert_eq!(
        cron("5-59/15 * * * *").minutes,
        1 << 5 | 1 << 20 | 1 << 35 | 1 << 50
    );
    let work = cron("0,30 9-17 * * mon-fri");
    assert_eq!(work.minutes, 1 | 1 << 30);
    assert_eq!(work.hours, 0b11_1111_1110_0000_0000);
    assert_eq!(work.weekdays, 0b011_1110);
    assert_eq!(work, cron("0,30 9-17 * * 1-5"));
    // Sunday is 0 and 7, by name in any case.
    for sunday in ["0", "7", "sun", "SUN", "Sun"] {
        assert_eq!(cron(&format!("0 0 * * {sunday}")).weekdays, 1, "{sunday}");
    }
    assert_eq!(cron("0 0 * * 5-7").weekdays, 1 | 1 << 5 | 1 << 6);
    assert_eq!(cron("0 0 * * 0-7").weekdays, 0x7f);
    assert_eq!(cron("0 0 1 jan,JUL *").months, 1 | 1 << 6);
    assert_eq!(cron("0 0 1 jan,JUL *"), cron("0 0 1 1,7 *"));
    // 29 February runs, once in four years; the last day a month holds.
    assert_eq!(cron("0 0 29 2 *").days, 1 << 28);
    assert_eq!(cron("0 0 31 1-12 *").days, 1 << 30);
    // A day of the month restricted beside every day of the week, and the
    // other way.
    assert_eq!(cron("0 12 */2 * *").weekdays, 0x7f);
    assert_eq!(cron("0 12 * * */2").weekdays, 1 | 1 << 2 | 1 << 4 | 1 << 6);
    // Mondays, as every cron reads `* * mon`: the day of the month open.
    let mondays = cron("0 0 * * mon");
    assert_eq!((mondays.days, mondays.weekdays), ((1 << 31) - 1, 1 << 1));
    // White space between fields is any.
    assert_eq!(cron("  0\t3  * *   * "), cron("0 3 * * *"));
}

#[test]
fn a_schedule_s_refusals() {
    // The floor, a minute, and a whole number of them.
    let floor = "at most once a minute";
    clause_refused("every 30s", floor, "30s");
    clause_refused("every 90s", floor, "90s");
    clause_refused("every 59999ms", floor, "59999ms");
    let units = "a whole number of minutes, hours, days";
    clause_refused("every 0m", units, "0m");
    clause_refused("every 1x", units, "1x");
    clause_refused("every 1 hour", units, "1 hour");
    let interval = "`every` takes an interval";
    clause_refused("every 1.5h", interval, "1.5h");
    clause_refused("every '1h'", interval, "'1h'");
    refused_at(
        "create statement s every 15 as del t where a = 1",
        interval,
        "15",
    );
    // At the text's end: what the shell reads as "go on".
    let at_end = |text: &str, words: &str| {
        let e = parse(text).unwrap_err().to_string();
        let end = format!("position {}:", text.chars().count());
        assert!(e.contains(words) && e.contains(&end), "{text}: {e}");
    };
    at_end("create statement s every", interval);
    at_end("create statement s at", "`at` takes a cron expression");
    at_end("create statement s every 1d in", "`in` names the clock");

    // Cron's fields, each refusal naming the form to write.
    let cron_refused = |cron: &str, words: &str| {
        clause_refused(&format!("at '{cron}'"), words, &format!("'{cron}'"));
    };
    cron_refused("0 * * * * *", "six fields, Quartz's with seconds first");
    cron_refused("0 * * * * *", "`every 1m`");
    cron_refused("* * * *", "five fields");
    cron_refused("", "five fields");
    cron_refused("60 * * * *", "`60` is no minute: a minute is 0 to 59");
    cron_refused("0 24 * * *", "no hour");
    cron_refused("0 0 0 * *", "no day of the month");
    cron_refused("0 0 32 * *", "no day of the month");
    cron_refused("0 0 * 13 *", "no month");
    cron_refused("0 0 * jam *", "`jan` to `dec`");
    cron_refused("0 0 * * 8", "no day of the week");
    cron_refused("0 0 * * -1", "no day of the week");
    cron_refused("0 0 * * monday", "`sun` to `sat`");
    cron_refused("0,,5 * * * *", "no minute");
    cron_refused("*/0 * * * *", "a step of the minute field is 1 to 59");
    cron_refused("*/60 * * * *", "a step of the minute field is 1 to 59");
    cron_refused(
        "0 0 */40 * *",
        "a step of the day of the month field is 1 to 31",
    );
    cron_refused("30-10 * * * *", "runs backwards");
    cron_refused("0 0 * * fri-sun", "Sunday 7 at its end");
    cron_refused("5/15 * * * *", "a step follows `*` or a range");
    cron_refused("0 0 ? * MON", "Quartz's `?`");
    cron_refused("0 0 L * *", "Quartz's `L` and `W`");
    cron_refused("0 0 15W * *", "Quartz's `L` and `W`");
    cron_refused("0 0 LW * *", "Quartz's `L` and `W`");
    cron_refused("0 0 * * 5#3", "Quartz's `#`");
    cron_refused("0 0 $ * *", "pg_cron's `$`");
    cron_refused("@hourly", "`every 1h`");
    cron_refused("@daily", "`every 1d`");
    cron_refused("@midnight", "`every 1d`");
    cron_refused("@weekly", "`at '0 0 * * 0'`");
    cron_refused("@monthly", "`every 1mo`");
    cron_refused("@yearly", "`every 1y`");
    cron_refused("@reboot", "no time");
    cron_refused("@every", "five fields");
    // Friday the 13th: cron runs every Friday and every 13th.
    cron_refused(
        "0 0 13 * 5",
        "two statements, `at '0 0 13 * *'` and `at '0 0 * * 5'`, say either",
    );
    cron_refused("0 0 */2 * mon", "restricts both");
    // By the text: a field written other than `*` is restricted to cron
    // though it names every value, so these run every day there -- on
    // either day -- and would run on fewer here, ANDed.
    cron_refused(
        "0 0 1-31 * mon",
        "restricts both the day of the month (`1-31`) and the day of the week (`mon`) -- a \
         field is open as `*` alone",
    );
    cron_refused(
        "0 0 13 * 0-6",
        "two statements, `at '0 0 13 * *'` and `at '0 0 * * 0-6'`, say either",
    );
    cron_refused("0 0 */1 * mon", "restricts both");
    cron_refused("0 0 13 * */1", "restricts both");
    cron_refused("0 0 1-31 * 0-7", "restricts both");
    // Never: no month named holds a day named.
    cron_refused("0 0 30 2 *", "never runs");
    cron_refused("0 0 31 apr,jun,sep,nov *", "never runs");
    clause_refused("at 0", "`at` takes a cron expression in quotes", "0");

    // The zone, held to the binary's table.
    clause_refused(
        "every 1d in 'Mars/Olympus'",
        "no time zone this build holds",
        "'Mars/Olympus'",
    );
    clause_refused("every 1d in Berlin", "`in` names the clock", "Berlin");
    clause_refused(
        "every 1d comment 'x' in 'UTC'",
        "follows the schedule",
        "in 'UTC'",
    );

    // What a statement no one calls has no use for, at the word.
    refused_at(
        "create statement s() every 1h as del t where a = 1",
        SCHEDULED_VALUES,
        "()",
    );
    refused_at(
        "create statement s($a int) at '0 * * * *' as del t where a = $a",
        SCHEDULED_VALUES,
        "($a",
    );
    for clause in ["claims (sub text)", "check now() > 0", "public", "()"] {
        let first = clause.split(' ').next().unwrap();
        clause_refused(&format!("every 1h {clause}"), SCHEDULED_VALUES, first);
    }
    let body = |b: &str| format!("create statement s every 1h as {b}");
    for (b, at) in [
        ("del t where a = $1", "$1"),
        ("del t where a = $a", "$a"),
        ("del t where owner = $jwt.sub", "$jwt.sub"),
    ] {
        refused_at(&body(b), SCHEDULED_VALUES, at);
    }
    for (b, at) in [
        ("del t where a = new.a", "new.a"),
        ("del t where old.a < now()", "old.a"),
        ("set t {a: NEW.b} where a = 1", "NEW.b"),
        ("set t {new.a: 1} where a = 1", "new.a"),
        ("insert t {Old.a: 1}", "Old.a"),
    ] {
        refused_at(&body(b), "runs on no write, so no row is new or old", at);
    }
    // Its rows go nowhere: a `get` is a guard, and a body of guards alone
    // changes nothing.
    refused_at(
        &body("begin get t select id; del t where a = 1; end"),
        "its `get` is a guard",
        "get t",
    );
    refused_at(
        &body("del t where a = 1 returning id"),
        "say nothing with `returning`",
        "del t",
    );
    refused_at(
        &body("get t select id where a = 1 limit 1 require 1"),
        "guards alone",
        "s every",
    );
    // F1's: a body writes no `put`, upsert, schema, call or `_` collection.
    for b in [
        "put t {a: 1}",
        "put t {a: 1} if absent else set {a: 2}",
        "create collection u (a int)",
        "call other",
        "del _idempotency where a = 1",
    ] {
        assert!(parse(&body(b)).is_err(), "{b}");
    }

    // A schedule and a binding are two statements, either order; and one
    // schedule a statement.
    refused_at(
        "create statement s every 1h after set t as del u where a = 1",
        SCHEDULED_AND_BOUND,
        "after",
    );
    refused_at(
        "create statement s after set t every 1h as del u where a = 1",
        SCHEDULED_AND_BOUND,
        "every",
    );
    refused_at(
        "create statement s after insert t where new.a = 1 at '0 0 * * *' as del u where a = 1",
        SCHEDULED_AND_BOUND,
        "at '0",
    );
    clause_refused("every 1h every 1d", "one schedule", "every 1d");
    clause_refused("every 1h at '0 0 * * *'", "one schedule", "at '");
    at_end(
        "create statement s every 1h in 'UTC' scans",
        "a scheduled statement's body follows its schedule and `as`",
    );
}

/// `slot()` is read in a scheduled body alone; out of a definition it is
/// what it was, a call the engine finds no function for.
#[test]
fn slot_is_a_scheduled_body_s_alone() {
    let refused = "read in its body alone";
    for (text, at) in [
        (
            "create statement s() public as set t {a: slot()} where id = 1",
            "slot()",
        ),
        (
            "create statement s($a int) check slot() > $a as get t select id where id = $a",
            "slot()",
        ),
        (
            "create statement b after insert t as insert u {at: SLOT()}",
            "SLOT()",
        ),
        (
            "create statement b after set t where new.at < slot() as insert u {a: 1}",
            "slot()",
        ),
    ] {
        refused_at(text, refused, at);
    }
    refused_at(
        "create statement s every 1h as set t {a: slot(1)} where a = 1",
        "`slot()` takes no argument",
        "1)",
    );
    let Ok(Statement::Select(sel)) = parse_one("get t select slot() as at") else {
        panic!("a read")
    };
    assert_eq!(sel.computed[0].expr, slot());
    // In a scheduled body, wherever a value stands.
    let d = definition(
        "create statement s at '0 0 1 * *' as begin insert log {at: slot()}; del t where at < \
         slot(); end",
    );
    assert_eq!(d.body.len(), 2);
}

/// The schedule is tag 10 of the canonical tree, written only where a
/// definition holds one, so no stored digest moves; every spelling of a
/// schedule's meaning is one digest, and every change of it another.
#[test]
fn a_schedule_is_in_the_canonical_tree() {
    let digest = |t: &str| definition(t).digest().unwrap();
    let base = digest(LAPSE);
    let spelled = "CREATE OR REPLACE STATEMENT lapse_holds EVERY 1M -- each minute
  COMMENT \"Lapses each hold whose time is up, a thousand a minute\"
AS BEGIN UPDATE holds SET state = 'lapsed', lapsed_at = now()
   WHERE (state = 'held') AND until < now() ORDER BY until LIMIT 1000; END";
    assert_eq!(digest(spelled), base);
    assert_eq!(digest(&LAPSE.replace("1m", "60s")), base);
    let at = |cron: &str| {
        digest(&format!(
            "create statement s at '{cron}' as del t where a = 1"
        ))
    };
    assert_eq!(at("*/15 * * * *"), at("0,15,30,45 * * * *"));
    assert_eq!(at("0 0 * * 7"), at("0 0 * * sun"));
    assert_eq!(at("0 0 * * 0-6"), at("0 0 * * *"));
    let every = |iv: &str| {
        digest(&format!(
            "create statement s every {iv} as del t where a = 1"
        ))
    };
    assert_eq!(every("60m"), every("1h"));
    assert_eq!(every("12mo"), every("1y"));
    assert_eq!(every("24h"), every("1d"));

    let mut seen = vec![base];
    for (from, to) in [
        ("lapse_holds", "lapse_hold"),
        ("every 1m", "every 2m"),
        ("every 1m", "every 1mo"),
        ("every 1m", "every 1m in 'Europe/Berlin'"),
        ("every 1m", "every 1m in 'Europe/Istanbul'"),
        ("every 1m", "at '* * * * *'"),
        ("a thousand", "a hundred"),
        ("limit 1000", "limit 100"),
    ] {
        assert!(LAPSE.contains(from), "{from}");
        let d = digest(&LAPSE.replacen(from, to, 1));
        assert!(!seen.contains(&d), "{from} -> {to}");
        seen.push(d);
    }
    // A week from a Monday is not seven days from a Thursday.
    for (a, b) in [("1w", "7d"), ("1d", "1mo")] {
        assert_ne!(every(a), every(b), "{a} {b}");
    }
    for cron in [
        "0 0 1 * *",
        "1 0 1 * *",
        "0 1 1 * *",
        "0 0 2 * *",
        "0 0 1 2 *",
        "0 0 * * 1",
    ] {
        let d = at(cron);
        assert!(!seen.contains(&d), "{cron}");
        seen.push(d);
    }
    let zoned = |z: &str| {
        digest(&format!(
            "create statement s every 1d in '{z}' as del t where a = 1"
        ))
    };
    assert_ne!(zoned("Europe/Berlin"), every("1d"));
    assert_ne!(zoned("Europe/Berlin"), zoned("+01:00"));
    // The zone read, never its spelling: UTC however written is no zone,
    // a name in any case the table's, an offset `+hh:mm`.
    for utc in ["UTC", "utc", "Z", "+00:00", "-00:00", "+00", "+0000"] {
        assert_eq!(zoned(utc), every("1d"), "{utc}");
    }
    for (a, b) in [
        ("europe/berlin", "Europe/Berlin"),
        ("EUROPE/BERLIN", "Europe/Berlin"),
        ("asia/kolkata", "Asia/Kolkata"),
        ("+0300", "+03:00"),
        ("+03", "+03:00"),
        ("-0530", "-05:30"),
    ] {
        assert_eq!(zoned(a), zoned(b), "{a} {b}");
    }
    // A link is a name of its own, as the table holds it: which name one
    // leads to moves with tzdata, and a digest must not. A zone of the
    // table is not a fixed offset, before 2000 refused where the offset
    // reads.
    assert_ne!(zoned("Asia/Calcutta"), zoned("Asia/Kolkata"));
    assert_ne!(zoned("Etc/UTC"), every("1d"));

    // Written as tag 10 after the body, and nowhere for any other:
    // `Debug`, which `make fennecql-corpus` compares, says it alike.
    let bytes = definition("create statement s every 1h as del u where a = 1")
        .canonical()
        .unwrap();
    // Tag 10, a node of kind 6: a step of 3 600 000 ms (tag 1, a varint),
    // no origin, no zone; the node's end and the definition's.
    let tail = [10, 6, 1, 0x80, 0xdd, 0xdb, 0x01, 0, 0];
    assert!(bytes.ends_with(&tail), "{bytes:?}");
    let cron_in = |zone: &str| {
        definition(&format!(
            "create statement s at '0 0 * * 1'{zone} as del u where a = 1"
        ))
        .canonical()
        .unwrap()
    };
    // Cron's masks under tags 4 to 8; `in 'UTC'` writes no zone, as none
    // does.
    let masks = [
        10, 6, 4, 1, 5, 1, 6, 0xff, 0xff, 0xff, 0xff, 0x07, 7, 0xff, 0x1f, 8, 2,
    ];
    let bytes = cron_in(" in 'UTC'");
    assert!(
        bytes.ends_with(&[&masks[..], &[0, 0]].concat()),
        "{bytes:?}"
    );
    assert_eq!(bytes, cron_in(""));
    // A zone under 9, as the table spells it.
    let bytes = cron_in(" in 'europe/berlin'");
    let tail = [&masks[..], &[9, 13], b"Europe/Berlin", &[0, 0]].concat();
    assert!(bytes.ends_with(&tail), "{bytes:?}");
    let bytes = cron_in(" in '+0300'");
    let tail = [&masks[..], &[9, 6], b"+03:00", &[0, 0]].concat();
    assert!(bytes.ends_with(&tail), "{bytes:?}");
    let callable = definition("create statement s($a int) public as del u where id = $a");
    assert!(!format!("{callable:?}").contains("schedule"));
    assert!(format!("{:?}", definition(LAPSE)).contains("schedule: Schedule"));
    // Pinned: a change to the encoding that moves this moves a stored
    // schedule's digest, and raises fennec_ql::GRAMMAR_EPOCH with it.
    assert_eq!(base, 0xb974_f801_58c8_1744, "{base:#018x}");
}

/// Where no scheduler runs the database's -- the shell, the native
/// library, a tenant node until F3's slice 4 -- every way of running a
/// statement meets the engine's definition, which refuses a schedule in
/// words of its own before it asks anything else -- alone, in a block
/// behind a write, which lands nothing, and as a replacement of a
/// statement held. Never a schedule no one runs.
#[test]
fn a_schedule_is_refused_where_statements_run() {
    fennec_ql::install();
    let mut db = Database::new();
    for s in parse(
        "create collection holds (state text, until timestamp); create collection plans (used \
         int); create statement s() as get holds select state",
    )
    .unwrap()
    {
        db.execute(&s).unwrap();
    }
    let lapse = parse_one(LAPSE).unwrap();
    let not_built = Error::Query(SCHEDULED_UNRUN.into());
    assert_eq!(db.execute(&lapse), Err(not_built.clone()));
    assert_eq!(db.execute_with(&lapse, &[]), Err(not_built.clone()));
    let put = parse_one("put plans {used: 3}").unwrap();
    assert_eq!(
        db.execute_block(&[(&put, &[]), (&lapse, &[])]),
        Err((1, not_built.clone()))
    );
    let replace = parse_one(
        "create or replace statement s at '0 0 1 * *' in 'Europe/Berlin' as set plans {used: 0} \
         where used > 0",
    )
    .unwrap();
    assert_eq!(db.execute(&replace), Err(not_built));
    let plans = db
        .execute(&parse_one("get plans select id").unwrap())
        .unwrap();
    assert_eq!(
        plans.rows().unwrap().rows.len(),
        0,
        "the block put back whole"
    );
    let listed = db.execute(&parse_one("statements").unwrap()).unwrap();
    assert_eq!(listed.rows().unwrap().rows.len(), 1, "`s` alone, as it was");
}
