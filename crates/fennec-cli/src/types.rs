//! `fennec types` -- generates a TypeScript declaration from the live schema.
//!
//! Drizzle/Kysely make you define the schema in TypeScript *again*; the two
//! sides drift apart over time. Here the single source is the file itself:
//! the declaration comes out of the schema and is regenerated when it changes.
//!
//! ```text
//! fennec types data.fennec > web/fennec-schema.d.ts
//! ```
//!
//! A file's named statements are written beside its collections, in every
//! language it writes (not `--schema`, not `--fennecql`): each statement's
//! arguments, from its signature, and its answer as far as it is typed --
//! a `get`'s `select` list or a write's `returning` over one collection,
//! each plain field typed as its collection's row types it, `id` and a
//! `count` integers, and every other item (computed, aggregated, renamed
//! with `as`, a path into a json field, a `lookup`'s rows) the language's
//! JSON value, until the engine infers the types of expressions. A body
//! whose last statement writes with no `returning` answers the rows it
//! wrote, a count: no row type, and the file says so. A broken statement of
//! a database is named in a comment and typed no further.

use fennec_core::prelude::*;
use fennec_core::schema::{Quant, TextIndexSpec, VectorIndexSpec};
use fennec_core::value::VecPrec;

const USAGE: &str = "\
usage: fennec types <file.fennec | schema.fennecql> [-o <out>] [--name <name>]
       fennec types --lang python|go|csharp|swift|kotlin|dart <file> [-o <out>] [--name <package>]
       fennec types --schema <file> [-o <schema.ts>] [--import <module>]
       fennec types --fennecql <file> [-o <schema.fennecql>]

  <file>               a database, or a schema written as FennecQL (.fennecql):
                       `create collection`, `create index` and `create
                       statement` statements
  -o, --out <path>     write to a file (default: stdout)
      --name <name>    the generated type (default: FennecSchema), the Go
                       package (fennecschema) or the C# namespace (FennecSchema)
      --lang <lang>    a row of each collection in Python (TypedDict), Go
                       (structs), C# (records), Swift (Codable structs), Kotlin
                       (data classes) or Dart (fromJson) rather than TypeScript
      --schema         the tables as code (@fennecdb/web/schema), for a project
                       that declares its schema from here on
      --import <from>  where --schema's file imports the builders from
                       (default: @fennecdb/web/schema)
      --fennecql       the schema as FennecQL, which every SDK opens a
                       database with (`schema`) and `fennec types` reads

  Every form but --schema and --fennecql writes the file's named statements
  too: each one's arguments and, where its answer is typed, a row of it
  (`FennecStatements` in TypeScript, `<Name>Args` and `<Name>Row` in the
  other languages).
";

pub fn main(args: &[String]) -> i32 {
    let mut path: Option<&str> = None;
    let mut out: Option<&str> = None;
    let mut name = "FennecSchema";
    let mut tables = false;
    let mut fennecql = false;
    let mut lang = None;
    let mut named = false;
    let mut import = "@fennecdb/web/schema";

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--schema" => tables = true,
            "--fennecql" => fennecql = true,
            "--lang" => {
                i += 1;
                match args.get(i).map(|l| (l, crate::langs::Lang::named(l))) {
                    Some((_, Some(l))) => lang = Some(l),
                    Some((l, None)) if l == "ts" || l == "typescript" => {}
                    _ => {
                        return fail("--lang expects ts, python, go, csharp, swift, kotlin or dart")
                    }
                }
            }
            "--import" => {
                i += 1;
                match args.get(i) {
                    Some(v) => import = v,
                    None => return fail("--import expects a module"),
                }
            }
            "-o" | "--out" => {
                i += 1;
                match args.get(i) {
                    Some(v) => out = Some(v),
                    None => return fail("-o expects a path"),
                }
            }
            "--name" => {
                i += 1;
                match args.get(i) {
                    Some(v) => {
                        name = v;
                        named = true;
                    }
                    None => return fail("--name expects a name"),
                }
            }
            other if !other.starts_with('-') => path = Some(other),
            other => return fail(&format!("unknown option: {other}")),
        }
        i += 1;
    }

    let Some(path) = path else {
        eprintln!("{USAGE}");
        return 2;
    };

    let file = match read(path) {
        Ok(f) => f,
        Err(e) => return fail(&e),
    };
    let schemas = &file.schemas;
    let src = match (tables, fennecql, lang) {
        (true, false, None) => render_tables(path, import, schemas),
        (false, true, None) => render_fennecql(path, schemas),
        (false, false, Some(lang)) => {
            let default = match lang {
                crate::langs::Lang::Go | crate::langs::Lang::Kotlin => "fennecschema",
                _ => "FennecSchema",
            };
            crate::langs::render(lang, path, if named { name } else { default }, &file)
        }
        (false, false, None) => render(path, name, &file),
        _ => return fail("one of --schema, --fennecql and --lang"),
    };
    match out {
        Some(p) => match std::fs::write(p, &src) {
            Ok(()) => match file.statements.len() {
                n if n > 0 && !tables && !fennecql => {
                    eprintln!("{p}  {} collections, {n} statements", schemas.len())
                }
                _ => eprintln!("{p}  {} collections", schemas.len()),
            },
            Err(e) => return fail(&format!("could not write {p}: {e}")),
        },
        None => print!("{src}"),
    }
    0
}

