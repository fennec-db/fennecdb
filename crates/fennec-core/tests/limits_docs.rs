//! site/content/docs/limits.html's table of fixed limits, every row held to
//! the code. Each row has one entry below: its first cell's words, the
//! numbers its second cell must say -- read from the constant the code
//! holds it by, never a copy typed here -- and the tests that hold it, each
//! one of three kinds:
//!
//! - driven: the bound is taken, one past it refused with the error the
//!   page names, and nothing cut -- a ranking, a list, a facet past it
//!   answers no rows at all;
//! - at the type's level, where the row cannot be driven at its size, with
//!   why: an 8 MiB stack, whose overflow is an abort. A bound no test
//!   machine reaches otherwise -- a 4 GiB payload, a graph of 2^32 - 1
//!   nodes -- is driven: where it is decided, and through the writes at a
//!   bound a test's thread holds them to;
//! - a path either side of a number that is no refusal (a batch, a
//!   threshold, a ring), with what it is.
//!
//! A row with no entry fails naming the row, an entry with no row naming
//! the entry (the list follows the page), a number the code does not hold
//! naming both, and a test that is gone naming its file. A test elsewhere
//! -- fennec-ql's, fennec-http's -- is named by its file and function, and
//! found there as a test CI runs (`held_by`): `#[test]` right before its
//! `fn`, outside comments and strings, neither ignored nor `cfg`'d out of
//! every build CI's `make test` makes. The mapping runs in every build
//! (`features.rs` takes this file in too); a row's own test here needing a
//! feature the build lacks is `cfg`'d with it, the row still mapped.

use fennec_core::prelude::*;

const PAGE: &str = include_str!("../../../site/content/docs/limits.html");

/// The repository's root, which a named test's file is under.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../");

/// A test that holds a row: its file from the repository's root, and its
/// function.
type Test = (&'static str, &'static str);

/// What holds a row.
enum Held {
    /// n taken, n + 1 refused in the page's words, nothing cut.
    Driven(&'static [Test]),
    /// At the type's level: why the row cannot be driven at its size.
    Typed(&'static str, &'static [Test]),
    /// Either side of a number that is no refusal: what it is.
    Path(&'static str, &'static [Test]),
}

struct Row {
    /// The first cell, tags stripped and whitespace folded.
    key: &'static str,
    /// The numbers the second cell says, from the code; `None` where this
    /// build cannot ask the code (a zone table it is made without).
    value: Option<Vec<f64>>,
    /// Where `value` comes from, for the refusal.
    from: &'static str,
    /// Words the second cell says: a type's name, a default of none.
    words: &'static str,
    /// What the third cell must say, from the code.
    note: Vec<String>,
    /// A number the page says in words, and the code's: (what, page, code).
    said: Vec<(&'static str, f64, f64)>,
    held: Held,
}

fn row(key: &'static str, value: Vec<f64>, from: &'static str, held: Held) -> Row {
    Row {
        key,
        value: Some(value),
        from,
        words: "",
        note: Vec::new(),
        said: Vec::new(),
        held,
    }
}

impl Row {
    fn words(mut self, words: &'static str) -> Row {
        self.words = words;
        self
    }

