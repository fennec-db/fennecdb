//! `fennec types --lang python|go|csharp|swift|kotlin|dart`: a row of each collection as the
//! language types JSON -- what each SDK's client reads a row into -- from a
//! database or a `schema.fennecql`, as `fennec types` writes TypeScript.
//!
//! The row as JSON carries it, not as the engine holds it: a timestamp is
//! ISO-8601 text, a vector an array of numbers, a sparse vector pgvector's
//! text, `bytes` an array of byte values, a `json` field any value -- and a
//! field that is not `required` reads `null`. `id` is every row's.
//!
//! Each named statement is two types after the collections, in name order:
//! `<Name>Args`, what a call takes -- a parameter with a default left out
//! when it is `null` -- in the form each SDK's `call` sends (a `TypedDict`,
//! a struct with JSON tags, a record, an `Encodable` struct, a data class
//! with `toMap`, a class with `toJson`), and `<Name>Row`, a row of its
//! answer, as a collection's row is read; a statement answering the rows it
//! wrote, or rows not typed yet, has a comment in the row's place
//! (`crate::types::Answer`). `integrations/types-golden/` holds each
//! language's output for one schema, which the tests make again.

use crate::types::{args_doc, no_row, row_doc, Answer, File, Member, Named};
use fennec_core::prelude::*;

/// A language `--lang` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Go,
    CSharp,
    Swift,
    Kotlin,
    Dart,
}

impl Lang {
    pub fn named(s: &str) -> Option<Lang> {
        match s {
            "python" | "py" => Some(Lang::Python),
            "go" => Some(Lang::Go),
            "csharp" | "cs" | "c#" | "dotnet" => Some(Lang::CSharp),
            "swift" => Some(Lang::Swift),
            "kotlin" | "kt" => Some(Lang::Kotlin),
            "dart" => Some(Lang::Dart),
            _ => None,
        }
    }
}

/// The file for `lang`; `name` is the Go package or the C# namespace.
pub fn render(lang: Lang, from: &str, name: &str, file: &File) -> String {
    let rows: Vec<(&str, Vec<Member>)> = file
        .schemas
        .iter()
        .filter(|s| !s.name.starts_with('_'))
        .map(|s| (s.name.as_str(), row_members(s)))
        .collect();
    let named = &file.statements[..];
    match lang {
        Lang::Python => python(from, &rows, named),
        Lang::Go => go(from, name, &rows, named),
        Lang::CSharp => csharp(from, name, &rows, named),
        Lang::Swift => swift(from, &rows, named),
        Lang::Kotlin => kotlin(from, name, &rows, named),
        Lang::Dart => dart(from, &rows, named),
    }
}

fn header(comment: &str, from: &str, flag: &str, file: &str) -> String {
    format!(
        "{comment} generated from the fennecdb schema: {from}\n\
         {comment} fennec types --lang {flag} {from} > {file}\n\
         {comment} Do not edit by hand -- regenerate when the schema changes.\n"
    )
}

/// A collection's row: its `id`, then each field.
fn row_members(s: &Schema) -> Vec<Member> {
    let mut out = vec![Member {
        name: "id".into(),
        ty: DataType::Int,
        required: true,
        note: String::new(),
    }];
    out.extend(s.fields.iter().map(|f| Member {
        name: f.name.clone(),
        ty: f.ty.clone(),
        required: f.required,
        note: note(f),
    }));
    out
}

/// A row's own `id`, an integer never `null`, which each language reads in
/// a form of its own.
fn is_id(m: &Member) -> bool {
    m.name == "id" && m.ty == DataType::Int && m.required
}

/// The names of a statement's two types, `<Name>Args` and `<Name>Row`: its
/// name made a type's by `case` (`pascal`, or Dart's), each taken once in
/// the file.
fn statement_names(n: &Named, case: fn(&str) -> String, taken: &mut Vec<String>) -> [String; 2] {
    let base = case(&n.name);
    [
        unique(format!("{base}Args"), taken),
        unique(format!("{base}Row"), taken),
    ]
}

/// The rows a statement's answer has, where it is typed.
fn answered(n: &Named) -> Option<&[Member]> {
    match &n.answer {
        Answer::Rows(rows) => Some(rows),
        _ => None,
    }
}

/// `title` -> `Title`, `product_id` -> `ProductId`: a type's or a member's
/// name, the field's own kept in the JSON tag beside it.
fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in name.chars() {
        if c == '_' {
            up = true;
            continue;
        }
        match up {
            true => out.extend(c.to_uppercase()),
            false => out.push(c),
        }
        up = false;
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'F');
    }
    out
}

