//! What the router answers a request it never got whole to the tenant's
//! node: its own 503, `Retry-After` and `retry_after` saying when to come
//! back and `Fennec-Unsent` that the request ran nowhere -- where it was a
//! 502, which says the node may have run it, so that a client could not
//! send an unkeyed write again without risking it twice. A request the node
//! took whole, or whose answer it began and broke off, stays a 502. Each
//! node here stands in, written by hand, so that what it takes and when it
//! goes is the test's to say.

use crate::upstream::until_fin_taken;
use fennec_shard::directory::{Directory, Node, State};
use fennec_shard::{Config, Router};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// A router placing tenant `acme` on node `n1` at `addr`, its upstream
/// timeout `timeout`.
fn router_to(addr: &str, timeout: Duration) -> String {
    fennec_http::audit::set_delay(0);
    let mut d = Directory::in_memory();
    let placed = Node {
        addr: addr.to_string(),
        token: "adm".into(),
        ..Node::default()
    };
    d.set_node("n1", placed).unwrap();
    d.place("acme", "n1", State::Active).unwrap();
    serve(Router::new(d, config(timeout)))
}

fn config(timeout: Duration) -> Config {
    Config {
        addr: "127.0.0.1:0".into(),
        upstream_timeout: timeout,
        ..Config::default()
    }
}

fn serve(router: std::sync::Arc<Router>) -> String {
    let listener = router.bind().unwrap();
    let front = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let _ = router.serve_on(listener);
    });
    front
}

/// An address nothing listens on: its connect is refused.
fn refusing() -> String {
    let gone = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = gone.local_addr().unwrap().to_string();
    drop(gone);
    addr
}

/// A listener that answers no connect, as a machine gone does: its queue
/// made short (`listen` again, which both systems take: 1, since macOS
/// reads 0 as the most it allows) and filled, so that the next SYN is
/// dropped. The connections that filled it, kept open.
fn deaf() -> (TcpListener, Vec<TcpStream>) {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn listen(fd: i32, backlog: i32) -> i32;
    }
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    // SAFETY: a listening socket's descriptor, open for the call.
    assert_eq!(unsafe { listen(l.as_raw_fd(), 1) }, 0);
    let at = l.local_addr().unwrap();
    let mut filled = Vec::new();
    while let Ok(s) = TcpStream::connect_timeout(&at, Duration::from_millis(500)) {
        filled.push(s);
        assert!(filled.len() < 64, "the queue never filled");
    }
    (l, filled)
}