    fn note(mut self, says: impl Into<String>) -> Row {
        self.note.push(says.into());
        self
    }
}

const ME: &str = "crates/fennec-core/tests/limits_docs.rs";
const NUMBERS: &str = "crates/fennec-ql/tests/numbers.rs";
const QUOTAS: &str = "crates/fennec-http/tests/quotas.rs";
const QUOTA: &str = "crates/fennec-http/src/quota.rs";
const MCP: &str = "crates/fennec-http/tests/mcp.rs";
const CALLS: &str = "crates/fennec-core/tests/calls.rs";
const VECTOR: &str = "crates/fennec-core/src/vector.rs";
const STORE: &str = "crates/fennec-core/src/store.rs";
const UNWINDING: &str = "crates/fennec-core/src/engine/unwinding.rs";
const HTTP: &str = "crates/fennec-http/src/lib.rs";
const MCP_SRC: &str = "crates/fennec-http/src/mcp/mod.rs";

/// Every row's entry, in the page's order.
fn rows(problems: &mut Vec<String>) -> Vec<Row> {
    use fennec_core::{codec, engine, named, query, schema, store, value, vector};
    let n = |v: usize| v as f64;
    let mut src =
        |file: &str, anchor: &str, needle: &str| source_number(file, anchor, needle, problems);
    let ranking = match engine::MAX_NEAR_ROWS == engine::MAX_MATCH_ROWS {
        true => vec![n(engine::MAX_NEAR_ROWS)],
        false => vec![n(engine::MAX_NEAR_ROWS), n(engine::MAX_MATCH_ROWS)],
    };
    let share = src(QUOTA, "", "pub const SHARE: usize = ");
    // `1.5e308` as the page writes it, `1.5×10^308`.
    let sci = |e: String| {
        let (m, e) = e.split_once('e').unwrap();
        format!("{m}×10^{e}")
    };
    // An `f16`'s largest finite number, by its format: ten bits of
    // fraction and 15 the largest exponent.
    let f16_max = (2.0 - 2f64.powi(-10)) * 2f64.powi(15);
    let years = i64::MAX as f64 / (365.2425 * 86_400_000.0);
    let rows = vec![
        row(
            "near, match, fuse ranking (limit + offset)",
            ranking,
            "engine::MAX_NEAR_ROWS, MAX_MATCH_ROWS",
            Held::Driven(&[
                (ME, "the_rankings_take_10000_rows_and_refuse_one_more"),
                ("crates/fennec-ql/tests/e2e.rs", "near_beyond_row_cap_is_rejected"),
                (
                    "crates/fennec-core/tests/counts.rs",
                    "a_parameter_is_refused_as_the_number_written_is",
                ),
            ]),
        ),
        row(
            "words one word of a match ... fuzzy or prefix reaches",
            vec![n(fennec_core::text::MAX_EXPANSIONS)],
            "text::MAX_EXPANSIONS",
            Held::Driven(&[
                ("crates/fennec-core/src/text.rs", "a_word_reaching_too_many_is_refused"),
                (
                    "crates/fennec-core/tests/fuzzy.rs",
                    "a_word_reaching_too_many_is_refused_not_cut",
                ),
            ]),
        ),
        row(
            "edits fuzzy N takes",
            vec![query::MAX_FUZZY as f64],
            "query::MAX_FUZZY",
            Held::Driven(&[(
                "crates/fennec-core/tests/fuzzy.rs",
                "the_options_parse_in_either_order_and_are_refused_past_two",
            )]),
        ),
        row(
            "rerank candidates, fuse candidates a side",
            vec![n(engine::MAX_MATCH_ROWS)],
            "engine::MAX_MATCH_ROWS, the parser's bound",
            Held::Driven(&[
                (NUMBERS, "a_number_in_its_range_is_read_as_written"),
                (NUMBERS, "a_number_past_its_range_is_refused_naming_it"),
            ]),
        ),
        row(
            "fuse k",
            vec![query::MAX_FUSE_K as f64],
            "query::MAX_FUSE_K",
            Held::Driven(&[
                (NUMBERS, "a_number_in_its_range_is_read_as_written"),
                (NUMBERS, "a_number_past_its_range_is_refused_naming_it"),
                (
                    "crates/fennec-core/tests/fuse.rs",
                    "what_fuse_does_not_combine_with_is_refused",
                ),
            ]),
        ),
        row(
            "near … ef, @hnsw's ef_search and ef_construction, a snippet's words",
            vec![u32::MAX as f64],
            "u32::MAX, `Parser::int_in`'s",
            Held::Driven(&[
                (NUMBERS, "a_number_in_its_range_is_read_as_written"),
                (NUMBERS, "a_number_past_its_range_is_refused_naming_it"),
                (NUMBERS, "the_widest_beam_answers"),
            ]),
        ),
        row(
            "expression depth",
            vec![n(fennec_ql::MAX_EXPR_DEPTH)],
            "fennec_ql::MAX_EXPR_DEPTH",
            Held::Driven(&[("crates/fennec-ql/src/lib.rs", "expression_depth_is_bounded")]),
        ),
        row(
            "lookup chain depth",
            vec![n(query::MAX_LOOKUP_DEPTH)],
            "query::MAX_LOOKUP_DEPTH",
            Held::Driven(&[
                (ME, "a_lookup_chains_8_levels_and_refuses_a_ninth"),
                (
                    "crates/fennec-core/tests/lookup.rs",
                    "refuses_a_repeated_collection_and_a_chain_too_deep",
                ),
            ]),
        ),
        row(
            "fields of a lookup key",
            vec![n(schema::MAX_COMPOSITE_KEYS)],
            "schema::MAX_COMPOSITE_KEYS",
            Held::Driven(&[
                (ME, "an_index_and_a_lookup_key_take_8_fields_and_refuse_a_ninth"),
                ("crates/fennec-core/tests/composite_lookup.rs", "refusals_name_what_is_missing"),
            ]),
        ),
        row(
            "in (get …) values",
            vec![n(query::MAX_SUBQUERY_VALUES)],
            "query::MAX_SUBQUERY_VALUES",
            Held::Driven(&[
                ("crates/fennec-core/tests/subquery.rs", "a_set_past_the_bound_is_refused"),
                ("crates/fennec-core/tests/subquery.rs", "the_bound_counts_distinct_values"),
            ]),
        ),
        row(
            "a policy rule's in (get …) values",
            vec![n(named::MAX_CLAIM_VALUES)],
            "named::MAX_CLAIM_VALUES, fennec-http's access::MAX_CLAIM_VALUES",
            Held::Driven(&[
                ("crates/fennec-http/tests/reach.rs", "a_rule_s_list_past_the_bound_is_refused"),
                ("crates/fennec-http/tests/reach.rs", "a_rule_s_list_is_not_cut_at_its_first_rows"),
            ]),
        )
        .note(spaced(query::MAX_SUBQUERY_VALUES as u64)),
        row(
            "in $1 values",
            vec![n(query::MAX_SUBQUERY_VALUES)],
            "query::MAX_SUBQUERY_VALUES",
            Held::Driven(&[
                ("crates/fennec-core/tests/in_param.rs", "the_list_is_bounded_and_must_be_one"),
                ("crates/fennec-core/tests/in_param.rs", "the_refusal_never_hangs_on_the_rows"),
            ]),
        ),
        row(
            "in (get …) nesting",
            vec![n(query::MAX_SUBQUERY_DEPTH)],
            "query::MAX_SUBQUERY_DEPTH",
            Held::Driven(&[("crates/fennec-core/tests/subquery.rs", "nesting_is_bounded")]),
        ),
        row(
            "a require's reason (else '…')",
            vec![n(query::MAX_REASON)],
            "query::MAX_REASON",
            Held::Driven(&[
                (
                    "crates/fennec-core/tests/require.rs",
                    "a_require_takes_its_reason_after_its_count",
                ),
                ("crates/fennec-core/tests/require.rs", "a_reason_is_a_text_of_the_statement"),
            ]),
        ),
        row(
            "values of one facet",
            vec![n(query::MAX_FACET_VALUES)],
            "query::MAX_FACET_VALUES",
            Held::Driven(&[
                (ME, "a_facet_answers_10000_values_and_refuses_one_more"),
                (
                    "crates/fennec-core/tests/counts.rs",
                    "a_parameter_is_refused_as_the_number_written_is",
                ),
            ]),
        ),
        row(
            "ranges of one facet ... ranges",
            vec![n(query::MAX_FACET_VALUES)],
            "query::MAX_FACET_VALUES",
            Held::Driven(&[
                (ME, "a_range_facet_takes_10001_bounds_and_refuses_one_more"),
                ("crates/fennec-core/tests/facets.rs", "what_facets_cannot_answer_is_refused"),
            ]),
        )
        .note(format!("past {} bounds", spaced(query::MAX_FACET_VALUES as u64 + 1))),
        row(
            "json value nesting",
            vec![n(value::MAX_JSON_DEPTH)],
            "value::MAX_JSON_DEPTH",
            Held::Driven(&[
                ("crates/fennec-core/tests/json.rs", "nesting_and_paths_have_limits_that_refuse"),
                (
                    "crates/fennec-core/src/codec.rs",
                    "a_value_nests_no_deeper_than_json_may_be_written",
                ),
                ("crates/fennec-core/src/json.rs", "json_nests_no_deeper_than_the_limit"),
                (
                    "crates/fennec-core/tests/damage.rs",
                    "a_record_nesting_past_the_limit_is_refused_where_a_file_is_opened",
                ),
            ]),
        )
        .note(format!("a value nests more than {} levels deep", codec::MAX_DEPTH)),
        row(
            "list type nesting",
            vec![n(codec::MAX_DEPTH)],
            "codec::MAX_DEPTH",
            Held::Driven(&[
                (ME, "a_list_type_nests_64_levels_and_no_more"),
                (
                    "crates/fennec-core/tests/damage.rs",
                    "a_point_past_the_depth_a_type_nests_is_refused_where_a_file_is_opened_and_applied",
                ),
            ]),
        )
        .note(format!("a list type nests at most {} levels", codec::MAX_DEPTH)),
        row(
            "places a collection holds over its life",
            vec![n(schema::MAX_PLACES)],
            "schema::MAX_PLACES",
            Held::Driven(&[
                (
                    "crates/fennec-core/tests/compact_places.rs",
                    "a_collection_takes_no_place_past_its_bound",
                ),
                (
                    "crates/fennec-core/tests/compact_places.rs",
                    "a_collection_past_the_bound_takes_every_alter_but_an_add",
                ),
            ]),
        ),
        row(
            "path length",
            vec![n(value::MAX_PATH_KEYS)],
            "value::MAX_PATH_KEYS",
            Held::Driven(&[(
                "crates/fennec-core/tests/json.rs",
                "nesting_and_paths_have_limits_that_refuse",
            )]),
        ),
        row(
            "fields of an index over several fields",
            vec![n(schema::MAX_COMPOSITE_KEYS)],
            "schema::MAX_COMPOSITE_KEYS",
            Held::Driven(&[
                (ME, "an_index_and_a_lookup_key_take_8_fields_and_refuse_a_ninth"),
                ("crates/fennec-core/tests/composite.rs", "what_an_index_over_several_fields_refuses"),
                (
                    "crates/fennec-core/tests/composite.rs",
                    "a_schema_holding_an_index_over_too_few_or_too_many_fields_is_refused",
                ),
            ]),
        ),
        row(
            "spans the in lists over an index's parts make",
            vec![src("crates/fennec-core/src/engine.rs", "", "const MAX_SPANS: usize = ")],
            "engine.rs's MAX_SPANS",
            Held::Path(
                "past it the lists are tested row by row, the answer the same: how an index \
                 answers a filter, not a refusal",
                &[(ME, "in_lists_over_an_indexs_parts_make_256_spans_and_past_it_are_tested_row_by_row")],
            ),
        ),
        row(
            "session stack (fennec-server)",
            vec![src(HTTP, "", "const CONNECTION_STACK: usize = ")],
            "fennec-http's CONNECTION_STACK",
            Held::Typed(
                "a stack overflow is no error to refuse but the process's abort: the stack is \
                 sized for the deepest expression the parser takes (`MAX_EXPR_DEPTH`), which \
                 the test runs on a thread of this size in a debug build",
                &[("crates/fennec-ql/src/lib.rs", "expression_depth_is_bounded")],
            ),
        ),
        row(
            "segment size",
            vec![n(store::SEGMENT_MAX)],
            "store::SEGMENT_MAX",
            Held::Path(
                "a full segment is sealed and the next record opens another: how the store \
                 grows, not a refusal",
                &[(ME, "a_segment_is_sealed_once_it_holds_8_mib")],
            ),
        ),
        row(
            "document payload, segment offset",
            vec![n(store::MAX_PAYLOAD)],
            "store::MAX_PAYLOAD",
            Held::Driven(&[
                (STORE, "a_payload_of_u32_max_bytes_is_taken_and_one_more_refused"),
                (
                    UNWINDING,
                    "a_document_past_a_frames_length_is_refused_and_its_block_put_back",
                ),
            ]),
        )
        .note(format!("{} bytes", spaced(store::MAX_PAYLOAD as u64)))
        .note(format!("a document is at most {} bytes encoded", store::MAX_PAYLOAD)),
        row(
            "vector index node count",
            vec![n(vector::MAX_NODES)],
            "vector::MAX_NODES",
            Held::Driven(&[
                (VECTOR, "a_graph_takes_its_last_node_and_refuses_one_more"),
                (
                    UNWINDING,
                    "a_vector_past_a_graphs_nodes_is_refused_and_its_block_put_back",
                ),
            ]),
        )
        .note(format!("a vector index holds at most {} nodes", vector::MAX_NODES)),
        row(
            "@hnsw's m",
            vec![2.0, n(schema::MAX_M)],
            "2 and schema::MAX_M",
            Held::Driven(&[
                (NUMBERS, "a_number_in_its_range_is_read_as_written"),
                (NUMBERS, "a_number_past_its_range_is_refused_naming_it"),
                (
                    "crates/fennec-core/tests/damage.rs",
                    "a_schema_past_the_languages_ranges_is_refused_where_a_file_is_opened_and_applied",
                ),
            ]),
        ),
        row(
            "bulk insert batch",
            vec![
                src(VECTOR, "", "const MIN_BATCH: usize = "),
                src(VECTOR, "", "const MAX_BATCH: usize = "),
            ],
            "vector.rs's MIN_BATCH and MAX_BATCH",
            Held::Path(
                "the nodes linked together, a sixteenth of the graph held to the range: the \
                 page's clamp is of a batch's size, which links the same vectors either side, \
                 not of a number a statement writes",
                &[(VECTOR, "a_batch_is_a_sixteenth_of_the_graph_held_to_its_range")],
            ),
        ),
        row(
            "parallel build threshold",
            vec![src(VECTOR, "", "const PARALLEL_FROM: usize = ")],
            "vector.rs's PARALLEL_FROM",
            Held::Path(
                "below it each vector is linked in turn, from it in batches: a path, not a \
                 refusal",
                &[(VECTOR, "a_graph_links_in_batches_from_1024_nodes")],
            ),
        ),
        row(
            "gap in an externally given id",
            vec![src("crates/fennec-core/src/store.rs", "", "const MAX_DENSE_GAP: u64 = ")],
            "store.rs's MAX_DENSE_GAP",
            Held::Path(
                "within it the dense array grows, past it the id goes into the sparse map, \
                 found either way: a path, not a refusal",
                &[(
                    "crates/fennec-core/src/store.rs",
                    "a_gap_of_4096_grows_the_dense_array_and_one_more_does_not",
                )],
            ),
        ),
        row(
            "int",
            Vec::new(),
            "i64",
            Held::Driven(&[
                (
                    "crates/fennec-core/tests/int_range.rs",
                    "a_document_past_an_ints_range_is_refused_naming_the_field",
                ),
                (
                    "crates/fennec-core/tests/int_range.rs",
                    "a_parameter_or_a_literal_past_it_is_refused",
                ),
                (
                    "crates/fennec-core/tests/int_range.rs",
                    "a_lists_element_past_it_is_refused_and_its_ends_kept",
                ),
            ]),
        )
        .words("i64")
        .note(format!("{} to {}", i64::MIN, i64::MAX)),
        row(
            "float",
            Vec::new(),
            "f64",
            Held::Driven(&[(NUMBERS, "a_float_past_its_range_is_refused_naming_it")]),
        )
        .words("f64")
        .note(format!("±{}", sci(format!("{:e}", f64::MAX))))
        .note(sci(format!("{:e}", f64::from_bits(1)))),
        row(
            "timestamp",
            vec![1.0],
            "Value::Timestamp, an i64 of milliseconds",
            Held::Path(
                "a time is read to the millisecond, as a float to its nearest: a reading's \
                 precision, not a bound, and the type's ends are an i64's",
                &[(ME, "a_time_is_kept_to_the_millisecond")],
            ),
        )
        .words("i64")
        .note(format!("±{:.1}×10^8 years", years / 1e8)),
        Row {
            value: zone_year().map(|y| vec![y]),
            ..row(
                "bucket by a named zone",
                Vec::new(),
                "the zone table's first instant",
                Held::Driven(&[
                    (ME, "a_named_zone_answers_from_its_first_instant_and_refuses_the_one_before"),
                    ("crates/fennec-core/tests/zones.rs", "what_is_no_zone_is_refused"),
                ]),
            )
        },
        row(
            "vector<N> component",
            vec![f32::MAX as f64],
            "f32::MAX",
            Held::Driven(&[
                (
                    "crates/fennec-core/tests/vector_range.rs",
                    "the_ends_of_an_f32_are_taken_and_below_them_is_zero",
                ),
                (
                    "crates/fennec-core/tests/vector_range.rs",
                    "a_component_past_an_f32_is_refused_naming_the_field_and_the_element",
                ),
                (
                    "crates/fennec-core/tests/vector_range.rs",
                    "near_refuses_a_query_vector_past_an_f32",
                ),
                (
                    "crates/fennec-core/tests/vector_range.rs",
                    "rerank_refuses_a_query_vector_past_an_f32",
                ),
            ]),
        )
        .note(sci(format!("{:e}", f32::MAX))),
        row(
            "vector<N, f16>",
            vec![f16_max],
            "an f16's largest finite number",
            Held::Driven(&[(
                "crates/fennec-core/tests/vector_range.rs",
                "an_f16_field_holds_65504_at_most",
            )]),
        )
        // The least that rounds to its infinity: half a step past it.
        .note(spaced(f16_max as u64 + 16)),
        row(
            "geo point",
            geo_ends(),
            "geo::valid's ends",
            Held::Driven(&[
                (ME, "a_point_takes_180_and_90_and_refuses_past_them"),
                ("crates/fennec-core/tests/geo.rs", "a_point_is_written_and_read_as_lon_lat"),
            ]),
        ),
        row(
            "vector<N> and sparse<N> dimension",
            vec![n(schema::MAX_DIM)],
            "schema::MAX_DIM",
            Held::Driven(&[
                (NUMBERS, "a_number_in_its_range_is_read_as_written"),
                (NUMBERS, "a_number_past_its_range_is_refused_naming_it"),
                (NUMBERS, "whatever_a_write_takes_an_open_reads_back"),
            ]),
        ),
        row(
            "change ring",
            vec![n(fennec_core::changes::DEFAULT_CAPACITY)],
            "changes::DEFAULT_CAPACITY",
            Held::Path(
                "how far behind a subscriber may fall and still be told the changes: past it \
                 it is sent the whole shape again, not refused",
                &[
                    (
                        "crates/fennec-core/src/changes.rs",
                        "overflow_raises_horizon_and_forces_reseed",
                    ),
                    ("crates/fennec-core/tests/changes.rs", "overflow_forces_reseed"),
                ],
            ),
        ),
        row(
            "concurrent subscriptions",
            vec![src(HTTP, "impl Default for Config", "max_streams: ")],
            "fennec-http's Config::default",
            Held::Driven(&[(
                "crates/fennec-http/tests/changes.rs",
                "stream_cap_is_separate_from_connection_cap",
            )]),
        ),
        row(
            "a tenant's file (--dir)",
            Vec::new(),
            "Quota::default",
            Held::Driven(&[
                (
                    QUOTAS,
                    "a_file_quota_refuses_the_write_that_would_pass_it_and_takes_deletes",
                ),
                (
                    "crates/fennec-core/tests/limit.rs",
                    "a_block_past_the_limit_is_put_back_and_deletes_still_land",
                ),
            ]),
        )
        .words("none unless set"),
        row(
            "a tenant's rows",
            Vec::new(),
            "Quota::default",
            Held::Driven(&[
                (QUOTAS, "a_rows_quota_counts_what_a_write_leaves"),
                (
                    "crates/fennec-core/tests/limit.rs",
                    "a_rows_limit_counts_what_the_block_leaves",
                ),
            ]),
        )
        .words("none unless set"),
        Row {
            said: vec![("a quarter: quota::SHARE", 4.0, share)],
            ..row(
                "a tenant's streams",
                Vec::new(),
                "quota::shared",
                Held::Driven(&[
                    (QUOTAS, "a_tenant_at_its_streams_quota_leaves_the_others_answered"),
                    (QUOTA, "a_tenants_waits_default_to_a_share_of_the_nodes_connections"),
                ]),
            )
            .words("a quarter of --max-connections unless set")
        },
        Row {
            said: vec![("a quarter: quota::SHARE", 4.0, share)],
            ..row(
                "a tenant's held writes",
                Vec::new(),
                "quota::shared",
                Held::Driven(&[
                    (QUOTAS, "a_tenant_at_its_held_quota_still_opens_its_streams"),
                    (QUOTA, "a_tenants_waits_default_to_a_share_of_the_nodes_connections"),
                ]),
            )
            .words("a quarter of --max-connections unless set")
        },
        row(
            "a tenant's requests in flight",
            Vec::new(),
            "Quota::default",
            Held::Driven(&[
                (QUOTAS, "a_tenant_at_its_requests_quota_leaves_the_others_answered"),
                (QUOTA, "requests_past_the_quota_are_refused_and_slots_given_back"),
            ]),
        )
        .words("none unless set"),
        row(
            "a tenant's rows written a second",
            Vec::new(),
            "Quota::default",
            Held::Driven(&[
                (QUOTAS, "a_rate_of_rows_charges_what_was_written"),
                (QUOTA, "a_rate_lets_its_second_through_and_then_refuses"),
            ]),
        )
        .words("none unless set"),
        row(
            "a tenant's bytes written a second",
            Vec::new(),
            "Quota::default",
            Held::Driven(&[
                (QUOTAS, "a_write_bytes_rate_charges_what_landed"),
                (QUOTA, "bytes_a_second_are_charged_after_and_waited_out"),
            ]),
        )
        .words("none unless set"),
        row(
            "rows an MCP tool answers",
            vec![src(MCP_SRC, "fn default() -> Options", "max_rows: ")],
            "mcp::Options::default",
            Held::Driven(&[(MCP, "an_answer_past_its_bound_is_refused_never_cut")]),
        ),
        row(
            "bytes an MCP tool answers",
            vec![src(MCP_SRC, "fn default() -> Options", "max_bytes: ")],
            "mcp::Options::default",
            Held::Driven(&[
                (MCP, "an_answer_past_its_bound_is_refused_never_cut"),
                (MCP, "a_call_tool_past_its_bound_is_refused_and_its_writes_put_back"),
            ]),
        ),
        row(
            "MCP tool calls a second",
            vec![src(MCP_SRC, "fn default() -> Options", "calls_per_sec: ")],
            "mcp::Options::default",
            Held::Driven(&[
                (
                    MCP_SRC,
                    "a_callers_rate_is_its_own_in_each_database_and_a_full_map_forgives_no_one",
                ),
                (MCP, "a_caller_past_its_rate_is_told_when_to_call_again"),
            ]),
        ),
        row(
            "rows a named statement's call examines",
            vec![n(named::CALL_MAX_ROWS)],
            "named::CALL_MAX_ROWS",
            Held::Driven(&[
                (CALLS, "a_call_past_its_rows_is_put_back"),
                (CALLS, "near_match_and_a_lookups_children_count_against_a_calls_rows"),
            ]),
        ),
        row(
            "named statement calls a second",
            vec![src("crates/fennec-http/src/calls.rs", "", "pub const CALL_RATE: u32 = ")],
            "fennec-http's calls::CALL_RATE",
            Held::Driven(&[
                (
                    "crates/fennec-http/tests/named.rs",
                    "a_caller_past_the_rate_waits_and_a_call_past_its_rows_is_put_back",
                ),
                ("crates/fennec-http/src/rate.rs", "a_caller_is_counted_by_its_tenant"),
            ]),
        ),
        row(
            "requests a second from one scoped caller",
            vec![src("crates/fennec-http/src/rate.rs", "", "pub const REQUEST_RATE: u32 = ")],
            "fennec-http's rate::REQUEST_RATE",
            Held::Driven(&[
                (
                    "crates/fennec-http/tests/request_rate.rs",
                    "a_caller_past_the_rate_is_told_its_wait_and_nothing_of_it_runs",
                ),
                ("crates/fennec-http/src/rate.rs", "the_wait_named_is_the_wait"),
            ]),
        ),
        row(
            "values of a list claim a named statement's call reads",
            vec![n(named::MAX_CLAIM_VALUES)],
            "named::MAX_CLAIM_VALUES",
            Held::Driven(&[(CALLS, "a_list_claim_past_its_bound_is_refused")]),
        ),
    ];
    rows
}

/// What is wrong between `page`'s table and the entries, each said whole.
fn problems(page: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let rows = rows(&mut problems);
    let table = table(page);
    if table.is_empty() {
        problems.push("limits.html: no rows found after `<h2>Fixed limits</h2>`".into());
    }
    for (i, r) in rows.iter().enumerate() {
        if rows[..i].iter().any(|o| o.key == r.key) {
            problems.push(format!("entry `{}` is in the list twice", r.key));
        }
        if !table.iter().any(|(key, ..)| key == r.key) {
            problems.push(format!(
                "entry `{}` names no row of limits.html: the row was removed or reworded, and \
                 the list follows the page",
                r.key
            ));
        }
    }
    for (key, value, note) in &table {
        let Some(r) = rows.iter().find(|r| r.key == key) else {
            problems.push(format!(
                "limits.html's row `{key}` has no entry in {ME}: name what holds it -- a test \
                 taking the bound and refusing one past it, one at the type's level, or one \
                 showing the path either side -- or write it"
            ));
            continue;
        };
        let said = numbers(value);
        if let Some(code) = &r.value {
            let same = said.len() == code.len()
                && said
                    .iter()
                    .zip(code)
                    .all(|((p, step), c)| (p - c).abs() <= step / 2.0);
            if !same {
                let page: Vec<f64> = said.iter().map(|(p, _)| *p).collect();
                problems.push(format!(
                    "row `{key}`: the page says {page:?} ({:?}), the code holds {code:?} ({})",
                    plain(value),
                    r.from
                ));
            }
        }
        if !plain(value).contains(r.words) {
            problems.push(format!(
                "row `{key}`: the page says {:?}, not {:?} ({})",
                plain(value),
                r.words,
                r.from
            ));
        }
        for says in &r.note {
            if !plain(note).contains(says.as_str()) {
                problems.push(format!("row `{key}`: its note does not say {says:?}"));
            }
        }
        for (what, page, code) in &r.said {
            if page != code {
                problems.push(format!(
                    "row `{key}`: {what} is {code}, where the page says {page}"
                ));
            }
        }
        let (tests, why) = match &r.held {
            Held::Driven(t) => (*t, "-"),
            Held::Typed(why, t) | Held::Path(why, t) => (*t, *why),
        };
        if tests.is_empty() || why.is_empty() {
            problems.push(format!(
                "row `{key}`: held by no test, or with no reason given"
            ));
        }
        for (file, name) in tests {
            match std::fs::read_to_string(format!("{ROOT}{file}")) {
                Ok(text) => {
                    if let Err(why) = held_by(&text, name) {
                        problems.push(format!("row `{key}`: {file}: {why}"));
                    }
                }
                Err(e) => problems.push(format!("row `{key}`: {file}: {e}")),
            }
        }
    }
    problems
}

/// The builds of CI's `make test`, `(os, arch)`, each crate with its
/// default features: a named test must run in one of them at least.
const BUILDS: [(&str, &str); 3] = [
    ("linux", "x86_64"),
    ("linux", "aarch64"),
    ("macos", "aarch64"),
];

/// fennec-core's default features: every one but `statements`, which a
/// crate depending on it asks for. Any other is taken as off.
const FEATURES: &[&str] = &[
    "std-fs",
    "full",
    "indexes",
    "vector",
    "text",
    "sparse",
    "sorted",
    "composite",
    "language",
    "geo",
    "nocase",
    "facets",
    "highlight",
    "analytics",
    "collate",
];

/// Whether `cfg` predicate `p` holds in build `(os, arch)`: `Err` names
/// what it cannot tell, a predicate never taken for true unread.
fn holds(p: &str, (os, arch): (&str, &str)) -> std::result::Result<bool, String> {
    let p = p.trim();
    let list = |inner: &str| -> std::result::Result<Vec<bool>, String> {
        let (mut out, mut depth, mut from) = (Vec::new(), 0, 0);
        for (i, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(holds(&inner[from..i], (os, arch))?);
                    from = i + 1;
                }
                _ => {}
            }
        }
        if !inner[from..].trim().is_empty() {
            out.push(holds(&inner[from..], (os, arch))?);
        }
        Ok(out)
    };
    let call = |f: &str| {
        p.strip_prefix(f)
            .and_then(|r| r.trim_start().strip_prefix('(')?.strip_suffix(')'))
    };
    if let Some(inner) = call("all") {
        return Ok(list(inner)?.into_iter().all(|b| b));
    }
    if let Some(inner) = call("any") {
        return Ok(list(inner)?.into_iter().any(|b| b));
    }
    if let Some(inner) = call("not") {
        return holds(inner, (os, arch)).map(|b| !b);
    }
    let atom = match p.split_once('=') {
        Some((k, v)) => (k.trim(), Some(v.trim().trim_matches('"'))),
        None => (p, None),
    };
    Ok(match atom {
        ("test" | "unix", None) => true,
        ("windows", None) => false,
        ("target_os", Some(v)) => v == os,
        ("target_arch", Some(v)) => v == arch,
        ("target_family", Some(v)) => v == "unix",
        ("target_pointer_width", Some(v)) => v == "64",
        ("feature", Some(f)) => FEATURES.contains(&f),
        _ => return Err(format!("`{p}` is no predicate this test reads")),
    })
}