/// A name every language takes as a word: a path an answer names,
/// `meta.lang`, as `meta_lang`, before a member is named from it.
fn word(name: &str) -> String {
    name.chars()
        .map(|c| match c.is_alphanumeric() || c == '_' {
            true => c,
            false => '_',
        })
        .collect()
}

/// A name every type of one file takes once: a second gets a number.
fn unique(name: String, taken: &mut Vec<String>) -> String {
    let mut n = name.clone();
    let mut i = 2;
    while taken.contains(&n) {
        n = format!("{name}{i}");
        i += 1;
    }
    taken.push(n.clone());
    n
}

/// A field's schema line, the comment beside its member.
pub(crate) fn note(f: &fennec_core::schema::Field) -> String {
    let mut s = f.ty.name();
    if let Some(c) = f.collate {
        s.push_str(" collate ");
        s.push_str(c.name());
    }
    if f.nocase {
        s.push_str(" nocase");
    }
    if let Some(ix) = fennec_core::declared::index_text(&f.index) {
        s.push(' ');
        s.push_str(&ix);
    }
    if f.required {
        s.push_str(" required");
    }
    s
}

/// A text inside a block comment, which neither ends it nor -- where
/// comments nest, Kotlin's and Swift's -- opens another.
fn block(text: &str) -> String {
    text.replace("*/", "*\\/").replace("/*", "/\\*")
}

// ------------------------------------------------------------------ Python

const PY_KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

fn py_type(t: &DataType) -> String {
    match t {
        DataType::Bool => "bool".into(),
        DataType::Int => "int".into(),
        DataType::Float => "float".into(),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => "str".into(),
        DataType::Bytes => "list[int]".into(),
        DataType::Vector(..) => "list[float]".into(),
        DataType::Json => "Any".into(),
        DataType::Geo => "list[float]".into(),
        DataType::List(inner) => format!("list[{}]", py_type(inner)),
    }
}

