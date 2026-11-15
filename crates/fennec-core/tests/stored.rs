//! Named statements as their database keeps them
//! (`docs/design/named-statements.md`, slice 2): record kind 13, alone and
//! in a block, one write of the change counter; put back with its block;
//! read back by every open, its digest checked, and kept by every image;
//! applied by a replica and passed over by a reader of the changes; and
//! held to the schema at each schema change's commit -- refused naming the
//! statement, or marked broken by a writer that cannot refuse.
//!
//! `tests/statements.fennec` is a file of stored statements written by a
//! build of this grammar's epoch, opened by every later one:
//!
//!     FENNEC_WRITE_FIXTURE=1 cargo test -p fennec-core --test all stored::

use fennec_core::codec::{encode_str, get_uvarint, put_uvarint};
use fennec_core::engine::{writes_in, ChangeKind, Statements};
use fennec_core::named::GRAMMAR_EPOCH;
use fennec_core::prelude::*;
use std::sync::{Arc, Mutex};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/statements.fennec");

pub(crate) const SCHEMA: &str = "create collection accounts (owner text @hash, balance int);
create collection entries (src int, dst int, amount int, by text);
create collection likes (post int, user text);
create index on likes (post, user) @unique";

const TRANSFER: &str = "create statement transfer($from int, $to int, $amount int)
  claims (sub text)
  comment 'Moves an amount from an account the caller owns to another'
  check $amount > 0 and $from != $to
as begin
  -- the caller's account, or nothing moves
  get accounts select id where id = $from and owner = $jwt.sub
    limit 1 require 1 else 'not your account';
  set accounts {balance: balance - $amount} where id = $from and balance >= $amount
    require 1 else 'insufficient funds';
  set accounts {balance: balance + $amount} where id = $to
    require 1 else 'no such account';
  insert entries {src: $from, dst: $to, amount: $amount, by: $jwt.sub}
    returning id
end";

const LIKE: &str = "create statement like($post int) claims (sub text) as
  insert likes {post: $post, user: $jwt.sub} if absent returning id";

/// Each write's record, by its number.
pub(crate) type Numbered = Vec<(u64, Vec<u8>)>;

/// A sink keeping the file's bytes and each write's record, numbered.
#[derive(Clone)]
pub(crate) struct Tap {
    pub(crate) file: Arc<Mutex<Vec<u8>>>,
    pub(crate) writes: Arc<Mutex<Numbered>>,
}

impl Default for Tap {
    fn default() -> Tap {
        Tap {
            file: Arc::new(Mutex::new(fennec_core::engine::MAGIC.to_vec())),
            writes: Arc::default(),
        }
    }
}

impl Sink for Tap {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn record(&mut self, seq: u64, bytes: &[u8]) -> Result<()> {
        self.file.lock().unwrap().extend_from_slice(bytes);
        self.writes.lock().unwrap().push((seq, bytes.to_vec()));
        Ok(())
    }
    fn rewrite(&mut self, bytes: &[u8]) -> Result<()> {
        *self.file.lock().unwrap() = bytes.to_vec();
        Ok(())
    }
}

impl Tap {
    /// A database writing into the tap.
    pub(crate) fn database(&self) -> Database {
        fennec_ql::install();
        Database::with_sink(Box::new(self.clone()))
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.file.lock().unwrap().clone()
    }

    pub(crate) fn last(&self) -> (u64, Vec<u8>) {
        self.writes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a write")
    }
}

/// A database opened from `bytes`, as an open of its file reads it.
pub(crate) fn opened(bytes: &[u8]) -> Database {
    fennec_ql::install();
    let mut db = Database::new();
    db.load(bytes).expect("the file opens");
    db
}

/// Each statement of `text` on its own, a block each.
pub(crate) fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for s in fennec_ql::parse(text).expect("parse") {
        last = db.execute(&s)?;
    }
    Ok(last)
}

/// The statements of `text` as one block.
pub(crate) fn block(db: &mut Database, text: &str) -> Result<Vec<Response>> {
    let stmts = fennec_ql::parse(text).expect("parse");
    let pairs: Vec<(&Statement, &[Value])> = stmts.iter().map(|s| (s, &[][..])).collect();
    db.execute_block(&pairs).map_err(|(_, e)| e)
}

pub(crate) fn schema(tap: &Tap) -> Database {
    let mut db = tap.database();
    run(&mut db, SCHEMA).unwrap();
    db
}

/// A statement as a reader compares two: its name, version, epoch,
/// digest, dependencies and text, and whether it is broken.
type Held = (String, (u64, u64), u32, u64, Vec<String>, String, bool);

/// What a table holds, as a reader compares two: everything but the tree.
fn held(t: &Statements) -> Vec<Held> {
    t.iter()
        .map(|s| {
            let s = s.clone();
            let broken = s.broken.is_some();
            (
                s.name, s.version, s.epoch, s.digest, s.depends, s.text, broken,
            )
        })
        .collect()
}

pub(crate) fn digest(text: &str) -> u64 {
    fennec_ql::definition(text).unwrap().digest().unwrap()
}

pub(crate) fn refused(r: Result<impl std::fmt::Debug>) -> String {
    r.expect_err("refused").to_string()
}

#[test]
fn a_definition_is_one_record_of_kind_13_and_one_write() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    let before = db.change_seq();
    run(&mut db, TRANSFER).unwrap();
    let (seq, record) = tap.last();
    assert_eq!(kind_of(&record), 13, "a record of its own kind");
    assert_eq!(writes_in(&record).unwrap(), 1, "a write the feed counts");
    assert_eq!((seq, db.change_seq()), (before + 1, before + 1));
    let t = db.statements();
    let s = t.get("transfer").expect("defined");
    assert_eq!(s.version, (db.history().current(), before + 1));
    assert_eq!(s.epoch, GRAMMAR_EPOCH);
    assert_eq!(s.digest, digest(TRANSFER));
    assert_eq!(
        s.text, TRANSFER,
        "the definition's own bytes, comments and all"
    );
    for d in [
        "accounts",
        "accounts.owner",
        "accounts.balance",
        "entries",
        "entries.by",
    ] {
        assert!(s.depends.iter().any(|x| x == d), "{d} in {:?}", s.depends);
    }
    assert!(s.broken.is_none() && s.tree.is_some());

    // A drop is a record of the kind too.
    run(&mut db, "drop statement transfer").unwrap();
    let (_, record) = tap.last();
    assert_eq!((kind_of(&record), writes_in(&record).unwrap()), (13, 1));
    assert!(db.statements().is_empty());
    assert!(refused(run(&mut db, "drop statement transfer")).contains("statement `transfer`"));
    run(&mut db, "drop statement if exists transfer").unwrap();
}

#[test]
fn a_definition_in_a_block_lands_with_its_writes_as_one_record() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    let before = db.change_seq();
    block(
        &mut db,
        &format!("put accounts {{owner: \"ann\", balance: 10}}; {LIKE}"),
    )
    .unwrap();
    let (seq, record) = tap.last();
    assert_eq!(kind_of(&record), 9, "a block record");
    assert_eq!(writes_in(&record).unwrap(), 2);
    assert_eq!(seq, before + 2);
    // Numbered as the block's second write.
    assert_eq!(db.statements().get("like").unwrap().version.1, before + 2);
}

#[test]
fn a_block_put_back_puts_back_its_definitions() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, LIKE).unwrap();
    let was = held(db.statements());
    let writes = tap.writes.lock().unwrap().len();
    let replaced = LIKE
        .replace("create statement", "create or replace statement")
        .replace("returning id", "returning id, post");
    // The replacement answers another field: refused as it is defined.
    assert!(refused(block(&mut db, &replaced)).contains("answers other fields"));
    let replaced = LIKE
        .replace("create statement", "create or replace statement")
        .replace("if absent ", "");
    let e = block(
        &mut db,
        &format!("{replaced}; {TRANSFER}; put accounts {{balance: \"many\"}}"),
    );
    assert!(e.is_err());
    assert_eq!(held(db.statements()), was, "the table as it was");
    assert_eq!(
        tap.writes.lock().unwrap().len(),
        writes,
        "nothing reached the file"
    );
}