/// What `fennec types` reads of a file: its collections, and its named
/// statements by name, each as the generators write it.
pub struct File {
    pub schemas: Vec<Schema>,
    pub statements: Vec<Named>,
}

/// A named statement as every generator writes it.
pub struct Named {
    pub name: String,
    /// `comment '<text>'`: what the generated types say of it.
    pub comment: Option<String>,
    /// Its parameters in the signature's order: one with a default may be
    /// left out or given as `null`, the others neither. Its claims are the
    /// token's, or the embedder's `as`, and never an argument.
    pub args: Vec<Member>,
    /// The token's claims its body reads, each `` `name` type``: what a
    /// caller's token, or an embedder's `as`, must hold.
    pub claims: Vec<String>,
    pub answer: Answer,
}

/// What a call of a statement answers, as far as it is typed.
pub enum Answer {
    /// Rows, each with these members, in the order the answer writes them.
    Rows(Vec<Member>),
    /// The number of rows its last statement wrote: a write with no
    /// `returning`.
    Wrote,
    /// Rows no generator types yet, and why.
    Untyped(String),
    /// A database's statement that cannot be called, and why: named in a
    /// comment and typed no further.
    Broken(String),
}

/// A member of a generated type: an argument, or an item of an answer's
/// row. One not `required` reads `null`, or may be left out or given as
/// `null`; one whose type is not known is `json`, the language's JSON value.
pub struct Member {
    pub name: String,
    pub ty: DataType,
    pub required: bool,
    /// The comment beside it: the schema's line, a parameter's type and
    /// default, or why it is not typed; empty for `id`, which needs none.
    pub note: String,
}

/// The collections a database holds and its named statements, or what a
/// `.fennecql` file declares; the statements by name.
pub fn read(path: &str) -> std::result::Result<File, String> {
    let (schemas, mut statements) = if path.ends_with(".fennecql") {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
        let (schemas, defs) =
            fennec_ql::schema_statements(&text).map_err(|e| format!("{path}: {e}"))?;
        let named: Vec<Named> = defs.iter().map(|d| typed(d, &schemas)).collect();
        (schemas, named)
    } else {
        // The open reads each stored statement's text back into its tree
        // with the reader installed: the shell's `main` installs it, and a
        // test reading a file may not have.
        fennec_ql::install();
        // Read only: the file is often a running server's, and an open that
        // may cut or create would write to it.
        let db = fennec_core::fs::open_read_only(path)
            .map_err(|e| format!("could not open {path}: {e}"))?;
        let mut schemas = Vec::new();
        for n in db.collection_names() {
            let c = db.collection(&n).map_err(|e| format!("{n}: {e}"))?;
            schemas.push(fennec_core::schema::Schema::clone(&c.schema));
        }
        let mut named = Vec::new();
        for s in db.statements().iter() {
            named.push(match (&s.broken, &s.tree) {
                (None, Some(def)) => typed(def, &schemas),
                (why, _) => Named {
                    name: s.name.clone(),
                    comment: None,
                    args: Vec::new(),
                    claims: Vec::new(),
                    answer: Answer::Broken(match why {
                        Some(why) => why.clone(),
                        None => "its text was not read".into(),
                    }),
                },
            });
        }
        (schemas, named)
    };
    statements.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(File {
        schemas,
        statements,
    })
}

/// A definition's arguments and answer, typed against the collections it
/// was defined over.
fn typed(def: &fennec_core::named::Definition, schemas: &[Schema]) -> Named {
    let args = def
        .params
        .iter()
        .map(|p| {
            let mut note = p.ty.to_string();
            if let Some(d) = &p.default {
                note.push_str(" = ");
                fennec_core::json::value_into(&mut note, d);
            }
            Member {
                name: p.name.clone(),
                ty: p.ty.clone(),
                required: p.default.is_none(),
                note,
            }
        })
        .collect();
    Named {
        name: def.name.clone(),
        comment: def.comment.clone(),
        args,
        claims: def
            .claims
            .iter()
            .map(|c| format!("`{}` {}", c.claim(), c.ty))
            .collect(),
        answer: answer_of(def, schemas),
    }
}

/// What the last statement of a body answers, typed as far as it is
/// (`Definition::answer`, which MCP's `outputSchema` reads too): a plain
/// field as its collection's row types it, `id` and a count as integers,
/// and anything else as JSON, since the engine infers no expression's type
/// yet.
fn answer_of(def: &fennec_core::named::Definition, schemas: &[Schema]) -> Answer {
    use fennec_core::named::{self, What};
    match def.answer(|c| schemas.iter().find(|s| s.name == c)) {
        named::Answer::Rows(items) => Answer::Rows(
            items
                .into_iter()
                .map(|i| Member {
                    note: match &i.what {
                        What::Id => String::new(),
                        What::Count => "a count".into(),
                        What::Field(f) => crate::langs::note(f),
                        What::Untyped(why) => format!("not typed: {why}"),
                    },
                    name: i.name,
                    ty: i.ty,
                    required: i.required,
                })
                .collect(),
        ),
        named::Answer::Wrote => Answer::Wrote,
        named::Answer::Untyped(why) => Answer::Untyped(why),
    }
}