/// Whether a key is no name a class body may declare: a keyword, or a path
/// an answer names (`meta.lang`).
fn py_functional(members: &[&Member]) -> bool {
    members.iter().any(|m| {
        PY_KEYWORDS.contains(&m.name.as_str())
            || m.name.starts_with(|c: char| c.is_ascii_digit())
            || !m.name.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// A `TypedDict` written as a class, or -- where a key is no Python name,
/// a keyword or a path -- in the functional form; `total` false where
/// every key may be left out, `bases` what a class form derives from.
fn py_dict(
    out: &mut String,
    name: &str,
    doc: Option<&str>,
    members: &[&Member],
    total: bool,
    bases: &str,
) {
    let ty = |m: &Member| {
        let t = py_type(&m.ty);
        match m.required || m.ty == DataType::Json {
            true => t,
            false => format!("Optional[{t}]"),
        }
    };
    let note = |m: &Member| match m.note.is_empty() {
        true => String::new(),
        false => format!("  # {}", m.note),
    };
    let total = if total { "" } else { ", total=False" };
    out.push_str("\n\n");
    if py_functional(members) {
        out.push_str(&format!("{name} = TypedDict('{name}', {{\n"));
        for m in members {
            out.push_str(&format!("    '{}': {},{}\n", m.name, ty(m), note(m)));
        }
        out.push_str(&format!("}}{total})\n"));
        if let Some(d) = doc {
            let d = d.replace('\\', "\\\\").replace('\'', "\\'");
            out.push_str(&format!("{name}.__doc__ = '{d}'\n"));
        }
        return;
    }
    out.push_str(&format!("class {name}({bases}{total}):\n"));
    match doc {
        Some(d) => {
            let d = d.replace('\\', "\\\\").replace('"', "\\\"");
            out.push_str(&format!("    \"\"\"{d}\"\"\"\n"));
            if !members.is_empty() {
                out.push('\n');
            }
        }
        None if members.is_empty() => out.push_str("    pass\n"),
        None => {}
    }
    for m in members {
        out.push_str(&format!("    {}: {}{}\n", m.name, ty(m), note(m)));
    }
}

/// A statement's arguments: the parameters with no default a total
/// `TypedDict`, those with one a `total=False` one -- Python 3.10, which
/// the SDK takes, has no `NotRequired` -- and the type both.
fn py_args(out: &mut String, name: &str, doc: &str, members: &[Member]) {
    let (required, optional): (Vec<&Member>, Vec<&Member>) =
        members.iter().partition(|m| m.required);
    if optional.is_empty() || required.is_empty() {
        py_dict(
            out,
            name,
            Some(doc),
            if optional.is_empty() {
                &required
            } else {
                &optional
            },
            optional.is_empty(),
            "TypedDict",
        );
        return;
    }
    let base = format!("_{name}");
    py_dict(out, &base, None, &required, true, "TypedDict");
    // A class form derives from the functional one; keys of the functional
    // form alone are a base of their own.
    if py_functional(&optional) {
        let more = format!("_{name}Optional");
        py_dict(out, &more, None, &optional, false, "TypedDict");
        py_dict(out, name, Some(doc), &[], true, &format!("{base}, {more}"));
    } else {
        py_dict(out, name, Some(doc), &optional, false, &base);
    }
}

/// A `TypedDict` a collection, the row as `json.loads` reads it; a name
/// Python cannot take as an attribute -- a keyword -- written in the
/// functional form. A statement's arguments and row after them.
fn python(from: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut out = header("#", from, "python", "fennec_schema.py");
    out.push_str("\nfrom typing import Any, Optional, TypedDict\n");
    let mut taken = Vec::new();
    for (name, members) in rows {
        let class = unique(pascal(name), &mut taken);
        let members: Vec<&Member> = members.iter().collect();
        let doc = format!("A row of `{name}`.");
        py_dict(&mut out, &class, Some(&doc), &members, true, "TypedDict");
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            out.push_str(&format!("\n\n# {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row] = statement_names(n, pascal, &mut taken);
        py_args(&mut out, &args, &args_doc(n), &n.args);
        match answered(n) {
            Some(members) => {
                let members: Vec<&Member> = members.iter().collect();
                py_dict(
                    &mut out,
                    &row,
                    Some(&row_doc(n)),
                    &members,
                    true,
                    "TypedDict",
                );
            }
            None => out.push_str(&format!("\n\n# {}\n", no_row(n).unwrap_or_default())),
        }
    }
    out
}

// ---------------------------------------------------------------------- Go

fn go_type(t: &DataType, json: &mut bool) -> String {
    match t {
        DataType::Bool => "bool".into(),
        DataType::Int => "int64".into(),
        DataType::Float => "float64".into(),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => "string".into(),
        DataType::Bytes => "[]int".into(),
        DataType::Vector(..) => "[]float32".into(),
        DataType::Json => {
            *json = true;
            "json.RawMessage".into()
        }
        // `[lon, lat]`, which `encoding/json` reads into an array of two.
        DataType::Geo => "[2]float64".into(),
        DataType::List(inner) => format!("[]{}", go_type(inner, json)),
    }
}

/// A struct, its doc comment `doc` after its name: each member's name, type,
/// JSON tag and comment, aligned in columns as gofmt aligns them.
fn go_struct(body: &mut String, name: &str, doc: &str, members: Vec<[String; 4]>) {
    body.push_str(&format!("\n// {name} {doc}\n"));
    if members.is_empty() {
        body.push_str(&format!("type {name} struct{{}}\n"));
        return;
    }
    body.push_str(&format!("type {name} struct {{\n"));
    let width = |s: &str| s.chars().count();
    let w = members.iter().map(|m| width(&m[0])).max().unwrap_or(0);
    let tw = members.iter().map(|m| m[1].len()).max().unwrap_or(0);
    let gw = members.iter().map(|m| width(&m[2]) + 9).max().unwrap_or(0);
    for [m, t, tag, note] in members {
        let pad = " ".repeat(w - width(&m) + 1);
        let tpad = " ".repeat(tw - t.len() + 1);
        let tag = format!("`json:\"{tag}\"`");
        let line = match note.is_empty() {
            true => format!("\t{m}{pad}{t}{tpad}{tag}"),
            false => format!(
                "\t{m}{pad}{t}{tpad}{tag}{} // {note}",
                " ".repeat(gw - width(&tag))
            ),
        };
        body.push_str(line.trim_end());
        body.push('\n');
    }
    body.push_str("}\n");
}

/// A row's members: a field that reads `null` a pointer, but for a slice
/// and a raw message, which read `null` as nil already.
fn go_row(members: &[Member], json: &mut bool) -> Vec<[String; 4]> {
    let mut names = Vec::new();
    members
        .iter()
        .map(|f| {
            let t = go_type(&f.ty, json);
            let nil = t.starts_with("[]") || t == "json.RawMessage";
            let t = match f.required || nil {
                true => t,
                false => format!("*{t}"),
            };
            let m = match is_id(f) {
                true => unique("ID".into(), &mut names),
                false => unique(pascal(&word(&f.name)), &mut names),
            };
            [m, t, f.name.clone(), f.note.clone()]
        })
        .collect()
}

/// A struct a collection, its fields tagged with their JSON names; a field
/// that reads `null` a pointer, but for a slice and a raw message, which
/// read `null` as nil already. A statement's arguments are a struct
/// `encoding/json` marshals, one with a default a pointer left out when
/// nil (`omitempty`), so the default is taken -- a pointer to a slice too,
/// which an empty slice does not leave out.
fn go(from: &str, package: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut json = false;
    let mut body = String::new();
    let mut taken = Vec::new();
    for (s, members) in rows {
        let name = unique(pascal(s), &mut taken);
        let doc = format!("is a row of `{s}`.");
        go_struct(&mut body, &name, &doc, go_row(members, &mut json));
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            body.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row] = statement_names(n, pascal, &mut taken);
        let mut names = Vec::new();
        let members = n
            .args
            .iter()
            .map(|a| {
                let t = go_type(&a.ty, &mut json);
                let m = unique(pascal(&word(&a.name)), &mut names);
                match a.required {
                    true => [m, t, a.name.clone(), a.note.clone()],
                    false => [
                        m,
                        format!("*{t}"),
                        format!("{},omitempty", a.name),
                        a.note.clone(),
                    ],
                }
            })
            .collect();
        let doc = args_doc(n).replacen("The arguments", "are the arguments", 1);
        go_struct(&mut body, &args, &doc, members);
        match answered(n) {
            Some(members) => {
                let doc = row_doc(n).replacen("A row", "is a row", 1);
                go_struct(&mut body, &row, &doc, go_row(members, &mut json));
            }
            None => body.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default())),
        }
    }
    let mut out = header("//", from, "go", "fennec_schema.go");
    out.push_str(&format!("\npackage {package}\n"));
    if json {
        out.push_str("\nimport \"encoding/json\"\n");
    }
    out.push_str(&body);
    out
}

// ---------------------------------------------------------------------- C#

fn cs_type(t: &DataType) -> String {
    match t {
        DataType::Bool => "bool".into(),
        DataType::Int => "long".into(),
        DataType::Float => "double".into(),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => "string".into(),
        DataType::Bytes => "int[]".into(),
        DataType::Vector(..) => "float[]".into(),
        DataType::Json => "JsonElement".into(),
        DataType::Geo => "double[]".into(),
        DataType::List(inner) => format!("{}[]", cs_type(inner)),
    }
}

/// A summary as XML documentation writes it: `<`, `>` and `&` escaped, and
/// each `` `name` `` a `<c>name</c>`.
fn xml(text: &str) -> String {
    let text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut out = String::new();
    let mut open = false;
    for c in text.chars() {
        match c {
            '`' => {
                out.push_str(if open { "</c>" } else { "<c>" });
                open = !open;
            }
            c => out.push(c),
        }
    }
    if open {
        out.push_str("</c>");
    }
    out
}

/// A positional record, its summary `doc`: each member one parameter.
fn cs_record(out: &mut String, name: &str, doc: &str, lines: &[String]) {
    out.push_str(&format!("\n/// <summary>{doc}</summary>\n"));
    match lines.is_empty() {
        true => out.push_str(&format!("public sealed record {name}();\n")),
        false => {
            out.push_str(&format!("public sealed record {name}(\n"));
            out.push_str(&lines.join(",\n"));
            out.push_str(");\n");
        }
    }
}

/// A member of a record: its comment, its JSON name and anything after
/// it in the attribute, its type and name.
fn cs_member(m: &Member, t: &str, names: &mut Vec<String>, attr: &str) -> String {
    let p = unique(pascal(&word(&m.name)), names);
    let comment = match m.note.is_empty() {
        true => String::new(),
        false => format!("/* {} */ ", block(&m.note)),
    };
    format!(
        "    {comment}[property: JsonPropertyName(\"{}\"){attr}] {t} {p}",
        m.name
    )
}

/// A record a collection, each member named for JSON by the field's name
/// (`System.Text.Json`); a field that reads `null` nullable. A statement's
/// arguments are a record `System.Text.Json` writes, one with a default
/// left out when `null` (`WhenWritingNull`), so the default is taken, and
/// given `= null` where no parameter without one comes after it.
fn csharp(from: &str, namespace: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut out = header("//", from, "csharp", "FennecSchema.cs");
    out.push_str(&format!(
        "\n#nullable enable\nusing System.Text.Json;\nusing System.Text.Json.Serialization;\n\nnamespace {namespace};\n"
    ));
    let row = |name: &str, members: &[Member]| {
        let mut names = vec![name.to_string()];
        members
            .iter()
            .map(|f| {
                let t = cs_type(&f.ty);
                let t = match f.required {
                    true => t,
                    false => format!("{t}?"),
                };
                cs_member(f, &t, &mut names, "")
            })
            .collect::<Vec<_>>()
    };
    let mut taken = Vec::new();
    for (s, members) in rows {
        let name = unique(pascal(s), &mut taken);
        let doc = format!("A row of <c>{s}</c>.");
        cs_record(&mut out, &name, &doc, &row(&name, members));
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row_name] = statement_names(n, pascal, &mut taken);
        let last_required = n.args.iter().rposition(|a| a.required);
        let mut names = vec![args.clone()];
        let lines: Vec<String> = n
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let t = cs_type(&a.ty);
                match a.required {
                    true => cs_member(a, &t, &mut names, ""),
                    false => {
                        let skip = ", JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)";
                        let mut line = cs_member(a, &format!("{t}?"), &mut names, skip);
                        if last_required.is_none_or(|l| i > l) {
                            line.push_str(" = null");
                        }
                        line
                    }
                }
            })
            .collect();
        cs_record(&mut out, &args, &xml(&args_doc(n)), &lines);
        match answered(n) {
            Some(members) => {
                let lines = row(&row_name, members);
                cs_record(&mut out, &row_name, &xml(&row_doc(n)), &lines);
            }
            None => out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default())),
        }
    }
    out
}

