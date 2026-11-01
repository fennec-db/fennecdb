//! `in $n`: one parameter holding the list, answered as the same list
//! written out with a parameter a value (`in [$1, $2, ...]`) -- the same
//! rows, through the same index, every number as it was written.

use fennec_core::json;
use fennec_core::prelude::*;

fn exec(db: &mut Database, sql: &str) {
    db.execute(&fennec_ql::parse_one(sql).expect(sql))
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// `sql` run with `params` as `/query` runs it: read the quick way, and
/// again as written where `Database::exactly` names them.
fn run(db: &mut Database, sql: &str, params: &[Value]) -> Result<Response> {
    let stmt = fennec_ql::parse_one(sql)?;
    match stmt.is_read_only() {
        true => db.query(&stmt, params),
        false => db.execute_with(&stmt, params),
    }
}

fn rows(db: &mut Database, sql: &str, params: &[Value]) -> Vec<(u64, Vec<Value>)> {
    match run(db, sql, params).unwrap_or_else(|e| panic!("{sql}: {e}")) {
        Response::Rows(rs) => rs.rows.into_iter().map(|r| (r.id, r.values)).collect(),
        other => panic!("{sql}: {other:?}"),
    }
}

fn plan(db: &mut Database, sql: &str, params: &[Value]) -> Vec<Value> {
    rows(db, &format!("explain {sql}"), params)
        .into_iter()
        .flat_map(|(_, v)| v)
        .collect()
}

/// `sql`, whose `$L` stands for the list after `fixed`'s parameters, both
/// ways: `in $<k>` handed the list, and `in [$<k>, $<k+1>, ...]` handed a
/// parameter a value.
fn both(sql: &str, fixed: &[Value], list: &[Value]) -> [(String, Vec<Value>); 2] {
    let k = fixed.len() + 1;
    let mut one = fixed.to_vec();
    one.push(Value::List(list.to_vec()));
    let items: Vec<String> = (0..list.len()).map(|i| format!("${}", k + i)).collect();
    let mut many = fixed.to_vec();
    many.extend(list.iter().cloned());
    [
        (sql.replace("$L", &format!("${k}")), one),
        (sql.replace("$L", &format!("[{}]", items.join(", "))), many),
    ]
}

fn products() -> Database {
    let mut db = Database::new();
    exec(
        &mut db,
        "create collection p (code text @unique, tag text @hash, n int @sorted, x float, \
         name text nocase @hash, note text, g int, @sorted(g, n))",
    );
    for i in 1..=300i64 {
        let note = match i % 7 {
            0 => "null".to_string(),
            k => format!("\"n{k}\""),
        };
        exec(
            &mut db,
            &format!(
                "put p {{id: {i}, code: \"c{i}\", tag: \"t{}\", n: {}, x: {}, name: \"{}\", \
                 note: {note}, g: {}}}",
                i % 10,
                i % 50,
                i as f64 / 10.0,
                if i % 2 == 0 {
                    format!("Ab{}", i % 5)
                } else {
                    format!("aB{}", i % 5)
                },
                i % 30,
            ),
        );
    }
    db
}

fn ints(v: &[i64]) -> Vec<Value> {
    v.iter().map(|&i| Value::Int(i)).collect()
}

fn texts(v: &[&str]) -> Vec<Value> {
    v.iter().map(|s| Value::Text(s.to_string())).collect()
}

#[test]
fn a_parameter_list_is_answered_as_the_list_written_out() {
    let mut db = products();
    let thousand: Vec<i64> = (0..1000).map(|i| i * 7 % 400).collect();
    let cases: Vec<(&str, Vec<Value>, Vec<Value>)> = vec![
        ("get p where id in $L", vec![], ints(&[3, 1, 999, 3])),
        ("get p where id in $L", vec![], ints(&thousand)),
        ("get p where id in $L", vec![], vec![]),
        (
            "get p where code in $L",
            vec![],
            texts(&["c5", "c77", "nope"]),
        ),
        ("get p where tag in $L", vec![], texts(&["t1", "t3"])),
        ("get p where n in $L", vec![], ints(&[4, 9, 49, 50])),
        (
            "get p where n in $L order n desc limit 7",
            vec![],
            ints(&[4, 9]),
        ),
        (
            "get p where g in $L and n >= $1",
            vec![Value::Int(10)],
            ints(&[4, 9, 31]),
        ),
        (
            "get p where x in $L",
            vec![],
            vec![Value::Float(0.1), Value::Float(2.5)],
        ),
        ("get p where name in $L", vec![], texts(&["ab1", "AB3"])),
        (
            "get p where note in $L",
            vec![],
            vec![Value::Null, Value::Text("n3".into())],
        ),
        (
            "get p where id not in $L limit 20",
            vec![],
            ints(&[1, 2, 3]),
        ),
        (
            "get p where n > $1 and tag in $L",
            vec![Value::Int(40)],
            texts(&["t2"]),
        ),
        (
            "get p where tag = $1 or id in $L",
            vec![Value::Text("t9".into())],
            ints(&[4]),
        ),
        ("select id, n from p where n in $L", vec![], ints(&[7])),
        (
            "get p select id, id in $L as hit order id limit 6",
            vec![],
            ints(&[2, 5]),
        ),
        (
            "get p where tag in $L select tag, count(*) as c group tag",
            vec![],
            texts(&["t1", "t4"]),
        ),
    ];
    for (sql, fixed, list) in cases {
        let [(param, one), (written, many)] = both(sql, &fixed, &list);
        let a = rows(&mut db, &param, &one);
        let b = rows(&mut db, &written, &many);
        assert_eq!(a, b, "{param} against {written}");
        if !sql.contains(" select ") && !sql.starts_with("select") {
            assert_eq!(
                plan(&mut db, &param, &one),
                plan(&mut db, &written, &many),
                "the plan of {param}"
            );
        }
    }
    // An index takes the list: the unique, the hash, the id index and the
    // ordered one over several fields, on its first part.
    for (sql, list, index) in [
        (
            "get p where code in $1",
            texts(&["c5", "c6"]),
            "the hash index",
        ),
        ("get p where tag in $1", texts(&["t5"]), "the hash index"),
        ("get p where id in $1", ints(&[5, 6]), "the id index"),
        ("get p where g in $1", ints(&[5, 6]), "the index on (g, n)"),
    ] {
        let steps = format!("{:?}", plan(&mut db, sql, &[Value::List(list)]));
        assert!(steps.contains(index), "{sql}: {steps}");
    }
}

#[test]
fn sql_spells_it_any_and_all() {
    let mut db = products();
    let list = Value::List(ints(&[2, 4, 6]));
    let p = [list.clone()];
    assert_eq!(
        rows(&mut db, "select id from p where id = any($1)", &p),
        rows(&mut db, "get p select id where id in $1", &p)
    );
    for ne in ["<>", "!="] {
        assert_eq!(
            rows(
                &mut db,
                &format!("select id from p where id {ne} all($1) limit 9"),
                &p
            ),
            rows(&mut db, "get p select id where id not in $1 limit 9", &p)
        );
    }
}

#[test]
fn numbers_in_the_list_are_read_as_written() {
    let mut db = Database::new();
    exec(&mut db, "create collection p (n int, x float)");
    // 2^24 + 1 and 2^53 + 1, which an `f32` and an `f64` make their
    // neighbours, and 0.1, which an `f32` makes 0.10000000149.
    exec(&mut db, "put p {id: 16777216, n: 9007199254740992, x: 0.1}");
    exec(&mut db, "put p {id: 16777217, n: 9007199254740993, x: 0.2}");
    let stmt = fennec_ql::parse_one("get p where id in $1").unwrap();
    assert_eq!(
        db.exactly(&stmt).params,
        vec![0],
        "a list of ids is read as written"
    );
    for (sql, list, want) in [
        ("get p where id in $1", "[[16777217]]", 16777217),
        ("get p where n in $1", "[[9007199254740993]]", 16777217),
        ("get p where x in $1", "[[0.1, 7]]", 16777216),
        ("get p where id not in $1", "[[16777216]]", 16777217),
    ] {
        let stmt = fennec_ql::parse_one(sql).unwrap();
        let mut params = json::parse_params(list).unwrap();
        // Read the quick way, the list is a vector's f32s, refused rather
        // than answered with another row's id.
        let e = db.query(&stmt, &params).unwrap_err().to_string();
        assert!(
            e.contains("`in $n` list keeps a list of numbers as written"),
            "{sql}: {e}"
        );
        if !db.exactly(&stmt).params.is_empty() {
            params = json::parse_params_exact(list).unwrap();
        }
        let got = match db.query(&stmt, &params).unwrap() {
            Response::Rows(rs) => rs.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(got, vec![want], "{sql} {list}");
    }
}

#[test]
fn the_list_is_bounded_and_must_be_one() {
    let mut db = products();
    let at = |n: i64| Value::List((1..=n).map(Value::Int).collect());
    let whole = rows(&mut db, "get p where id in $1 select id", &[at(100_000)]);
    assert_eq!(whole.len(), 300);
    for sql in [
        "get p where id in $1",
        "del p where id in $1",
        "get p select id in $1 as a",
    ] {
        let e = run(&mut db, sql, &[at(100_001)]).unwrap_err().to_string();
        assert!(
            e.contains("`in $1` holds 100001 values, more than 100000"),
            "{sql}: {e}"
        );
    }
    for (v, kind) in [
        (Value::Int(5), "int"),
        (Value::Text("a".into()), "text"),
        (Value::Null, "null"),
    ] {
        let e = run(&mut db, "get p where id in $1", &[v])
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(&format!("`in $1` takes a list, and $1 is of type {kind}")),
            "{e}"
        );
    }
    let e = run(&mut db, "get p where id in $2", &[at(1)])
        .unwrap_err()
        .to_string();
    assert!(e.contains("parameter $2 is not bound"), "{e}");
    // `in []` and `in ($1)` -- SQL's list of one value -- stay as they were.
    assert!(rows(&mut db, "get p where id in []", &[]).is_empty());
    assert_eq!(
        rows(
            &mut db,
            "get p where id in ($1) select id",
            &[Value::Int(4)]
        )
        .len(),
        1
    );
    assert!(rows(&mut db, "get p where id in [$1]", &[at(2)]).is_empty());
}

#[test]
fn writes_pick_their_rows_by_the_list() {
    let mut a = products();
    let mut b = products();
    let list = ints(&[2, 3, 5, 7, 11, 400]);
    for sql in [
        "set p {note: \"hit\"} where id in $L",
        "del p where tag in $L",
        "set p {n: n + 1} where n in $L order id limit 3",
    ] {
        let list = match sql.contains("tag") {
            true => texts(&["t2", "t3"]),
            false => list.clone(),
        };
        let [(param, one), (written, many)] = both(sql, &[], &list);
        let x = run(&mut a, &param, &one).unwrap();
        let y = run(&mut b, &written, &many).unwrap();
        assert_eq!(format!("{x:?}"), format!("{y:?}"), "{param}");
    }
    assert_eq!(rows(&mut a, "get p", &[]), rows(&mut b, "get p", &[]));
}

#[test]
fn the_parsed_tree_holds_no_value() {
    let db = products();
    let stmt = fennec_ql::parse_one("get p where id in $1 select id").unwrap();
    let before = format!("{stmt:?}");
    for list in [ints(&[1, 2]), ints(&[9]), ints(&[])] {
        let n = list.len();
        match db.query(&stmt, &[Value::List(list)]).unwrap() {
            Response::Rows(rs) => assert_eq!(rs.rows.len(), n),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        format!("{stmt:?}"),
        before,
        "the parameter's values went into the tree"
    );
}

#[test]
fn a_named_statement_takes_its_list_by_name() {
    fennec_ql::install();
    let mut db = Database::new();
    exec(&mut db, "create collection p (n int)");
    exec(&mut db, "put p {id: 16777216, n: 1}");
    exec(&mut db, "put p {id: 16777217, n: 2}");
    exec(
        &mut db,
        "create statement picked($ids [int]) public as get p select id where id in $ids",
    );
    let call = fennec_ql::parse_one("call picked($1)").unwrap();
    // Read the quick way, a list argument is a vector's `f32`s: the call
    // names it, and it is read as written.
    assert_eq!(db.exactly(&call).params, vec![0]);
    let params = json::parse_params_exact("[[16777217]]").unwrap();
    let got = match db.execute_with(&call, &params).unwrap() {
        Response::Rows(rs) => rs.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(got, vec![16777217]);
}

/// A list of none, or past the bound, is refused before the plan, whatever
/// the rows: over an empty collection, under a filter the id index finds
/// nothing for, in a clause no row reaches, a `/batch`'s line and a
/// definition's body alike. Left to the row a filter tests, each was
/// answered over no rows and refused once one was tested.
#[test]
fn the_refusal_never_hangs_on_the_rows() {
    fennec_ql::install();
    let mut db = Database::new();
    exec(&mut db, "create collection e (n int @hash, tag text)");
    exec(&mut db, "create collection f (n int, tag text)");
    exec(&mut db, "put f {id: 1, n: 1, tag: \"a\"}");
    let long = Value::List((0..=100_000).map(Value::Int).collect());
    let refused = |e: Error| {
        let e = e.to_string();
        assert!(
            e.contains("`in $1` takes a list") || e.contains("more than 100000"),
            "{e}"
        );
    };
    for sql in [
        "get e where n in $1",
        "get e where tag = 'x' or n not in $1",
        "get f where id = 7 and n in $1",
        "get f where id in [7, 8] and n in $1",
        "get e select n in $1 as hit",
        "get e order n in $1",
        "get f where n in (get e select n where n in $1)",
        "get f where id = 7 lookup e on n = n where n in $1",
        "get e facet tag where n in $1",
        "explain get e where n in $1",
        "set e {tag: 'x'} where n in $1",
        "del e where n in $1",
        "del f where id = 7 and n in $1",
        "set f {tag: 'y'} where id > 5 and n in $1 order id limit 1",
        "put f {id: 1, n: 1} if absent else set {tag: n in $1}",
    ] {
        for bad in [Value::Int(7), long.clone()] {
            match run(&mut db, sql, &[bad]) {
                Err(e) => refused(e),
                Ok(r) => panic!("{sql}: answered {r:?}"),
            }
        }
    }
    // A `/batch` of lines, each with its own parameters.
    let get = fennec_ql::parse_one("get e where n in $1").unwrap();
    let del = fennec_ql::parse_one("del e where n in $1").unwrap();
    let (good, bad) = ([Value::List(vec![])], [Value::Int(3)]);
    let (at, e) = db
        .execute_block(&[(&get, &good), (&del, &bad)])
        .unwrap_err();
    assert_eq!(at, 1);
    refused(e);
    // A definition's body, handed its arguments.
    exec(
        &mut db,
        "create statement gone($ids json) public as del e where n in $ids",
    );
    exec(
        &mut db,
        "create statement found($ids json) public as get e select n where n in $ids",
    );
    for sql in ["call gone {ids: 7}", "call found {ids: 7}"] {
        let e = run(&mut db, sql, &[]).unwrap_err().to_string();
        assert!(
            e.contains("takes a list, and $1 is of type int"),
            "{sql}: {e}"
        );
    }
    let call = fennec_ql::parse_one("call found($1)").unwrap();
    let e = db.query(&call, &[long]).unwrap_err().to_string();
    assert!(e.contains("more than 100000"), "{e}");
}
