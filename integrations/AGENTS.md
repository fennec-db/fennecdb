# AGENTS.md

The integrations' invariants -- outside packages and the builders, the desktop app, Grafana's data source -- for code in `integrations/`. The rules every change meets, and where every other area's are, are in the root's [AGENTS.md](../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### Integrations and deploy

**[`integrations/` may use outside packages; the crates may
not.](../docs/design/integrations.md#integrations)** A builder change starts
in `web/builder.js` and a case in `golden.mjs`, then every SDK's follows,
held to `integrations/builder-golden.json` and `make types-check`. A
builder tab on the site is a golden case's chain rendered by
`site/builder_tabs.py`, never written by hand: its statement held to the
page's or written from the case, the JS and Python tabs run by its
`--check`, the others tests of their SDKs (`make docs-builder`); a
builder's API change owes its renderer. Go's and .NET's SDKs bound a request by its context or token, never the HTTP client's
timeout; Python's, Go's and .NET's clients, and Swift's, Kotlin's and
Dart's `FennecRemote`, send a request again by `FennecHttp`'s rule
(web/CLAUDE.md); a store checks its names against FennecQL's and passes
values as parameters.

**[The desktop app is a window on a studio a `fennec-server` serves, one
server per file.](../docs/design/integrations.md#studio-desktop)** In
`integrations/studio-desktop` only `server.ts` starts a server, its token
in an allowlisted environment (never argv), its stdin a pipe the app holds
(`--exit-on-stdin-close`: none outlives the app, `kill -9` included); only
`session.ts` gives a page a token, a frozen `window.fennecHost` defined
where `location.origin` is the server's alone. A studio window is
`sandbox: true` (the event bridge only) and held to its origin, one with
the RPC bridge to its `views://` folder (`holdTo`). A saved server's token
is in the Keychain alone with its address (`token.ts`), never sent
elsewhere or along a redirect; a failure is a notice window, never a
native alert. Each window has its own data store, a removed connection's
cleared after its window closes by `STORE_KINDS`, never
`clearStorageData("all")` (`checks/storage.mjs`). The shipped bundle holds
no test hook, such as an environment read outside `__TESTABLE__`
(`checks/release.mjs`). Each check starts with none of the test variant's
state, its `state.json`, Keychain items and WebKit stores (`freshState`), so
one that fails half-way fails alone; a failed one keeps the system log of the
app and of WebKit's processes (`keepEvidence`), the main record of a page that
never loads. The toolchain is fetched into `.hutch-home/`,
never `~/.hutch` or a shell file, and held to `hutch-pin.json`, a pin of
every system it builds on, Hutch and what Hutch fetches, a system without
one refused (`scripts/electrobun.mjs`); signing runs by hand from `main` alone, in an
Environment given its reviewer and `main` rule before its secrets
(`desktop.yml`). The DMG a release ships is the app unpacked, never
Electrobun's self-extractor (a quarantined copy must run read-only), each
Mach-O and then the bundle signed ad hoc (`scripts/dist.sh`), held as it
ships by `checks/dist.mjs`: its signature, no hook, a launch and quit from
the read-only image and from a copy left unchanged. It is attached only
from `main`, where the signing job ran and signed nothing, by a job with no
secret; its line is appended to `SHA256SUMS`, which only grows, and a DMG
there is never replaced.

**[Grafana's data source binds a dashboard's values; it never splices
them.](../docs/design/integrations.md#grafana)** Names become `$n`s
(`fennecql.go`); only `get`, `select` and `explain` go out.