#[test]
fn every_open_reads_a_statement_back_and_every_image_keeps_it() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, TRANSFER).unwrap();
    run(&mut db, LIKE).unwrap();
    run(&mut db, "drop statement like").unwrap();
    run(&mut db, LIKE).unwrap();
    let want = held(db.statements());

    let reopened = opened(&tap.bytes());
    assert_eq!(held(reopened.statements()), want);
    assert_eq!(reopened.change_seq(), db.change_seq());
    for s in reopened.statements().iter() {
        let tree = s.tree.as_ref().expect("read into its tree");
        assert_eq!(tree.digest().unwrap(), s.digest);
    }

    // An image: what a compact writes, a replica is sent and a page keeps.
    let image = db.snapshot();
    let again = opened(&image);
    assert_eq!(held(again.statements()), want, "versions kept through it");
    assert_eq!(held(opened(&again.snapshot()).statements()), want);
}

#[test]
fn a_compact_keeps_the_statements_as_they_were() {
    let dir = std::env::temp_dir().join(format!("fennec-stored-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for mapped in [false, true] {
        let path = dir.join(format!("compact-{mapped}.fennec"));
        let _ = std::fs::remove_file(&path);
        fennec_ql::install();
        let mut db = match mapped {
            true => fennec_core::fs::open_mapped(&path).unwrap(),
            false => fennec_core::fs::open(&path).unwrap(),
        };
        run(&mut db, SCHEMA).unwrap();
        run(&mut db, TRANSFER).unwrap();
        run(
            &mut db,
            "put accounts {owner: \"ann\", balance: 1}; del accounts where id = 1",
        )
        .unwrap();
        let want = held(db.statements());
        run(&mut db, "compact").unwrap();
        assert_eq!(held(db.statements()), want);
        drop(db);
        let db = fennec_core::fs::open(&path).unwrap();
        assert_eq!(held(db.statements()), want, "mapped: {mapped}");
        assert!(db.statements().get("transfer").unwrap().tree.is_some());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_replica_applies_a_definition_and_a_reader_of_the_changes_passes_over_it() {
    let tap = Tap::default();
    let mut primary = schema(&tap);
    run(&mut primary, "put accounts {owner: \"ann\", balance: 10}").unwrap();
    run(&mut primary, TRANSFER).unwrap();
    block(
        &mut primary,
        &format!("{LIKE}; put accounts {{owner: \"bob\", balance: 5}}"),
    )
    .unwrap();
    run(&mut primary, "drop statement transfer").unwrap();
    run(&mut primary, "put accounts {owner: \"cy\", balance: 1}").unwrap();

    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    let writes = tap.writes.lock().unwrap().clone();
    for (seq, record) in &writes {
        replica.apply(record).unwrap();
        assert_eq!(replica.change_seq(), *seq, "the counter, write for write");
    }
    assert_eq!(held(replica.statements()), held(primary.statements()));
    assert!(replica.statements().get("like").unwrap().tree.is_some());
    // The replica's file holds the records as they came.
    assert_eq!(
        held(opened(&replica_tap.bytes()).statements()),
        held(primary.statements())
    );

    // A reader of the changes: the definitions and the drop are writes
    // of no collection, numbered, and nothing else is told of them.
    let all: Vec<u8> = writes.iter().flat_map(|(_, r)| r.clone()).collect();
    let mut seen = Vec::new();
    let last = primary
        .changes_in(&all, 1, &mut |c| {
            seen.push((
                c.seq,
                c.collection.clone(),
                matches!(c.kind, ChangeKind::Statement),
            ));
            true
        })
        .unwrap();
    assert_eq!(last, primary.change_seq());
    let numbers: Vec<u64> = seen.iter().map(|s| s.0).collect();
    assert_eq!(
        numbers,
        (1..=last).collect::<Vec<_>>(),
        "one number each, none skipped"
    );
    let named: Vec<_> = seen.iter().filter(|s| s.2).collect();
    assert_eq!(named.len(), 3, "transfer, like and the drop: {seen:?}");
    assert!(named.iter().all(|s| s.1.is_none()));
}

#[test]
fn a_schema_change_breaking_a_statement_is_refused_naming_it() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, TRANSFER).unwrap();
    run(&mut db, LIKE).unwrap();
    let writes = tap.writes.lock().unwrap().len();
    for (change, says) in [
        (
            "drop index on likes (post, user)",
            "statement `like` relies on `likes (post, user)` being unique: replace or drop it \
             first",
        ),
        (
            "alter collection accounts drop field balance",
            "statement `transfer` reads `accounts.balance`: replace or drop it first",
        ),
        (
            "alter collection accounts rename field owner to holder",
            "statement `transfer` reads `accounts.owner`",
        ),
        (
            "drop collection entries",
            "statement `transfer` reads `entries`",
        ),
    ] {
        let e = refused(run(&mut db, change));
        assert!(e.contains(says), "{change}: {e}");
    }
    assert_eq!(tap.writes.lock().unwrap().len(), writes, "nothing landed");
    assert!(db.collection("entries").is_ok());

    // With the statement replaced first, in one block, the drop lands.
    let replaced = "create or replace statement like($post int) claims (sub text) as
      del likes where post = $post and user = $jwt.sub returning id";
    block(
        &mut db,
        &format!("{replaced}; drop index on likes (post, user)"),
    )
    .unwrap();
    // And dropped first, so does the field's.
    block(
        &mut db,
        "drop statement transfer; alter collection accounts drop field balance",
    )
    .unwrap();
}

#[test]
fn a_rebuild_under_a_statement_lands_where_it_still_checks() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, TRANSFER).unwrap();
    // `declared::rebuild`: the field added again, set from the old,
    // dropped and renamed -- one block, which finds it under its name.
    let s = db.collection("accounts").unwrap().schema.clone();
    let rebuild = fennec_core::declared::rebuild(&s, "balance").unwrap();
    block(&mut db, &rebuild).unwrap();
    assert!(db.statements().get("transfer").unwrap().broken.is_none());
    // A block taking it away for good is refused, naming it.
    let e = refused(block(
        &mut db,
        "alter collection accounts add field cents int; alter collection accounts drop field \
         balance",
    ));
    assert!(
        e.contains("statement `transfer` reads `accounts.balance`"),
        "{e}"
    );
}

#[test]
fn a_writer_that_cannot_refuse_marks_a_statement_broken_while_it_lacks_what_it_reads() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, TRANSFER).unwrap();
    let drop = fennec_ql::parse_one("drop collection entries").unwrap();
    // A follower copying the collection again, the sync's remake.
    db.internally(|db| db.execute(&drop)).unwrap();
    let why = db.statements().get("transfer").unwrap().broken.clone();
    assert_eq!(
        why.as_deref(),
        Some("it reads `entries`, which the schema no longer holds")
    );
    // A replica takes the drop as it came, and marks it too.
    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    for (_, record) in tap.writes.lock().unwrap().iter() {
        replica.apply(record).unwrap();
    }
    assert_eq!(replica.statements().get("transfer").unwrap().broken, why);
    // So does an open.
    assert_eq!(
        opened(&tap.bytes())
            .statements()
            .get("transfer")
            .unwrap()
            .broken,
        why
    );
    // The collection made again, it holds what it reads.
    run(
        &mut db,
        "create collection entries (src int, dst int, amount int, by text)",
    )
    .unwrap();
    assert!(db.statements().get("transfer").unwrap().broken.is_none());
}

/// A statement marked broken keeps its contract as one that is not, as
/// the declared plan holds it: the mark is what the schema lacks, and the
/// tree it was defined as is still the statement a grant names.
#[test]
fn a_broken_statement_is_replaced_only_within_its_contract() {
    use fennec_core::declared::{plan_statements, Declared, Mode, Plan};
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, TRANSFER).unwrap();
    let drop = fennec_ql::parse_one("drop collection entries").unwrap();
    db.internally(|db| db.execute(&drop)).unwrap();
    assert!(db.statements().get("transfer").unwrap().broken.is_some());

    let reads = "create or replace statement transfer($from int) claims (role text) as
      get accounts select id where id = $from and owner = $jwt.role";
    let writes = tap.writes.lock().unwrap().len();
    let e = refused(run(&mut db, reads));
    assert!(
        e.contains("statement `transfer` reads, where it wrote: a replacement keeps"),
        "{e}"
    );
    assert_eq!(tap.writes.lock().unwrap().len(), writes, "nothing landed");
    // The declared plan refuses it alike.
    let mut d = Declared {
        collections: Vec::new(),
        statements: Vec::new(),
        text: None,
        migrations: Vec::new(),
        bound: Vec::new(),
        scheduled: Vec::new(),
        hooked: Vec::new(),
        broken: Vec::new(),
    };
    let text = format!("create collection accounts (owner text @hash, balance int); {reads}");
    fennec_ql::schema_into(&mut d, &text).unwrap();
    let mut plan = Plan::default();
    plan_statements(&mut plan, db.statements(), &d, Mode::Apply);
    let kinds: Vec<_> = plan.refusals.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, ["statement_contract_changed"]);

    // One keeping it lands, holding what it reads.
    let kept = "create or replace statement transfer($from int, $to int, $amount int)
      claims (sub text) as
      set accounts {balance: balance - $amount} where id = $from and owner = $jwt.sub
        returning id";
    run(&mut db, kept).unwrap();
    assert!(db.statements().get("transfer").unwrap().broken.is_none());
}