/// `product_id` -> `productId`: a member's name where the language's own
/// is camel case, the field's kept beside it for the JSON.
fn camel(name: &str) -> String {
    let p = pascal(name);
    let mut c = p.chars();
    match c.next() {
        Some(first) => first.to_lowercase().chain(c).collect(),
        None => p,
    }
}

// ------------------------------------------------------------------- Swift

/// Swift's keywords a declaration's name is escaped from (`` `in` ``), in
/// its declarations, statements and expressions.
const SWIFT_KEYWORDS: [&str; 45] = [
    "Any",
    "as",
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "let",
    "nil",
    "operator",
    "private",
    "protocol",
    "public",
    "repeat",
    "rethrows",
    "return",
    "self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "var",
];

fn swift_type(t: &DataType, json: &mut bool) -> String {
    match t {
        DataType::Bool => "Bool".into(),
        DataType::Int => "Int64".into(),
        DataType::Float => "Double".into(),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => "String".into(),
        DataType::Bytes => "[UInt8]".into(),
        DataType::Vector(..) => "[Float]".into(),
        DataType::Json => {
            *json = true;
            "JSON".into()
        }
        DataType::Geo => "[Double]".into(),
        DataType::List(inner) => format!("[{}]", swift_type(inner, json)),
    }
}