/// A comment's text on one line, for a comment beside a type.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What a generated file says of a statement it writes no row type for.
pub fn no_row(n: &Named) -> Option<String> {
    match &n.answer {
        Answer::Rows(_) => None,
        Answer::Wrote => Some(format!(
            "`{}` answers the number of rows it wrote: no row type.",
            n.name
        )),
        Answer::Untyped(why) => Some(format!(
            "`{}` answers rows not typed yet, {}: no row type.",
            n.name,
            one_line(why)
        )),
        Answer::Broken(why) => Some(format!(
            "statement `{}` is broken, and no type is written for it: {}",
            n.name,
            one_line(why)
        )),
    }
}

/// The summary of a statement's arguments type, its comment after it, and
/// the claims it reads, which are no argument.
pub fn args_doc(n: &Named) -> String {
    let mut doc = match &n.comment {
        Some(c) => format!("The arguments of `{}`: {}", n.name, one_line(c)),
        None => format!("The arguments of `{}`.", n.name),
    };
    if let Some(c) = claims_doc(n) {
        doc.push(' ');
        doc.push_str(&c);
    }
    doc
}

/// What a statement's types say of the claims it reads.
pub fn claims_doc(n: &Named) -> Option<String> {
    match n.claims.is_empty() {
        true => None,
        false => Some(format!(
            "The claims it reads come from the token, never the arguments: {}.",
            n.claims.join(", ")
        )),
    }
}

/// The summary of a statement's row type.
pub fn row_doc(n: &Named) -> String {
    format!("A row of what `{}` answers.", n.name)
}

/// `fennec types --fennecql`: the schema as FennecQL, every option written --
/// what an SDK opens its database with, and `fennec types` reads.
fn render_fennecql(path: &str, schemas: &[Schema]) -> String {
    format!(
        "-- generated from the fennecdb schema: {path}\n\
         -- fennec types --fennecql {path} > schema.fennecql\n\
         -- The schema as FennecQL: an open with it (`schema`) checks the database\n\
         -- against it, and `fennec types schema.fennecql` writes its types.\n\n{}",
        fennec_core::declared::fennecql(schemas)
    )
}

fn fail(msg: &str) -> i32 {
    eprintln!("{msg}");
    2
}

/// Produces the `.d.ts` body from the schema and its named statements.
fn render(path: &str, name: &str, file: &File) -> String {
    let schemas = &file.schemas;
    let mut out = String::new();
    out.push_str(&format!(
        "// generated from the fennecdb schema: {path}\n\
         // fennec types {path} > fennec-schema.d.ts\n\
         // Do not edit by hand -- regenerate when the schema changes.\n\n"
    ));

    // Only the branded types that are actually used are written: by a
    // field, an argument or an item of an answer.
    let members = file.statements.iter().flat_map(|n| {
        let rows = match &n.answer {
            Answer::Rows(r) => &r[..],
            _ => &[],
        };
        n.args.iter().chain(rows).map(|m| &m.ty)
    });
    let types: Vec<&DataType> = schemas
        .iter()
        .flat_map(|s| s.fields.iter().map(|f| &f.ty))
        .chain(members)
        .collect();
    let used = |f: fn(&DataType) -> bool| types.iter().any(|t| f(t));
    // The brands are structural: they are identical to the definitions in
    // `web/fennec.d.ts`, so values pass freely between the two with no import.
    if used(|t| {
        matches!(t, DataType::Timestamp)
            || matches!(t, DataType::List(i) if **i == DataType::Timestamp)
    }) {
        out.push_str(
            "/** `timestamp`: ISO-8601 text when read; a Date/number is accepted when writing. */\n\
             export type Timestamp = string & { readonly __fennec: 'timestamp' };\n",
        );
    }
    if used(|t| matches!(t, DataType::Sparse(_))) {
        out.push_str(
            "/** `sparse<N>`: pgvector's text form, `{1:0.5,3:0.25}/N`, indices from 1. */\n\
             export type Sparse = string & { readonly __fennec: 'sparse' };\n",
        );
    }
    if used(|t| matches!(t, DataType::Vector(..))) {
        out.push_str(
            "/** `vector<N>`: an array of numbers in JSON. */\n\
             export type Vector = number[] & { readonly __fennec: 'vector' };\n",
        );
    }
    if used(|t| {
        matches!(t, DataType::Geo) || matches!(t, DataType::List(i) if **i == DataType::Geo)
    }) {
        out.push_str(
            "/** `geo`: a point, its longitude and latitude in degrees. */\n\
             export type Point = [lon: number, lat: number];\n",
        );
    }
    if used(|t| matches!(t, DataType::Json)) {
        out.push_str(
            "/** `json`: any value JSON holds; a path reads into it, `'meta.lang'`. */\n\
             export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };\n",
        );
    }
    if used(|t| {
        matches!(t, DataType::Bytes) || matches!(t, DataType::List(i) if **i == DataType::Bytes)
    }) {
        out.push_str(
            "/** `bytes`: an array of bytes in JSON. */\n\
             export type Bytes = number[] & { readonly __fennec: 'bytes' };\n",
        );
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }

    if schemas.is_empty() {
        out.push_str(&format!(
            "// (there are no collections in the file)\nexport type {name} = {{}};\n"
        ));
        ts_statements(&mut out, &file.statements);
        return out;
    }

    out.push_str(&format!("export type {name} = {{\n"));
    for (i, s) in schemas.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("  {}: {{\n", key(&s.name)));
        for f in &s.fields {
            // The schema line is kept in a comment: the type name loses
            // information (`vector<768> @hnsw(cosine)` -> `Vector`).
            let mut note = f.ty.name();
            if let Some(ix) = index_note(&f.index) {
                note.push_str(&ix);
            }
            if f.required {
                note.push_str(" required");
            }
            out.push_str(&format!("    /** {note} */\n"));
            // `id` is automatic and added by `Row<F>`.
            // A field that is not required reads back as `null` when unwritten.
            let nullable = if f.required { "" } else { " | null" };
            out.push_str(&format!(
                "    {}: {}{nullable};\n",
                key(&f.name),
                ts_type(&f.ty)
            ));
        }
        out.push_str("  };\n");
    }
    out.push_str("};\n");
    ts_statements(&mut out, &file.statements);
    out
}

