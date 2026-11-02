//! A field's two indexes: `@text` beside a `@hash`, `@unique` or `@sorted`
//! of its own, each built and kept up -- the `@unique` refusing on every
//! write as it does alone -- and every other second index refused where it
//! is declared. Kept as the last one written, `email text @unique @text`
//! was a text index alone and took every duplicate the `@unique` was there
//! to refuse.

use fennec_core::prelude::*;
use std::sync::{Arc, Mutex};

fn run(db: &mut Database, sql: &str) -> Result<Response> {
    db.execute(&fennec_ql::parse_one(sql).expect("parse"))
}

fn run_with(db: &mut Database, sql: &str, params: &str) -> Result<Response> {
    let params = fennec_core::json::parse_params(params).expect("params");
    db.execute_with(&fennec_ql::parse_one(sql).expect("parse"), &params)
}

fn ok(db: &mut Database, sql: &str) {
    run(db, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn duplicate(r: Result<Response>, what: &str) {
    match r {
        Err(Error::Duplicate(m)) => assert!(m.contains("is unique"), "{what}: {m}"),
        other => panic!("{what}: {other:?}"),
    }
}

/// The ids a read answers, in its order.
fn ids(db: &Database, sql: &str) -> Vec<u64> {
    let r = db
        .query(&fennec_ql::parse_one(sql).expect("parse"), &[])
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    r.rows().unwrap().rows.iter().map(|r| r.id).collect()
}

/// The ids a `match` finds, ascending: it orders by relevance.
fn matched(db: &Database, sql: &str) -> Vec<u64> {
    let mut found = ids(db, sql);
    found.sort_unstable();
    found
}

/// The steps `explain` names for a read.
fn plan(db: &Database, sql: &str) -> String {
    let stmt = fennec_ql::parse_one(&format!("explain {sql}")).expect("parse");
    let r = db
        .query(&stmt, &[])
        .unwrap_or_else(|e| panic!("explain {sql}: {e}"));
    let rows = &r.rows().unwrap().rows;
    let steps: Vec<String> = rows
        .iter()
        .map(|r| match &r.values[..] {
            [Value::Text(s)] => s.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    steps.join("\n")
}

/// Every write that can set a value, each refused for one another row
/// holds, and nothing of any of them written.
fn refuses_every_write(db: &mut Database, c: &str, held: &str, other_id: u64) {
    let tries = [
        format!(r#"insert {c} {{email: "{held}"}}"#),
        format!(r#"put {c} {{email: "{held}"}}"#),
        format!(r#"put {c} {{id: {other_id}, email: "{held}"}}"#),
        format!(r#"set {c} {{email: "{held}"}} where id = {other_id}"#),
        format!(r#"put {c} [{{email: "n@x.y"}}, {{email: "n@x.y"}}]"#),
    ];
    let before = ids(db, &format!("get {c} order id"));
    for t in &tries {
        duplicate(run(db, t), t);
    }
    // `put <c> $n`, its documents a parameter's.
    let t = format!("put {c} $1");
    duplicate(run_with(db, &t, &format!(r#"[{{"email": "{held}"}}]"#)), &t);
    // A block's second statement against its first.
    let put = fennec_ql::parse_one(&format!(r#"put {c} {{email: "m@x.y"}}"#)).unwrap();
    let again = fennec_ql::parse_one(&format!(r#"insert {c} {{email: "m@x.y"}}"#)).unwrap();
    let r = db.execute_block(&[(&put, &[]), (&again, &[])]);
    assert!(matches!(r, Err((1, Error::Duplicate(_)))), "{r:?}");
    assert_eq!(ids(db, &format!("get {c} order id")), before);
}

#[test]
fn unique_beside_text_refuses_on_every_write_and_both_answer() {
    // In either order, and with SQL's word for the text index.
    for decl in [
        "email text @unique @text",
        "email text @text @unique",
        "email text @unique @bm25",
    ] {
        let mut db = Database::new();
        ok(&mut db, &format!("create collection a ({decl}, n int)"));
        ok(&mut db, r#"insert a {email: "x@y.z", n: 1}"#);
        ok(&mut db, r#"insert a {email: "p@q.r", n: 2}"#);
        refuses_every_write(&mut db, "a", "x@y.z", 2);
        // The upsert finds the row by the unique value, and sets it.
        ok(
            &mut db,
            r#"put a {email: "x@y.z", n: 5} if absent else set {n: n + new.n}"#,
        );
        assert_eq!(ids(&db, "get a where n = 6"), [1], "{decl}");
        // The text index answers `match`, kept up by every write.
        ok(&mut db, r#"set a {email: "p@q.r again"} where id = 2"#);
        assert_eq!(ids(&db, r#"get a match email "again""#), [2], "{decl}");
        // The hash answers `=`.
        let p = plan(&db, r#"get a where email = "x@y.z""#);
        assert!(p.contains("the hash index on email"), "{decl}: {p}");
        // A value let go of is free again.
        ok(&mut db, "del a where id = 1");
        ok(&mut db, r#"insert a {email: "x@y.z"}"#);
    }
}

#[test]
fn nocase_unique_beside_text_refuses_by_the_fold() {
    let mut db = Database::new();
    ok(
        &mut db,
        "create collection a (email text nocase @unique @text)",
    );
    ok(&mut db, r#"insert a {email: "Sami@X.io"}"#);
    ok(&mut db, r#"insert a {email: "bob@x.io"}"#);
    duplicate(run(&mut db, r#"insert a {email: "sami@x.IO"}"#), "fold");
    duplicate(
        run(&mut db, r#"set a {email: "SAMI@x.io"} where id = 2"#),
        "fold",
    );
    // The clash names the value as the row holding it stores it.
    match run(&mut db, r#"put a {email: "SAMI@X.IO"}"#) {
        Err(Error::Duplicate(m)) => assert!(m.contains("\"Sami@X.io\""), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(ids(&db, r#"get a match email "sami""#), [1]);
}

#[test]
fn hash_and_sorted_beside_text_answer_each_its_own_question() {
    let mut db = Database::new();
    ok(
        &mut db,
        "create collection a (tag text @hash @text, title text @text @sorted)",
    );
    for (tag, title) in [
        ("red fox", "b fox"),
        ("red fox", "a hound"),
        ("blue", "c fox"),
    ] {
        ok(
            &mut db,
            &format!(r#"put a {{tag: "{tag}", title: "{title}"}}"#),
        );
    }
    let p = plan(&db, r#"get a where tag = "red fox""#);
    assert!(p.contains("the hash index on tag"), "{p}");
    let p = plan(&db, r#"get a where title > "a" order title limit 2"#);
    assert!(p.contains("ordered index on title"), "{p}");
    assert_eq!(
        ids(&db, r#"get a where title > "a" order title"#),
        [2, 1, 3]
    );
    assert_eq!(matched(&db, r#"get a match tag "red""#), [1, 2]);
    assert_eq!(matched(&db, r#"get a match title "fox""#), [1, 3]);
    // A hash beside text takes a value twice, as `@hash` does.
    ok(&mut db, r#"put a {tag: "blue"}"#);
}

#[test]
fn every_other_second_index_is_refused_where_it_is_declared() {
    let refused = [
        "a text @hash @unique",
        "a text @unique @unique",
        "a text @hash @sorted",
        "a text @unique @sorted",
        "a text @sorted @unique",
        "a text @text @text",
        "a text @unique @text @hash",
        "a text @unique @text @sorted",
        "a timestamp @sorted @ttl(1d)",
        "a timestamp @unique @ttl(1d)",
        "a vector<2> @hnsw(cosine) @hash",
        "a sparse<8> @inverted @hash",
        "a geo @geo @sorted",
        "a int @hash @text",
    ];
    for decl in refused {
        let mut db = Database::new();
        let r = fennec_ql::parse_one(&format!("create collection t ({decl})"))
            .map_err(|e| e.to_string())
            .and_then(|s| db.execute(&s).map_err(|e| e.to_string()));
        assert!(r.is_err(), "{decl} was taken: {r:?}");
        // `add field` reads a field as `create collection` does.
        let mut db = Database::new();
        ok(&mut db, "create collection t (b int)");
        let r = fennec_ql::parse_one(&format!("alter collection t add field {decl}"))
            .map_err(|e| e.to_string())
            .and_then(|s| db.execute(&s).map_err(|e| e.to_string()));
        assert!(r.is_err(), "add field {decl} was taken: {r:?}");
    }
}

#[test]
fn create_index_adds_one_beside_the_other_and_refuses_what_cannot_stand_beside() {
    let mut db = Database::new();
    ok(
        &mut db,
        "create collection a (email text @text, nick text @unique, n int @sorted)",
    );
    ok(&mut db, r#"put a {email: "x@y.z hello", nick: "x"}"#);
    ok(&mut db, r#"put a {email: "p@q.r", nick: "p"}"#);
    // `@unique` beside the text index, and `@text` beside the unique one.
    ok(&mut db, "create index on a (email) @unique");
    ok(&mut db, "create index on a (nick) @text");
    duplicate(run(&mut db, r#"put a {email: "p@q.r"}"#), "email");
    duplicate(run(&mut db, r#"put a {nick: "x"}"#), "nick");
    assert_eq!(ids(&db, r#"get a match email "hello""#), [1]);
    assert_eq!(ids(&db, r#"get a match nick "p""#), [2]);
    // The one it keeps, again: refused, or answered under `if not exists`.
    for (field, kind) in [
        ("email", "@unique"),
        ("email", "@text"),
        ("email", "@hash"),
        ("n", "@hash"),
    ] {
        let sql = format!("create index on a ({field}) {kind}");
        assert!(matches!(run(&mut db, &sql), Err(Error::Exists(_))), "{sql}");
        ok(
            &mut db,
            &format!("create index if not exists on a ({field}) {kind}"),
        );
    }
}

#[test]
fn unique_beside_text_over_duplicates_is_refused_naming_them() {
    // A file made before the rule kept `@text` alone: `create index ...
    // @unique` puts the `@unique` back, refused as over any duplicates.
    let mut db = Database::new();
    ok(&mut db, "create collection a (email text @text)");
    ok(&mut db, r#"insert a {email: "x@y.z"}"#);
    ok(&mut db, r#"insert a {email: "p@q.r"}"#);
    ok(&mut db, r#"insert a {email: "x@y.z"}"#);
    match run(&mut db, "create index on a (email) @unique") {
        Err(Error::Duplicate(m)) => {
            assert!(m.contains("\"x@y.z\"") && m.contains("1 and 3"), "{m}")
        }
        other => panic!("{other:?}"),
    }
    // Nothing of it: the text index answers, and writes go on as before.
    assert_eq!(matched(&db, r#"get a match email "x""#), [1, 3]);
    ok(&mut db, r#"insert a {email: "x@y.z"}"#);
    // Beside the database, as a server builds it: refused alike.
    let lock = std::sync::RwLock::new(db);
    let index = fennec_ql::parse_one("create index on a (email) @unique").unwrap();
    let r = Database::maintain(&lock, &index).unwrap();
    assert!(matches!(r, Err(Error::Duplicate(_))), "{r:?}");
    let mut db = lock.into_inner().unwrap();
    // Once each value is held once, it is made beside the text index.
    ok(&mut db, r#"del a where email = "x@y.z" and id != 1"#);
    let lock = std::sync::RwLock::new(db);
    Database::maintain(&lock, &index).unwrap().unwrap();
    let mut db = lock.into_inner().unwrap();
    duplicate(run(&mut db, r#"insert a {email: "x@y.z"}"#), "after");
    assert_eq!(ids(&db, r#"get a match email "x""#), [1]);
}

#[test]
fn a_block_put_back_leaves_the_field_its_one_index() {
    let mut db = Database::new();
    ok(
        &mut db,
        "create collection a (email text @text, nick text @unique)",
    );
    ok(&mut db, r#"put a {email: "x@y.z", nick: "x"}"#);
    ok(&mut db, r#"put a {email: "x@y.z", nick: "y"}"#);
    // Each index made, then a write the block fails on: both put back.
    let unique = fennec_ql::parse_one("create index on a (email) @unique").unwrap();
    let text = fennec_ql::parse_one("create index on a (nick) @text").unwrap();
    let clash = fennec_ql::parse_one(r#"put a {nick: "x"}"#).unwrap();
    let r = db.execute_block(&[(&text, &[]), (&clash, &[])]);
    assert!(matches!(r, Err((1, Error::Duplicate(_)))), "{r:?}");
    let r = db.execute_block(&[(&unique, &[]), (&clash, &[])]);
    assert!(matches!(r, Err((0, Error::Duplicate(_)))), "{r:?}");
    // The text one answers as before, the unique one refuses as before.
    assert_eq!(matched(&db, r#"get a match email "x""#), [1, 2]);
    assert!(run(&mut db, r#"get a match nick "x""#).is_err());
    ok(&mut db, r#"put a {email: "x@y.z"}"#);
    duplicate(run(&mut db, r#"put a {nick: "y"}"#), "nick");
    let s = db.collection("a").unwrap().schema.clone();
    assert_eq!(fennec_core::declared::create(&s)[0], db_ddl());
}

/// The collection [`a_block_put_back_leaves_the_field_its_one_index`] ends
/// with, as `create collection` declares it.
fn db_ddl() -> String {
    let d = fennec_core::schema::TextIndexSpec::default().args();
    format!("create collection a (email text @text({d}), nick text @unique)")
}

/// A file in memory, and the records it was handed.
#[derive(Clone)]
struct Tap {
    file: Arc<Mutex<Vec<u8>>>,
    records: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Sink for Tap {
    fn append(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn record(&mut self, _seq: u64, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        self.records.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        *self.file.lock().unwrap() = bytes.to_vec();
        Ok(())
    }
}

#[test]
fn a_reopened_file_and_a_replica_keep_both_and_refuse_as_the_primary() {
    let tap = Tap {
        file: Arc::new(Mutex::new(fennec_core::engine::MAGIC.to_vec())),
        records: Arc::default(),
    };
    let mut primary = Database::with_sink(Box::new(tap.clone()));
    ok(
        &mut primary,
        "create collection a (email text nocase @unique @text, nick text @text)",
    );
    ok(&mut primary, r#"insert a {email: "x@y.z", nick: "x"}"#);
    ok(&mut primary, "create index on a (nick) @sorted");
    ok(&mut primary, r#"insert a {email: "p@q.r", nick: "p"}"#);
    duplicate(run(&mut primary, r#"insert a {email: "X@Y.Z"}"#), "primary");
    // A replica applying the primary's records, then promoted.
    let mut replica = Database::new();
    for r in tap.records.lock().unwrap().iter() {
        replica.apply(r).unwrap();
    }
    // The file opened again, by its image too once compacted.
    let mut reopened = Database::with_sink(Box::new(tap.clone()));
    let bytes = tap.file.lock().unwrap().clone();
    reopened.load(&bytes).expect("reopen");
    for (what, db) in [("replica", &mut replica), ("reopened", &mut reopened)] {
        assert_eq!(ids(db, r#"get a match email "p""#), [2], "{what}");
        assert_eq!(ids(db, r#"get a match nick "x""#), [1], "{what}");
        let p = plan(db, r#"get a where nick > "a" order nick limit 1"#);
        assert!(p.contains("ordered index on nick"), "{what}: {p}");
        duplicate(run(db, r#"insert a {email: "P@q.r"}"#), what);
        ok(db, r#"insert a {email: "new@x.y", nick: "n"}"#);
        assert_eq!(ids(db, r#"get a match email "new""#), [3], "{what}");
    }
    ok(&mut reopened, "compact");
    let mut again = Database::with_sink(Box::new(tap.clone()));
    let bytes = tap.file.lock().unwrap().clone();
    again.load(&bytes).expect("reopen the image");
    duplicate(run(&mut again, r#"insert a {email: "NEW@x.y"}"#), "image");
    assert_eq!(ids(&again, r#"get a match nick "n""#), [3]);
}

#[test]
fn a_text_index_beside_is_written_behind_a_tag_a_binary_before_it_refuses() {
    use fennec_core::codec::{decode_type, TAG_BESIDE};
    use fennec_core::schema::{Field, IndexKind, TextIndexSpec};
    let text = IndexKind::Text(TextIndexSpec::default());
    let schema = |kinds: &[&IndexKind]| {
        let mut f = Field::new("e", DataType::Text);
        for k in kinds {
            f.add_index((*k).clone()).unwrap();
        }
        Schema::new("u", vec![f]).unwrap().encode()
    };
    let unique = schema(&[&IndexKind::UNIQUE]);
    let both = schema(&[&IndexKind::UNIQUE, &text]);
    // In either order, one field.
    assert_eq!(schema(&[&text, &IndexKind::UNIQUE]), both);
    // Read back, both indexes.
    let back = Schema::decode(&both, &mut 0).unwrap();
    assert_eq!(back.fields[0].index, IndexKind::UNIQUE);
    assert_eq!(back.fields[0].text_index(), Some(TextIndexSpec::default()));
    // Where a binary before it reads the field's type it meets the tag,
    // which is no type: it refuses the file rather than open it with the
    // `@unique` alone, or the text index alone as the last one kept.
    let at = unique.iter().zip(&both).position(|(a, b)| a != b).unwrap();
    assert_eq!(both[at], TAG_BESIDE);
    assert!(decode_type(&both[at..], &mut 0).is_err());
    // A field of one index is written as it always was.
    let alone = Schema::new(
        "u",
        vec![Field::new("e", DataType::Text).indexed(text.clone())],
    );
    assert_eq!(alone.unwrap().encode(), schema(&[&text]));
}