/// The status, the head and the body of an unkeyed write -- the request a
/// 502 left a client unable to send again -- with `extra` headers, on a
/// connection of its own.
fn put(addr: &str, extra: &str) -> (u16, String, String) {
    send(addr, extra, r#"{"query":"put notes {title: \"t\"}"}"#)
}

/// [`put`] of `body`.
fn send(addr: &str, extra: &str, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let req = format!(
        "POST /t/acme/query HTTP/1.1\r\nHost: t\r\nConnection: close\r\n{extra}\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// The router's own 503 for a request that ran nowhere: when to come
/// back, in the header and the body, its mark and the router's.
fn told_unsent((status, head, body): &(u16, String, String)) {
    assert_eq!(*status, 503, "{head}\n{body}");
    assert_eq!(header(head, "retry-after"), Some("1"), "{head}");
    assert!(body.contains(r#""retry_after":1"#), "{body}");
    assert!(body.contains("ran nowhere"), "{body}");
    assert_eq!(header(head, "fennec-unsent"), Some("1"), "{head}");
    assert_eq!(header(head, "fennec-shard"), Some("1"), "{head}");
    // The router took the request: it is no connection it served nothing
    // on, which the entry would take for no router at all.
    assert_eq!(header(head, "fennec-unserved"), None, "{head}");
}

/// A 502: the node took the request whole, or began its answer, and may
/// have run it.
fn told_may_have_run((status, head, body): &(u16, String, String)) {
    assert_eq!(*status, 502, "{head}\n{body}");
    assert_eq!(header(head, "fennec-unsent"), None, "{head}");
    assert_eq!(header(head, "fennec-shard"), Some("1"), "{head}");
}

/// One request's head and body, read off a stand-in node's connection.
fn read_request(conn: &mut BufReader<TcpStream>) -> String {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(conn.read_line(&mut line).unwrap(), 0, "the router closed");
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = header(&head, "content-length").map_or(0, |n| n.parse().unwrap());
    let mut body = vec![0u8; length];
    conn.read_exact(&mut body).unwrap();
    head + std::str::from_utf8(&body).unwrap()
}

const ANSWERED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}";

/// A stand-in node at `node`, answering the router's first write: the
/// router keeps the connection, which is returned.
fn first_answered(node: &TcpListener, front: &str) -> BufReader<TcpStream> {
    let front = front.to_string();
    let first = std::thread::spawn(move || put(&front, ""));
    let mut conn = BufReader::new(node.accept().unwrap().0);
    read_request(&mut conn);
    conn.get_mut().write_all(ANSWERED).unwrap();
    let r = first.join().unwrap();
    assert_eq!(r.0, 200, "{}", r.2);
    conn
}

#[test]
fn a_write_to_a_node_refusing_its_connection_is_the_router_s_503_unsent() {
    let front = router_to(&refusing(), Duration::from_secs(10));
    told_unsent(&put(&front, ""));
}

/// A node answering no connect -- a machine gone -- is waited for as long
/// as the router's upstream timeout, then told as a refusal is.
#[test]
fn a_write_to_a_node_answering_no_connect_is_the_router_s_503_unsent() {
    let (l, _filled) = deaf();
    let timeout = Duration::from_secs(1);
    let front = router_to(&l.local_addr().unwrap().to_string(), timeout);
    let start = Instant::now();
    told_unsent(&put(&front, ""));
    assert!(start.elapsed() >= timeout, "{:?}", start.elapsed());
}

/// A node gone after it answered: the connection the router kept is found
/// closed before the write and let go of, the pool has none other, and the
/// one the router dials is refused.
#[test]
fn a_write_to_a_node_that_closed_its_pooled_connection_and_went_is_503_unsent() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(
        &node.local_addr().unwrap().to_string(),
        Duration::from_secs(10),
    );
    let conn = first_answered(&node, &front);
    drop(node);
    conn.get_ref().shutdown(Shutdown::Write).unwrap();
    until_fin_taken(conn.get_ref());
    told_unsent(&put(&front, ""));
}

/// Through a router relaying to the home region's routers, none of which
/// takes the connection: the relaying router's own 503, unsent as well.
#[test]
fn a_write_relayed_to_a_region_whose_routers_refuse_it_is_503_unsent() {
    let mut d = Directory::in_memory();
    d.set_region("us", Some(&[format!("http://{}", refusing())]))
        .unwrap();
    d.place_home("acme", "us").unwrap();
    let front = serve(Router::new(d, config(Duration::from_secs(10))));
    let r = put(&front, "");
    told_unsent(&r);
    assert!(r.2.contains("region `us`"), "{}", r.2);
}

/// A keyed write is sent again once its pooled connection ends before an
/// answer, running twice harmless; the node took it whole before it went,
/// and may have run it, so the dial refused after is no request that ran
/// nowhere: a 502, which a client sends again under the same key. Told it
/// ran nothing, a client might send it under a new one, and make it twice.
#[test]
fn a_keyed_write_its_node_took_whole_before_it_went_stays_a_502() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(
        &node.local_addr().unwrap().to_string(),
        Duration::from_secs(10),
    );
    let mut conn = first_answered(&node, &front);
    let second = std::thread::spawn({
        let front = front.clone();
        move || put(&front, "Idempotency-Key: k-1\r\n")
    });
    let taken = read_request(&mut conn);
    assert!(taken.contains("Idempotency-Key: k-1"), "{taken}");
    drop(node);
    drop(conn);
    told_may_have_run(&second.join().unwrap());
}

/// An answer the node began and broke off is the node's: a 502, the
/// request having run, never one the router says ran nowhere.
#[test]
fn an_answer_the_node_broke_off_stays_a_502() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(
        &node.local_addr().unwrap().to_string(),
        Duration::from_secs(10),
    );
    let asked = std::thread::spawn({
        let front = front.clone();
        move || put(&front, "")
    });
    let mut conn = BufReader::new(node.accept().unwrap().0);
    read_request(&mut conn);
    conn.get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"a\"")
        .unwrap();
    drop(conn);
    let r = asked.join().unwrap();
    told_may_have_run(&r);
    assert!(r.2.contains("broke off"), "{}", r.2);
}