/// `FennecStatements`, after the schema: each named statement's `args`,
/// what a call takes, and `row`, a row of its answer -- `unknown` where the
/// answer is a count of the rows written or not typed. Written only where
/// the file holds a statement; a broken one is a comment before it.
fn ts_statements(out: &mut String, statements: &[Named]) {
    let (broken, live): (Vec<&Named>, Vec<&Named>) = statements
        .iter()
        .partition(|n| matches!(n.answer, Answer::Broken(_)));
    if !broken.is_empty() {
        out.push('\n');
        for why in broken.iter().filter_map(|n| no_row(n)) {
            out.push_str(&format!("// {why}\n"));
        }
    }
    if live.is_empty() {
        return;
    }
    let comment = |text: &str| one_line(text).replace("*/", "*\\/");
    out.push_str(
        "\n/**\n * The named statements: what a call of each takes (`args`, one with a\n \
         * default may be left out) and a row of what it answers (`row`, `unknown`\n \
         * where it answers the rows it wrote, or rows not typed yet).\n */\n\
         export type FennecStatements = {\n",
    );
    let member = |out: &mut String, m: &Member, optional: bool| {
        if !m.note.is_empty() {
            out.push_str(&format!("      /** {} */\n", comment(&m.note)));
        }
        let nullable = if m.required { "" } else { " | null" };
        let mark = if optional { "?" } else { "" };
        out.push_str(&format!(
            "      {}{mark}: {}{nullable};\n",
            key(&m.name),
            ts_type(&m.ty)
        ));
    };
    for (i, n) in live.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut doc = n.comment.as_deref().map(comment).unwrap_or_default();
        for more in [claims_doc(n), no_row(n)].into_iter().flatten() {
            if !doc.is_empty() {
                doc.push(' ');
            }
            doc.push_str(&comment(&more));
        }
        if !doc.is_empty() {
            out.push_str(&format!("  /** {doc} */\n"));
        }
        out.push_str(&format!("  {}: {{\n", key(&n.name)));
        match n.args.is_empty() {
            true => out.push_str("    args: {};\n"),
            false => {
                out.push_str("    args: {\n");
                for m in &n.args {
                    member(out, m, !m.required);
                }
                out.push_str("    };\n");
            }
        }
        match &n.answer {
            Answer::Rows(rows) => {
                out.push_str("    row: {\n");
                for m in rows {
                    member(out, m, false);
                }
                out.push_str("    };\n");
            }
            _ => out.push_str("    row: unknown;\n"),
        }
        out.push_str("  };\n");
    }
    out.push_str("};\n");
}

/// The builders `--schema`'s file may import: a table's variable takes
/// another name rather than shadow one.
const BUILDERS: [&str; 13] = [
    "fennecTable",
    "text",
    "integer",
    "doublePrecision",
    "boolean",
    "timestamp",
    "bytea",
    "json",
    "vector",
    "halfvec",
    "sparsevec",
    "index",
    "uniqueIndex",
];

/// Words a JavaScript `const` cannot be named.
const RESERVED: [&str; 38] = [
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "let",
    "yield",
];

