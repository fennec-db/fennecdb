# Security policy

## Reporting a vulnerability

Report it privately, through GitHub:
**https://github.com/fennec-db/fennecdb/security/advisories/new**

That form is visible only to the maintainers. Please do not open a public issue
for a vulnerability, and please do not post a working exploit anywhere public
before a fix is out.

Useful in a report: the version or commit, the surface (wire protocol, HTTP,
file format, parser, WASM), and the smallest input that triggers it. If it
needs a crafted file or packet, attach it rather than describing it.

This is a small project, with no bounty. A report is acknowledged within 3
working days; the fix and its release follow at the pace the bug allows, and
you are told where it stands.

## Supported versions

**Today fennecdb is 0.x, an alpha.** A vulnerability found in a 0.x release
gets no security advisory, no CVE and no patch release. The fix lands on
`main` and ships in the next release, and the pull request that fixes it
says which releases had the bug. Run the latest release, or `main`; an older
0.x tag is not fixed.

**From 1.0.0** the latest 1.x minor gets security fixes, and the previous
minor for 6 months after the next one ships. A `release/1.N` branch is cut
when a backport needs one. A fix there is released as a patch version,
published as a GitHub security advisory, and noted in the release; the steps are in
[RELEASING.md](RELEASING.md#a-security-release). A 1.x release only grows what the
[compatibility contract](docs/design/compatibility.md) freezes, replicas
upgrading before primaries; a fix that has to touch a protocol or a
file format names in its advisory the SDKs that must move with it.

## Where the risk actually is

fennecdb takes **no external crates** in its shipped crates but one:
`fennec-tls` (rustls, ring and webpki-roots), behind a `tls` feature, which
the server, the router, the shell and `fennec import` use for HTTPS and for
PostgreSQL over TLS. Its tree is pinned by name and version, and
`tools/deps_check.py` in CI fails a crate that comes in or moves unreviewed
(it pins crates by name and version, so a feature matters where it brings in
a crate); `fennec-core`, `fennec-ql`, `fennec-abi`,
`fennec-wasm` and `fennec-ffi`, and so the browser module, reach none of it
([docs/design/crates.md](docs/design/crates.md#tls-the-one-exception)).
The packages' own dependencies (npm, Go, NuGet, Kotlin, Dart) are audited
for known advisories by `audit.yml`; the desktop app is not among them: it
ships Electrobun with a Bun runtime
([`integrations/studio-desktop/README.md`](integrations/studio-desktop/README.md)),
outside that audit. For everything else the code is ours: own codec, own
JSON, own HNSW, own gzip over a deflate of our own, and the cryptography
below. That removes the upstream CVE surface for all but TLS, and the
desktop app's runtime, and puts the rest in this repository instead.

The primitives we wrote are SHA-256, HMAC-SHA-256, PBKDF2-HMAC-SHA-256,
ChaCha20-Poly1305, RSA PKCS#1 v1.5 verification with SHA-256, ECDSA P-256
verification (ES256) and the client half of SCRAM-SHA-256 that
`fennec import` signs in to PostgreSQL with. They are in
`crates/fennec-http/src/crypto.rs` (ES256 in `crypto/p256.rs`) and
`crates/fennec-wire/src/client.rs`, and are exactly the code that most
deserves a second pair of eyes, with the in-tree bindings to the system's
randomness (`crates/fennec-http/src/crypto/unix.rs` and `windows.rs`), which
the keys and nonces come from. Each primitive is held to its RFC's vectors
and, where Project Wycheproof has a file for it, to every case of that file:
ChaCha20-Poly1305, RSA PKCS#1 v1.5 with SHA-256 at 2048 to 4096 bits, ECDSA
P-256, HMAC-SHA-256 and PBKDF2-HMAC-SHA-256
([docs/design/conventions.md](docs/design/conventions.md#secrets-compared)).
Vectors are no proof: a bug they miss is as welcome a report as any. What a
browser page seals with `cryptoKey` is WebCrypto's AES-GCM, not ours.

In scope, and worth reporting:

- Memory unsafety, panics or unbounded allocation reachable from input that
  crosses a trust boundary: the HTTP/JSON surface and the MCP endpoint, the
  FennecQL parser, the on-disk file format and an archive's or a sealed
  file's, a replica's sync and replication streams, a webhook's event, the
  router and the global entry, the SQLite and PostgreSQL readers of
  `fennec import`, the native library's C ABI and the WASM boundary.
- Anything that authenticates when it should not, or that lets a client skip
  authentication: the bearer token, a JWT and the policy it is held to, a
  webhook's signature, a tenant's binding, a scoped token reading a field or
  a row its rules withhold, or reaching a collection of the server's own.
- A non-constant-time comparison on a secret. Every token, mark, MAC and
  signature is compared in constant time on purpose, by `crypto::ct_eq` alone,
  and CI refuses `==` on one by its name (`tools/secret_eq_check.py`); a
  regression there is a bug worth reporting.
- Allocation driven by an unauthenticated peer. A request's head is capped and
  its body bounded by `max_body` before it is read -- a tenant node reads past
  it an image's import alone, and only once its head carries the admin token;
  a path around either is a vulnerability.
- A connection held past its request's time by a slow peer. `fennec-server`
  and `fennec-shard` read a request within `--header-timeout` (10 s by
  default) of its first byte and a second more for each KiB of it that came
  -- a pace of 1 KiB a second, head and body alike, over HTTPS from its
  first record's first part -- and a write of an answer waits
  `--write-timeout` (30 s by default) at most for its client to take any of
  it; a way to hold a thread and a `--max-connections` slot past them is a
  vulnerability.
- A crafted `.fennec` file that does more than fail to open — the reader is
  expected to reject a damaged or hostile file, not to be exploited by it.
- Query text reaching an identifier or a value without validation, from any
  client surface.
- An integrity break in a sealed file: a frame that opens in a file it was
  not sealed into (a version 2 file binds each frame to its file, its place
  and whether it is the last), or one changed byte that still opens.
- Studio, the page the server serves: it is served before any token, under a
  strict CSP, and shows stored rows to whoever holds the server token, so a
  script injected through a row it shows would take that token
  ([`studio/CLAUDE.md`](studio/CLAUDE.md)). The desktop app's own boundary is
  in scope as well: its token in an allowlisted environment, never on argv,
  one server per file ([`integrations/CLAUDE.md`](integrations/CLAUDE.md)).

Where each surface's rules are kept, and held by tests: the HTTP server,
replication and webhooks in
[`crates/fennec-http/CLAUDE.md`](crates/fennec-http/CLAUDE.md), a
connection's time, ceiling and TLS in
[`crates/fennec-http/src/conn/CLAUDE.md`](crates/fennec-http/src/conn/CLAUDE.md), the MCP
endpoint in
[`crates/fennec-http/src/mcp/CLAUDE.md`](crates/fennec-http/src/mcp/CLAUDE.md), tokens,
policies and tenant binding in
[`crates/fennec-http/src/access/CLAUDE.md`](crates/fennec-http/src/access/CLAUDE.md),
the file format and the engine in
[`crates/fennec-core/CLAUDE.md`](crates/fennec-core/CLAUDE.md), the router
in [`crates/fennec-shard/CLAUDE.md`](crates/fennec-shard/CLAUDE.md), the sync
and the native library in
[`crates/fennec-abi/CLAUDE.md`](crates/fennec-abi/CLAUDE.md) and
[`crates/fennec-ffi/CLAUDE.md`](crates/fennec-ffi/CLAUDE.md), the PostgreSQL
and SQLite readers in
[`crates/fennec-import/CLAUDE.md`](crates/fennec-import/CLAUDE.md), the
browser client in [`web/CLAUDE.md`](web/CLAUDE.md), studio in
[`studio/CLAUDE.md`](studio/CLAUDE.md). The desktop app (beta) is a window on
a studio that a `fennec-server` serves
([`integrations/CLAUDE.md`](integrations/CLAUDE.md)); what a page shows of
the server's data is the server's to decide, as in a browser.

Six parsers are fuzzed, the targets of `crates/fennec-fuzz`: an HTTP request
(`http`), JSON (`json`), FennecQL (`fennecql`), the file format's codec
(`codec`), a `.fennec` file's open, mapped as a server opens it, with a
replica's apply (`image`), and a SQLite file (`sqlite`), under
AddressSanitizer on every pull request and for longer each week (`make fuzz`
lists them). **Not fuzzed today:** the PostgreSQL wire and `pgoutput`
reader, the framing of a sealed file and of an archive, JWT and JWKS
reading, and a webhook's headers; a report there is as welcome, and a fuzz
target for each is a contribution. An input that crashes a target is a
report worth making, privately as above.

CI's smoke runs each target a fixed count of runs, from the tree's seeds and
a fixed seed, so a commit's smoke hands it the same inputs on any machine;
on a finding it prints the panic's message and ASan's report on a public
log. Those inputs are the commit's own, made from the repository's seeds. The weekly run
(`fuzz.yml`) uses a seed of its own and keeps only the recipe -- the target,
the seed, a run and the input's hash -- from which `fennec-fuzz reproduce`
makes the input again on a maintainer's machine.

## What is documented behaviour, not a vulnerability

These are deliberate. The [server](https://fennecdb.com/docs/server),
[access](https://fennecdb.com/docs/http-access) and
[import](https://fennecdb.com/docs/import) pages explain most of them, and
[limits](https://fennecdb.com/docs/limits) the ceilings:

- **TLS is off until a certificate is given.** `fennec-server` and
  `fennec-shard` speak plain HTTP unless `--tls-cert` and `--tls-key` are
  passed, which make a listener HTTPS alone; without them a token and the data
  flow in the clear, so on an open network give a certificate or put a TLS
  terminator (nginx, Caddy, stunnel) in front. What they dial themselves --
  a primary, a node, an OTLP collector -- is dialed over TLS when its URL
  says `https://`, against Mozilla's roots or `--tls-ca`'s, and no client
  certificates are asked or presented; the shell's `fennec backup` dials its
  primary the same way
  ([server docs](https://fennecdb.com/docs/server#tls)).
- **`--insecure` does what it says.** Binding to a non-loopback address without
  authentication is refused unless that flag is passed deliberately. A server
  that asks for no token answers only under this machine's name
  (`localhost` or an IP address, else 403) and, to a page in a browser, only
  from its own origin or the one `--http-cors` names, so a page whose DNS
  name was pointed at 127.0.0.1 reads nothing; to reach it by another name,
  give it a token. `--http-cors '*'` lets every origin in: any site can then
  read and write an open loopback server, so give a server that is open a
  named origin, or a token.
- **A short token is taken on loopback alone.** Past loopback `fennec-server`
  and `fennec-shard` refuse at the start a token shorter than 32 bytes --
  `--http-token`, `--admin-token`, `--replication-token`, the router's
  `--token`, `--entry-token` and `--replication-token`, or the variable each
  is read from -- naming the flag; `--insecure` does not waive it. On
  loopback any length is taken, so a short token there is the operator's
  call -- but behind a proxy or a router, 32 bytes too: a TLS proxy in front
  of a server on 127.0.0.1, or a router on the same machine relaying to
  nodes there, reaches that listener for the whole network, and behind a
  plain proxy every refusal waits by the proxy's address, which any
  client's good token clears. Such a server logs a warning once, at the
  first request carrying `Forwarded`, `X-Forwarded-For` or
  `X-Forwarded-Host`, while it holds a shorter token; it refuses nothing.
  Loopback is judged of the addresses the listener is bound on, and a name
  that cannot be looked up counts as past it. A refused token waits, 100 ms
  doubled to 5 s, by its address -- an IPv6 address by its /64 -- in a table
  of 10 000 counts that lets the one refused longest ago go when full:
  against a client holding more addresses than that, the token's length is
  what holds.
- **A webhook's secret has no minimum.** One shorter than 32 bytes is taken
  and warned of -- at the start where `--http` is past loopback, through a
  proxy as a short token is, and in a node's log as a tenant's is set --
  since a sender's secret is the sender's to make. A GitHub secret is the
  operator's to choose: a short one is guessed offline from one event it
  signed.
- **An empty token is no token.** A variable set empty is unset, and an
  empty token flag, JWT secret file or key file is refused at the start, so
  an unset variable in a deployment cannot be read as a token of no length.
- **An expired token is told so, and that is all.** The answer is 401 with
  the code `token_expired`; the server keeps no list of revoked tokens, so
  signing out a token before its `exp` is the issuer's to do.
- **`explain` is the server token's.** A scoped token's `explain` is refused
  (403): a plan counts rows before the token's rules hold them.
- **A token on the command line shows up in `ps`.** `FENNEC_HTTP_TOKEN`,
  `FENNEC_JWT_SECRET`, `--jwt-secret-file` and `FENNEC_REPLICATION_TOKEN` exist
  for that reason.
- **`--jwt-require-exp off` takes a token with no `exp` for ever.** By
  default such a token is refused (401); turning that off is the operator's
  call, as is how far ahead `--jwt-max-age` lets an `exp` lie.
- **`--jwt-unbound-tenants` takes a token naming no tenant for every tenant**
  of a `--dir` node. By default such a token is refused there (403), and a
  token naming a tenant reaches that tenant alone.
- **Without `--jwt-issuer` and `--jwt-audience`, every token the keys sign
  is taken.** An identity provider's keys sign tokens for every app of its
  tenant, pool or team; told an issuer and an audience (or `--jwt-client`),
  the server refuses the tokens issued for another (401). With `--jwt-keys`
  and no `--jwt-issuer` the server says so on stderr as it starts. Leaving
  them out where the keys sign for more than one app is the operator's
  call -- but not with Firebase, whose keys sign every project's tokens:
  there, without both, anyone's project mints tokens this server takes,
  claims and all.
- **Issuer and audience are not enough where the caller names the
  audience.** Google's identity tokens (`https://accounts.google.com`),
  GitHub Actions' and a Kubernetes cluster's projected tokens carry
  whatever audience whoever asked for them named: any account of the
  issuer can have one for this server. There the policy's rules must pin
  whose token it is (`sub`, `azp`, `repository`); the flags say only which
  issuer and which audience.
- **A score is of the rows the request may read.** A scoped token's
  `match` takes BM25's statistics over the rows its filter selects, so its
  scores say nothing of rows it cannot read; the server's own token scores
  over the collection. The time a `match` takes still grows with every row
  holding its words, the token's or not, as an index walk does: users who
  must learn nothing of each other's rows, not even through timing, belong
  in tenants, a file each, or in collections of their own.
- **A scoped update reaches every field unless its grant names them.**
  `accounts update for app` lets the app's token `set accounts {kind:
  ...}` as freely as `{balance: ...}`; `accounts update(balance, held,
  status) for app` refuses a write changing any other field (403), by
  every route. A field list holds a token to fields, not to amounts: one
  that may move a balance may move it anywhere its rules' filters allow,
  and a journal beside it, append-only, is what a reconciliation holds it
  to.
- **`append-only` binds scoped tokens, not the server's own.** A policy's
  `<collection> append-only` refuses every update and delete a JSON Web
  Token asks for there; the `--http-token` still changes and deletes rows,
  as corrections, erasure and `@ttl` sweeps need. An app server that should
  only append is given a scoped token. Against whoever holds the server's
  token and the file, an append-only log is a sealed archive
  (`fennec archive --key-file`) kept elsewhere.
- **Two processes opening the same file corrupts it.** There is a single writer,
  which is why everything that writes a file runs as a thread of one
  `fennec-server`. `fennec-server --file`, each tenant a `--dir` node has open,
  `fennec-shard`'s directory, the native library and every `fennec` way that
  writes a file (the shell, `mcp --file`, `import`, a backup or restore into
  a file) take `<file>.lock` (an `fcntl` record lock, `fennec_abi::lock`) and
  refuse a file another holds; a file opened through a symbolic link is the
  file it leads to, for its lock and every rewrite alike.
  Embedded Rust's `fs::open` takes none, a hard link is a name with a lock of
  its own, and a record lock does not cross a Docker Desktop bind mount or a
  network file system that keeps no locks: one process over each is the
  operator's to keep.
- **A batch holds the database.** From its first write to its end every other
  writer waits on it, and readers on a batch that is still being written.
  A batch of reads alone holds the read lock: writers wait for it as for
  any read, readers go on beside it.
  An import that stops halfway cannot be rolled back.
- **A sealed file from 0.1.7 to 0.2.0 is not bound to its file.** Its tags
  cover a frame's place and whether it is the last, not its file, so a frame of another such file under the
  same key, at the same place, opens in it. Such a file still opens and a
  segment of one goes on as one; every file begun now binds its frames to its
  own. A key that sealed a file of that version should be changed.
- **Nothing in memory is encrypted, and the live file is plain on disk.** The
  indexes (a text index's terms, a hash index's keys, vectors), the documents
  written since the last handover to the file (16 MB, 65 536 records or
  16 384 writes) and, under `--no-mmap` or on a target that does not map
  files, every document are anonymous memory, which swap and a core dump can
  reach. A mapped file's pages are file-backed (`MAP_SHARED`, read-only): the
  system drops them rather than swapping them, and a default Linux core dump
  leaves out shared file mappings. Only an archive or a backup can be sealed
  (`--key-file`), never the file a server serves: the disk's own encryption
  covers that one, and swap and core dumps are the host's to protect.
- **On a shared machine, the files are their user's: other users read none,
  that user and root read all.** On Linux and macOS every file
  `fennec-server` and `fennec` make -- a database, its sync log and lock, a
  node's tenants and their records, an archive, a backup, a restore, the
  audit log -- is made mode `0600` by the call that makes it, whatever the
  umask, and a directory they make `0700`
  ([crates/fennec-core/src/fs.rs](crates/fennec-core/src/fs.rs)); up to 0.2.0
  each took the umask's mode, usually readable by every user. Not defended:
  anything running as the server's user or as root; a file made by 0.2.0 or
  before, or given a wider mode by its operator, which keeps that mode -- a
  file replaced keeps the mode of the one it replaces, a group's bits only
  where the new file is in that group (a setgid directory keeps it), and
  nothing of a file another user owns; a copy made elsewhere
  with other modes; and what the process list shows, a password in
  `--follow`'s connection string among it
  ([server docs](https://fennecdb.com/docs/server#file-modes)). On Windows a
  file takes its folder's access list, and a key or a tenant's hook secrets
  are made their user's alone.
- **What the process holds is resident,** with no buffer pool of its own to
  evict from: the indexes, and the documents too under `--no-mmap`, since a
  mapped file's pages are the system's to keep or drop. A `--dir` node does
  close a tenant nothing has touched for `--idle-close` seconds (300 by
  default), and idle ones least recently used first when `--max-memory`
  would be passed, until the next request opens it again. An authenticated
  client can ask for work that costs memory; `--max-memory` is the bound, and
  `make memory` is how you calibrate it.
- **A peer holding many connections, each within its bounds, is not cut.**
  `fennec-server` and `fennec-shard` cap connections in all
  (`--max-connections`), not by address: a peer holding many, each read and
  written in time, takes the slots a newcomer is refused (503) for. A cap
  per address is the operator's, in front. A client that reads an answer
  slowly, taking some of each write within `--write-timeout`, is never cut.
- **A PostgreSQL connection is protected as its `sslmode` says.** Only
  `verify-full` keeps out someone standing in for the server. `prefer` is the
  default, as libpq's, and goes on in plain text where the server, or whoever
  stands in for it, refuses TLS; `require` checks nothing without a CA file,
  and `verify-ca` checks the chain but not the host's name. And the client
  answers a cleartext-password request under any mode, so whoever stands in
  for the server gets the password as well as the rows. Use `verify-full`,
  and a role that signs in by SCRAM-SHA-256 alone
  ([import docs](https://fennecdb.com/docs/import#tls)).
- **`md5` authentication is not supported** by `fennec import` and
  `--follow` when they sign in to PostgreSQL. SCRAM-SHA-256 or cleartext.
- **A GitHub webhook event carries no signed time,** so after its hook's
  `keep` (7 days by default) a captured one can be sent again and is taken
  again; until then its body's digest answers it.

If you think one of these is worse than the docs make it sound, that is
worth an issue — as a documentation or design argument, not as an advisory.

## An application built on it

[`examples/saas`](examples/saas) is a multi-tenant product on fennecdb with
its threat model written out: what the app owns (passwords, sessions,
rate limits, the signing key), what fennec-server enforces (scoped,
tenant-bound tokens, per-operation and per-field grants, append-only
logs), and the attacks its tests try against both.