/// A node that takes each connection's head and closes it before the
/// body, so that the router's write fails before its last byte: the body
/// is past what both sockets' buffers hold, and the close with the body
/// unread resets the connection. Sent again on a fresh connection, as a
/// request not sent whole is, it fails the same way: the request ran
/// nowhere.
#[test]
fn a_write_its_node_ended_before_its_last_byte_is_503_unsent() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(
        &node.local_addr().unwrap().to_string(),
        Duration::from_secs(10),
    );
    std::thread::spawn(move || {
        for conn in node.incoming() {
            let mut conn = BufReader::new(conn.unwrap());
            let mut line = String::new();
            while conn.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
        }
    });
    let title = "x".repeat(32 << 20);
    let body = format!(r#"{{"query":"put notes {{title: \"{title}\"}}"}}"#);
    told_unsent(&send(&front, "", &body));
}

/// A node's answer saying it was never sent the request is no word of the
/// router's, which alone says so: the mark goes no further.
#[test]
fn a_node_s_unsent_mark_is_not_passed_on() {
    let node = TcpListener::bind("127.0.0.1:0").unwrap();
    let front = router_to(
        &node.local_addr().unwrap().to_string(),
        Duration::from_secs(10),
    );
    let asked = std::thread::spawn({
        let front = front.clone();
        move || put(&front, "")
    });
    let mut conn = BufReader::new(node.accept().unwrap().0);
    read_request(&mut conn);
    conn.get_mut()
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nFennec-Unsent: 1\r\nRetry-After: 1\r\n\
              Content-Length: 2\r\nConnection: close\r\n\r\n{}",
        )
        .unwrap();
    let (status, head, body) = asked.join().unwrap();
    assert_eq!(status, 503, "{head}\n{body}");
    assert_eq!(header(&head, "fennec-unsent"), None, "{head}");
    assert_eq!(header(&head, "fennec-shard"), Some("1"), "{head}");
}

/// A router relaying to the home region's, which never sent the request
/// on: its 503 is the answer, passed on with its mark.
#[test]
fn a_write_relayed_to_a_router_that_never_sent_it_is_503_unsent() {
    let unsent = router_to(&refusing(), Duration::from_secs(10));
    let mut d = Directory::in_memory();
    d.set_region("us", Some(&[format!("http://{unsent}")]))
        .unwrap();
    d.place_home("acme", "us").unwrap();
    let front = serve(Router::new(d, config(Duration::from_secs(10))));
    told_unsent(&put(&front, ""));
}

/// A keyed write the home region's first router took whole and ended on
/// before its answer goes to the next, running twice harmless; that one's
/// 503 saying it never sent the request is no answer, the first having
/// perhaps run it: a 502, which a client sends again under the same key.
/// Passed on, the mark told a client it could send it under a new one.
#[test]
fn a_keyed_write_a_router_took_whole_is_never_told_unsent_by_the_next() {
    let taking = TcpListener::bind("127.0.0.1:0").unwrap();
    let first = taking.local_addr().unwrap().to_string();
    let took = std::thread::spawn(move || {
        let mut conn = BufReader::new(taking.accept().unwrap().0);
        read_request(&mut conn)
    });
    let unsent = router_to(&refusing(), Duration::from_secs(10));
    let mut d = Directory::in_memory();
    let routers = [format!("http://{first}"), format!("http://{unsent}")];
    d.set_region("us", Some(&routers)).unwrap();
    d.place_home("acme", "us").unwrap();
    let front = serve(Router::new(d, config(Duration::from_secs(10))));
    let r = put(&front, "Idempotency-Key: k-1\r\n");
    let taken = took.join().unwrap();
    assert!(taken.contains("Idempotency-Key: k-1"), "{taken}");
    told_may_have_run(&r);
    assert!(r.2.contains("may have run"), "{}", r.2);
}