/// `fennec types --schema`: the tables as code, Drizzle's way, which a
/// project that moves from reading its schema out of the database to
/// declaring it starts from -- `drizzle-kit pull`'s file. Opened with it,
/// the database holds what the code declares, to the last option: every
/// option that is not the engine's default is written.
fn render_tables(path: &str, import: &str, schemas: &[Schema]) -> String {
    let schemas: Vec<&Schema> = schemas
        .iter()
        .filter(|s| !s.name.starts_with('_'))
        .collect();
    let mut body = String::new();
    let mut used: Vec<&str> = vec!["fennecTable"];
    let mut names = Vec::new();
    for s in &schemas {
        let var = variable(&s.name, &names);
        names.push(var.clone());
        body.push_str(&format!(
            "\nexport const {var} = fennecTable('{}', {{\n",
            s.name
        ));
        let mut indexes = Vec::new();
        for f in &s.fields {
            let col = column(&f.ty, &mut used);
            let mut chain = col;
            if let Some(c) = f.collate {
                // Before `.array()`: the collation is the text's.
                chain = match chain.strip_suffix(".array()") {
                    Some(base) => format!("{base}.collate('{}').array()", c.name()),
                    None => format!("{chain}.collate('{}')", c.name()),
                };
            }
            if f.nocase {
                chain.push_str(".nocase()");
            }
            if f.required {
                chain.push_str(".notNull()");
            }
            if f.index == IndexKind::UNIQUE {
                chain.push_str(".unique()");
            } else if let Some(ix) =
                index_code(s, &f.name, &member(&f.name), &f.ty, &f.index, &mut used)
            {
                indexes.push(ix);
            }
            body.push_str(&format!("  {}: {chain},\n", key(&f.name)));
        }
        for p in &s.paths {
            let Some((field, keys)) = fennec_core::schema::split_path(&p.name) else {
                continue;
            };
            let target = format!("{}.path('{keys}')", member(field));
            if let Some(ix) = index_code(s, &p.name, &target, &p.ty, &p.index, &mut used) {
                indexes.push(ix);
            }
        }
        // An index over several fields, Drizzle's way: `.on(t.owner,
        // t.at.desc())`, named by its table and fields.
        for c in &s.composites {
            let b = if c.unique { "uniqueIndex" } else { "index" };
            if !used.contains(&b) {
                used.push(b);
            }
            let mut name = s.name.clone();
            let mut on = Vec::new();
            for (k, desc) in &c.keys {
                name.push('_');
                name.push_str(&k.replace('.', "_"));
                let target = match fennec_core::schema::split_path(k) {
                    Some((field, keys)) => format!("{}.path('{keys}')", member(field)),
                    None => member(k),
                };
                on.push(match desc {
                    true => format!("{target}.desc()"),
                    false => target,
                });
            }
            indexes.push(format!("{b}('{name}_idx').on({})", on.join(", ")));
        }
        match indexes.is_empty() {
            true => body.push_str("});\n"),
            false => {
                body.push_str("}, (t) => [\n");
                for ix in indexes {
                    body.push_str(&format!("  {ix},\n"));
                }
                body.push_str("]);\n");
            }
        }
    }
    let imports: Vec<&str> = BUILDERS
        .iter()
        .copied()
        .filter(|b| used.contains(b))
        .collect();
    let mut out = format!(
        "// generated from the fennecdb schema: {path}\n\
         // fennec types --schema {path} > schema.ts\n\
         // The tables are the code's from here on: change them here, and an open\n\
         // with them checks the database against them (Fennec.open's `schema`).\n\n\
         import {{ {} }} from '{import}';\n",
        imports.join(", ")
    );
    if schemas.is_empty() {
        out.push_str("\n// (there are no collections in the file)\n");
    }
    out.push_str(&body);
    out
}

/// A table's variable: its name where JavaScript takes it, else made so.
fn variable(name: &str, taken: &[String]) -> String {
    let ascii = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut v = match ascii {
        true => name.to_string(),
        false => name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect(),
    };
    if BUILDERS.contains(&v.as_str())
        || RESERVED.contains(&v.as_str())
        || v.starts_with(|c: char| c.is_ascii_digit())
    {
        v.push_str("Table");
    }
    while taken.contains(&v) {
        v.push('_');
    }
    v
}

/// A column of the callback's `t`: `t.title`, or `t['año']`.
fn member(name: &str) -> String {
    let k = key(name);
    match k.starts_with('\'') {
        true => format!("t[{k}]"),
        false => format!("t.{k}"),
    }
}

/// The builder a type is made by.
fn column(t: &DataType, used: &mut Vec<&'static str>) -> String {
    let (b, args): (&'static str, String) = match t {
        DataType::Bool => ("boolean", String::new()),
        DataType::Int => ("integer", String::new()),
        DataType::Float => ("doublePrecision", String::new()),
        DataType::Text => ("text", String::new()),
        DataType::Bytes => ("bytea", String::new()),
        DataType::Timestamp => ("timestamp", String::new()),
        DataType::Json => ("json", String::new()),
        DataType::Vector(n, VecPrec::F32) => ("vector", format!("{{ dimensions: {n} }}")),
        DataType::Vector(n, VecPrec::F16) => ("halfvec", format!("{{ dimensions: {n} }}")),
        DataType::Sparse(n) => ("sparsevec", format!("{{ dimensions: {n} }}")),
        DataType::Geo => ("geometry", "{ type: 'point' }".into()),
        DataType::List(inner) => return format!("{}.array()", column(inner, used)),
    };
    if !used.contains(&b) {
        used.push(b);
    }
    format!("{b}({args})")
}

