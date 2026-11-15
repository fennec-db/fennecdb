//! A `fennec-server` process for a test, and an HTTP client to talk to it.
//!
//! What these tests hold -- a shutdown's sync, a crash's file, a replica's
//! promotion -- cannot be exercised inside the test's own process: a
//! shutdown ends in `process::exit` and its flag is the whole process's.
//! So the binary runs, on a port it picks, and is asked over HTTP as any
//! client asks it.
//!
//! It is stopped as a supervisor stops it: `SIGTERM`, and on Windows, which
//! has no signals, Ctrl+Break to the process group each server is started
//! in ([`own_group`], docs/design/windows.md, decision 4); killed with
//! `SIGKILL`, on Windows `TerminateProcess`.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// libc's `kill`, declared as the server declares `signal`: no dependency.
#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;

/// Asks `child` to stop as a supervisor does: `SIGTERM`, on Windows
/// Ctrl+Break to its process group, which [`own_group`] made.
pub fn ask_to_stop(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        kill(child.id() as i32, SIGTERM);
    }
    #[cfg(windows)]
    windows::ctrl_break(child);
}

/// `cmd` started in a process group of its own, on Windows, where the
/// group is what Ctrl+Break is sent to ([`ask_to_stop`]): sent to the
/// harness's own group, it would stop the harness and every server beside
/// the one meant. Unix sends a signal to a process, and changes nothing.
pub fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    windows::own_group(cmd);
    cmd
}

/// Windows' half of stopping a server: Ctrl+Break, and the console it is
/// sent through.
#[cfg(windows)]
pub mod windows {
    use std::process::{Child, Command};
    use std::sync::OnceLock;

    const CTRL_BREAK_EVENT: u32 = 1;
    /// winbase.h's: the child heads a process group of its own, its id the
    /// child's.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    #[link(name = "kernel32")]
    extern "system" {
        fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
        fn GetConsoleProcessList(pids: *mut u32, len: u32) -> u32;
        fn AllocConsole() -> i32;
        fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
        fn SetStdHandle(which: u32, handle: *mut std::ffi::c_void) -> i32;
    }

    /// Whether this process had a console of its own before the first
    /// server started, or [`console`] made one: what the runner's step
    /// gives a test is the design's question (3), which
    /// `tests/windows.rs` prints.
    static HAD_ONE: OnceLock<bool> = OnceLock::new();

    /// The processes attached to this process's console, none where it has
    /// none.
    pub fn console_processes() -> Vec<u32> {
        let mut pids = vec![0u32; 64];
        loop {
            // SAFETY: a buffer of `pids.len()` ids.
            let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
            if (n as usize) <= pids.len() {
                pids.truncate(n as usize);
                return pids;
            }
            pids.resize(n as usize, 0);
        }
    }

    /// Makes sure this process has a console its servers share, which
    /// `GenerateConsoleCtrlEvent` asks: it sends to processes of the
    /// caller's console alone. A process without one is given one
    /// (`AllocConsole`), its standard handles kept as they were -- cargo's
    /// pipes, where its output goes. Whether one was there: [`had_one`].
    pub fn console() -> bool {
        *HAD_ONE.get_or_init(|| {
            if !console_processes().is_empty() {
                return true;
            }
            // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE.
            let which = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
            // SAFETY: the process's own standard handles, read and put back.
            unsafe {
                let kept = which.map(|w| GetStdHandle(w));
                assert_ne!(AllocConsole(), 0, "{}", std::io::Error::last_os_error());
                for (w, h) in which.iter().zip(kept) {
                    SetStdHandle(*w, h);
                }
            }
            false
        })
    }

    /// Whether the harness had a console before its first server: `None`
    /// until one started.
    pub fn had_one() -> Option<bool> {
        HAD_ONE.get().copied()
    }

    pub fn own_group(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        console();
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    /// Ctrl+Break to `child`'s group, as `kill` sends `SIGTERM`: nothing to
    /// a child that has ended. The call refuses a group no process of the
    /// console is in, so a refusal is the test's error unless the child
    /// ended meanwhile; its id names no other process while `Child` holds
    /// its handle.
    pub fn ctrl_break(child: &mut Child) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        // SAFETY: no pointers; the group is the child's own, made by
        // `own_group`.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
        if sent == 0 {
            let e = std::io::Error::last_os_error();
            if !matches!(child.try_wait(), Ok(Some(_))) {
                panic!("Ctrl+Break to {} was refused: {e}", child.id());
            }
        }
    }
}

/// A fresh path under a directory of this test process's own.
pub fn tmp(tag: &str, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fennec-server-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&path);
    path
}