/// A `create index` run beside the database lands its schema change
/// outside `commit`, and works the marks out again as `commit` and a
/// replica's apply do: the primary, its replica and an open agree.
#[test]
fn an_index_built_beside_the_database_clears_the_mark_on_the_primary_as_on_a_replica() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    run(&mut db, "create index on entries (by) @hash").unwrap();
    let probes = "create statement history() claims (sub text) as
      get accounts select id where owner = $jwt.sub lookup entries on by = owner select amount";
    run(&mut db, probes).unwrap();
    run(&mut db, LIKE).unwrap();
    // A follower copying the collections again, their indexes to come.
    for drop in [
        "drop collection entries",
        "drop index on likes (post, user)",
    ] {
        let drop = fennec_ql::parse_one(drop).unwrap();
        db.internally(|db| db.execute(&drop)).unwrap();
    }
    run(
        &mut db,
        "create collection entries (src int, dst int, amount int, by text)",
    )
    .unwrap();
    for name in ["history", "like"] {
        assert!(
            db.statements().get(name).unwrap().broken.is_some(),
            "{name}"
        );
    }

    let db = std::sync::RwLock::new(db);
    for index in [
        "create index on entries (by) @hash",
        "create index on likes (post, user) @unique",
    ] {
        let index = fennec_ql::parse_one(index).unwrap();
        Database::maintain(&db, &index)
            .expect("built beside the database")
            .unwrap();
    }
    let db = db.into_inner().unwrap();
    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    for (_, record) in tap.writes.lock().unwrap().iter() {
        replica.apply(record).unwrap();
    }
    assert_eq!(held(db.statements()), held(replica.statements()));
    assert_eq!(
        held(db.statements()),
        held(opened(&tap.bytes()).statements())
    );
    for name in ["history", "like"] {
        assert!(
            db.statements().get(name).unwrap().broken.is_none(),
            "{name}"
        );
    }
}

#[test]
fn a_definition_names_collections_there_and_lookups_an_index_answers() {
    let tap = Tap::default();
    let mut db = schema(&tap);
    let e = refused(run(
        &mut db,
        "create statement s() as get nothing select id",
    ));
    assert!(e.contains("collection `nothing`"), "{e}");
    let e = refused(run(
        &mut db,
        "create statement s() as get accounts select id lookup entries on by = owner select amount",
    ));
    assert!(e.contains("needs an index"), "{e}");
    assert!(db.statements().is_empty());
    let e = refused(run(&mut db, &format!("{LIKE}; {LIKE}")));
    assert!(
        e.contains("`create or replace statement` replaces it"),
        "{e}"
    );
}

/// Holds every digest across grammar epochs: the fixture a build of epoch
/// 1 wrote, `stale` in it a record whose digest no tree of its text has --
/// what a later grammar reading the text otherwise would find.
#[test]
fn the_fixture_reads_back_each_digest_and_marks_another_broken() {
    if std::env::var_os("FENNEC_WRITE_FIXTURE").is_some() {
        // The records an earlier build wrote are kept as it wrote them, at
        // the epoch the file is here to hold, and written afresh only
        // where there is no file.
        let file = std::fs::read(FIXTURE).unwrap_or_else(|_| {
            let tap = Tap::default();
            let mut db = schema(&tap);
            run(&mut db, TRANSFER).unwrap();
            run(&mut db, LIKE).unwrap();
            let mut file = db.snapshot();
            let text = "create statement stale() as get accounts select id";
            let mut body = vec![1u8];
            put_uvarint(&mut body, 0);
            put_uvarint(&mut body, db.change_seq() + 1);
            put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
            body.extend_from_slice(&(digest(text) ^ 1).to_le_bytes());
            encode_str(&mut body, "stale");
            put_uvarint(&mut body, 1);
            encode_str(&mut body, "accounts");
            encode_str(&mut body, text);
            // With its checksum, as this version writes a record.
            file.extend_from_slice(&fennec_core::engine::record(13, 0, &body));
            file
        });
        let file = crate::hooked::with_hook(crate::scheduled::with_schedule(with_binding(file)));
        std::fs::write(FIXTURE, file).unwrap();
    }
    let db = opened(&std::fs::read(FIXTURE).expect("the fixture"));
    let t = db.statements();
    assert_eq!(
        t.len(),
        6,
        "transfer, like, stale, the binding, the schedule and the hook"
    );
    for (name, text) in [("transfer", TRANSFER), ("like", LIKE)] {
        let s = t.get(name).unwrap();
        assert_eq!((s.epoch, s.digest), (1, digest(text)), "{name}");
        assert!(
            s.broken.is_none() && s.tree.is_some(),
            "{name}: {:?}",
            s.broken
        );
    }
    let stale = t.get("stale").unwrap();
    assert!(stale.tree.is_none());
    let why = stale.broken.as_deref().unwrap_or_default();
    assert!(
        why.starts_with("its text reads as another statement"),
        "{why}"
    );
    // An unchanged digest calls; another is refused as broken, with why.
    let mut db = db;
    let like = fennec_ql::parse_one("call like {post: 1} as {sub: 'alice'}").unwrap();
    assert!(db.execute(&like).unwrap().rows().is_some());
    let stale = fennec_ql::parse_one("call stale").unwrap();
    let e = db.execute(&stale).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("reads as another statement")),
        "{e}"
    );
}

/// A definition's record (op 1) holds a callable statement: a text there
/// reading as a binding, its digest and all, is none this build writes --
/// a binding is op 3's (named-statements.md, F2's slice 2) -- and is
/// marked broken, never loaded as a statement a call runs.
#[test]
fn a_binding_s_text_in_a_callable_record_is_marked_broken() {
    let tap = Tap::default();
    let db = schema(&tap);
    let mut file = db.snapshot();
    let text = "create statement credit after insert entries as set accounts {balance: balance + \
                new.amount} where id = new.dst";
    let mut body = vec![1u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, db.change_seq() + 1);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    body.extend_from_slice(&digest(text).to_le_bytes());
    encode_str(&mut body, "credit");
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "accounts");
    encode_str(&mut body, text);
    // With its checksum, as this version writes a record.
    file.extend_from_slice(&fennec_core::engine::record(13, 0, &body));
    let mut db = opened(&file);
    let credit = db.statements().get("credit").unwrap();
    assert!(credit.tree.is_none());
    let why = credit.broken.clone().unwrap_or_default();
    assert!(
        why.starts_with("its text reads as a bound statement"),
        "{why}"
    );
    let call = fennec_ql::parse_one("call credit").unwrap();
    let e = db.execute(&call).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("reads as a bound statement")),
        "{e}"
    );
}

/// A definition stored before a number past a float's range was refused --
/// `1e400` an infinity then, `-1e-400` 0 -- opens unbroken under the epoch
/// it was stored with, its digest the one main's parser gave its text, and
/// runs as it did; a definition holding one now is refused at its `create`.
#[test]
fn a_definition_stored_with_a_float_past_the_range_runs_as_it_did() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(&mut db, "create collection t (x float, y float)").unwrap();
    run(
        &mut db,
        "put t [{id: 1, x: 5, y: 1}, {id: 2, x: 1e308, y: -1}]",
    )
    .unwrap();
    let text = "create statement big() as get t where x < 1e400 and y > -1e-400 select id";
    let e = fennec_ql::parse_one(text).unwrap_err().to_string();
    assert!(e.contains("number `1e400` is past a float's range"), "{e}");
    let mut file = db.snapshot();
    let mut body = vec![1u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, db.change_seq() + 1);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    // As main's parser took it, `1e400` an infinity.
    body.extend_from_slice(&0x5aef_e2bf_1e46_f4cfu64.to_le_bytes());
    encode_str(&mut body, "big");
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "t");
    encode_str(&mut body, text);
    file.extend_from_slice(&fennec_core::engine::record(13, 0, &body));
    let mut db = opened(&file);
    let s = db.statements().get("big").unwrap();
    assert!(s.broken.is_none() && s.tree.is_some(), "{:?}", s.broken);
    let call = fennec_ql::parse_one("call big").unwrap();
    let rows = db.execute(&call).unwrap();
    let ids: Vec<_> = rows.rows().unwrap().rows.iter().map(|r| r.id).collect();
    assert_eq!(ids, [1]);
}

