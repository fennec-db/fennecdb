//! # fennec-import
//!
//! Builds a fennecdb collection from an existing database: from a SQLite file
//! or from a live PostgreSQL server.
//!
//! Both readers feed through a single interface ([`Source`]), so the load
//! pipeline ([`load`]) and the plan output ([`map`]) are source independent.
//!
//! The load order is the one fennecdb recommends: bulk write first, index build
//! second. Updating the HNSW graph on every insert does the same work far
//! more expensively (see `fennec_core::query::Statement::CreateIndex`).
//!
//! ```no_run
//! use fennec_import::{load, Options};
//! let mut src = fennec_import::sqlite::Reader::open("data.sqlite", "docs")?;
//! let mut db = fennec_core::fs::open("data.fennec")?;
//! let summary = load::run(&mut src, &mut db, &Options::new("articles"))?;
//! println!("{} rows", summary.rows);
//! # Ok::<(), fennec_core::error::Error>(())
//! ```

pub mod args;
pub mod follow;
pub mod load;
pub mod map;
pub mod pg;
pub mod sqlite;

/// The unit tests' bytes counted a thread at a time, as the integration
/// tests' are: `--follow`'s stand-in streams are held to a bound by what
/// the follower allocates (`follow`'s tests).
#[cfg(test)]
#[path = "../tests/held.rs"]
mod held;
#[cfg(test)]
#[global_allocator]
static COUNTING: held::Counting = held::Counting;

use fennec_core::error::Result;
use fennec_core::query::Expr;
use fennec_core::schema::IndexKind;
use fennec_core::value::{DataType, Value};
use std::time::Duration;

/// A single column of the source table and its fennecdb counterpart.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// The column name in the source.
    pub name: String,
    /// The inferred fennecdb type. `None` means there is no safe counterpart
    /// (`numeric`) and `--cast` becomes mandatory.
    pub ty: Option<DataType>,
    /// The source type, verbatim: `INTEGER`, `numeric`, `vector(384)`.
    /// Only for the plan output and error text.
    pub source_type: String,
    /// A lossy-mapping warning, or the reason there is no `ty`.
    pub note: Option<String>,
    /// The source compares it without case -- PostgreSQL's `citext`,
    /// SQLite's `COLLATE NOCASE` -- and so does the field, `nocase`, while
    /// it is text.
    pub nocase: bool,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: DataType, source_type: impl Into<String>) -> Column {
        Column {
            name: name.into(),
            ty: Some(ty),
            source_type: source_type.into(),
            note: None,
            nocase: false,
        }
    }

    /// A column with no safe counterpart in fennecdb. Without `--cast` it
    /// errors at the plan stage -- stopping beats silently losing a type.
    pub fn unsupported(
        name: impl Into<String>,
        source_type: impl Into<String>,
        reason: impl Into<String>,
    ) -> Column {
        Column {
            name: name.into(),
            ty: None,
            source_type: source_type.into(),
            note: Some(reason.into()),
            nocase: false,
        }
    }

    /// Compared without case in the source ([`Column::nocase`]).
    pub fn nocase(mut self) -> Column {
        self.nocase = true;
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Column {
        self.note = Some(note.into());
        self
    }
}

/// A source that streams rows.
///
/// Every call to `next_row` returns one row in source column order; `None`
/// signals the end of the stream. Value types must be produced to match
/// [`Column::ty`]: `Value::coerce` does not go from text to number or from
/// integer to bool, and those conversions are the reader's job.
pub trait Source {
    /// The column list. Called before row reading starts.
    fn columns(&mut self) -> Result<Vec<Column>>;

    /// The next row, or `None` when the stream is done.
    fn next_row(&mut self) -> Result<Option<Vec<Value>>>;

    /// The known row count; `None` when unknown. Used only for progress
    /// output, so its accuracy does not affect the load.
    fn row_count(&self) -> Option<u64> {
        None
    }

    /// What the source has that its rows do not carry, a line each, which
    /// the import's summary names beside its warnings: a SQLite table's
    /// VIRTUAL generated columns, which SQLite computes as a row is read and
    /// never stores.
    fn left_out(&self) -> Vec<String> {
        Vec::new()
    }

    /// [`Source::next_row`], and the bytes the row holds past its values'
    /// own `size_of`, never fewer but for the allocator's rounding: what
    /// `load::run` holds a batch to ([`BATCH_BYTES`]). The default counts
    /// the values one by one; a source that knows it from what it read
    /// says so at a row's cost -- the SQLite reader its record's length and
    /// the defaults of the columns the record lacks. The count comes with
    /// its row, so no source keeps a row's count between two calls.
    fn next_counted(&mut self) -> Result<Option<(Vec<Value>, usize)>> {
        Ok(self.next_row()?.map(|row| {
            let n = load::values_bytes(&row);
            (row, n)
        }))
    }
}