/// A `Codable` struct, its doc comment `doc`; a member named apart from
/// its key keeps it in `CodingKeys`. With `init`, a public memberwise
/// initialiser, each optional member `nil` unless given -- what a caller
/// in another module builds arguments with.
fn swift_struct(
    body: &mut String,
    json: &mut bool,
    name: &str,
    doc: &str,
    members: &[Member],
    init: bool,
) {
    body.push_str(&format!(
        "\n/// {doc}\npublic struct {name}: Codable, Sendable, Equatable {{\n"
    ));
    let mut keys = Vec::new();
    let mut renamed = false;
    let mut names = Vec::new();
    let mut params = Vec::new();
    let mut sets = Vec::new();
    for f in members {
        let t = swift_type(&f.ty, json);
        let opt = if f.required { "" } else { "?" };
        let m = unique(camel(&word(&f.name)), &mut names);
        let shown = match SWIFT_KEYWORDS.contains(&m.as_str()) {
            true => format!("`{m}`"),
            false => m.clone(),
        };
        match f.note.is_empty() {
            true => body.push_str(&format!("    public var {shown}: {t}{opt}\n")),
            false => body.push_str(&format!(
                "    public var {shown}: {t}{opt}  // {}\n",
                f.note
            )),
        }
        let default = if f.required { "" } else { " = nil" };
        params.push(format!("{shown}: {t}{opt}{default}"));
        sets.push(format!("        self.{shown} = {shown}\n"));
        match m == f.name {
            true => keys.push(shown),
            false => {
                renamed = true;
                keys.push(format!("{shown} = \"{}\"", f.name));
            }
        }
    }
    if init {
        match members.is_empty() {
            true => body.push_str("    public init() {}\n"),
            false => body.push_str(&format!(
                "\n    public init({}) {{\n{}    }}\n",
                params.join(", "),
                sets.concat()
            )),
        }
    }
    if renamed {
        body.push_str("\n    enum CodingKeys: String, CodingKey {\n");
        for k in keys {
            body.push_str(&format!("        case {k}\n"));
        }
        body.push_str("    }\n");
    }
    body.push_str("}\n");
}