/// No record this build writes holds a schedule (op 4 is F3's slice 2): a
/// definition's record whose text reads as one, its digest and all, is
/// marked broken, never loaded as a statement a call runs.
#[test]
fn a_schedule_s_text_in_a_callable_record_is_marked_broken() {
    let tap = Tap::default();
    let db = schema(&tap);
    let mut file = db.snapshot();
    let text = "create statement zero every 1d in 'Europe/Berlin' as set accounts {balance: 0} \
                where balance < 0";
    let mut body = vec![1u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, db.change_seq() + 1);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    body.extend_from_slice(&digest(text).to_le_bytes());
    encode_str(&mut body, "zero");
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "accounts");
    encode_str(&mut body, text);
    // With its checksum, as this version writes a record.
    file.extend_from_slice(&fennec_core::engine::record(13, 0, &body));
    let mut db = opened(&file);
    let zero = db.statements().get("zero").unwrap();
    assert!(zero.tree.is_none());
    let why = zero.broken.clone().unwrap_or_default();
    assert!(
        why.starts_with("its text reads as a scheduled statement"),
        "{why}"
    );
    let call = fennec_ql::parse_one("call zero").unwrap();
    let e = db.execute(&call).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("reads as a scheduled statement")),
        "{e}"
    );
}

/// No record this build writes holds a hook (op 6 is F4's slice 2): a
/// definition's record whose text reads as one, its digest and all, is
/// marked broken, never loaded as a statement a call runs.
#[test]
fn a_hook_s_text_in_a_callable_record_is_marked_broken() {
    let tap = Tap::default();
    let db = schema(&tap);
    let mut file = db.snapshot();
    let text = "create statement zero($n text from event.data.object.id) on hook stripe where \
                event.type = 'x' as set accounts {balance: 0} where owner = $n";
    let mut body = vec![1u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, db.change_seq() + 1);
    put_uvarint(&mut body, GRAMMAR_EPOCH as u64);
    body.extend_from_slice(&digest(text).to_le_bytes());
    encode_str(&mut body, "zero");
    put_uvarint(&mut body, 1);
    encode_str(&mut body, "accounts");
    encode_str(&mut body, text);
    // With its checksum, as this version writes a record.
    file.extend_from_slice(&fennec_core::engine::record(13, 0, &body));
    let mut db = opened(&file);
    let zero = db.statements().get("zero").unwrap();
    assert!(zero.tree.is_none());
    let why = zero.broken.clone().unwrap_or_default();
    assert!(
        why.starts_with("its text reads as a webhook consumer"),
        "{why}"
    );
    let call = fennec_ql::parse_one("call zero {n: 'a'}").unwrap();
    let e = db.execute(&call).unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("reads as a webhook consumer")),
        "{e}"
    );
}

// ------------------------------------------------- write-bound statements
//
// F2's slice 2 (named-statements.md, *F2 in slices*): a binding's record,
// op 3, kept and applied as op 1 is, the collections it binds masked, and
// the writes it binds refused by a build that runs none and by a binding
// broken or unread -- a write landing without its rows is the one thing a
// binding must never let happen. Slice 3 runs them (`tests/bound.rs`).

/// A statement bound to the rows `entries` makes, over the fixture's
/// schema.
const BINDING: &str = "create statement credit after insert entries where new.amount > 0
  comment 'Credits the account an entry names'
as set accounts {balance: balance + new.amount} where id = new.dst";

/// A database over the fixture's schema.
fn binding(tap: &Tap) -> Database {
    schema(tap)
}

/// `file` with the binding after its records, once, as this build writes
/// it: what `tests/statements.fennec` gained in F2's slice 2.
pub(crate) fn with_binding(file: Vec<u8>) -> Vec<u8> {
    let tap = Tap {
        file: Arc::new(Mutex::new(file.clone())),
        writes: Arc::default(),
    };
    let mut db = tap.database();
    db.load(&file).unwrap();
    if db.statements().get("credit").is_none() {
        run(&mut db, BINDING).unwrap();
    }
    tap.bytes()
}

/// Where a record's body starts: past its kind, its collection and its
/// length.
pub(crate) fn body_at(record: &[u8]) -> usize {
    let mut p = 1;
    get_uvarint(record, &mut p).unwrap();
    get_uvarint(record, &mut p).unwrap();
    p
}

/// A record's kind, the bits that say it carries a checksum and how wide
/// its claim is taken off.
pub(crate) fn kind_of(record: &[u8]) -> u8 {
    match record[0] & 0x80 {
        0 => record[0],
        _ => record[0] & 0x1f,
    }
}

/// Where a record's body ends: before its claim and checksum, where it
/// carries them.
pub(crate) fn body_end(record: &[u8]) -> usize {
    record.len()
        - match record[0] & 0x80 {
            0 => 0,
            _ => ((record[0] >> 5) & 3) as usize + 1 + 4,
        }
}

/// The events `collection`'s writes wake, as its mask holds them.
fn mask(db: &Database, collection: &str) -> u8 {
    db.collection(collection).unwrap().bound
}

/// A kind-13 record of op 3 written by hand: what a later build, or
/// another, may have written.
fn bound_record(name: &str, text: &str, collection: &str, events: u8, change: u64) -> Vec<u8> {
    bound_record_at(GRAMMAR_EPOCH, name, text, collection, events, change)
}

/// [`bound_record`] as a build of grammar `epoch` wrote it.
fn bound_record_at(
    epoch: u32,
    name: &str,
    text: &str,
    collection: &str,
    events: u8,
    change: u64,
) -> Vec<u8> {
    let mut body = vec![3u8];
    put_uvarint(&mut body, 0);
    put_uvarint(&mut body, change);
    put_uvarint(&mut body, epoch as u64);
    let digest = fennec_ql::definition(text)
        .ok()
        .and_then(|d| d.digest().ok());
    body.extend_from_slice(&digest.unwrap_or(1).to_le_bytes());
    encode_str(&mut body, name);
    put_uvarint(&mut body, 1);
    encode_str(&mut body, collection);
    encode_str(&mut body, collection);
    body.push(events);
    encode_str(&mut body, text);
    // As this version writes a record, with its checksum: a file it
    // wrote holds none without one after one with.
    fennec_core::engine::record(13, 0, &body)
}

#[test]
fn a_binding_is_one_record_of_op_3_and_the_writes_it_binds_run_it() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    run(&mut db, "put accounts {owner: \"ann\", balance: 10}").unwrap();
    let before = db.change_seq();
    run(&mut db, BINDING).unwrap();
    let (seq, record) = tap.last();
    assert_eq!(
        (kind_of(&record), record[body_at(&record)]),
        (13, 3),
        "kind 13, op 3"
    );
    assert_eq!(writes_in(&record).unwrap(), 1, "a write the feed counts");
    assert_eq!(seq, before + 1);
    let t = db.statements();
    let s = t.get("credit").expect("defined");
    assert_eq!(s.after, Some(("entries".to_string(), 1)));
    assert_eq!((s.epoch, s.digest), (GRAMMAR_EPOCH, digest(BINDING)));
    assert!(s.broken.is_none() && s.tree.is_some());
    for d in [
        "entries",
        "entries.amount",
        "entries.dst",
        "accounts",
        "accounts.balance",
    ] {
        assert!(s.depends.iter().any(|x| x == d), "{d} in {:?}", s.depends);
    }
    let names: Vec<&str> = t.bound_to("entries").map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["credit"]);
    assert_eq!((mask(&db, "entries"), mask(&db, "accounts")), (1, 0));

    // The write it binds runs it, in its block: the entry and the credit
    // land as one record, or neither does.
    let writes = tap.writes.lock().unwrap().len();
    run(&mut db, "insert entries {src: 1, dst: 1, amount: 5}").unwrap();
    assert_eq!(tap.writes.lock().unwrap().len(), writes + 1, "one record");
    assert_eq!(db.change_seq(), seq + 2, "two writes in it");
    let balance = |db: &mut Database| match run(db, "get accounts select balance where id = 1") {
        Ok(Response::Rows(rs)) => rs.rows[0].values[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(balance(&mut db), Value::Int(15));
    // What it is not bound to wakes nothing: `entries`' sets and deletes,
    // and every other collection.
    run(
        &mut db,
        "set entries {amount: 1} where src = 1; del entries where src = 1; put accounts {owner: \
         \"cy\", balance: 1}",
    )
    .unwrap();
    assert_eq!(balance(&mut db), Value::Int(15));
    // Never called: its values are a row's.
    let e = refused(run(&mut db, "call credit"));
    let says =
        "statement `credit` is bound after `insert` on `entries`: the writes it binds run it";
    assert!(e.contains(says), "{e}");

    // Dropped, the writes it bound wake nothing.
    run(&mut db, "drop statement credit").unwrap();
    assert_eq!(mask(&db, "entries"), 0);
    assert!(db.statements().bound_to("entries").next().is_none());
    run(&mut db, "insert entries {src: 1, dst: 1, amount: 5}").unwrap();
    assert_eq!(balance(&mut db), Value::Int(15));
}

/// A collection's bindings are a run of the table's index, in name order,
/// whatever is defined or dropped beside them.
#[test]
fn a_collection_s_bindings_are_read_in_name_order() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    let body = "as set accounts {balance: 0} where id = 1";
    for text in [
        format!("create statement b after del likes {body}"),
        format!("create statement d after insert entries {body}"),
        format!("create statement a after insert likes {body}"),
        "create statement c() as get accounts select id".into(),
        format!("create statement aa after set likes {body}"),
    ] {
        run(&mut db, &text).unwrap();
    }
    let names = |db: &Database, c: &str| -> Vec<String> {
        db.statements()
            .bound_to(c)
            .map(|s| s.name.clone())
            .collect()
    };
    assert_eq!(names(&db, "likes"), ["a", "aa", "b"]);
    assert_eq!(names(&db, "entries"), ["d"]);
    assert!(names(&db, "accounts").is_empty());
    assert_eq!((mask(&db, "likes"), mask(&db, "entries")), (7, 1));
    run(&mut db, "drop statement aa; drop statement c").unwrap();
    assert_eq!(names(&db, "likes"), ["a", "b"]);
    assert_eq!(mask(&db, "likes"), 5);
    assert_eq!(names(&opened(&tap.bytes()), "likes"), ["a", "b"]);
}

