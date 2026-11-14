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
in an allowlisted environment (never argv; names compared case-blind on
Windows), its stdin a pipe the app holds (`--exit-on-stdin-close`: none
outlives the app, `kill -9` included); it is stopped by SIGTERM, on Windows
by the end of that stdin (SIGTERM is TerminateProcess there, no checkpoint),
a hard kill only for one not ended 30 s later (`process.ts`). On Windows and
Linux the last window closed quits the app as File, Quit does, its servers
stopped first, while nothing is opening; every window the app makes is
`counted` (`onLastWindowClosed`, `checks/lastwindow.mjs`); macOS's stays. On
Windows each window's page is given the keyboard focus once it loads, at
each activation and when it comes forward from behind (a watch while it is
behind), only from a window already in front and never by bringing one
forward (`focusesPage`, `keysToPage`); `checks/menu.mjs` types into the
main window as it opened and clicks into its page as a person would, never
activating a window or dispatching keys in the page there. Only
`session.ts` gives a page a token, a frozen `window.fennecHost` defined
where `location.origin` is the server's alone. A studio window is
`sandbox: true` (the event bridge only) and held to its origin, one with
the RPC bridge to its `views://` folder (`holdTo`); on Windows, where
Electrobun leaves a page's request for a new window to WebView2 (a window of
its own, held to nothing), a preload in every document of a window, every
frame's, takes each way a page names one first -- `window.open`,
`document.open` with three arguments, and a link or form whose own,
submitter's or `<base>` target is another window, by a click, `click()`,
`dispatchEvent`, `submit()` or `requestSubmit()` -- a studio window's to the
same filter (`newWindowPreload`, `checks/hostile.mjs`), and a page that loaded
before its preload is loaded again (`guardPreload`, `checks/latepreload.mjs`).
There WebView2 runs with Electrobun's `--disable-web-security`, which no
variable takes away (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` only adds, and
an elevated app's is ignored): `hostile` holds a page to its origin's reads
on macOS and Linux, and on Windows fails once the switch is gone, so the
reads are held there too, and `sandbox` holds a studio window's to its own
origin by its CSP on every system; no `WEBVIEW2_*` variable the app was
started with reaches it, each taken out of the process's environment block
(`webview2.ts`, `win32.ts`), never by `process.env` alone, whose delete did
not reach WebView2. On Linux a studio window's first load that WebKitGTK
ended with nothing committed (a cancel, which Electrobun reports as a load
finished) is loaded again on that event, never a timer, at most three times
(`loadAgainIfDropped`, `checks/firstload.mjs`). No path or link the app hands the system goes through a
shell: on Linux `xdg-open` with an argument vector, never Electrobun's `openExternal` or `showItemInFolder`
(`opener.ts`, `checks/opener.mjs`). One instance a user listens where only
that user can (`instance.ts`). A `.fennec` double-clicked on Windows or Linux
is read back from the launcher, the app's parent, which drops its arguments,
only where that parent is the launcher beside the app's Bun (`handover.ts`),
each argument a path opened as Open… opens one, never run or read as a command;
a second instance hands its paths to the first over that channel. The app
registers itself, for its user, at every start of the one instance and only
what differs, never over another program's default, an entry whose launcher
is gone ignored on Linux (`TryExec`), and what an uninstall leaves, every
key and file, is the README's and the studio page's (`associate.ts`,
`checks/finder.mjs`, its control the registration taken away). A saved server's token is in the system's
credential store alone with its address (`token.ts`), never sent elsewhere
or along a redirect: on Windows kept on the machine (`win32.ts`), on Linux
refused in words where no Secret Service with a default collection answers
within a bound, a "/" asked again on its connection, which gnome-keyring may
not know yet (`secretservice.ts`, `checks/keyring.mjs`) -- never a file,
never taken for "no token"; a failure is a notice window, never a native
alert. Each window has its own data store, a removed connection's
cleared after its window closes by `STORE_KINDS`, never
`clearStorageData("all")`; on Windows, where Electrobun's clear empties
another window's, by removing its WebView2 folder once no window uses it, a
file window's too (`stores.ts`, `checks/storage.mjs`). The shipped bundle holds
no test hook, such as an environment read outside `__TESTABLE__`
(`checks/release.mjs`). Each check starts with none of the test variant's
state, its `state.json`, credential store items and web stores (`freshState`),
and on Windows with no picker of the shell's dimming the screen, one a check
left failing it (`clearDimming`), so one that fails half-way fails alone; a failed one keeps every fact the app
reported, each window's load step by step, and the system log of the app and
of WebKit's processes (`keepEvidence`), the main record of a page that never
loads; a command the app could not carry out fails its check at once with the
app's words (`cmd-failed`). The toolchain is fetched into `.hutch-home/`,
never `~/.hutch` or a shell file, and held to `hutch-pin.json`, a pin of
every system it builds on, Hutch and what Hutch fetches, a system without
one refused (`scripts/electrobun.mjs`); signing runs by hand from `main` alone, in an
Environment given its reviewer and `main` rule before its secrets
(`desktop.yml`). The DMG a release ships is the app unpacked, never
Electrobun's self-extractor (a quarantined copy must run read-only), each
Mach-O and then the bundle signed ad hoc (`scripts/dist.sh`), held as it
ships by `checks/dist.mjs`: its signature, no hook, a launch and quit from
the read-only image and from a copy left unchanged. Windows' zip and Linux's
tarball, x64 and arm64, are the payload unpacked too, one folder, never
Electrobun's installer (`scripts/dist.sh`), held by `checks/dist-archive.mjs`:
no hook, started where it is unpacked and where `install.ps1 -App` or
`install.sh --app` put it from a release laid out on the runner, unchanged by
either run but for the launcher's own log (`bin\app.log`, on Windows). A release's downloads are attached only from `main`, the DMG
where the signing job ran and signed nothing, by a job with no secret
(`scripts/attach.sh`): their lines are appended to `SHA256SUMS` in one write,
which only grows, and an asset there is never replaced.

**[Grafana's data source binds a dashboard's values; it never splices
them.](../docs/design/integrations.md#grafana)** Names become `$n`s
(`fennecql.go`); only `get`, `select` and `explain` go out.