/// A `Codable` struct a collection, what `Row.decode(as:)` reads a row
/// into; a member named apart from its field keeps it in `CodingKeys`, and
/// a `json` field is a `JSON` the file declares, as Swift has no value of
/// any JSON of its own. A statement's arguments are a struct `JSONEncoder`
/// writes, a member left `nil` left out (`encodeIfPresent`), so its
/// default is taken.
fn swift(from: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut json = false;
    let mut body = String::new();
    let mut taken = Vec::new();
    for (s, members) in rows {
        let name = unique(pascal(s), &mut taken);
        let doc = format!("A row of `{s}`.");
        swift_struct(&mut body, &mut json, &name, &doc, members, false);
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            body.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row] = statement_names(n, pascal, &mut taken);
        swift_struct(&mut body, &mut json, &args, &args_doc(n), &n.args, true);
        match answered(n) {
            Some(members) => swift_struct(&mut body, &mut json, &row, &row_doc(n), members, false),
            None => body.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default())),
        }
    }
    let mut out = header("//", from, "swift", "FennecSchema.swift");
    out.push_str("\nimport Foundation\n");
    if json {
        out.push_str(
            "\n/// `json`: any value JSON holds.\n\
             public indirect enum JSON: Codable, Sendable, Equatable {\n\
             \x20   case null, bool(Bool), number(Double), string(String), array([JSON]), object([String: JSON])\n\n\
             \x20   public init(from decoder: Decoder) throws {\n\
             \x20       let c = try decoder.singleValueContainer()\n\
             \x20       if c.decodeNil() { self = .null }\n\
             \x20       else if let b = try? c.decode(Bool.self) { self = .bool(b) }\n\
             \x20       else if let n = try? c.decode(Double.self) { self = .number(n) }\n\
             \x20       else if let s = try? c.decode(String.self) { self = .string(s) }\n\
             \x20       else if let a = try? c.decode([JSON].self) { self = .array(a) }\n\
             \x20       else { self = .object(try c.decode([String: JSON].self)) }\n\
             \x20   }\n\n\
             \x20   public func encode(to encoder: Encoder) throws {\n\
             \x20       var c = encoder.singleValueContainer()\n\
             \x20       switch self {\n\
             \x20       case .null: try c.encodeNil()\n\
             \x20       case .bool(let b): try c.encode(b)\n\
             \x20       case .number(let n): try c.encode(n)\n\
             \x20       case .string(let s): try c.encode(s)\n\
             \x20       case .array(let a): try c.encode(a)\n\
             \x20       case .object(let o): try c.encode(o)\n\
             \x20       }\n\
             \x20   }\n\
             }\n",
        );
    }
    out.push_str(&body);
    out
}

// ------------------------------------------------------------------ Kotlin