/// Whether a test is found, or what is wrong.
type Found = std::result::Result<(), String>;

/// Whether `text`, a Rust file, holds `name` as a test CI runs: `fn
/// <name>(` outside its comments and literals, `#[test]` among the
/// attributes right before it and neither `ignore` nor `cfg_attr`, nothing
/// around it but `mod`s, and every `cfg` on it, on those `mod`s and on the
/// file met by one of CI's builds ([`BUILDS`]). Found as text alone, a
/// helper of the name, a test
/// commented out, `#[ignore]`d or `cfg`'d out of every build passed for
/// one. What is wrong, otherwise.
fn held_by(text: &str, name: &str) -> Found {
    let code = blanked(text);
    let needle = format!("fn {name}(");
    let mut first = None;
    let mut from = 0;
    while let Some(at) = code[from..].find(&needle).map(|i| from + i) {
        from = at + needle.len();
        if code[..at].ends_with(ident) {
            continue;
        }
        match runs(text, &code, at) {
            Ok(()) => return Ok(()),
            Err(why) => first = first.or(Some(why)),
        }
    }
    Err(first.unwrap_or_else(|| format!("holds no `fn {name}`, its comments aside")))
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the `fn` at `at` of `text` (`code` blanked) runs as a test.
fn runs(text: &str, code: &str, at: usize) -> Found {
    let mut attrs = attributes(text, code, at);
    if !attrs.iter().any(|a| a == "test") {
        return Err("its `fn` has no `#[test]` right before it: a helper, no test".into());
    }
    if attrs
        .iter()
        .any(|a| a.starts_with("ignore") || a.starts_with("cfg_attr"))
    {
        return Err("its test is `#[ignore]`d, or may be".into());
    }
    let mut open = Vec::new();
    for (i, c) in code[..at].char_indices() {
        match c {
            '{' => open.push(i),
            '}' => drop(open.pop()),
            _ => {}
        }
    }
    for brace in open {
        let Some(item) = module_at(&code[..brace]) else {
            return Err("its test is inside something not a `mod`".into());
        };
        attrs.extend(attributes(text, code, item));
    }
    // The file's own: `#![cfg(...)]` at its head.
    let mut p = code.len() - code.trim_start().len();
    while code[p..].starts_with("#![") {
        let Some(end) = closing(&code[p + 3..]) else {
            return Err("an attribute not closed".into());
        };
        attrs.push(fold(&text[p + 3..p + 3 + end]));
        p += 3 + end + 1;
        p += code[p..].len() - code[p..].trim_start().len();
    }
    let cfgs: Vec<&str> = (attrs.iter())
        .filter_map(|a| a.strip_prefix("cfg(")?.strip_suffix(')'))
        .collect();
    for build in BUILDS {
        let mut met = true;
        for p in &cfgs {
            met &= holds(p, build)?;
        }
        if met {
            return Ok(());
        }
    }
    Err(format!(
        "`cfg`'d out of every build CI's `make test` makes: {cfgs:?}"
    ))
}

/// Where the `mod` whose block opens right after `head` starts, its
/// visibility included; `None` where the block is no module's.
fn module_at(head: &str) -> Option<usize> {
    let head = head.trim_end();
    let named = head.trim_end_matches(ident);
    let before = named.trim_end().strip_suffix("mod")?;
    if named.len() == head.len() || before.ends_with(ident) {
        return None;
    }
    let before = before.trim_end();
    // `pub`, `pub(crate)`, `pub(super)`, `pub(in path)`.
    let vis = match before.strip_suffix(')') {
        Some(b) => b.rfind('(').map(|o| b[..o].trim_end()),
        None => Some(before),
    };
    Some(match vis.and_then(|v| v.strip_suffix("pub")) {
        Some(v) if !v.ends_with(ident) => v.len(),
        _ => before.len(),
    })
}

/// The outer attributes right before the item at `at` -- found in `code`,
/// read in `text` -- each as written inside its `#[...]`, spaces folded.
fn attributes(text: &str, code: &str, at: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut end = code[..at].trim_end().len();
    while code[..end].ends_with(']') {
        let mut depth = 0;
        let mut open = None;
        for (i, c) in code[..end - 1].char_indices().rev() {
            match c {
                ']' => depth += 1,
                '[' if depth == 0 => {
                    open = Some(i);
                    break;
                }
                '[' => depth -= 1,
                _ => {}
            }
        }
        let Some(open) = open.filter(|&o| code[..o].ends_with('#') && !code[..o].ends_with("#!"))
        else {
            break;
        };
        out.push(fold(&text[open + 1..end - 1]));
        end = code[..open - 1].trim_end().len();
    }
    out
}

/// Where the bracket opened before `after` closes in it.
fn closing(after: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in after.char_indices() {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => return Some(i),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// `s`'s whitespace folded to one space, and none inside a bracket's edges.
fn fold(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// `text` with its comments, its strings' and its characters' contents
/// made spaces, every byte where it was: what is left is code, whose
/// braces and brackets are the code's.
fn blanked(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |e| i + e);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let (mut depth, mut j) = (1, i + 2);
                while j < b.len() && depth > 0 {
                    if b[j..].starts_with(b"/*") {
                        depth += 1;
                        j += 2;
                    } else if b[j..].starts_with(b"*/") {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            b'r' | b'b' if i == 0 || !word(b[i - 1]) => {
                // `r"..."`, `r#"..."#`, `b"..."`, `br#"..."#`: raw ones end
                // at a quote and as many hashes as they began with.
                let mut j = i + 1;
                if b[i] == b'b' && b.get(j) == Some(&b'r') {
                    j += 1;
                }
                let raw = b[i] == b'r' || j == i + 2;
                let hashes = b[j..].iter().take_while(|&&c| c == b'#').count();
                if b.get(j + hashes) != Some(&b'"') || (!raw && hashes > 0) {
                    i += 1;
                    continue;
                }
                let from = j + hashes + 1;
                let end = match raw {
                    true => {
                        let close = [b"\"".as_slice(), &vec![b'#'; hashes]].concat();
                        b[from..]
                            .windows(close.len())
                            .position(|w| w == close)
                            .map_or(b.len(), |e| from + e)
                    }
                    false => string_end(b, from),
                };
                blank(&mut out, from, end);
                i = end + 1;
            }
            b'"' => {
                let end = string_end(b, i + 1);
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            // A character, as opposed to a lifetime or a label: `'x'`,
            // `'\n'`, `'\u{..}'`.
            b'\'' => {
                let end = match b.get(i + 1) {
                    Some(b'\\') => (b.get(i + 3..))
                        .and_then(|r| r.iter().position(|&c| c == b'\''))
                        .map(|e| i + 3 + e),
                    Some(_) => {
                        let len = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                        (b.get(i + 1 + len) == Some(&b'\'')).then_some(i + 1 + len)
                    }
                    None => None,
                };
                match end {
                    Some(end) => {
                        blank(&mut out, i + 1, end);
                        i = end + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Where the string whose text begins at `from` ends: its closing quote,
/// an escaped one passed over.
fn string_end(b: &[u8], from: usize) -> usize {
    let mut j = from;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j,
            _ => j += 1,
        }
    }
    b.len()
}

/// The fixed limits' rows: each one's first cell as its key, its second
/// cell and its third, as written.
fn table(page: &str) -> Vec<(String, String, String)> {
    let Some(at) = page.find("<h2>Fixed limits</h2>") else {
        return Vec::new();
    };
    let page = &page[at..];
    let (Some(from), Some(to)) = (page.find("<tbody>"), page.find("</tbody>")) else {
        return Vec::new();
    };
    page[from..to]
        .split("<tr>")
        .skip(1)
        .map(|tr| {
            let cells: Vec<String> = tr
                .split("<td")
                .skip(1)
                .map(|td| {
                    let td = &td[td.find('>').map_or(0, |i| i + 1)..];
                    td[..td.find("</td>").unwrap_or(td.len())].to_string()
                })
                .collect();
            let cell = |i: usize| cells.get(i).cloned().unwrap_or_default();
            (plain(&cell(0)), cell(1), cell(2))
        })
        .collect()
}

/// A cell's text: a superscript as `^`, tags stripped, entities read,
/// whitespace folded.
fn plain(html: &str) -> String {
    let html = html.replace("<sup>", "^").replace("</sup>", "");
    let mut text = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            c if !tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The numbers a value cell says, in order, each with the step its last
/// digit stands for: `10 000` is 10000 to the unit, `3.4×10^38` is 3.4e38
/// to 1e37, `2^32` and `8 MiB` are what they say. A number joined to a
/// letter before it -- the 64 of `i64` -- is a type's name, no number.
fn numbers(cell: &str) -> Vec<(f64, f64)> {
    let c: Vec<char> = plain(cell).chars().collect();
    let digits = |from: usize, n: usize| {
        (from..from + n).all(|k| c.get(k).is_some_and(char::is_ascii_digit))
    };
    let int = |i: &mut usize| {
        let negative = c.get(*i) == Some(&'-');
        *i += negative as usize;
        let mut v = 0i32;
        while let Some(d) = c.get(*i).and_then(|d| d.to_digit(10)) {
            v = v * 10 + d as i32;
            *i += 1;
        }
        if negative {
            -v
        } else {
            v
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let joined = i > 0 && (c[i - 1].is_alphanumeric() || matches!(c[i - 1], '^' | '.'));
        if !c[i].is_ascii_digit() || joined {
            i += 1;
            continue;
        }
        let mut whole = String::new();
        while i < c.len() && c[i].is_ascii_digit() {
            whole.push(c[i]);
            i += 1;
        }
        // `10 000`: a group of three after a space, no digit after it.
        while c.get(i) == Some(&' ') && digits(i + 1, 3) && !digits(i + 4, 1) {
            whole.extend(&c[i + 1..i + 4]);
            i += 4;
        }
        let mut fraction = String::new();
        if c.get(i) == Some(&'.') && digits(i + 1, 1) {
            i += 1;
            while i < c.len() && c[i].is_ascii_digit() {
                fraction.push(c[i]);
                i += 1;
            }
        }
        let mut value: f64 = format!("{whole}.{fraction}0").parse().unwrap();
        let mut step = 10f64.powi(-(fraction.len() as i32));
        if c.get(i) == Some(&'^') {
            i += 1;
            value = value.powi(int(&mut i));
            step = 1.0;
        } else if c.get(i) == Some(&'×') && c[i + 1..].starts_with(&['1', '0', '^']) {
            i += 4;
            let scale = 10f64.powi(int(&mut i));
            value *= scale;
            step *= scale;
        }
        let rest: String = c[i..].iter().take(5).collect();
        for (unit, scale) in [(" GiB", 30), (" MiB", 20), (" KiB", 10), ("K", 10)] {
            let after = rest.strip_prefix(unit).map(|r| r.chars().next());
            if after.is_some_and(|c| !c.is_some_and(char::is_alphanumeric)) {
                value *= 2f64.powi(scale);
                step *= 2f64.powi(scale);
            }
        }
        out.push((value, step));
    }
    out
}

/// The number written after `needle` -- after `anchor` where one is given --
/// in a file under the repository's root, as Rust writes it: `100`,
/// `10_000`, `64 << 10`. A constant of a crate this one cannot call, or a
/// private one, read where it is defined.
fn source_number(file: &str, anchor: &str, needle: &str, problems: &mut Vec<String>) -> f64 {
    let text = std::fs::read_to_string(format!("{ROOT}{file}")).unwrap_or_default();
    let from = text.find(anchor).unwrap_or(usize::MAX);
    let Some(at) = text
        .get(from..)
        .and_then(|t| t.find(needle))
        .map(|at| from + at)
    else {
        problems.push(format!("{file}: no `{needle}` after `{anchor}`"));
        return f64::NAN;
    };
    let expr = &text[at + needle.len()..];
    let expr = expr[..expr.find([',', ';', '\n']).unwrap_or(expr.len())].replace('_', "");
    let read = |s: &str| s.trim().parse::<u64>().ok();
    let v = match expr.split_once("<<") {
        Some((a, b)) => read(a).zip(read(b)).map(|(a, b)| a << b),
        None => read(&expr),
    };
    v.map_or_else(
        || {
            problems.push(format!("{file}: `{needle}{expr}` is no number this reads"));
            f64::NAN
        },
        |v| v as f64,
    )
}

/// `n` as the page writes it: `10 001`, `100 000`.
fn spaced(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(d);
    }
    out
}

/// The longitude and latitude a point takes at most, as `geo::valid`
/// draws them: each taken, and the next float past it not.
fn geo_ends() -> Vec<f64> {
    use fennec_core::geo::valid;
    let (lon, lat) = (180.0f64, 90.0f64);
    let ends = valid(lon, lat)
        && valid(-lon, -lat)
        && !valid(lon.next_up(), 0.0)
        && !valid(0.0, lat.next_up())
        && !valid((-lon).next_down(), 0.0)
        && !valid(0.0, (-lat).next_down());
    match ends {
        true => vec![lon, lat],
        false => Vec::new(),
    }
}

/// The year a named zone's rules start in, as a refusal before them says
/// it; `None` in a build without the zones.
fn zone_year() -> Option<f64> {
    #[cfg(feature = "analytics")]
    {
        let from = zone_from();
        Some(from[..4].parse().unwrap())
    }
    #[cfg(not(feature = "analytics"))]
    None
}

/// The first instant `Europe/Berlin` is answered for, as its refusal of
/// 1970 writes it.
#[cfg(feature = "analytics")]
fn zone_from() -> String {
    let mut db = Database::new();
    run(
        &mut db,
        "create collection ev (at timestamp); put ev {at: 0}",
    )
    .unwrap();
    let e = ask(
        &mut db,
        "get ev select bucket(at, 1d, 'Europe/Berlin') as d",
        &[],
    )
    .unwrap_err();
    let m = e.to_string();
    let rest = &m[m.find("rules from ").expect(&m) + "rules from ".len()..];
    rest[..rest.find(' ').expect(&m)].to_string()
}

#[test]
fn every_row_of_the_page_is_held() {
    let problems = problems(PAGE);
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    let mut none = Vec::new();
    assert_eq!(rows(&mut none).len(), table(PAGE).len());
}

/// What the mapping tells, made to happen: a row added to the page with
/// no entry, a row taken out of it, and a number changed in it.
#[test]
fn a_row_added_removed_or_changed_is_told() {
    let added = PAGE.replacen(
        "<tbody>\n",
        "<tbody>\n<tr><td>frobs a <code>widget</code> holds</td><td class=\"n\">7</td><td>refused</td></tr>\n",
        1,
    );
    let said = problems(&added);
    assert!(
        said.len() == 1 && said[0].contains("row `frobs a widget holds` has no entry"),
        "{said:#?}"
    );
    let ring = PAGE.lines().find(|l| l.contains(">change ring<")).unwrap();
    let said = problems(&PAGE.replacen(ring, "", 1));
    assert!(
        said.len() == 1 && said[0].contains("entry `change ring` names no row"),
        "{said:#?}"
    );
    let said = problems(&PAGE.replacen(
        "<td class=\"n\">8 MiB</td><td>sealed",
        "<td class=\"n\">16 MiB</td><td>sealed",
        1,
    ));
    assert!(
        said.len() == 1
            && said[0].contains("row `segment size`: the page says [16777216.0]")
            && said[0].contains("the code holds [8388608.0]"),
        "{said:#?}"
    );
}

/// A test an entry names is found only as one CI runs: a helper of its
/// name, a test commented out or in a string, `#[ignore]`d, `cfg`'d out
/// of every build CI makes -- on itself, a `mod` around it or its file --
/// or inside something not a `mod` is told, each saying why; a test among
/// braces in characters, raw strings and lifetimes, in a `mod` CI builds,
/// is found.
#[test]
fn a_helper_or_a_test_ci_does_not_run_is_no_test() {
    let wrong = [
        ("fn held() {}", "a helper, no test"),
        ("// #[test]\n// fn held() {}\nfn other() {}", "holds no `fn held`"),
        ("/* #[test]\nfn held() {} /* nested */ */", "holds no `fn held`"),
        ("const S: &str = \"#[test]\\\"fn held() {}\";", "holds no `fn held`"),
        ("#[test]\nfn hold() {}\n#[test]\nfn unheld() {}", "holds no `fn held`"),
        ("#[test]\n#[ignore]\nfn held() {}", "`#[ignore]`d"),
        ("#[test]\n#[cfg_attr(unix, ignore = \"slow\")]\nfn held() {}", "`#[ignore]`d"),
        ("#[cfg(windows)]\n#[test]\nfn held() {}", "out of every build"),
        ("#[cfg(feature = \"statements\")]\n#[test]\nfn held() {}", "out of every build"),
        (
            "#[cfg(all(target_os = \"linux\", target_arch = \"aarch64\", not(unix)))]\n#[test]\nfn held() {}",
            "out of every build",
        ),
        (
            "#[cfg(target_arch = \"wasm32\")]\nmod m {\n    #[test]\n    fn held() {}\n}",
            "out of every build",
        ),
        ("#![cfg(any())]\n#[test]\nfn held() {}", "out of every build"),
        ("fn outer() {\n    #[test]\n    fn held() {}\n}", "not a `mod`"),
        ("impl S {\n    #[test]\n    fn held() {}\n}", "not a `mod`"),
        ("#[cfg(miri)]\n#[test]\nfn held() {}", "`miri` is no predicate"),
    ];
    for (text, says) in wrong {
        match held_by(text, "held") {
            Err(why) => assert!(why.contains(says), "{text:?}: {why}"),
            Ok(()) => panic!("{text:?}: found"),
        }
    }
    let right = [
        "#[test]\nfn held() {}",
        "/// Its doc.\n#[test] // why\n#[should_panic]\nfn held() {}",
        "fn f<'a>(x: &'a str) -> char {\n    let _ = (x, b'{', r#\"{\"#, \"}\\\"{\");\n    '{'\n}\n\
         #[test]\nfn held() {}",
        "#![cfg(unix)]\n//! A file.\n#[cfg(test)]\npub(crate) mod tests {\n    use super::*;\n\n    \
         #[cfg(all(feature = \"vector\", not(target_os = \"linux\")))]\n    #[test]\n    fn held() {}\n}",
        "mod a {\n    fn held() {}\n}\nmod b {\n    #[test]\n    fn held() {}\n}",
    ];
    for text in right {
        assert_eq!(held_by(text, "held"), Ok(()), "{text:?}");
    }
}

#[test]
fn a_value_cell_reads_as_the_numbers_it_says() {
    let read = |cell: &str| {
        numbers(cell)
            .into_iter()
            .map(|(v, _)| v)
            .collect::<Vec<_>>()
    };
    assert_eq!(read("<b>10 000 rows</b>"), [10_000.0]);
    assert_eq!(read("4 294 967 295"), [4_294_967_295.0]);
    assert_eq!(read("2<sup>32</sup>"), [2f64.powi(32)]);
    assert_eq!(read("2–32 767"), [2.0, 32_767.0]);
    assert_eq!(read("8 MiB"), [8.0 * 1024.0 * 1024.0]);
    assert_eq!(read("<b>64K</b>"), [65_536.0]);
    assert_eq!(read("1 ms, i64"), [1.0]);
    assert_eq!(read("i64"), [] as [f64; 0]);
    assert_eq!(read("±180°, ±90°"), [180.0, 90.0]);
    let [(v, step)] = numbers("±3.4×10<sup>38</sup>")[..] else {
        panic!("one number")
    };
    assert!(
        v == 3.4e38 && (step / 1e37 - 1.0).abs() < 1e-9,
        "{v} {step}"
    );
    assert_eq!(read("4 096"), [4096.0]);
    assert_eq!(spaced(10_001), "10 001");
    assert_eq!(spaced(100), "100");
}

// ---------------------------------------------------------------- rows

fn run(db: &mut Database, text: &str) -> Result<Response> {
    let mut last = Response::Affected(0);
    for t in text.split(';').filter(|t| !t.trim().is_empty()) {
        for s in fennec_ql::parse_for(db, t)? {
            last = db.execute_with(&s, &[])?;
        }
    }
    Ok(last)
}

/// One statement, with its parameters: refused where it is read or where
/// it runs.
fn ask(db: &mut Database, text: &str, params: &[Value]) -> Result<Response> {
    let s = fennec_ql::parse_one_for(db, text)?;
    db.execute_with(&s, params)
}

fn count(r: &Response) -> usize {
    r.rows().map_or(0, |rs| rs.rows.len())
}

/// `ask`'s refusal, which must be a query error saying `says`: a parse
/// error is one, at its position.
#[track_caller]
fn refused(db: &mut Database, text: &str, params: &[Value], says: &str) {
    match ask(db, text, params) {
        Err(Error::Query(m)) => assert!(m.contains(says), "{text}: {m}"),
        Err(e) => panic!("{text}: not a query error: {e:?}"),
        Ok(r) => panic!("{text}: answered {} rows", count(&r)),
    }
}

/// `near`, `match` and `fuse` answer `limit + offset` up to 10 000 rows,
/// written or a parameter's, and refuse one more -- answering no row, not
/// the first 10 000.
#[cfg(all(feature = "vector", feature = "text"))]
#[test]
fn the_rankings_take_10000_rows_and_refuse_one_more() {
    use fennec_core::engine::{MAX_MATCH_ROWS, MAX_NEAR_ROWS};
    assert_eq!(MAX_NEAR_ROWS, MAX_MATCH_ROWS);
    let cap = MAX_NEAR_ROWS;
    let mut db = Database::new();
    run(
        &mut db,
        "create collection r (body text @text, v vector<2> @hnsw);
         put r [{body: \"fox\", v: [1, 0]}, {body: \"red fox\", v: [0, 1]}, \
         {body: \"fox den\", v: [1, 1]}]",
    )
    .unwrap();
    for ranking in [
        "near v [1, 0]",
        "match body \"fox\"",
        "match body \"fox\" near v [1, 0] fuse",
    ] {
        let at = Value::Int(cap as i64);
        for (page, params) in [
            (format!("limit {cap}"), vec![]),
            (format!("limit 1 offset {}", cap - 1), vec![]),
            ("limit $1".to_string(), vec![at]),
            (String::new(), vec![]),
        ] {
            let text = format!("get r select id {ranking} {page}");
            let r = ask(&mut db, &text, &params).unwrap_or_else(|e| panic!("{text}: {e}"));
            let want = if page.contains("offset") { 0 } else { 3 };
            assert_eq!(count(&r), want, "{text}");
        }
        let past = Value::Int(cap as i64 + 1);
        for (page, params) in [
            (format!("limit {}", cap + 1), vec![]),
            (format!("limit 1 offset {cap}"), vec![]),
            ("limit $1".to_string(), vec![past]),
        ] {
            let says = format!("at most {cap} rows, {} were requested", cap + 1);
            refused(
                &mut db,
                &format!("get r select id {ranking} {page}"),
                &params,
                &says,
            );
        }
    }
}

/// A chain of 8 `lookup`s answers, each level's child there; a ninth is a
/// query error where it is read.
#[test]
fn a_lookup_chains_8_levels_and_refuses_a_ninth() {
    let most = fennec_core::query::MAX_LOOKUP_DEPTH;
    let mut db = Database::new();
    for i in 0..=most + 1 {
        run(
            &mut db,
            &format!("create collection c{i} (p int @hash); put c{i} {{p: 1}}"),
        )
        .unwrap();
    }
    let chain =
        |n: usize| (1..=n).fold("get c0".to_string(), |q, i| format!("{q} lookup c{i} on p"));
    let r = ask(&mut db, &chain(most), &[]).unwrap();
    assert_eq!(count(&r), 1);
    let json = fennec_core::json::response_to_string(&r);
    assert!(json.contains(&format!("\"c{most}\"")), "{json}");
    let says = format!("`lookup` chained too deep: at most {most} levels");
    refused(&mut db, &chain(most + 1), &[], &says);
}

/// An index over several fields takes 8 and refuses 9, and 1; a `lookup`
/// key of 8 is that index's run, one of 9 refused naming the index it
/// would need -- one no declaration takes.
#[cfg(feature = "composite")]
#[test]
fn an_index_and_a_lookup_key_take_8_fields_and_refuse_a_ninth() {
    let most = fennec_core::schema::MAX_COMPOSITE_KEYS;
    let names = |n: usize| {
        (1..=n)
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let fields = (1..=most + 1)
        .map(|i| format!("a{i} int"))
        .collect::<Vec<_>>()
        .join(", ");
    let one = (1..=most + 1)
        .map(|i| format!("a{i}: 1"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut db = Database::new();
    run(
        &mut db,
        &format!(
            "create collection p ({fields}); create collection c ({fields}, @sorted({}));
             put p {{{one}}}; put c {{{one}}}",
            names(most)
        ),
    )
    .unwrap();
    let says = format!("an index takes at most {most} fields, not {}", most + 1);
    let nine = format!("create index on c ({}) @sorted", names(most + 1));
    refused(&mut db, &nine, &[], &says);
    refused(
        &mut db,
        "create collection d (a1 int, @sorted(a1))",
        &[],
        "one field",
    );
    let key = |n: usize| format!("get p lookup c on ({}) = ({})", names(n), names(n));
    let r = ask(&mut db, &key(most), &[]).unwrap();
    let json = fennec_core::json::response_to_string(&r);
    assert!(json.contains("\"c\":[{"), "{json}");
    let says = format!(
        "needs an index whose first fields are those (declare it with @sorted({}))",
        names(most + 1)
    );
    refused(&mut db, &key(most + 1), &[], &says);
}

/// A facet answers its 10 000 values without `top`, and a `top` of 10 000;
/// a field holding 10 001 values is refused without `top`, as is a `top`
/// past it, written or a parameter's -- never the first 10 000 of them.
#[cfg(feature = "facets")]
#[test]
fn a_facet_answers_10000_values_and_refuses_one_more() {
    let max = fennec_core::query::MAX_FACET_VALUES;
    let mut db = Database::new();
    let docs: Vec<String> = (0..max).map(|i| format!("{{k: {i}}}")).collect();
    run(
        &mut db,
        &format!("create collection f (k int); put f [{}]", docs.join(", ")),
    )
    .unwrap();
    let values = |db: &mut Database, text: &str, params: &[Value]| {
        let r = ask(db, text, params).unwrap_or_else(|e| panic!("{text}: {e}"));
        r.rows().unwrap().facets[0].values.len()
    };
    let at = [Value::Int(max as i64)];
    assert_eq!(values(&mut db, "get f limit 1 facet k", &[]), max);
    assert_eq!(
        values(&mut db, &format!("get f limit 1 facet k top {max}"), &[]),
        max
    );
    assert_eq!(values(&mut db, "get f limit 1 facet k top $1", &at), max);
    run(&mut db, &format!("put f {{k: {max}}}")).unwrap();
    let says = format!(
        "`facet k` holds {} values, past the {max} one answers with",
        max + 1
    );
    refused(&mut db, "get f limit 1 facet k", &[], &says);
    assert_eq!(values(&mut db, "get f limit 1 facet k top $1", &at), max);
    let says = format!("a facet answers at most {max} values");
    let past = [Value::Int(max as i64 + 1)];
    refused(
        &mut db,
        &format!("get f limit 1 facet k top {}", max + 1),
        &[],
        &says,
    );
    refused(&mut db, "get f limit 1 facet k top $1", &past, &says);
}

/// `ranges` takes 10 001 bounds, 10 000 ranges each answered, an empty
/// one with 0; 10 002 are refused.
#[cfg(feature = "facets")]
#[test]
fn a_range_facet_takes_10001_bounds_and_refuses_one_more() {
    let max = fennec_core::query::MAX_FACET_VALUES;
    let mut db = Database::new();
    run(
        &mut db,
        "create collection g (n int); put g [{n: 0}, {n: 5}, {n: 9999}, {n: 10000}]",
    )
    .unwrap();
    let bounds = |n: usize| (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let text = format!("get g limit 1 facet n ranges [{}]", bounds(max + 1));
    let r = ask(&mut db, &text, &[]).unwrap_or_else(|e| panic!("{e}"));
    let ranges = &r.rows().unwrap().facets[0].values;
    assert_eq!(ranges.len(), max);
    // The last bound is excluded: 10 000 counts in none.
    assert_eq!(ranges.iter().map(|(_, n)| n).sum::<u64>(), 3);
    assert_eq!(ranges[max - 1].1, 1);
    let text = format!("get g limit 1 facet n ranges [{}]", bounds(max + 2));
    refused(&mut db, &text, &[], "takes 2 to 10 001 numbers");
}

/// A list type nests 64 levels and no more: a point counts one, and a
/// list of `json` is refused at any depth.
#[test]
fn a_list_type_nests_64_levels_and_no_more() {
    let depth = fennec_core::codec::MAX_DEPTH;
    let lists = |n: usize, inner: &str| format!("{}{inner}{}", "[".repeat(n), "]".repeat(n));
    let mut db = Database::new();
    run(
        &mut db,
        &format!("create collection l (x {})", lists(depth, "int")),
    )
    .unwrap();
    let past = format!("create collection m (x {})", lists(depth + 1, "int"));
    refused(
        &mut db,
        &past,
        &[],
        &format!("a list type nests at most {depth} levels"),
    );
    run(
        &mut db,
        &format!("create collection p (x {})", lists(depth - 1, "geo")),
    )
    .unwrap();
    let says = format!("`x`: a type nests at most {depth} levels, a list one and a point one");
    match run(
        &mut db,
        &format!("create collection q (x {})", lists(depth, "geo")),
    ) {
        Err(Error::Type(m)) => assert_eq!(m, says),
        other => panic!("{:?}", other.map(|r| count(&r))),
    }
    for n in [1, 2, depth] {
        match run(
            &mut db,
            &format!("create collection j (x {})", lists(n, "json")),
        ) {
            Err(Error::Type(m)) => assert!(m.contains("a list of json is a json field"), "{m}"),
            other => panic!("{n}: {:?}", other.map(|r| count(&r))),
        }
    }
    assert_eq!(db.collection_names(), ["l", "p"]);
}

/// `in` lists over an index's parts making 256 spans read the index's
/// spans; making 272, the second list is tested row by row -- the answer
/// the same either way.
#[cfg(feature = "composite")]
#[test]
fn in_lists_over_an_indexs_parts_make_256_spans_and_past_it_are_tested_row_by_row() {
    let mut db = Database::new();
    let docs: Vec<String> = (0..20)
        .flat_map(|a| (0..20).map(move |b| format!("{{a: {a}, b: {b}}}")))
        .collect();
    run(
        &mut db,
        &format!(
            "create collection s (a int, b int, @sorted(a, b)); put s [{}]",
            docs.join(", ")
        ),
    )
    .unwrap();
    let list = |n: usize| (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    for (a, b, spans) in [
        (16, 16, "a, b fixed, 256 spans"),
        (17, 16, "a fixed, 17 spans"),
    ] {
        let filter = format!("where a in [{}] and b in [{}]", list(a), list(b));
        let r = ask(&mut db, &format!("get s {filter} count"), &[]).unwrap();
        assert_eq!(
            r.rows().unwrap().rows[0].values[0],
            Value::Int((a * b) as i64)
        );
        let plan = ask(&mut db, &format!("explain get s {filter} count"), &[]).unwrap();
        let plan = fennec_core::json::response_to_string(&plan);
        assert!(plan.contains(spans), "{plan}");
    }
}

/// The record that takes a segment past 8 MiB lands in it whole; the one
/// after opens another.
#[test]
fn a_segment_is_sealed_once_it_holds_8_mib() {
    use fennec_core::store::{Store, OP_PUT, SEGMENT_MAX};
    let mut st = Store::new();
    let payload = vec![7u8; SEGMENT_MAX / 8];
    for id in 1..=8 {
        st.append(OP_PUT, id, &payload);
        assert_eq!(st.segment_count(), 1, "record {id}");
    }
    st.append(OP_PUT, 9, &payload);
    assert_eq!(st.segment_count(), 2);
}

/// A time is read to the millisecond, the rest of a fraction dropped --
/// never rounded up into the next one.
#[test]
fn a_time_is_kept_to_the_millisecond() {
    let mut db = Database::new();
    run(&mut db, "create collection t (at timestamp)").unwrap();
    for (i, (text, ms)) in [
        ("2026-01-02T03:04:05.123Z", 123),
        ("2026-01-02T03:04:05.123456Z", 123),
        ("2026-01-02T03:04:05.123999Z", 123),
        ("2026-01-02T03:04:05.124Z", 124),
    ]
    .into_iter()
    .enumerate()
    {
        run(&mut db, &format!("put t {{at: \"{text}\"}}")).unwrap();
        let r = ask(
            &mut db,
            &format!("get t where id = {} select at", i + 1),
            &[],
        )
        .unwrap();
        match &r.rows().unwrap().rows[0].values[0] {
            Value::Timestamp(t) => assert_eq!(t.rem_euclid(1000), ms, "{text}"),
            other => panic!("{text}: {other:?}"),
        }
    }
}

/// A named zone answers from the first instant of its table, and refuses
/// the millisecond before it -- never answered by rules it does not hold.
#[cfg(feature = "analytics")]
#[test]
fn a_named_zone_answers_from_its_first_instant_and_refuses_the_one_before() {
    let from = zone_from();
    let mut db = Database::new();
    run(
        &mut db,
        &format!("create collection ev (at timestamp); put ev {{at: \"{from}\"}}"),
    )
    .unwrap();
    let r = ask(&mut db, "get ev select at", &[]).unwrap();
    let Value::Timestamp(first) = r.rows().unwrap().rows[0].values[0] else {
        panic!("{r:?}")
    };
    let bucket = "get ev where at = $1 select bucket(at, 1d, 'Europe/Berlin') as d";
    let r = ask(&mut db, bucket, &[Value::Timestamp(first)]).unwrap();
    assert_eq!(count(&r), 1);
    ask(&mut db, "put ev {at: $1}", &[Value::Timestamp(first - 1)]).unwrap();
    let says = format!("`bucket` knows a zone's rules from {from} on");
    refused(&mut db, bucket, &[Value::Timestamp(first - 1)], &says);
}

/// A point takes ±180 and ±90 as written, and refuses past either --
/// never wrapped.
#[test]
fn a_point_takes_180_and_90_and_refuses_past_them() {
    let mut db = Database::new();
    run(&mut db, "create collection p (loc geo)").unwrap();
    for ok in ["[180, 90]", "[-180, -90]", "[-180, 90]"] {
        run(&mut db, &format!("put p {{loc: {ok}}}")).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for past in [
        "[180.000001, 0]",
        "[0, 90.000001]",
        "[-180.000001, 0]",
        "[0, -90.000001]",
        "[540, 0]",
    ] {
        let says = format!(
            "a point is [lon, lat], a longitude from -180 to 180 and a latitude from -90 to 90; \
             found {}",
            past.replace(' ', "")
        );
        match run(&mut db, &format!("put p {{loc: {past}}}")) {
            Err(Error::Type(m)) => assert!(m.ends_with(&says), "{past}: {m}"),
            other => panic!("{past}: {other:?}"),
        }
    }
    let r = ask(&mut db, "get p select loc", &[]).unwrap();
    let json = fennec_core::json::response_to_string(&r);
    assert!(
        json.contains("[180,90]") && json.contains("[-180,-90]") && json.contains("[-180,90]"),
        "{json}"
    );
    assert_eq!(count(&r), 3);
}
