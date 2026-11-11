//! What a page on another origin is told (`--http-cors`): by a node on
//! every answer, and by `fennec-shard` on every answer it gives, its own and
//! the ones it forwards. One list for both, so a page reads the same
//! headers whichever of the two it reaches.
//!
//! And which pages a server answers at all ([`foreign`]): one that asks for
//! no token answers a request only under this machine's name, and a page
//! only from its own origin or the one `--http-cors` names, before a route
//! is chosen (`docs/design/http.md`, *Open servers*).

use crate::http::{Request, Response};

/// Why a request is refused for where it comes from, if it is: an `Origin`
/// that is neither the server's own nor the one `cors` (`--http-cors`) lets
/// in, or -- where `token_flag` names the flag that would give the server a
/// token, which it was not given -- a `Host` that names something other
/// than this machine.
///
/// A server that asks for no token runs whatever reaches it with every
/// right. A page on any site reaches `127.0.0.1` with a simple request --
/// `fetch(url, {method: "POST", mode: "no-cors", body})`, no preflight --
/// whose answer it cannot read but whose write runs: its `Origin` is how it
/// is told apart. A page whose DNS name was rebound to 127.0.0.1 is on the
/// server's origin as the browser sees it: it sends its own name as `Host`
/// and as its origin, which agree, and a same-origin `GET` sends no
/// `Origin` at all; only `Host` tells it apart. An address written as one
/// cannot be rebound, and a name under `localhost` is this machine's in
/// every browser, which never asks DNS for it.
///
/// A server with a token asks neither: a page sends no `Authorization` to
/// another origin without a preflight, which CORS refuses but for the
/// origin `--http-cors` names, and a rebound page does not hold the token.
/// So the `Host` part is asked of an open server alone, and its caller asks
/// the `Origin` part of an open one alone too, but for MCP's endpoint,
/// which asks it of every server, as its binding asks of every connection.
/// A request with no `Origin` -- curl, the SDKs, another server -- is no
/// page's.
pub fn foreign(cors: Option<&str>, token_flag: Option<&str>, req: &Request) -> Option<String> {
    if let Some(why) = token_flag.and_then(|flag| foreign_host(flag, req)) {
        return Some(why);
    }
    let host = req.header("host").map(str::trim);
    let origin = req.header("origin")?.trim();
    // The origin `--http-cors` lets in, as every answer's CORS says it.
    match allows(cors, req) || same_origin(origin, host) {
        true => None,
        false => Some(format!(
            "Origin `{origin}` may not call this server: --http-cors names the origin a page may come from"
        )),
    }
}

/// The `Host` part of [`foreign`] alone, of a server that asks for no
/// token, `token_flag` the flag that would give it one: a router with no
/// `--http-cors` of its own asks it of a forward, whose page is judged by
/// the node's rule, the one its answer carries through.
pub fn foreign_host(token_flag: &str, req: &Request) -> Option<String> {
    let h = req.header("host")?.trim();
    (!local_host(h)).then(|| {
        format!(
            "Host `{h}` is not this machine's, and this server asks for no token: \
             reach it as localhost or by its address, or give it {token_flag}"
        )
    })
}

/// Whether a router may answer for `req`'s origin to the node it forwards
/// the request to, which then needs none: one its `--http-cors` lets in,
/// whose page reads the router's CORS in place of the node's
/// (`fennec-shard`'s rule), or the router's own under a name of this
/// machine -- a page it served, its studio's. Under any other name its own
/// origin is a rebound page's as much as the router's: the node is sent it
/// and asks it as its own.
pub fn vouched(cors: Option<&str>, req: &Request) -> bool {
    let Some(origin) = req.header("origin").map(str::trim) else {
        return false;
    };
    let host = req.header("host").map(str::trim);
    allows(cors, req) || (same_origin(origin, host) && host.is_some_and(local_host))
}

/// Whether `origin` (`scheme://authority`) is the one `host` names: a page
/// the server itself served.
fn same_origin(origin: &str, host: Option<&str>) -> bool {
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| Some(authority) == host)
}

/// Whether `host` (a `Host` header, port and all) names this machine or an
/// address: `localhost` and its subdomains, or an IP literal.
pub fn local_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        // `[::1]:8080`
        Some(rest) => {
            return rest
                .split(']')
                .next()
                .is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok())
        }
        None => host.rsplit_once(':').map_or(host, |(n, port)| {
            if port.bytes().all(|b| b.is_ascii_digit()) {
                n
            } else {
                host
            }
        }),
    };
    let name = name.trim_end_matches('.').as_bytes();
    // Every request of an open server asks it: compared in place, never
    // lowered into a copy.
    let under = |suffix: &[u8]| {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    };
    name.eq_ignore_ascii_case(b"localhost")
        || (name.len() > 10 && under(b".localhost"))
        || std::str::from_utf8(name).is_ok_and(|n| n.parse::<std::net::IpAddr>().is_ok())
}

/// Whether `--http-cors` lets `req`'s origin in: `*` lets every one, an
/// origin itself alone. A preflight from another is answered as before,
/// after its token.
pub fn allows(cors: Option<&str>, req: &Request) -> bool {
    match cors {
        None => false,
        Some("*") => true,
        Some(allowed) => req.header("origin") == Some(allowed),
    }
}

