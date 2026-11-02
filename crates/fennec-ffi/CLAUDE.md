# CLAUDE.md

The native library's invariant, for code in `crates/fennec-ffi/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### The native library and sync

**[An app keeps its database through
`fennec-ffi`.](../../docs/design/native-and-sync.md#the-native-library)** A text
is answered through `fennec-abi` to the browser module's bytes. A handle
indexes a table of `Arc`s, never a pointer; every call returns a code under
`catch_unwind`, built in the unwinding `ffi` profile; a test holds the
hand-written `include/fennec.h` to the exports. As a server's, a long read
pins and the fsync and a buffered write go after the lock
(`Native::after`); JNI passes text as UTF-8 bytes; `<file>.lock` is an
`fcntl` record lock (Windows: `LockFileEx`), never `flock`.
