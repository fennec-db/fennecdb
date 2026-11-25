# Security

The threat model: every trust boundary the area files keep as rules --
access, the HTTP server, the router, tenants, the file format's readers,
the native library and the sync, the JS client, the studio, the desktop
app, the release tooling -- ordered in one place. For each, what it
protects, who attacks it and with what, the code that enforces it, the
test that proves it and what is deliberately not defended. It holds no
invariant of its own: each boundary's rule stays in the CLAUDE.md of the
area whose code enforces it, its long form in the design file its section
links, and a change to a boundary changes those first and this table in
the same pull request. [SECURITY.md](../../SECURITY.md) is the policy a
reporter and an operator act on; this is the map behind it, and the scope
an outside review would take ([For a reviewer](#for-a-reviewer)).

**How it is held.** `tools/security_md_check.py`, in CI's fmt job beside
SECURITY.md's own check, reads the [table](#the-boundaries) row by row:
each enforcing item, written `path::item`, must be defined at that path
outside its test module; each cited test, `path::test`, must be a test
there that CI builds and runs (a Rust `#[test]` not `#[ignore]`d, in a
file its crate's `tests/all.rs` or Cargo.toml names; a JS `test(` by its
title; a Python or shell test by its name); a row cites a test or says "no
test", never neither; there are 15 rows at the least; and every path,
link, anchor and flag of this file resolves. A Rust item or test is found
in code alone, where Rust's grammar puts an item: its comments, strings
and characters blanked by `tools/secret_eq_check.py`'s reader, every item
a `#[cfg(test)]` is on left out, so a name surviving in a comment, a
string or a type (`&'static T`, `*const T`) once its item is renamed or
deleted holds the row up no longer. A function renamed or a test deleted
fails the check, naming the row.

What the check proves is that each name exists where the row cites it,
and no more: not that the test exercises the boundary, nor that it would
fail with the enforcement taken away. That is the claim each row makes,
and the reviewers' to judge. It was read, not run -- each test's deciding
assertion against the item it stands for -- and a reviewer's first check
is to try some; the tests the internal review's fixes added (#677, #678,
#679) were each run against the code before their fix, and failed there.

**How a row reads.** *Enforced by* names the items that decide, not every
item on the path. *Proved by* names the tests whose assertions fail
without them. "No test" says what no test proves, a whole row's
enforcement or a part of it: each is a finding still open, which the check
names on every run, never hidden by citing a test nearby.

## The attackers

Each boundary's attacker is one of these, by what it holds:

- **A network peer** holding nothing: it can open connections, send any
  bytes, hold a connection open and read slowly.
- **A scoped caller**: a holder of a JSON Web Token the server's keys
  signed, its policy's rules what it may do.
- **Another tenant**: a tenant's own admin, its token or its webhook
  sender, against another tenant of the same node or router.
- **A page**: a web page in a victim's browser, against a server on the
  victim's machine or network -- a cross-site request, or a DNS name
  rebound to 127.0.0.1.
- **A webhook's sender**, or whoever captured an event and sends it again.
- **A peer of replication**: a primary a replica follows, a replica or an
  archiver a primary serves, or whoever stands in for one on a plain
  `http://` path.
- **Whoever hands the process bytes to open**: a `.fennec` file, an image,
  an archive, a backup, a sealed file, a SQLite file.
- **A PostgreSQL server** `fennec import` or a follower dials, or whoever
  stands in for it.
- **A registry or mirror** between a release and its installer, or a step
  of the build itself.
- **A co-tenant on the host**: another local user of the machine, or a
  second process.
- **The embedding app or page**: what calls the native library or the
  browser module, against the library's memory.

## The boundaries

Paths are the repository's; `a.rs::T::f` is the method `f` of `T` in
`a.rs`. Each boundary's section follows the [list of what is not
defended](#not-defended).

| # | Boundary | Attacker | Enforced by | Proved by |
|---|---|---|---|---|
| 1 | [Scoped tokens](#scoped-tokens) | a scoped caller | `crates/fennec-http/src/lib.rs::scoped`, `crates/fennec-http/src/access.rs::Scope::rewrite_with`, `crates/fennec-http/src/access.rs::and`, `crates/fennec-http/src/access.rs::Check::before_upsert` | `crates/fennec-http/tests/rules_first.rs::a_failing_term_over_a_row_the_rules_hide_answers_as_no_row`, `crates/fennec-http/tests/rules_first.rs::an_upserts_else_set_reads_no_row_the_token_may_not_update`, `crates/fennec-http/tests/rules_first.rs::a_scoped_explain_is_refused`, `crates/fennec-http/tests/access.rs::a_write_outside_the_rules_is_refused` |
| 2 | [Fields a rule withholds](#fields-a-rule-withholds) | a scoped caller | `crates/fennec-http/src/access/fields.rs::hold`, `crates/fennec-http/src/access/fields.rs::write`, `crates/fennec-http/src/access/fields.rs::Scope::shown`, `crates/fennec-http/src/access/fields.rs::Scope::shown_schema` | `crates/fennec-http/tests/fields.rs::a_where_over_a_hidden_field_tells_nothing_of_it`, `crates/fennec-http/tests/fields.rs::a_write_reads_no_field_its_rows_do_not_show`, `crates/fennec-http/tests/fields.rs::a_row_shows_the_fields_of_the_rules_admitting_it`, `crates/fennec-http/tests/fields.rs::a_field_no_rule_grants_is_not_there` |
| 3 | [Rules reading another collection](#rules-reading-another-collection) | a scoped caller | `crates/fennec-http/src/access.rs::Scope::bound`, `crates/fennec-http/src/access.rs::listed`, `crates/fennec-http/src/access.rs::ring` | `crates/fennec-http/tests/reach.rs::a_rule_s_list_past_the_bound_is_refused`, `crates/fennec-http/tests/reach.rs::rules_reading_each_other_are_refused_at_startup` |
| 4 | [The server's own collections](#the-servers-own-collections) | a scoped caller | `crates/fennec-http/src/access.rs::covers`, `crates/fennec-core/src/declared.rs::own`, `crates/fennec-http/src/access.rs::own_refused`, `crates/fennec-http/src/lib.rs::path_refusal` | `crates/fennec-http/tests/own.rs::a_star_rule_reaches_none_of_the_servers_collections`, `crates/fennec-http/tests/own.rs::the_servers_token_reads_them_and_changes_none`, `crates/fennec-http/tests/own.rs::a_rule_naming_or_reading_one_is_refused_at_startup` |
| 5 | [A token bound to its tenant](#a-token-bound-to-its-tenant) | another tenant | `crates/fennec-http/src/lib.rs::route_tenant`, `crates/fennec-http/src/access.rs::Scope::reaches`, `crates/fennec-http/src/tenants.rs::Tenants::check_scoped_writes` | `crates/fennec-http/tests/tenants.rs::a_token_reaches_only_the_tenant_it_names`, `crates/fennec-http/tests/tenants.rs::a_scoped_write_to_a_tenant_is_checked`, `crates/fennec-shard/tests/router.rs::a_token_for_one_tenant_reaches_no_other_through_the_router` |
| 6 | [Whom a token is for](#whom-a-token-is-for) | another app's token from the same keys | `crates/fennec-http/src/access.rs::Demands::whom`, `crates/fennec-http/src/access.rs::Access::check` | `crates/fennec-http/src/access.rs::whom_a_token_is_for_is_asked_where_told`, `crates/fennec-http/tests/providers.rs::a_token_for_another_issuer_or_audience_is_refused_on_every_route` |
| 7 | [Expiry](#expiry) | a stale token's holder | `crates/fennec-http/src/access.rs::Access::verify`, `crates/fennec-http/src/access.rs::refusal_code`, `crates/fennec-http/src/access.rs::Access::scope`, `crates/fennec-http/src/sse.rs::EXPIRED` | `crates/fennec-http/src/access.rs::only_an_expired_token_is_told_by_its_code`, `crates/fennec-http/src/access.rs::an_exp_past_the_range_ends_nothing_early`, `crates/fennec-http/tests/access.rs::a_subscription_ends_when_its_token_expires` |
| 8 | [A token's signature](#a-tokens-signature) | a forger | `crates/fennec-http/src/access.rs::Access::check`, `crates/fennec-http/src/access.rs::read_jwks`, `crates/fennec-http/src/crypto.rs::RsaKey::new`, `crates/fennec-http/src/crypto.rs::RsaKey::verify_sha256`, `crates/fennec-http/src/crypto/p256.rs::P256Key::verify_sha256` | `crates/fennec-http/src/access.rs::a_forged_or_stale_token_is_refused`, `crates/fennec-http/src/access.rs::an_identity_providers_keys_verify_its_tokens`, `crates/fennec-http/src/access.rs::a_jwks_read_half_written_is_read_again_once_whole`, `crates/fennec-http/tests/wycheproof.rs::rsa_pkcs1_sha256_wycheproof_2048`, `crates/fennec-http/src/crypto.rs::rs256_at_each_end_of_the_keys_it_takes`, `crates/fennec-http/tests/es256.rs::every_wycheproof_p256_sha256_case`, `crates/fennec-http/tests/es256.rs::a_jwks_takes_p256_keys_and_passes_over_other_curves`, `crates/fennec-http/tests/es256.rs::a_server_takes_es256_tokens_and_refuses_them_changed` |
| 9 | [Empty and short tokens](#empty-and-short-tokens) | a network peer | `crates/fennec-server/src/main.rs::env_secret`, `crates/fennec-http/src/lib.rs::Config::settled`, `crates/fennec-http/src/lib.rs::short_token`, `crates/fennec-http/src/lib.rs::TOKEN_MIN` | `crates/fennec-http/tests/empty_tokens.rs::an_empty_admin_token_leaves_admin_off`, `crates/fennec-shard/tests/tokens.rs::a_token_set_empty_is_no_token`, `crates/fennec-http/src/lib.rs::a_token_past_loopback_is_32_bytes_at_least`, `crates/fennec-server/tests/tokens.rs::a_short_token_is_refused_outside_loopback` |
| 10 | [Guessing a token](#guessing-a-token) | a network peer; a scoped caller | `crates/fennec-http/src/crypto.rs::ct_eq`, `crates/fennec-http/src/lib.rs::authenticate`, `crates/fennec-http/src/audit.rs::failed_as`, `crates/fennec-http/src/audit.rs::verified_jwt` | `crates/fennec-http/tests/audit.rs::refusals_wait_and_schema_changes_and_refusals_are_logged`, `crates/fennec-http/tests/audit.rs::a_good_jwt_clears_no_count_of_guesses_at_a_servers_token`; no test of `ct_eq`'s time, which `tools/secret_eq_check.py` holds by its callers alone |
| 11 | [A server that asks for no token](#a-server-that-asks-for-no-token) | a page; a network peer | `crates/fennec-http/src/lib.rs::open_refusal`, `crates/fennec-http/src/cors.rs::foreign`, `crates/fennec-http/src/cors.rs::local_host`, `crates/fennec-http/src/lib.rs::exposed`, `crates/fennec-http/src/audit.rs::routed_past_loopback`, `crates/fennec-shard/src/lib.rs::Router::open_node` | `crates/fennec-http/tests/open_servers.rs::a_rebound_name_is_refused_a_read_included`, `crates/fennec-http/tests/open_servers.rs::another_origins_post_is_refused_before_it_runs`, `crates/fennec-server/tests/start.rs::an_open_endpoint_without_a_token_is_refused_before_the_open`, `crates/fennec-shard/tests/cors.rs::a_router_past_loopback_reaches_no_node_that_takes_no_token`, `crates/fennec-shard/tests/cors.rs::a_router_past_loopback_asks_a_node_from_before_whether_it_is_open` |
| 12 | [CORS](#cors) | a page | `crates/fennec-http/src/cors.rs::allows`, `crates/fennec-http/src/cors.rs::vouched`, `crates/fennec-http/src/cors.rs::is_cors_header`, `crates/fennec-shard/src/lib.rs::forwarded_headers` | `crates/fennec-http/tests/api.rs::a_preflight_is_answered_before_the_token_is_asked_for`, `crates/fennec-shard/tests/cors.rs::a_forwarded_answer_tells_the_routers_rule_once`, `crates/fennec-shard/tests/cors.rs::a_router_answers_for_the_origins_it_lets_in_to_an_open_node`, `crates/fennec-shard/tests/cors.rs::a_token_router_tells_an_open_node_the_name_its_client_reached_it_by` |
| 13 | [The router's token and its nodes'](#the-routers-token-and-its-nodes) | a network peer; another tenant | `crates/fennec-shard/src/lib.rs::Router::refused`, `crates/fennec-shard/src/lib.rs::Router::admin`, `crates/fennec-http/src/admin.rs::carries`, `crates/fennec-http/src/lib.rs::admin_query`, `crates/fennec-shard/src/region.rs::Router::elsewhere`, `crates/fennec-shard/src/region.rs::Router::vouches_for` | `crates/fennec-shard/tests/router.rs::refusals`, `crates/fennec-shard/tests/fanout.rs::a_token_bound_to_one_tenant_reaches_no_query_across_them`, `crates/fennec-shard/tests/refusals.rs::a_router_tells_no_one_it_cannot_vouch_for_which_tenants_exist` |
| 14 | [A tenant's hook secrets](#a-tenants-hook-secrets) | another tenant; a data token | `crates/fennec-http/src/tenant_hooks.rs::Store`, `crates/fennec-http/src/tenant_hooks.rs::Store::is_current`, `crates/fennec-http/src/tenant_hooks.rs::Store::set_aside`, `crates/fennec-http/src/admin.rs::handle` | `crates/fennec-http/tests/tenant_hooks.rs::a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone`, `crates/fennec-http/tests/tenant_hooks.rs::a_file_put_in_place_under_a_running_node_is_armed_by_no_secret_before_it`, `crates/fennec-http/tests/tenant_hooks.rs::a_sidecar_that_does_not_read_is_set_aside_never_removed` |
| 15 | [A webhook's event](#a-webhooks-event) | a webhook's sender | `crates/fennec-http/src/hooks.rs::verified`, `crates/fennec-http/src/crypto.rs::HmacKey`, `crates/fennec-http/src/hooks.rs::hook_of` | `crates/fennec-http/src/hooks.rs::a_timestamp_past_the_tolerance_is_stale`, `crates/fennec-http/src/hooks.rs::each_scheme_verifies_its_senders_example`, `crates/fennec-server/tests/hooks.rs::a_stripe_event_is_taken_once_by_its_key`; no test of `SKEW_MS` in a hook's `keep`, which the 1-day floor decides today |
| 16 | [A body's ceiling](#a-bodys-ceiling) | a network peer | `crates/fennec-http/src/lib.rs::body_ceiling`, `crates/fennec-http/src/http.rs::read_head`, `crates/fennec-http/src/http.rs::chunked_framing`, `crates/fennec-http/src/http.rs::read_onto` | `crates/fennec-http/tests/tenants.rs::only_an_import_carrying_the_admin_token_is_read_past_the_data_ceiling`, `crates/fennec-http/tests/chunked.rs::a_framing_read_two_ways_is_refused_and_the_connection_closed`, `crates/fennec-http/src/http.rs::a_declared_chunk_grows_the_body_only_as_its_bytes_come` |
| 17 | [A slow client and the connection cap](#a-slow-client-and-the-connection-cap) | a network peer | `crates/fennec-http/src/conn.rs::Paced::timed`, `crates/fennec-http/src/conn.rs::MIN_RATE`, `crates/fennec-http/src/accept.rs::Slot::take`, `crates/fennec-http/src/accept.rs::no_thread` | `crates/fennec-http/tests/server.rs::a_trickled_body_is_cut`, `crates/fennec-http/tests/server.rs::a_trickled_head_is_cut_at_its_deadline`, `crates/fennec-http/tests/server.rs::connection_limit_refuses_extra_connections`, `crates/fennec-http/tests/api.rs::a_connection_with_no_thread_is_answered_503_and_told_when_to_come_back` |
| 18 | [TLS served](#tls-served) | a network peer on the path | `crates/fennec-http/src/conn.rs::from_flags`, `crates/fennec-http/src/conn.rs::Conn::open`, `crates/fennec-http/src/conn.rs::Conn::secured` | `crates/fennec-http/tests/tls.rs::plain_http_is_told_to_ask_over_https`, `crates/fennec-http/tests/tls.rs::a_handshake_past_its_bound_is_closed` |
| 19 | [TLS dialed](#tls-dialed) | a stand-in on the path | `crates/fennec-http/src/dial.rs::tls_of`, `crates/fennec-http/src/dial.rs::secure`, `crates/fennec-http/src/dial.rs::trust_ca`, `crates/fennec-tls/src/lib.rs::Client::dial_within` | `crates/fennec-http/tests/dials.rs::a_name_the_certificate_does_not_hold_is_refused_before_a_byte_is_sent`, `crates/fennec-http/tests/dials.rs::a_handshake_never_answered_is_a_timeout_at_the_dial_s_bound` |
| 20 | [Idempotency keys](#idempotency-keys) | a scoped caller | `crates/fennec-http/src/idempotent.rs::key`, `crates/fennec-http/src/access.rs::Scope::authority`, `crates/fennec-http/src/idempotent.rs::answered` | `crates/fennec-http/tests/idempotency.rs::a_key_is_for_one_request_one_user_and_a_write_that_landed`, `crates/fennec-http/tests/idempotency.rs::a_key_is_replayed_only_to_a_caller_of_the_same_authority` |
| 21 | [A scoped caller's rate](#a-scoped-callers-rate) | a scoped caller | `crates/fennec-http/src/rate.rs::admitted`, `crates/fennec-http/src/rate.rs::keyed` | `crates/fennec-http/tests/request_rate.rs::a_caller_past_the_rate_is_told_its_wait_and_nothing_of_it_runs` |
| 22 | [MCP's tools](#mcps-tools) | a scoped caller's agent; a page | `crates/fennec-http/src/mcp/mod.rs::admit`, `crates/fennec-http/src/mcp/mod.rs::admitted`, `crates/fennec-http/src/mcp/tools.rs::read_served`, `crates/fennec-http/src/mcp/tools.rs::write` | `crates/fennec-http/tests/mcp.rs::a_scoped_token_s_agent_sees_what_its_query_sees`, `crates/fennec-http/tests/mcp.rs::a_callers_rate_is_its_own_in_each_tenant_through_the_endpoint` |
| 23 | [The studio page](#the-studio-page) | a page; a stored row | `crates/fennec-http/src/studio.rs::Studio::new`, `crates/fennec-http/src/studio.rs::guarded`, `studio/statements.js::enclosed`, `studio/statements.js::filter` | `crates/fennec-server/tests/studio.rs::the_studio_is_served_with_its_headers`, `studio/test/e2e.test.mjs::embedded in an app: the session it hands over, never stored, and no form to sign in with`, `studio/test/statements.test.mjs::a cell is written by id, the value a parameter, the count required` |
| 24 | [The desktop app's window](#the-desktop-apps-window) | a page loaded in the app | `integrations/studio-desktop/src/bun/session.ts::sessionPreload`, `integrations/studio-desktop/src/bun/windows.ts::holdTo`, `integrations/studio-desktop/src/bun/opener.ts::newWindowPreload`, `integrations/studio-desktop/src/bun/env.ts::serverEnvironment` | `integrations/studio-desktop/test/session.test.ts::a page that is not the server's gets nothing: any other origin, a lookalike, another scheme or port`, `integrations/studio-desktop/test/process.test.ts::a window's server: loopback, any port, the token in its environment and in no argument, and a stop that is its shutdown` |
| 25 | [A file's bytes](#a-files-bytes) | whoever hands the process a file | `crates/fennec-core/src/engine.rs::Rec::holds`, `crates/fennec-core/src/codec.rs::MAX_DEPTH`, `crates/fennec-core/src/codec.rs::take`, `crates/fennec-core/src/schema.rs::Schema::decode` | `crates/fennec-core/tests/persist.rs::a_page_lost_inside_a_synced_vector_record_refuses_the_open`, `crates/fennec-core/src/codec.rs::a_value_nests_no_deeper_than_json_may_be_written`, `crates/fennec-core/src/codec.rs::a_count_of_items_past_usize_is_refused`, `crates/fennec-core/tests/damage.rs::a_schema_past_the_languages_ranges_is_refused_where_a_file_is_opened_and_applied` |
| 26 | [A sealed archive's bytes](#a-sealed-archives-bytes) | whoever reads or writes where archives are kept | `crates/fennec-http/src/seal.rs::Frames::aad`, `crates/fennec-http/src/seal.rs::begin`, `crates/fennec-http/src/seal.rs::vouched_in`, `crates/fennec-http/src/seal.rs::open_frames`, `crates/fennec-http/src/seal.rs::open_whole`, `crates/fennec-http/src/archive.rs::Archive::held`, `crates/fennec-http/src/archive.rs::entries` | `crates/fennec-http/src/seal.rs::a_frame_from_another_file_does_not_open`, `crates/fennec-http/src/seal.rs::frames_out_of_place_do_not_open`, `crates/fennec-http/src/seal.rs::frames_sealed_again_after_a_cut_do_not_splice_with_those_cut`, `crates/fennec-http/src/seal.rs::the_head_is_the_first_frames_and_a_version_is_named`, `crates/fennec-http/src/seal.rs::each_file_seals_under_its_own_key`, `crates/fennec-http/src/seal.rs::a_frame_holds_a_mebibyte_at_most`, `crates/fennec-http/tests/archive.rs::a_sealed_archive_restores_with_its_key_and_nothing_else`, `crates/fennec-http/tests/archive.rs::a_segment_renamed_into_anothers_place_is_refused`, `crates/fennec-http/tests/archive.rs::an_image_and_a_backup_do_not_stand_in_for_each_other`, `crates/fennec-http/tests/node_archive.rs::a_tenants_archive_copied_over_anothers_is_refused`, `crates/fennec-http/tests/node_archive.rs::presence_and_began_edited_are_refused` |
| 27 | [A primary's stream](#a-primarys-stream) | a primary, or a stand-in on a plain path | `crates/fennec-core/src/engine.rs::Database::apply_records`, `crates/fennec-core/src/engine.rs::Database::land_applied`, `crates/fennec-http/src/replication.rs::kind_head`, `crates/fennec-http/src/replication.rs::kind_body`, `crates/fennec-http/src/replication.rs::decode`, `crates/fennec-http/src/replication.rs::read_head`, `crates/fennec-http/src/replication.rs::Follower::session_caught` | `crates/fennec-core/tests/replica.rs::a_record_of_an_unknown_kind_or_failing_its_sum_is_refused_and_none_of_it_applied`, `crates/fennec-core/src/engine/unwinding.rs::a_panic_anywhere_in_applying_a_record_puts_it_back_whole`, `crates/fennec-core/tests/damage.rs::a_primarys_record_nesting_past_the_limit_is_refused_by_its_replica`, `crates/fennec-http/src/replication.rs::a_message_is_held_in_its_length_and_its_kind_bound`, `crates/fennec-http/src/replication.rs::a_message_is_held_as_it_comes_not_as_it_was_announced`, `crates/fennec-http/tests/message_peak.rs::a_message_holds_twice_its_length_and_three_times_what_came`, `crates/fennec-http/tests/replication.rs::a_panic_applying_a_write_ends_the_follower_and_never_its_halt`; no test of a count of writes past its message, a head past 64 KiB or a panic outside `apply` |
| 28 | [A replica's request](#a-replicas-request) | a network peer posing as a replica | `crates/fennec-http/src/replication.rs::Replication::authorized`, `crates/fennec-http/src/lib.rs::route_tenant`, `crates/fennec-http/src/replication.rs::handle_tenant`, `crates/fennec-http/src/node_stream.rs::handle`, `crates/fennec-core/src/history.rs::History::continues` | `crates/fennec-http/tests/empty_tokens.rs::an_empty_replication_token_opens_no_stream`, `crates/fennec-http/tests/primaries.rs::a_tenants_copy_is_promoted_by_the_admin_alone`, `crates/fennec-http/tests/replication.rs::a_promoted_replica_forks_and_the_old_primary_rejoins_under_it`, `crates/fennec-core/src/history.rs::a_history_continues_up_to_where_it_was_left` |
| 29 | [PostgreSQL dialed](#postgresql-dialed) | a PostgreSQL server, or a stand-in | `crates/fennec-wire/src/proto.rs::read_message`, `crates/fennec-wire/src/client.rs::Url::parse_in`, `crates/fennec-wire/src/client.rs::RequireAuth`, `crates/fennec-wire/src/client.rs::Scram::last`, `crates/fennec-wire/src/client.rs::ServerFinal::verify`, `crates/fennec-wire/src/client.rs::negotiate`, `crates/fennec-wire/src/client.rs::peer_text`, `crates/fennec-import/src/follow.rs::ensure_copy` | `crates/fennec-wire/src/proto.rs::a_message_s_length_is_held_to_the_protocol_and_never_aborts`, `crates/fennec-wire/tests/tls.rs::off_this_machine_the_default_checks_the_certificate`, `crates/fennec-wire/tests/client.rs::an_authentication_ok_before_any_exchange_is_refused_where_a_password_is_held`, `crates/fennec-wire/tests/client.rs::a_password_goes_in_clear_only_where_the_string_says_so`, `crates/fennec-wire/tests/client.rs::the_client_refuses_a_server_signature_it_cannot_verify`, `crates/fennec-wire/tests/client.rs::scram_refuses_a_cheap_proof_its_own_nonce_and_an_extension`, `crates/fennec-wire/tests/client.rs::scram_refuses_iterations_past_the_bound`, `crates/fennec-wire/tests/tls.rs::a_sign_in_over_tls_is_bound_to_the_servers_certificate`, `crates/fennec-wire/tests/tls.rs::bytes_after_the_servers_s_are_never_read_as_the_servers`, `crates/fennec-wire/tests/tls.rs::verify_full_refuses_a_name_the_certificate_does_not_hold_before_a_byte_is_sent`, `crates/fennec-server/tests/follow.rs::a_copy_held_up_holds_up_no_request_nor_the_shutdown`, `crates/fennec-import/src/follow.rs::a_keepalive_moves_no_confirmed_position_on_a_stream_not_proven`; no test of a forged `Commit`'s end on a stream not proven, which is confirmed |
| 30 | [A SQLite file](#a-sqlite-file) | whoever hands the importer a file | `crates/fennec-import/src/sqlite.rs::Pager::payload`, `crates/fennec-import/src/sqlite.rs::Reached`, `crates/fennec-import/src/sqlite.rs::Walk::cells_apart`, `crates/fennec-import/src/sqlite.rs::MIN_USABLE` | `crates/fennec-import/tests/sqlite.rs::a_payload_longer_than_the_file_is_refused_before_it_is_allocated`, `crates/fennec-import/tests/sqlite.rs::a_page_reached_twice_is_refused_not_walked_for_ever`, `crates/fennec-import/tests/sqlite.rs::an_overflow_chain_that_cycles_is_refused`, `crates/fennec-import/tests/sqlite.rs::a_leaf_naming_one_cell_again_and_again_is_refused_holding_a_page`, `crates/fennec-import/tests/sqlite.rs::a_cell_sharing_bytes_or_outside_the_cell_area_is_refused`, `crates/fennec-import/tests/sqlite.rs::a_page_under_sqlites_usable_floor_is_refused`, `crates/fennec-import/tests/sqlite.rs::a_hot_rollback_journal_is_refused` |
| 31 | [The browser module's memory](#the-browser-modules-memory) | the embedding page; a stored image | `crates/fennec-wasm/src/lib.rs::with_slot`, `crates/fennec-wasm/src/lib.rs::str_from`, `crates/fennec-wasm/src/lib.rs::fennec_alloc`, `crates/fennec-wasm/src/lib.rs::boxed`, `web/fennec.js::lengthOf`, `web/fennec.js::guarded` | `crates/fennec-wasm/src/lib.rs::text_that_is_not_utf8_reads_as_from_utf8_lossy_reads_it`, `crates/fennec-wasm/src/lib.rs::an_allocation_past_isize_max_is_refused`, `web/fennec.test.js::a length that is no length is refused before the module is handed it`, `web/fennec.test.js::after the module traps every call says to open the database again`, `web/fennec.test.js::an address past 2 GiB is read as the unsigned one it is`; no test of a handle the module never gave, nor of the glue's write's or `boxed`'s answer to an allocation the page's memory refuses |
| 32 | [The native library and the sync](#the-native-library-and-the-sync) | the embedding app; the server a replica syncs with | `crates/fennec-ffi/src/lib.rs::native`, `crates/fennec-ffi/src/lib.rs::call`, `crates/fennec-ffi/src/lib.rs::text`, `crates/fennec-ffi/src/jni.rs::array_len`, `crates/fennec-ffi/src/jni.rs::guarded`, `crates/fennec-abi/src/sync/mod.rs::said` | `crates/fennec-ffi/src/lib.rs::a_panic_stops_at_the_boundary`, `crates/fennec-ffi/tests/ffi.rs::an_error_is_its_code_and_its_json`, `crates/fennec-ffi/tests/ffi.rs::a_closed_handle_is_refused_and_never_reused`, `crates/fennec-ffi/src/jni.rs::an_answer_no_jvm_array_holds_is_a_query_error_never_cut`, `crates/fennec-ffi/src/jni.rs::a_panic_in_a_jni_functions_own_work_is_answered_never_unwound`, `crates/fennec-abi/src/sync/mod.rs::an_answer_says_its_quota_and_its_wait_or_nothing` |
| 33 | [A page's sealed persistence](#a-pages-sealed-persistence) | whoever reads or edits the browser's storage | `web/fennec.js::aad`, `web/fennec.js::seal`, `web/fennec.js::restoreNow`, `web/fennec.js::#loadPersist` | `web/fennec.persist.test.js::a sealed database restores with its key and nothing else`, `web/fennec.persist.test.js::a sealed store cut short opens as it stood or as the page went on, never spliced`, `web/fennec.sync.test.js::a store that does not open under the key given is kept with its writes, and the right key sends them` |
| 34 | [FennecQL's text](#fennecqls-text) | any client | `crates/fennec-ql/src/parser.rs::Parser::deepen`, `crates/fennec-ql/src/parser.rs::MAX_EXPR_DEPTH`, `crates/fennec-ql/src/parser.rs::Parser::int_in`, `crates/fennec-core/src/query.rs::param_list` | `crates/fennec-ql/src/lib.rs::expression_depth_is_bounded`, `crates/fennec-core/tests/in_param.rs::the_list_is_bounded_and_must_be_one` |
| 35 | [Started as root](#started-as-root) | a bug reached over a connection | `crates/fennec-server/src/user.rs::become_user`, `crates/fennec-server/src/user.rs::wanted`, `crates/fennec-server/src/user.rs::hand_over` | `crates/fennec-server/tests/user.rs::a_user_the_server_cannot_become_is_refused_before_anything_is_opened`, `crates/fennec-server/src/user.rs::a_hand_over_changes_only_the_inode_it_found`, `crates/fennec-server/src/user.rs::a_link_in_a_files_place_is_not_followed`; no test of the drop itself, which CI, never root, cannot make |
| 36 | [An install](#an-install) | a registry or mirror; a damaged download | `site/install.sh::main`, `site/install.ps1::Get-FennecAsset`, `tools/homebrew-formula.py::render` | `site/test_install.sh::tampered: installs nothing`, `tools/test_homebrew_formula.py::test_every_platform_gets_its_own_tarball_and_sum`; no test of a `.deb` or `.rpm` refusing a changed byte, which neither is checked for as it installs |
| 37 | [A release's provenance](#a-releases-provenance) | a step of the build; a token that publishes; whoever replaces an asset and its sum together | `.github/workflows/release.yml::binaries`, `.github/workflows/release.yml::publish`, `.github/workflows/packages.yml::npm`, `tools/workflow_check.py::check_text`, `tools/release-verify.sh::verified` | `tools/test_workflow_check.py::test_each_build_beside_a_write`, `tools/test_workflow_check.py::test_a_job_with_an_undeclared_token_is_named`, `tools/test_release_verify.py::test_a_missing_attestation`, `tools/test_release_verify.py::test_a_wrong_signer` |
| 38 | [Dependencies](#dependencies) | a crate, package or action from outside | `tools/deps_check.py::check`, `tools/deps_check.py::check_tls`, `tools/osv_check.py::npm_findings` | `tools/test_osv_check.py::test_npm_ignore`; no test of `tools/deps_check.py` itself, which CI runs bare |
| 39 | [Another local user](#another-local-user) | a co-tenant on the host; a second process | `crates/fennec-abi/src/lock.rs::hold`, `crates/fennec-core/src/fs.rs::private`, `crates/fennec-core/src/fs.rs::create_private`, `crates/fennec-core/src/fs.rs::create_like`, `crates/fennec-core/src/fs.rs::create_dir_private`, `crates/fennec-core/src/fs.rs::still_named`, `crates/fennec-http/src/owner_only.rs::restrict`, `tools/file_create_check.py::check` | `crates/fennec-abi/src/lock.rs::another_process_is_refused_while_one_holds_it`, `crates/fennec-abi/src/lock.rs::a_lock_file_is_made_its_owners_alone_or_as_its_database`, `crates/fennec-core/src/fs.rs::files_are_made_their_owners_alone_and_keep_their_mode`, `crates/fennec-core/src/fs.rs::a_link_at_a_side_files_name_is_never_followed`, `crates/fennec-http/tests/modes.rs::files_are_made_their_owners_alone_and_keep_their_mode`, `crates/fennec-cli/tests/lock.rs::what_the_program_makes_is_its_owners_alone_and_keeps_its_mode`, `crates/fennec-http/tests/tenant_hooks.rs::a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone`, `crates/fennec-cli/tests/tokens.rs::a_new_key_is_its_owner_s_alone` |
| 40 | [Secrets kept out of logs](#secrets-kept-out-of-logs) | whoever reads the logs or a trace collector | `crates/fennec-http/src/audit.rs::refusal`, `crates/fennec-http/src/audit.rs::hook_refused`, `crates/fennec-http/src/statements.rs::shape` | `crates/fennec-server/tests/hooks.rs::a_stripe_event_is_taken_once_by_its_key`, `crates/fennec-server/tests/hooks.rs::a_short_secret_is_warned_of_and_taken`, `crates/fennec-server/tests/tracing.rs::a_durable_write_is_traced_through_the_lock_the_disk_and_the_replicas`; no test that a refused token's value is never logged |
| 41 | [The system's randomness](#the-systems-randomness) | anyone predicting or forcing a nonce used twice | `crates/fennec-http/src/crypto.rs::system_random`, `crates/fennec-http/src/crypto/unix.rs::fill`, `crates/fennec-http/src/crypto/windows.rs::fill`, `crates/fennec-http/src/crypto.rs::id_u64` | `crates/fennec-http/src/crypto.rs::no_randomness_refuses_nonces_and_names_ids`, `crates/fennec-http/src/crypto.rs::randomness_needs_no_descriptor`, `crates/fennec-http/src/seal.rs::no_randomness_refuses_a_seal`, `crates/fennec-http/src/seal.rs::frames_sealed_back_to_back_take_their_own_nonces`; no test of `fill` answering nothing, nor of the `/dev/urandom` fallback |

## Not defended

Said plainly, and in SECURITY.md's
[documented behaviour](../../SECURITY.md#what-is-documented-behaviour-not-a-vulnerability)
where an operator meets it:

- **The server's own user, and root on the host.** The live file is
  plain on disk; only an archive or a backup is sealed. Whoever reads the
  file, its sync log, the audit log, the process's memory, its swap or a
  core dump has the data in them. On Linux and macOS every file
  fennec-server and `fennec` make is made `0600` by the call that makes
  it, whatever the umask, and every directory `0700` (#678, G14 F5; up to
  0.2.0 each took the umask's mode, usually readable by every user), and a
  TLS or sealing key others may read is warned of: another user reads
  none of them. Not held off: anything running as the server's user or as
  root; a file 0.2.0 or before made, or one its operator gave a wider
  mode, which keeps it; a copy made elsewhere; a user who may write the
  directory a database is in, trusted with its files as one who may
  replace the database itself; and what the process list shows, a
  password in `--follow`'s connection string among it.
- **Whoever holds the server's token** owns the database: every row,
  every schema, `append-only` collections included. On a node, the admin
  token reads every tenant and every hook secret; on a router, its token
  reaches every node's admin token through the directory.
- **Denial of service by work a client may ask for.** An authenticated
  client can ask for work that costs memory and time (`--max-memory` is
  fennec-server's bound); a peer holding many connections, each in its
  time, takes the slots a newcomer is refused for; a body to its ceiling
  is read before its token is refused. SECURITY.md lists each.
- **Side channels beyond the comparison of secrets.** Secrets are compared
  in constant time (`crypto::ct_eq`), their lengths aside; nothing else is
  claimed: the time a `match` or an index walk takes grows with rows the
  caller cannot read, cache timing is not considered, and RSA and P-256
  verification run on public values in variable time.
- **What the operator chose**: fennec-server's `--insecure`,
  `--http-cors '*'`, `--jwt-unbound-tenants`, `--jwt-require-exp off`, no
  `--jwt-issuer`, a short webhook secret, an `sslmode` short of
  `verify-full` (the default off this machine; `prefer` on it), plain HTTP
  without `--tls-cert`, a router dialing its nodes over `http://`, and
  `--insecure` on a router and the open node behind it.
- **Revocation.** The server keeps no list of revoked tokens; a token is
  good until its `exp`, and no leeway is given either side of it.
- **Rollback of what is stored where the attacker can write it**: a
  sealed segment cut at a frame's edge, a page's store cut at a record's
  edge, an older copy of a file, an archive, a tenant's line or a page's
  whole store put back, a later file deleted, one backup or one database's
  archive put in another's place. Since #677 and #679 a sealed file names
  what it is, its frames and a page's records are chained, and each is
  held to its name, place and order: a splice, a renamed segment, another
  tenant's or archive's file and an image read as a backup are refused
  (see the [open questions](#open-questions)).
- **The browser, the OS and the toolchain**: a compromised browser, a
  script of the page's own origin (it holds the page's `CryptoKey` as the
  page does), the Rust compiler, rustls and ring (pinned by version, not
  audited here), the desktop app's runtime.

## Each boundary

### Scoped tokens

*Protects* the rows and collections outside a token's rules. *Against* a
scoped caller using a failing term over a hidden row as an oracle, an
upsert's `else set` reading a row it may not update, `explain` counting
rows before the rules, or a write landing outside its filter. *Enforced*
by `scoped()`, every path that runs a scoped statement, which rewrites it
through `Scope::rewrite_with` (refusing `explain` and schema statements),
`access::and` putting the rules ahead of every term of the caller's, and
the `Check` hook judging every written row a second time
(`before_upsert`, `before_write`). *Not defended*: timing (a `match` or a
walk still costs by rows the caller cannot read), amounts within a field
list, and the server token, which `append-only` does not bind.
Long form: [access.md](access.md#scoped-tokens).

### Fields a rule withholds

*Protects* the fields a read rule does not show. *Against* a scoped caller
using `where`, `order`, `group`, `set` or `returning` over a hidden field
as an oracle, or carrying a hidden value into a shown one. *Enforced* by
`fields::hold` and `fields::write`, which guard or refuse each use before
the rules are ANDed in, and `Scope::shown` and `Scope::shown_schema`,
which mask rows and schema answers. *Not defended*: timing; `id` is always
shown. [access.md](access.md#field-lists).

### Rules reading another collection

*Protects* the rows of the collection a rule reads through. *Against* a
scoped caller growing a rule's `in` list past its bound so that a cut list
would let it in, or a policy whose rules read each other. *Enforced* by
`Scope::bound`, answering each list under the lock, `access::listed`,
refusing one past `MAX_CLAIM_VALUES` rather than cutting it, and
`access::ring`, refusing rings at startup. *Not defended*: availability
(a list past its bound is a refusal). [access.md](access.md#rules-reading-another-collection).

### The server's own collections

*Protects* `_idempotency`, `_consumers`, the webhook events and every
other `_` collection. *Against* a scoped caller's `*` rule, a rule naming
one, or the server's token writing one by statement. *Enforced* by
`access::covers` and fennec-core's `declared::own` (a `*` reaches none),
`access::own_refused` (no statement makes, changes or writes one) and
`path_refusal` (answered as a missing collection on every route). *Not
defended*: the server's token reads them. [access.md](access.md#the-servers-own-collections).

### A token bound to its tenant

*Protects* each tenant's file on a `--dir` node. *Against* one tenant's
scoped token sent to `/t/<other>/`, directly or through the router.
*Enforced* by `route_tenant` asking `Scope::reaches` before the tenant is
looked up, and every tenant's database installing the `Check` hook
(`Tenants::check_scoped_writes`). *Not defended*: there is no admin token
of a tenant's own -- the node's tokens reach every tenant -- and
fennec-server's `--jwt-unbound-tenants` lets a token naming none reach
all. [access.md](access.md#tenant-binding).

### Whom a token is for

*Protects* an app's data from tokens the same keys signed for another
app, project or client. *Enforced* by `Demands::whom`, asking `iss`, `aud`
and `azp` (else `client_id`) in `Access::check`, after the signature and
before the token is kept. *Not defended*: a server told no issuer or
audience takes every token its keys sign (Firebase's sign every
project's); an issuer whose callers name the audience (Google, GitHub
Actions, Kubernetes) needs rules pinning `sub`; `typ` is not read.
[access.md](access.md#whom-a-token-is-for).

### Expiry

*Protects* the time a leaked token is good for. *Enforced* by
`Access::verify` (`exp`, `nbf`, `--jwt-max-age` and `--jwt-require-exp`,
fennec-server's flags), `refusal_code` telling `token_expired` alone, and
a stream ended at its token's `exp` (`sse::EXPIRED`), held in
milliseconds that saturate (`Access::scope`): until #677 an `exp` past
about 1.8e16 wrapped its multiplication by 1 000 and ended a stream early
in a release build, v0.1.11 to v0.2.0 (acceptance was decided apart, in
`f64`). *Not defended*: revocation; clock skew (no leeway is given, `iat`
is not read).
[access.md](access.md#expired-tokens).

### A token's signature

*Protects* the authenticity of every claim. *Against* a forger: `alg`
`none`, HS256 and RS256 confused, a `kid` naming no key, a signature of
the wrong length or out of range, a padding that is not PKCS#1's.
*Enforced* by `Access::check`, which takes the algorithm from the header
but only HS256, RS256 or ES256 and verifies each with a key of its own
kind (an HMAC key never an RSA token's), `read_jwks`, which takes `oct`
keys of 32 bytes, RSA keys of 2048 to 8192 bits (`RsaKey::new`,
counting from the modulus's top bit, an odd exponent from 3, of 64
bits at most) and P-256 keys on the curve, `RsaKey::verify_sha256`,
which builds
the whole encoded message and compares it rather than parsing the
padding, and `P256Key::verify_sha256`, which takes `R || S` with each in
`[1, n)`. A JWKS is a file fennec-server's `--jwt-keys` names, read again
where its time changed, at most once a second, and one that does not
parse read again as it changes or a minute later (`Access::look`; until
#677 a file read half-written was never read again under the same time,
so a key a rotation took out stayed taken, v0.1.7 to v0.2.0): the server
fetches no URL.
*Not defended*: other algorithms (refused); ES256's second signature
`(r, n - s)` and a token's other spellings (base64url's trailing bits
read as they come), neither a forgery, the server keeping verified tokens
by their text (open question 4); public-key arithmetic in variable time,
its inputs public. Wycheproof's RSA files stop at 4 096 bits; the keys'
two ends, 2048 and 8192 bits, are held to signatures OpenSSL made, and
2047 bits and 8200 refused. `RsaKey::new` counted the modulus in whole
bytes, 256 at the least, and so took keys of 2041 to 2047 bits, short
of RFC 7518's 2048 (section 3.3), v0.1.7 to v0.2.0; it counts bits
now, a stricter refusal -- a JWKS file holding such a key is refused
whole -- and before 1.0 a 0.x change, no break of the 1.x surfaces
([compatibility.md](compatibility.md#what-10-refuses-that-v020-took)).
[access.md](access.md#es256-and-the-algorithms-still-refused).

### Empty and short tokens

*Protects* the server's, the admin's and the replication tokens. *Against*
an unset variable read as a token of no length, and a short token guessed
online past loopback. *Enforced* by fennec-server's `env_secret` (a
variable set empty is unset; fennec-shard's `env` does the same),
`Config::settled` (a library's empty token is none), and `short_token`,
refusing at the start a token shorter than `TOKEN_MIN` (32 bytes) where
its listener binds past loopback. *Not defended*: any length on loopback,
warned of once through a proxy. [access.md](access.md#empty-tokens),
[access.md](access.md#short-tokens).

### Guessing a token

*Protects* the server's and the admin's tokens. *Against* an online
guesser and a timing side channel. *Enforced* by `crypto::ct_eq` in
`authenticate`, and the wait after each refusal (`audit::failed_as`:
100 ms doubling to 5 s, by address, an IPv6 one by its /64), which only a
token of its own kind taken clears: the counts keep a server's own tokens
apart from JSON Web Tokens (`audit::Kind`), and a good JWT clears only
the JWTs' (`audit::verified_jwt`) -- until #677 a scoped caller cleared
its count of guesses at the server's or admin token with its own JWT,
v0.1.7 to v0.2.0. *Not defended*: a token's length; a client with more
addresses than the table's 10 000; guesses in parallel up to
`--max-connections`; a request carrying a router's mark waits nothing at
the node (the router waits it by the client's address), and the mark is
one fixed value for each node, so whoever reads an `http://` hop can send
it, naming any client (`audit::router_mark`; SECURITY.md says so). The
comparison's time has no test of its own:
`tools/secret_eq_check.py` holds that every secret goes through `ct_eq`,
not that `ct_eq` keeps its time. [conventions.md](conventions.md#secrets-compared),
[http.md](http.md#audit).

### A server that asks for no token

*Protects* the rows of a server, a node or a router with no data token,
and a router's `/_shard/`. *Against* a page -- a cross-site `POST` needing
no preflight, or a DNS name rebound to 127.0.0.1 -- and a network peer
where it is bound past loopback. *Enforced* by `open_refusal` asking
`cors::foreign` of every request: a `Host` or `X-Forwarded-Host` neither
`localhost`, a name under `.localhost` nor an IP literal
(`cors::local_host`) is 403, and so is an `Origin` neither the server's
own, scheme and all, nor `--http-cors`'s; `exposed` refuses at the start
a listener past loopback with no token unless `--insecure` is given. A
router asks a tenant's request no token, the node does, and a client
names any `Host` it likes -- an IP literal passes `local_host`, this
machine's or not -- so a router listening past loopback forwards nothing
to a node asking no data token (`Router::open_node`: 403, counted in
`fennec_router_open_node_refusals_total`, and 503 where a node has not
said, one from before asked its `/_whoami`), and such a node refuses
what a router past loopback sends, told by its mark
(`audit::routed_past_loopback`; a router on loopback sends another), a
relaying router or the global entry, unless both run `--insecure`. Until
#677 (G14 F4) a token router past loopback opened such a node's every
tenant to any peer naming `Host: localhost`, v0.1.5 to v0.2.0. *Not
defended*: fennec-server's `--insecure`, `--http-cors '*'`, `/_health`
and an allowed preflight under any name, a body read to its ceiling
before the 403; a router on loopback behind a proxy or a tunnel on its
machine, whose clients are the network's (give its nodes a data token).
[http.md](http.md#open-servers).

### CORS

*Protects* which pages may send a request and read its answer. *Against*
a page of another origin, and a rebound page setting `Origin` or
`X-Forwarded-Host` as it likes. *Enforced* by `cors::allows` (a preflight
answered only for `--http-cors`'s origin), `cors::vouched` (the router
drops from a forward an `Origin` it answers for), `cors::is_cors_header`
(a node's CORS headers dropped for the router's own) and fennec-shard's
`forwarded_headers` (a client's `X-Forwarded-Host` replaced by the `Host`
it reached the router by). *Not defended*: `*` on the router vouches every
origin to an open node. [router.md](router.md#cors).

### The router's token and its nodes'

*Protects* `/_shard/` (tenants created, deleted, moved and failed over,
and the directory holding every node's admin token), a node's `/_admin/`
and the queries across tenants. *Against* an anonymous client, a
tenant-bound token, a holder of a node's data token. *Enforced* by
`Router::refused`, comparing fennec-shard's `--token` first in
`Router::admin`, the router sending a node its admin token from the
directory, and a node's `admin::carries`, with a query across tenants run
read-only (`admin_query`); and which tenants exist told to no caller the
router cannot vouch for: a tenant it does not place is a node's own
answer -- its 401, a hook's one 401 -- sent to a node's stand-in, to
anyone but the router's token, a relaying router or the global entry
naming its client (`Router::elsewhere`, `Router::vouches_for`). Until
#677 (G14 F4) a missing tenant was the router's 404, before any token,
v0.1.5 to v0.2.0, and one of another region a 503 naming it, from v0.1.11.
*Not defended*: the router judges no token on `/t/` (the node does); a
router without a token is an open server; a node dialed over `http://`
sees its admin token in the clear; a tenant the global directory homes in
this router's region and its own directory does not hold yet -- moving, or
being made -- is a 503 naming the region to anyone
(`crates/fennec-shard/src/region.rs:371`).
[router.md](router.md#queries-across-tenants).

### A tenant's hook secrets

*Protects* the secrets a tenant's webhooks are verified with. *Against* a
data token, another tenant, a file put in place under a running node
inheriting a set, and a crash or a downgrade leaving a set no binary
reads. *Enforced* by `tenant_hooks::Store`, binding each set to its file
and writing it for its owner alone, `Store::is_current` (an event against
a set no longer the tenant's is the one 401), `Store::set_aside` (a set
that does not read is renamed aside, never removed), and `hook-secrets`
answered to the admin token alone (`admin::handle`). *Not defended*: the
admin token reads every secret; secrets are plain on disk and in memory.
[named-statements.md](named-statements.md#where-the-secret-lives).

### A webhook's event

*Protects* the statements a hook runs with no token. *Against* a forger,
a replayer, and a timing probe of which check failed. *Enforced* by
`hooks::verified`: the headers each read once, one scheme, the signed
time within the hook's tolerance, the body within its ceiling, and two
HMAC-SHA-256s always (`crypto::HmacKey`) over the body to the widest
ceiling, each compared by `ct_eq`, before the body is parsed or a tenant
opened; `hook_of` holds a hook's `keep` past twice its tolerance, so a
replay inside the tolerance meets its key. *Not defended*: a GitHub event,
which carries no signed time, is taken again after its `keep` (7 days by
default); a short secret is warned of, never refused. The `SKEW_MS` term
of `keep`'s bound is not what decides today -- the 1-day floor is -- and
no test would fail without it. [named-statements.md](named-statements.md#verifying-an-event).

### A body's ceiling

*Protects* memory and a request's framing. *Against* a peer declaring a
huge body and a framing read two ways (request smuggling). *Enforced* by
`body_ceiling` (the server's ceiling, past it only an import carrying the
admin token, judged from the head with the body unread), `read_head`,
`chunked_framing` (a `Content-Length` beside `Transfer-Encoding` is 400)
and `read_onto` (a chunked body grows as its bytes come). *Not defended*:
any peer's body is read up to the ceiling before its token is judged, and
a node's admin is read up to the import ceiling.
[http.md](http.md#a-bodys-ceiling), [http.md](http.md#chunked-bodies).

### A slow client and the connection cap

*Protects* threads and connection slots. *Against* a slow-loris peer, a
client that never reads, a flood. *Enforced* by `conn::Paced` (a request
within `--header-timeout` and a second more for every `MIN_RATE` bytes
that came, else 408), the write timeout set on every connection, and
`accept::Slot::take` and `accept::no_thread` (503 past
`--max-connections` or where no thread can be made). *Not defended*: one
peer holding many connections, each within its bounds (the cap is not by
address); a reader slow within `--write-timeout`.
[http.md](http.md#slow-clients), [http.md](http.md#connections-without-a-thread).

### TLS served

*Protects* tokens and data in transit to fennec-server and fennec-shard.
*Enforced* by `conn::from_flags` (`--tls-cert` and `--tls-key` together or
neither) and `Conn::open` and `Conn::secured`, each handshake on its
connection's thread and bounded, a plain request a 400. *Not defended*:
TLS is off without the flags; no client certificates.
[http.md](http.md#tls).

### TLS dialed

*Protects* tokens and data the server and the shell send to a primary, a
node, an OTLP collector. *Against* a stand-in on the path. *Enforced* by
`dial::tls_of` (`https://` is TLS), `dial::secure` and fennec-tls's
`Client::dial_within` (the name checked, the handshake bounded, nothing
sent before), and `dial::trust_ca` (`--tls-ca` in place of Mozilla's
roots, never beside them). A PostgreSQL dial is bounded too, by
fennec-wire's `negotiate`: the lookup, the connect, the answer to its
`SSLRequest`, the handshake and the sign-in share one `connect_timeout`
([PostgreSQL dialed](#postgresql-dialed)). *Not defended*: an `http://`
address goes plain. [http.md](http.md#dialing-https).

### Idempotency keys

*Protects* a write's kept answer, which may hold its returned rows.
*Against* another scoped caller sending someone's key. *Enforced* by
`idempotent::key`, keeping a scoped caller's key under its authority --
16 bytes of a digest of its issuer, subject, tenant and role claims and
every claim a rule reads (`Scope::authority`) -- and refusing a token with
no `sub`, and `idempotent::answered` (the same key with another request
is 422). Until #677 a key was kept under `sub` alone, so a token of the
same subject from another issuer, or with fewer rights, was replayed a
richer one's answer, v0.1.7 to v0.2.0. *Not defended*: every caller on the server's token,
or on an open server, shares one owner; two tokens with the same values
of those claims share keys, as one caller.
[http.md](http.md#idempotency-keys).

### A scoped caller's rate

*Protects* the server's capacity from a scoped caller. *Enforced* by
`rate::admitted`, before anything runs, keyed by the database, the issuer
and the subject (`rate::keyed`). *Not defended*: the server's token is
never held; past the table's callers a new one goes unheld; tokens with
no subject share one count. [quotas.md](quotas.md#a-callers-request-rate).

### MCP's tools

*Protects* what `/query` protects, from an agent holding a token.
*Against* the agent itself, a page, a rebound name. *Enforced* by
`mcp::admit` (the origin asked before the token), reads through
`scoped()` (`tools::read_served`), `tools::write`, which takes `put`,
`set` and `del` alone and runs them as the token, and `mcp::admitted`,
counting a tool call by the token's issuer and subject in the database it
calls (the server's token by the address it calls from), where until
#677 one subject shared one count across every tenant of a node, v0.1.11
to v0.2.0. *Not
defended*: what the token may do, the agent may; tokens with no `sub`
share one count an issuer and database. [http.md](http.md#mcp).

### The studio page

*Protects* the token pasted into the studio, or the session an app hands
it, and the statements it writes. *Against* a script injected through a
row it shows, a page framing it, another origin, a value or a name
crafted to change a statement. *Enforced* by `Studio::new` writing its
CSP and `guarded` putting it, with framing and sniffing refused, on every
answer; the session taken for the page's own origin alone, frozen, never
stored; values sent as parameters and a typed `where` held inside its
parentheses (`statements.js`'s `enclosed` and `filter`). *Not defended*:
the page does what its token may. [http.md](http.md#studio).

### The desktop app's window

*Protects* the server token the app holds and the host the app runs on.
*Against* a page loaded in a window -- a saved remote server's, or
content injected into one. *Enforced* by `sessionPreload` (the session
defined only where `location.origin` is the server's), `holdTo` (a window
held to its origin), `newWindowPreload` (on Windows, every way a page
opens a window filtered) and `serverEnvironment` (the token in an
allowlisted environment, never on argv). *Not defended*: on Windows
WebView2 runs with web security off, so a page's cross-origin reads are
not blocked there; a server's own page gets that server's token.
[integrations.md](integrations.md#studio-desktop).

### A file's bytes

*Protects* the process opening a `.fennec` file, an image or a backup's
contents, and the data it then serves. *Against* a damaged or crafted
file. *Enforced* by `Rec::holds` (a record's CRC32C), `codec::MAX_DEPTH`
(64 levels, else `Error::Corrupt`), `codec::take` (a length checked
against what is left before it is read) and `Schema::decode` (a schema
read off the bytes held to the language's ranges). Fuzzed by the `image`
and `codec` targets. *Not defended*: records below the newest whole claim
are read unchecked by an open (`fennec verify` checks them all).
[storage.md](storage.md#file-format), [crates.md](crates.md#how-deep-a-value-nests).

### A sealed archive's bytes

*Protects* an archive's and a backup's confidentiality and integrity
where they are kept. *Against* whoever reads or writes that storage.
*Enforced* by sealed version 3: `seal::begin` writes a head naming what
the file is -- its role (a segment, an image, a history, a presence, a
backup), its archive's id, its tenant, a segment's first change, an
image's change and time -- and seals the file under a key of its own,
HChaCha20 of the file's id under a key HKDF-Expand makes of the key, so
the count of nonces one key draws is a file's; `Frames::aad` (a frame's
tag covers the tag of the frame before it -- the first frame's, the whole
head -- its place and whether it is the last), and each frame at most a
mebibyte; `seal::vouched_in`, `seal::open_frames` and `seal::open_whole`
(the head vouched for by the first frame, a later version refused by its
number, a whole file cut short refused); and `Archive::held` holding each
file a reader opens to its name and its archive -- a segment to the
change its name says, an image to its change and time, every file to the
archive's id and tenant, a role to the one it is read as -- with
`archive::entries` numbering the writes from there. Fixed in #677 (G14
F3): a segment renamed into a gap, and an image and a backup standing in
for each other, every release that seals, v0.1.7 to v0.2.0; a tenant's
directory or line copied over another's and `presence` and `began`
edited, v0.1.11 to v0.2.0; frames spliced across a cut, one key's
uncounted nonces and a batch's length cut to 32 bits, v0.1.7 to v0.2.0.
*Not defended*: a file cut at a frame's edge opens as the shorter one, as
a crash leaves it (the archive's order of changes notices it in all but
its last segment); an older copy of a file, an archive or a tenant's
line put back, a later file deleted, one backup in another's place; files
sealed by version 1 (0.1.7 to 0.2.0) or 2 (main before 1.0), which still
open and are bound to nothing of what they are -- a version 1 frame
splices across files -- read as the line's own before its first version 3
file. Not fuzzed. [replication.md](replication.md#sealed-archives).

### A primary's stream

*Protects* a replica's file and process, and its promotion. *Against* a
primary sending what it should not, or whoever stands in for one on an
`http://` path (the replica presents its token; the primary proves
nothing back but by TLS). *Enforced* by `Database::apply_records` (a
record's sum and its kind read before it lands, a kind it does not know
refused by name), `Database::land_applied` (each record whole in memory
before the sink takes it); the stream's framing, as a follower, `fennec
archive` and `fennec backup` read it: `kind_head` (a message's kind and
length read before any of its body, each kind held to what a primary
sends of it -- an image or a run of writes to 1 TiB, the rest to a few
bytes), `kind_body` (the body grown as its bytes come, never as
announced, a refused allocation an error in words), `decode` (a count of
writes held to the bytes its message holds, by checked arithmetic) and
`read_head` (the answer's head to 64 KiB); and `Follower::session_caught`
(a panic anywhere in a session said and the follower connecting again,
its `Ends` guard marking the thread ended however it ends, so a halt never
waits on a dead one). Fuzzed by the `stream` target, whose seeds hold a
count of writes past its message and a head with no end. *Not defended*:
the primary is trusted with the data; a peer's words in a log are cut and
escaped, never refused. No test holds a count of writes past its message,
a head past 64 KiB or a panic outside `apply` (the `stream` target's seeds
reach the first two); fixed in #678 (G14 F1), each in every release from
v0.1.5 to v0.2.0, and a panic outside `apply` unsaid in v0.2.0 (any panic
from v0.1.5 to v0.1.12). [replication.md](replication.md#replication).

### A replica's request

*Protects* every write and image of the file or of every tenant, the
primary's durable writes, and which copy is promoted. *Against* a peer
posing as a replica or an archiver. *Enforced* by `Replication::authorized`
(fennec-server's `--replication-token` compared by `ct_eq` first), on a
node `route_tenant` asking that token before a tenant's stream is looked
up, the node's stream (`node_stream::handle`), `History::continues` (a
follower's history judged before it is sent the writes after it), and
`handle_tenant`, which answers no `promote` on a tenant's path: a
tenant's copy is promoted by the node's admin alone
(`/_admin/tenants/<t>/promote`, behind the lease's fence). *Not
defended*: a token's holder reads everything and can ask for an image at
will, and promotes a single server's replica
(`/_replication/promote`); in a `--replicas` region every node and
archiver holds the same token. Until #678 (G14 F1) the token promoted a
tenant's copy past the lease's fence at `/t/<t>/_replication/promote`,
v0.1.5 to v0.2.0. [replication.md](replication.md#replication).

### PostgreSQL dialed

*Protects* the PostgreSQL password, the importer's and the server's
memory and time, and the rows it mirrors. *Against* a hostile server, or
one standing in for it. *Enforced* by fennec-wire's `read_message` (a
message held to the protocol's length before it is allocated);
`Url::parse_in` (`sslmode=verify-full` the default for a host off this
machine, `prefer` on it, as libpq's everywhere; a `%` not followed by two
hex digits, a `?` and a wrong scheme refused without echoing the string);
`RequireAuth` (holding a password, the client signs in by SCRAM-SHA-256
alone unless `require_auth` says otherwise: a cleartext request, or an
`AuthenticationOk` before the exchange ends, refused); `Scram::last` (a
verifier of fewer than 4 096 iterations or more than 2^22, a server nonce
that does not extend the client's, an `m=` extension, each refused before
PBKDF2 runs), SCRAM-SHA-256-PLUS over TLS where offered
(`tls-server-end-point`), with `channel_binding=require`, and
`ServerFinal::verify` (the server's signature compared by `ct_eq`, an
`AuthenticationOk` in its place refused); `negotiate` (libpq's `sslmode`,
one byte read after the server's `S` so nothing it sent before the
handshake is read as sent over TLS, CVE-2021-23222's case; the dial, the
handshake and the sign-in held to `connect_timeout` together); `query()`
and a COPY line held to a bound; `peer_text` (a server's words escaped and
cut before a log or a terminal shows them, and over a connection no CA
checked an error's own text not shown); `ensure_copy` (`--follow`'s copy
made under no lock of the server's, a stop looked at as it waits); and a
keepalive's end confirmed only on a stream proven to be the server's (a
CA-checked TLS, a bound sign-in, or this machine). The `certificate` fuzz
target walks the certificate a channel binding hashes. Fixed in #677
(G14 F2a): the copy under the follower's write lock in every server that
follows (v0.1.5 to v0.2.0), the unbounded dial and answers, the password
answered in clear, `AuthenticationOk` taken with a password held, the
iteration count, nonce and `m=` unchecked and a server's words printed
raw (every release from v0.1.0; v0.2.0 brought TLS, `prefer` the default
everywhere and no channel binding), a keepalive's end confirmed on any
stream (v0.1.5 to v0.2.0), the parameter error echoing part of a password
(v0.2.0). *Not defended*: under `prefer`, `require`, `verify-ca` or
`disable` -- named in the string, or `prefer` on this machine -- a
stand-in gets the rows and can stop `fennec-server --follow`, which exits
on a message it cannot go on from, by design; under `require` it can take
the `-PLUS` offer out and relay the sign-in in plain text unless the
server takes TLS alone (`hostssl`) or the string says
`channel_binding=require`; on such a stream a forged `Commit`'s end is
confirmed as a keepalive's no longer is, so the slot skips every change
before it (`crates/fennec-import/src/follow.rs:796`, confirmed by
`settle`); a client holding no password takes any request; no SASLprep (a
password goes in as its UTF-8 bytes); PBKDF2's own time is held by its
2^22 ceiling, not by `connect_timeout`; the password in `--follow`'s
string is on the process's command line. The rest of the wire and
`pgoutput` is not fuzzed. [replication.md](replication.md#follow).

### A SQLite file

*Protects* the importer's process, and the rows as SQLite reads them.
*Against* a crafted file. *Enforced* by `Pager::payload` (a payload's
length held to the file's before it is allocated, its overflow chain
ending at its length), `Reached` (one bitmap for the b-tree's pages and
the overflow chains': each page reached once), `Walk::cells_apart` (a
leaf's cells refused where two share a byte or one lies outside the cell
area, before a row of the page is yielded, a payload read only as its
cell is) and `MIN_USABLE`; a file beside a hot rollback journal, its
link's target's too, refused; a VIRTUAL generated column, a comment in
`CREATE TABLE` and a `""` column read as SQLite reads them. Fuzzed by the
`sqlite` target, four 64 KiB pages a run. Fixed in #677 (G14 F2b): a leaf
of repeated cell pointers asking for gigabytes, overflow pages reached
again, columns shifted or misread and a hot journal ignored, every
release with the reader, v0.1.0 to v0.2.0. *Not defended*: an import
that stops halfway is not rolled back.
[crates.md](crates.md#a-sqlite-files-lengths).

### The browser module's memory

*Protects* the module's state in the page. *Against* the page's own
script calling its exports with handles and lengths, and an image read
back from storage. *Enforced* by `with_slot` (a handle looked up in the
module's table), `str_from` (text that is not UTF-8 read as lossy text),
`fennec_alloc` (a length past `isize::MAX` refused, a null) and `boxed`
(a null allocation an abort, never a write at address 0), `Database::load`
behind `fennec_load`; and in the glue, `lengthOf` (a length that is not a
whole number of bytes from 0 to 2 147 483 647 refused by name before the
module is handed it, which the room an image is given asks), the write
into the module refusing a null allocation, an address read unsigned, and `guarded` (after a trap, every
call on the module's databases throws, naming it). *Not defended*: the
page owns the module's linear memory -- a pointer and a length are taken
as the page gives them, the same trust domain. No test holds a handle the
module never gave, nor the glue's write's or `boxed`'s answer to an
allocation the page's memory refuses. Until #678 (G50) `loadParts` wrote past its
allocation for a length ToInt32 cut (`2**32 + 8`, `NaN`): v0.1.11 to
v0.2.0. [js-client.md](js-client.md#the-module-and-the-client).

### The native library and the sync

*Protects* the app's process and its replica. *Against* a caller's bad
handle or text, and the server a replica syncs with. *Enforced* by
`native` (a handle in a table, never reused), `call` (a panic caught at
the boundary), `text` (a null with a length, or text not UTF-8, refused
as misuse), the JNI binding's `array_len` (an answer no `byte[]` holds a
query error naming its length, never cut) and `guarded` (each JNI
function's own work under `catch_unwind`), and the sync's `said` (a
server's wait held to its bound). `jni.rs` is built for the tests too, so
a plain `cargo test -p fennec-ffi` runs its own. *Not defended*: a
pointer's validity is the app's; the transport is the binding's. Until
#679 (G14 F6) an answer of 2 GiB or more went over as a negative length,
or cut, v0.1.8 to v0.2.0. [native-and-sync.md](native-and-sync.md#the-native-library),
[native-and-sync.md](native-and-sync.md#sync-core).

### A page's sealed persistence

*Protects* a page's data at rest in IndexedDB, where `persist` was given a
`cryptoKey`, and a replica's writes not yet sent. *Against* whoever reads
or edits the browser's storage without the key. *Enforced* by `seal`
(WebCrypto's AES-GCM, a random 12-byte IV a record) under `aad` (the name
it is stored under, the image's random generation, its place and, from
version 2, the tag of the record before it), `restoreNow` refusing a
sealed record without its key and a clear one with it, and the
replica's `loadPersist` (`FennecSync`'s) refusing to start a replica
whose store does not restore, never seeding over it. Fixed in #679 (G14 F6): chunks sealed again after
a cut opening spliced with those cut, v0.1.7 to v0.2.0; a replica given
the wrong key seeding over its store and the writes it held unsent, from
v0.1.0 (a wrong key from v0.1.7). *Not defended*: `openFile`'s file in
OPFS, never sealed; a store cut at a record's edge, which restores as the
page left it there, and an older store put back whole; until its next
`persist`, a store 0.2.0 sealed (version 1), whose chunks 0.2.0 sealed
after a cut still open among those cut; sizes and record names; a script
of the page's own origin, which holds the `CryptoKey` as the page does;
every record's IV is random under one key, where a repeat gives GHASH's
key away -- some 2^32 records, which no page reaches.
[js-client.md](js-client.md#persistence-in-the-page).

### FennecQL's text

*Protects* the server's stack and what a statement selects. *Against* any
client's text and parameters. *Enforced* by `Parser::deepen` and
`MAX_EXPR_DEPTH` (512 levels, a query error), `Parser::int_in` (a number a
statement writes read as written or refused), and fennec-core's
`param_list` (an `in $n` list held to 100 000 values); values reach a
statement as parameters, never spliced. Fuzzed by the `fennecql` and
`json` targets. *Not defended*: costly work inside the limits.
[crates.md](crates.md#limits).

### Started as root

*Protects* root on the host or the container. *Against* a bug reached over
a connection, which would run as root, and a sidecar sharing the volume.
*Enforced* by `become_user` (groups, group and user set, then checked,
failing unless root cannot be taken back), `wanted` (only a root process
changes its user, fennec-server's `--user` refused otherwise) and
`hand_over` (the volume's own files handed over through descriptors,
before any file is opened, each opened through no link and with no wait
on a FIFO -- `O_NOFOLLOW` and `O_NONBLOCK` by each target's number, a
Unix whose number is not known failing to build -- and held to the inode
it found). *Not defended*: the user empty keeps root;
the drop itself has no test, since CI never runs as root -- the design
file's Docker run is its check. [integrations.md](integrations.md#the-images-user).

### An install

*Protects* the user's machine. *Against* a damaged or truncated download,
and whoever stands between a release and its installer. *Enforced* by
`install.sh`'s `main` and `install.ps1`'s `Get-FennecAsset`, each checking
the asset's sum before it is unpacked, and the Homebrew formula carrying a
sum for each URL (`tools/homebrew-formula.py`). *Not defended*: the sums
come from the same release, so a release replaced whole is not caught as
it installs -- its attestation catches it, verified by hand ([A release's
provenance](#a-releases-provenance)); the `.deb` and `.rpm` are not
signed, and no test holds either refusing a changed byte.
[integrations.md](integrations.md#install-channels).

### A release's provenance

*Protects* the release's assets, the image and the published packages.
*Against* a build step or an action running with a write token, a token
that publishes, and whoever replaces an asset and `SHA256SUMS` together.
*Enforced* by the release's building jobs running with `contents: read`
and a checkout keeping no token (`binaries` and its siblings), which
`tools/workflow_check.py` holds of every workflow (`check_text`: a job
that writes runs only what it lists, never a build, and a job naming no
`permissions:` is refused); `publish` attesting every file it attaches
(a SLSA provenance statement, `actions/attest-build-provenance`) before
it writes `SHA256SUMS`, and never replacing a published asset; `manifest`
attesting the image; RELEASING.md's `tools/release-verify.sh` verifying
each attestation against `release.yml` at the tag's ref (`verified`); and
npm's, PyPI's and NuGet's trusted publishing. *Not defended*: an
attestation names the workflow, the ref and the commit, not that the
source is what its reviewers read, and no build is reproducible; the
installers check a sum, not an attestation, which is the user's to verify
(`gh attestation verify`); the binaries and the Windows zip are unsigned;
the desktop app's lines in `SHA256SUMS` are signed by nothing.
[integrations.md](integrations.md#provenance).

### Dependencies

*Protects* the build and the binaries from code from outside. *Enforced*
by `deps_check.py`'s `check` (only fennec-tls and fennec-bench name a
crate from outside, and no closed crate reaches one) and `check_tls`
(fennec-tls's tree pinned by name and version), and `audit.yml` reading
the packages' advisories (`osv_check.py`). *Not defended*: an advisory
with no fix, taken with an expiry; rustls, ring and webpki-roots by
version, not by advisory. `deps_check.py` has no test of its own.
[crates.md](crates.md#zero-dependencies).

### Another local user

*Protects* the database, its secrets and its single writer. *Against*
another local user and a second process over one file. *Enforced* by
`lock::hold` (`<file>.lock` held by every writer, a second refused), and
on Linux and macOS every file the crates make its owner's alone, mode
`0600`, and every directory `0700`, whatever the umask, by the call that
makes it (`fs::private`, `fs::create_private`, `fs::create_like` -- a file
renamed over another keeping that one's mode -- and
`fs::create_dir_private`), a side file through no link at its name, and
an archive's records, a backup, an unseal, a compact's swap and a
replica's image held to their writer's handle before their rename
(`fs::still_named`); `tools/file_create_check.py`, in CI's fmt job, holds
every call that makes a file to those helpers. On Windows a secret's file
takes a DACL before a byte is written (`owner_only::restrict`). Fixed in
#678 (G14 F5): every release from v0.1.0 to v0.2.0 made its files with
the umask's mode, usually readable by every user, each from the release
that brought it (only `fennec key`'s file and 0.2.0's hook secrets were
`0600`). *Not defended*: a file 0.2.0 or before made, or one its
operator widened, keeps its mode; a user who may write the directory a
database is in, trusted as one who may replace the database: a restore
opens its `.restoring` again by name and renames it in after its
checkpoint, and other side files are renamed in by their name, unheld to
their writer's handle (a tenant's records, its hook secrets, a schedule's
hint, the shell's database rewrite:
`crates/fennec-http/src/tenants.rs:208`, `crates/fennec-http/src/tenant_hooks.rs:411`,
`crates/fennec-http/src/schedule/hint.rs:243`, `crates/fennec-cli/src/main.rs:963`);
on Windows a file takes its folder's access list; a lock is not kept
across a bind mount or a network file system; a user who may read
`<file>.lock` may hold a lock on it. [engine.md](engine.md#single-writer).

### Secrets kept out of logs

*Protects* tokens, hook secrets and a statement's literals. *Against*
whoever reads stderr, the audit log, the statements' counts or an OTLP
collector. *Enforced* by `audit::refusal` and `audit::hook_refused`,
naming the check that failed and never the value, and `statements::shape`,
writing a literal as `$n`. *Not defended*: a token on the command line is
in `ps`; `hook-secrets` answers the admin token with every secret. No
test holds that a refused token's value is never logged: the type of the
refusal's reason, a `&'static str`, does. [http.md](http.md#audit).

### The system's randomness

*Protects* keys, a seal's nonces and file ids, SCRAM's nonces. *Against* a
nonce used twice or one read off the clock, as a server at its file
descriptors' limit once did. *Enforced* by `crypto::system_random`
(`getrandom`, `getentropy`, `BCryptGenRandom`, with no descriptor;
`/dev/urandom`, which takes one, only where Linux's call is missing or
denied -- `ENOSYS`, `EPERM` -- or on another Unix; `None` where the
system or that file gives nothing, and the seal or the sign-in refused),
and `id_u64`, the only one that falls back to the clock, for ids alone.
No test holds `fill` answering nothing, nor the `/dev/urandom` fallback:
the tests' `without_randomness` answers `None` above it.
[replication.md](replication.md#sealed-archives).

## For a reviewer

The scope package for an outside review of the code written by hand and
never reviewed from outside (the 1.0 plan's G14): what each file
implements and against which specification, what holds it today, which
fuzz target reaches it, and the questions to start from. Lines are `wc -l`
at the commit this was written against: the code before the file's test
module, then the whole file.

### What was written by hand

| Code | Lines | Implements | Specification | Vectors and known answers |
|---|---|---|---|---|
| `crates/fennec-http/src/crypto.rs` | 821 of 1 429 | SHA-256; HMAC-SHA-256 and `HmacKey`; PBKDF2-HMAC-SHA-256, one block; ChaCha20, HChaCha20, Poly1305 and their AEAD; base64 and base64url; `ct_eq`; the system's randomness; RS256 verification (`RsaKey`) | FIPS 180-4; RFC 2104, 4231; RFC 8018 section 5.2; RFC 8439; draft-irtf-cfrg-xchacha section 2.2; RFC 4648; RFC 8017 section 8.2.2, RFC 7518 section 3.3 | its test module (`sha256_vectors`, `hmac_rfc4231_all`, `pbkdf2_sha256_vectors`, `aead_rfc8439` and the appendix A tests, `hchacha20_draft_irtf_cfrg_xchacha`, `rsa_pkcs1_sha256_as_node_signs`, `rs256_at_each_end_of_the_keys_it_takes`: 2048 and 8192 bits and just past each, signatures OpenSSL made); `crates/fennec-http/tests/wycheproof.rs`, every case of Wycheproof's ChaCha20-Poly1305, HMAC-SHA-256, PBKDF2-HMAC-SHA-256 and RSA PKCS#1 v1.5 SHA-256 at 2048, 3072 and 4096 bits |
| `crates/fennec-http/src/crypto/p256.rs` | 485 of 646 | ES256 verification: P-256's field and scalars in Montgomery form, Jacobian points, Shamir's trick | FIPS 186-5 section 6.4.2; SEC 1 section 4.1.4; RFC 7518 section 3.4 | `crates/fennec-http/tests/es256.rs`: Wycheproof's P1363 file (262 cases), RFC 7515 A.3, tokens node:crypto signed; the curve's identities in the file's tests |
| `crates/fennec-http/src/crypto/unix.rs`, `windows.rs` | 76 and 42 of 76 and 62 | the system's randomness | getrandom(2), getentropy(3), BCryptGenRandom | `random_is_not_constant`, `no_randomness_refuses_nonces_and_names_ids`, `randomness_needs_no_descriptor` |
| `crates/fennec-http/src/seal.rs` | 799 of 1 544 | sealed files, version 3: the head naming what the file is (its role, archive, change, time and tenant), a key a file (HChaCha20 of its id under a key HKDF-Expand makes of the key), frames chained by their tags, each bound to its place and whether it is the last; versions 1 and 2 opened | RFC 8439's AEAD; draft-irtf-cfrg-xchacha's HChaCha20; RFC 5869's HKDF-Expand; the framing is ours | its test module (18 tests, `the_file_keys_key_is_hkdf_expand` against RFC 5869's cases and `a_file_key_is_the_one_another_reader_makes` against a vector computed apart from this code among them); `crates/fennec-http/tests/archive.rs`, `node_archive.rs` |
| `crates/fennec-wire/src/client.rs` | 2 453 of 3 246; SCRAM, `require_auth` and channel binding about 500, `sslmode` and TLS about 370 | the client half of SCRAM-SHA-256 and SCRAM-SHA-256-PLUS (`tls-server-end-point`); libpq's `sslmode`, `require_auth` and `channel_binding`; the protocol's framing | RFC 5802, RFC 7677, RFC 5929 section 4; PostgreSQL's protocol 3.0 | `scram_rfc7677_exchange`, held to RFC 7677's messages byte for byte; `scram_iterations_are_held_to_their_range`; `crates/fennec-wire/tests/client.rs`, `tls.rs`; the `certificate` fuzz target |
| `crates/fennec-tls/src/lib.rs` | 1 151 of 2 088 | rustls's use: a server's certificate and key, a client's roots and name check, a record read in parts at a pace | RFC 8446, through rustls | `crates/fennec-http/tests/tls.rs`, `dials.rs`; `crates/fennec-wire/tests/tls.rs` |
| `crates/fennec-http/src/access.rs`, `access/fields.rs` | 4 428 of 6 056, and 938 | bearer and JSON Web Tokens: a token's header, signature and claims (about 700 lines from `Access` to `Access::check`), JWKS files, policies and their rules, field lists | RFC 7515, 7517, 7518, 7519 | `crates/fennec-http/tests/access.rs`, `fields.rs`, `providers.rs`, `claim_values.rs`, `rules_first.rs`, `es256.rs`; the file's tests |
| `crates/fennec-http/src/hooks.rs`, `tenant_hooks.rs` | 1 605 of 2 355, and 1 110 | a webhook's verification: Stripe's, GitHub's, Standard Webhooks' and Svix's schemes; a tenant's secrets | the Standard Webhooks specification; Stripe's and GitHub's documented schemes | `each_scheme_verifies_its_senders_example` (each sender's own example); `crates/fennec-server/tests/hooks.rs`; `crates/fennec-http/tests/tenant_hooks.rs` |
| `crates/fennec-http/src/cors.rs` and `open_refusal` | 290 of 437, and 20 | the open server's guard: `Host` and `Origin`, CORS | RFC 6454; RFC 6761 section 6.3; the Fetch standard's CORS | `crates/fennec-http/tests/open_servers.rs`; `crates/fennec-shard/tests/cors.rs`; the file's tests |
| `web/fennec.js`'s `seal`, `unseal`, `aad`, `persist` and `restore` | about 200 of 4 387 | a page's image and chunks sealed with a `CryptoKey`, version 2: each record's tag covering the tag of the record before it | WebCrypto's AES-GCM, NIST SP 800-38D | `web/fennec.persist.test.js`, `web/fennec.sync.test.js` |

The cryptography is 2 223 lines of code (3 757 with its tests); with the
PostgreSQL client, the use of TLS, the tokens, the webhooks, the guard and
the page's sealing, about 14 400, of which the tokens' policies and field
lists are some 4 600 a review of the cryptography and the sign-in can
leave for last. Of it a fuzz target reaches only fennec-tls's walk of a
PostgreSQL server's certificate (`certificate`).

### The fuzzing it has

The eight targets of `crates/fennec-fuzz` (`make fuzz` lists them) run
under AddressSanitizer a fixed count of runs on every pull request
(`fuzz-smoke`: `http` 120 000, `json` 70 000, `fennecql` 180 000, `codec`
60 000, `image` 35 000, `sqlite` 45 000, `certificate` 300 000, `stream`
90 000) and half an hour each a week (`fuzz.yml`);
[conventions.md](conventions.md#fuzzing) has how. `certificate` walks the
certificate a PostgreSQL server shows to the hash its channel binding
takes, and `stream` reads a primary's answer as a replica, an archiver
and a backup read it. None reaches the cryptography above, a sealed
file's or an archive's framing, JWT and JWKS reading, a webhook's
headers, the SCRAM exchange or the rest of the PostgreSQL wire and
`pgoutput`: SECURITY.md lists them as not fuzzed. The `json` target
reaches the parser a token's claims and a JWKS file are read with, and
the `http` target the request a guard is asked of, not the guard. The
`unsafe` code of the crates an app's or a page's pointers reach --
fennec-ffi, fennec-abi, fennec-core -- runs its tests under
AddressSanitizer in CI's `sanitize` job, and every `unsafe` block says
why it is sound ([conventions.md](conventions.md#sanitizers)); Miri over
the pure-Rust `unsafe` paths is the plan's G50 still to come.

### Open questions

Ranked: the first three are where a mistake would cost the most and where
reading did not settle it.

1. **Nonces under long-lived keys.** Answered for archives by #677 (G14
   F3): a version 3 file seals under a key of its own, HChaCha20 of its
   random id under a key HKDF-Expand makes of the operator's key (the
   XChaCha20 construction), its frames' nonces still random, so the
   frames one nonce space covers are a file's -- a segment of 64 MB, an
   image -- not every frame a key ever sealed. Still so: the file ids are
   128 random bits under one key, and a page's `persist` draws a random
   IV for every record under one AES-GCM key, where a repeat gives away
   GHASH's key (some 2^32 records, which no page reaches). Is either
   owed more?
2. **Truncation and rollback.** Since #677 and #679 frames and a page's
   records are chained, so a run sealed again after a cut no longer
   splices with what was cut, and a file is held to its name, archive and
   tenant. What stays, by design and in SECURITY.md: `seal::open_appended`
   opens a segment cut at any frame's edge, as a crash leaves one, and a
   page's store cut at a record's edge restores as it stood; an older
   file, archive, line or store put back opens as what it was, and so does
   a line with a later file deleted -- a tenant's `presence` gone, the
   tenant restores as never gone. Would an archive's manifest, signed
   apart, or the last frame's count be worth its cost, or is a bucket that
   keeps versions the answer?
3. **SCRAM's downgrades, and a stream not proven.** Answered by #677 (G14
   F2a): holding a password the client signs in by SCRAM alone, libpq
   16's `require_auth` rule (an `AuthenticationOk` before the exchange
   ends, and a cleartext request, refused unless the string says so),
   offers `SCRAM-SHA-256-PLUS` over TLS, takes 4 096 iterations at the
   least, and dials `verify-full` by default off this machine. What
   stays: under `prefer`, `require`, `verify-ca` or `disable` a stand-in
   still gets the rows, can stop the follower, and under `require`
   relays the sign-in unless `hostssl` or `channel_binding=require` keeps
   it out; on such a stream a forged `Commit`'s end is confirmed to the
   real server's slot (`crates/fennec-import/src/follow.rs:796`). Should
   `--follow` refuse a mode short of `verify-full` off this machine, and
   hold a `Commit`'s end on a stream not proven as it holds a
   keepalive's?
4. **ECDSA malleability and a token's spellings.** `(r, n - s)` verifies
   as `(r, s)` does (JWS does not ask for low S), and `b64url_decode`
   takes non-canonical trailing bits, so a token has several spellings. A
   server keeps verified tokens by their text (`Access::verified`, a
   `HashMap` looked up by the token, so compared with a guess by `==`):
   the internal review found no security decision keyed on that text and
   the lookup not exploitable (SipHash under a random key, a 7-bit tag,
   `memcmp` in words), but a reviewer should look again.
5. **RSA PKCS#1 v1.5's strictness.** `RsaKey::verify_sha256` builds the
   whole encoded message, its `DigestInfo` with NULL parameters, and
   compares it, never parsing the padding -- so the form without
   parameters is refused (Wycheproof's `MissingNull` case, on purpose).
   The key: 2048 to 8192 bits, counted from the modulus's top bit (in
   whole bytes until v0.2.0, which took 2041), an odd exponent of at
   most 64 bits from 3, `R^2 mod n` by doubling;
   Montgomery multiplication (CIOS) over 32 to 128 limbs. Wycheproof's
   files reach 4 096 bits; both ends, and just past each, are held to
   signatures OpenSSL made (`rs256_at_each_end_of_the_keys_it_takes`).
6. **Constant time.** Claimed only for comparisons of secrets (`ct_eq`, a
   loop through `black_box`, best effort; a length is no secret here).
   ChaCha20 and Poly1305 (26-bit limbs) have no table and no branch on a
   secret; HMAC's key setup branches on the key's length alone. A secret
   read through `b64_decode` (a webhook's `whsec_` secret, at the start)
   meets a branch for each character's class. RSA and P-256 work on
   public values. A bearer token is looked up among those verified by a
   hash map (question 4). Is there a comparison or a table lookup on a
   secret left?
7. **The JWT's header.** `alg` must be HS256, RS256 or ES256 and the key's
   kind must match; `kid` only narrows the keys tried (a token naming none
   is tried against all); `crit` is not read, where RFC 7515 section
   4.1.11 has a token naming an extension the reader does not know
   refused; `typ` is not read, so an ID token and an access token from one
   issuer are told apart by `aud` alone.
8. **The clock.** `exp` and `nbf` are compared with no leeway and `iat` is
   not read; a server whose clock runs late takes an expired token for as
   long as it is late. An `exp` past about 1.8e16, which overflowed a
   multiplication by 1 000 and ended a stream early, saturates since #677
   (`Access::scope`).
9. **JWKS fetching and caching.** None: a JWKS is a file, read again where
   its time changed, at most once a second, the old keys kept where the
   new file does not read, and since #677 a file that does not parse read
   again as it changes or a minute later (`Access::look`), where one read
   half-written was never read again under the same time. A rotation
   written within the file system's time resolution, or by a tool keeping
   the time, is still missed until the next change. Keys a server
   verified a token with are dropped from its cache at a reload.
10. **A webhook's replay window.** A Stripe or Standard Webhooks event is
    taken within the hook's tolerance (1 s to 1 h), its key kept past
    twice that; a GitHub event, which signs no time, is answered by its
    body's digest for `keep` (7 days by default) and taken again after.
11. **PBKDF2's count and SCRAM's nonce.** A server's iteration count is
    held to 4 096 to 2^22 (from 1 until #677); the client's nonce is 24
    random bytes; the server's must extend it, strictly longer, where it
    was asked only to begin with it. `pbkdf2_sha256` computes one block,
    all SCRAM needs; a password goes in as its UTF-8 bytes, with no
    SASLprep.

## An outside audit

**Decided: fennecdb 1.0 ships without a paid outside audit and does not
wait for one.** It is alpha today, with no budget set aside for an audit,
and the plan's exit for this item (G14) takes either a published review
with its high and critical findings fixed, or this decision written in
SECURITY.md; it is the second, in SECURITY.md's own words: "No outside
audit has been made of this code, and 1.0 does not wait for one".

What stands in for it before 1.0: a review independent of the code's
author over the scope [above](#what-was-written-by-hand), starting from
the [open questions](#open-questions), every finding it makes fixed with
a regression test in the crate it is in. The vectors (the plan's G6), the
fuzz targets (G5) and this threat model (G2) are what it is handed.

It was made on 2026-11-15, by four reviewers reading the code alone, each
an area: the primitives and sealed files (A), the tokens at the HTTP edge
and the router (B), the PostgreSQL client, the importer and the SQLite
reader (C), and replication's streams, an archive's restore, the sync and
both C ABIs (D). Five of its findings were medium -- a sealed file bound
to nothing of what it is (A), an open node behind a token router open to
the network (B), fennec-server's `--follow` copying under its write lock
and a stand-in poisoning the slot under the default `sslmode` (C), a
tenant's copy promoted past the lease's fence (D) -- the rest low or
hardening. Each fixed one was fixed with a test that fails without it, in
#677 (F2a, F2b, F3, F4), #678 (F1, F5, and the page's lengths in G50) and
#679 (F6); every row above names the fix, its test and the releases that
had the bug, and corrects what the reviewers found the row had wrong.
What it left open is in its row's *Not defended* or "no test", with its
place in the code where it is one.

What keeps the door open: this section and the scope package are kept
current with the code -- a file of the scope growing or a primitive added
updates the table, whose boundaries the check holds to the tree -- so that
an audit can be commissioned from a firm, or funding applied for (OSTIF,
Alpha-Omega), whenever the owner chooses, with nothing to rebuild first.
SECURITY.md's sentences are held by its check: the promise is "no outside
audit has been made", and a sentence saying the code was audited is
refused while none was.
