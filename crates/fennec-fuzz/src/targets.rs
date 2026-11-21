//! The targets: each a parser of bytes a process takes from outside, the
//! seeds it starts from and the tokens its inputs are spelled with. A new
//! parser of untrusted bytes gets its target here, in its pull request, and
//! `.github/workflows/fuzz.yml`'s matrix its name (`tests/all.rs` holds the
//! two to each other).

use crate::Target;
use fennec_core::prelude::*;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub static TARGETS: &[Target] = &[
    Target {
        name: "http",
        about: "fennec_http::http::read_request: a request's head, its body by length or chunked, pipelined, read whole or a few bytes at a time",
        max_len: 64 << 10,
        smoke: 120_000,
        seeds: http_seeds,
        dictionary: HTTP_TOKENS,
        run: http,
    },
    Target {
        name: "json",
        about: "fennec_core::json: parse_json, parse, a page's documents and a query's parameters; what parses is written back and read as itself",
        max_len: 64 << 10,
        smoke: 70_000,
        seeds: json_seeds,
        dictionary: JSON_TOKENS,
        run: json,
    },
    Target {
        name: "fennecql",
        about: "fennec_ql::parse and parse_exact, FennecQL and SQL's spellings, over the repository's corpus of texts",
        max_len: 16 << 10,
        smoke: 180_000,
        seeds: fennecql_seeds,
        dictionary: FENNECQL_TOKENS,
        run: fennecql,
    },
    Target {
        name: "codec",
        about: "fennec_core::codec: a value's and a type's encoding decoded, skipped, read by a path and as a document's fields",
        max_len: 64 << 10,
        smoke: 60_000,
        seeds: codec_seeds,
        dictionary: CODEC_TOKENS,
        run: codec,
    },
    Target {
        name: "image",
        about: "a .fennec file opened read into memory (Database::load) and mapped as a server opens it (load_mapped: an image's index of its frames adopted, in shares on threads, the rest replayed in place), each read whole, its indexes built and its vectors linked, and its records applied as a replica applies a primary's",
        max_len: 256 << 10,
        smoke: 35_000,
        seeds: image_seeds,
        dictionary: IMAGE_TOKENS,
        run: image,
    },
    Target {
        name: "sqlite",
        about: "fennec_import::sqlite: a SQLite file's schema found, its table's rows read and counted",
        // Four pages of SQLite's largest, 64 KiB: page 1, a leaf and its
        // overflow pages. At 64 KiB a file of such pages held page 1
        // alone, and the reader's paths of large pages were never fuzzed.
        max_len: 256 << 10,
        // Each run writes its input as a file, now up to four times as
        // long: 80 000 runs took 65 s on an M-series Mac where 64 KiB
        // inputs took 36, run back to back. 45 000, about 37 s at that
        // rate, hold its time and reach some 3 570 edges where the 64 KiB
        // inputs' 80 000 reached 2 431.
        smoke: 45_000,
        seeds: sqlite_seeds,
        dictionary: SQLITE_TOKENS,
        run: sqlite,
    },
    Target {
        name: "certificate",
        about: "fennec-tls's DER walk of the certificate a PostgreSQL server shows, to its signature algorithm: the hash of a channel binding (tls-server-end-point)",
        max_len: 16 << 10,
        smoke: 300_000,
        seeds: certificate_seeds,
        dictionary: CERTIFICATE_TOKENS,
        run: certificate,
    },
    Target {
        name: "stream",
        about: "a primary's answer as a replica, an archiver and a backup read it (fennec_http::replication: read_head, read_message, Receiver::receive), an image it promised adopted by a copy mapped as a server's (loaded, Database::adopt: repoint, rebind), and a node's stream of every tenant as its archiver reads it (node_archive), read whole or a few bytes at a time",
        max_len: 64 << 10,
        smoke: 90_000,
        seeds: stream_seeds,
        dictionary: STREAM_TOKENS,
        run: stream,
    },
];

/// The repository, where the fixtures the seeds are read from are.
fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// A directory of this process's own for the files a target writes, made
/// at its first use.
fn work() -> &'static Path {
    static WORK: OnceLock<PathBuf> = OnceLock::new();
    WORK.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("fennec-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("the fuzzer's work directory");
        d
    })
}