/// Where the document's `id` comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IdSource {
    /// Use an `id` column when it is a positive integer, otherwise let fennecdb assign one.
    #[default]
    Auto,
    /// Use this column as the `id`.
    Column(String),
    /// Whatever the source `id` is, let fennecdb assign its own.
    Generated,
}

/// Import options. A one-to-one match for the CLI flags.
#[derive(Debug, Clone)]
pub struct Options {
    /// Target collection name.
    pub into: String,
    /// `--vector <field>:<N>` -- take a BLOB/array column as `vector<N>`.
    pub vectors: Vec<(String, usize)>,
    /// `--cast <field>=<type>` -- override the inferred type.
    pub casts: Vec<(String, DataType)>,
    /// `--index <field>@hnsw(..)` -- built once the load finishes.
    pub indexes: Vec<(String, IndexKind)>,
    /// `--index owner,created desc@sorted` -- an index over several fields,
    /// built once the load finishes too.
    pub composites: Vec<fennec_core::schema::Composite>,
    /// `--id <column>|none`
    pub id: IdSource,
    /// `--where <expr>` -- a FennecQL filter applied before the row is written.
    /// Field names are the names in the *target* schema (after renaming).
    pub filter: Option<Expr>,
    /// `--limit <N>` -- read at most this many rows.
    pub limit: Option<u64>,
    /// `--batch <N>` -- documents per `put` statement, at most: a batch is
    /// put sooner once its rows hold [`BATCH_BYTES`].
    pub batch: usize,
}

/// The `put` batch size. The value fennec-bench uses in its own measurements
/// (`crates/fennec-bench/src/main.rs`): a larger batch does not lower the
/// per-document cost, while peak memory grows linearly.
pub const DEFAULT_BATCH: usize = 2_000;

/// The most a batch holds of the rows it puts, in bytes, but the row that
/// reached it: a batch is put once it holds `--batch` rows or this many
/// bytes, whichever comes first (`load::run`), a row counted once, from
/// sizes known before its values are looked at -- its fields' pairs and
/// names once a plan, what its source read ([`Source::next_counted`], a
/// SQLite row its record's length and the defaults it lacks), and a number
/// its field makes text, as it is made. A row holds a value for each column
/// its source declares and a SQLite row a copy of each default its record
/// lacks, whatever its bytes in the source: rows bounded by their count
/// alone, 2 000 rows of three bytes each under a table of 87 376 columns
/// held some 12.6 GB of a 384 KiB file (G14 F2b's review). 16 MiB is the
/// most the engine holds of an open block before it spills it into the
/// file (`SPILL_AT`, 16 MB): a batch is one `put`, one block, which the
/// engine holds again as its documents, and as their frames twice until
/// the block lands -- in its record and in the store, each less than the
/// batch -- beside the frames the store holds since its file was opened,
/// up to `HANDOVER_AT` (16 MB). So an import holds about five times this
/// and its largest row at most, whatever its source declares. A SQLite row
/// is at most about twice the file's length as it is read: its payload,
/// no longer than the file, and its values -- the record's texts and
/// blobs, and the defaults it lacks, no longer than the `CREATE TABLE`
/// text -- more where a text is not UTF-8, each such byte U+FFFD's three.
/// Rows under 8 KiB each -- 2 000 of them -- never reach it, so `--batch`
/// alone decides for them, as before.
pub const BATCH_BYTES: usize = 16 << 20;

impl Options {
    pub fn new(into: impl Into<String>) -> Options {
        Options {
            into: into.into(),
            vectors: Vec::new(),
            casts: Vec::new(),
            indexes: Vec::new(),
            composites: Vec::new(),
            id: IdSource::Auto,
            filter: None,
            limit: None,
            batch: DEFAULT_BATCH,
        }
    }
}

/// The result of a load.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// Number of documents written.
    pub rows: u64,
    /// Number of rows dropped by `--where`.
    pub skipped: u64,
    pub elapsed: Duration,
    /// Warnings produced at the plan stage (lossy mapping, renaming).
    pub warnings: Vec<String>,
}

/// A source that reads rows from memory. For tests and for experiments
/// outside `--dry-run`; the real readers live in the [`sqlite`] and [`pg`]
/// modules.
pub struct Rows {
    columns: Vec<Column>,
    rows: std::vec::IntoIter<Vec<Value>>,
    total: u64,
}

impl Rows {
    pub fn new(columns: Vec<Column>, rows: Vec<Vec<Value>>) -> Rows {
        let total = rows.len() as u64;
        Rows {
            columns,
            rows: rows.into_iter(),
            total,
        }
    }
}

impl Source for Rows {
    fn columns(&mut self) -> Result<Vec<Column>> {
        Ok(self.columns.clone())
    }
    fn next_row(&mut self) -> Result<Option<Vec<Value>>> {
        Ok(self.rows.next())
    }
    fn row_count(&self) -> Option<u64> {
        Some(self.total)
    }
}
