# Integrations and deploy

The long form of the invariants [integrations/CLAUDE.md](../../integrations/CLAUDE.md), [deploy/CLAUDE.md](../../deploy/CLAUDE.md) and [tools/CLAUDE.md](../../tools/CLAUDE.md) list under *Integrations and deploy*: `integrations/`, the Grafana data source, the desktop app, the `deploy/` kit and the install channels. Each section below is a paragraph moved word for word from CLAUDE.md, which keeps its rule in a few sentences and links here; "above" and "below" in them refer to the order the invariants had there, which [README.md](README.md#the-original-order) lists.

## Integrations

**`integrations/` may use outside packages; the crates may not.** The
LangChain and LlamaIndex vector stores (`integrations/python`, one package,
the standard library for its client) and `useLiveQuery`
(`integrations/react`) are held to their frameworks' own tests, and each
language's docs example to a real server (`integrations/languages`: the
same flow over HTTP in Python, JavaScript, Go, C#, Java, PHP, Ruby and
Rust, each with its own HTTP client) -- `make python-test` runs LangChain's
standard suite and the tests LlamaIndex's integrations run from a
`python:3.13` container against a fennec-server started here, `make
languages-test` the eight examples (Python, Java, PHP and Ruby from their
images, the others with the toolchain on the machine), `make react-test`
runs the hook against a real replica, and CI runs all three
(`integrations`). The Go and .NET SDKs (`integrations/go`, module
`github.com/fennec-db/fennecdb/integrations/go`, package `fennecdb`;
`integrations/dotnet`, NuGet's `FennecDb`, `net8.0`) are the standard
library alone, the Python client's surface -- a statement and its rows,
typed rows, writes with their `Fennec-Seq`, `/batch`, an idempotency key,
`After` for a replica, a subscription, `/_changes`, a tenant, `/_health`,
a typed error -- and write a `float32` as the shortest text that reads back
as it through an `f64`, which is how the server reads a number, but for
`json::TIE`, which goes as its `f64`'s text: `encoding/json` wrote its
shortest `f32` text and the server stored the float above. A request is
bounded through its context or token, never the HTTP client's timeout,
which would cut a subscription short -- each try's own, its head and each
piece of its body -- and sent again by `FennecHttp`'s rule, as Python's
is ([js-client.md](js-client.md#retries)). `make go-test` and `make
dotnet-test` start the servers they need (`FENNEC_SERVER`, else
`target/debug/fennec-server`), CI runs both, and `make languages-test`
runs the docs' Go and C# examples through each SDK too (`go-sdk`,
`dotnet-sdk`). Python, Go and .NET have the JS builder too
(`fennecdb/builder.py`'s `db.collection(...)`, Go's `db.From(...)` and
.NET's `db.From(...)`), and all four make the same text of the same chain,
to the byte, refusals by the same message: `integrations/builder-golden.json`
holds the chains (`{op, args}`, the JS builder's names, what JSON has no
word for an object of one `$` key) and what the JS builder made of each --
`web/golden.mjs` holds the chains and writes the file (`make
builder-golden`), `web/fennec.test.js` runs every case through the JS
builder again so the file cannot drift, and `make python-test`, `go-test`
and `dotnet-test` run every case through their own builder, the
endpoints' statements read off a recording transport. A builder change
starts in `web/builder.js` and a case in `golden.mjs`, then the three
follow, and the Swift, Kotlin and Dart builders with them. Six forms a
page wrote and no builder could (#409's list) came in together, JavaScript,
Python, Go and .NET first and Swift, Kotlin and Dart after (a Swift
`Value.param(300)`, a Kotlin `param(300)`, a Dart `Param(300)`; Swift's
`Cond.in(field, query)`, since a `Query` is no `Value`, where Kotlin's and
Dart's maps hold one): `param(value)`, a value bound once wherever it
is used -- the binder keeps each param's `$n` by its identity, so the same
value given twice stays two parameters, as before -- spelt
`{"$param": [name, value]}`, one param a name within a case; an inner
query of `in`, which JavaScript alone took, spelt `{"$query": [steps]}`,
its parameters numbered where its text stands; `insertNew`, FennecQL's
`insert` (writes.md); an `order` key that is an expression; a `fuse`
written in the order its `match` and `near` were first called (the cases
that called `match` first were turned to call `near` first, keeping their
text); and `fuzzy: true` as FennecQL's `fuzzy` alone, a number of edits
still written (`fuzzy 2`, the case that wrote it and the tour taking `2`).
No case's text changed: the builders not yet taught failed only the new
cases. A sync replica needs no builder of its own for `insertNew`: the
native bindings run its text through the one state machine
(`fennec_abi::sync`), which `sync-scenarios.json` holds, and each binding's
sync test only shows its `insertNew` meets it. Go's object conditions are `Fields`/`Ops` and a document a
`D(...)`, names and values in turn, since a map has no order and the order
is the text's; its options are functional, since a struct cannot tell
`limit 0` from none. CI also builds the five packages as a release
publishes them and installs and uses them (`integrations/packages.sh`):
PyPI's `fennecdb`, npm's `@fennecdb/web` -- the client, the module and
`collate/`, `web/package.json`, and no test build -- `@fennecdb/react`,
`@fennecdb/cloudflare` and `@fennecdb/langchain`. They go out when a
release's draft is published (`packages.yml`), only after that same check,
with a token where the registry's secret holds one (`NPM_TOKEN`,
`PYPI_API_TOKEN`), with the workflow's OIDC identity (trusted publishing)
where it does not (RELEASING.md). NuGet's `FennecDb` is packed, installed
into a fresh console app and used against a server
(`integrations/dotnet/package.sh`) and only then pushed with a key
nuget.org's trusted publishing gives the job for an hour (`NuGet/login`),
or `NUGET_API_KEY` where the exchange fails, its job failing with neither,
and pushed by a job of its own that builds nothing; the Go
module has no registry, and `release.yml` pushes the tag
`integrations/go/vX.Y.Z` it is fetched by beside `vX.Y.Z`. With
`full_text=True` a store indexes its text for BM25 as well and searches by
the words (`match`) or by the words and the vector fused (`fuse`):
LlamaIndex's `TEXT_SEARCH` and `HYBRID`, LangChain's `mode="text"` and
`"hybrid"`. `alpha` is not read, since `fuse` adds ranks; `quant` passes
`int8` or `bit` codes to `@hnsw`. The same store for LangChain.js
(`integrations/langchain`, `@fennecdb/langchain`) runs over anything with
`run(sql, params)` -- a `Fennec` keeps a RAG index in the page -- and is
held, LangChain.js publishing no standard suite, to what its own vector
store integrations are tested for, over a database in the page and over a
fennec-server's HTTP endpoint (`make langchain-test`); the Vercel AI SDK has no
store interface, so `integrations/ai-sdk/rag.js` is three functions to copy,
held to the SDK's mock models (`make ai-sdk-test`). `web/fennec.d.ts` is
held to `web/fennec.js` by `make types-check`: every export and class method
read at run time and touched from a generated file, and a caller's code with
the lines the types must refuse, under `tsc --strict` -- the module stays
JS with no build step. A store names
its collection and metadata columns in the statement's text, so both are
checked against FennecQL's name pattern; values always go in as parameters,
and `in` takes one per element (`in [$2, $3]`), since a parameter binds a
value and not a list.

The FennecQL reference pages show an example's statement beside the same
query in each builder, in tabs, and Your language's builder section is a
tour in the same tabs. The tabs are written from golden cases by
`site/builder_tabs.py`, from `{{builder "case" ...}}` above the page's
block. Snippets written by hand in seven languages would drift from the
builders, which is what the golden file is there to stop; a tab rendered
from a case can be held to it.

- **A page's statement is the one the builders send.** The build holds
  each case's statement to the page's: the text, each parameter written in
  as FennecQL writes the value or matched to a `$n` the page has there. So
  only an example the builders make byte for byte gets tabs. Where the
  builders' order reads as well, the page took it: `order x asc` with its
  direction written, `facet` after `limit` and `order`, `select` before
  `where`, and a `lookup` level's list before its `order` and `limit` (the
  second pass, which also wrote JSON keys bare and split each
  `create`/`alter` group off into a block of its own, since no builder
  writes one). The rest stays as written, with no tabs, where the builders
  have no form for it or the page leans on its own wording: `explain` with
  its plan, a `create` or an `alter`, and a block holding any of these,
  which is not split to tab the rest. `fuzzy` without a number, `match`
  before `near` in a `fuse`, `insert`, one `$n` in two places, `in (get
  ...)` and an `order` key that is an expression were on that list until
  the builders learned them (row 68): seven blocks took tabs then, a
  `not (x in (...))` and an `order` key's direction written into the page
  as the builders write them.
- **A form reaches the builders in two changes, and a tab waits for its
  SDK.** The six forms came to JavaScript, Python, Go and .NET first, and to
  Swift, Kotlin and Dart in a second change. Between the two, a box whose
  cases used one had no tab in those three languages and their test files
  left the cases out: a tab is code its SDK's tests ran, and a call the SDK
  has not got would compile nowhere. A form the next time reaches the
  builders in two steps takes the same: a list of the languages that wait,
  and a test of whether a case needs the form, in `builder_tabs.py`, both
  deleted when the last language has it.
- **A placeholder over no block writes its FennecQL tab.** languages.html's
  tour of the builder has no statement of its own in the prose, so
  `{{builder ...}}` alone on its line makes the FennecQL tab of the cases:
  each statement with its parameters written in as FennecQL writes them (an
  object's key bare where it is a name, quoted otherwise), broken before a
  clause where it passes 72 characters, never before the first, and before
  every `lookup` level. FennecQL's strings escape `\n`, `\t`, `\r` and `\0`
  and have no `\u`, so a parameter text holding any other C0 control,
  U+0085, U+2028 or U+2029 fails the build: the block is the page's to
  write. `selftest` holds the refusal, the escapes, the quoted keys and the
  lines. The tour's collections are a block of their own above it, since
  its `if absent` passes over a row only where `slug` is `@unique`. The tour replaced two tables of the seven builders' spellings side
  by side, a call a cell: cut off on the right at a phone's width, written
  by hand and run by nothing.
- **Each language is proven its own way.** The JavaScript and Python tabs
  run as the page shows them, against `connect` with a recording `fetch`
  and a `Client` whose `query` records (`builder_tabs.py --check`, CI's
  integrations job). The Go, .NET, Swift, Kotlin and Dart tabs are tests
  of those SDKs: `make docs-builder` writes
  `integrations/go/docs_builder_test.go` and its four siblings, and each
  SDK's job runs its file against the SDK's recording transport.
- **A stale test file fails the check.** The comparison is token by token:
  whitespace outside strings and comments is dropped, since `gofmt` and
  `dart format` lay the files out, and so is a comma before a closing
  bracket, which `dart format`'s tall style (a 3.7 constraint on) adds.
  The first version dropped all whitespace, and a space put into a
  string passed it.
- **Every string goes through one escaper.** The C0 controls, U+0085,
  U+2028 and U+2029 are written as each language's `\u` escape; C#
  refuses U+2028 in a string literal. Two cases of the check's own, not
  the golden file's, run a text of every such character through every
  language's tab code, in the tests and in `--check`.
- **What a tab shows.** A tab's case ends at an endpoint (`rows`,
  `count`, `update` ...), since an app runs its query. The tab shows that
  call on `db` (and Go's `ctx`), under a label naming its import. The
  statement it sends is written once under the panels; the widget leaves
  it showing under every language's tab and CSS hides it under FennecQL's.
  A copy under each tab put six more of 3.2 KB of statements into the
  page text, its search sections and `llms-full.txt`: 19.4 KB.
- **One language name site-wide.** The tabs share the reader's remembered
  language (`fennec-lang`) with the Quickstart's and Your language's, so
  each names JavaScript one way: "JavaScript". Your language's tab still
  shows TypeScript code; its file labels say so.
- **A one-off sweep.** When the renderer was written, every golden case it
  can render (222, leaving out those with a `$date` or a `$f32` value) was
  rendered in all seven languages and run through each SDK. Each sent its
  case's text and parameters.

## Grafana

**Grafana's data source binds a dashboard's values; it never splices them**
(`integrations/grafana`, plugin id `fennecdb-datasource`). The Infinity
plugin wrote variables into a body's text, sorted a frame's columns by name
and alerted on nothing a browser fills in, so a backend plugin (Go,
grafana-plugin-sdk-go 0.297, the Go SDK through `replace ../go`) and a
TypeScript frontend in `@grafana/create-plugin` 7.12.2's layout run the
queries server side. The backend reads a statement only where it must agree
with fennec-ql's lexer -- strings, comments, words, a `$` (`fennecql.go`) --
and makes each name a `$n` after the editor's parameter list: `$__from`,
`$__to`, `$__interval_ms`, `$__max_data_points` from the request, which an
alert rule has too, and `$sym` or `${n:int}` from the values the browser
sends beside the query (`values.ts`), a multi-value one after `in` a bracket
of them. Only `get`, `select` and `explain` are sent. The answer is read in
its order (`answer.go`: encoding/json's maps lose it, so the SDK grew
`QueryRaw`/`BatchRaw` and `IfNoneMatch`) and typed by the
`collections` schema, kept 30 s, where a column is a field and by its values
where computed; a group by time and a text column is a series a label (long
to wide), which a rule evaluates an instance a symbol. One query goes with
its last answer's `ETag` and a 304 is answered from the kept body: 3-5 ms
with the server's token against 10-82 ms run, nothing gained with a scoped
JWT, whose tag the server makes by running the query (`make grafana-check`'s
stack, `monitoring/grafana/bench.mjs`); several are one read-only `/batch`.
The client follows no redirect and sends the token alone, to the configured
URL and the plugin's paths. `monitoring/` builds Grafana with it
(`integrations/grafana/Dockerfile`), and `make grafana-check` holds every
panel's, variable's and annotation's frame to fennec-server's own answer, in
the select list's order, and the provisioned rule's firing instances to the
server's rows. A jest test importing `datasource.ts` -- `@grafana/runtime`
and `@grafana/ui` with it -- sat at 0% CPU for ten minutes, so the logic
jest runs lives in modules importing types alone. `site/build.py` writes
the editor's FennecQL colours into `src/fennecql/highlight.js` as it writes
the studio's.

## Studio desktop

**The desktop app is a window on a studio a `fennec-server` serves, one server
per file** (`integrations/studio-desktop`, Electrobun 2.0.4, macOS on Apple
silicon, beta). The app bundles `fennec-server` and starts `fennec-server
--file <path> --studio --http 127.0.0.1:0` for each open file, with a random
32-byte token in `FENNEC_HTTP_TOKEN` (the environment: `ps` shows every user's
arguments), finds it by the `listening on:` line on stderr, as every harness
of this repository finds a server on port 0, and opens a window on its own
`/_studio/`; closing the window sends SIGTERM (on Windows ends its stdin: the
process and security layer, below), which syncs and writes the
checkpoint. A saved server is a window on that server's own studio, same
origin, so no CORS, its token in the system's credential store, the Keychain on
macOS (`Bun.secrets`, service the app's
identifier, name `connection:<id>`: Electrobun has no keychain API, and the
`security` command-line fallback was dropped because `add-generic-password -w`
puts the secret in argv). The server is started in `server.ts` and a token reaches a page in
`session.ts`, each the one place. The server is given `--exit-on-stdin-close`
and a pipe on its stdin that the app holds for its life: a killed process sends
its children no signal, but the system closes every descriptor it held, so the
server sees the pipe end however the app ended and shuts down as on SIGTERM
(sync, checkpoint, `<file>.lock` let go, exit 0). The first design noted each
server in a file and stopped the strays at the next launch (SIGTERM, a wait,
SIGKILL, what survived kept): it went when this passed, and with it the file,
since no release of the app ever had a server that needed it. The write end must
stay in the app alone: `checks/orphan.mjs` opens two files one after the other,
reads the descriptor tables (lsof) to see that each server's stdin is written to
by the app's processes alone, with a control that starts a process holding
another's write end and requires the same reading to find it, then kills the app
and needs both servers to end and both files to be free (a descriptor not marked
close-on-exec would have the second hold the first's pipe). Both ending after the
kill is no proof of that, since the second ends when its own stdin does and lets
the first's go; the first version stopped the second server (SIGSTOP) and required
the first to end meanwhile, and on a hosted macOS runner a stopped process made
`ps` hang, which hung the runner's own cleanup and with it the job (it never
finished, its timeouts included), three times in a row.
A database served with `--metrics` says where they are on a line of its own,
`metrics on:`, which the metrics listener's thread writes as it starts serving,
as the data endpoint's thread writes `listening on:`: the two come in either
order. The start first took the server as up on the data line and read the
metrics' only before it, so a start whose metrics thread ran later had none:
the process test failed so twice on the hosted runner, and beside 8 or 16 busy
loops in 2 runs of 400, where the metrics' line came last in 8 starts of 200
and the start itself, looped, came back without them 4 times in 300. It now waits
for every line its plan asked for, within the start's 30 s, and a server that
never names its metrics is stopped and the start fails saying so; a line is
read for where the server listens only until then, after which what one says
is the data's. Under the same loads the test then passed 400 runs of 400 and
the start had its metrics 300 times in 300. MCP has no listener of its own (it
is POST /mcp on the data endpoint), so nothing else is waited for.
The exit codes (server.html: 0 stopped as asked, 1 could not start, 2 a flag
refused) are the notices': a held lock is "open in another program", 2 is "out of
step", the rest "could not open", and `get-server.sh` refuses a server without
both flags (the first release with `--studio` is 0.1.11, with both 0.1.12). The
session is a frozen `window.fennecHost` that the studio reads before storage,
defined by the preload only where `location.origin` is the server's (a preload
runs in every page the window loads, and a page of another origin that found
`fennecHost` would read the token) and never stored: `test/session.test.ts` runs
the script and then the studio's own `connect.js` on what it made,
`checks/sandbox.mjs` asks the running window (Electrobun's preload runs in the
page's world: an isolated one would show the studio no session).

The server's environment is an allowlist (`HOME`, `PATH`, `TMPDIR`, `LANG`,
`LC_*`, `USER`, `LOGNAME`, `TZ`, and the token), not "everything but ours": the
server reads `OTEL_EXPORTER_OTLP_*` (which turns trace export on) and
`FENNEC_JWT_SECRET`, and an app started from a shell must not hand a database it
opens whatever that shell exported (`env.ts`; `checks/environment.mjs` starts
the app with such variables and reads the server's own environment back through
a wrapper script). A quit waits for every server, one being started and one whose
window closed a moment ago included (`serversBusy`, `stopAll`), and an open of a
file waits for that file's server to finish stopping: a second server started
over the first's checkpoint would be refused, and the user told the file is open
elsewhere.

Windows. A studio window is `sandbox: true` (no call into Bun, only the
event-only `__electrobunSendToHost`, which `checks/sandbox.mjs` holds to the
six globals Electrobun's sandboxed preload defines) with navigation rules for
the server's origin and links out as web addresses to the browser, at most one
in three seconds from a window (2.0.4 does not say whether a click asked for a
`new-window-open`, and a page could otherwise open tabs without end). It does
not answer for `views://`, whose handler sends `Access-Control-Allow-Origin: *`.
**Every window of the app is held**, not only a studio window: the launcher and
the new-database window have the RPC bridge, and Electrobun's full preload is
added to every document a window loads (not the main frame alone), so a link or
a file dropped on one is a navigation, and what it loads is handed `openPath`,
`openConnection` and the saved servers. Each is held to its own `views://`
folder (`holdTo`; the constructor's `navigationRules` option did not take in
2.0.4, a navigation to another origin reported `allowed: true`, the setter does)
and `checks/pages.mjs` fails, as the launcher goes to example.com, if the rule is
taken away. A `file:` address is not even asked about: WebKit refuses it from a
page of another scheme. An origin that holds a `*` or a `?` is refused: it is
the rule's glob. Failures are windows of
the app's own (`messages.ts`, a bundled sandboxed page that draws with
`textContent`), never `Utils.showMessageBox`: that is a synchronous native call
in an `async` function, so the main process's JavaScript, every server's
supervision and the quit handler stood still until someone clicked (a check that
never clicked held the app for good). `exitOnLastWindowClosed` is false: the
default quits as the last window's close handlers return, too soon for a
server's SIGTERM to finish while the app still holds its lock; on macOS the app
stays in the Dock, as a document app does, and on Windows and Linux its last
window closed is a quit of its own (Desktop integration, below); either way it
stops its servers itself on quit (`before-quit` held until they have). A second open of a file the app is still
starting a server for waits for that one, and so does a second click on a saved
server while it is being looked at (a few seconds, up to the probe's timeout):
both are in `starting`, one open of a thing at a time.

The data store. What a page keeps (`localStorage`: the studio's query history,
saved queries and theme) lives in a WebKit data store, and `BrowserWindow`
makes its page's view in the one default store, told apart by origin alone: a
file's server listens on a port the system picks, so a window on a port used
before would show the earlier file's history. `BrowserWindow` has no `partition`
option, so the window is made with no page and the view made in it with one
(`BrowserView`'s `partition`, `autoResize`, `windowId`): a name without
`persist:` is `nonPersistentDataStore`, one store for that window, which a file
gets; `persist:remote-<id>` is `dataStoreForIdentifier`, kept between runs and
apart from every other saved server's, cleared when the connection is removed.
**`clearStorageData("all")` does nothing in Electrobun 2.0.4**: the native side
(`sessionClearStorageData`) takes a comma-separated list of exactly `cookies`,
`localStorage`, `sessionStorage`, `indexedDB`, `cache` and `serviceWorkers`
(its strings are those six and no "all"), and a name it does not know is
skipped, so the first version left a removed connection's history in the
store, to be shown to a new connection that got the same id's store (ids are
random, but the README promised removal clears). The list is explicit
(`STORE_KINDS`), the connection's own window is closed first (a page writes
again while its store is cleared), and the Keychain item goes before the store. The blank view Electrobun made for the window is
removed on Windows and Linux and kept on macOS (the window's own view, below).
`checks/storage.mjs` holds the stores apart by reading a store from
outside the window that wrote it: a test hook (`store-window`) opens a second
window on the same server's origin in the store it is told, so under one shared
store (the thing the partitions are for) the file window's history would show,
and a window in the very same store is the positive control. A removed
connection's store, opened again under its id, is empty, with a window open on
it at the removal or not. Two saved servers on one address do not share, a saved
server's survives its window, the page is the window's size and has focus.

The window's own view. `BrowserWindow` makes a blank view in the default store
under the page's. Windows and Linux remove it (`win.webview.remove()`): their
Electrobun removes a view on its UI thread, in order after making it (Linux by
`g_idle_add`, Windows through `dispatchWebviewOperation`). **macOS keeps it.**
There Electrobun 2.0.4 makes a view's WKWebView in a block WKWebViewImpl's
`initWithWebviewId:` queues on the main queue (`dispatch_async`), and
`webviewRemove` runs at once on the thread that asks, Bun's, with no hop to the
main thread. A removal straight after the window, as the app asked until now,
mostly did nothing: `-remove` returns while `self.webView` is nil, and the view
made after it stayed (a probe of the WKWebViewImpl's `nsView` through libobjc,
read at the removal: not made for the first window of every launch, and for
later ones 0 of 40 in one session, 2 of 60 beside six busy loops, 40 of 40 in
another). Falling inside the block, it cleared `webView` under it, and the
block's last `objc_setAssociatedObject(self.webView, "WKWebViewImpl", self,
...)` was handed nil: SIGSEGV at 0x0 in that block (`libNativeWrapper`, offset
0x16fd8). The finder check met it once on a hosted runner (run 43182618196, the
crash report among its evidence: the app died 0.8 s into its second launch,
which shows the launcher first and so warms WebKit, and the check saw only a
server that never started). Here a pause before the removal did it: 0 to 4.9 ms,
0.1 ms more each round of a file window opened and closed with the launcher up,
crashed 3 sessions of 4 (at rounds 35 to 88), and a fixed 3 ms a fourth, each at
the same frame and offset. Removed after the page's view is made, it is safe, as
that is `initWebview`'s `dispatch_sync` onto the main queue, which runs the
blank view's block first (1 020 rounds with the same pauses: every view made at
its removal, none crashed); but then it costs every open the removal's work and
frees nothing. On an M1 at load 1.4 to 2.0, the 50-row file opened 8 times a
session with the launcher up: 239-241 ms to the studio's collections kept,
267-273 ms removed (medians, 4 sessions each, interleaved); a launch with the
file 407-417 ms against 423-436 ms (3 each); idle RSS 256-258 MB after a launch
and 316-319 MB after the opens, either way. So it is kept, held to no address
(`^*`, which Electrobun sets on the main thread after the block); it loads
nothing, so the default store gets nothing from it, and the core removes it with
the window. The test variant reports whether a window kept it (`own-view`, read
when the window is made), and `waitStudio` holds every studio window a check
opens to it: kept on macOS, removed elsewhere. Put back straight after the
window, the removal fails the finder check on its first window, whatever the
timing. A failed check also says now whether the app still runs (`context`),
which the timeout did not.

The saved server. The probe asks `/_studio/` (served before any token), then
`/_whoami` **without** the token and sends it only if fennecdb's own 401 (a JSON
body naming an error) comes back, and follows no redirect (`redirect: "manual"`,
a message that says where, as an origin): an `Authorization` header goes wherever
a redirect says, and a proxy's sign-in page or a moved address would be told the
token. A token is for the address it was saved with: a connection pointed at
another origin with no token typed is refused (`tokenMayStay`), since keeping it
would send the old server's token to the new one; and the Keychain item holds
the token **with the origin it was saved for** (`token.ts`, `{origin, token}`),
read back only for that origin, and the connection is read again after the
Keychain answers (a Keychain read can wait on the user), the open stopping if
its address changed meanwhile: a token from an edit that landed between the two
never reaches the address it was not saved for. A saved token is taken away
alone with "Forget token" (a blank token field means keep it, as it did, so
there was no way to remove one but to delete the connection). The Keychain keeps it from
other users and from pages, **not from other programs of the same user**: the
client is the bundled `bun`, a general-purpose runtime that any such program can
run with a script of its own (`Bun.secrets.get` with the service and the id that
`state.json` names), and nothing here makes the Keychain ask first. Whether a
signed build's items are bound to its identity, so that the system asks, is a
thing to look at with the first signed build. There is no fallback to the
`security` command-line tool, whose `add-generic-password -w` puts the secret in
argv.

New database. Electrobun 2.0.4 has no save dialog, so the window takes a folder
from the folder dialog and a name, and `createDatabase` makes the file with
`open(path, "wx")`, which refuses a name that exists whatever its case (APFS),
and then starts a server on it: `fennec-server` takes a zero-length file as a
new database (an 8-byte head). It does not truncate or otherwise write to a file
that is not one: the signature is read first and refused (`invalid fennecdb
signature`), and the app says so and nothing changed (a check holds it). A server
that then fails to start does not leave the empty file behind. Open looks first
(`dbfile.ts`): `fennec-server` makes an empty file into a database and leaves
`<file>.lock` beside any file it opens, so a file that is empty (and not one New
database just made) or does not start with `FENECDB` is a message and a server
is never started on it; the panel is limited to `.fennec`.

The test variant. The checks drive the app through a control file and read its
reports, which a shipped app must not read: `build.bun.define` makes
`__TESTABLE__` a constant, `testhooks.ts` is imported only behind it, and
`FENNEC_DESKTOP_TEST_BUILD=1` builds the same app with it true under its own
identifier (so its `userData` and Keychain service are its own: a run never
touches an install's recent files or saved servers), into `build-test/`.
`checks/release.mjs` reads the shipped app's main bundle (out of the
self-extractor's `.tar.zst`, without running it) and fails on any hook marker,
and on the test variant's lacking one, so a rename cannot make it pass by
looking for what was never there. The checks wait for events -- a fact the app
reports, a process leaving, a page drawing -- and wait out a time only for what
must not happen (`waitNone`, 1.2 s, a link that must not be opened).

A check fails alone. The test variant keeps state between launches -- its
`state.json` (recent files, saved connections, the databases it served), its
Keychain items (service `dev.fennecdb.studio.test`) and its WebKit data stores
(`~/Library/WebKit/dev.fennecdb.studio.test`, one directory a `persist:` store,
named by the SHA-256 of the store's name) -- and `run.mjs` used to empty only
the first, once, before the first check. In run 42876456804 `storage` failed
after saving a connection and before removing it, and `remote`, two checks
later, failed on its last assertion (no saved connections) for that
connection. So `run.mjs` empties all three before each check (`freshState`),
`state.json` alone of its folder, whose rest is Electrobun's (its
self-extraction cache, its uninstall notes) and no check's state, and which
goes once a run, before `warmup`, as it went before (a CI run emptying the
whole folder before each check passed too, so this is less removed under
`~/Library`, not a fix):
the Keychain is emptied by service, one `security delete-generic-password -s`
an item until none is left, which matches the service exactly (checked: an item
under a longer service that begins with it stays), so a real install's
(`dev.fennecdb.studio`) is never touched. A `storage` made to fail where that
run did, then `remote`: on the old `run.mjs` both fail, `remote` on the same
assertion; with `freshState` `storage` fails and `remote` passes. The stores
went with nothing before: a Mac that ran the checks for a few weeks held 865 of
them (201 MB), every saved server's a check ever made, each emptied by its
removal and left in place.

A failed check keeps what the app cannot say (`keepEvidence`, before `run.mjs`
kills what is left): the unified log of the app and of WebKit's processes since
the check started, a `sample` of the app's `bun` while it runs, and any crash
report written meanwhile, all in the logs CI uploads. A window whose page never
loads is otherwise silent: Electrobun 2.0.4's navigation delegate implements
neither `didFailProvisionalNavigation` nor
`webViewWebContentProcessDidTerminate`, so a load WebKit gave up on, or a web
process that ended under it, reaches the app as nothing at all. WebKit's own
log has each step (`WebPageProxy::loadRequestWithNavigationShared`, the web
process launched, `decidePolicyForNavigationAction` -- the app's
`will-navigate` -- then the navigation handed to a second, new web process,
`ProvisionalPageProxy::didCommitLoadForFrame`, `didFinishLoadForFrame`), and
the test hooks report `did-commit-navigation` as well, so the next such failure
says where its load stopped. That is what run 42876456804's `storage` was: the
first window in a new `persist:remote-<id>` store reported `will-navigate`,
allowed, and nothing more for 30 s. A slow load would have shown a tail: in 115
passing CI runs, the step it failed in (two windows in a new persistent store)
took 1.7 to 6.0 s, p99 5.8 s. It did not come back in 350 tries:
- 215 on a MacBook Air (macOS 26.1), each beside 12 CPU and 2 fsync busy loops,
  load average 15 to 30:
  - 65 `storage` runs on the old `run.mjs` and 60 on this one;
  - 70 new stores' windows in one process;
  - 80 launches that opened the first new store from an empty WebKit directory;
- 135 `storage` runs on hosted runners (macOS 26.7.2), each from an empty state.

So its cause is not known. The check's server was answering (a thread per
connection, an accept loop that reads nothing), the navigation rules allowed
the load, and the window and its view are made as every other window's are.
What the app could not see is the evidence these hooks now keep.

Windows and Linux on CI (phase 1 of the plan in #578, nothing shipped).
`desktop.yml`'s `other-systems` job builds the shipped app and the test
variant on `windows-latest`, `windows-11-arm`, `ubuntu-24.04` and
`ubuntu-24.04-arm` with a release's server and shell (pinned in the workflow;
the macOS job bundles this checkout's, and here the app is what is under test),
and runs the type check, the unit tests and the checks. `get-server.sh` takes
the release's Windows zip, held to `SHA256SUMS` as the tarballs are, and
unpacks it with `unzip` or, in Git's bash where there is none, Windows' own
`tar.exe` (bsdtar reads a zip); the machine's processor is read from the
registry, since an x64 Git runs emulated on Windows on Arm, where its
`PROCESSOR_ARCHITECTURE`, and Windows PowerShell's `OSArchitecture` (emulated
too), said x64 and fetched the x64 server. The checks' platform layer is
`checks/lib.mjs`: where the app is and how it is installed, its folders, the
processes (`ps`; on Windows WMI through PowerShell 7 where there is one, since
Windows PowerShell, emulated on `windows-11-arm`, took 42-45 s a query where
`pwsh` took 0.6 s, both 0.4 s on x64), the credential
store, a killed process's exit code (TerminateProcess's 1 on Windows), a path's
separator, and whether the file system tells case apart, asked of the folder
(`newdb`'s `FRESH` is another file on ext4). A stable build there is an
installer, so `warmup` installs the test variant with it and every other check
runs the installed copy: on Linux the installer starts that copy, on Windows it
said "Installation completed successfully" and then, 120 s on, had neither
started it nor ended, so `warmup` waits for either, ends both, and starts the
installed app itself. `freshState` removes the pages' stores (Electrobun keeps
WebKitGTK's under `$HOME/.local/share/<id>/stable/WebKit` and
`$HOME/.cache/<id>/stable/WebKit`, HOME and not `XDG_DATA_HOME`, and WebView2's
under `%LOCALAPPDATA%\<id>\stable\WebView2`) and the credential store's items
of the test variant's service: `secret-tool clear service <id>` (Bun.secrets
keeps libsecret's attributes `service` and `account`, matched exactly) and the
`cmdkey` targets `LegacyGeneric:target=<id>/...` (Bun's generic credential is
named `<service>/<name>`). Checked once on each system with items of the test
variant, of a service one character longer and of a real install's: the first
went, the others stayed (runs 43044414083 and 43060489878, a step since taken
out). `secret-tool clear` with no item to clear exits 1 and says nothing, which
is no failure; a store that cannot answer says why. On Windows a process of the app is also one that names
its folder, since WebView2's processes are told their user data folder and
outlive the app by a moment, and `run.mjs` waits for what it killed to be gone
before the next check removes that folder. A check that cannot pass on a system
until a later phase is skipped there by name, its reason and phase in
`run.mjs`'s list and the run's SKIP lines; a unit test likewise, its reason
beside it in `node --test`'s output (`test/platform.ts`).

The process and security layer (phase 2 of #578, nothing shipped). What
macOS gave the app for nothing is made on Windows and Linux, and every phase 2
skip went; what each rests on was measured on the four runners (runs
43072853201 and 43077312854, a probe of each call beside the checks, and
43080486766), and each is held by a check or a unit test on every system it
applies to.

- **The stop.** On Windows libuv's and Bun's `kill("SIGTERM")` is
  TerminateProcess: the spike's server ended in 1 to 3 ms with no shutdown line,
  and phase 1's `newdb` lost a new database's first collection, still buffered
  under `--sync 250`, that way. The end of the server's stdin is its one
  shutdown there (`--exit-on-stdin-close`, docs/design/windows.md, decision 4),
  so `process.ts` ends the pipe it holds (`Child.endStdin`, `STOP_BY_STDIN`)
  and keeps TerminateProcess for a server that has not ended 30 s later; the
  unit tests hold the stop to exit 0 on every system (TerminateProcess is exit
  1), and `files`, `newdb`, `serve` and `backups` now run on Windows. On both
  Windows runners the stop was exit 0 with the file free to the next server at
  once, and `newdb` found the first collection a server stopped so had
  buffered.
  Not taken: a console control event (CTRL_BREAK) needs the server in a
  console of its own the app shares, where Electrobun starts Bun with
  CREATE_NO_WINDOW; and the server has no route that stops it. The shell's
  one-shot runs are still terminated at a quit: they hold no lock past their
  run and write nothing in place.
- **The environment.** Windows reads a variable's name case-blind and sets
  `Path`, which the allowlist, compared as macOS's, dropped. `env.ts` compares
  names case-blind there and names Windows' own for the system's, temporary
  and user's folders: libuv's `required_vars`, which it hands a child from the
  parent whatever environment it is given, and `TMP`. `env.test.ts` holds each
  system's rule on every system (the platform is a parameter); `environment`
  reads the server's environment back through a stand-in, on Windows a program
  the app's Bun builds (`bun build --compile`), since Bun starts no script there
  without a shell. On x64 the server was handed its token, `HOME`, `PATH` and
  Windows' eleven, and nothing else. On arm64 it held `PROCESSOR_ARCHITECTURE`
  (`ARM64`) as well, the stand-in a native arm64 program: Windows sets it in a
  process's environment whatever the process is handed, which the check's
  control shows by starting the stand-in with `SystemRoot` alone, and the check
  allows only what that control holds.
- **One instance a user.** Linux's socket was in `/tmp`, which every user
  writes to; it is in `$XDG_RUNTIME_DIR` (the login session's, 0700), else the
  app's own data folder where that is the user's and no one else may write to
  it, else nowhere (the app runs unguarded rather than in a shared folder).
  Windows takes no socket path in Node and a pipe's name is one for the whole
  machine, so the guard there is `\\.\pipe\<identifier>.instance.<the user's
  SID>`, listened on through Bun's `node:net`: libuv makes the first instance
  with FILE_FLAG_FIRST_PIPE_INSTANCE, so a listen that succeeds is a pipe this
  process made, and the app then gives it a protected DACL of one entry, every
  right to the user's SID (`SetNamedSecurityInfoW` on its name as a file
  object, which opens it as a client would: its server sees a connection that
  ends at once). The kernel object type, tried first, takes by name only a
  semaphore, an event, a mutex, a waitable timer or a file mapping: every call
  answered 161 (ERROR_BAD_PATHNAME) on both runners, so no pipe was made its
  user's and a second instance, taking ours for someone else's, ran unguarded.
  `pages` reads that DACL back on Windows: `O:LAD:PAI(A;;FA;;;LA)` on the
  runners, whose user is the built-in Administrator (RID 500, which SDDL writes
  `LA`), `AI` the flag SetNamedSecurityInfo adds. There a first claim was
  "alone" and a second "another", the first asked once to show itself. Another user
  can still make a pipe of that name first; a pipe whose owner is not this user
  (its SID, its token's default owner, or Administrators, which only an
  administrator makes) is never asked or obeyed, and one this user cannot open
  is the same: the app runs unguarded rather than quit for someone else
  (`instance.ts`, its unit tests with stand-ins for the security calls).
  Rejected: the spike's hash of the user's temporary folder (anyone can
  compute it); an AF_UNIX socket in `%LOCALAPPDATA%`, which Bun takes and Node,
  running the unit tests, does not; and making the pipe through FFI with its
  DACL at creation, which needs a blocking accept loop or a Win32 last error
  read by a second FFI call, which the runtime may have changed in between.
  The Windows calls (`win32.ts`) are each one whose answer is its return value
  for that reason.
- **No shell.** Electrobun 2.0.4's Linux `openExternal` falls back to
  `system("xdg-open \"<url>\"")` and `showItemInFolder` always runs
  `system("xdg-open \"<file URI>\"")` (linux/nativeWrapper.cpp:10014 and
  10059); the spike's link made its marker through the first. The app calls
  neither on Linux: `opener.ts` starts `xdg-open` (`gio open` where there is
  none) with the address as one argument, and makes the address inert besides
  -- a link as the URL parser reads it with `$`, `` ` ``, `\`, `"` and `'`
  percent-encoded, a folder as a file: URI with every byte but letters,
  digits, `-._~` and `/` encoded -- since xdg-open is itself a script.
  `checks/opener.mjs` is the spike's probe inverted: the control runs the same
  link through `sh -c` as Electrobun would and requires the marker; then the app
  hands a link and a folder named `x$(cd$IFS~;touch$IFS<marker>)` to a stand-in
  xdg-open, which records one argument each, the encoding the check works out
  on its own, and to the real one with nothing it can start; no marker either
  time. On both Linux runners the stand-in was handed one argument each, as the
  check works them out, and the real xdg-open, with nothing it could start,
  exited 4; no marker. macOS (NSWorkspace) and Windows (ShellExecute, Explorer's
  `/select,"<path>"`, where a path holds no `"`) take no shell, and are handed
  the same normalised link.
- **The credential store.** Bun 1.4.0 writes every Windows item with
  CRED_PERSIST_ENTERPRISE and has no option for another
  (`src/jsc/bindings/SecretsWindows.cpp`): it roams with a domain profile to
  every machine the user signs in to. The app writes the item itself
  (`CredWriteW`, the same `<service>/<name>` target and UTF-8 bytes, kept with
  CRED_PERSIST_LOCAL_MACHINE) and reads and deletes it through Bun;
  `remote` asks `cmdkey` that the saved token is kept on this machine.
  On both Windows runners `cmdkey` lists the app's item as "Local machine
  persistence" and one Bun wrote with none, and Bun reads the app's back.
  On Linux libsecret answered neither "no store" nor "no keyring" in
  time (25 s a call where the service never answered; `get` null with no
  default collection). The app asks first, itself, over the session bus's
  socket (`secretservice.ts`: SASL EXTERNAL, `Hello`, then
  `org.freedesktop.Secret.Service.ReadAlias("default")`), bounded at 3 s: a
  collection is a store; "/" is no keyring to keep anything in; an error or no
  answer is no store. Each is refused in words, before Bun is called, and
  every caller takes the error as one: an open of a saved server says the
  token could not be read and sends nothing, and the main window shows "token
  store not answering", never "no token". The call opens no prompt (a locked
  keyring is unlocked by libsecret, with the user, when an item is read,
  unbounded), and needs no program outside the app. On the Linux runners
  `ReadAlias` named the login collection in 5 to 14 ms where the keyring was
  unlocked and answered "/" in 26 and 27 ms where gnome-keyring ran with none,
  and a bus that was not there was said in 6 to 8 ms; in `keyring` the app
  refused a token in 3.14 to 3.19 s on a bus whose Secret Service never
  answered, and in 163 ms, saying there is no default keyring, on one whose
  gnome-keyring has none. That case is a gnome-keyring of its own (its own
  runtime and data folders): one started beside the run's found the run's
  daemon by its control socket, registered nothing on the check's bus, and the
  case was the first again. Rejected: racing
  Bun.secrets against a timer (a `set` that lost the race may land after the
  app said it kept nothing, and an unlock prompt would be cut off); `gdbus`,
  `busctl` or `dbus-send` (none is on every desktop); and a file of the app's
  own where there is no store, never.
- **WebView2's stores.** Electrobun's Windows `sessionClearStorageData` clears
  the first WebView2 view it finds, whatever partition it is asked for
  (win/nativeWrapper.cpp:14580, upstream #608): another window's store, while
  one is open. It is never called there. Every partition is a folder,
  `<LOCALAPPDATA>\<identifier>\<channel>\WebView2\Partitions\<name>` for
  `persist:<name>` and `Ephemeral\<view id>` for any other, the view id a
  counter that starts again each run (shared/windows_profile_paths.h), so a
  file window of the next run with an earlier one's id would open on its store:
  the store the partitions are for. A file's window there is given
  `persist:file-<uuid>`, a folder no later window is given, and removed once
  the window has closed; a removed connection's folder likewise; and the one
  instance sweeps at start what a run that ended left (`file-*`, `Ephemeral`).
  A folder is never removed while a window is in its partition (`stores.ts`
  counts them from before a view is made until its window closed, and asks
  before each try, each try one synchronous call on the main process's one
  thread), only under `Partitions\`, by Electrobun's own naming (a name not
  plain is hashed as Electrobun hashes it, so none climbs out); WebView2's
  processes hold its files a moment after the window closes, so a removal
  tries again for up to 20 s. A removal is done when the folder is not there,
  asked after each try, not when the call returned: on the arm64 runner the app
  once said a removed connection's folder was gone and the check found it
  there. `storage` now runs on Windows and holds each folder gone, a closed
  file window's too, on both runners.
- **New windows.** Electrobun 2.0.4 handles no WebView2 NewWindowRequested (it
  raises `new-window-open` only for a Ctrl+click it sees starting a
  navigation), so WebView2 opens a window of its own for a page's
  `window.open`, as it does wherever its host does not answer: a window held
  to no origin, in the page's store, and on both runners `sandbox`'s requests
  left WebView2's processes, three renderers each, running when the check's
  40 s for the app to be gone ran out. On Windows every window of the app is given a preload
  (`newWindowPreload`, opener.ts) that takes `window.open`, and a link that
  would open a window (a target other than the page's own, a modifier key, the
  middle button), before WebView2 does, opens nothing, and sends the address
  over the event bridge as `link-out`: a studio window's goes to `linkOut`, as
  macOS's `new-window-open` does, and the app's own pages drop it. Neither the
  studio nor the app's pages opens a window of its own. `sandbox` holds on
  Windows, as on macOS, that the page's request reaches the app and the filter
  is what refuses it, both links within the gap reached it, and no WebView2
  process of the app's has a window; `pages` the last for the launcher's
  `window.open`; `test/opener.test.ts` runs the preload as a page would. What
  this guessed it could not hold, a frame's own `window.open`, it did; what it
  did not, and the switches Electrobun starts WebView2 with, are below
  (WebView2's two holes).
- **A preload that comes late.** Electrobun adds a view's preload
  (AddScriptToExecuteOnDocumentCreated, which completes later) and navigates
  in the same call without waiting for it (win/nativeWrapper.cpp:7828 and
  8077), so a first document can load before its script is there: on the x64
  runner a saved server's window, opened again at once, loaded with no event
  bridge (no session, no guard for new windows) and never reported. Each
  window is asked once it navigated, through ExecuteScript, which needs no
  preload, whether Electrobun's bridge is there, and loaded again if not, at
  most three times a view (`guardPreload`, webview2.ts; `latepreload` holds
  it, below).
- **The checks that could not ask.** `orphan` reads Linux's `/proc` (a pipe's
  two ends are one `pipe:[inode]`, told apart by the descriptor's access mode;
  a socketpair's are paired by `ss`), with the same control as macOS's; on
  Windows, which has no table a check reads without a handle walker of its
  own, it kills the app and needs every server to end and every file free:
  that no server holds another's pipe is what it no longer proves there.
  `sandbox` cannot see a page's `window.open` on WebKitGTK, which raises no
  `new-window-open` for a script's (a popup no gesture asked for); there it
  holds that nothing was opened and the window stayed, and asks the app's
  filter directly (`open-link`), which `test/opener.test.ts` holds as well:
  what it no longer proves on Linux is that a page's request reaches the filter
  at all. `pages` failed its eval step on Windows because WebView2 runs
  the check's script (ExecuteScript) as DevTools' Runtime.evaluate does, whose
  allowUnsafeEvalBlockedByCSP is on: the check's own `new Function` ran. A
  string given to `setTimeout` is compiled by the page in a task of its own
  after that script returned, and was refused on all three: the page's CSP
  holds. `keyPlacement` read a key's mode, which is 0666 for every Windows
  file; it reads the key's DACL there, by `icacls <key> /save` (SDDL, SIDs
  and no language's group names), open where an entry lets Everyone,
  Authenticated Users or Users read it, as the server reads a TLS key's
  (`owner_only::open_to`), and its folder test compared `/` paths, which
  Windows' are not.

WebView2's two holes (before phase 4 ships Windows). Measured on both Windows
runners, WebView2 156.0.4314.58 on each, by a spike since taken out (runs
43104202430, 43107758866 and 43109301246): a saved server's page with no CSP, a
stand-in's, asked for another origin's answers and for windows in every way it
has.

- **The same-origin policy is off, and the app cannot turn it on.**
  Electrobun 2.0.4 hands every WebView2 environment
  `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection
  --allow-insecure-localhost --disable-web-security`
  (win/nativeWrapper.cpp:8262 on main 89b102c8d0; 2.0.4's libNativeWrapper.dll
  holds the same string), with no option to leave one out, where its CEF path
  has one. On both runners the page read another origin's answer that allowed
  no reader, by fetch and by XHR, and the document of a frame of another
  origin. `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, the route tried, is added to
  the host's arguments, not put in their place: Microsoft's reference for
  CreateCoreWebView2EnvironmentWithOptions says "appended", and an app started
  without an administrator's rights (a token SAFER made a normal user's, set
  to medium integrity), the variable set in its own environment, had WebView2's
  browser process
  started with its argument beside all three of Electrobun's, which WebView2
  sorts and keeps. Where the app is elevated the variable is ignored outright
  ("Develop secure WebView2 apps"; the runners' user is the built-in
  Administrator, TokenIsElevated 1, and the app started as the checks start it
  had none of it). The loader inside Electrobun's library names only the
  browser folder, channel, user-data and Edge View variables (the `WEBVIEW2_`
  strings 2.0.4's libNativeWrapper.dll holds); the arguments are the runtime's
  to merge, and no Chromium switch puts the policy back. So neither
  `--disable-web-security` nor `--allow-insecure-localhost` can be taken away,
  though nothing of the app's needs the second: the servers it starts are
  plain http on 127.0.0.1, and a saved https server's certificate is checked
  by Bun's fetch in the probe before any window opens on it. The ways left, none
  taken here: Electrobun's own (an option for WebView2's arguments, as CEF's
  `chromiumFlags`, or no such switch; then a version bump); the CEF renderer on
  Windows, whose switches can be left out (2.0.4's CEF archive is 169 MB for
  x64 and 166 MB for arm64, against 10.7 and 11.8 MB for its core); another
  shell (Tauri, re-decided above); a copy of Electrobun's library with the
  string patched out, which the pin (`hutch-pin.json`) holds to its release.
  What is open meanwhile is a saved server's own page: the studio's CSP
  (`connect-src 'self'`, `default-src 'self'` for frames) refuses those reads
  to the studio and to anything injected into it, before a request leaves;
  `sandbox` holds that on every system, Windows included: a studio window's
  fetch and XHR of another origin, one that allows any reader too, are
  refused and that origin is asked nothing, where the studio's own origin
  answers it (the control). `hostile` holds a page with no CSP to its origin's
  reads on macOS and Linux and, on Windows, that WebView2 is still started
  with the switch: an Electrobun that drops it fails the check until the reads
  are held there.
- **New windows.** Phase 2's preload runs in every document of the window,
  every frame's: the initial about:blank of a frame a moment old (reached by
  index too), a frame of about:blank, of srcdoc (its own script too), of a
  data: and a blob: address, of the page's origin and another's, one written
  by document.write and a frame in a frame each had the bridge and the
  preload's `window.open`, and none opened a window. What opened WebView2's
  own window on both runners, elevated or not, was every way a page names a
  window without `window.open`: `document.open` with three arguments, in the
  page and in a frame; `form.submit()` to a target, which fires no submit
  event; a script's click on a link in a shadow root, whose event, not
  composed, never reaches the window; and a link under `<base target>`. Run
  elevated, the five windows' processes outlived the app's quit. The preload
  now takes, in each document, `document.open` with three arguments as
  `window.open`; a link (`a`, `area`, SVG's `a`) whose target -- its own, else
  the document's `<base target>` -- is another window, or followed with a
  modifier key or the middle button, by the window's click listener,
  `click()` (a link outside the document is clicked where no listener hears
  it) and `dispatchEvent` (at the link or at any node in it); and a form so
  targeted, its submitter's `formtarget` first, by `submit()`,
  `requestSubmit()`, a click on its button and Enter in its field (a form in a
  shadow root raises a submit event the window never hears). Each replaced
  function is neither writable nor configurable, and no realm keeps WebView2's,
  every document having its own. `hostile` asks 27 kinds on every system and
  needs no address of the stand-in's loaded, and on Windows no window of
  WebView2's processes; `test/opener.test.ts` runs the preload as a page
  would. Not taken: `--block-new-web-contents=true`, which Microsoft's list of
  WebView2 flags says makes every popup fail: it reached the de-elevated app's
  browser process and the five windows opened as before; WebView2's
  NewWindowRequested and its frames' (FrameCreated), which Electrobun 2.0.4
  registers neither of and nothing outside its library reaches; a listener in
  each shadow root through `attachShadow`, which a declarative shadow root
  never calls. Not covered: what a person must click or allow first where no
  document's script is, a link in WebView2's PDF viewer and a notification's
  `clients.openWindow`, which Electrobun asks the user to allow.
- **WebView2's variables.** WebView2 reads `WEBVIEW2_*` from the app's
  process where the app is not elevated, so the app takes every one it was
  started with out of its environment before its first window (`webview2.ts`):
  `WEBVIEW2_USER_DATA_FOLDER` would put every window in one store, the one
  thing the partitions are for, and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
  can add `--remote-debugging-port`, handing every page and a studio window's
  token to any program of the machine. They go through SetEnvironmentVariableW,
  each judged gone by GetEnvironmentVariableW after, not through `process.env`
  alone. Started de-elevated with `WEBVIEW2_USER_DATA_FOLDER` and
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, the app that only deleted them from
  its `process.env` had WebView2 make that folder on both runners (43107758866);
  the app that takes them out with SetEnvironmentVariableW had its browser
  processes in its own partitions' folders, without the variable's argument,
  and the folder never made, on both (43109301246). Why the delete did not
  reach WebView2 is not known. Electrobun runs the app's code in a Worker (its
  main.js, `new Worker(entry, {})`), but a Worker the bundled Bun made outside
  the app, deleting a name from its `process.env`, took it out of the block
  GetEnvironmentVariableW reads on both runners, as the main thread's delete
  did (43109301246); only the main thread's `process.env` kept it, a copy of
  its own. `hostile` starts the app with three of them and needs none left in
  the block; that no window got the named store it can only see where WebView2
  is not elevated, which on the runners it is: there it holds the block alone.