/// Removes this process's work directory, if one was made.
pub fn clean_work() {
    let d = std::env::temp_dir().join(format!("fennec-fuzz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(d);
}

/// The seeds a make target wrote beside the build (`target/fuzz/seeds`), or
/// where `FENNEC_FUZZ_SEEDS` says: the FennecQL corpus of
/// `tools/fennecql_corpus.py --list`, a JSON object a line.
fn seeds_dir() -> PathBuf {
    match std::env::var_os("FENNEC_FUZZ_SEEDS") {
        Some(d) => PathBuf::from(d),
        None => repo().join("target/fuzz/seeds"),
    }
}

/// The inputs the repository keeps for target `name`, in
/// `crates/fennec-fuzz/seeds/<name>/`, in the order of their names: a
/// target's fixtures, and each finding's input once it was fixed, so every
/// run -- `cargo test` among them -- hands it the target again.
fn kept_seeds(name: &str) -> Vec<Vec<u8>> {
    let dir = repo().join("crates/fennec-fuzz/seeds").join(name);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.sort();
    files.iter().filter_map(|f| std::fs::read(f).ok()).collect()
}

/// `with` before each of `inputs`: a target whose first byte picks how it
/// reads the rest is seeded with each way.
fn each_way(inputs: &[Vec<u8>], ways: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for i in inputs {
        for w in 0..ways {
            let mut s = vec![w];
            s.extend_from_slice(i);
            out.push(s);
        }
    }
    out
}

// ------------------------------------------------------------------ http

const HTTP_TOKENS: &[&[u8]] = &[
    b"GET ",
    b"POST ",
    b"PUT ",
    b"PATCH ",
    b"DELETE ",
    b"OPTIONS ",
    b"HEAD ",
    b" HTTP/1.1\r\n",
    b" HTTP/1.0\r\n",
    b"\r\n",
    b"\r\n\r\n",
    b"Content-Length: ",
    b"Transfer-Encoding: chunked\r\n",
    b"Transfer-Encoding: gzip, chunked\r\n",
    b"Connection: close\r\n",
    b"Connection: keep-alive\r\n",
    b"Host: localhost\r\n",
    b"Expect: 100-continue\r\n",
    b"0\r\n\r\n",
    b"ffffffffffffffff\r\n",
    b";ext=1",
    b"%00",
    b"%zz",
    b"%2F",
    b"?where=",
    b"&limit=",
    b"\t",
    b"\n",
    b"\r",
];

/// A request of `head` (its lines, CRLF-ended) and `body`, by length.
fn req(head: &str, body: &str) -> Vec<u8> {
    let mut r = head.to_string();
    if !body.is_empty() {
        r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    r.push_str("\r\n");
    r.push_str(body);
    r.into_bytes()
}

fn http_seeds() -> Vec<Vec<u8>> {
    let mut s = vec![
        req("GET / HTTP/1.1\r\nHost: localhost\r\n", ""),
        req(
            "GET /docs?where=n%20%3E%201&limit=10&order=-n&select=a,b HTTP/1.1\r\nHost: a\r\nAuthorization: Bearer secret\r\nAccept-Encoding: gzip\r\nIf-None-Match: \"x\"\r\n",
            "",
        ),
        req(
            "POST /query HTTP/1.1\r\nHost: a\r\nContent-Type: application/json\r\nIdempotency-Key: k1\r\n",
            r#"{"q":"get docs where n > $1 limit 5","params":[1]}"#,
        ),
        req(
            "PUT /docs/7 HTTP/1.1\r\nHost: a\r\nIf-Match: \"abc\"\r\nConnection: close\r\n",
            r#"{"title":"x","n":1}"#,
        ),
        req("PATCH /docs/7 HTTP/1.1\r\nHost: a\r\n", r#"{"n":2}"#),
        req("DELETE /docs/7 HTTP/1.1\r\nHost: a\r\n", ""),
        req(
            "OPTIONS /query HTTP/1.1\r\nHost: a\r\nOrigin: http://localhost:3000\r\nAccess-Control-Request-Method: POST\r\n",
            "",
        ),
        req("HEAD /_health HTTP/1.0\r\nConnection: keep-alive\r\n", ""),
        req(
            "POST /batch HTTP/1.1\r\nHost: a\r\nFennec-Wait: 5\r\n",
            r#"[{"q":"put docs {a: 1}"},{"q":"get docs"}]"#,
        ),
        b"POST /docs HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nTrailer-A: x\r\n\r\n".to_vec(),
        b"POST /docs HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n1a\r\n{\"title\":\"chunks and all\"}\r\n0\r\n\r\nGET / HTTP/1.1\r\n\r\n".to_vec(),
        b"GET /a HTTP/1.1\r\n\r\nGET /b?x=%41%2 HTTP/1.1\r\nHost: b\r\n\r\n".to_vec(),
        // Framings refused: a length beside a coding, a folded line, a bare
        // LF, a name with a space before its colon.
        b"POST / HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec(),
        b"GET / HTTP/1.1\r\nX-A: 1\r\n folded\r\n\r\n".to_vec(),
        b"GET / HTTP/1.1\nHost: a\n\n".to_vec(),
        b"GET / HTTP/1.1\r\nHost : a\r\n\r\n".to_vec(),
        b"POST / HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\nabcd".to_vec(),
    ];
    s.dedup();
    each_way(&s, 3)
}

/// A reader handing out at most `step` bytes a read, as a slow client's
/// socket does: a line's and a chunk's reads then end where a read does.
struct Trickle<'a> {
    bytes: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = out.len().min(self.step).min(self.bytes.len());
        out[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

fn http(data: &[u8]) {
    let Some((&how, bytes)) = data.split_first() else {
        return;
    };
    let max_body = 64 << 10;
    match how % 3 {
        0 => requests(&mut std::io::Cursor::new(bytes), max_body),
        n => {
            let step = 1 + (how as usize / 3) % 7 * n as usize;
            let mut r = std::io::BufReader::with_capacity(step.max(1), Trickle { bytes, step });
            requests(&mut r, max_body)
        }
    }
}

/// Requests read off `r` until one is refused or the bytes end, as a
/// connection reads them, each looked at as a route would.
fn requests<R: BufRead>(r: &mut R, max_body: usize) {
    for _ in 0..256 {
        match fennec_http::http::read_request(r, max_body) {
            Ok(Some(req)) => {
                let _ = req.segments();
                let _ = req.header("content-type");
                if !req.keep_alive {
                    return;
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

// ------------------------------------------------------------------ json

const JSON_TOKENS: &[&[u8]] = &[
    b"{",
    b"}",
    b"[",
    b"]",
    b":",
    b",",
    b"\"",
    b"\\",
    b"\\u",
    b"\\ud800",
    b"\\udc00",
    b"\\u0000",
    b"null",
    b"true",
    b"false",
    b"-",
    b"e+",
    b"E-",
    b".",
    b"0",
    b"1e308",
    b"\"id\":",
    b"\"$vector\":",
    b"[[[[[[[[",
    b"]]]]]]]]",
];

fn json_seeds() -> Vec<Vec<u8>> {
    let mut s: Vec<Vec<u8>> = [
        "null",
        "true",
        "-0",
        "0.1",
        "1e400",
        "-1e-400",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775809",
        "123456789012345678901234567890",
        "[1, 2.5, -3e2, 16777217]",
        r#"{"a": {"b": [1, "x", null, true, {"c": []}]}, "z": "é😀\n\t"}"#,
        r#"[{"id": 1, "title": "a", "v": [0.1, 0.2]}, {"id": 2, "meta": {"lang": "tr"}}]"#,
        r#"{"q": "get docs", "params": [1, "two", [3, 4], {"five": 5}]}"#,
        r#""\ud800""#,
        r#"{"a": 1, "a": 2}"#,
    ]
    .iter()
    .map(|t| t.as_bytes().to_vec())
    .collect();
    let deep = |n: usize| ("[".repeat(n) + &"]".repeat(n)).into_bytes();
    s.push(deep(64));
    s.push(deep(65));
    // The repository's JSON fixtures, each member of their top value a seed.
    for dir in ["integrations", "integrations/openapi"] {
        let Ok(entries) = std::fs::read_dir(repo().join(dir)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for f in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "json"))
        {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            match fennec_core::json::parse_json(&text) {
                Ok(Value::Object(members)) => {
                    for (_, v) in members.iter().take(64) {
                        s.push(fennec_core::json::to_string(v).into_bytes());
                    }
                }
                Ok(Value::List(items)) => {
                    for v in items.iter().take(64) {
                        s.push(fennec_core::json::to_string(v).into_bytes());
                    }
                }
                _ => s.push(text.into_bytes()),
            }
        }
    }
    s
}

fn json(data: &[u8]) {
    use fennec_core::json;
    let text = String::from_utf8_lossy(data);
    let _ = json::parse(&text);
    let _ = json::parse_documents(&text);
    let _ = json::parse_params(&text);
    let _ = json::parse_object(&text);
    // What is read is written back and read as itself, as `=` compares,
    // every number among it: one past a double's range, which JSON cannot
    // say back, is refused by the reader (`num::past_range`), where it
    // read `1e400` as an infinity written back as `null`, and the round
    // trip was held to finite values alone.
    if let Ok(v) = json::parse_json(&text) {
        let out = json::to_string(&v);
        let again =
            json::parse_json(&out).unwrap_or_else(|e| panic!("{out:?} written, then refused: {e}"));
        assert!(
            again.cmp_value(&v) == std::cmp::Ordering::Equal,
            "{text:?} read as {v:?}, written as {out:?}, read back as {again:?}"
        );
    }
}

// ------------------------------------------------------------- fennecql

const FENNECQL_TOKENS: &[&[u8]] = &[
    b"get ",
    b"put ",
    b"insert ",
    b"set ",
    b"del ",
    b"create collection ",
    b"create index on ",
    b"create statement ",
    b"alter collection ",
    b"drop ",
    b"select ",
    b"where ",
    b"order ",
    b"limit ",
    b"offset ",
    b"group ",
    b"lookup ",
    b"near ",
    b"match ",
    b"fuse ",
    b"rerank ",
    b"facet ",
    b"returning ",
    b"require ",
    b"explain ",
    b"begin",
    b"commit",
    b"call ",
    b"in (get ",
    b"$1",
    b"$name",
    b"@hash",
    b"@sorted",
    b"@unique",
    b"@text",
    b"@hnsw",
    b"@geo",
    b"@ttl(1h)",
    b"@inverted",
    b"vector<3>",
    b"sparse<8>",
    b"[[[[",
    b"]]]]",
    b"((((",
    b"))))",
    b"{a: ",
    b"}",
    b"'",
    b"\"",
    b"`",
    b"--",
    b"/*",
    b"*/",
    b";",
    b" and ",
    b" or ",
    b" not ",
    b" = ",
    b" != ",
    b" like ",
    b" is null",
    b" json",
    b" nocase",
    b" collate tr",
    b"SELECT * FROM ",
    b" JOIN ",
    b" ON ",
    b"WITH ",
];

fn fennecql_seeds() -> Vec<Vec<u8>> {
    let mut s: Vec<Vec<u8>> = [
        "create collection docs (title text @text, n int @sorted, v vector<3> @hnsw, meta json, tags [text], at timestamp @ttl(1h))",
        "put docs {title: 'a b', n: 1, v: [0.1, 0.2, 0.3], meta: {lang: 'tr', rank: [1, 2.5]}, tags: ['x']}",
        "get docs where n > 1 and title match 'b' order n desc limit 10 offset 2",
        "get docs near v [0.1, 0.2, 0.3] limit 5 select id, _score",
        "get docs where id in (get docs where n = 1 select id) lookup tags on id = n required",
        "set docs {n: n + 1} where meta.lang = 'tr' order n limit 3 returning id, n",
        "del docs where at < now() - 1d require 2 else 'two rows'",
        "get docs select n, count(*) as c, approx_count_distinct(title) group n having c > 1 facet tags top 5",
        "create statement bump($by int) as set docs {n: n + $by} where n > 0",
        "begin",
        "commit",
        "SELECT a, b FROM t JOIN u ON t.id = u.t_id WHERE a > 1 ORDER BY 1 LIMIT 3",
        "create table t (a integer primary key, b text[], c jsonb, d vector(3))",
        "get docs where title like 'a%' or not (n between 1 and 5) and highlight(title) is not null",
    ]
    .iter()
    .map(|t| t.as_bytes().to_vec())
    .collect();
    s.push(
        ("get d where n = ".to_string() + &"(".repeat(600) + "1" + &")".repeat(600)).into_bytes(),
    );
    s.push(
        ("create collection c (f ".to_string() + &"[".repeat(65) + "int" + &"]".repeat(65) + ")")
            .into_bytes(),
    );
    // The repository's texts: `tools/fennecql_corpus.py --list`, which the
    // make target writes beside the build, `{"text": ..., "from": ...}` a
    // line.
    if let Ok(lines) = std::fs::read_to_string(seeds_dir().join("fennecql.jsonl")) {
        for line in lines.lines() {
            if let Ok(Value::Object(members)) = fennec_core::json::parse_json(line) {
                for (k, v) in members {
                    if let ("text", Value::Text(t)) = (k.as_str(), v) {
                        s.push(t.into_bytes());
                    }
                }
            }
        }
    }
    s
}

fn fennecql(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let _ = fennec_ql::parse(&text);
    let _ = fennec_ql::parse_exact(&text);
}

// ---------------------------------------------------------------- codec

const CODEC_TOKENS: &[&[u8]] = &[
    &[0],
    &[1, 1],
    &[
        2, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
    ],
    &[3, 0, 0, 0, 0, 0, 0, 0xf8, 0x7f],
    &[4, 3, b'a', b'b', b'c'],
    &[5, 2, 0xff, 0xfe],
    &[6, 2, 0, 0, 0x80, 0x3f, 0, 0, 0, 0x40],
    &[7, 1],
    &[7, 0xff, 0xff, 0xff, 0xff, 0x0f],
    &[8, 1, 0, 0x3c],
    &[9, 2],
    &[10, 8, 2, 1, 2, 0, 0, 0x80, 0x3f, 0, 0, 0x80, 0x3f],
    &[13, 1, 1, b'a'],
    &[15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
];

/// Values of every kind, each encoded: the codec's seeds.
fn codec_values() -> Vec<Value> {
    let obj =
        |m: Vec<(&str, Value)>| Value::Object(m.into_iter().map(|(k, v)| (k.into(), v)).collect());
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(-9_000_000_000),
        Value::Float(-0.0),
        Value::Float(f64::NAN),
        Value::Timestamp(1_700_000_000_000),
        Value::Text("hello world".into()),
        Value::Bytes(vec![0, 1, 0xff]),
        Value::Vector(vec![0.1, -0.2, 0.3]),
        Value::Sparse(30_522, vec![(0, 0.5), (7, -1.25), (30_521, 3.0)]),
        Value::List(vec![
            Value::Int(1),
            Value::Text("a".into()),
            Value::List(vec![]),
        ]),
        obj(vec![
            ("a", Value::Int(1)),
            (
                "b",
                obj(vec![(
                    "c",
                    Value::List(vec![Value::Null, Value::Float(2.5)]),
                )]),
            ),
        ]),
    ]
}

fn codec_seeds() -> Vec<Vec<u8>> {
    let mut s = Vec::new();
    let values = codec_values();
    for v in &values {
        let mut b = Vec::new();
        fennec_core::codec::encode_value(&mut b, v);
        s.push(b);
    }
    // A document's fields, as a store holds them.
    let mut doc = Vec::new();
    for v in &values {
        fennec_core::codec::encode_value(&mut doc, v);
    }
    s.push(doc);
    let mut point = Vec::new();
    fennec_core::codec::encode_value_as(
        &mut point,
        &fennec_core::codec::geo_value((13.4, 52.5)),
        Some(&DataType::Geo),
    );
    s.push(point);
    let mut deep = Vec::new();
    for _ in 0..64 {
        deep.extend_from_slice(&[fennec_core::codec::TAG_LIST, 1]);
    }
    deep.push(0);
    s.push(deep);
    for ty in [
        DataType::List(Box::new(DataType::List(Box::new(DataType::Int)))),
        DataType::Vector(768, VecPrec::F16),
        DataType::Sparse(30_522),
    ] {
        let mut b = Vec::new();
        fennec_core::codec::encode_type(&mut b, &ty);
        s.push(b);
    }
    s.extend(kept_seeds("codec"));
    s
}

/// The schemas a document's bytes are read by: one of each type, and one
/// with a dropped place.
fn codec_schemas() -> &'static [fennec_core::schema::Schema] {
    static SCHEMAS: OnceLock<Vec<fennec_core::schema::Schema>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let mut out = Vec::new();
        for text in [
            "create collection a (n int, f float, t text, b bytes, at timestamp, ok bool)",
            "create collection b (v vector<3>, h vector<3, f16>, s sparse<16>, loc geo)",
            "create collection c (meta json, xs [int], tags [text], ys [[float]], at [timestamp])",
        ] {
            if let Ok(fennec_core::query::Statement::CreateCollection { schema, .. }) =
                fennec_ql::parse_one(text)
            {
                out.push(schema);
            }
        }
        if let Some(s) = out.first() {
            let mut dropped = s.clone();
            dropped.dropped = vec![1, 3];
            out.push(dropped);
        }
        out
    })
}

/// A document as the bytes of what it holds: its id, its fields' count,
/// and each field's name and value as `encode_value` writes them, a float
/// by its bits. What a reclaim keeps it copies as its bytes stood, so a
/// document read after it is this one bit for bit; `==` cannot say so of
/// one holding a `NaN`, which equals nothing -- the codec's own document
/// seed puts one at a kept place -- while these bytes find a `NaN` kept
/// equal and one with another payload, or a value moved, apart.
fn doc_bits(doc: &fennec_core::value::Document) -> Vec<u8> {
    use fennec_core::codec::{encode_str, encode_value, put_uvarint};
    let mut out = Vec::new();
    put_uvarint(&mut out, doc.id);
    put_uvarint(&mut out, doc.fields.len() as u64);
    for (name, v) in &doc.fields {
        encode_str(&mut out, name);
        encode_value(&mut out, v);
    }
    out
}

fn codec(data: &[u8]) {
    use fennec_core::codec::*;
    let mut read = 0;
    let decoded = decode_value(data, &mut read);
    let mut skipped = 0;
    let skip = skip_value(data, &mut skipped);
    // The skip passes over what the decode reads: the same bytes, never a
    // refusal of what the decode took.
    if decoded.is_ok() {
        assert!(skip.is_ok(), "decoded, and the skip refused it: {skip:?}");
        assert_eq!(read, skipped, "the decode and the skip end apart");
    }
    if let Ok(v) = &decoded {
        let mut once = Vec::new();
        encode_value(&mut once, v);
        let again = decode_value(&once, &mut 0)
            .unwrap_or_else(|e| panic!("{v:?} written, then refused: {e}"));
        let mut twice = Vec::new();
        encode_value(&mut twice, &again);
        assert_eq!(once, twice, "{v:?} written, read and written again");
    }
    let _ = skip_field(data, &mut 0);
    let _ = decode_type(data, &mut 0);
    for path in ["a", "b.c", "a.b.c.d"] {
        let _ = decode_path(data, 0, path);
    }
    let mut reclaimed = Vec::new();
    for schema in codec_schemas() {
        let read = schema.read_doc(1, data);
        // What a compact reclaims of a dropped place reads as the payload
        // did, and leaves nothing more to reclaim; the walk a compact makes
        // first says so of every payload it changes.
        let looked = fennec_core::schema::reclaims(&schema.dropped, data);
        if fennec_core::schema::reclaim(&schema.dropped, data, &mut reclaimed) {
            assert!(looked, "reclaimed, and the walk said nothing would be");
            if let Ok(doc) = &read {
                let again = schema.read_doc(1, &reclaimed);
                assert_eq!(
                    again.as_ref().ok().map(doc_bits),
                    Some(doc_bits(doc)),
                    "reclaimed, read otherwise: {again:?}, read before as {doc:?}"
                );
            }
            assert!(!fennec_core::schema::reclaim(
                &schema.dropped,
                &reclaimed,
                &mut Vec::new()
            ));
            assert!(!fennec_core::schema::reclaims(&schema.dropped, &reclaimed));
        }
    }
}

// ---------------------------------------------------------------- image

const IMAGE_TOKENS: &[&[u8]] = &[
    b"FENECDB\x01",
    &[0x83],
    &[0x81],
    &[0x89],
    &[0x8b],
    &[0x8d],
    &[3],
    &[1],
    &[9],
    &[6],
    &[4],
    &[13],
];

/// What each seed file is written by: a script of steps run against a file
/// a server's way (`fs::open`), a step of several statements one block --
/// one record -- and each file kept as written and summed anew.
const SCRIPTS: &[&[&[&str]]] = &[
    &[
        &["create collection docs (title text @text, body text, n int @sorted, price float @hash, tags [text], meta json, at timestamp, email text nocase @unique, loc geo @geo, v vector<4> @hnsw)"],
        &["put docs {id: 1, title: 'hello world', body: 'a b c', n: 1, price: 1.5, tags: ['a', 'b'], meta: {a: {b: [1, 2.5, 'x', null, true]}}, at: '2024-01-01T00:00:00Z', email: 'A@x.com', loc: [13.4, 52.5], v: [0.1, 0.2, 0.3, 0.4]}"],
        &["put docs {id: 2, title: 'second row', n: 2, price: 2.5, tags: [], meta: {lang: 'tr'}, email: 'b@x.com', loc: [-73.9, 40.7], v: [0.4, 0.3, 0.2, 0.1]}"],
        &["put docs {id: 3, title: 'third', n: 3, email: 'c@x.com', v: [1, 0, 0, 0]}"],
        &["set docs {n: n + 10, title: 'changed'} where id = 1"],
        &["del docs where id = 2"],
    ],
    &[
        &["create collection s (k text @hash, w sparse<16> @inverted, h vector<3, f16> @hnsw, b bytes, d int, e timestamp @ttl(1h), @sorted(d, e desc), @unique(k, d))"],
        &["put s {k: 'a', w: '{1:0.5,5:1.5}/16', h: [0.5, 0.25, 1], d: 1, e: '2030-01-01T00:00:00Z'}"],
        &["put s {k: 'b', h: [1, 1, 1], d: 2}"],
        &["put s {k: 'c', d: 3}", "set s {d: d * 2} where k = 'a'", "del s where k = 'b'"],
    ],
    &[
        &["create collection u (a int @hash, b text, c [float])"],
        &["put u {a: 1, b: 'x', c: [1.5, 2.5]}"],
        &["put u {a: 2, b: 'y'}"],
        &["alter collection u add field d text"],
        &["alter collection u drop field b"],
        &["create statement mark($n int) public as set u {d: 'marked'} where a = $n"],
        &["put u {a: 3, d: 'z'}"],
        &["compact u"],
        &["put u {a: 4, c: [0.5]}"],
    ],
];

fn image_seeds() -> Vec<Vec<u8>> {
    let mut files = Vec::new();
    for (i, script) in SCRIPTS.iter().enumerate() {
        let path = work().join(format!("seed-{i}.fennec"));
        let _ = std::fs::remove_file(&path);
        let mut db = fennec_core::fs::open(&path).expect("a seed's file");
        for step in script.iter() {
            let stmts: Vec<Statement> = step
                .iter()
                .map(|text| {
                    fennec_ql::parse_one_for(&db, text).unwrap_or_else(|e| panic!("{text}: {e}"))
                })
                .collect();
            match stmts.as_slice() {
                [one] => {
                    db.execute(one).unwrap_or_else(|e| panic!("{step:?}: {e}"));
                }
                many => {
                    let block: Vec<(&Statement, &[Value])> =
                        many.iter().map(|s| (s, &[][..])).collect();
                    db.execute_block(&block)
                        .unwrap_or_else(|(i, e)| panic!("{}: {e}", step[i]));
                }
            }
        }
        files.push(db.snapshot());
        drop(db);
        files.push(std::fs::read(&path).expect("a seed's file"));
        let _ = std::fs::remove_file(&path);
    }
    // An image whose index of its frames is read in two shares on threads
    // by a mapped open (`store::adopt_shares`, past 16 384 documents), its
    // documents as small as they come so it fits the target's inputs.
    let mut db = Database::new();
    db.execute(&fennec_ql::parse_one("create collection t (n int)").expect("a create"))
        .expect("a create");
    let docs: Vec<String> = (0..16_400).map(|i| format!("{{n: {}}}", i % 100)).collect();
    let put = format!("put t [{}]", docs.join(", "));
    db.execute(&fennec_ql::parse_one(&put).expect("a put"))
        .expect("a put");
    files.push(db.snapshot());
    for f in ["language.fennec", "statements.fennec"] {
        if let Ok(b) = std::fs::read(repo().join("crates/fennec-core/tests").join(f)) {
            files.push(b);
        }
    }
    // A finding's input carries the byte that says how it was read.
    let mut seeds = each_way(&files, 2);
    seeds.extend(kept_seeds("image"));
    seeds
}

/// The collections of `db` read whole -- every document decoded, every
/// index an open leaves for the first read built -- and a page of each as
/// a server writes it, JSON from the stored rows.
fn read_all(db: &Database) {
    for (c, f, k) in db.unbuilt_indexes(&[]) {
        let _ = db.warm_index(&c, &f, k);
    }
    for name in db.collection_names() {
        if let Ok(c) = db.collection(&name) {
            for id in c.store.iter_ids().take(10_000) {
                let _ = c.store.read(&c.schema, id);
            }
        }
        let plain =
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if let Ok(stmt) = fennec_ql::parse_one(&format!("get {name} limit 100")) {
            if plain {
                let _ = db.query(&stmt, &[]);
                let _ = db.query_json(&stmt, &[], &mut String::new());
            }
        }
    }
}

/// The time every database of the `image` target reads as now, in
/// milliseconds: 2026-01-01T00:00:00Z. A read of a collection whose rows
/// expire (`@ttl`, seed `s`'s `e`) asks the clock for its cutoff
/// (`Database::alive`), and the system's moves between runs and between
/// processes: a row alive in one run and expired in the next reached
/// other code, the corpus a process's own -- what a count of runs must
/// never be (docs/design/conventions.md#fuzzing). Before seed `s`'s
/// `2030-01-01` and after the epoch, so its row is read alive and a
/// mutated time on either side of it is met.
const CLOCK: i64 = 1_767_225_600_000;

fn image(data: &[u8]) {
    let Some((&how, bytes)) = data.split_first() else {
        return;
    };
    let mut file = bytes.to_vec();
    // Summed anew, the open reads what a record holds: as written, it stops
    // at the first record whose sum fails, the way damage is met.
    if how % 2 == 1 {
        fennec_core::engine::resum_all(&mut file);
    }
    let _ = fennec_core::engine::check_records(&file);
    let mut db = Database::new();
    db.set_clock(Some(CLOCK));
    if db.load(&file).is_ok() {
        read_all(&db);
    }
    // The open a server runs (`fs::open_serving`): mapped, the documents
    // read where they lie in the file -- an image's data record taken from
    // the index it wrote of its frames (`Store::adopt_index`), any other
    // replayed in place (`replay_mapped`) -- and the vectors it left out
    // of the graphs linked after, as a server links them beside its
    // queries.
    let mut served = Database::new();
    served.set_clock(Some(CLOCK));
    served.defer_linking();
    let base: fennec_core::store::Base = std::sync::Arc::new(file.clone());
    if served.load_mapped(base).is_ok() {
        served.link_pending(usize::MAX);
        read_all(&served);
    }
    let magic = fennec_core::engine::MAGIC.len();
    if file.len() > magic && file[..magic] == fennec_core::engine::MAGIC[..] {
        let mut replica = Database::new();
        replica.set_clock(Some(CLOCK));
        if replica.apply(&file[magic..]).is_ok() {
            read_all(&replica);
        }
    }
}

// --------------------------------------------------------------- sqlite

const SQLITE_TOKENS: &[&[u8]] = &[
    b"SQLite format 3\0",
    &[0x0d],
    &[0x05],
    &[0x0a],
    &[0x02],
    &[0x02, 0x00],
    &[0x00, 0x01],
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    b"CREATE TABLE t (",
    b"INTEGER PRIMARY KEY",
    b" COLLATE NOCASE",
    b"\xc2\xa0",
    b"WITHOUT ROWID",
    b" AS (a * 2) VIRTUAL",
    b" GENERATED ALWAYS AS (a) STORED",
    b" DEFAULT ",
    b" PRIMARY KEY DESC",
    b"\"\"",
    b"-- ",
    b"/* ",
    b" */",
    b"x'00ff'",
    b" DEFAULT (CAST(",
    b" AS TEXT))",
    b" DEFAULT (-(-",
    b"0x100000000",
];

/// The tables the target opens: `t` and `u`, and those of the seeds SQLite
/// wrote for `fennec-import`'s tests (`tests/sqlite-files/`), so their
/// schemas are read and not only found.
const SQLITE_TABLES: &[&str] = &[
    "t",
    "u",
    "quotes",
    "comments",
    "empty",
    "my (table",
    "desc_pk",
    "named_pk",
    "Ünits",
];

fn sqlite_seeds() -> Vec<Vec<u8>> {
    kept_seeds("sqlite")
}

fn sqlite(data: &[u8]) {
    use fennec_import::Source;
    let path = work().join("input.sqlite");
    if std::fs::write(&path, data).is_err() {
        return;
    }
    for &table in SQLITE_TABLES {
        if let Ok(mut r) = fennec_import::sqlite::Reader::open_sampled(&path, table, 16) {
            let _ = r.columns();
            let _ = r.left_out();
            for _ in 0..100_000 {
                match r.next_row() {
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
        }
        let _ = fennec_import::sqlite::count_rows(&path, table);
    }
}

// ---------------------------------------------------------- certificate

/// fennec-tls's walk, the file itself: fennec-tls is rustls and ring, which
/// the fuzzer, taking nothing from outside the workspace, does not link.
/// The file holds std alone for this (its own words say so).
#[path = "../../fennec-tls/src/end_point.rs"]
mod end_point;

const CERTIFICATE_TOKENS: &[&[u8]] = &[
    &[0x30, 0x82],
    &[0x30, 0x81],
    &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
    &[0x30, 0x80],
    &[0x05, 0x00],
    &[0xa0],
    &[0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01],
    &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03],
    &[0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02],
    &[0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a],
    &[0x06, 0x03, 0x2b, 0x65, 0x70],
];

/// A DER element, its length written short or long as its size asks.
fn element(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match contents.len() {
        n @ 0..=0x7f => out.push(n as u8),
        n @ 0x80..=0xff => out.extend_from_slice(&[0x81, n as u8]),
        n => out.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(contents);
    out
}

/// The certificates fennec-wire's tests serve and trust
/// (`crates/fennec-wire/tests/certs`), and a certificate's shape -- the
/// parts the walk passes over empty -- for every signature algorithm it
/// tells apart: RSA's with each hash, ECDSA's, RSASSA-PSS's with and
/// without its parameters' hash, and Ed25519's, which has no single hash.
fn certificate_seeds() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for name in ["ca.pem", "other-ca.pem", "server.pem"] {
        let path = repo().join("crates/fennec-wire/tests/certs").join(name);
        let Ok(pem) = std::fs::read_to_string(path) else {
            continue;
        };
        for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
            let body = block.split("-----END").next().unwrap_or("");
            if let Some(der) = fennec_http::crypto::b64_decode(body.trim()) {
                out.push(der);
            }
        }
    }
    let oid = |bytes: &[u8]| element(0x06, bytes);
    let shaped = |alg: &[u8]| {
        let mut body = element(0x30, &[]);
        body.extend_from_slice(&element(0x30, alg));
        body.extend_from_slice(&element(0x03, &[0]));
        element(0x30, &body)
    };
    const RSA: [u8; 8] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01];
    const ECDSA: [u8; 6] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04];
    const NIST: [u8; 8] = [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02];
    for last in [4, 5, 0x0b, 0x0c, 0x0d, 0x0e] {
        let mut alg = oid(&[&RSA[..], &[last]].concat());
        alg.extend_from_slice(&element(0x05, &[]));
        out.push(shaped(&alg));
    }
    out.push(shaped(&oid(&[&ECDSA[..], &[0x01]].concat())));
    for last in 1..=4 {
        out.push(shaped(&oid(&[&ECDSA[..], &[0x03, last]].concat())));
    }
    let pss = |params: &[u8]| {
        let mut alg = oid(&[&RSA[..], &[0x0a]].concat());
        alg.extend_from_slice(&element(0x30, params));
        shaped(&alg)
    };
    out.push(pss(&[]));
    out.push(pss(&element(
        0xa0,
        &element(0x30, &oid(&[0x2b, 0x0e, 0x03, 0x02, 0x1a])),
    )));
    for last in 1..=4 {
        let hash = oid(&[&NIST[..], &[last]].concat());
        out.push(pss(&element(0xa0, &element(0x30, &hash))));
    }
    out.push(shaped(&oid(&[0x2b, 0x65, 0x70])));
    out
}

fn certificate(data: &[u8]) {
    // Its answer kept, so no optimizer reads the walk as work for nothing.
    let _ = std::hint::black_box(end_point::hash_of(data));
}

// --------------------------------------------------------------- stream

const STREAM_TOKENS: &[&[u8]] = &[
    b"HTTP/1.1 200 OK\r\n",
    b"HTTP/1.1 401 Unauthorized\r\n",
    b"Content-Length: ",
    b"Content-Type: application/x-fennec-replication\r\n",
    b"\r\n",
    b"\r\n\r\n",
    b"H",
    b"I",
    b"W",
    b"K",
    b"E",
    b"N",
    b"L",
    b"G",
    &[0x01],
    &[0, 0, 0, 0, 0, 0, 0, 0],
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    // 1 MiB, the bound of a message but an image or writes; 1 TiB, theirs;
    // 2^61, a count of writes whose times wrap past 2^64 bytes.
    &[0, 0, 0x10, 0, 0, 0, 0, 0],
    &[0, 0, 0, 0, 0, 1, 0, 0],
    &[0, 0, 0, 0, 0, 0, 0, 0x20],
];

/// A message as a stream frames it: its kind, its length, its body.
fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&(body.len() as u64).to_le_bytes());
    m.extend_from_slice(body);
    m
}

/// A primary's answer and a node's, as their streams write them, and a few
/// a stand-in might send: a refusal, a count of writes past its message, a
/// head with no end. The first byte says which reader reads it -- even a
/// replica's, odd a node's archiver's -- and how many bytes a read takes.
fn stream_seeds() -> Vec<Vec<u8>> {
    let head: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-fennec-replication\r\n\
Cache-Control: no-cache, no-transform\r\nConnection: close\r\n\r\n";
    let mut db = Database::new();
    for q in ["create collection c (x int @hash)", "put c {x: 1}"] {
        db.execute(&fennec_ql::parse_one(q).expect("a statement"))
            .expect("a statement");
    }
    let image = db.snapshot();
    // Records as a feed holds them: an image's, after its magic.
    let records = image[fennec_core::engine::MAGIC.len()..].to_vec();
    let lineage = History {
        lineage: vec![((7 << 32) | 1, 0), ((7 << 32) | 9, 2)],
        following: false,
    }
    .encode();
    let u64s = |v: &[u64]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
    let hello = |image: bool| {
        let mut b = vec![1, image as u8];
        b.extend(u64s(&[2, 2, 1_700_000_000_000]));
        b.extend_from_slice(&lineage);
        b
    };
    let mut writes = u64s(&[3, 2, 1_700_000_000_001, 1_700_000_000_002]);
    writes.extend_from_slice(&records);
    let alive = u64s(&[3, 3, 1_700_000_000_003]);
    let answer = |messages: &[Vec<u8>]| {
        let mut a = head.to_vec();
        for m in messages {
            a.extend_from_slice(m);
        }
        a
    };
    // The same image and writes as a version before checksums wrote them,
    // as a primary sends them to a follower that does not say `sums=1`: a
    // record without a sum passes `check_records` whatever its bytes, so a
    // mutation of one reaches the image's load and its adoption by the
    // copy -- `repoint`'s walk, `rebind` -- where one of a checked image's
    // stops at its sum.
    let plain = fennec_core::engine::plain_image(&image).expect("the image's records");
    let mut plain_writes = u64s(&[3, 2, 1_700_000_000_001, 1_700_000_000_002]);
    plain_writes
        .extend_from_slice(&fennec_core::engine::plain_records(&records).expect("its records"));
    let replica = [
        answer(&[
            framed(b'H', &hello(true)),
            framed(b'I', &image),
            framed(b'W', &writes),
            framed(b'K', &alive),
            framed(b'E', b"the database is closing"),
        ]),
        answer(&[
            framed(b'H', &hello(true)),
            framed(b'I', &plain),
            framed(b'W', &plain_writes),
            framed(b'K', &alive),
        ]),
        answer(&[framed(b'H', &hello(false)), framed(b'W', &writes)]),
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 36\r\n\r\ninvalid or missing replication token"
            .to_vec(),
        answer(&[
            framed(b'H', &hello(false)),
            framed(b'W', &[u64s(&[3, 1 << 61]), vec![0; 16]].concat()),
        ]),
        // Writes of a terabyte announced, a few bytes of them sent.
        [
            answer(&[framed(b'H', &hello(false))]),
            vec![b'W'],
            u64s(&[1 << 40]),
            vec![0; 64],
        ]
        .concat(),
        b"HTTP/1.1 200 OK\r\nX-Endless: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(),
    ];
    let named = |body: &[u8]| [&[4u8][..], b"acme", body].concat();
    let node = [answer(&[
        framed(b'N', &[1]),
        framed(b'H', &named(&hello(true))),
        framed(b'I', &named(&image)),
        framed(b'W', &named(&writes)),
        framed(b'L', &named(&lineage)),
        framed(b'G', &named(&u64s(&[1_700_000_000_004]))),
        framed(b'K', &u64s(&[1_700_000_000_005])),
        framed(b'E', b"the node is shutting down"),
    ])];
    let mut s = Vec::new();
    for (inputs, first) in [(&replica[..], 0u8), (&node[..], 1)] {
        for i in inputs {
            for way in [first, first + 2, first + 6] {
                s.push([&[way][..], i].concat());
            }
        }
    }
    s
}

/// The bytes after the first read as a stand-in's answer: by a replica's
/// reader or a node archiver's (the first byte's lowest bit), whole or a
/// few bytes a read (the rest of it), as a slow peer's socket hands them.
fn stream(data: &[u8]) {
    let Some((&how, bytes)) = data.split_first() else {
        return;
    };
    let node = how & 1 == 1;
    match (how >> 1) % 4 {
        0 => answer(&mut std::io::BufReader::new(bytes), node),
        n => {
            let step = 1 + (how as usize >> 3) % 7 * n as usize;
            let mut r = std::io::BufReader::with_capacity(step, Trickle { bytes, step });
            answer(&mut r, node)
        }
    }
}

fn answer<R: BufRead>(r: &mut R, node: bool) {
    if node {
        let _ = fennec_http::node_archive::read_answer(r);
        return;
    }
    // A copy as a server's is: mapped, its file a sink's that hands the
    // rewritten bytes back as the mapping -- so an image the follower's
    // path accepts is adopted as a replica adopts one, its stores pointed
    // at the bytes (`repoint`'s walk) and its statements bound (`rebind`).
    let mut copy = Database::with_sink(Box::new(Remapped(None)));
    copy.set_clock(Some(CLOCK));
    let empty: fennec_core::store::Base = std::sync::Arc::new(Database::new().snapshot());
    if copy.load_mapped(empty).is_err() {
        return;
    }
    let (_, _) = fennec_http::replication::read_answer(r, Some(&mut copy));
    read_all(&copy);
}

/// A file's sink in memory: a rewrite's bytes kept, and handed back as the
/// file mapped, as a file's sink hands back its mapping after one.
struct Remapped(Option<fennec_core::store::Base>);

impl fennec_core::engine::Sink for Remapped {
    fn append(&mut self, _: &[u8]) -> fennec_core::error::Result<()> {
        Ok(())
    }

    fn rewrite(&mut self, bytes: &[u8]) -> fennec_core::error::Result<()> {
        self.0 = Some(std::sync::Arc::new(bytes.to_vec()));
        Ok(())
    }

    fn remapped(&mut self) -> Option<fennec_core::store::Base> {
        self.0.take()
    }
}