/// An index as the callback declares it, named `<table>_<field>_idx` --
/// fennecdb keeps no name, Drizzle wants one.
fn index_code(
    s: &Schema,
    field: &str,
    target: &str,
    ty: &DataType,
    k: &IndexKind,
    used: &mut Vec<&'static str>,
) -> Option<String> {
    let name = format!("{}_{}_idx", s.name, field.replace('.', "_"));
    let mut use_ = |b: &'static str| {
        if !used.contains(&b) {
            used.push(b);
        }
    };
    Some(match k {
        IndexKind::None => return None,
        IndexKind::Hash { unique: true } => {
            use_("uniqueIndex");
            format!("uniqueIndex('{name}').on({target})")
        }
        IndexKind::Hash { unique: false } => {
            use_("index");
            format!("index('{name}').using('hash', {target})")
        }
        IndexKind::Sorted { ttl } => {
            use_("index");
            let mut s = format!("index('{name}').on({target})");
            if let Some(ms) = ttl {
                s.push_str(&format!(".ttl('{}')", fennec_core::schema::ttl_text(*ms)));
            }
            s
        }
        IndexKind::Inverted => {
            use_("index");
            format!("index('{name}').using('inverted', {target})")
        }
        // PostGIS's spatial index, as Drizzle declares one over a point.
        IndexKind::Geo => {
            use_("index");
            format!("index('{name}').using('gist', {target})")
        }
        IndexKind::Vector(spec) => {
            use_("index");
            let half = matches!(ty, DataType::Vector(_, VecPrec::F16));
            let op = match (spec.metric.name(), half) {
                ("l2", false) => "vector_l2_ops",
                ("dot", false) => "vector_ip_ops",
                (_, false) => "vector_cosine_ops",
                ("l2", true) => "halfvec_l2_ops",
                ("dot", true) => "halfvec_ip_ops",
                (_, true) => "halfvec_cosine_ops",
            };
            let default = VectorIndexSpec {
                metric: spec.metric,
                quant: spec.quant,
                ..VectorIndexSpec::default()
            }
            .resolved();
            let mut with = Vec::new();
            if spec.m != default.m {
                with.push(format!("m: {}", spec.m));
            }
            if spec.ef_construction != default.ef_construction {
                with.push(format!("ef_construction: {}", spec.ef_construction));
            }
            if spec.ef_search != default.ef_search {
                with.push(format!("ef_search: {}", spec.ef_search));
            }
            if spec.quant != Quant::None {
                with.push(format!("quant: '{}'", spec.quant.name()));
            }
            let mut s = format!("index('{name}').using('hnsw', {target}.op('{op}'))");
            if !with.is_empty() {
                s.push_str(&format!(".with({{ {} }})", with.join(", ")));
            }
            s
        }
        IndexKind::Text(spec) => {
            use_("index");
            let d = TextIndexSpec::default();
            let mut with = Vec::new();
            let mut num = |label: &str, v: f32| {
                let mut s = format!("{label}: ");
                fennec_core::num::f32_into(&mut s, v);
                with.push(s);
            };
            if spec.k1_pct != d.k1_pct {
                num("k1", spec.k1());
            }
            if spec.b_pct != d.b_pct {
                num("b", spec.b());
            }
            if spec.prefix_max != 0 {
                with.push(format!("prefix: {}", spec.prefix_max));
                if spec.prefix_min != d.prefix_min {
                    with.push(format!("prefix_min: {}", spec.prefix_min));
                }
            }
            if spec.chars {
                with.push("chars: true".into());
            }
            let mut s = format!("index('{name}').using('bm25', {target})");
            if !with.is_empty() {
                s.push_str(&format!(".with({{ {} }})", with.join(", ")));
            }
            s
        }
    })
}

fn ts_type(t: &DataType) -> String {
    match t {
        DataType::Bool => "boolean".into(),
        DataType::Int | DataType::Float => "number".into(),
        DataType::Text => "string".into(),
        DataType::Bytes => "Bytes".into(),
        DataType::Timestamp => "Timestamp".into(),
        DataType::Vector(..) => "Vector".into(),
        // pgvector's text form, `{1:0.5}/30522`: the string every transport
        // carries a sparse vector as.
        DataType::Sparse(_) => "Sparse".into(),
        DataType::List(inner) => format!("{}[]", ts_type(inner)),
        // Any value JSON holds: an object, a list, a number, text, a
        // boolean or null.
        DataType::Json => "Json".into(),
        // `[lon, lat]`, as a point is written and read.
        DataType::Geo => "Point".into(),
    }
}

fn index_note(k: &IndexKind) -> Option<String> {
    match k {
        IndexKind::None => None,
        IndexKind::Hash { unique: false } => Some(" @hash".into()),
        IndexKind::Hash { unique: true } => Some(" @unique".into()),
        IndexKind::Sorted { ttl: None } => Some(" @sorted".into()),
        IndexKind::Sorted { ttl: Some(ms) } => {
            Some(format!(" @ttl({})", fennec_core::schema::ttl_text(*ms)))
        }
        IndexKind::Vector(spec) => Some(format!(
            " @hnsw({}, m={}, ef_search={}{})",
            spec.metric.name(),
            spec.m,
            spec.ef_search,
            spec.quant_arg()
        )),
        IndexKind::Text(spec) => Some(format!(" @text({})", spec.args())),
        IndexKind::Inverted => Some(" @inverted".into()),
        IndexKind::Geo => Some(" @geo".into()),
    }
}

