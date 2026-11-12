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
/// page's. `tls`: whether the listener speaks HTTPS, whose own origin is
/// `https://` and no other.
pub fn foreign(
    cors: Option<&str>,
    token_flag: Option<&str>,
    tls: bool,
    req: &Request,
) -> Option<String> {
    let (host, origin) = match looked(token_flag, req) {
        Ok(found) => found,
        Err(why) => return Some(why),
    };
    let origin = origin?;
    // The origin `--http-cors` lets in, as every answer's CORS says it.
    let allowed = matches!(cors, Some(c) if c == "*" || Some(c) == req.header("origin"));
    match allowed || same_origin(origin, host, tls) {
        true => None,
        false => Some(format!(
            "Origin `{origin}` may not call this server: --http-cors names the origin a page may come from"
        )),
    }
}

/// The header a router names the `Host` its client reached it by in, on
/// every request it forwards: the client's own, the router's name for
/// itself, which the node never sees otherwise -- a forward's `Host` is
/// the node's address, which a proxy in front of a node may route by. The
/// one a client sent is dropped and the router's put in its place; a
/// relaying router's alone is kept, under its mark (fennec-shard's
/// `forwarded_headers`). `X-Forwarded-Host` rather than RFC 7239's
/// `Forwarded: host=`: one value, no list of hops with its quoting to
/// parse on every request of an open server, and the one the proxies a
/// node may sit behind send of their own -- Caddy and Traefik by default,
/// nginx where told -- so the rule meets theirs too.
pub const FORWARDED_HOST: &str = "X-Forwarded-Host";

/// The `Host` part of [`foreign`] alone, of a server that asks for no
/// token, `token_flag` the flag that would give it one: a router with no
/// `--http-cors` of its own asks it of a forward, whose page is judged by
/// the node's rule, the one its answer carries through.
///
/// Asked of [`FORWARDED_HOST`] as of `Host`, every value of each and every
/// name in a list: a router in front of an open node reached by a rebound
/// name sends the node that name there, the node seeing its own address as
/// `Host` -- a token router's, which asks a page nothing on a forward, let
/// a rebound page read a tenant's rows, its streams and `/_changes`
/// through it. The header is believed from no one, for it can only refuse
/// more: a request naming this machine in both passes as it did with
/// `Host` alone, and one sending it of its own -- a rebound page can, being
/// same-origin -- refuses itself. Nothing a client sends lifts a refusal,
/// so the router overwrites it rather than this asking who sent it.
pub fn foreign_host(token_flag: &str, req: &Request) -> Option<String> {
    looked(Some(token_flag), req).err()
}

/// One pass over `req`'s headers, as every request of an open server
/// makes it: its first `Host` and `Origin`, trimmed, or -- where
/// `token_flag` asks it -- why a `Host` or [`FORWARDED_HOST`] names another
/// machine than this one. Each name is told by its length before its
/// letters, in the one pass: asked by name in turn, the three cost an
/// SDK's request with no `Origin` 46 ns, where `Host` and `Origin` alone
/// had cost 32; so, 29 (55 and 54 with its own origin; best of five runs
/// of twenty million, release).
fn looked<'r>(
    token_flag: Option<&str>,
    req: &'r Request,
) -> Result<(Option<&'r str>, Option<&'r str>), String> {
    let (mut host, mut origin) = (None, None);
    for (k, v) in &req.headers {
        let foreign = match k.len() {
            4 if k.eq_ignore_ascii_case("host") => {
                let h = v.trim();
                host.get_or_insert(h);
                // A `Host` holds no comma, and is asked whole.
                token_flag.and(Some(h)).filter(|h| !local_host(h))
            }
            6 if k.eq_ignore_ascii_case("origin") => {
                origin.get_or_insert(v.trim());
                None
            }
            // A proxy adds its own to the list after the ones before it.
            16 if k.eq_ignore_ascii_case(FORWARDED_HOST) && token_flag.is_some() => {
                v.split(',').map(str::trim).find(|h| !local_host(h))
            }
            _ => None,
        };
        if let (Some(h), Some(flag)) = (foreign, token_flag) {
            let through = match k.len() == 4 {
                true => "",
                false => " (the name a router or proxy was reached by)",
            };
            return Err(format!(
                "Host `{h}`{through} is not this machine's, and this server asks for no \
                 token: reach it as localhost or by its address, or give it {flag}"
            ));
        }
    }
    Ok((host, origin))
}

