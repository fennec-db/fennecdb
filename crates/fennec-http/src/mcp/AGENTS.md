# AGENTS.md

fennec-http's MCP server -- an agent's tools, each a statement -- for code in `crates/fennec-http/src/mcp/`; the rest of fennec-http's rules are in [its AGENTS.md](../../AGENTS.md), the rules every change meets in the root's [AGENTS.md](../../../../AGENTS.md). It is written from CLAUDE.md by `make agents-md`: edit that one.

## Invariants

### The HTTP server

**[An agent's MCP tool is a statement, held to the token as `/query`
is.](../../../../docs/design/http.md#mcp)** A model's filter goes in parsed, never as
text, and a read past `--mcp-max-rows` or `--mcp-max-bytes` is refused,
never cut. The origin is asked before the token, and with no token only a
`Host` naming localhost or an address is answered; `write` never changes a
schema or compacts. A named statement the token may call is a `call_<name>`
tool, its answer held to the bounds before a write's block lands
(`mcp::land`).
