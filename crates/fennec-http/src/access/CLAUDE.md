# CLAUDE.md

fennec-http's field lists -- what a read rule withholds, held out of what a statement does (`fields.rs`) -- for code in `crates/fennec-http/src/access/`; the rest of fennec-http's rules, access's among them, are in [its CLAUDE.md](../../CLAUDE.md), the rules every change meets in the root's [CLAUDE.md](../../../../CLAUDE.md).

## Invariants

### Access

**[A field a read rule withholds is held out of what a statement
does.](../../../../docs/design/access.md#field-lists)** `fields::hold` and
`fields::write` guard or refuse (403) each use of it before the rules are
ANDed in; it is `null` in a row (`Scope::shown`), absent from every schema
answer (`Scope::shown_schema`) and refused by name as a missing one, and a
stream sends no row a write left showing the same (`sse::Seen`).
