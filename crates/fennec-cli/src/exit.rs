//! The codes `fennec` exits with, each with what it means: a frozen surface
//! (docs/design/compatibility.md), dumped with every command's flags into
//! `integrations/api-golden/cli-fennec.json` by `tests/api_golden.rs`, which
//! takes this file in by its path (the crate has no library). Every exit
//! is a [`Code`] handed to [`now`] or returned by a subcommand, never a
//! number: the codes were literals at some forty places, and the test now
//! finds every one here (it refuses an exit anywhere else in `src/`). A new
//! code is a variant with its meaning, which only grows the dump; a meaning
//! reworded is a break the dump's check names.

/// What the process ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Ok = 0,
    Failed = 1,
    Refused = 2,
}

// Read by the test alone: the binary never asks a code what it means.
#[allow(dead_code)]
impl Code {
    /// Every code, as the dump lists them.
    pub const ALL: [Code; 3] = [Code::Ok, Code::Failed, Code::Refused];

    /// What the code means, as the dump holds it.
    pub fn means(self) -> &'static str {
        match self {
            Code::Ok => {
                "done: the session, every statement of -c, or the subcommand did what it was \
                 asked; --help or --version printed"
            }
            Code::Failed => {
                "failed: a statement of -c, the file's open or its sync, its lock another's, or \
                 the subcommand's work (an import, a backup, a restore, a verify finding what \
                 cannot be restored)"
            }
            Code::Refused => {
                "refused before anything ran: a flag, an argument or a value, a file it would \
                 write over without --force, or a subcommand this build was made without"
            }
        }
    }
}

/// Ends the process with `code`: the one place the binary exits.
pub fn now(code: Code) -> ! {
    std::process::exit(code as i32)
}
