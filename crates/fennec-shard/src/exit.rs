//! The codes `fennec-shard` exits with, each with what it means: a frozen
//! surface (docs/design/compatibility.md), dumped with the flags into
//! `integrations/api-golden/cli-fennec-shard.json` by `tests/api_golden.rs`.
//! Every exit is a [`Code`] handed to [`now`], never a number: the codes
//! were literals at its three exits, and the test now finds every one here
//! (it refuses an exit anywhere else in `src/`). A new code is a variant
//! with its meaning, which only grows the dump; a meaning reworded is a
//! break the dump's check names.

/// What the process ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Ok = 0,
    Failed = 1,
    Refused = 2,
}

impl Code {
    /// Every code, as the dump lists them.
    pub const ALL: [Code; 3] = [Code::Ok, Code::Failed, Code::Refused];

    /// What the code means, as the dump holds it.
    pub fn means(self) -> &'static str {
        match self {
            Code::Ok => {
                "--help or --version printed; a router serving runs until it is stopped, and a \
                 signal ends it by the system's default, with no code of its own"
            }
            Code::Failed => {
                "the directory or its global copy is another process's (its lock is held), or the \
                 listener failed while serving"
            }
            Code::Refused => {
                "refused at the start: a flag, its value or what they configure, the directory or \
                 its global copy, or its lock file, that could not be opened, made or read, the \
                 address that could not be had, or a thread the start could not make"
            }
        }
    }

    /// The code `n` is, where it is one.
    pub fn of(n: i32) -> Option<Code> {
        Code::ALL.into_iter().find(|c| *c as i32 == n)
    }
}

/// Ends the process with `code`: the one place the binary exits.
pub fn now(code: Code) -> ! {
    std::process::exit(code as i32)
}
