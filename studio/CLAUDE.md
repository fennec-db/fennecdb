# CLAUDE.md

fennec studio's invariant, for code in `studio/` and the routes fennec-http serves it from. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../CLAUDE.md).

## Invariants

### The HTTP server

**[fennec studio is a page with no authority of its
own.](../docs/design/http.md#studio)** Its files are served before any token
under a strict CSP; it sends the pasted token as any client does. An app
embedding it hands over a frozen `window.fennecHost.session`, taken for the
page's own origin alone, never stored, with no sign-in or sign-out beside
it. Its statements are written in `studio/statements*.js`: values as
parameters, a name only as FennecQL writes one, the typed `where` inside
parentheses it cannot close; `transport.test.mjs` holds the in-page answers
to a server's.