- **A late preload, held.** WebView2's race cannot be had on demand, so the
  test variant makes what it leaves: `FENNEC_DESKTOP_BRIDGELESS_FIRST=1` takes
  the bridge from the first document of each studio window. `latepreload`
  needs that window loaded again, with its bridge and its session, and the
  control, the guard turned off (`FENNEC_DESKTOP_NO_PRELOAD_GUARD=1`), the same
  window silent; the load again came 47 to 82 ms after the first on x64 and
  59 to 113 ms on arm64 (one each in 43104202430, 43107758866 and
  43109301246).

Desktop integration (phase 3 of #578). A `.fennec` opens from Explorer and a
file manager, the app registers itself for that, the menus' keys, the last
window and the app's words are each system's, and the app has its icon there.

- **What reaches the app.** Electrobun 2.0.4 registers a document type on
  macOS alone, and its launcher, the one program a registration can name,
  starts the main process as `[bun, main.js]`, its own arguments dropped
  (launcher/main.zig; upstream #483, #554). Measured on the four runners, the
  test variant started by GIO's `gio open` on a `.fennec` (Linux) and by the
  shell's ShellExecuteEx on one (Windows), reporting what it was given:
  - its own arguments: Bun's and the app's script alone, the file in neither
    (`[<app>/bin/bun, <app>/Resources/app/bun/index.js]`);
  - its environment: the caller's (gio's, or the program that asked the
    shell), with the launcher's own additions (`ELECTROBUN_LAUNCHER_PID`,
    `ELECTROBUN_INSTALL_ROOT_NAME`, `ICU_DATA`, and on Linux
    `LD_LIBRARY_PATH`) and GIO's (`GIO_LAUNCHED_DESKTOP_FILE`, `_PID`): no
    file;
  - its parent: the launcher, by `process.ppid`, the pid the launcher names
    in `ELECTROBUN_LAUNCHER_PID`, which waits on it for the app's life
    (`WaitForSingleObject` on Windows, a spawn and a wait elsewhere), and
    whose own command line holds the file whole -- on Linux
    `[<app>/bin/launcher, <file>]`, spaces and non-ASCII letters as given, its
    folder the caller's.

  So the app reads the file back from its launcher (`handover.ts`): on Linux
  `/proc/<parent>/cmdline`, the arguments as the system handed them,
  NUL-separated, and `/proc/<parent>/cwd` for a relative one; on Windows the
  command line through NtQueryInformationProcess (ProcessCommandLineInformation,
  with PROCESS_QUERY_LIMITED_INFORMATION, as Task Manager reads one), split by
  the C runtime's rules since 2008, the ones the launcher's own Zig runtime
  splits by. Only a parent whose program is the `launcher` beside the app's
  Bun is read (`/proc/<parent>/exe`, QueryFullProcessImageNameW): a terminal
  that ran Bun, a harness or a reparenting init hands nothing. Every argument
  after the program is a path but the launcher's own flags (Linux's
  `--automation`; with `--uninstall` it never starts the app), opened as
  Open… opens one, so a file that is not a database is said in words and a
  server is never started on it; nothing of it is run or read as a command.
  With the app running, the double-click starts a second instance, which
  hands its paths to the first over `instance.ts`'s channel, its user's alone,
  as one line of JSON (`show` otherwise, as before), and quits before it reads
  a list; a path is at most 32 767 characters on Windows, so an ask is bounded
  at a mebibyte and 256 files, and anything else that connects asks nothing
  (Windows' owner check opens the pipe and says nothing).
  Rejected, the plan's candidate first: an opener program of our own,
  registered as the handler, handing the path over and starting the app where
  none runs. On Windows it must be a GUI-subsystem executable, since the app's
  Bun is a console program whose window would flash at every double-click, so
  it is a binary to build for each system and processor, and a process more
  between the click and the app, for what the launcher already holds. Also
  rejected: an environment variable (nothing puts the file in one, and a
  variable the shipped app reads for itself is a hook, `release.mjs`); a
  shell, `cmd /c`, `sh -c`, PowerShell or a script host as the handler (a
  shell, and a console); waiting for upstream (#483 and #554, open). A way
  was found, so Tauri is not taken up again.
- **Who registers it: the app, at every start.** Electrobun's installer and
  Setup.exe run nothing of the app's, the release's zip and tarball (phase 4)
  are unpacked anywhere with no installer at all, and the install scripts
  (`install.ps1 -App`, `install.sh --app`) cover their own copies alone and
  would point nowhere once one was moved. So the one instance writes it at each
  start (`associate.ts`), only what differs from what is there: a moved copy
  points it at itself the next time it runs, and a start that changes nothing
  writes nothing (`finder` holds a second start to no write). On Windows,
  under `HKCU\Software\Classes`: a ProgID `<identifier>.fennec`, its name, a
  DefaultIcon (Hutch's `Resources\app.ico`) and `shell\open\command` =
  `"<launcher>" "%1"`, a REG_SZ, so nothing in it is expanded but the shell's
  own `%1`, and a launcher path holding a `%` (which the shell would
  substitute) or a quote is not registered; listed in
  `.fennec\OpenWithProgids`, and made `.fennec`'s default only where, as
  Explorer reads it (HKCR: the user's over the machine's), there is none, it
  is the app's already, or it names a ProgID whose quoted program is gone. A
  program the user picked is Explorer's own record (UserChoice, hash-sealed),
  which wins and is never written; another program's default is left, and a
  command this cannot read (unquoted, another type) is taken for another
  program's. Then SHChangeNotify(SHCNE_ASSOCCHANGED), so Explorer shows the
  icon. The calls are win32.ts's, each answered by its return value. On Linux,
  under `$XDG_DATA_HOME` (`~/.local/share`): the shared-mime-info type
  `application/x-fennecdb`, `*.fennec`, in `mime/packages/<identifier>.xml`,
  and a desktop entry `applications/<identifier>.desktop` with
  `MimeType=application/x-fennecdb;`, `Exec="<launcher>" %f` (quoted and
  escaped as the specification reads it: the string's escapes, then its
  quoting; a launcher path holding a `%` is not registered, since GIO looks
  for the program before it expands the `%%` the specification asks for, and
  ignored such an entry on the runners), `NoDisplay=true` (Electrobun's own entry,
  `fennecstudio.desktop`, with no `MimeType` and no `%f`, is the one in the
  menus) and `TryExec=<launcher>`; each written by a rename, and then
  `update-mime-database` and `update-desktop-database`, each started with an
  argument vector where it is installed (a missing one is said, not failed).
  Nothing is made the type's default: GIO took the only program for the type
  as its default with no `mimeapps.list` line, and a program the user picked
  there stays theirs. GIO reads a `.fennec` as the type once the database is
  made again (`gio info`); xdg-open with no desktop session asks `file(1)`,
  which knows no `.fennec`, so the check drives `gio open`, which GNOME's
  file manager is. KDE's (KService's cache over the same entries) is not
  checked.
- **What is left.** The download is the app unpacked, with no uninstaller
  (phase 4); removing its folder, and the shortcut or menu entry the install
  scripts made, leaves the registration, and so would Electrobun's
  uninstaller (Windows' Apps settings, the launcher's `--uninstall`), which
  removes its own records alone. On Windows the ProgID's command then names a
  launcher that is gone, and a double-click says Windows cannot find it, until
  the three `reg delete` lines in the README's Uninstall and the studio page's
  Windows tab; on Linux `TryExec` has GIO ignore the entry once its launcher is
  gone (`finder` removes the copy an entry names and the double-click starts
  nothing), so nothing points nowhere, and the two files stay, for the two
  `rm`s there. The owner's decision: those lines are the uninstall for now,
  written out in both places with every key and file, and an uninstall entry
  of the app's own is a later phase's. The per-start registration was asked
  whether it could drop one that names a path no longer there, cheaply: it has
  none to drop. It writes one ProgID and one entry for its identifier, pointed
  at the copy that starts, so a copy moved or removed is written over by the
  next start of any other; once the last copy is gone, nothing of the app runs
  to remove it. (`.fennec`'s default naming a ProgID whose program is gone is
  already taken over, above.)
- **The checks.** `finder` drives the double-click as close as a runner
  allows: on Windows .NET's Process.Start with UseShellExecute, which is the
  shell's ShellExecuteEx, as Explorer's double-click is; on Linux `gio open`.
  Its control takes the registration away as the README tells a user to, and
  the same double-click starts nothing of the app: on Linux gio says no
  program is registered for the type (`application/octet-stream` there); on
  Windows the shell hands a file of no known type to the Unknown class's
  openas verb, `OpenWith.exe` (`cmd /c start` would do the same through
  cmd), which is the event waited for, then closed, with the picker it
  started (PickerHost.exe on Windows 11), as a person closes them
  (CloseMainWindow, WM_CLOSE; a kill only where one has no window or does
  not end), and no dimming window of the shell's (Shell_SystemDim) it brought
  may be left. Such a picker dims the whole screen and takes every click: on
  the windows-11-arm runner the click `menu` made into its main window's page
  landed on the dim of a PickerHost.exe that was there from before `finder`
  ran. Two were started, by their start times when `test/process.test.ts`
  and then `serve` started fennec-server on 0.0.0.0: Windows 11's firewall
  asking whether a program listening on the network may be reached (Windows
  Server, the x64 runner, does not ask). `desktop.yml` tells the Windows
  runners' firewall not to ask (NotifyOnListen off), the tests and checks
  reaching every server over loopback, which it does not filter; `run.mjs`
  says what dims the screen when the run starts and after each check
  (`dimWindows`: each dimming window's program and when it started), asks it
  to close as a person would (`clearDimming`; never a kill, as a picker host
  outlives its window), and fails a check that left one. Then the app's first start
  registers it, the double-click starts the app with the file, which opens
  with no main window over it; a second file's is handed to the instance
  there; a `.fennec` that is not a database is said; on Linux, the app run
  from a folder named `odd "q" 'a' \`cd&&touch <marker>\`` registers itself
  there and a double-click starts that very launcher with the file, the name
  whole and no marker, and once the copy is removed the entry is ignored; the
  registration removed again, nothing starts. The app does not start at all
  from a folder whose name holds a backslash (Bun 1.4.0 reads one in its
  script's path as a separator: "ENOENT reading .../b/app/Resources/main.js")
  or `$(...)` (Electrobun's core could not open its native library,
  FileNotFound: its launcher puts the folder in `LD_LIBRARY_PATH`, where the
  loader reads `$` and splits at `:` and `;`), so the name holds neither. `run.mjs` removes the test variant's
  registration at the start and the end of every run.
- **Menus.** The File menu's accelerators were a bare letter, which
  Electrobun makes Command on macOS and Ctrl on Windows (its
  setMenuItemAccelerator takes a lone letter as Ctrl and the letter), and
  `shift+o`, which was Shift and O with neither on any system; they are
  `CmdOrCtrl+N`, `+O` and `+Shift+O`, Command and Ctrl. Windows gives each
  window a menu bar, and translates accelerators in the main process's
  message loop (TranslateAccelerator), which sees the keys of its own
  windows; WebView2 in a window is another process's window. Measured on the
  Windows x64 runner: with the main window in front and with a click into its
  page (text typed after it arrived in the search box), Ctrl+N, Ctrl+O and
  Ctrl+Shift+O reached no menu item, and nothing else either (no WebView2
  window of its own): WebView2 has the keys, and Electrobun 2.0.4 handles none
  of its AcceleratorKeyPressed. (On the arm64 runner the system took the keys
  a check typed, GetLastInputInfo moving, and the page got none of them: it
  had not the focus, `document.hasFocus()` false, which was on the bare window,
  GetGUIThreadInfo's focus the top-level BasicWindowClass; after Electrobun's
  `activate()` it was in WebView2's Chrome_WidgetWin_1 and the same text
  arrived, three keys seen. Fixed by the app, below: keys in a window's page.)
  So the main window takes the File menu's keys
  in its own page on Windows and Linux (src/launcher/index.ts, not on macOS,
  whose menu bar takes Command's first); a key the menu did take would never
  reach the page, so none runs twice. A studio window's page is a server's,
  with the event bridge alone: its menu bar works with the mouse, its keys do
  nothing. Not taken: a preload that sends them over the event bridge, which
  any page can send on as well, so a server's page could open the Open dialog
  at will.
  Linux has no application menu ("Application menus are not supported on
  Linux", linux/nativeWrapper.cpp), so the main window's own buttons are its
  File menu: New database, Open file…, the Databases list's Open (Open Recent),
  Remote connections (Connect to a Server…); with no Dock, the main window
  closed is shown again by starting the app a second time, which asks the one
  there. `menu` presses each of them, and the keys: on Windows as a person's
  (testkeys.ts: keybd_event, and a click as a mouse's, absolute, which also
  brings the window to the front), its control twice -- text typed into the
  main window as it opened, with no click into its page, arriving in its
  search box, then a click into its page reaching the page and text typed
  after it arriving --
  each key's effect asserted and taken once, by the menu bar or the page,
  never both; on Linux, which has no input a check can press as a person's
  under xvfb, dispatched in the page. Where the main window opened behind
  another program's, the app leaves it there, and the check brings it forward
  as a person would, by a click on its title bar (where WindowFromPoint names
  another window over every point of it, the window and whether DWM cloaks it
  are said, and the click made all the same: the window's coming forward is
  what is asked): the app's look once it is in front (its activation's, or
  the watch's for a window that was behind) is then the one that moves the
  focus, the other half of the rule. (The windows-11-arm image comes up
  with the Start menu open, its search in front, and a "Microsoft account"
  sign-in window, WWAHost.exe, each started before the job and in front of
  every window: the Start menu lay over the middle of the main window's title
  bar, whose right end ran off the screen, and the sign-in window was not
  among the visible ones when a step before the checks looked. `desktop.yml`
  asks each WWAHost window to close then, ends a WWAHost still running 15 s
  on, and closes the Start menu, in front, by Escape; the check looks along
  the whole visible title bar for a point that is the window's; `run.mjs`
  says when another program's window is in front.) Nothing
  else of the check's own brings a window forward or moves its focus: no Alt (the first version pressed and let
  go of one before a SetForegroundWindow, which can put the menu bar of a
  window in front by then in menu mode, where keys go), no activation, and no keys
  dispatched in the page on Windows, where the first version did so when the
  system seemed to take none, a reading that ran ahead of the system's input
  thread and hid the arm64 runner's failure. Keys or a click the page did not
  get fail it, with what the page saw, the click's point and the window
  under it, the focus and the menu mode of the window's thread, the windows
  within it and the app's looks at its focus.
- **Keys in a window's page (Windows).** Activating a window, Windows'
  DefWindowProc puts the keyboard focus on the window itself (WM_ACTIVATE),
  and a WebView2 page is a child window of WebView2's process, which has the
  focus only when the host moves it there (the controller's MoveFocus).
  Electrobun 2.0.4 moves it only in `activate()` (activateWindow's
  FocusActiveView, win/nativeWrapper.cpp:11496): not on WM_ACTIVATE (:6074),
  which it passes to DefWindowProc, and not once a view is made, which comes
  after its window was shown and activated. So a window shown in front had the
  focus on the bare window, and what a user typed before clicking into the
  page reached nothing (measured on the arm64 runner, above). The app moves it
  itself (`keysToPage`, webview2.ts; `focusesPage`, windows.ts, given every
  window the app makes): once a window's page has loaded (`did-navigate`) and
  at each of its activations (its `focus` event), where GetGUIThreadInfo on the
  window's own thread says the focus is on a window of the app's process or
  none (no page: a page's window is WebView2's process's) and no menu has its
  input, it calls `activate()`. That call also brings a window forward, by
  AttachThreadInput to the thread in front where SetForegroundWindow is
  refused, so it is made only for a window that is the one in front
  (GetForegroundWindow) and never brings one forward: a window behind another
  program's stays behind, and its page is given the focus when the user
  brings it forward. That needs a look of its own: a window Electrobun made
  the active one of its thread while another program's was in front (its
  creation's SetActiveWindow, where the foreground was refused) gets no
  WM_ACTIVATE when a click brings it forward. Measured on the arm64 runner,
  the Start menu open as the main window opened: clicked by its title bar,
  it came to the front with no `focus` event and its focus on the bare
  window. So a window a look finds behind is looked at every 100 ms
  (`BEHIND_LOOK_MS`) until it is in front, then given the focus (`forward`),
  or until it closes; one in front is never watched, so a window that opens
  in front costs no timer. What it does not change: Electrobun makes every window
  activated (`activate` defaults to true), by the same AttachThreadInput, so a
  window that opens while the user is in another program, a file handed over
  late, comes forward as it did before; and between the look and the call, a
  few milliseconds, the user may switch away, which the call would undo. On
  macOS and Linux the page is the window's own view, which AppKit makes its
  first responder and GTK gives the window's focus, and `activate()` would
  bring the app forward over another: nothing is done there (`menu` cannot
  press keys as a person's on either). `test/webview2.test.ts` holds the
  rule, `menu` the running app on both Windows runners.
- **The last window.** macOS keeps an app with no window in the Dock, and the
  app stays there as a document app does, a served database still running.
  Windows and Linux have no Dock: an app whose last window closed is gone from
  the taskbar and the switcher, and the convention is that it quits. So there
  the last window closed is a quit (`onLastWindowClosed`, index.ts): the same
  `Utils.quit()` File, Quit makes, whose `before-quit` holds it until every
  server has stopped by its one shutdown (the end of its stdin on Windows,
  SIGTERM on Linux, each writing its checkpoint, a hard kill only 30 s on), a
  served database's too. It is the owner's choice from the platforms'
  convention; a served database needs a window left open there. Not
  Electrobun's `exitOnLastWindowClosed`, which quits as the close handlers
  return, before any server has stopped (above). Every window the app makes
  is counted (`counted`, windows.ts: the main window, the new-database
  window, a notice, a studio window), and the count is read on the next turn
  after a close, with nothing opening (`anythingOpen`): a notice's Try again
  closes the notice, the last window then, and opens its file, which makes a
  window of its own, so that quits nothing. `lastwindow` holds both systems'
  rules, the notice's Try again with the app left up, and on Windows and
  Linux the quit: `last-window-closed`, the window's and the served
  database's servers each stopped with exit 0, nothing of the app left. The
  checks that close a file's only window and open it again (`files`,
  `environment`) show the main window too, and `menu` closes Linux's main
  window with a file's open.
- **The app's words.** The main window named the computer "This Mac" (its
  side bar, the Databases section, the serve sheet's reach and metrics, a
  served database's card) and its button "Reveal in Finder". Each system's own
  word is used: "This PC", as Windows' Explorer names it, and "This computer"
  on Linux, whose desktops each name it their own way; "Show in Explorer" and
  "Open its folder" (`xdg-open` opens the folder there). They come with the
  main window's first list (`Overview.machine`, `.reveal`; `thisMachine`,
  `revealLabel`), the HTML's own being macOS's; the credential store's name
  already came so. `pages` holds the words on each system and, on Windows and
  Linux, no Mac, Finder or Keychain left in any section of the main window;
  the notices name none.
- **Icons.** `icon/icon.ico` (16, 24, 32, 48, 64, 128 and 256 pixels, each a
  PNG entry, 48.9 KB) and `icon/icon.png` (512 pixels, 69.2 KB), made from
  `icon/icon.svg` by `scripts/make-icon.sh rasters` on a Mac (sips draws the
  SVG, as for the iconset) and kept in git with `icon/rasters.json`, their and
  the SVG's digests, since neither Windows nor Linux has an SVG renderer of its
  own to make them where they are built; `test/icon.test.ts` fails once the
  SVG changes until they are made again. `build.win.icon` and
  `build.linux.icon`: Hutch embeds the .ico in `launcher.exe` and the
  Setup.exe and keeps it as `Resources\app.ico`, and keeps the PNG as
  `Resources/appIcon.png` and `Resources/app/icon.png`; Electrobun's Start-menu
  shortcut and menu entry show them, and the registration names those copies
  rather than adding its own. What they cost, the release build of this
  branch beside main's on the same x64 runner, twice (the two differ by
  timestamps alone): Windows' `Setup.zip` 39 897 737 -> 40 014 917 bytes
  (+117 180, +0.29%; the second +120 113), its update archive (`.tar.zst`)
  +72 129 and +72 083, `launcher.exe` and the Setup.exe +49 664 each (the
  icon's images), `Resources\app.ico` 48 892; Linux's `Setup.tar.gz`
  41 806 525 -> 41 875 912 (+69 387, +0.17%; the second +69 315), the update
  archive +69 236 and +69 187 (Hutch's two copies of the PNG are one in
  zstd's window). The build also writes the icon's images into the app's own
  `bun.exe`, read from the x64 download: RT_ICON 1 to 7 are the .ico's seven
  images and RT_GROUP_ICON 1 names them, as in `launcher.exe`, but Bun's own
  group, the named `IDI_MYICON`, stays, and still names RT_ICON 1 as its one
  256-pixel image of 270 600 bytes, which is now the app's 16-pixel one; a
  named group comes before a numbered one, so it is the icon Windows draws for
  that file, the 16-pixel image (its hash, `environment`'s, is neither the
  toolchain Bun's nor the launcher's, on both runners). The write leaves the
  security directory naming Bun's signature at 88 815 616, 10 328 bytes, in a
  file of 88 594 432: Windows reads it as NotSigned (the toolchain's is Valid),
  and Bun refuses that copy as the base of a program (InvalidSecurityDirectory:
  `environment`'s stand-in is built by the toolchain's). Whether a window's
  taskbar button or Task Manager shows that icon, Electrobun setting none of
  its own, is not measured. macOS's is the iconset it had. Without them the launcher, its shortcut, its menu entry and
  each window show a default program's icon, and a `.fennec` file none.

Toolchain. `electrobun init` installed Hutch (the build tool) in `~/.hutch` and
appended two lines to `~/.zshrc`, and the npm package's resolver would, were
its download to fail, run that installer. `scripts/electrobun.mjs` runs the
package's own resolver with `HUTCH_HOME` in the project (`.hutch-home/`,
ignored) and an `installHutch` that refuses; the archive is checked against
Electrobun's release index by the resolver, and `electrobun` is an exact
version in a lock file, so nothing is fetched that is not named by a hash or a
version. The npm package's `exports` map every subpath to a stub, so the
resolver is required by path. The index the resolver checks the archive against
is fetched from the same release as the archive, so `hutch-pin.json` holds, in
the repository, the SHA-256 of the archive as downloaded (checked through the
resolver's `download` hook) and of the two binaries in it (checked on every run,
cached or fresh: a CI cache of `.hutch-home/` is not trusted for being one);
`ELECTROBUN_HUTCH_BINARY`, which the resolver takes unchecked, is ignored, and
the resolver is told no file exists (`existsSync: () => false`), which is how it
decides on a Hutch "already at `$HUTCH_HOME/bin`" that it would run unchecked
when the paired download failed (a cache's, with no pin). That is not all that
ships: Hutch then fetches the Electrobun runtime (`libNativeWrapper.dylib`, the
launcher, the main process's API sources, `preload-sandboxed.js`), its Bun and
Cottontail, and puts the first two inside the app. It checks those against a
lock of its own (`.hutch/dependencies.lock`, not in git, no hash for its Bun) and
a tampered file in a restored cache would be in the lock that cache wrote. So
`hutch-pin.json` holds a digest of each directory too (`scripts/tree-digest.mjs`:
the path, executable bit and SHA-256 of every file, hidden ones too, and a
symlink as itself), and `scripts/electrobun.mjs` runs `sync` first (which runs
nothing of the app's), compares, and only then builds; tried with a byte added
to the preload, to the native library and to the Bun, each refused, and a
Hutch binary changed in the cache, which is not run. The digests are
deterministic (a fresh `.hutch-home/` gives them again, and the hosted runner
checks them each run). An Electrobun that Hutch could not fetch without its
install script was also considered: a lock file was not committed, since
Hutch's own is per project and per machine and with no Bun hash proved nothing
here. A Bun upgrade is a pin change by the same command
(`--print-runtime-pin`), which prints every directory Hutch fetched for the
system, pinned or not: a new Electrobun, Bun or system is a directory the pin
does not name yet (the first version printed only the pinned ones).

The pin is per system, since Hutch builds only for the one it runs on and each
system's archive, binaries (`bin/hutch.exe` on Windows) and runtime are their
own: macOS on Apple silicon, and Windows and Linux on x64 and arm64, each built
by `desktop.yml` on its own runner. The Windows and Linux runtimes' digests
were taken from the spike's runs (#578: 42997677468, 42999915984 and
43002802508, each printing them three times, the same every time) and are held
on every CI run as the Mac's are. A digest taken on Windows records no
executable bit (Node reports a file's mode there as 0666 or 0444) and joins its
paths with `\`: it is the same on Windows each time, and is compared only
there. A system with Hutch pinned and no runtime is refused: the first version
read a missing runtime as nothing to check, which would have built a new
system's app with whatever Hutch fetched.

The first launch. A stable Electrobun build is a self-extractor: the first run
unpacks the app in place with an installer window and starts it as a process of
its own, and the first process stays in its event loop, so quitting the app
leaves it alive (seen unsigned, from `open` and from a harness). The checks do
one launch first (`warmup`) and end the helper. Not verified, for want of a
Developer ID certificate: that an in-place unpacking leaves a signed and
notarized bundle's seal whole, and that Hutch gives the bundled `fennec-server`
the hardened runtime (`desktop.yml`'s signing job fails if `codesign` says
otherwise).

Signing. `desktop.yml` builds and checks on every pull request and push to the
paths the app is made from (the studio, `fennec-server`, `fennec-http`,
`fennec-core`, the app), and signs only when a maintainer starts it by hand from
`main`, in the Environment `desktop-signing` that holds the six secrets (the
Developer ID's name, the `.p12` and its password, the App Store Connect key's id,
issuer and `.p8`): a pull request's or another branch's code never meets one, and
a dispatch from another branch is skipped. The Environment is made, with a required
reviewer and `main` as its only deployment branch, **before any secret**: a
secret added to one without rules is readable from any branch that names the
Environment, and the order cannot be enforced by the workflow alone, since the
Environments API asks for administration rights that the job's token does not
have; the job's first step reads the rules back where the token can, and fails
unless they are those, and warns where it cannot (RELEASING.md has the `gh api`
commands). It reads them only where the Environment holds at least one of the
secrets: the first dispatch makes the Environment with no rules, and with
nothing in it to guard, failing would only stop the unsigned DMG, whose job
waits on this one. The signing job has read access only,
restores no cache (what a cache holds was written by runs without the secrets),
checks out without persisting the token, imports the certificate into a
keychain of its own with `-T /usr/bin/codesign -T /usr/bin/security` and not
`-A`, validates the release it is given before it reaches `make` (which
expands `$(shell ...)` in a value on its command line), runs `release.mjs` on the
signed bundle, and greps the bundled server's signature for the hardened runtime
and for the Developer ID separately (one `grep -E` with an alternation passes if
either is there). A second job, with a write permission and no secret, attaches
the DMG the first uploaded. Where the Environment holds no secrets the unsigned
DMG is attached instead (The download, below): a signed one wins, and a signing
job that failed or was not approved attaches nothing, so a maintainer who meant
to sign never finds an unsigned DMG in its place.

The download. The owner decided (2026-11-11) to ship the app without an Apple
Developer account, unsigned, as the Windows zip ships, until there is one.
Electrobun's stable build cannot be that download: it is a self-extractor, a
launcher (linker-signed ad hoc, identifier `extractor`) and the app as a
`.tar.zst` in its Resources, which the first launch unpacks into the bundle
itself, and `codesign --verify --deep --strict` fails on it ("code has no
resources but signature indicates they must be present"). A browser's
download is quarantined, and a quarantined app whose signature does not hold
is refused as "damaged" with no Open Anyway; and a quarantined app opened where
Finder did not move it runs translocated, from a read-only mount (it was seen
running from `/private/var/folders/.../AppTranslocation/...`), where an
extractor cannot write into its bundle. So `scripts/dist.sh` ships the state
the first launch leaves: it untars the same payload, clears the extended
attributes `codesign` refuses, signs each Mach-O file (found by `file`, the
deepest first: the six in `Contents/MacOS`, the bundled `fennec-server` and
`fennec`, and the extractor Electrobun keeps as `Contents/Resources/uninstall`)
and then the bundle, ad hoc (`codesign --force --sign -`), and verifies it.
That untarring is the first launch's result was checked once, on the test
variant: its payload untarred beside it and the bundle `warmup` left had the
same 25 files, the same digest (`tree-digest.mjs`, executable bits included)
and no difference under `diff -r`; only the system's `com.apple.provenance`
attributes differ. The extractor's other work (its installer window, the
quarantine it takes off, the tar it keeps under Application Support for bsdiff
updates, an uninstaller's record) is outside the bundle and for updates the app
does not have. Not used: `--deep` to sign (it signs nested code with the outer
bundle's options, and Apple advises against it), the hardened runtime (Bun's
JIT would need entitlements, and ad hoc code gains nothing from it). The image
is HFS+, the app beside a link to `/Applications`, LZMA-compressed (ULMO,
macOS 10.15 and later; Apple silicon starts at 11): 24.2 MB (24 202 568 bytes)
for an app of 79 MB on disk, against 32.8 MB as ULFO (LZFSE), 36.2 MB as UDZO
(zlib) and 25.9 MB for Electrobun's self-extracting DMG (25 876 994 bytes), the
same build, measured on the M1 above. It is made in steps: an empty read-write
image, attached by the script at a mount point of its own with
`.metadata_never_index` at its root, the app and the link written into it,
`sync`, a detach, then `hdiutil convert` (which mounts nothing). `hdiutil create
-srcfolder`, the first version, does the same with a detach of its own, and on
the hosted runner it failed once in the first two runs, after the copy, with
exit 1 and nothing said (`-quiet` closes stderr too): the classic "Resource
busy" of a scanner, XProtect or Spotlight, holding a file just written to the
volume, here ten Mach-O files where Electrobun's own DMG, which never failed, has
a tarball. A volume with a file held open was seen to refuse a plain detach
("Resource busy", exit 16) and take a forced one, so a detach refused is forced:
everything is written and synced by then, and `hdiutil verify` and
`checks/dist.mjs` hold the image file by file. `hdiutil makehybrid -hfs`, which
writes the volume from a folder with no mount at all, was tried and rejected:
it gives every file Finder information, which `codesign --verify --strict`
refuses ("resource fork, Finder information, or similar detritus"), and
`dist.mjs` failed it.

`checks/dist.mjs` holds the file that ships, on every CI run and first in `make
studio-desktop-test`: the image verifies and is ULMO, its volume the app and the
link alone; the app's signature holds, ad hoc, for `dev.fennecdb.studio`, its
resources sealed and not the linker's, and each Mach-O file verifies alone; no
payload is left and the launcher is not the extractor (it holds no
`Self-extractor` string); `release.mjs` finds no hook in it. It launches the app
from the image's own read-only mount, the stand-in for a translocated run, then
from a copy `ditto` took out (as Finder and `install.sh` take it) once the image
is let go: each time with a `HOME` and a `TMPDIR` of its own, up when the app's
instance socket is made there (the app's own code ran) and a window of its
process is on screen (CoreGraphics' window list through JXA: a window's owner
needs no permission, its title would), and quit with SIGTERM, the app's own
handler, exit 0 and none of its processes left. The copy's tree digest is the
same before and after its run and its signature still holds. The shipped app
has no hooks, so this is all a check can see of it; the test variant's checks
hold the rest. WebKit keeps a store for the identifier in
`~/Library/WebKit/dev.fennecdb.studio` whatever `HOME` says, and macOS at times
its preferences in `~/Library/Preferences/dev.fennecdb.studio.plist` (seen
after one run, not after three others): both take the home from the system. The check removes what
it made, the preferences through `defaults delete` first (cfprefsd would write
a plist removed under it again), leaves what a real install had, and tells
LaunchServices to forget both copies it ran.

The download on Windows and Linux (phase 4 of #578). The app ships unsigned
there as on the Mac, and as the payload unpacked: one folder,
`fennec-studio`, the files Electrobun's installer copies into
`<appdata>/dev.fennecdb.studio/stable/app`, in a zip on Windows and a `.tar.xz`
on Linux, x64 and arm64 (`scripts/dist.sh`, from the build's
`Resources/*.tar.zst`, decompressed by Node's own zstd and unpacked by
Windows' `tar.exe` or GNU tar). It runs where it is unpacked, which is what a
browser's download is; `install.ps1 -App` and `install.sh --app` put it where
Electrobun's installer would, so the two never make two copies (Install
channels, below). Rejected, Electrobun's own download, `win-<arch>-<name>-Setup.zip`
(`Setup.exe` and a hidden `.installer\` payload) and
`linux-<arch>-<name>-Setup.tar.gz` (a 45 MB `installer`): on the runners
`Setup.exe` said "Installation completed successfully" and then, 120 s on, had
neither started the app nor ended (phase 1's `warmup`), so a script could only
start it and leave it running (and `install.ps1`'s check refuses
`Start-Process`); it is an unsigned program a browser's download meets
SmartScreen with before anything is checked; Linux's installer run over a
running copy installed again under it ("File not found: launcher/index.html")
and starts the app itself; and both keep the whole payload again under
`stable/self-extraction/` as a tar for an updater the app does not have
(122 941 440 bytes beside Linux's 118 MB app). What they give and this does
not: Electrobun's uninstaller and its HKCU Uninstall entry, and a Desktop
shortcut; removal here is two paths, which the installers print. Sizes, the
first CI runs (the spike's installer download beside each):

| | download | installer's | unpacked (`du`, MiB) |
|---|---|---|---|
| Windows x64, zip | 49.3 MB | 39.9 MB | 107, 23 files |
| Windows arm64, zip | 45.0 MB | 35.9 MB | 95, 23 files |
| Linux x64, `.tar.xz` | 36.8 MB | 41.8 MB | 117, 24 files |
| Linux arm64, `.tar.xz` | 33.3 MB | 40.3 MB | 115, 24 files |

Linux's is xz because it was the smallest the systems open themselves: the x64
app was 36.8 MB as `xz -9`, 50.6 MB as `gzip -9` (the first version) and
40.2 MB as `zstd -19`, which tar hands to a `zstd` a desktop may not have;
`install.sh --app` asks for `xz` with the rest. Windows' is deflate, about 9 MB
more than Electrobun's zstd, because a zip's deflate is all `Expand-Archive`
and Explorer read. On disk an install is the app alone, about half of
Electrobun's (the app and its kept tar).

`checks/dist-archive.mjs` (which `dist.mjs` hands over to there) holds the file
that ships, on every CI run: the one folder, its launcher, Bun and main
process, the bundled `fennec-server` and `fennec`, executable bits kept on
Linux, no payload left, and a launcher that is not the extractor -- the
build folder's own launcher is, and must be found to hold `Self-extractor`,
`self-extracting` or "Installation completed successfully" (it held all three
on all four runners), so the words looked for are ones there to find;
`release.mjs`, now reading a Windows or Linux payload (zstd and a tar read in
memory) or an unpacked folder, finds no hook in it. It starts the app where it
was unpacked, then installs it through the installer a user runs (`install.ps1
-App` under Windows PowerShell 5.1 from a folder, `install.sh --app` from a
release served on 127.0.0.1) into a home of its own, holds the installed folder
to the archive's file for file and the Start-menu shortcut's target or the menu
entry's `Exec` (and `desktop-file-validate`) to its launcher, and starts it
again by that target. Each start has a home of the check's own (`HOME` on
Linux, the XDG folders unset; `LOCALAPPDATA`, `APPDATA` and the temporary
folder on Windows) and is up when the main process runs and a window of the
app's processes is on screen (`xdotool search --onlyvisible --pid` under
xvfb; a non-zero `MainWindowHandle` on Windows): 0.6 s on Linux, 1.7-3.3 s on
Windows and once 48.8 s (`windows-11-arm`, the installed copy's fresh WebView2
profile), within a bound of 240 s. Linux stops it with SIGTERM to the main process, the app's own
handler, the launcher's exit 0 and nothing left; Windows ends the tree
(`taskkill /T /F`), since a SIGTERM there is TerminateProcess and a quit by
the app's own hand is phase 2's. A run changes no file of the app but one:
Electrobun's launcher writes its log beside itself on Windows, `bin\app.log`,
at every start (x64 and arm64), so the folder must be its user's to write --
an install in the profile is, and a download unpacked into Downloads. What a
run made under the real home for the shipped identifier is removed, where it
made it (nothing was, on any runner).

The release. A dispatch from `main` naming a release builds in the `app` job
itself, with the release's server and shell (`get-server.sh`, the tarball held
to the release's `SHA256SUMS`), and in each of `other-systems`' four jobs with
that release's tarball or zip, so every download attached is one every check
ran on. `attach-unsigned` (needs `app`, `sign` and `other-systems`, and runs
unless the run was cancelled) holds `contents: write` and no secret, checks
out one file, `scripts/attach.sh` (main's, as the workflow is, no token kept),
and runs it and `gh` alone. It attaches what nothing signs: the DMG where
`sign` ran and signed nothing, and Windows' and Linux's four downloads where
all four of their jobs passed (one system's failure holds back no other's, and
a release has all four or none), each named
`fennec-studio-v<version>-<system>-<arch>.<ext>`, and appends their lines to the
release's `SHA256SUMS`, the one place `install.sh` and `install.ps1` learn a
sum from, in one write. That is the one asset of a published release
replaced, and it only grows: the new file must start with every byte of the
old (`head -c`, not `cmp -n`, which macOS's `cmp` fails at the shorter file's
end), have one line more for each download it lists anew, and read back as
written, so no sum anyone took changes. A download of its name already on the
release is never replaced: one whose SHA-256 is this run's is a re-run's and
the job goes on to the line, another fails it -- and every file is held to the
release before anything is written, so a failure uploads nothing.
`test/attach.test.ts` runs the script against a stand-in `gh` that keeps each
release in a folder: the five attached and listed in order, a re-run writing
nothing, a run cut off after its uploads listed the next time, another file of
a download's name or a line with another sum failing before any upload, a
`SHA256SUMS` with no last newline, one that does not read back, and what is not
a release, a download or a file. A dispatch from a branch builds and checks
every download and attaches nothing (run 43069634009: `attach-unsigned`
skipped). A separate sums file was considered and not taken: every installer
and the formula read `SHA256SUMS`, and a second file would be a second place to
trust. GitHub cannot write an asset in place, so `--clobber` deletes and
uploads, and for that moment the release has no `SHA256SUMS` and an install
fails saying it could not download it; RELEASING.md runs the dispatch after
`packages.yml`.
Not verified: the Finder double-click of a quarantined copy and Open Anyway.
A terminal here is a developer tool Gatekeeper exempts, so `open` started a
quarantined copy without asking; it needs a person at a Mac, once a release
carries the DMG. Nor, on Windows, SmartScreen's prompt for a browser's copy of
the zip and Smart App Control's refusal: CI has neither a mark of the web from
a browser nor Smart App Control, so RELEASING.md has a person start a
browser's copy once. And no attach to a real release was run before this
landed: the script's every path is the test's, the job's `if` the dispatch's.

Rejected. webview-bun with `bun build --compile` (an 89.8 MB binary, studio
visible in 0.87 s): `Webview.run()` blocks the JavaScript thread in the native
loop, so with a `setInterval` and a SIGTERM handler installed it ticked 0 times
in 5 s and ignored SIGTERM; nothing in JavaScript can supervise the server while
a window is open, and it has no menus, no file dialog and one process per
window. Tauri is the better long-term shell (a save panel, a signing and
installing story on all three systems, `keyring`, an updater, single-instance,
and `fennec-http` could run in the app's own process: no child, no orphan, no
stderr parsing, the process's own `fcntl` lock) and a larger rewrite in Rust
outside the zero-dependency set (allowed under `integrations/`) at 3-10 MB for
the shell instead of Bun's 63; re-decided before Windows, below. No server at all, the
studio's local mode over the file's bytes, reads a 100 000-row file (8.3 MB)
in 5.3 ms in Node and the 64 MB graph file in 101 ms, and is the wrong design
for a file: the module has no mmap and the file is loaded whole into wasm
linear memory (+39 MB for the 8.3 MB file, +268 MB, 4.2x, for the 64 MB one,
never given back, wasm32 capped near 2-4 GB where the server opens a 10 GB
file on an 8 GB machine), writes would cross an RPC to Bun to be appended with
an fsync policy of our own (WKWebView has no File System Access API, and the
window would need the RPC bridge, so no sandbox), the `<file>.lock` is an
`fcntl` record lock a page cannot take, and it is a second engine on one thread
without `--warm`, auto-compact, `@ttl` sweeps, `/_changes`, MCP, the admin view
or metrics. It stays right for the site's playground.

Re-decided before Windows and Linux (2026-11-11), from a spike's runs on
`windows-latest`, `windows-11-arm`, `ubuntu-24.04` and `ubuntu-24.04-arm`
(42997677468, 42999915984, 43002802508; #578, closed unmerged): Electrobun
stays. 2.0.4 builds the app on all four and a file opens, is edited through
the studio and reopens on each. What this design rests on holds there: a
server whose holder is killed hard ends in 104-155 ms and frees its file. On
Windows that is only by the end of its stdin: `kill("SIGTERM")` there is
TerminateProcess, ending the server in 1-3 ms with no shutdown line, so no sync
and no checkpoint. Its gaps each have a way round in the app but one:
- The launcher drops its arguments (upstream #483, #554), so a `.fennec`
  double-clicked on Windows or Linux cannot reach the app. This one had none
  in sight then; phase 3 found one: the launcher stays the app's parent, and
  the app reads its arguments back from it (Desktop integration, above).
- WebView2's `sessionClearStorageData` clears the first open view's profile, not
  the partition it is given; the app removes the partition's folder itself.
- Linux's `openExternal` and `showItemInFolder` fall back to
  `system("xdg-open \"…\"")`, a shell over a URL or a path: a URL's `$(…)` ran on
  the runners. The app never calls them on Linux and spawns `xdg-open` with an
  argument vector.
- Linux has no application menu, so the main window's buttons stand in for it.
- Nothing signs on Windows.

Linux needs glibc 2.38 and WebKitGTK 4.1 (Ubuntu 24.04, Debian 13, Fedora 39 and
later; not Ubuntu 22.04, Debian 12 or RHEL 9). Tauri is taken up again only if
a double-clicked file finds no way to the app: neither an argument forwarded
upstream nor a small opener of our own. Phase 3 found a third, smaller than
either: the launcher's own command line, read back by the app.

Measured on a MacBook Air (MacBookAir10,1, Apple M1, 8 GB), macOS 26.1, with
other work running (load average 3 to 4), by `scripts/measure.mjs` on the test
variant after its first launch, five runs each, warm page cache (not a reboot):

| | 50 rows | 100 000 rows (8.7 MB) |
|---|---|---|
| studio showing the collections, from launching the executable | 436-529 ms, median 443 | 443-452 ms, median 445 |
| `fennec-server` listening | 138-211 ms | 142-204 ms |
| idle RSS, the app's processes and WebKit's | 250-251 MB | 274-276 MB |
| ... of which Bun and the launcher, WebKit, the server | 139, 106-107, 5 | 139, 107-110, 27 |
| quit, with the server's checkpoint | 183-449 ms | 326-449 ms |

The DMG is 23.6 MB (the update archive 22.6 MB), the unpacked app 74.7 MB (Bun
63.6, `fennec-server` 4.4), against 23.0 MB and 71 MB for the spike: the icon,
three pages and the failure wording. RSS counts a shared page in every process
that maps it, so a total is an upper bound, and a window is a server process:
the server's share is per open file. Not taken again: the first launch from the
DMG (3.7 s, n=1, in the spike) and the 100 000 x 128 file with a graph (64 MB,
server 145 MB, total 392 MB there). The figures are the test variant's, which is
the shipped app plus the hooks (10 KB of JavaScript). They are of the window as it was
before each got a data store of its own (a window with no page, and the page's
view made in it). The two were measured side by side afterwards, the commit
before and this one, interleaved, on a Mac whose load other jobs moved between
2 and 25: launch to the studio and idle memory are the same within that noise
(for the 50-row file, idle RSS 215-246 MB before and 212-219 MB after, medians of
five). A blank view left under the page's was measured then as about 20 MB more,
which is why it was removed; but that removal did nothing on the launch with a
file those figures take (the window's own view, above), so both sides kept the
view, and the difference was the load. Measured again with it removed for
certain, it frees nothing on macOS, and there it is kept.

## deploy

**`deploy/` is the kit for running regions, held to the binaries by two
checks** (`deploy/RUNBOOK.md`). systemd units for a tenant node, a region's
router and its standby, the global router and its standby, a node's
archiver (`fennec-archive@<node>`) and its rclone copy to a bucket, and
cloudflared; their environment files (`deploy/env/`), alert rules for
Prometheus and Grafana (`deploy/alerts/`) and the runbook. A token goes in
an environment file systemd reads as root (`EnvironmentFile=`, 0600), the
archive key and the tunnel's and rclone's credentials through
`LoadCredential=`, and never on a command line, which `ps` shows every user
(every token has a variable, and an empty one is none: An empty token is
no token, above). Each role runs as a user of its own (`fennec-node`,
`fennec-router`, `fennec-archive`, `fennec-sync`): a process reads
`/proc/<pid>/environ` of its own user's processes, which `ProtectProc=`
hides only from other users, so one `fennec` user let rclone read the
archiver's replication token. No `ProcSubset=pid`: it hid `/proc/meminfo`,
and with it `cache::room`, the compact before a file outgrows the page
cache. The tunnel's hostname is public, so `cloudflared/config.yml` sends a
tenant's paths alone to the router and answers 404 to the rest -- a
tenant's `/t/<t>/_replication` first, which the router would forward to the
node and the one replication token opens: let through, it was every
tenant's rows behind one token from anywhere. cloudflared matches the path
decoded, so `%5F` and `%2F` meet that rule, and `deploy/test/ingress.py`
asks cloudflared's own matcher (`tunnel ingress rule`) of 28 paths. Access
in front says who: the entry sends a service token
(`ACCESS_CLIENT_ID`/`ACCESS_CLIENT_SECRET`, both or neither) and the router
drops `CF-Access-Client-*` before a node (`forwarded_headers`, which
`forward` now shares with `forward_near`: its own copy of the filter kept
the token once only the shared one dropped it). The bucket copy is
`deploy/bin/fennec-archive-sync`, `rclone copy` (never `sync`: a lost
archive takes nothing from the bucket, whose lifecycle rule expires what
`--keep` let go of) with `--local-no-check-updated` -- rclone refused a
segment growing as it read it, so a busy tenant's newest segment never
reached the bucket, every run failing in the test -- and `--fast-list`;
it refuses a first copy from an archive it has not copied before (the
directory's filesystem and inode, noted in its state directory) where the
bucket holds a file the disk does not, since a rebuilt machine's new
archive rewrote the bucket's `history`, `began` and `presence` within the
timer's first five minutes; and writes each success into node_exporter's
textfile collector 0644 -- under the unit's `UMask` it was 0600,
node_exporter read nothing and `FennecArchiveSyncStale` could never fire,
so the test scrapes a real node_exporter (`FennecTextfileUnread`,
`FennecArchiveSyncUnreported` over the absent case). Access outside strict
service token authentication answers a service token's request with a
`CF_Authorization` cookie for the hostname, so the entry drops `Set-Cookie`
from every answer (no router or node sets one), the router drops every
`Cf-Access-*` header (the JWT Access adds too), and the entry refuses an
`http://` router with a token set. `FennecAutoFailoverIncomplete` is read
off the directory -- `fennec_router_tenants > 0` on a node
`fennec_router_node_failed_over` -- since the count of failed failovers,
reset only by a restart, held it after the fix and paged again at the next
node failed over whole; `FennecLeaseRenewalLate` passes over a node failed
over in the last ten minutes (its five-minute window held the outage
after the node came back) and holds a minute (the scrape before a failover
marks its node). The rules read
Prometheus's `up` by job for processes that export nothing because they
are down, a router's role (primary, standby) taken from its last
`fennec_router_following`; `tests/datadog.rs` passes over `job` and
`instance` matchers, holds the jobs to the rules' scrape config and the
textfile metric to the unit writing it. A standby router's
unit carries its primary's flags and the same directory file and conflicts
with the primary's, and a promotion is `POST /_replication/promote` and the
switch of units at once: `--replica-of` makes any file follow again
(`replication::settle_history`), so the standby's unit started after a
promotion would have the promoted file follow the dead router. `make
deploy-check` runs promtool over the rules and their tests, then boots
systemd in two privileged containers (`deploy/Dockerfile`'s `systemd`, whose
boot script makes `/` shared first: systemd in a container leaves it
private, and `LoadCredential=` then hands a service an empty directory) and
installs, verifies, starts and uses every unit (`deploy/test/units.py`:
real rclone into a `local` remote while a tenant takes 64 KB rows as fast
as it can, the copy verified as an archive; every role's user refused
every other role's environment; `/proc/meminfo` read in the node's mount
namespace);
fennec-shard's `tests/datadog.rs` holds every metric and label value the
rules read to a scrape of every kind of process, a leasing router and a
replicated node among them, and Grafana's rules to Prometheus's. `make
drill` (`deploy/drill/drill.py`, the standard library and the Docker CLI)
runs two regions -- a router keeping each one's directory and its standby,
two nodes in two zones under `--lease --replicas --sync always`, an
archiver a node, the global router and its standby -- every token in the
environment and no port fixed, a writer a tenant putting rows by id 1..K
through the routers; it cuts the global routers off (each region writes
on), cuts a node off the network and `docker kill`s it -- a machine lost:
killed alone, its kernel closed its sockets and delivered what they held --
and starts it again, kills a region's router and promotes its standby as
the runbook does, restores a tenant from its node's archive to a moment it
marked, and reads every acknowledged write back from each primary and its
replica, the router's failover read off its log against the killed
container's end (`failed_over_after`, both by the Docker VM's clock). Three
runs at a lease of 2 s and an upstream timeout of 4 s, what they saw and
not bounds (a reviewer's machine saw a repair of 1.07 s, writes stopping
1.31 s after a router's kill before grants were numbered and gaps of 28-30
ms): 41 577
acknowledged, 0 lost; the node failed over 1.92, 2.56 and 1.90 s after the
kill, its tenants' writers writing again 3.90, 3.97 and 3.93 s after the
kill, the tenant whose replica it held losing 17, 18 and 17 ms at most, every other tenant
16, 18 and 17; the repair 0.51, 0.51 and 0.50 s after the node's start; a region
whose router was killed stopped writing 1.67, 1.61 and 1.66 s after and wrote
again 0.43-0.51 s after its standby's promotion -- since grants are
numbered, 1.02, 0.83 and 1.00 s and 0.42-0.48, the node killed failed over
1.90, 2.06 and 2.56 s after, its writers again 3.90-3.94 s, 40 588
acknowledged and 0 lost (three runs of this kit's drill). The writers' figure
was the upstream timeout's, not the failover's: a writer's request in
flight to the dead node waited there until the router let it go, which at
fennec-shard's 60 s was the writer's own 5 s client timeout, every run's
4.86-4.92 s -- quoted at first as a promotion waiting out the replica's
reconnect. The failover answers it now (the paragraph on what a failover
ends, above): in two runs a side in turns, the writers wrote again 2.52 and
1.93 s after the kill as the router failed the node over at 2.53 and 1.93,
against main's 3.94 and 3.95 beside failovers at 2.49 and 2.26. CI's runs met a worse one before #340 -- a dead node's name its
resolver was slow to refuse held up the router's whole lease round, the
region's healthy nodes unrenewed and failed over, 26.5-31.4 s to write again
and 12-18 s without a write for a tenant on a healthy node, the repair
missing its 60 s in four of six runs -- which #340 fixed (the lease
paragraph above). The kit's routers run `--upstream-timeout` at 10 s
(`FENNEC_UPSTREAM_TIMEOUT`), the drill's at twice its lease: chosen at twice
the lease so a failed-over node's requests were let go of, it now bounds
only what no failover ends -- a node not failed over, a relay to a router
gone, a standby's forwards -- and need not follow the lease; past it a
slow statement is a 502 while the node finishes it, and a request
carrying `Fennec-Wait` is allowed the timeout plus its wait. When
the drill fails it prints the last 300 lines of every router's and node's
log (`DRILL_LOG_LINES`), since the cleanup takes the containers and CI
leaves nothing else to read. CI's `deploy` job runs both checks.

### The chaos drill

**`make chaos` breaks a region by seed, one fault at a time, and holds its
history to an oracle.** The drill above runs one sequence, the same every
time; `make chaos SEEDS=N` (`deploy/drill/chaos.py`, the standard library
and the Docker CLI) draws a schedule of faults from each seed and holds
what the clients saw to `deploy/drill/oracle.py`. One region, as the kit
runs it: the global router, the region's router (`--replicas`,
`--auto-failover` at the kit's lease of 5 s, `--upstream-timeout` twice
that) and its standby, three nodes in three zones (`--dir --lease --sync
always`),
four tenants each with a replica in another zone, every token in the
environment. Every container is held to `--cpus` and `--memory`, the
driver too: it runs in a container of the chaos image on the same network,
Docker's socket handed in, so its clients reach the routers by name and
take no port of the machine running it -- loopback load once left 16 000
sockets in `TIME_WAIT` and cut every other process off the network -- and
a client refused or cut off dials again at most 20 times a second, less
as it goes on. The run is removed by its label however it ends, the driver
first when the process running `make` is stopped.

*The workload.* Two clients a tenant, a connection each, through the
region's router (its standby where the router cannot be reached). Each
writes registers -- `put regs {id: k, v: <a value no other write in the
run writes>, at: now()}`, four registers a tenant, and in the same block,
one `/batch`, `put wl {id: <that value>, k: k}`, the ledger -- reads them
back, and appends rows of values of its own (`put log`), and records every
operation
with when it was sent and answered, on the driver's monotonic clock, and
how: answered (2xx, with `Fennec-Seq` and `Fennec-History`); refused,
which cannot have run -- a 4xx, a 503 (every 503 says nothing landed,
[quotas.md](quotas.md#retry-after-in-bodies)), a connection never made;
or neither -- a 500 (`outcome: unknown` among them), a 502 or 504, a
request sent and never answered -- which may have run, then or later.
Each tenant starts with 200 rows of 1 KB, so a move's image and a copy's
catch-up over a slowed link take seconds.

*The faults*, drawn by weight from the seed, each held for half a lease to
four, then healed:

- `kill`: `docker kill` of a node -- the process lost, the kernel
  delivering what its sockets held, the case the replication paragraph's
  bound covers -- and started again;
- `pause`: `docker pause` of a node, shorter or longer than a lease. The
  freezer stops the process and not its clocks, so a node woken past its
  lease holds none, and a write it was answering is `outcome: unknown`
  (`answered_under_lease`), never a 200 its failover lost;
- `router`: tc netem between the region's router and a node, both ways: a
  delay of 50-400 ms with a quarter of it as jitter, a loss of 5-30%,
  both, or a partition (all of it lost). Never its standby's, which
  leases nothing and carries no client: drawn a third of the time as the
  drill first stood, it was a fault nothing met;
- `replica`: the same between two nodes a tenant's stream runs between,
  its primary's and its replica's as the router places them -- as first
  drawn, any two nodes, a pair that may share no tenant;
- `catch-up`: a node holding a primary killed long enough to be failed
  over, started again with its peers' links to it slowed to 256 kbit-1
  Mbit, and killed and started again while a copy the router named on it
  holds fewer appends than its primary -- the copy of a tenant it lost,
  which follows the new primary from an image, its history forked past;
  a node holding replicas alone has them taken off as it is failed over;
- `move`: a tenant moved to the third node, its source's link to the
  router slowed so that the image takes seconds, and the source killed,
  paused or cut off from the router once the tenant is seen `moving`, for
  more than a lease: a failover during a move;
- `clock`: the clock of a node holding a primary moved an hour to 30 days
  ahead or back, every copy on the node read back -- `get probe select
  now()`, the node's own reading -- the shift away from the system's clock
  before it holds, and the system's again, every one, after.

*Each fault lands, or is drawn again.* Each says whether it did: a kill
or a pause the container's state shows, a link's netem tc shows on both
ends, a catch-up a copy seen behind, a move a tenant seen moving, a clock
every copy on the node read shifted. As the drill first stood, a
catch-up whose copies caught up before it looked, a move that ended
before its source was hit or found no tenant, and a link fault on a pair
sharing no tenant or on the standby router went on as faults, and a seed
could pass with a kind never made. One that did not land is drawn again,
the same kind; three in a row fail the seed, naming why, and the run goes
on past its minutes until every kind has landed once. The summary counts
each kind landed and drawn again.

netem is applied from inside each container (`--cap-add NET_ADMIN`, the
image's `tc`) on a fourth band of a `prio` qdisc that no priority maps to,
filled only by a `u32` filter per peer address, so a fault touches the one
link it names; the run first applies one and reads it back, since a kernel
without `sch_netem` would make every link fault nothing and the run a
pass (CI's runner loads the module).

*The clock.* Docker cannot move one container's wall clock -- a time
namespace offsets `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME` alone -- and the
kit's binaries are static musl, so no faked `clock_gettime` can be
preloaded. The nodes run fennec-server built with `test-clock`
(`crates/fennec-http/src/test_clock.rs`): where `FENNEC_TEST_CLOCK` names
a file, a thread sets every open tenant's clock (`Database::set_clock`, the
seam the browser module hands its time through) to the system's time plus
the milliseconds the file holds, every 50 ms while it holds one -- a
pinned clock stands still -- and back to the system's when it holds none.
The registry passes over a slot busy as it looks (an open, a close, an
install: `open_tenants`' `try_lock`), so the system's clock is set again
every 50 ms until no slot was passed over, for 10 s at most, the node
saying so past it: set once, as first built, a tenant whose slot was busy
at that moment stood at the shifted time for good
(`a_tenant_busy_as_its_clock_shift_ends_takes_the_systems_clock_back`, run
in CI's test job with the feature).
A file, not a route: a route is one the OpenAPI document lists
([http.md](http.md#openapi)) or one a test build answers that no release
does, and the file is reachable only from the node's own machine, as its
clock is. What it moves is what reads a database's clock: `now()` -- the
registers' `at`, which a replica must hold as the primary wrote it, read
in the digests -- and the rows a `@ttl` leaves out. Not the lease, which
runs on the boot clock and is no wall clock's to move
([tenants.md](tenants.md#scaling-out-by-tenant)), nor what reads the
system's time itself (the sweep, a token's `exp`, the archive's stamps).
It stays out of a release by what is checked, not by care. Off, the
feature compiles nothing (`#[cfg]`). `tools/release_features_check.py`, in
CI's fmt job, holds that only fennec-http and fennec-server define it, as
`[]` and `["fennec-http/test-clock"]`, that no other feature -- `default`
above all -- nor any dependency turns it on, and that nothing builds with
it -- `--features ...test-clock`, `FEATURES=...test-clock` or
`--all-features` outside a `cargo clippy`, `test`, `check`, `doc` or
`metadata` -- but `deploy/Dockerfile`'s `chaos-build` stage, which only its
`chaos` stage copies from and no stage is built on. A binary built with it
says `fennec-server <version>+test-clock` on `--version`, and `release.yml`
refuses one: every tarball's fennec-server is run where its runner runs
it, its line held to `fennec-server <version>`, and read for the mark
where it does not (the Mac's x86_64), and the image's is run before it is
pushed; the `.deb` and the `.rpm`, made from the Linux tarballs, are held
to the same line once installed (`tools/linux-packages-check.sh`). As
first written, this paragraph said that last check ran on every release's
tarballs: it ran on the Linux packages alone, and nothing looked at the
Mac's tarballs or the image. CI's clippy job lints the gated code with the
feature, and fennec-server's `tests/test_clock.rs` holds both builds: the
shift moves `now()` a hundred years and back in a test build, and moves
nothing, and is never mentioned, in every other.

*Settling.* After each fault every node and router runs unpaused, no netem
is left -- read back with tc, five clears at most, before any copy is
compared: a clear's failure was passed over, and a link left delayed would
have held a copy behind through the next comparison -- every clock is the
system's, and every tenant is placed, active, with a replica, within 180 s.
The router repairs on its own as a node answers again; past 20 s
without, the drill does every 20 s what the runbook has an operator do
(*Replicas*, *Files left on nodes*): a repair (`POST /_shard/replicas`),
each file it lists as left removed on its node
(`DELETE /_admin/tenants/<t>`) where the directory places neither the
tenant nor its replica, and a repair again, every file removed recorded.
It is needed: a node failed over leaves its replica copies behind as
files noted with no identity -- a detached replica that stayed, which the
router never takes back or follows over by design
([tenants.md](tenants.md#replicas-tried-again)) -- so a region of three
nodes, two of them failed over in turn, had no node left to put a
tenant's replica on, and the first runs stopped there -- a gap an operator
meets too (RUNBOOK.md, *Known gaps*), whose fix is proposed in
[tenants.md](tenants.md#a-detached-copy-taken-back-by-its-identity). Then
the clients pause, and each tenant's replica must hold its primary's rows
-- `regs`, `log` and `wl`, read from each node directly, a SHA-256 of them
by id -- within 60 s, the placement read again at each try, since a lease
missed under load moves the primary. A replica that never does is a
violation, and ends the run. So every fault starts from a region whose
copies are caught up. Last, each tenant's primary is written once on
`probe`, directly, and its `Fennec-History` is the term it answers in as
the next fault starts.

*The oracle.* At the end the clients stop, the region settles once more,
and every register is read once more through the router. Then:

- *registers*: fennec-bench's YCSB checker (`check` in
  `crates/fennec-bench/src/bin/ycsb.rs`, [benchmarks.md](benchmarks.md#ycsb))
  for one field a record, with a write whose outcome is unknown taken as
  one that may land at any moment after it was sent -- an end at infinity,
  overlapping every later read and never the last to end before one: a
  read must find the value of a write that could have been the last before
  it, one that ended before the read began and was followed by none that
  also did, or one that overlapped it, or nothing where no write had ended
  before it. A write answered and lost is found by the first read after it
  that finds an older value, the final reads at the latest; a read is held
  to the writes and not to the reads before it, as YCSB's is, so a read may
  see a write before its fsync and a later read the value before it (a
  plain read may, [named-statements.md](named-statements.md#where-a-run-happens-and-once));
- *the ledger and the appends*: every register write answered is in its
  tenant's `wl` and every append answered in its `log`, and none refused
  is in either -- a refusal that ran is a client told to send again what
  landed. The registers alone could not see every loss: a write lost and
  written over before any read of its register, or lost behind an earlier
  write whose outcome is unknown (w10 answered, w11 unknown, w12 answered
  and lost, a read finding 11, which may have landed after 12), passed;
  the ledger's row is in the write's own block and never written over;
- *epochs*: no tenant written by two primaries at once, read off the
  positions its answered writes were given. A promotion forks the history
  into the next term under a fresh id, and a move keeps it, so within one
  history no change number is given two writes, a history's id names one
  term and a term one id, and no write is answered in a term after one
  sent after a later term's write was answered: an older term answering
  after a newer has is the primary failed over still writing. The review
  found the check of a term's one id missing: two copies promoted from term
  2 both answer in term 3, each under an id of its own, and interleaved
  writes under the two passed every other check.

*What a failover may lose, and nothing more.* A write answered and lost is
a violation unless it is in the window the design names (*A node lost* in
RUNBOOK.md, the replication paragraph above): a write answered while its
replica is behind -- its stream past `SENT_WAIT` and not waited for until
it catches up, or no stream at all, the copy down or made anew -- is lost
with its primary. The oracle holds each lost write to that by what the
drill did (`documented`): each fault's span runs from the settle before it
to its own, and records each tenant's primary, replica and term as that
settle found them and what the fault held behind -- the nodes whose copies
it killed, paused or made catch up, the links it impaired. A loss is in
the window when its term is not the one its tenant ends in (a promotion
ended it) and either it was answered in the term its span began in, by
the primary whose replica the fault held behind, once the fault had begun
-- a replica fault's lag, say, and a lease missed under load failing the
primary over meanwhile -- or in a later term, one a failover of that span
began, whose new copy
starts behind, before the settle saw it caught up. Such a loss is counted
(`acknowledged_lost` in `result.json`) and named with its reason, and its
register write is taken as one whose outcome is unknown by the register
check; every other loss fails the seed. The window is the drill's
knowledge of what it held behind, never wider: a lag no fault made -- a
replica starved of CPU past `SENT_WAIT` -- with a failover beside it fails
the seed, and its logs say which it was.

`deploy/drill/test_oracle.py` holds each check to histories made by hand,
each failing where it must. A violation is reported with its seed, the
schedule (`result.json`), the history and every container's log; the
schedule is the seed's (`make chaos SEEDS=1 SEED0=<seed>` draws it again),
the moments are the machine's.

*One fault at a time.* The replication paragraph's bound is what is
tested, not past it: a durable write is answered once every stream has
written it into its socket, a lagging one not waited for past `SENT_WAIT`,
so a primary that dies while its replica lags, or a machine lost with a
write in its socket, loses what it answered -- the window the design names
([named-statements.md](named-statements.md#what-a-sender-is-told), the
failover table's last row). Faults overlapping on two links or two nodes
would find that window and nothing else; one at a time, each settled
before the next, a loss outside it is a bug. Load overlaps them anyway: a
lease missed on a loaded machine fails a node over beside whatever fault
runs, so a replica fault's lag can meet a failover no fault made. At a
lease of 2 s, on a runner of 4 CPUs under 5.5 CPUs of quota, that failed
seeds with no bug, which the window above now names and counts, and the
kit's lease of 5 s makes rare.

*In CI.* `.github/workflows/chaos.yml`, weekly and by hand: 50 seeds of 10
minutes each at a lease of 5 s, more than a hosted job's six hours, so
three jobs of a share each after one that builds the image, each run's
seeds its own (its number times 1000 on), every seed's result and a failed
one's history and logs kept for 14 days. Never on a pull request: hours
long. A dispatch's `seeds` is digits or the job stops before any
arithmetic -- bash evaluates a variable's text as an expression, so
`a[$(...)]` would run a command in `$(( n / 3 ))` -- and `minutes` a
number. `CHAOS_KEEP=1` leaves a failed seed's containers, volumes and
network for a look and runs no seed after it; the sweep after each seed
removed them as first written.

*What the first runs saw*, on a laptop whose load other work held at 50 to
110, seeds 1 to 4 (1.5, 2.5, 2.5 and 2.5 minutes of faults): 32 faults,
every kind in each seed of 2.5 minutes, 36 settles with every replica's
digest its primary's, 51 132 register writes and 22 603 appends answered
and 39 622 reads, 43 operations whose outcome was unknown, no violation.
Leases missed under that load failed nodes over besides the faults: a
tenant reached its fifth term in 4 minutes. Eleven files left on nodes were
removed as the runbook's operator removes them. The first runs stopped on
the drill's own faults, each fixed: a pad row of id 0 (an id is positive),
a region that had no node left for a replica (the operator's repair above),
and the oracle formatting a passing read's message with no write before
it, which ended two seeds' checks -- their histories, checked again, held.

*What the review's runs saw*, at a lease of 5 s on the same laptop, Docker
otherwise idle and other agents' test suites holding its load at 60 to
137, seeds 901 to 903 of 2 minutes each and 902 a second time: 27 faults,
every kind in each seed, each landing at its first draw; the clock read
the shift away on every copy of its node -- 3 599 982 to 3 599 985 ms of an
hour ahead, 2 591 999 953 to 2 591 999 978 of 30 days, -86 400 031 and
-86 400 077 of a day back -- and the system's clock again after. 901, 903
and 902's second run passed: 24 settles with every replica's digest its
primary's, 59 194 register writes, 26 260 appends and 46 321 reads
answered, 38 operations whose outcome was unknown, no acknowledged write
lost in the window the design names or out of it, 8 files left on nodes
removed as the runbook's operator removes them. 902's first run stopped
after its catch-up: t1's copy on n2 -- the old primary's file, which the
router's repair had follow the new primary on n1 as n2 answered again,
n2 then killed by the fault while that copy was behind -- held none of the
primary's writes since the first kill for the 60 s the settle waits, every
link read back clear and every node running, and n2 logged nothing of
replication once it started again. CI's run before the review (43464712047,
a lease of 2 s) stopped the same way on seed 1003, its copy on n1, while
its two other seeds passed, and so did all three of the review's hosted
run at a lease of 5 s (43482324234, seeds 2001 to 2003, each at its
catch-up fault). It was fennecdb's, not the drill's: the record making the
old primary's file a follower's was left in the sink's buffer as the
follow was answered, and a copy waiting on its image writes nothing that
would have written it behind. The kill took the record, and the restart
opened the file as a primary's, following nothing
([replication.md](replication.md#replication), *A restart reads a copy's
role from its file*). A follow now syncs it before it is answered, and an
image comes with it inside its rename.

## Install channels

**A release is installed from its own files, each checked before it is used.** Four ways to install `fennec` and `fennec-server` other than a registry or the image -- Homebrew's formula, `https://fennecdb.com/install.sh`, a `.deb` and an `.rpm`, and on Windows `https://fennecdb.com/install.ps1` -- all take the archives `release.yml` built (the musl tarballs for Linux, which are static, the two macOS ones, and the two Windows zips) and build nothing themselves. Each names what it installs by a SHA-256, which is what holds them to one source: the formula carries the four sums of `SHA256SUMS`, each installer checks the tarball or the zip against the sum of the release it came from, and the packages are made from those tarballs and listed in `SHA256SUMS` with the rest. Because all three name the release's files by checksum, a published release's assets are never replaced: re-running `release.yml` for a tag whose release is public (`publish`) uploads only the files the release has no asset of, with a warning that its `SHA256SUMS` does not list them, where a draft's are replaced as before. A rebuild is not byte for byte the first build (the Rust toolchain, the runner image), so a replaced tarball would have failed every `brew install` and every sum already taken from the old `SHA256SUMS`. The image jobs still push `:X.Y.Z` and `:latest` again on a re-run; the image is not named by a checksum anywhere here. One file of a published release is written again, and only by growing: `desktop.yml` appends the app's lines to `SHA256SUMS` -- the Mac's DMG, Windows' zips and Linux's tarballs -- every line before them kept byte for byte, and never replaces a download ([Studio desktop](#studio-desktop), The release).

The formula is `tools/homebrew-formula.py`'s output and nobody's text. The release job and a maintainer by hand run the same script on the release's `SHA256SUMS`. Two checks hold a copy, and neither is enough alone. `--check` writes the formula again from the version and sums the file holds, for `fennec-db/fennecdb`, and fails on any difference, so a hand edit of the text or of the repository is found by CI (the `packaging` job), as is a template change not followed by a new formula; it cannot find a sum edited to another value, since the sums are the file's own input, and a repository edited in the header and the urls together passes unless the repository must be this one, which it must (`--repo`). `--verify` downloads the four tarballs from the repository's own releases and holds them to the sums, which is what `brew install` will do and what finds that sum; CI runs both on the committed copy, which is the one a maintainer puts in the tap, and the release job runs both on what it is about to push. The formula is pushed by `packages.yml` (`homebrew`) once the release is published, not by `release.yml` at the tag, because it names the release's download URLs and a draft's are not public: a tap updated at the tag would send `brew install` to a 404 until someone published the notes, where a tap a few minutes behind sends it nowhere. A draft or a pre-release is not offered (the event says it, and a dispatch asks the release), since `brew upgrade` would hand a pre-release to everyone, and neither is a version older than the formula the tap holds (`sort -V` on the version in its url), so that a dispatch for an old tag, or two releases published close together and finished out of order, cannot move the tap back; the older run says so in a warning and in the summary and ends green. One run pushes at a time (`concurrency: homebrew-tap`, never cancelling one under way). The push takes `HOMEBREW_TAP_TOKEN`, a fine-grained token with Contents read and write on the tap alone, kept as a secret of the `homebrew` environment, which only `main` and `v*` tags may use (RELEASING.md has the commands), so a workflow on any other branch cannot read it; the repository-level secret of that name is deleted, because a repository secret is every workflow's. The job hands the token to `git` as an `http.extraheader` in `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0`, which is in no URL, no argument and no `.git/config`, and the encoded form is masked as well as the token. A job with the secret set and a push that fails fails: a tap that silently stays a release behind is the failure nobody sees, so only the secret's absence is a notice. The formula's `test do` asks each binary for its version and runs a three-line script through the shell in memory. From 0.1.13 that is `fennec --version` and `fennec-server --version`, each held to `<name> <version>` on stdout and an exit of 0 (`shell_output`). A release before it has no version flag -- `fennec` rejects `--version`, and `fennec-server` names its version only on the first line of `--help`, on stderr -- so its formula asserts that line instead: the renderer picks the form by the version it writes (`VERSION_FLAG`), and the formula of a release from before the flag -- whichever one the tap holds, or one written again for an older tag -- comes out byte for byte as it did before, which `--check` on the committed 0.1.11 copy holds. Cutting over by version rather than by asking a binary keeps the script from downloading anything to write a formula. `tools/linux-packages-check.sh` makes the same choice by the binaries' version (`FENNEC_BINARY_VERSION`, else the package's), since CI runs it on the latest release, which may be one from before the flag. `brew audit --strict --online`, `brew style`, `brew livecheck` and `brew test` were run on it from a throwaway local tap (Homebrew installs no formula from a bare path), and `brew test` was seen to fail on a wrong version. The `--version` form was held to `brew style` and `brew test` the same way, on this code built as 0.1.13, and the packages check to both forms: 0.1.11's packages took the `--help` path, packages of binaries built as 0.1.13 the flag's, and 0.1.11's binaries named 0.1.13 failed it on `fennec --version`.

`site/install.sh` is served by the site's worker as a static asset (`site/build.py` copies it to `dist/install.sh`, and `_headers` serves it as `text/plain` so a browser shows it, cached five minutes so a fix reaches the next install). It is POSIX `sh`, and outside its functions there is nothing but `set -eu`, comments and plain assignments before `main "$@"` on the last line, so a copy cut short stops at a syntax error or before the call and runs nothing: `build.py` refuses a script that is not that, and `site/test_install.sh` cuts it at some 300 points and asserts none runs anything. It works the platform out (a Rosetta shell on an Apple silicon Mac gets the native build; Windows is refused, `install.ps1` and WSL named), finds the latest release by the tarball names in `releases/latest/download/SHA256SUMS` or takes `FENNEC_VERSION`, downloads over https, checks the sum before it unpacks, and puts the two files in `~/.fennecdb/bin` or `$FENNEC_INSTALL/bin` (made absolute, so the `PATH` line it prints is good from anywhere) by writing beside and renaming over, so a running server's file is not overwritten in place and a copy that fails leaves no staged file. It edits no shell file and prints the `PATH` line instead; it evals nothing, and a version is held to `[0-9A-Za-z.+-]` before it goes into a URL. The downloader is curl (`--proto =https --tlsv1.2`) or a wget that checks certificates: GNU's, run with `--https-only --secure-protocol=TLSv1_2`, and BusyBox's only where it hands the connection to `ssl_client`, which Alpine's does (it refused a self-signed certificate) and upstream's own TLS does not -- that one checks nothing, so a BusyBox wget without `ssl_client`, or one that says "certificate validation not implemented", is refused with the instruction to install curl, and any other wget is refused as unknown. `FENNEC_BASE_URL` moves the releases to a mirror, https or `http://localhost` and `http://127.0.0.1` with an optional port alone (matched whole, with any `@`, so `http://localhost:x@example.com`, a request to example.com, is refused, as is a space), which is how the test serves a release of its own. The sum is the same release's, so it catches a damaged or truncated download and not a release someone replaced, which the docs say. It was run for real on macOS (arm64, and under Rosetta) and in `alpine` (busybox `wget` and `sha256sum`; arm64, and x86_64 under emulation) and `debian`. On Windows it names `install.ps1` and stops ([windows.md](windows.md#6-a-windows-release) rejects extending it to Git Bash and MSYS).

`install.sh --app` installs the app instead of the two programs, by the same rules: the platform first (macOS on Apple silicon, a Rosetta shell's included by `hw.optional.arm64`, or Linux on x86_64 or arm64, below; an Intel Mac, FreeBSD or a 32-bit processor is told what the app is for, Windows' shells are sent to `install.ps1 -App`, and nothing is downloaded). On a Mac it takes the DMG: the release found as for the programs (the latest by its `aarch64-apple-darwin` tarball, or `FENNEC_VERSION`), and `fennec-studio-v<version>-macos-arm64.dmg`'s sum from that release's `SHA256SUMS` checked before the image is attached (`desktop.yml` appends the line, [Studio desktop](#studio-desktop)). It attaches it read-only, `-nobrowse` and at a mount point in its own temporary folder, copies the app out with `ditto` into a staging folder inside `~/Applications` (made if missing, so the rename that follows stays on one volume), lets the image go, holds the copy to `codesign --verify --deep --strict` (ad hoc, but it must hold: a copy cut short or an app changed after it was signed is refused), and only then renames the old app aside and the new one into place, putting the old one back if the second rename fails. An app running from that place is refused before anything is downloaded: a process whose command starts with the bundle's `Contents/MacOS/`, read from `ps` by `awk` (not `pgrep -f`, whose pattern is a regular expression, nor a `grep`, whose own command holds the path and matched itself in the first version). Its cleanup lets the image go before it removes the temporary folder that holds the mount point. It needs no `sudo` and writes nothing but the app. A file `curl` downloads carries no quarantine, so the app opens with no Gatekeeper prompt, where the same DMG from a browser asks once (Open Anyway). `site/test_install.sh` refuses `--app` on an Intel Mac, FreeBSD, a 32-bit ARM Linux and Windows' shells, under a `uname` and a `sysctl` that answer as told; on a Mac (CI's macOS `test` job) it installs a stand-in DMG (a bundle around `/bin/sleep`, signed ad hoc), again over it, under Rosetta and pinned, and refuses a tampered DMG, an app whose signature does not hold, a release with no DMG and an app running from the place (a `sleep` whose argv[0] is a path inside it: a copy of `/bin/sleep` there is killed at exec, arm64e running only as the system's own), each leaving no image attached and no staging folder; and `desktop.yml`'s app job runs the same on the DMG it just made (`FENNEC_TEST_DMG`).

On Linux, `install.sh --app` first asks whether the system has what the app links, before anything is downloaded, and says what to do where it does not: glibc 2.38 or later (`getconf GNU_LIBC_VERSION`; none, as on musl, is said as such), since Electrobun's native library takes `GLIBC_2.38` symbols; `libwebkit2gtk-4.1.so.0` and `libayatana-appindicator3.so.1`, in the loader's cache (`ldconfig -p`, looked for in `/sbin` and `/usr/sbin` too, off a user's `PATH` on some systems) or, with no `ldconfig`, the usual library folders, with the packages that hold them on Ubuntu and Debian, Fedora and Arch; and `xz`, which tar hands the `.tar.xz` to. The release is found by the musl tarball of the machine's processor, and `fennec-studio-v<version>-linux-<x64|arm64>.tar.xz`'s sum checked before it is unpacked -- into a staging folder beside its place, `${XDG_DATA_HOME:-~/.local/share}/dev.fennecdb.studio/stable/app` (where Electrobun's installer puts it; a relative `XDG_DATA_HOME` is ignored, as the spec says), the old app renamed aside, the new one into place, the old one put back if that fails; the folder above it is the app's own (`state.json`) and is never touched. An app running from its place is refused, as on the Mac (`ps -A -ww -o args=`, which both systems read alike). Its menu entry, `applications/fennecstudio.desktop` beside it (the name Electrobun's installer uses, so one entry either way), is written beside and renamed into place: `Exec` the launcher's path in double quotes, `StartupWMClass=fennecstudio`; a path holding a quote, a backslash, a dollar, a backquote, a percent sign or a newline -- each of which the entry spec would want escaped twice over -- gets no entry and is said, the app installed. It prints how to start the app and the two paths that remove it. `site/test_install.sh` holds it on any host, a `getconf` and an `ldconfig` answering as told: a stand-in app (a launcher that sleeps) installed for x64 and arm64, with and without `XDG_DATA_HOME`, pinned, again over it keeping the app's `state.json`, its entry's `Exec`, nothing else in the home; refused on glibc 2.35, with no glibc, without WebKitGTK, without AyatanaAppIndicator and without `xz`, each downloading and installing nothing; refused while it runs, for a tampered tarball and for a release with no Linux app; and a home with a dollar in its path, the app and no entry. `checks/dist-archive.mjs` runs it on the real tarball on each Linux runner and starts what it installed by the entry's `Exec` ([Studio desktop](#studio-desktop)).

The Windows zip and `site/install.ps1` are [windows.md](windows.md#6-a-windows-release)'s decision 6. `release.yml`'s `windows` job builds `fennec.exe` on the cli profile and `fennec-server.exe` on release for `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`, each on a runner of its architecture (`windows-latest`, `windows-11-arm`), with the C runtime linked in (`+crt-static`, `tools/windows-zip.sh`, under the runner's bash as the tarballs' build is), so they ask a fresh Windows for no Visual C++ redistributable, and zips them as `fennec-v<version>-<target>.zip`, the folder the tarballs hold with the same five files -- Windows' own `tar.exe` (libarchive's bsdtar) writes the zip, Git's writes none. Before it is kept, `tools/windows-zip-check.ps1` holds it as `linux-packages-check.sh` holds a package: the folder and its five files, neither program importing the Visual C++ runtime (`vcruntime140.dll`, `msvcp140.dll`, the UCRT's `api-ms-win-crt-*.dll`, read from each program's import directory), then `install.ps1` installing it from the folder it was made in under Windows PowerShell 5.1 and PowerShell 7, each program saying its version, and the shell writing a row into a file and reading it back in another process; a failure stops the release, as a package that does not install does. CI's `windows (release zip)` job runs the same two scripts on every change, so a tag meets no step of the Windows release CI has not run -- the Unix release's steps are held only by their pieces before a tag (the `build` job's profiles, the `packaging` job's installer and packages), which a Windows runner's slower start made worth a whole run here. A tag from before the scripts, dispatched again, builds no zip and says so. The zip is in `SHA256SUMS` with the rest (`publish` globs `*.zip`), and unsigned: signing is the owner's decision.

`install.ps1` is `install.sh`'s counterpart, run as `irm https://fennecdb.com/install.ps1 | iex`, and served the same way (`_headers`, `text/plain`, five minutes). `iex` runs a text in the caller's session, so the script is `function` definitions, each named for fennec, and `Install-Fennec @args` as its last statement, and ASCII, which Windows PowerShell 5.1 reads a file without a byte order mark as. `tools/install-ps1-check.ps1` holds the first with PowerShell's own parser (`Parser.ParseFile`), not a reading of lines -- one that let a quote inside a string end the check's idea of a string, and a `}; command` after a function's closing brace, through: it fails unless the file parses, its top level is those and nothing else (no other statement on any line, no param, begin/process/end block, trap or `using`), and no command anywhere in it, a nested block or a string's `$( )` included, is `Invoke-Expression`, `iex`, `Invoke-Command`, `Add-Type`, `Start-Job`, `Start-Process` or an alias-making command, under any module prefix or quoting, no command's name is computed but `& (Join-Path ...)`, and no member is `Create`, `NewScriptBlock`, `InvokeScript`, `ExpandString`, `InvokeCommand` or `Invoke`, nor `Run`, `Exec` or `ShellExecute` (the shell's COM objects' ways to start a program: `-App` makes its shortcut with WScript.Shell, which has the first two), or computed, nor a `[scriptblock]` type named (its strings and comments aside: the usage names `irm | iex`). CI's Windows job runs it under both PowerShells and fails copies made to run a command (`test_install.ps1`); `build.py` runs it with the `pwsh` on its PATH -- the site's deploy and CI's ubuntu job have one, a build without it says so, and under CI it is a failure -- and itself holds the ASCII and `text/plain`. A copy cut short parses to nothing or defines functions and runs none. Run in the caller's session, it assigns no variable there, but its fennec-named functions stay defined until the session ends; its settings (`Set-StrictMode`, `$ErrorActionPreference`, `$ProgressPreference`) set inside its functions, and the one process-wide setting it needs, Windows PowerShell 5.1's TLS versions (`ServicePointManager.SecurityProtocol`, held to TLS 1.2), is put back as it was. A failure is one line, `fennecdb install: ...`: run from a file it exits 1, through `iex` it throws, which fails the line it was run from -- `powershell -Command "irm ... | iex"` exits 1 -- and not the session. It finds the machine's processor in the system's environment (`HKLM\...\Session Manager\Environment`), since an x64 PowerShell on an arm64 Windows says AMD64 in its own, and takes that processor's zip and no other -- a release without the zip for an arm64 machine is refused, naming the release, since an x86_64 program runs on Windows 11's emulation and not on a Windows 10 for arm64, and a release has both Windows zips or neither; the latest release by the zip names in `releases/latest/download/SHA256SUMS`, or `FENNEC_VERSION`, held to `[0-9A-Za-z.+-]`; `FENNEC_BASE_URL` held to https or `http://localhost` and `http://127.0.0.1` as `install.sh` holds it; and `FENNEC_FROM`, a folder holding a release's zip and `SHA256SUMS`, from which it installs with nothing downloaded -- the release job's check, and a machine off the network. Downloads are `Invoke-WebRequest`'s, held to TLS 1.2 or 1.3 under PowerShell 7 (`-SslProtocol`), and a redirect from https that ends anywhere else is refused and its bytes removed, Windows PowerShell 5.1 following one that PowerShell 7 refuses. The sum is checked before `Expand-Archive`, then the zip's mark of the web, where it has one, is taken off (`Unblock-File`), so nothing it installs carries the mark into SmartScreen's warning. It installs into `%LOCALAPPDATA%\fennecdb\bin` or `$env:FENNEC_INSTALL\bin` (made absolute against the session's folder), both programs copied beside their places and then each renamed into its place, the first put back if the second cannot be (the failure names it, or the one it could not put back); Windows refuses to write over or delete a running program's file but lets it be renamed, so a program there already is renamed aside first, removed once nothing runs it -- at once, or by the next install, which sweeps what the last left -- and an upgrade under a running server installs, the server keeping its old file until it stops. It touches no profile and no `PATH`, user's or machine's: it prints the line that puts the folder on the session's `PATH`, and names the account's environment dialog for the user's `Path`, never a command writing the registry (.NET's `SetEnvironmentVariable` writes the user's `Path` back as `REG_SZ` with its `%...%` names expanded, and `setx` cuts it at 1 024 characters). It runs nothing it downloaded but the installed `fennec-server.exe --version`. `site/test_install.ps1`, in CI's `windows (release zip)` job, holds it under both PowerShells against the zip that job built, the release laid out on disk and served by Python on port 0: an install from the URL and from a folder, again over the first, over a running server and once it stopped; on the `PATH` and not; a pinned version with and without its `v`, one with no release, one not a version; a latest release with no Windows build (every one before slice 5), a tampered zip from either source, both sources named at once, a folder with no `SHA256SUMS`; every refused base URL `test_install.sh` refuses; a relative `FENNEC_INSTALL`, one that cannot be made, an unknown argument and `--help`; through `iex`, the session's settings unchanged and a failure failing the line; the script cut at every line's end, across its bytes and through its last line short of the call's name, each cut run through `iex` with nothing run; a zip marked as a browser marks one installing programs that carry no mark; and no profile and no value of the user's environment touched across all of it. It prints what Windows does with the mark of the web -- whether `Invoke-WebRequest` marks a download, from the internet and from the machine, and whether `Expand-Archive` carries a zip's mark onto what it unpacks.

`install.ps1 -App` (or `--app`) installs the app instead of the two programs. `irm | iex` hands a script no argument, so the line the docs give makes the downloaded text a script block and calls it with `-App`, `& ([scriptblock]::Create((irm https://fennecdb.com/install.ps1))) -App`: the same text, its functions defined in that block's scope and gone after it. The release is found as for the programs, by the zip of the machine's processor (or `FENNEC_VERSION`), and `fennec-studio-v<version>-windows-<x64|arm64>.zip`'s sum checked before anything reads it, from the URL or from `FENNEC_FROM` alike (`Get-FennecAsset`, which the programs' path takes too), its mark taken off. It refuses while a program runs from the app's place (`Get-Process`'s paths: Windows refuses to move a folder a running program's file is in), unpacks the zip into a staging folder beside that place, `%LOCALAPPDATA%\dev.fennecdb.studio\stable\app` (Electrobun's installer's, so one copy either way; the folder above it is the app's own and never touched), renames the old app aside and the new one into place, putting the old one back if that fails and saying where it is if it cannot, and sweeps what an earlier install could not remove. A Start-menu shortcut, `fennec studio.lnk` in `$env:APPDATA\...\Start Menu\Programs` (Electrobun's name too), is written through WScript.Shell's `CreateShortcut`, the one writer of the binary `.lnk` every Windows has; one that cannot be written is said, the app installed. No administrator, no `PATH`, no registry, and nothing it unpacked runs; there is no uninstall entry (Electrobun's installer writes one, with its own uninstaller), so it prints the two paths that remove the app. `site/test_install.ps1` holds it under both PowerShells with a stand-in (the folder `fennec-studio`, a copy of `ping.exe` as its launcher, which runs from anywhere and can be kept running): from a URL into a `LOCALAPPDATA` and an `APPDATA` of the test's own, the shortcut's target the launcher, nothing beside the app, the programs not installed; again over it, the app's `state.json` kept and no staging left; spelled `--app`; refused while the stand-in runs from its place, for a tampered zip, for a release with no app and for a latest release with no Windows build, each installing nothing; from a folder whose zip a browser marked, the launcher carrying no mark; and through the script block the docs give, the session's settings unchanged and no function of the script left defined. `checks/dist-archive.mjs` runs it on the real zip under Windows PowerShell 5.1 on each Windows runner and starts what it installed by the shortcut's target ([Studio desktop](#studio-desktop)).

The packages are nfpm's (`packaging/nfpm.yaml`): one description makes both formats on a runner of either architecture with no dpkg or rpmbuild, where `rpmbuild` would have wanted a spec, a build root per architecture and its strip and requires scripts turned off for a prebuilt static binary. nfpm itself is the release binary `tools/install-nfpm.sh` pins by checksum (copied from the release's `checksums.txt`), shared by `release.yml` and CI. The deb's version carries nfpm's `release: 1` as its Debian revision (`0.1.11-1`, in the file's name too), `version_schema: semver` turns a pre-release's `-rc1` into `~rc1` in the package's version (`0.2.0~rc1-1`, an rpm's `0.2.0~rc1`; the file's name keeps the `-`), which sorts before `0.2.0` as a pre-release must, and `tools/linux-packages-check.sh` holds both packages to that form and to the ordering (`dpkg --compare-versions`, `rpm.vercmp`). `SOURCE_DATE_EPOCH` fixes the files' times: the same tarballs gave the same `.deb`, byte for byte, from nfpm on a Mac and on Linux, but the `.rpm` records the build host's name, so two machines' rpms differ by that and by nothing else. The files are `/usr/bin/fennec`, `/usr/bin/fennec-server` and the licence where each distribution looks. Each architecture is packed on a runner of its own (with `contents: read` and no stored credentials), and `tools/linux-packages-check.sh` installs the packages in `debian:12-slim` and `fedora:42` of that architecture, asserts their metadata and that they depend on nothing -- every assertion printing what it found and what it wanted -- runs both binaries, removes the package and checks the files are gone, before the artifact is kept: a package that does not install stops the release. CI's `packaging` job does the same with the latest release's tarball and again with it named as a pre-release. The job's checkout is the workflow's own commit and not a dispatched tag, which may be from before the packaging files. There is no apt or yum repository, only files on the release.