/// A TypeScript key: quoted when it is not an ASCII identifier. FennecQL names
/// accept Unicode letters (`año`, `résumé`), and so does TS, but quoting is
/// valid either way and `keyof` works unchanged.
fn key(name: &str) -> String {
    let ascii_ident = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ascii_ident {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fennec_core::schema::{Field, VectorIndexSpec};
    use fennec_core::value::VecPrec;

    fn schema() -> Schema {
        Schema::new(
            "articles",
            vec![
                Field::new("title", DataType::Text).required(),
                Field::new("tags", DataType::List(Box::new(DataType::Text))),
                Field::new("year", DataType::Int).indexed(IndexKind::HASH),
                Field::new("published", DataType::Timestamp),
                Field::new("embed", DataType::Vector(768, VecPrec::F32))
                    .indexed(IndexKind::Vector(VectorIndexSpec::default())),
                Field::new("meta", DataType::Json),
            ],
        )
        .unwrap()
    }

    #[test]
    fn renders_declaration() {
        let file = File {
            schemas: vec![schema()],
            statements: Vec::new(),
        };
        let out = render("data.fennec", "FennecSchema", &file);
        assert!(out.contains("export type FennecSchema = {"));
        assert!(out.contains("  articles: {"));
        // A required field takes no null, the others do.
        assert!(out.contains("    title: string;"));
        assert!(out.contains("    tags: string[] | null;"));
        assert!(out.contains("    year: number | null;"));
        assert!(out.contains("    published: Timestamp | null;"));
        assert!(out.contains("    embed: Vector | null;"));
        // A json field holds any value JSON does, the type `fennec.d.ts`
        // declares the same way, so the two pass between each other.
        assert!(out.contains("    meta: Json | null;"));
        assert!(out.contains(
            "export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };"
        ));
        // `id` is automatic, added by `Row<F>`.
        assert!(!out.contains("id:"));
        // Branded types that are used are written, unused ones are not.
        assert!(out.contains("export type Timestamp"));
        assert!(out.contains("export type Vector"));
        assert!(!out.contains("export type Bytes"));
        // The schema line survives in a comment.
        assert!(out.contains("@hnsw(cosine"), "{out}");
        assert!(out.contains("/** int @hash */"), "{out}");
    }

    #[test]
    fn empty_file() {
        let file = File {
            schemas: Vec::new(),
            statements: Vec::new(),
        };
        let out = render("empty.fennec", "FennecSchema", &file);
        assert!(out.contains("export type FennecSchema = {};"));
        // No statements, no `FennecStatements`.
        assert!(!out.contains("FennecStatements"), "{out}");
    }

    #[test]
    fn quotes_non_ascii_keys() {
        assert_eq!(key("title"), "title");
        assert_eq!(key("_a1"), "_a1");
        assert_eq!(key("summary"), "summary");
        assert_eq!(key("r\u{e9}sum\u{e9}"), "'r\u{e9}sum\u{e9}'");
    }

    /// integrations/types-golden: what each generator writes for one
    /// schema, read from its `.fennecql` file as `fennec types` reads it.
    /// `FENNEC_TYPES_GOLDEN=write` writes the files again. Where Python, Go
    /// or .NET is installed, the file is compiled as well.
    #[test]
    fn every_generator_writes_what_types_golden_holds() {
        use crate::langs::{render as lang, Lang};
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../integrations/types-golden");
        let from = "schema.fennecql";
        let file = read(dir.join(from).to_str().unwrap()).unwrap();
        let schemas = &file.schemas;
        let files = [
            ("fennec-schema.d.ts", render(from, "FennecSchema", &file)),
            (
                "tables.ts",
                render_tables(from, "@fennecdb/web/schema", schemas),
            ),
            ("schema.out.fennecql", render_fennecql(from, schemas)),
            (
                "fennec_schema.py",
                lang(Lang::Python, from, "FennecSchema", &file),
            ),
            (
                "fennec_schema.go",
                lang(Lang::Go, from, "fennecschema", &file),
            ),
            (
                "FennecSchema.cs",
                lang(Lang::CSharp, from, "FennecSchema", &file),
            ),
            ("FennecSchema.swift", lang(Lang::Swift, from, "", &file)),
            (
                "FennecSchema.kt",
                lang(Lang::Kotlin, from, "fennecschema", &file),
            ),
            ("fennec_schema.dart", lang(Lang::Dart, from, "", &file)),
        ];
        let write = std::env::var("FENNEC_TYPES_GOLDEN").is_ok_and(|v| v == "write");
        for (name, made) in &files {
            let path = dir.join(name);
            if write {
                std::fs::write(&path, made).unwrap();
                continue;
            }
            let kept = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                &kept, made,
                "{name}: FENNEC_TYPES_GOLDEN=write cargo test -p fennec-cli types"
            );
        }
        // The FennecQL a database is pulled into reads back as the same schemas.
        let pulled = fennec_ql::schema_text(&render_fennecql(from, schemas)).unwrap();
        assert_eq!(&pulled, schemas);
        // Each file is the language's own, where the language is here to say.
        let ok = |cmd: &mut std::process::Command| {
            cmd.output().map(|o| {
                (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                )
            })
        };
        if let Ok((true, _)) = ok(std::process::Command::new("python3").arg("--version")) {
            let r = ok(std::process::Command::new("python3")
                .args(["-c", "import ast,sys; ast.parse(open(sys.argv[1]).read())"])
                .arg(dir.join("fennec_schema.py")));
            assert!(matches!(r, Ok((true, _))), "python: {r:?}");
        }
        if let Ok(o) = std::process::Command::new("swiftc")
            .args(["-typecheck"])
            .arg(dir.join("FennecSchema.swift"))
            .output()
        {
            assert!(
                o.status.success(),
                "swiftc: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        if let Ok(o) = std::process::Command::new("dart")
            .args(["analyze", "--fatal-infos"])
            .arg(dir.join("fennec_schema.dart"))
            .output()
        {
            assert!(
                o.status.success(),
                "dart: {}",
                String::from_utf8_lossy(&o.stdout)
            );
        }
        if let Ok(o) = std::process::Command::new("gofmt")
            .arg("-d")
            .arg(dir.join("fennec_schema.go"))
            .output()
        {
            assert!(
                o.status.success() && o.stdout.is_empty(),
                "gofmt: {}",
                String::from_utf8_lossy(&o.stdout)
            );
        }
    }

    /// Every language's file for `file`, as `fennec types` writes it.
    fn every_file(from: &str, file: &File) -> Vec<String> {
        use crate::langs::{render as lang, Lang};
        let mut out = vec![render(from, "FennecSchema", file)];
        for l in [
            Lang::Python,
            Lang::Go,
            Lang::CSharp,
            Lang::Swift,
            Lang::Kotlin,
            Lang::Dart,
        ] {
            out.push(lang(l, from, "FennecSchema", file));
        }
        out
    }

    /// A directory of this test's own, removed when it ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("fennec-types-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A database holding named statements writes them as the schema text
    /// declaring them does: read off the file's records, by name, and
    /// typed against the collections the file holds.
    #[test]
    fn a_database_writes_its_statements_as_its_schema_text_does() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../integrations/types-golden");
        let text = std::fs::read_to_string(dir.join("schema.fennecql")).unwrap();
        let scratch = Scratch::new("statements");
        let path = scratch.0.join("data.fennec");
        fennec_ql::install();
        {
            let mut db = fennec_core::fs::open(&path).unwrap();
            for st in fennec_ql::parse(&text).unwrap() {
                db.execute(&st).unwrap();
            }
        }
        let stored = read(path.to_str().unwrap()).unwrap();
        let declared = read(dir.join("schema.fennecql").to_str().unwrap()).unwrap();
        let names = |f: &File| {
            f.statements
                .iter()
                .map(|n| n.name.clone())
                .collect::<Vec<_>>()
        };
        assert!(!declared.statements.is_empty());
        assert_eq!(names(&stored), names(&declared));
        // In name order, whatever order they were written in.
        let mut sorted = names(&declared);
        sorted.sort();
        assert_eq!(names(&declared), sorted);
        for (a, b) in every_file("data.fennec", &stored)
            .iter()
            .zip(every_file("data.fennec", &declared))
        {
            assert_eq!(a, &b);
        }
    }

    /// A statement a schema change left broken is named in a comment in
    /// every language, and typed no further; the rest are written.
    #[test]
    fn a_broken_statement_is_named_and_not_typed() {
        let scratch = Scratch::new("broken");
        let path = scratch.0.join("data.fennec");
        fennec_ql::install();
        {
            let mut db = fennec_core::fs::open(&path).unwrap();
            for t in [
                "create collection notes (body text, rank int @hash)",
                "create statement by_rank($rank int) as get notes select id, body where rank = $rank",
                "create statement bodies() as get notes select body",
                "alter collection notes drop rank",
            ] {
                let st = fennec_ql::parse(t).unwrap().remove(0);
                db.internally(|db| db.execute(&st)).unwrap();
            }
        }
        let file = read(path.to_str().unwrap()).unwrap();
        assert!(matches!(file.statements[1].answer, Answer::Broken(_)));
        for out in every_file("data.fennec", &file) {
            assert!(
                out.contains("statement `by_rank` is broken, and no type is written for it"),
                "{out}"
            );
            assert!(
                !out.contains("ByRank") && !out.contains("by_rank:"),
                "{out}"
            );
            assert!(
                out.contains("BodiesRow") || out.contains("bodies: {"),
                "{out}"
            );
        }
    }

    /// What an answer is typed as: a plain field as its row has it, `id`
    /// and a count integers, the rest JSON; a write with no `returning` no
    /// rows at all.
    #[test]
    fn an_answer_is_typed_as_far_as_its_items_are_fields() {
        let (schemas, defs) = fennec_ql::schema_statements(
            "create collection notes (body text required, rank int @hash, meta json)
             create statement a($r int) as get notes select id, body, rank, rank * 2 as twice, \
                 body as text, meta.lang where rank = $r
             create statement b() as get notes count
             create statement c() as get notes select rank, count(*) group rank
             create statement d($r int) public as del notes where rank = $r
             create statement e($r int) public as del notes where rank = $r returning id, rank",
        )
        .unwrap();
        let typed: Vec<Named> = defs.iter().map(|d| typed(d, &schemas)).collect();
        let row = |i: usize| match &typed[i].answer {
            Answer::Rows(r) => r
                .iter()
                .map(|m| (m.name.as_str(), m.ty.clone(), m.required))
                .collect::<Vec<_>>(),
            _ => panic!("{} answers no rows", typed[i].name),
        };
        assert_eq!(
            row(0),
            [
                ("id", DataType::Int, true),
                ("body", DataType::Text, true),
                ("rank", DataType::Int, false),
                ("twice", DataType::Json, false),
                ("text", DataType::Json, false),
                ("meta.lang", DataType::Json, false),
            ]
        );
        assert_eq!(row(1), [("count", DataType::Int, true)]);
        assert_eq!(
            row(2),
            [
                ("rank", DataType::Int, false),
                ("count", DataType::Int, true)
            ]
        );
        assert!(matches!(typed[3].answer, Answer::Wrote));
        assert_eq!(
            row(4),
            [("id", DataType::Int, true), ("rank", DataType::Int, false)]
        );
        // A parameter is required unless it has a default.
        assert!(typed[0].args[0].required);
    }
}
