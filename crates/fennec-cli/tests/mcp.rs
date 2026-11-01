//! `fennec mcp` as a client starts it: a process speaking stdio, over a file
//! it holds the lock of, or in front of a server's endpoint. A binary built
//! without the `mcp` feature has no `fennec mcp` to test.
#![cfg(feature = "mcp")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_fennec");

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fennec-cli-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    for suffix in ["", ".lock", ".sync"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start(args: &[&str], env: &[(&str, &str)]) -> Session {
        let mut cmd = Command::new(BIN);
        cmd.arg("mcp")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            stdin,
            stdout,
        }
    }

    /// Sends a line and reads the answer's line.
    fn ask(&mut self, line: &str) -> String {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        let mut out = String::new();
        self.stdout.read_line(&mut out).unwrap();
        out
    }

    /// Closes stdin, as a client ends a session, and the exit status.
    fn end(self) -> (i32, String) {
        drop(self.stdin);
        let out = self.child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;

#[test]
fn a_session_over_a_file_holds_its_lock_and_leaves_its_writes_on_disk() {
    let path = scratch("notes.fennec");
    let p = path.to_str().unwrap();
    let mut s = Session::start(&["--file", p, "--memory", "memories"], &[]);
    let a = s.ask(INIT);
    assert!(a.contains("\"protocolVersion\":\"2025-11-25\""), "{a}");
    // A notification is answered with nothing: the next line is the call's.
    writeln!(
        s.stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    let a = s.ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remember","arguments":{"text":"the build uses cargo"}}}"#);
    assert!(a.contains("\"remembered\":1"), "{a}");

    // A second process over the file is refused while the session holds it.
    let second = Command::new(BIN)
        .args(["mcp", "--file", p])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    let why = String::from_utf8_lossy(&second.stderr);
    // Named: the session's own process, the shell's binary.
    let holder = format!("is open in another process (fennec, pid {})", s.child.id());
    assert!(why.contains(&holder), "{why}");
    // ... but one that only reads, and takes no lock, is let in.
    let mut ro = Session::start(&["--file", p, "--read-only", "--memory", "memories"], &[]);
    let a = ro.ask(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"recall","arguments":{"text":"build"}}}"#);
    assert!(a.contains("the build uses cargo"), "{a}");
    let a = ro.ask(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"remember","arguments":{"text":"x"}}}"#);
    assert!(
        a.contains("\"isError\":true") && a.contains("read-only"),
        "{a}"
    );
    assert_eq!(ro.end().0, 0);

    let (code, log) = s.end();
    assert_eq!(code, 0, "{log}");
    // stdout carried messages alone: the log went to stderr.
    assert!(log.contains("over stdio"), "{log}");
    // The memory is in the file, and the lock let go with the session.
    let mut again = Session::start(&["--file", p, "--memory", "memories"], &[]);
    let a = again.ask(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"recall","arguments":{}}}"#);
    assert!(a.contains("the build uses cargo"), "{a}");
    assert_eq!(again.end().0, 0);
}

#[test]
fn a_session_over_a_server_carries_the_token_from_the_environment() {
    let db = std::sync::Arc::new(std::sync::RwLock::new(fennec_core::engine::Database::new()));
    {
        let mut g = db.write().unwrap();
        for sql in [
            "create collection docs (body text @text)",
            "put docs [{body: \"tokens come from the environment\"}]",
        ] {
            g.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
        }
    }
    let server = fennec_http::Server::new(
        db,
        fennec_http::Config {
            addr: "127.0.0.1:0".into(),
            token: Some("s3cret".into()),
            mcp: Some(std::sync::Arc::new(fennec_http::mcp::Options::default())),
            ..fennec_http::Config::default()
        },
    );
    let listener = server.bind().unwrap();
    let url = format!("http://{}/_mcp", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    let mut s = Session::start(&["--url", &url], &[("FENNEC_TOKEN", "s3cret")]);
    assert!(s.ask(INIT).contains("\"serverInfo\""));
    let a = s.ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"collection":"docs","text":"environment"}}}"#);
    assert!(a.contains("**environment**"), "{a}");
    assert_eq!(s.end().0, 0);

    // A tool's option is the server's: the bridge refuses one.
    let out = Command::new(BIN)
        .args(["mcp", "--url", &url, "--write"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    // An https URL is refused with what to do instead.
    let out = Command::new(BIN)
        .args(["mcp", "--url", "https://db.example/_mcp"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("TLS"));
}

/// A local session lists the file's statements as tools of their own and
/// calls them under the lock -- a write in a block held to `--max-rows`
/// before it lands, put back past it -- and a read-only one calls the ones
/// that read, the ones that write no tools of it.
#[test]
fn a_session_over_a_file_calls_its_statements_as_tools() {
    let path = scratch("calls.fennec");
    let p = path.to_str().unwrap();
    let made = Command::new(BIN)
        .args([
            p,
            "-c",
            "create collection marks (n int); \
             create statement mark($n int) public scans as begin insert marks {n: $n}; \
             get marks select n end; \
             create statement marked() as get marks select n",
        ])
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let mut s = Session::start(&["--file", p, "--write", "--max-rows", "2"], &[]);
    s.ask(INIT);
    let a = s.ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    assert!(
        a.contains(r#""name":"call_mark""#) && a.contains(r#""name":"call_marked""#),
        "{a}"
    );
    let call = |id: i64, n: i64| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"call_mark","arguments":{{"n":{n}}}}}}}"#
        )
    };
    for n in [1, 2] {
        let a = s.ask(&call(2 + n, n));
        assert!(a.contains("\"isError\":false"), "{a}");
    }
    // A third row is past the bound: refused, and the insert put back.
    let a = s.ask(&call(5, 3));
    assert!(
        a.contains("\"isError\":true") && a.contains("--mcp-max-rows"),
        "{a}"
    );
    assert_eq!(s.end().0, 0);
    let mut ro = Session::start(&["--file", p, "--read-only"], &[]);
    let a = ro.ask(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"call_marked","arguments":{}}}"#);
    assert!(a.contains(r#"\"rows\":[{\"n\":1},{\"n\":2}]"#), "{a}");
    // Without --write, which a read-only session cannot take, a statement
    // that writes is no tool of it.
    let a = ro.ask(&call(7, 4));
    assert!(a.contains("Unknown tool: call_mark"), "{a}");
    assert_eq!(ro.end().0, 0);
}
