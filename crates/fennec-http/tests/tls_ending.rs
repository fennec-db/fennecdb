//! The one shutdown's end of the TLS connections (`conn::end_all`) with a
//! handshake under way. A binary of its own, and its one test: the shutdown
//! is the process's (`conn::ending`, never undone), and would end any other
//! test's connections -- a second `#[test]` here would meet it.
#![cfg(feature = "tls")]

use fennec_core::engine::Database;
use fennec_http::conn::{self, Tls};
use fennec_http::{Config, Server};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// A bound only a failure meets: every wait here is for an event, the
/// shutdown's among them, so none stands on `conn::END_GRACE`.
const FAILED: Duration = Duration::from_secs(60);

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

fn connect(addr: impl ToSocketAddrs) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(FAILED)).unwrap();
    s
}

/// A handshake over `s` by a client trusting the test CA alone.
fn dial(s: TcpStream) -> Result<fennec_tls::TlsStream, fennec_tls::Error> {
    fennec_tls::Client::trusting_pem_file(&certs().join("ca.pem"))
        .unwrap()
        .alpn(&[b"http/1.1"])
        .dial(s, "localhost")
}

/// A connection whose client has its handshake done while the server's is
/// not is waited for by the one shutdown, and ends with its `close_notify`
/// as a listed connection waiting for its request does; one that has sent
/// nothing is not waited for, and a handshake that would begin after is
/// refused before the server sends a byte. TLS 1.3 ends the client's
/// handshake at the server's `Finished`, the server's at the client's,
/// which a relay holds here until the listed connection has read its end:
/// the shutdown, given a bound only a failure meets, cannot return before
/// the relay lets it go. Before, the connection in its handshake was
/// neither listed nor waited for, and the exit cut it with a TCP end
/// alone (docs/design/http.md#tls).
#[test]
fn the_shutdown_waits_for_a_handshake_under_way_and_lets_none_begin() {
    // A handshake's bound past the shutdown's: the silent connection's peek
    // outlasts it, and would hold it to its bound were it waited for.
    let tls = Tls::from_pem_files(&certs().join("server.pem"), &certs().join("server.key"))
        .unwrap()
        .handshake_within(2 * FAILED);
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        tls: Some(Arc::new(tls)),
        ..Config::default()
    };
    let server = Arc::new(Server::new(Arc::new(RwLock::new(Database::new())), cfg));
    let listener = server.bind().unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(&server);
    std::thread::spawn(move || {
        let _ = serving.serve_on(listener);
    });

    // A connection that has sent nothing: a scan, a balancer's made ahead.
    let silent = connect(("127.0.0.1", port));
    // A listed connection, a request answered over it, waiting for its next.
    let mut listed = BufReader::new(dial(connect(("127.0.0.1", port))).unwrap());
    let q = r#"{"query":"collections"}"#;
    let ask = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{q}",
        q.len()
    );
    listed.get_mut().write_all(ask.as_bytes()).unwrap();
    let head = answered(&mut listed);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    // A connection in its handshake: the client's done, the server's
    // waiting for the client's `Finished`, which the relay holds.
    let (relay, held) = finished_held(port);
    let mut shaking = dial(connect(relay)).unwrap();
    let held = held.recv().expect("the relay");

    let (ended, end) = mpsc::channel();
    std::thread::spawn(move || {
        let began = Instant::now();
        conn::end_all(FAILED);
        let _ = ended.send(began.elapsed());
    });
    // The listed connection's end: its read side shut, its thread sent
    // its `close_notify`.
    let read = listed.read_to_end(&mut Vec::new());
    assert!(read.is_ok(), "the listed connection: {read:?}");
    // The shutdown waits for the handshake the relay holds: it cannot
    // return while that is counted, however long this waits, and a shutdown
    // that does not wait returns within a few milliseconds of the listed
    // connection's end (its poll is 2 ms) -- the `close_notify` the relayed
    // connection is given at its listing comes whether or not the shutdown
    // waited, so only this wait tells the two apart. A pass stands on no
    // time; a second bounds how soon a shutdown that did not wait is caught.
    assert_eq!(
        end.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the shutdown returned with a handshake under way"
    );
    held.pass_on();
    let read = shaking.read_to_end(&mut Vec::new());
    assert!(read.is_ok(), "the connection in its handshake: {read:?}");
    // Then it returns, both gone, under its bound: the silent connection,
    // its peek outlasting that, was not waited for.
    let took = end.recv().expect("the shutdown");
    assert!(took < FAILED, "the shutdown waited {took:?}");

    // A first byte after the shutdown, and a connection made after it: no
    // handshake begins.
    let refused = dial(silent);
    assert!(refused.is_err(), "the silent connection's handshake");
    let refused = dial(connect(("127.0.0.1", port)));
    assert!(refused.is_err(), "a handshake after the shutdown");
}

/// An answer's head read off `r`, and its body by `Content-Length`.
fn answered(r: &mut impl BufRead) -> String {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert!(r.read_line(&mut line).unwrap() > 0, "closed in: {head}");
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .map_or(0, |n| n.trim().parse().unwrap());
    r.read_exact(&mut vec![0; len]).unwrap();
    head
}

/// A relay to `port` for one connection: it passes on everything the server
/// sends, and of the client's bytes its first record -- its `ClientHello`
/// -- then hands over both sockets, holding the rest, a TLS 1.3 client's
/// `Finished`, until [`Held::pass_on`]. The client's handshake is then
/// done, the server's not.
fn finished_held(port: u16) -> (SocketAddr, mpsc::Receiver<Held>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (hand, held) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut client, _) = listener.accept().expect("the client");
        let mut server = TcpStream::connect(("127.0.0.1", port)).expect("the server");
        let (mut from, mut to) = (server.try_clone().unwrap(), client.try_clone().unwrap());
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut from, &mut to);
            let _ = to.shutdown(Shutdown::Write);
        });
        // A record's head is five bytes, its length the last two.
        let mut head = [0u8; 5];
        client
            .read_exact(&mut head)
            .expect("the ClientHello's head");
        let mut hello = vec![0u8; u16::from_be_bytes([head[3], head[4]]) as usize];
        client.read_exact(&mut hello).expect("the ClientHello");
        server.write_all(&head).unwrap();
        server.write_all(&hello).unwrap();
        let _ = hand.send(Held { client, server });
    });
    (addr, held)
}

/// A relayed connection's client bytes held after its `ClientHello`.
struct Held {
    client: TcpStream,
    server: TcpStream,
}

impl Held {
    /// What the client sent since, and all it sends after, passed on.
    fn pass_on(mut self) {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut self.client, &mut self.server);
            let _ = self.server.shutdown(Shutdown::Write);
        });
    }
}
