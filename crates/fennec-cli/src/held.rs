//! `<file>.lock`, taken before the shell or a subcommand opens or writes a
//! database file, as `fennec-server --file`, `fennec mcp --file` and the
//! native library take it (`fennec_abi::lock`): two writers over one mapped
//! file corrupt it. Taken by nothing, a shell beside a server appended its
//! write where the server appended its own next one, and the server's
//! records written over the shell's left the rest of the shell's body
//! behind them: "1 records", exit 0, and the next open refused the file
//! (`corrupt: unknown record kind`). A tool that only looks -- `fennec
//! types`, `--read-only` -- opens with `fs::open_read_only` and takes none.

pub use fennec_abi::lock::FileLock;
use fennec_abi::lock::{self, Holder, Refused};

/// What takes the lock, for what a refusal tells it to do instead. A way a
/// feature takes is there only with it: a build without the feature warned
/// that the way was never constructed (`make small`, `backup` alone).
#[derive(Clone, Copy)]
pub enum Way {
    /// The shell over the file: `fennec <file>`, `-c`, a script.
    Shell,
    /// `fennec mcp --file`.
    #[cfg(feature = "mcp")]
    Mcp,
    /// `fennec import` into the file.
    #[cfg(feature = "import")]
    Import,
    /// `fennec backup` or `fennec restore` into the file.
    #[cfg(feature = "backup")]
    Copy,
    /// `.save <path>`.
    Save,
}

/// `<path>.lock`, held for as long as the guard lives; refused, the text
/// to say: who holds the file, and what to do instead.
pub fn take(path: &str, way: Way) -> Result<FileLock, String> {
    match lock::hold(path) {
        Ok(l) => Ok(l),
        Err(Refused::Held) => Err(match lock::holder(path) {
            // `.save` over the session's own file: nothing else here opens
            // a second file.
            Some(h) if h.pid == Some(std::process::id()) => format!(
                "{path} is open in this process already\n  {}",
                match way {
                    Way::Save => "save to another path",
                    _ => "a file is opened once at a time",
                }
            ),
            // Let go since, or a holder the system does not name: refused
            // all the same, not tried again, which a race could win.
            h => format!(
                "{path} is open in another process{}, and two writers over one file \
                 corrupt it\n  {}",
                h.as_ref()
                    .filter(|h| h.is_known())
                    .map_or(String::new(), |h| format!(" ({h})")),
                instead(path, way, h.as_ref())
            ),
        }),
        // A directory the user may only read, a read-only file system,
        // another user's lock file: a read there opened before the lock
        // was taken, and opens still without it.
        // The lock file named as it is, past a link to the file.
        Err(Refused::Io(e)) => Err(match (way, lock::lock_path(path).display()) {
            (Way::Shell, lock) => format!(
                "could not take {lock}: {e}\n  `fennec --read-only {path}` reads it without a lock"
            ),
            #[cfg(feature = "mcp")]
            (Way::Mcp, lock) => {
                format!("could not take {lock}: {e}\n  --read-only reads it without a lock")
            }
            (_, lock) => format!("could not take {lock}: {e}"),
        }),
    }
}

/// What to do instead of writing a file `holder` has open: what the holder
/// is decides the first half -- a server is asked over HTTP, a session or
/// an app is quit -- and the way in the second.
fn instead(path: &str, way: Way, holder: Option<&Holder>) -> String {
    let program = holder.and_then(|h| h.program.as_deref());
    let first = match (way, program) {
        (Way::Shell, Some("fennec-server")) => "query it over HTTP, or stop the server first",
        #[cfg(feature = "mcp")]
        (Way::Mcp, Some("fennec-server")) => {
            "reach it through the server (--url http://<its address>/_mcp), or stop the server first"
        }
        (_, Some("fennec-server")) => "stop the server first",
        (_, Some("fennec")) => "quit that fennec session first",
        // An app through the native library, or a holder the system does
        // not name.
        _ => "close it where it is open first",
    };
    let then = match way {
        Way::Shell => format!("; `fennec --read-only {path}` reads it as it stands"),
        #[cfg(feature = "mcp")]
        Way::Mcp => "; --read-only reads it as it stands".into(),
        #[cfg(feature = "import")]
        Way::Import => ", or import into another file (--file <path>)".into(),
        #[cfg(feature = "backup")]
        Way::Copy => ", or write another file".into(),
        Way::Save => ", or save to another path".into(),
    };
    format!("{first}{then}")
}