/// The headers an answer carries under `--http-cors <origin>`.
fn headers(origin: &str) -> [(&'static str, &str); 6] {
    [
        ("Access-Control-Allow-Origin", origin),
        // `PUT /<name>/<id>` writes a row whole: the server token's alone,
        // a scoped token's 403 whatever it asks, so allowed for a page
        // holding that token (an admin's tool). A user's page holds a
        // write to the row it read with `PATCH` or `DELETE` under
        // `If-Match`, the header allowed below.
        (
            "Access-Control-Allow-Methods",
            "GET, POST, PUT, PATCH, DELETE, OPTIONS",
        ),
        // A page's replica sends each write under an `Idempotency-Key`
        // and reads the `Fennec-Seq` of its answer, which tells it when a
        // stream is past the write: refused and unread across origins
        // before, its retries made a row twice and an insert the shape
        // did not hold kept its temporary row until the next seed. The
        // request id goes out too: the studio's query editor, served by
        // a node and asking a router (`--studio-connect`), shows it
        // beside an answer so a person can find its lines in the logs.
        // An MCP client in a page -- the MCP Inspector's, an agent's --
        // mirrors a 2026-07-28 request's version, method and tool into
        // headers its binding requires, refused unread otherwise. A page
        // marks its reads for a replica near it with `Fennec-Read`, which
        // refused, went home every time. A router's 503 for a request it
        // never got to the node says so (`Fennec-Unsent`), which a page
        // reads to send an unkeyed write again. A deprecated route's
        // answer says so in RFC 9745's `Deprecation`, RFC 8594's `Sunset`
        // and its `Link` (`openapi::Deprecated`), none of them safelisted:
        // unexposed, a page's client could not warn of a route going. The
        // list is one constant, written as it is on every answer.
        (
            "Access-Control-Allow-Headers",
            "content-type, authorization, idempotency-key, fennec-after, fennec-history, fennec-wait, \
             fennec-read, if-match, if-none-match, mcp-protocol-version, mcp-method, mcp-name",
        ),
        (
            "Access-Control-Expose-Headers",
            "fennec-seq, fennec-history, fennec-next, idempotent-replayed, etag, x-request-id, \
             fennec-quota, retry-after, fennec-unsent, deprecation, sunset, link",
        ),
        ("Access-Control-Max-Age", "600"),
        ("Vary", "Origin"),
    ]
}

/// `resp` with the headers `cors` names, or as it is without one.
pub fn apply(resp: Response, cors: Option<&str>) -> Response {
    match cors {
        None => resp,
        Some(origin) => headers(origin)
            .into_iter()
            .fold(resp, |resp, (k, v)| resp.header(k, v)),
    }
}

/// The same headers written into a head being built by hand: the router's
/// for an answer it forwards.
pub fn push_into(head: &mut String, origin: &str) {
    for (k, v) in headers(origin) {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
}

/// Whether a header of a node's answer is one [`apply`] writes: a router
/// with `--http-cors` of its own puts its own in their place. Both kept, a
/// node allowing `*` behind a router allowing one origin answered two
/// `Access-Control-Allow-Origin`s, which a browser refuses whole; and the
/// router answers the preflights itself, so the answer has to say what its
/// preflight said. A `Vary` naming anything but `Origin` alone is kept.
pub fn is_cors_header(name: &str, value: &str) -> bool {
    let name = name.as_bytes();
    let prefix = b"access-control-";
    (name.len() > prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix))
        || (name.eq_ignore_ascii_case(b"vary") && value.trim().eq_ignore_ascii_case("origin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_host_is_this_machine_or_an_address() {
        for h in [
            "localhost",
            "localhost:8080",
            "LOCALHOST.:80",
            "app.localhost:3000",
            "127.0.0.1:8080",
            "10.0.0.7:8080",
            "[::1]:8080",
            "[fe80::1]",
        ] {
            assert!(local_host(h), "{h}");
        }
        for h in [
            "evil.example:8080",
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.nip.io:80",
        ] {
            assert!(!local_host(h), "{h}");
        }
    }

    #[test]
    fn a_nodes_cors_headers_are_told_from_the_rest() {
        assert!(is_cors_header("Access-Control-Allow-Origin", "*"));
        assert!(is_cors_header("access-control-expose-headers", "x"));
        assert!(is_cors_header("Vary", "Origin"));
        assert!(is_cors_header("vary", " origin "));
        assert!(!is_cors_header("Vary", "Accept-Encoding"));
        assert!(!is_cors_header("Vary", "Origin, Accept-Encoding"));
        assert!(!is_cors_header("Access-Control-", ""));
        assert!(!is_cors_header("Content-Type", "application/json"));
        assert!(!is_cors_header("Fennec-Seq", "3"));
    }

    #[test]
    fn written_by_hand_as_onto_a_response() {
        let resp = apply(Response::empty(204), Some("https://app.example"));
        let mut head = String::new();
        push_into(&mut head, "https://app.example");
        let from_resp: String = resp
            .extra
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        assert_eq!(head, from_resp);
        assert!(apply(Response::empty(204), None).extra.is_empty());
    }

    /// A page on another origin reads what a deprecated route answers
    /// (`openapi::Deprecated::headers`), each name as it writes it.
    #[test]
    fn a_deprecations_headers_are_exposed() {
        let resp = apply(Response::empty(204), Some("https://app.example"));
        let exposed = resp
            .extra
            .iter()
            .find(|(k, _)| k == "Access-Control-Expose-Headers")
            .map(|(_, v)| v.as_str())
            .unwrap();
        let names: Vec<&str> = exposed.split(", ").collect();
        let marked = crate::openapi::Deprecated {
            since: 1_794_096_000,
            sunset: Some(1_809_734_400),
            link: "https://fennecdb.com/docs/http-reference#compatibility",
        };
        for (name, _) in marked.headers() {
            assert!(
                names.contains(&name.to_ascii_lowercase().as_str()),
                "{name} is not exposed: {exposed}"
            );
        }
    }
}
