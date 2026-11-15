//! The codes `fennec-server` exits with, each with what it means: a frozen
//! surface (docs/design/compatibility.md), dumped with the flags into
//! `integrations/api-golden/cli-fennec-server.json` by `tests/api_golden.rs`.
//! Every exit is a [`Code`] handed to [`now`], never a number: the codes
//! were literals at a dozen exits, and the test now finds every one here
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
                "stopped as asked (a signal, or stdin's end under --exit-on-stdin-close) after the \
                 final sync; --help, --version or --mint-token printed; --ping: a server answered"
            }
            Code::Failed => {
                "a failure: the file could not be opened or its history settled, a file's server \
                 could not have its HTTP port, or what runs beside the database stopped it (the \
                 listener, --follow's mirror, the scheduler); --ping: no server answered"
            }
            Code::Refused => {
                "refused at the start: a flag, its value or what they configure, a --user it could \
                 not become, a node's directory or port, or a start that failed before it served"
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