#[test]
fn an_open_an_image_a_replica_a_compact_and_a_restore_keep_a_binding() {
    fennec_ql::install();
    let tap = Tap::default();
    let mut db = binding(&tap);
    // What an archive's image holds: the schema, before the binding.
    let image = db.snapshot();
    let imaged = db.change_seq();
    run(&mut db, BINDING).unwrap();
    run(&mut db, TRANSFER).unwrap();
    let want = held(db.statements());
    let kept = |db: &Database, how: &str| {
        assert_eq!(held(db.statements()), want, "{how}");
        let credit = db.statements().get("credit").unwrap();
        assert_eq!(credit.after, Some(("entries".into(), 1)), "{how}");
        assert!(credit.tree.is_some() && credit.broken.is_none(), "{how}");
        assert_eq!(mask(db, "entries"), 1, "{how}");
        assert_eq!(mask(db, "accounts"), 0, "{how}");
    };
    kept(&opened(&tap.bytes()), "an open");
    let snap = db.snapshot();
    kept(&opened(&snap), "an image");
    kept(&opened(&opened(&snap).snapshot()), "an image of one");

    // A replica: the records as they came, the counter write for write.
    let writes = tap.writes.lock().unwrap().clone();
    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    for (seq, record) in &writes {
        replica.apply(record).unwrap();
        assert_eq!(replica.change_seq(), *seq, "the counter, write for write");
    }
    kept(&replica, "a replica's apply");
    kept(&opened(&replica_tap.bytes()), "a replica's file");
    // Promoted, it runs the binding from its first write.
    run(&mut replica, "put accounts {owner: \"r\", balance: 0}").unwrap();
    run(&mut replica, "insert entries {src: 1, dst: 1, amount: 3}").unwrap();
    let rows = run(&mut replica, "get accounts select balance where id = 1").unwrap();
    assert!(
        matches!(&rows, Response::Rows(rs) if rs.rows[0].values[0] == Value::Int(3)),
        "{rows:?}"
    );

    // A reader of the changes passes over it, numbered as the write it is.
    let all: Vec<u8> = writes.iter().flat_map(|(_, r)| r.clone()).collect();
    let mut named = Vec::new();
    db.changes_in(&all, 1, &mut |c| {
        if matches!(c.kind, ChangeKind::Statement) {
            named.push((c.seq, c.collection.clone()));
        }
        true
    })
    .unwrap();
    assert_eq!(named, [(imaged + 1, None), (imaged + 2, None)]);

    let dir = std::env::temp_dir().join(format!("fennec-bound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A restore: an archive's image and the records after it, opened,
    // forked and checkpointed as `fennec restore` writes its file.
    let path = dir.join("restored.fennec");
    let mut file = image;
    for (_, record) in writes.iter().filter(|(seq, _)| *seq > imaged) {
        file.extend_from_slice(record);
    }
    std::fs::write(&path, &file).unwrap();
    let mut restored = fennec_core::fs::open(&path).unwrap();
    kept(&restored, "a restore");
    restored.fork(7).unwrap();
    restored.checkpoint().unwrap();
    drop(restored);
    kept(
        &fennec_core::fs::open(&path).unwrap(),
        "a restore's checkpoint",
    );

    // A compact, of a file read into memory and of a mapped one.
    for mapped in [false, true] {
        let path = dir.join(format!("compact-{mapped}.fennec"));
        let _ = std::fs::remove_file(&path);
        let mut db = match mapped {
            true => fennec_core::fs::open_mapped(&path).unwrap(),
            false => fennec_core::fs::open(&path).unwrap(),
        };
        run(&mut db, SCHEMA).unwrap();
        run(&mut db, BINDING).unwrap();
        run(
            &mut db,
            "put accounts {owner: \"ann\", balance: 1}; del accounts where id = 1",
        )
        .unwrap();
        let want = held(db.statements());
        run(&mut db, "compact").unwrap();
        assert_eq!(held(db.statements()), want, "mapped: {mapped}");
        assert_eq!(mask(&db, "entries"), 1, "mapped: {mapped}");
        drop(db);
        let db = fennec_core::fs::open(&path).unwrap();
        assert_eq!(held(db.statements()), want, "mapped: {mapped}");
        assert_eq!(mask(&db, "entries"), 1, "mapped: {mapped}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_mask_follows_a_drop_a_rename_and_an_alter_of_the_bound_collection() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    run(&mut db, BINDING).unwrap();
    // Refused while it stands, naming it.
    for (change, says) in [
        (
            "drop collection entries",
            "statement `credit` reads `entries`",
        ),
        (
            "alter collection entries rename field amount to sum",
            "statement `credit` reads `entries.amount`",
        ),
        (
            "alter collection entries drop field dst",
            "statement `credit` reads `entries.dst`",
        ),
    ] {
        let e = refused(run(&mut db, change));
        assert!(e.contains(says), "{change}: {e}");
    }
    // An alter it does not reach keeps the collection bound.
    run(&mut db, "alter collection entries add field note text").unwrap();
    assert_eq!(mask(&db, "entries"), 1);

    // A writer that cannot refuse drops it: the binding, broken, outlives
    // it, and the collection made again is bound again, the mark gone.
    let drop = fennec_ql::parse_one("drop collection entries").unwrap();
    db.internally(|db| db.execute(&drop)).unwrap();
    let credit = |db: &Database| db.statements().get("credit").unwrap().broken.clone();
    assert_eq!(
        credit(&db).as_deref(),
        Some("it reads `entries`, which the schema no longer holds")
    );
    run(
        &mut db,
        "create collection entries (src int, dst int, amount int, by text)",
    )
    .unwrap();
    assert_eq!(mask(&db, "entries"), 1);
    assert_eq!(credit(&db), None);

    // Renamed by one, the binding is broken, and its writes refused as a
    // broken one's are: 409, until it is defined again or dropped.
    let rename =
        fennec_ql::parse_one("alter collection entries rename field amount to sum").unwrap();
    db.internally(|db| db.execute(&rename)).unwrap();
    let e = run(&mut db, "insert entries {src: 1}").unwrap_err();
    let says = "would wake `credit`, which is broken: it reads `entries.amount`";
    assert!(matches!(&e, Error::Exists(m) if m.contains(says)), "{e}");
    // So on a replica and after an open.
    let replica_tap = Tap::default();
    let mut replica = replica_tap.database();
    for (_, record) in tap.writes.lock().unwrap().iter() {
        replica.apply(record).unwrap();
    }
    let reopened = opened(&tap.bytes());
    for (db, how) in [(&replica, "a replica"), (&reopened, "an open")] {
        assert_eq!(mask(db, "entries"), 1, "{how}");
        assert!(credit(db).is_some(), "{how}");
    }

    // A block put back puts the mask back: the binding dropped, then the
    // collection, then a write that fails.
    let e = block(
        &mut db,
        "drop statement credit; drop collection entries; put accounts {balance: \"many\"}",
    );
    assert!(e.is_err());
    assert_eq!(mask(&db, "entries"), 1);
    assert!(db.statements().get("credit").is_some());
    run(&mut db, "drop statement credit").unwrap();
    assert_eq!(mask(&db, "entries"), 0);
    run(&mut db, "insert entries {src: 1}").unwrap();
}

/// A write waking several bindings, one of them broken, is answered the
/// same whichever comes first by name: 409 naming the broken one.
#[test]
fn a_write_waking_several_is_answered_whatever_their_names_order() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    let body = "after insert entries as set accounts {balance: balance + 1} where id = new.src";
    run(&mut db, &format!("create statement audit {body}")).unwrap();
    run(&mut db, BINDING).unwrap();
    run(&mut db, &format!("create statement zeta {body}")).unwrap();
    run(&mut db, "insert entries {src: 1}").unwrap();
    // `credit` broken, between two that are not: 409 naming it.
    let rename =
        fennec_ql::parse_one("alter collection entries rename field amount to sum").unwrap();
    db.internally(|db| db.execute(&rename)).unwrap();
    let says = "would wake `credit`, which is broken";
    let e = run(&mut db, "insert entries {src: 1}").unwrap_err();
    assert!(matches!(&e, Error::Exists(m) if m.contains(says)), "{e}");
    // The one before it dropped, or the one after it: the same answer.
    for name in ["audit", "zeta"] {
        run(&mut db, &format!("drop statement {name}")).unwrap();
        let e = run(&mut db, "insert entries {src: 1}").unwrap_err();
        assert!(
            matches!(&e, Error::Exists(m) if m.contains(says)),
            "{name}: {e}"
        );
    }
}

#[test]
fn after_del_and_ttl_refuse_each_other_whichever_comes_second() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    let body = "as set accounts {balance: balance + 1} where id = 1";
    run(
        &mut db,
        "create collection sessions (seen timestamp @ttl(1h), user text)",
    )
    .unwrap();
    // A row `@ttl` takes out leaves by no write: `after del` would miss it.
    let e = refused(run(
        &mut db,
        &format!("create statement gone after del sessions {body}"),
    ));
    let says = "is bound after `del` on `sessions`, whose rows `@ttl` takes out";
    assert!(e.contains(says), "{e}");
    // `insert` and `set` hold on one: every row made is an insert, and no
    // `set` finds an expired row.
    run(
        &mut db,
        &format!("create statement seen after insert or set sessions {body}"),
    )
    .unwrap();

    // The other order: `@ttl` given a collection a statement is bound
    // after `del` to -- by `alter`, by `create index`, and by one built
    // beside the database.
    run(
        &mut db,
        "create collection visits (at timestamp @sorted, user text); create collection stays \
         (at timestamp, user text)",
    )
    .unwrap();
    run(
        &mut db,
        &format!("create statement left after del visits {body}"),
    )
    .unwrap();
    run(
        &mut db,
        &format!("create statement went after del stays {body}"),
    )
    .unwrap();
    for (change, says) in [
        (
            "alter collection visits alter field at @ttl(1h)",
            "statement `left` is bound after `del` on `visits`",
        ),
        (
            "create index on stays (at) @ttl(1h)",
            "statement `went` is bound after `del` on `stays`",
        ),
    ] {
        let e = refused(run(&mut db, change));
        assert!(e.contains(says), "{change}: {e}");
    }
    let db = std::sync::RwLock::new(db);
    let index = fennec_ql::parse_one("create index on stays (at) @ttl(1h)").unwrap();
    let e = Database::maintain(&db, &index)
        .expect("beside")
        .unwrap_err();
    assert!(e.to_string().contains("statement `went`"), "{e}");
    let mut db = db.into_inner().unwrap();

    // A writer that cannot refuse marks it broken, which refuses its
    // writes; the sweep, which no binding sees, takes the rows out.
    let insert = fennec_ql::parse_one("insert visits {at: $1, user: \"a\"}").unwrap();
    db.execute_with(&insert, &[Value::Timestamp(1_000)])
        .unwrap();
    let ttl = fennec_ql::parse_one("alter collection visits alter field at @ttl(1h)").unwrap();
    db.internally(|db| db.execute(&ttl)).unwrap();
    let why = db.statements().get("left").unwrap().broken.clone();
    let says = "it is bound after `del` on `visits`, whose rows `@ttl` takes out with no write \
                to wake it";
    assert_eq!(why.as_deref(), Some(says));
    let e = run(&mut db, "del visits where user = \"a\"").unwrap_err();
    let says = "`left`, which is broken";
    assert!(matches!(&e, Error::Exists(m) if m.contains(says)), "{e}");
    let now = 1_800_000_000_000;
    let ids = db.expired("visits", now, 10).unwrap();
    assert_eq!(db.sweep("visits", now, &ids).unwrap(), 1);
}

#[test]
fn a_replacement_keeps_the_kind_the_binding_and_what_it_writes() {
    let tap = Tap::default();
    let mut db = binding(&tap);
    run(&mut db, BINDING).unwrap();
    run(&mut db, LIKE).unwrap();
    let bound = "create or replace statement credit after insert entries";
    let credit = "set accounts {balance: balance + new.amount} where id = new.dst";
    for (text, says) in [
        (
            "create or replace statement credit() as get accounts select id".to_string(),
            "statement `credit` is called, where it was bound to writes",
        ),
        (
            "create or replace statement like after insert likes as set accounts {balance: \
             balance + 1} where id = new.post"
                .to_string(),
            "statement `like` is bound to writes, where it was called",
        ),
        (
            format!("create or replace statement credit after insert or set entries as {credit}"),
            "is bound after `insert or set` on `entries`, where it was bound to other writes",
        ),
        (
            "create or replace statement credit after insert likes as set accounts {balance: \
             balance + 1} where id = new.post"
                .to_string(),
            "is bound after `insert` on `likes`, where it was bound to other writes",
        ),
        (
            format!("{bound} as begin {credit}; insert likes {{post: new.src, user: new.by}} end"),
            "statement `credit` inserts into `likes`, which it did not",
        ),
        (
            format!("{bound} as begin {credit}; del accounts where id = new.src end"),
            "statement `credit` deletes from `accounts`, which it did not",
        ),
    ] {
        let e = refused(run(&mut db, &text));
        assert!(
            e.contains(says) && e.contains("drop statement"),
            "{text}: {e}"
        );
    }
    // Within it: the condition, a value, a guard -- and writing less.
    run(
        &mut db,
        &format!(
            "{bound} where new.amount > 10 as begin get accounts select id where id = new.dst \
             limit 1 require 1 else 'no such account'; set accounts {{balance: balance + \
             new.amount * 2}} where id = new.dst end"
        ),
    )
    .unwrap();
    run(
        &mut db,
        &format!("{bound} as get accounts select id where id = new.dst limit 1 require 1"),
    )
    .unwrap();

    // A record whose text this build cannot read keeps its kind and its
    // binding all the same: they are the record's, outside the text.
    let mut file = db.snapshot();
    let text = "create statement later after del entries as frobnicate entries";
    file.extend(bound_record(
        "later",
        text,
        "entries",
        4,
        db.change_seq() + 1,
    ));
    let mut db = opened(&file);
    let later = db.statements().get("later").unwrap();
    assert!(later.tree.is_none() && later.broken.is_some());
    assert_eq!(mask(&db, "entries"), 5);
    let e = refused(run(
        &mut db,
        "create or replace statement later() as get accounts select id",
    ));
    assert!(e.contains("is called, where it was bound to writes"), "{e}");
    run(
        &mut db,
        "create or replace statement later after del entries as set accounts {balance: 0} \
         where id = old.dst",
    )
    .unwrap();
    assert!(db.statements().get("later").unwrap().broken.is_none());
}

/// What a body writes is counted as the write path counts it
/// (`put_with`): `insert` and `put ... if absent` make a row; `put` and
/// `put ... if absent else set` make one or set one; `set` sets one. A
/// replacement widening by any of them is refused naming the kind it
/// gained; one writing what it wrote, or less, is not. A body's text
/// refuses `put` and an upsert today, so the trees are made by hand: the
/// contract must not lean on that refusal.
#[test]
fn a_replacement_widening_by_a_put_or_an_upsert_is_refused() {
    use fennec_core::query::{Expr, Statement};
    let bound = |body: &str| {
        fennec_ql::definition(&format!(
            "create statement mirror after insert entries as {body}"
        ))
        .unwrap()
    };
    let insert = bound("insert likes {post: new.src, user: new.by}");
    let with = |insert_: bool, if_absent_: bool, upsert: bool| {
        let mut d = insert.clone();
        match &mut d.body[0] {
            Statement::Put {
                insert,
                if_absent,
                else_set,
                ..
            } => {
                (*insert, *if_absent) = (insert_, if_absent_);
                *else_set = upsert.then(|| vec![("user".into(), Expr::Field("user".into()))]);
            }
            s => panic!("{s:?}"),
        }
        d
    };
    let put = with(false, false, false);
    let absent = with(true, true, false);
    let upsert = with(true, true, true);
    let set = bound("set likes {user: new.by} where post = new.src");
    let sets = "statement `mirror` sets rows of `likes`, which it did not";
    let inserts = "statement `mirror` inserts into `likes`, which it did not";
    let refused_as = |new: &fennec_core::named::Definition, old, says: &str| {
        let e = new.breaks_contract(old).expect("refused");
        assert!(e.contains(says) && e.contains("drop statement"), "{e}");
    };

    // Bound with an `insert` (or `if absent`): a `put` or an upsert may set
    // a row.
    for old in [&insert, &absent] {
        refused_as(&put, old, sets);
        refused_as(&upsert, old, sets);
        assert_eq!(absent.breaks_contract(old), None);
        assert_eq!(insert.breaks_contract(old), None);
    }
    // Bound with a `set`: each of them may make one.
    for new in [&put, &upsert, &insert, &absent] {
        refused_as(new, &set, inserts);
    }
    // Bound with a `put` or an upsert, each writes what it wrote, or less.
    for old in [&put, &upsert] {
        for new in [&put, &upsert, &set, &insert, &absent] {
            assert_eq!(new.breaks_contract(old), None);
        }
    }
}

/// A text read as a binding of other writes than its record names -- or
/// a binding's record holding a callable text -- is none this build wrote:
/// marked broken, its record's mask kept, so its writes stay refused.
#[test]
fn a_bound_record_reading_otherwise_is_marked_broken() {
    let tap = Tap::default();
    let db = schema(&tap);
    let mut file = db.snapshot();
    let other = "create statement other after set entries as set accounts {balance: 0} where id \
                 = old.dst";
    let callable = "create statement callable() as get accounts select id";
    let seq = db.change_seq();
    file.extend(bound_record("other", other, "entries", 1, seq + 1));
    file.extend(bound_record("callable", callable, "likes", 4, seq + 2));
    let mut db = opened(&file);
    for (name, says) in [
        ("other", "binds other writes than its record"),
        ("callable", "reads as a callable statement"),
    ] {
        let s = db.statements().get(name).unwrap();
        let why = s.broken.as_deref().unwrap_or_default();
        assert!(s.tree.is_none() && why.contains(says), "{name}: {why}");
    }
    assert_eq!((mask(&db, "entries"), mask(&db, "likes")), (1, 4));
    let e = run(&mut db, "insert entries {src: 1}").unwrap_err();
    let says = "`other`, which is broken";
    assert!(matches!(&e, Error::Exists(m) if m.contains(says)), "{e}");
}

/// Grammar epoch 3: a number a statement writes is read as written within
/// its range or refused (`Parser::int_in`), where epoch 2 clamped it or
/// read it whole -- `fuse k 2000000` and `99999999999`, `ef 0` read as 1,
/// `rerank ... candidates 0` read as 1. A definition an epoch-2 build
/// stored holding one no longer reads: marked broken, told to define it
/// again, its calls refused; a bound one refuses every write that would
/// wake it -- never run as a tree its author did not write, nor its writes
/// let land without its rows. Defined again within the range, it runs.
#[test]
fn a_number_epoch_3_refuses_marks_a_stored_definition_broken() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(
        &mut db,
        "create collection a (body text @text, e vector<3> @hnsw);
         create collection log (n int)",
    )
    .unwrap();
    run(&mut db, "put a {body: \"rust\", e: [1, 0, 0]}").unwrap();
    let mut file = db.snapshot();
    // A callable definition's record (op 1) as epoch 2 wrote it; no tree of
    // this grammar is its digest, so any stands for it.
    let callable = |name: &str, text: &str, change: u64| {
        let mut body = vec![1u8];
        put_uvarint(&mut body, 0);
        put_uvarint(&mut body, change);
        put_uvarint(&mut body, 2);
        body.extend_from_slice(&7u64.to_le_bytes());
        encode_str(&mut body, name);
        put_uvarint(&mut body, 1);
        encode_str(&mut body, "a");
        encode_str(&mut body, text);
        // As this version writes a record, with its checksum: a file it
        // wrote holds none without one after one with.
        fennec_core::engine::record(13, 0, &body)
    };
    let seq = db.change_seq();
    let near = "get a select body match body \"rust\" near e [1, 0, 0]";
    let texts = [
        (
            "s1",
            format!("create statement s1() as {near} fuse k 2000000"),
        ),
        (
            "s2",
            format!("create statement s2() as {near} fuse k 99999999999"),
        ),
        (
            "s3",
            "create statement s3() as get a near e [1, 0, 0] ef 0".into(),
        ),
        (
            "s4",
            "create statement s4() as get a match body \"rust\" rerank e [1, 0, 0] candidates 0"
                .into(),
        ),
    ];
    for (i, (name, text)) in texts.iter().enumerate() {
        file.extend(callable(name, text, seq + 1 + i as u64));
    }
    let bound = "create statement b2 after insert log scans as set a {body: \"seen\"} where id in \
                 (get a select id near e [1, 0, 0] ef 0 limit 1)";
    file.extend(bound_record_at(2, "b2", bound, "log", 1, seq + 5));
    let mut db = opened(&file);
    let why = |db: &Database, name: &str| {
        let s = db.statements().get(name).unwrap();
        assert!(s.tree.is_none(), "{name}");
        s.broken.clone().unwrap_or_default()
    };
    for (name, says) in [
        ("s1", "`k` is 0 to 1000000, not 2000000"),
        ("s2", "`k` is 0 to 1000000, not 99999999999"),
        ("s3", "`ef` is 1 to 4294967295, not 0"),
        ("s4", "`candidates` is 1 to 10000, not 0"),
        ("b2", "`ef` is 1 to 4294967295, not 0"),
    ] {
        let why = why(&db, name);
        let ends = format!(
            "under grammar epoch {GRAMMAR_EPOCH}, where it was defined under 2: define it again"
        );
        assert!(
            why.starts_with("its text no longer reads: ")
                && why.contains(says)
                && why.ends_with(&ends),
            "{name}: {why}"
        );
    }
    // A call is refused, as broken.
    let e = run(&mut db, "call s1").unwrap_err();
    assert!(
        matches!(&e, Error::Exists(m) if m.contains("no longer reads")),
        "{e}"
    );
    // The write the binding would wake is refused: its rows never land
    // without it.
    let e = run(&mut db, "insert log {n: 1}").unwrap_err();
    let says = "the write would wake `b2`, which is broken: its text no longer reads";
    assert!(matches!(&e, Error::Exists(m) if m.contains(says)), "{e}");
    let count = |db: &mut Database| run(db, "get log").unwrap().rows().unwrap().rows.len();
    assert_eq!(count(&mut db), 0);
    // Defined again within the range, each runs.
    run(
        &mut db,
        &format!("create or replace statement s1() as {near} fuse k 1000000"),
    )
    .unwrap();
    assert!(db.statements().get("s1").unwrap().broken.is_none());
    assert!(run(&mut db, "call s1").unwrap().rows().is_some());
    let again = bound
        .replace("statement b2", "or replace statement b2")
        .replace("ef 0", "ef 1");
    run(&mut db, &again).unwrap();
    run(&mut db, "insert log {n: 1}").unwrap();
    assert_eq!(count(&mut db), 1);
    let r = run(&mut db, "get a select body").unwrap();
    assert_eq!(
        r.rows().unwrap().rows[0].values[0],
        Value::Text("seen".into())
    );
}

/// Grammar epoch 4: a list of numbers whose `f32`s would pass for the whole
/// numbers written where one was another reads as written, a list, where
/// epoch 3 read `[1.00000001, 2]` into the vector `[1, 2]`. A definition an
/// epoch-3 build stored holding one reads as another tree, and is marked
/// broken, told to define it again; one holding whole numbers written as
/// floats, or an embedding's fractions, reads as it did.
#[test]
fn a_hidden_rounding_epoch_4_marks_a_stored_definition_broken() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(&mut db, "create collection a (fs [float], e vector<2>)").unwrap();
    let mut file = db.snapshot();
    let hidden = "create statement hidden() public as insert a {fs: [1.00000001, 2]}";
    // The tree epoch 3 read the text into, and so the digest it stored.
    let mut was = fennec_ql::definition(hidden).unwrap();
    match &mut was.body[0] {
        Statement::Put { docs, .. } => match &mut docs[0][0].1 {
            Expr::Lit(v) => {
                assert_eq!(
                    *v,
                    Value::List(vec![Value::Float(1.00000001), Value::Int(2)])
                );
                *v = Value::Vector(vec![1.0, 2.0]);
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    let whole = "create statement whole() public as insert a {fs: [1.0, 2], e: [0.1, 0.2]}";
    let record = |name: &str, text: &str, digest: u64, change: u64| {
        let mut body = vec![1u8];
        put_uvarint(&mut body, 0);
        put_uvarint(&mut body, change);
        put_uvarint(&mut body, 3);
        body.extend_from_slice(&digest.to_le_bytes());
        encode_str(&mut body, name);
        put_uvarint(&mut body, 1);
        encode_str(&mut body, "a");
        encode_str(&mut body, text);
        // As this version writes a record, with its checksum: a file it
        // wrote holds none without one after one with.
        fennec_core::engine::record(13, 0, &body)
    };
    let seq = db.change_seq();
    file.extend(record("hidden", hidden, was.digest().unwrap(), seq + 1));
    file.extend(record("whole", whole, digest(whole), seq + 2));
    let mut db = opened(&file);
    let s = db.statements().get("hidden").unwrap();
    assert_eq!(
        s.broken.as_deref(),
        Some(
            format!(
                "its text reads as another statement under grammar epoch {GRAMMAR_EPOCH}, \
                 where it was defined under 3: define it again"
            )
            .as_str()
        )
    );
    assert!(db.statements().get("whole").unwrap().broken.is_none());
    run(&mut db, "call whole").unwrap();
    run(
        &mut db,
        &hidden.replace("statement", "or replace statement"),
    )
    .unwrap();
    run(&mut db, "call hidden").unwrap();
    let r = run(&mut db, "get a select fs order id").unwrap();
    let fs = |xs: &[f64]| Value::List(xs.iter().map(|&x| Value::Float(x)).collect());
    let got: Vec<Value> = r
        .rows()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.values[0].clone())
        .collect();
    assert_eq!(got, [fs(&[1.0, 2.0]), fs(&[1.00000001, 2.0])]);
}

/// A definition runs as its text read as written, held to the digest of
/// the tree it reads to the quick way (`fennec_ql::stored_definition`): a
/// body's `[0.1, 2]` handed a list of numbers or a json field is the
/// numbers written -- a call's, a binding's at each write that wakes it, a
/// stored record's an epoch-3 build wrote, which reads to its digest still
/// -- where the quick read's `f32`s were refused at every call and every
/// write a binding wakes, the collection unwritable. A vector field
/// compared with such a list matches the row holding its `f32`s.
#[test]
fn a_definition_runs_its_lists_as_written() {
    let tap = Tap::default();
    let mut db = tap.database();
    run(
        &mut db,
        "create collection na (k text, fs [float], j json, v vector<2>);
         create collection nlog (n int)",
    )
    .unwrap();
    run(&mut db, "put na {k: 'row', v: [0.5, 2]}").unwrap();
    let mut file = db.snapshot();
    // The definitions each a record an epoch-3 build wrote: its digest the
    // quick read's tree's, which the text read as written is held to.
    let texts = [
        (
            "tenth",
            "create statement tenth() public as insert na {k: 'tenth', fs: [0.5, 2], j: [0.1, 2]}",
        ),
        (
            "aimed",
            "create statement aimed() public scans as \
             set na {fs: [0.25, 0.1]} where v = [0.5, 2] require 1",
        ),
    ];
    let record = |name: &str, text: &str, change: u64| {
        let mut body = vec![1u8];
        put_uvarint(&mut body, 0);
        put_uvarint(&mut body, change);
        put_uvarint(&mut body, 3);
        body.extend_from_slice(&digest(text).to_le_bytes());
        encode_str(&mut body, name);
        put_uvarint(&mut body, 1);
        encode_str(&mut body, "na");
        encode_str(&mut body, text);
        // As this version writes a record, with its checksum: a file it
        // wrote holds none without one after one with.
        fennec_core::engine::record(13, 0, &body)
    };
    let seq = db.change_seq();
    for (i, (name, text)) in texts.iter().enumerate() {
        file.extend(record(name, text, seq + 1 + i as u64));
    }
    // Opened from the records, and defined here.
    let mut defined = opened(&tap.bytes());
    for (_, text) in texts {
        run(&mut defined, text).unwrap();
    }
    for mut db in [opened(&file), defined] {
        for (name, _) in texts {
            let s = db.statements().get(name).unwrap();
            assert!(s.broken.is_none(), "{name}: {:?}", s.broken);
        }
        run(&mut db, "call tenth").unwrap();
        run(&mut db, "call aimed").unwrap();
        let r = run(&mut db, "get na select k, fs, j order id").unwrap();
        let r = fennec_core::json::response_to_string(&r);
        let want =
            r#"[{"k":"row","fs":[0.25,0.1],"j":null},{"k":"tenth","fs":[0.5,2],"j":[0.1,2]}]"#;
        assert!(r.contains(want), "{r}");
    }
    // A binding: each `insert nlog` wakes it, and lands with its row.
    run(
        &mut db,
        "create statement logged after insert nlog as insert na {k: 'logged', fs: [0.25], j: [0.1]}",
    )
    .unwrap();
    for n in 0..2 {
        run(&mut db, &format!("insert nlog {{n: {n}}}")).unwrap();
    }
    let r = run(&mut db, "get na select fs, j where k = 'logged'").unwrap();
    let r = fennec_core::json::response_to_string(&r);
    let want = r#"[{"fs":[0.25],"j":[0.1]},{"fs":[0.25],"j":[0.1]}]"#;
    assert!(r.contains(want), "{r}");
}

/// An op or an event this version does not know is refused, never read
/// as something else -- what a build before op 3 does with op 3, which
/// `make upgrade-check` holds an older binary to.
#[test]
fn an_op_or_an_event_this_version_does_not_read_is_refused() {
    let tap = Tap::default();
    let db = schema(&tap);
    let text = "create statement later after insert entries as set accounts {balance: 0} where \
                id = new.dst";
    for (op, events) in [(4u8, 1u8), (3, 8), (3, 0)] {
        let record = bound_record("later", text, "entries", events, db.change_seq() + 1);
        // The op patched, and the record summed again: a whole record of
        // an op this version does not know, not a torn one.
        let mut body = record[body_at(&record)..body_end(&record)].to_vec();
        body[0] = op;
        let record = fennec_core::engine::record(13, 0, &body);
        let mut file = db.snapshot();
        file.extend_from_slice(&record);
        let e = Database::new().load(&file).unwrap_err();
        assert!(
            e.to_string()
                .contains("a statement record this version does not read"),
            "op {op}, events {events}: {e}"
        );
    }
}

/// `tests/statements.fennec` holds the binding after F1's records: every
/// build reads it and keeps it through its images; this one runs it, and
/// the browser module refuses its writes in its own words
/// (`web/fennec.test.js`).
#[test]
fn the_fixture_s_binding_is_read_and_runs() {
    let mut db = opened(&std::fs::read(FIXTURE).expect("the fixture"));
    let credit = db.statements().get("credit").expect("the binding");
    assert_eq!(credit.after, Some(("entries".to_string(), 1)));
    // Written under epoch 2, its digest the one every later grammar reads.
    assert_eq!((credit.epoch, credit.digest), (2, digest(BINDING)));
    assert!(credit.tree.is_some() && credit.broken.is_none());
    assert_eq!(mask(&db, "entries"), 1);
    let again = opened(&db.snapshot());
    assert_eq!(held(again.statements()), held(db.statements()));
    let (_, kept) = Database::bound_counts();
    run(&mut db, "insert entries {src: 1, dst: 1, amount: 5}").unwrap();
    assert_eq!(Database::bound_counts().1, kept + 1, "the row woke it");
    run(&mut db, "del entries where src = 1").unwrap();
}
