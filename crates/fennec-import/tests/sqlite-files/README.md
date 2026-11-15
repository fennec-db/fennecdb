# Files SQLite wrote, for `tests/sqlite.rs`

Each file here was written by SQLite itself (`sqlite3` 3.51.0, macOS's), so
what the reader is held to is SQLite's own format and its reading of its own
`CREATE TABLE` text, not our idea of them. The tests read the files and never
run `sqlite3`. They are made again by the commands below, run in an empty
directory and then copied here (`sh` from the block, as written):

```sh
sqlite3 generated.sqlite "PRAGMA page_size=512;
CREATE TABLE t(a INTEGER, b AS (a*2) VIRTUAL, c TEXT, d INTEGER GENERATED ALWAYS AS (a+1) STORED, e TEXT, f AS (c || e));
INSERT INTO t(a, c, e) VALUES (1, 'x', 'y'), (2, 'p', 'q');"

sqlite3 defaults.sqlite "PRAGMA page_size=512;
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES (1, 'one');
ALTER TABLE t ADD COLUMN c INTEGER NOT NULL DEFAULT 7;
ALTER TABLE t ADD COLUMN d TEXT DEFAULT 5;
ALTER TABLE t ADD COLUMN e REAL DEFAULT '2';
ALTER TABLE t ADD COLUMN f DEFAULT -3.5;
ALTER TABLE t ADD COLUMN g BLOB DEFAULT x'00ff';
ALTER TABLE t ADD COLUMN h DEFAULT TRUE;
ALTER TABLE t ADD COLUMN i NUMERIC DEFAULT '3.0e+5';
ALTER TABLE t ADD COLUMN j INT DEFAULT (12);
ALTER TABLE t ADD COLUMN k TEXT;
ALTER TABLE t ADD COLUMN v AS (a * 10) VIRTUAL;
ALTER TABLE t ADD COLUMN l TEXT DEFAULT 'last';
ALTER TABLE t ADD COLUMN m DEFAULT abc;
ALTER TABLE t ADD COLUMN n TEXT DEFAULT -0x10;
ALTER TABLE t RENAME COLUMN b TO \"b two\";
INSERT INTO t(a, \"b two\", c, d, e, f, g, h, i, j, k, l, m, n)
  VALUES (2, 'two', 8, 'w', 1.5, 0, x'01', 0, 1, 2, 'k', 'l', 'm', 'n');"

sqlite3 names.sqlite "PRAGMA page_size=512;
CREATE TABLE quotes(\"dq\" INT, 'sq' TEXT, \`bt\` INT, [br] TEXT, x DECIMAL(10,2), z INT,
  PRIMARY KEY (dq, bt), CONSTRAINT positive CHECK (dq > 0 AND bt IN (1, 2, 3)), UNIQUE (x, z));
INSERT INTO quotes VALUES (1, 's', 2, 'b', 4.5, 9);
CREATE TABLE comments(a INT, -- a comment, with ( a paren, a comma and a ' quote
  b TEXT /* another, ) ' \" comment */, c TEXT DEFAULT 'not -- a comment, (', d INT);
INSERT INTO comments(a, b, d) VALUES (1, 'two', 4);
CREATE TABLE empty(a INT, \"\" INT, c TEXT);
INSERT INTO empty VALUES (1, 2, 'three');
CREATE TABLE \"my (table\"(a INT, b TEXT);
INSERT INTO \"my (table\" VALUES (1, 'b');"

sqlite3 rowid.sqlite "PRAGMA page_size=512;
CREATE TABLE desc_pk(id INTEGER PRIMARY KEY DESC, v TEXT);
CREATE TABLE table_desc(id INTEGER, v TEXT, PRIMARY KEY(id DESC));
CREATE TABLE named_pk(id INTEGER, v TEXT, CONSTRAINT pk PRIMARY KEY(\"id\" COLLATE BINARY ASC));
CREATE TABLE cased(Id INTEGER, v TEXT, PRIMARY KEY(id));
CREATE TABLE in_text(id INTEGER DEFAULT ' PRIMARY KEY ', v TEXT);
INSERT INTO desc_pk VALUES (10, 'a');
INSERT INTO table_desc VALUES (10, 'a');
INSERT INTO named_pk VALUES (10, 'a');
INSERT INTO cased VALUES (10, 'a');
INSERT INTO in_text VALUES (10, 'a');"

sqlite3 without-rowid.sqlite "PRAGMA page_size=512;
CREATE TABLE \"Ünits\"(code TEXT PRIMARY KEY, n INT) WITHOUT ROWID, STRICT;
INSERT INTO \"Ünits\" VALUES ('m', 1);
CREATE TABLE plain(a INT, b TEXT) STRICT;
INSERT INTO plain VALUES (1, 'one');"

sqlite3 large-pages.sqlite "PRAGMA page_size=65536;
CREATE TABLE t(a INTEGER PRIMARY KEY, b BLOB);
INSERT INTO t VALUES (1, zeroblob(70000)), (2, x'0102');"

# A hot rollback journal: SQLite killed in the middle of a transaction whose
# pages spilled into the file, so the file alone is torn and the journal
# holds what puts it back.
sqlite3 hot.sqlite "PRAGMA page_size=512; PRAGMA journal_mode=DELETE;
CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT);
INSERT INTO t(b) VALUES ('committed');"
printf '%s\n' "PRAGMA cache_size=10;" "PRAGMA cache_spill=1;" "BEGIN;" \
  "UPDATE t SET b = 'never committed' WHERE a = 1;" \
  "INSERT INTO t(b) WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<6000) SELECT printf('torn %d', x) FROM c;" \
  '.shell kill -9 $PPID' | sqlite3 hot.sqlite || true

# A journal that is not hot: PERSIST mode zeroes its header at each commit
# and leaves it.
sqlite3 persist.sqlite "PRAGMA page_size=512; PRAGMA journal_mode=PERSIST;
CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT);
INSERT INTO t(b) VALUES ('kept');"
```

Never open `hot.sqlite` with `sqlite3` here: SQLite rolls a hot journal back
at its first read and deletes it, and the file is then no longer what its
test reads.