pub struct Server {
    pub child: Child,
    pub port: u16,
    pub log: Arc<Mutex<String>>,
    /// The thread reading stderr into `log`, which ends as the pipe does.
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the binary with `args`, on a port of its own unless `args` name
/// one with `--http`, and waits for its HTTP listener; everything it logs
/// is kept.
pub fn start(args: &[&str]) -> Server {
    try_start(args).unwrap_or_else(|log| panic!("fennec-server ended before it listened:\n{log}"))
}

/// [`start`], or what the server logged before it ended without
/// listening.
pub fn try_start(args: &[&str]) -> Result<Server, String> {
    try_start_env(args, &[])
}

/// [`start`] with variables of its own in the server's environment.
pub fn start_env(args: &[&str], env: &[(&str, &str)]) -> Server {
    try_start_env(args, env)
        .unwrap_or_else(|log| panic!("fennec-server ended before it listened:\n{log}"))
}

/// [`try_start`] with variables of its own in the server's environment.
pub fn try_start_env(args: &[&str], env: &[(&str, &str)]) -> Result<Server, String> {
    try_start_with(args, env, Stdio::inherit())
}

/// [`start`] with a pipe on the server's stdin, its write end in
/// `child.stdin`: dropped, the server reads the end of its input.
pub fn start_piped(args: &[&str]) -> Server {
    try_start_with(args, &[], Stdio::piped())
        .unwrap_or_else(|log| panic!("fennec-server ended before it listened:\n{log}"))
}

/// `cmd` without the token variables this process inherited
/// (`FENNEC_*_TOKEN`): a server a test starts reads only the tokens the
/// test gives it. One exported in a developer's shell stood in for a flag a
/// test left out -- a short one there refused at the start for a flag the
/// test never named.
pub fn without_tokens(cmd: &mut Command) -> &mut Command {
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str() {
            if name.starts_with("FENNEC_") && name.ends_with("_TOKEN") {
                cmd.env_remove(name);
            }
        }
    }
    cmd
}

/// [`try_start_env`] with `stdin` as the server's stdin.
pub fn try_start_with(args: &[&str], env: &[(&str, &str)], stdin: Stdio) -> Result<Server, String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fennec-server"));
    own_group(&mut cmd);
    without_tokens(&mut cmd).envs(env.iter().copied());
    if !args.contains(&"--http") {
        cmd.args(["--http", "127.0.0.1:0"]);
    }
    let mut child = cmd
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start fennec-server");
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let log = Arc::new(Mutex::new(String::new()));
    // "fennec-http 0.1.6 listening on: http://127.0.0.1:54321  [no auth]"
    let mut port = None;
    while port.is_none() {
        let mut line = String::new();
        if err.read_line(&mut line).unwrap_or(0) == 0 {
            let _ = child.wait();
            let log = log.lock().unwrap().clone();
            return Err(log);
        }
        if line.contains("listening on: http://") || line.contains("listening on: https://") {
            if let Some(rest) = line.rsplit_once("://").map(|(_, r)| r) {
                let addr: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                port = addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok());
            }
        }
        log.lock().unwrap().push_str(&line);
    }
    let sink = Arc::clone(&log);
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        while err.read_line(&mut line).unwrap_or(0) > 0 {
            sink.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    Ok(Server {
        child,
        port: port.unwrap(),
        log,
        reader: Some(reader),
    })
}

impl Server {
    pub fn http(&self) -> Http {
        Http::open(self.port)
    }