/// Kotlin's hard keywords, which no name may be unless escaped.
const KOTLIN_KEYWORDS: [&str; 28] = [
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

/// The Kotlin type, and how a value of it is read off the binding's `Row`
/// (`v` the value, `Any?`).
fn kotlin_type(t: &DataType) -> (String, String) {
    match t {
        DataType::Bool => ("Boolean".into(), "(v as Boolean)".into()),
        DataType::Int => ("Long".into(), "(v as Number).toLong()".into()),
        DataType::Float => ("Double".into(), "(v as Number).toDouble()".into()),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => {
            ("String".into(), "(v as String)".into())
        }
        DataType::Bytes => (
            "List<Int>".into(),
            "(v as List<*>).map { (it as Number).toInt() }".into(),
        ),
        DataType::Vector(..) => (
            "List<Float>".into(),
            "(v as List<*>).map { (it as Number).toFloat() }".into(),
        ),
        DataType::Json => ("Any".into(), "v".into()),
        DataType::Geo => (
            "List<Double>".into(),
            "(v as List<*>).map { (it as Number).toDouble() }".into(),
        ),
        DataType::List(inner) => {
            let (t, read) = kotlin_type(inner);
            (
                format!("List<{t}>"),
                format!("(v as List<*>).map {{ v -> {read} }}"),
            )
        }
    }
}

/// A member's Kotlin name, escaped where it is a keyword. A property named
/// `class` would be read through `getClass()`, which every JVM object has
/// and no class may declare again: it is `class_`.
fn kotlin_name(name: &str, names: &mut Vec<String>) -> String {
    let m = match camel(&word(name)) {
        m if m == "class" => "class_".to_string(),
        m => m,
    };
    let m = unique(m, names);
    match KOTLIN_KEYWORDS.contains(&m.as_str()) {
        true => format!("`{m}`"),
        false => m,
    }
}

/// A data class of a row, and `from(row)`, which reads one off the
/// binding's `Row`: a member named apart from its field reads it by the
/// field's name.
fn kotlin_row(out: &mut String, name: &str, doc: &str, members: &[Member]) {
    let mut fields = Vec::new();
    let mut reads = Vec::new();
    let mut names = Vec::new();
    for f in members {
        if is_id(f) {
            names.push("id".to_string());
            fields.push("    val id: Long,".to_string());
            reads.push("            id = (row[\"id\"] as Number).toLong(),".to_string());
            continue;
        }
        let (t, read) = kotlin_type(&f.ty);
        let shown = kotlin_name(&f.name, &mut names);
        let nullable = !f.required || f.ty == DataType::Json;
        let t = if nullable { format!("{t}?") } else { t };
        fields.push(format!("    val {shown}: {t}, // {}", f.note));
        let key = f.name.replace('"', "\\\"");
        reads.push(match nullable {
            true => format!("            {shown} = row[\"{key}\"]?.let {{ v -> {read} }},"),
            false => format!("            {shown} = row[\"{key}\"]!!.let {{ v -> {read} }},"),
        });
    }
    out.push_str(&format!(
        "\n/** {} */\ndata class {name}(\n{}\n) {{\n    companion object {{\n        /** The row as the binding reads it. */\n        fun from(row: Row) = {name}(\n{}\n        )\n    }}\n}}\n",
        block(doc),
        fields.join("\n"),
        reads.join("\n")
    ));
}

/// A statement's arguments: a data class, and `toMap()`, the map a call
/// takes, a member left `null` left out so its default is taken. A
/// statement of no parameters is a plain class, as a data class takes one.
fn kotlin_args(out: &mut String, name: &str, doc: &str, members: &[Member]) {
    let doc = block(doc);
    let to_map = "    /** The arguments as a call takes them: one left `null` is left out, for its default. */\n";
    if members.is_empty() {
        out.push_str(&format!(
            "\n/** {doc} */\nclass {name} {{\n{to_map}    fun toMap(): Map<String, Any?> = emptyMap()\n}}\n"
        ));
        return;
    }
    let mut fields = Vec::new();
    let mut puts = Vec::new();
    let mut names = Vec::new();
    for a in members {
        let (t, _) = kotlin_type(&a.ty);
        let shown = kotlin_name(&a.name, &mut names);
        let key = a.name.replace('"', "\\\"");
        // `this.`, as a parameter may be named `m`.
        match a.required {
            true => {
                fields.push(format!("    val {shown}: {t}, // {}", a.note));
                puts.push(format!("        m[\"{key}\"] = this.{shown}"));
            }
            false => {
                fields.push(format!("    val {shown}: {t}? = null, // {}", a.note));
                puts.push(format!(
                    "        if (this.{shown} != null) m[\"{key}\"] = this.{shown}"
                ));
            }
        }
    }
    out.push_str(&format!(
        "\n/** {doc} */\ndata class {name}(\n{}\n) {{\n{to_map}    fun toMap(): Map<String, Any?> {{\n        val m = mutableMapOf<String, Any?>()\n{}\n        return m\n    }}\n}}\n",
        fields.join("\n"),
        puts.join("\n")
    ));
}

/// A data class a collection, and `from(row)`, which reads one off the
/// binding's `Row`: a member named apart from its field reads it by the
/// field's name. A statement's arguments and row after them.
fn kotlin(from: &str, package: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut out = header("//", from, "kotlin", "FennecSchema.kt");
    out.push_str(&format!(
        "\npackage {}\n\nimport com.fennecdb.Row\n",
        package.to_lowercase()
    ));
    let mut taken = Vec::new();
    for (s, members) in rows {
        let name = unique(pascal(s), &mut taken);
        kotlin_row(&mut out, &name, &format!("A row of `{s}`."), members);
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row] = statement_names(n, pascal, &mut taken);
        kotlin_args(&mut out, &args, &args_doc(n), &n.args);
        match answered(n) {
            Some(members) => kotlin_row(&mut out, &row, &row_doc(n), members),
            None => out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default())),
        }
    }
    out
}

// -------------------------------------------------------------------- Dart