/// Whether a router may answer for `req`'s origin to the node it forwards
/// the request to, which then needs none: one its `--http-cors` lets in,
/// whose page reads the router's CORS in place of the node's
/// (`fennec-shard`'s rule), or the router's own under a name of this
/// machine -- a page it served, its studio's. Under any other name its own
/// origin is a rebound page's as much as the router's: the node is sent it
/// and asks it as its own. `tls`: whether the router's listener speaks
/// HTTPS.
pub fn vouched(cors: Option<&str>, tls: bool, req: &Request) -> bool {
    let Some(origin) = req.header("origin").map(str::trim) else {
        return false;
    };
    let host = req.header("host").map(str::trim);
    allows(cors, req) || (same_origin(origin, host, tls) && host.is_some_and(local_host))
}

/// Whether `origin` (`scheme://authority`) is the one `host` names, on a
/// listener speaking HTTPS where `tls`: a page the server itself served.
/// The scheme is half of an origin: `http://localhost`, port 80's page,
/// is no page of an HTTPS listener on 443 reached as `localhost`, whose
/// `Host` names the same authority.
fn same_origin(origin: &str, host: Option<&str>, tls: bool) -> bool {
    let scheme = if tls { "https" } else { "http" };
    origin
        .split_once("://")
        .is_some_and(|(s, authority)| s == scheme && Some(authority) == host)
}

/// Whether `host` (a `Host` header, port and all) names this machine or an
/// address: `localhost` and its subdomains, or an IP literal.
pub fn local_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        // `[::1]:8080`: the address, then nothing or a port alone --
        // `[::1]evil.example` names no address.
        Some(rest) => {
            return rest.split_once(']').is_some_and(|(ip, port)| {
                ip.parse::<std::net::Ipv6Addr>().is_ok()
                    && (port.is_empty()
                        || port
                            .strip_prefix(':')
                            .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit())))
            })
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
            "[::1]",
            "[fe80::1]",
        ] {
            assert!(local_host(h), "{h}");
        }
        for h in [
            "evil.example:8080",
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.nip.io:80",
            // After the bracket, a port alone.
            "[::1]evil.example",
            "[::1].evil.example:80",
            "[::1]:abc",
            "[::1]:80evil",
        ] {
            assert!(!local_host(h), "{h}");
        }
    }

    fn request(headers: &[(&str, &str)]) -> Request {
        Request {
            method: crate::http::Method::Post,
            target: "/query".into(),
            path: "/query".into(),
            query: Vec::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    /// An origin is its scheme too: a listener speaking HTTPS reached as
    /// `localhost` is no page's of `http://localhost`, port 80's, and a
    /// plain one none of `https://`'s.
    #[test]
    fn a_servers_own_origin_is_its_scheme_too() {
        let open = Some("--http-token");
        let page = |origin| request(&[("Host", "localhost"), ("Origin", origin)]);
        assert_eq!(foreign(None, open, true, &page("https://localhost")), None);
        assert!(foreign(None, open, true, &page("http://localhost")).is_some());
        assert_eq!(foreign(None, open, false, &page("http://localhost")), None);
        assert!(foreign(None, open, false, &page("https://localhost")).is_some());
        // MCP asks a server with a token the same.
        assert!(foreign(None, None, true, &page("http://localhost")).is_some());
        // The router answers for its own page alike.
        assert!(vouched(None, true, &page("https://localhost")));
        assert!(!vouched(None, true, &page("http://localhost")));
        assert!(!vouched(None, false, &page("https://localhost")));
    }

    /// `X-Forwarded-Host` is asked as `Host` is, every name of it, and can
    /// only refuse: the name a router in front was reached by.
    #[test]
    fn a_forwarded_host_can_only_refuse() {
        let asked = |headers: &[(&str, &str)]| foreign_host("--http-token", &request(headers));
        let node = ("Host", "127.0.0.1:8081");
        assert_eq!(asked(&[node]), None);
        assert_eq!(asked(&[node, ("X-Forwarded-Host", "localhost:8090")]), None);
        let why = asked(&[node, ("x-forwarded-host", "evil.example:8090")]).unwrap();
        assert!(why.contains("Host `evil.example:8090`"), "{why}");
        assert!(why.contains("a router or proxy"), "{why}");
        // Every name in its list, and every one of the header.
        let list = ("X-Forwarded-Host", "localhost, evil.example");
        assert!(asked(&[node, list]).is_some());
        let twice = [
            node,
            ("X-Forwarded-Host", "localhost"),
            ("X-Forwarded-Host", "evil.example"),
        ];
        assert!(asked(&twice).is_some());
        // It lifts no refusal of `Host`.
        let rebound = ("Host", "evil.example");
        assert!(asked(&[rebound, ("X-Forwarded-Host", "localhost")]).is_some());
        // No `Host` but the header: still asked.
        assert!(asked(&[("X-Forwarded-Host", "evil.example")]).is_some());
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