    pub fn logged(&self, what: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.log.lock().unwrap().contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Sends `sig`, waits for the process to end; its exit code and
    /// everything it logged. On Windows `SIGTERM` is Ctrl+Break to the
    /// server's process group and `SIGKILL` is `TerminateProcess`, a crash.
    pub fn signal(mut self, sig: i32) -> (i32, String) {
        #[cfg(unix)]
        unsafe {
            kill(self.child.id() as i32, sig)
        };
        #[cfg(windows)]
        match sig {
            SIGTERM => windows::ctrl_break(&mut self.child),
            SIGKILL => {
                let _ = self.child.kill();
            }
            _ => panic!("signal {sig}: Windows has Ctrl+Break and TerminateProcess alone"),
        }
        let status = self.child.wait().expect("could not wait for the process");
        // The reader thread takes the last lines as the pipe closes.
        std::thread::sleep(Duration::from_millis(100));
        let log = self.log.lock().unwrap().clone();
        (status.code().unwrap_or(-1), log)
    }

    pub fn terminate(self) -> (i32, String) {
        self.signal(SIGTERM)
    }

    /// Waits up to `within` for the process to end on its own: its exit
    /// code and everything it logged, or `None` if it was still running.
    pub fn exited(&mut self, within: Duration) -> Option<(i32, String)> {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("could not wait for the process")
            {
                break status;
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Every line is in once the reader has met the pipe's end -- unless
        // a server another test spawned meanwhile inherited the pipe's write
        // end (macOS marks a new pipe close-on-exec after making it, not
        // with it), which holds the pipe open past this one's exit: waited
        // for within the same bound, the lines then those read so far.
        while self.reader.as_ref().is_some_and(|r| !r.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(reader) = self.reader.take_if(|r| r.is_finished()) {
            let _ = reader.join();
        }
        let log = self.log.lock().unwrap().clone();
        Some((status.code().unwrap_or(-1), log))
    }
}

/// A keep-alive HTTP/1.1 connection.
pub struct Http {
    r: BufReader<TcpStream>,
    w: TcpStream,
    /// Sent with every request: `Authorization: Bearer ...`, a tenant's
    /// prefix is the target's.
    pub token: Option<String>,
}

pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl Http {
    pub fn open(port: u16) -> Http {
        let s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        Http {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
            token: None,
        }
    }

    pub fn with_token(mut self, token: &str) -> Http {
        self.token = Some(token.to_string());
        self
    }

    /// One request; `None` when the server closed the connection.
    pub fn try_ask(
        &mut self,
        method: &str,
        target: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Option<Answer> {
        let mut req = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\n");
        if let Some(t) = &self.token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        self.w.write_all(req.as_bytes()).ok()?;
        let mut line = String::new();
        if self.r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let status = line.split_whitespace().nth(1)?.parse().ok()?;
        let mut len = 0;
        let mut out = Vec::new();
        loop {
            let mut h = String::new();
            self.r.read_line(&mut h).ok()?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().ok()?;
                }
                out.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let mut body = vec![0; len];
        self.r.read_exact(&mut body).ok()?;
        Some(Answer {
            status,
            headers: out,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    pub fn ask(&mut self, method: &str, target: &str, body: &str) -> Answer {
        self.try_ask(method, target, body, &[])
            .expect("the server closed the connection")
    }

    /// [`Http::ask`] with headers of its own.
    pub fn ask_with(
        &mut self,
        method: &str,
        target: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> Answer {
        self.try_ask(method, target, body, headers)
            .expect("the server closed the connection")
    }

    /// A statement through `POST <prefix>/query`: its body, or the status
    /// and body it was refused with.
    pub fn query_at(&mut self, prefix: &str, q: &str) -> Result<String, (u16, String)> {
        let body = format!("{{\"query\": {}}}", json_string(q));
        let a = self.ask("POST", &format!("{prefix}/query"), &body);
        match a.status {
            200 => Ok(a.body),
            s => Err((s, a.body)),
        }
    }

    pub fn query(&mut self, q: &str) -> Result<String, (u16, String)> {
        self.query_at("", q)
    }

    /// The statement, which must be answered.
    pub fn run(&mut self, q: &str) -> String {
        self.query(q).unwrap_or_else(|e| panic!("{q}: {e:?}"))
    }
}

pub fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The values of `field` in a JSON answer's rows, as text, in order.
pub fn column(body: &str, field: &str) -> Vec<String> {
    let key = format!("\"{field}\":");
    body.match_indices(&key)
        .map(|(i, _)| {
            let rest = &body[i + key.len()..];
            let rest = rest.trim_start();
            if let Some(s) = rest.strip_prefix('"') {
                s[..s.find('"').unwrap_or(s.len())].to_string()
            } else {
                rest.chars()
                    .take_while(|c| !matches!(c, ',' | '}' | ']'))
                    .collect::<String>()
                    .trim()
                    .to_string()
            }
        })
        .collect()
}

/// The `count` a `get ... count` answered.
pub fn count(body: &str) -> i64 {
    column(body, "count")
        .first()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no count in {body}"))
}

/// The exit code and stderr of a child that must end of its own accord,
/// within `within`; killed past it. Its stderr is read within the same
/// bound: a server another test spawned meanwhile can hold the pipe open.
pub fn ended(mut child: Child, within: Duration) -> (i32, String) {
    let mut err = child.stderr.take().unwrap();
    let log = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&log);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = err.read(&mut buf) {
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let deadline = Instant::now() + within;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the server did not end:\n{}", log.lock().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = log.lock().unwrap().clone();
    (status.code().unwrap_or(-1), log)
}

/// `fennec-server` with `args` as given, no `--http` added, stdin
/// `/dev/null` and stderr piped: for a start that must end of its own
/// accord ([`ended`]).
pub fn command(args: &[&str]) -> Child {
    command_env(args, &[])
}

/// [`command`] with variables of its own in the server's environment, the
/// token variables this process inherited taken out ([`without_tokens`]).
pub fn command_env(args: &[&str], env: &[(&str, &str)]) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fennec-server"));
    without_tokens(own_group(&mut cmd))
        .envs(env.iter().copied())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not start fennec-server")
}

/// Documents in `collection` of the file at `path`, opened on its own as
/// after a crash.
pub fn documents(path: &std::path::Path, collection: &str) -> usize {
    let db = fennec_core::fs::open(path).expect("could not reopen the file");
    db.stats()
        .iter()
        .find(|s| s.name == collection)
        .map(|s| s.documents)
        .unwrap_or(0)
}