/// Dart's reserved words, which no name may be.
const DART_KEYWORDS: [&str; 33] = [
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// A Dart name: ASCII letters, digits and `_` alone, as Dart reads one.
fn dart_name(n: &str) -> String {
    let mut out: String = n
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) || DART_KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// A Dart type's name made from a statement's or a collection's.
fn dart_type_name(n: &str) -> String {
    dart_name(&pascal(n))
}

/// The Dart type, and how a value of it is read off a JSON map (`v`).
fn dart_type(t: &DataType) -> (String, String) {
    match t {
        DataType::Bool => ("bool".into(), "v as bool".into()),
        DataType::Int => ("int".into(), "v as int".into()),
        DataType::Float => ("double".into(), "(v as num).toDouble()".into()),
        DataType::Text | DataType::Timestamp | DataType::Sparse(_) => {
            ("String".into(), "v as String".into())
        }
        DataType::Bytes => (
            "List<int>".into(),
            "[for (final e in v as List) e as int]".into(),
        ),
        DataType::Vector(..) => (
            "List<double>".into(),
            "[for (final e in v as List) (e as num).toDouble()]".into(),
        ),
        DataType::Json => ("Object".into(), "v".into()),
        DataType::Geo => (
            "List<double>".into(),
            "[for (final e in v as List) (e as num).toDouble()]".into(),
        ),
        DataType::List(inner) => {
            let (t, read) = dart_type(inner);
            (
                format!("List<{t}>"),
                format!("[for (final v in v as List) {read}]"),
            )
        }
    }
}

/// A class of a row, and `fromJson`, which reads one off the map a row is:
/// a member named apart from its field reads it by the field's name.
fn dart_row(out: &mut String, name: &str, doc: &str, members: &[Member]) {
    let mut fields = Vec::new();
    let mut params = Vec::new();
    let mut reads = Vec::new();
    // No member takes the constructor's name.
    let mut names = vec!["fromJson".to_string()];
    for f in members {
        if is_id(f) {
            names.push("id".to_string());
            fields.push("  final int id;".to_string());
            params.push("required this.id".to_string());
            reads.push("        id: j['id'] as int,".to_string());
            continue;
        }
        let (t, read) = dart_type(&f.ty);
        let m = unique(dart_name(&camel(&word(&f.name))), &mut names);
        let nullable = !f.required || f.ty == DataType::Json;
        let shown_t = if nullable { format!("{t}?") } else { t };
        fields.push(format!("  /// {}\n  final {shown_t} {m};", f.note));
        params.push(match nullable {
            true => format!("this.{m}"),
            false => format!("required this.{m}"),
        });
        let key = f.name.replace('\'', "\\'");
        reads.push(match nullable {
            true => {
                format!("        {m}: switch (j['{key}']) {{ null => null, final v => {read} }},")
            }
            false => format!("        {m}: switch (j['{key}']) {{ final v => {read} }},"),
        });
    }
    out.push_str(&format!(
        "\n/// {doc}\nclass {name} {{\n{}\n\n  const {name}({{{}}});\n\n  /// The row as JSON reads it.\n  factory {name}.fromJson(Map<String, Object?> j) => {name}(\n{}\n      );\n}}\n",
        fields.join("\n"),
        params.join(", "),
        reads.join("\n")
    ));
}

/// A statement's arguments: a class, and `toJson()`, the map a call takes,
/// a member left `null` left out so its default is taken.
fn dart_args(out: &mut String, name: &str, doc: &str, members: &[Member]) {
    let to_json =
        "  /// The arguments as a call takes them: one left `null` is left out, for its default.\n";
    if members.is_empty() {
        out.push_str(&format!(
            "\n/// {doc}\nclass {name} {{\n  const {name}();\n\n{to_json}  Map<String, Object?> toJson() => {{}};\n}}\n"
        ));
        return;
    }
    let mut fields = Vec::new();
    let mut params = Vec::new();
    let mut entries = Vec::new();
    let mut names = vec!["toJson".to_string()];
    for a in members {
        let (t, _) = dart_type(&a.ty);
        let m = unique(dart_name(&camel(&word(&a.name))), &mut names);
        let key = a.name.replace('\'', "\\'");
        match a.required {
            true => {
                fields.push(format!("  /// {}\n  final {t} {m};", a.note));
                params.push(format!("required this.{m}"));
                entries.push(format!("        '{key}': {m},"));
            }
            false => {
                fields.push(format!("  /// {}\n  final {t}? {m};", a.note));
                params.push(format!("this.{m}"));
                entries.push(format!("        if ({m} != null) '{key}': {m},"));
            }
        }
    }
    out.push_str(&format!(
        "\n/// {doc}\nclass {name} {{\n{}\n\n  const {name}({{{}}});\n\n{to_json}  Map<String, Object?> toJson() => {{\n{}\n      }};\n}}\n",
        fields.join("\n"),
        params.join(", "),
        entries.join("\n")
    ));
}

/// A class a collection, and `fromJson`, which reads one off the map a row
/// is: a member named apart from its field reads it by the field's name. A
/// statement's arguments and row after them.
fn dart(from: &str, rows: &[(&str, Vec<Member>)], named: &[Named]) -> String {
    let mut out = header("//", from, "dart", "fennec_schema.dart");
    let mut taken = Vec::new();
    for (s, members) in rows {
        let name = unique(dart_type_name(s), &mut taken);
        dart_row(&mut out, &name, &format!("A row of `{s}`."), members);
    }
    for n in named {
        if let Answer::Broken(_) = n.answer {
            out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default()));
            continue;
        }
        let [args, row] = statement_names(n, dart_type_name, &mut taken);
        dart_args(&mut out, &args, &args_doc(n), &n.args);
        match answered(n) {
            Some(members) => dart_row(&mut out, &row, &row_doc(n), members),
            None => out.push_str(&format!("\n// {}\n", no_row(n).unwrap_or_default())),
        }
    }
    out
}
