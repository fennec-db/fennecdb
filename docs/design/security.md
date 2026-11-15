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
link, anchor and flag of this file resolves. A function renamed or a test
deleted fails the check, naming the row. What it cannot hold is the claim
each row makes: that the test cited would fail with the enforcement taken
away. That was read, not run -- each test's deciding assertion against the
item it stands for -- and a reviewer's first check is to try some.

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
| 7 | [Expiry](#expiry) | a stale token's holder | `crates/fennec-http/src/access.rs::Access::verify`, `crates/fennec-http/src/access.rs::refusal_code`, `crates/fennec-http/src/sse.rs::EXPIRED` | `crates/fennec-http/src/access.rs::only_an_expired_token_is_told_by_its_code`, `crates/fennec-http/tests/access.rs::a_subscription_ends_when_its_token_expires` |
| 8 | [A token's signature](#a-tokens-signature) | a forger | `crates/fennec-http/src/access.rs::Access::check`, `crates/fennec-http/src/access.rs::read_jwks`, `crates/fennec-http/src/crypto.rs::RsaKey::verify_sha256`, `crates/fennec-http/src/crypto/p256.rs::P256Key::verify_sha256` | `crates/fennec-http/src/access.rs::a_forged_or_stale_token_is_refused`, `crates/fennec-http/src/access.rs::an_identity_providers_keys_verify_its_tokens`, `crates/fennec-http/tests/wycheproof.rs::rsa_pkcs1_sha256_wycheproof_2048`, `crates/fennec-http/tests/es256.rs::every_wycheproof_p256_sha256_case`, `crates/fennec-http/tests/es256.rs::a_server_takes_es256_tokens_and_refuses_them_changed` |
| 9 | [Empty and short tokens](#empty-and-short-tokens) | a network peer | `crates/fennec-server/src/main.rs::env_secret`, `crates/fennec-http/src/lib.rs::Config::settled`, `crates/fennec-http/src/lib.rs::short_token`, `crates/fennec-http/src/lib.rs::TOKEN_MIN` | `crates/fennec-http/tests/empty_tokens.rs::an_empty_admin_token_leaves_admin_off`, `crates/fennec-shard/tests/tokens.rs::a_token_set_empty_is_no_token`, `crates/fennec-http/src/lib.rs::a_token_past_loopback_is_32_bytes_at_least`, `crates/fennec-server/tests/tokens.rs::a_short_token_is_refused_outside_loopback` |
| 10 | [Guessing a token](#guessing-a-token) | a network peer | `crates/fennec-http/src/crypto.rs::ct_eq`, `crates/fennec-http/src/lib.rs::authenticate`, `crates/fennec-http/src/audit.rs::failed` | `crates/fennec-http/tests/audit.rs::refusals_wait_and_schema_changes_and_refusals_are_logged`; no test of `ct_eq`'s time, which `tools/secret_eq_check.py` holds by its callers alone |
| 11 | [A server that asks for no token](#a-server-that-asks-for-no-token) | a page; a network peer | `crates/fennec-http/src/lib.rs::open_refusal`, `crates/fennec-http/src/cors.rs::foreign`, `crates/fennec-http/src/cors.rs::local_host`, `crates/fennec-http/src/lib.rs::exposed` | `crates/fennec-http/tests/open_servers.rs::a_rebound_name_is_refused_a_read_included`, `crates/fennec-http/tests/open_servers.rs::another_origins_post_is_refused_before_it_runs`, `crates/fennec-server/tests/start.rs::an_open_endpoint_without_a_token_is_refused_before_the_open` |
| 12 | [CORS](#cors) | a page | `crates/fennec-http/src/cors.rs::allows`, `crates/fennec-http/src/cors.rs::vouched`, `crates/fennec-http/src/cors.rs::is_cors_header`, `crates/fennec-shard/src/lib.rs::forwarded_headers` | `crates/fennec-http/tests/api.rs::a_preflight_is_answered_before_the_token_is_asked_for`, `crates/fennec-shard/tests/cors.rs::a_forwarded_answer_tells_the_routers_rule_once`, `crates/fennec-shard/tests/cors.rs::a_router_answers_for_the_origins_it_lets_in_to_an_open_node`, `crates/fennec-shard/tests/cors.rs::a_token_router_tells_an_open_node_the_name_its_client_reached_it_by` |
| 13 | [The router's token and its nodes'](#the-routers-token-and-its-nodes) | a network peer; another tenant | `crates/fennec-shard/src/lib.rs::Router::refused`, `crates/fennec-shard/src/lib.rs::Router::admin`, `crates/fennec-http/src/admin.rs::carries`, `crates/fennec-http/src/lib.rs::admin_query` | `crates/fennec-shard/tests/router.rs::refusals`, `crates/fennec-shard/tests/fanout.rs::a_token_bound_to_one_tenant_reaches_no_query_across_them` |
| 14 | [A tenant's hook secrets](#a-tenants-hook-secrets) | another tenant; a data token | `crates/fennec-http/src/tenant_hooks.rs::Store`, `crates/fennec-http/src/tenant_hooks.rs::Store::is_current`, `crates/fennec-http/src/tenant_hooks.rs::Store::set_aside`, `crates/fennec-http/src/admin.rs::handle` | `crates/fennec-http/tests/tenant_hooks.rs::a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone`, `crates/fennec-http/tests/tenant_hooks.rs::a_file_put_in_place_under_a_running_node_is_armed_by_no_secret_before_it`, `crates/fennec-http/tests/tenant_hooks.rs::a_sidecar_that_does_not_read_is_set_aside_never_removed` |
| 15 | [A webhook's event](#a-webhooks-event) | a webhook's sender | `crates/fennec-http/src/hooks.rs::verified`, `crates/fennec-http/src/crypto.rs::HmacKey`, `crates/fennec-http/src/hooks.rs::hook_of` | `crates/fennec-http/src/hooks.rs::a_timestamp_past_the_tolerance_is_stale`, `crates/fennec-http/src/hooks.rs::each_scheme_verifies_its_senders_example`, `crates/fennec-server/tests/hooks.rs::a_stripe_event_is_taken_once_by_its_key`; no test of `SKEW_MS` in a hook's `keep`, which the 1-day floor decides today |
| 16 | [A body's ceiling](#a-bodys-ceiling) | a network peer | `crates/fennec-http/src/lib.rs::body_ceiling`, `crates/fennec-http/src/http.rs::read_head`, `crates/fennec-http/src/http.rs::chunked_framing`, `crates/fennec-http/src/http.rs::read_onto` | `crates/fennec-http/tests/tenants.rs::only_an_import_carrying_the_admin_token_is_read_past_the_data_ceiling`, `crates/fennec-http/tests/chunked.rs::a_framing_read_two_ways_is_refused_and_the_connection_closed`, `crates/fennec-http/src/http.rs::a_declared_chunk_grows_the_body_only_as_its_bytes_come` |
| 17 | [A slow client and the connection cap](#a-slow-client-and-the-connection-cap) | a network peer | `crates/fennec-http/src/conn.rs::Paced::timed`, `crates/fennec-http/src/conn.rs::MIN_RATE`, `crates/fennec-http/src/accept.rs::Slot::take`, `crates/fennec-http/src/accept.rs::no_thread` | `crates/fennec-http/tests/server.rs::a_trickled_body_is_cut`, `crates/fennec-http/tests/server.rs::a_trickled_head_is_cut_at_its_deadline`, `crates/fennec-http/tests/server.rs::connection_limit_refuses_extra_connections`, `crates/fennec-http/tests/api.rs::a_connection_with_no_thread_is_answered_503_and_told_when_to_come_back` |
| 18 | [TLS served](#tls-served) | a network peer on the path | `crates/fennec-http/src/conn.rs::from_flags`, `crates/fennec-http/src/conn.rs::Conn::open`, `crates/fennec-http/src/conn.rs::Conn::secured` | `crates/fennec-http/tests/tls.rs::plain_http_is_told_to_ask_over_https`, `crates/fennec-http/tests/tls.rs::a_handshake_past_its_bound_is_closed` |
| 19 | [TLS dialed](#tls-dialed) | a stand-in on the path | `crates/fennec-http/src/dial.rs::tls_of`, `crates/fennec-http/src/dial.rs::secure`, `crates/fennec-http/src/dial.rs::trust_ca`, `crates/fennec-tls/src/lib.rs::Client::dial_within` | `crates/fennec-http/tests/dials.rs::a_name_the_certificate_does_not_hold_is_refused_before_a_byte_is_sent`, `crates/fennec-http/tests/dials.rs::a_handshake_never_answered_is_a_timeout_at_the_dial_s_bound` |
| 20 | [Idempotency keys](#idempotency-keys) | a scoped caller | `crates/fennec-http/src/idempotent.rs::key`, `crates/fennec-http/src/idempotent.rs::answered` | `crates/fennec-http/tests/idempotency.rs::a_key_is_for_one_request_one_user_and_a_write_that_landed` |
| 21 | [A scoped caller's rate](#a-scoped-callers-rate) | a scoped caller | `crates/fennec-http/src/rate.rs::admitted`, `crates/fennec-http/src/rate.rs::keyed` | `crates/fennec-http/tests/request_rate.rs::a_caller_past_the_rate_is_told_its_wait_and_nothing_of_it_runs` |
| 22 | [MCP's tools](#mcps-tools) | a scoped caller's agent; a page | `crates/fennec-http/src/mcp/mod.rs::admit`, `crates/fennec-http/src/mcp/tools.rs::read_served`, `crates/fennec-http/src/mcp/tools.rs::write` | `crates/fennec-http/tests/mcp.rs::a_scoped_token_s_agent_sees_what_its_query_sees` |
| 23 | [The studio page](#the-studio-page) | a page; a stored row | `crates/fennec-http/src/studio.rs::Studio::new`, `crates/fennec-http/src/studio.rs::guarded`, `studio/statements.js::enclosed`, `studio/statements.js::filter` | `crates/fennec-server/tests/studio.rs::the_studio_is_served_with_its_headers`, `studio/test/e2e.test.mjs::embedded in an app: the session it hands over, never stored, and no form to sign in with`, `studio/test/statements.test.mjs::a cell is written by id, the value a parameter, the count required` |
| 24 | [The desktop app's window](#the-desktop-apps-window) | a page loaded in the app | `integrations/studio-desktop/src/bun/session.ts::sessionPreload`, `integrations/studio-desktop/src/bun/windows.ts::holdTo`, `integrations/studio-desktop/src/bun/opener.ts::newWindowPreload`, `integrations/studio-desktop/src/bun/env.ts::serverEnvironment` | `integrations/studio-desktop/test/session.test.ts::a page that is not the server's gets nothing: any other origin, a lookalike, another scheme or port`, `integrations/studio-desktop/test/process.test.ts::a window's server: loopback, any port, the token in its environment and in no argument, and a stop that is its shutdown` |
| 25 | [A file's bytes](#a-files-bytes) | whoever hands the process a file | `crates/fennec-core/src/engine.rs::Rec::holds`, `crates/fennec-core/src/codec.rs::MAX_DEPTH`, `crates/fennec-core/src/codec.rs::take`, `crates/fennec-core/src/schema.rs::Schema::decode` | `crates/fennec-core/tests/persist.rs::a_page_lost_inside_a_synced_vector_record_refuses_the_open`, `crates/fennec-core/src/codec.rs::a_value_nests_no_deeper_than_json_may_be_written`, `crates/fennec-core/src/codec.rs::a_count_of_items_past_usize_is_refused`, `crates/fennec-core/tests/damage.rs::a_schema_past_the_languages_ranges_is_refused_where_a_file_is_opened_and_applied` |
| 26 | [A sealed archive's bytes](#a-sealed-archives-bytes) | whoever reads or writes where archives are kept | `crates/fennec-http/src/seal.rs::FileId::aad`, `crates/fennec-http/src/seal.rs::open_frames`, `crates/fennec-http/src/seal.rs::open_whole`, `crates/fennec-http/src/archive.rs::entries` | `crates/fennec-http/src/seal.rs::a_frame_from_another_file_does_not_open`, `crates/fennec-http/src/seal.rs::frames_out_of_place_do_not_open`, `crates/fennec-http/src/seal.rs::an_id_cut_short_or_changed_does_not_open`, `crates/fennec-http/tests/archive.rs::a_sealed_archive_restores_with_its_key_and_nothing_else` |
| 27 | [A primary's stream](#a-primarys-stream) | a primary, or a stand-in on a plain path | `crates/fennec-core/src/engine.rs::Database::apply_records`, `crates/fennec-core/src/engine.rs::Database::land_applied`, `crates/fennec-http/src/replication.rs::Follower::session` | `crates/fennec-core/src/engine/unwinding.rs::a_panic_anywhere_in_applying_a_record_puts_it_back_whole`, `crates/fennec-core/tests/damage.rs::a_primarys_record_nesting_past_the_limit_is_refused_by_its_replica`, `crates/fennec-http/tests/replication.rs::a_panic_applying_a_write_ends_the_follower_and_never_its_halt`; no test of a record kind refused by name, of the primary's sums checked on apply, or of the stream's framing (`Receiver::receive`) |
| 28 | [A replica's request](#a-replicas-request) | a network peer posing as a replica | `crates/fennec-http/src/replication.rs::Replication::authorized`, `crates/fennec-http/src/node_stream.rs::handle`, `crates/fennec-core/src/history.rs::History::continues` | `crates/fennec-http/tests/empty_tokens.rs::an_empty_replication_token_opens_no_stream`, `crates/fennec-http/tests/replication.rs::a_promoted_replica_forks_and_the_old_primary_rejoins_under_it`, `crates/fennec-core/src/history.rs::a_history_continues_up_to_where_it_was_left` |
| 29 | [PostgreSQL dialed](#postgresql-dialed) | a PostgreSQL server, or a stand-in | `crates/fennec-wire/src/proto.rs::read_message`, `crates/fennec-wire/src/client.rs::scram_iterations`, `crates/fennec-wire/src/client.rs::negotiate`, `crates/fennec-wire/src/client.rs::ServerFinal::verify` | `crates/fennec-wire/src/proto.rs::a_message_s_length_is_held_to_the_protocol_and_never_aborts`, `crates/fennec-wire/src/client.rs::scram_rfc7677_exchange`, `crates/fennec-wire/tests/client.rs::scram_refuses_iterations_past_the_bound`, `crates/fennec-wire/tests/tls.rs::verify_full_refuses_a_name_the_certificate_does_not_hold_before_a_byte_is_sent` |
| 30 | [A SQLite file](#a-sqlite-file) | whoever hands the importer a file | `crates/fennec-import/src/sqlite.rs::Pager::payload`, `crates/fennec-import/src/sqlite.rs::Walk::visit`, `crates/fennec-import/src/sqlite.rs::MIN_USABLE` | `crates/fennec-import/tests/sqlite.rs::a_payload_longer_than_the_file_is_refused_before_it_is_allocated`, `crates/fennec-import/tests/sqlite.rs::a_page_reached_twice_is_refused_not_walked_for_ever`, `crates/fennec-import/tests/sqlite.rs::a_page_under_sqlites_usable_floor_is_refused` |
| 31 | [The browser module's memory](#the-browser-modules-memory) | the embedding page; a stored image | `crates/fennec-wasm/src/lib.rs::with_slot`, `crates/fennec-wasm/src/lib.rs::str_from`, `crates/fennec-wasm/src/lib.rs::fennec_alloc` | `crates/fennec-wasm/src/lib.rs::text_that_is_not_utf8_reads_as_from_utf8_lossy_reads_it`; no test of a handle the module never gave, nor of an allocation the page's memory refuses |
| 32 | [The native library and the sync](#the-native-library-and-the-sync) | the embedding app; the server a replica syncs with | `crates/fennec-ffi/src/lib.rs::native`, `crates/fennec-ffi/src/lib.rs::call`, `crates/fennec-ffi/src/lib.rs::text`, `crates/fennec-abi/src/sync/mod.rs::said` | `crates/fennec-ffi/tests/ffi.rs::an_error_is_its_code_and_its_json`, `crates/fennec-ffi/tests/ffi.rs::a_closed_handle_is_refused_and_never_reused`, `crates/fennec-abi/src/sync/mod.rs::an_answer_says_its_quota_and_its_wait_or_nothing` |
| 33 | [A page's sealed persistence](#a-pages-sealed-persistence) | whoever reads or edits the browser's storage | `web/fennec.js::aad`, `web/fennec.js::seal`, `web/fennec.js::restoreNow` | `web/fennec.persist.test.js::a sealed database restores with its key and nothing else` |
| 34 | [FennecQL's text](#fennecqls-text) | any client | `crates/fennec-ql/src/parser.rs::Parser::deepen`, `crates/fennec-ql/src/parser.rs::MAX_EXPR_DEPTH`, `crates/fennec-ql/src/parser.rs::Parser::int_in`, `crates/fennec-core/src/query.rs::param_list` | `crates/fennec-ql/src/lib.rs::expression_depth_is_bounded`, `crates/fennec-core/tests/in_param.rs::the_list_is_bounded_and_must_be_one` |
| 35 | [Started as root](#started-as-root) | a bug reached over a connection | `crates/fennec-server/src/user.rs::become_user`, `crates/fennec-server/src/user.rs::wanted`, `crates/fennec-server/src/user.rs::hand_over` | `crates/fennec-server/tests/user.rs::a_user_the_server_cannot_become_is_refused_before_anything_is_opened`, `crates/fennec-server/src/user.rs::a_hand_over_changes_only_the_inode_it_found`; no test of the drop itself, which CI, never root, cannot make |
| 36 | [An install](#an-install) | a registry or mirror; a damaged download | `site/install.sh::main`, `site/install.ps1::Get-FennecAsset`, `tools/homebrew-formula.py::render` | `site/test_install.sh::tampered: installs nothing`, `tools/test_homebrew_formula.py::test_every_platform_gets_its_own_tarball_and_sum`; no test of a `.deb` or `.rpm` refusing a changed byte, which neither is checked for as it installs |
| 37 | [A release's provenance](#a-releases-provenance) | a step of the build; a token that publishes | `.github/workflows/release.yml::binaries`, `.github/workflows/release.yml::publish`, `.github/workflows/packages.yml::npm` | no test: nothing holds a release job's permissions, and no asset carries an attestation |
| 38 | [Dependencies](#dependencies) | a crate, package or action from outside | `tools/deps_check.py::check`, `tools/deps_check.py::check_tls`, `tools/osv_check.py::npm_findings` | `tools/test_osv_check.py::test_npm_ignore`; no test of `tools/deps_check.py` itself, which CI runs bare |
| 39 | [Another local user](#another-local-user) | a co-tenant on the host; a second process | `crates/fennec-abi/src/lock.rs::hold`, `crates/fennec-http/src/tenant_hooks.rs::Private::mode_private`, `crates/fennec-http/src/owner_only.rs::restrict`, `crates/fennec-cli/src/backup.rs::write_private` | `crates/fennec-abi/src/lock.rs::another_process_is_refused_while_one_holds_it`, `crates/fennec-http/tests/tenant_hooks.rs::a_tenants_secrets_are_set_listed_and_exported_by_its_admin_alone`, `crates/fennec-cli/tests/tokens.rs::a_new_key_is_its_owner_s_alone` |
| 40 | [Secrets kept out of logs](#secrets-kept-out-of-logs) | whoever reads the logs or a trace collector | `crates/fennec-http/src/audit.rs::refusal`, `crates/fennec-http/src/audit.rs::hook_refused`, `crates/fennec-http/src/statements.rs::shape` | `crates/fennec-server/tests/hooks.rs::a_stripe_event_is_taken_once_by_its_key`, `crates/fennec-server/tests/hooks.rs::a_short_secret_is_warned_of_and_taken`, `crates/fennec-server/tests/tracing.rs::a_durable_write_is_traced_through_the_lock_the_disk_and_the_replicas`; no test that a refused token's value is never logged |
| 41 | [The system's randomness](#the-systems-randomness) | anyone predicting or forcing a nonce used twice | `crates/fennec-http/src/crypto.rs::system_random`, `crates/fennec-http/src/crypto/unix.rs::fill`, `crates/fennec-http/src/crypto/windows.rs::fill`, `crates/fennec-http/src/crypto.rs::id_u64` | `crates/fennec-http/src/crypto.rs::no_randomness_refuses_nonces_and_names_ids`, `crates/fennec-http/src/crypto.rs::randomness_needs_no_descriptor`, `crates/fennec-http/src/seal.rs::no_randomness_refuses_a_seal`, `crates/fennec-http/src/seal.rs::frames_sealed_back_to_back_take_their_own_nonces` |

## Not defended

Said plainly, and in SECURITY.md's
[documented behaviour](../../SECURITY.md#what-is-documented-behaviour-not-a-vulnerability)
where an operator meets it:

- **A local user who can read the file, and root on the host.** The live
  file is plain on disk; only an archive or a backup is sealed. Whoever
  reads the file, its sync log, the audit log, the process's memory, its
  swap or a core dump has the data in them. The server writes its
  secrets' files for its owner alone (a tenant's `<t>.hooks`, a key
  `fennec key` writes) and warns of a TLS key others may read, but it
  creates the database, its sync log, its audit log and its lock file
  with the process's umask -- readable by every user under the usual
  022 -- so the directory's mode, or a umask of 077, is the operator's to
  set. Root, or the server's own user, is not an attacker this model
  holds off.
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
  `--jwt-issuer`, a short webhook secret, `sslmode=prefer`, plain HTTP
  without `--tls-cert`, a router dialing its nodes over `http://`.
- **Revocation.** The server keeps no list of revoked tokens; a token is
  good until its `exp`, and no leeway is given either side of it.
- **Rollback of what is stored where the attacker can write it**: a
  sealed segment's latest whole frames, a page's latest chunks, an older
  image put back whole (see the [open questions](#open-questions)).
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
a stream ended at its token's `exp` (`sse::EXPIRED`). *Not defended*:
revocation; clock skew (no leeway is given, `iat` is not read).
[access.md](access.md#expired-tokens).

### A token's signature

*Protects* the authenticity of every claim. *Against* a forger: `alg`
`none`, HS256 and RS256 confused, a `kid` naming no key, a malleable or
padded signature. *Enforced* by `Access::check`, which takes the
algorithm from the header but only HS256, RS256 or ES256 and verifies
each with a key of its own kind (an HMAC key never an RSA token's),
`read_jwks`, which takes `oct` keys of 32 bytes, RSA keys of 2048 bits or
more and P-256 keys on the curve, `RsaKey::verify_sha256`, which builds
the whole encoded message and compares it rather than parsing the
padding, and `P256Key::verify_sha256`, which takes `R || S` with each in
`[1, n)`. A JWKS is a file fennec-server's `--jwt-keys` names, read again
where its time changed, at most once a second: the server fetches no URL.
*Not defended*: other algorithms (refused); public-key arithmetic in
variable time, its inputs public. [access.md](access.md#es256-and-the-algorithms-still-refused).

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
`authenticate`, and the wait after each refusal (`audit::failed`: 100 ms
doubling to 5 s, by address, an IPv6 one by its /64), which only a token
taken clears. *Not defended*: a token's length; a client with more
addresses than the table's 10 000; guesses in parallel up to
`--max-connections`. The comparison's time has no test of its own:
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
a listener past loopback with no token unless `--insecure` is given.
*Not defended*: fennec-server's `--insecure`, `--http-cors '*'`,
`/_health` and an allowed preflight under any name, a body read to its
ceiling before the 403. [http.md](http.md#open-servers).

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
read-only (`admin_query`). *Not defended*: the router judges no token on
`/t/` (the node does); a router without a token is an open server; a
node dialed over `http://` sees its admin token in the clear.
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
roots, never beside them). *Not defended*: an `http://` address goes
plain. [http.md](http.md#dialing-https).

### Idempotency keys

*Protects* a write's kept answer, which may hold its returned rows.
*Against* another scoped caller sending someone's key. *Enforced* by
`idempotent::key`, keeping a scoped caller's key under its `sub` and
refusing a token with none, and `idempotent::answered` (the same key with
another request is 422). *Not defended*: every caller on the server's
token, or on an open server, shares one owner; a key is kept under `sub`
alone, not its issuer's too. [http.md](http.md#idempotency-keys).

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
`scoped()` (`tools::read_served`) and `tools::write`, which takes `put`,
`set` and `del` alone and runs them as the token. *Not defended*: what
the token may do, the agent may. [http.md](http.md#mcp).

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
*Enforced* by `FileId::aad` (a frame's tag covers its file's id, its place
and whether it is the last), `seal::open_frames` and `seal::open_whole`
(a later version refused by its number, a whole file cut short refused)
and `archive::entries`. *Not defended*: the whole frames at an appended
segment's end can be cut and the segment still opens, as a torn tail
does; a version 1 file's frames splice across files; a whole archive can
be rolled back. Not fuzzed. [replication.md](replication.md#sealed-archives).

### A primary's stream

*Protects* a replica's file and process, and its promotion. *Against* a
primary sending what it should not, or whoever stands in for one on an
`http://` path (the replica presents its token; the primary proves
nothing back but by TLS). *Enforced* by `Database::apply_records` (a
record's sum and its kind read before it lands), `Database::land_applied`
(each record whole in memory before the sink takes it) and
`Follower::session` (a panic applying a record caught with the lock held).
*Not defended*: the primary is trusted with the data. No test holds a
record kind the replica does not know refused by name, the primary's sums
checked on apply, or the stream's framing, which no fuzz target reaches.
[replication.md](replication.md#replication).

### A replica's request

*Protects* every write and image of the file or of every tenant, and the
primary's durable writes. *Against* a peer posing as a replica or an
archiver. *Enforced* by `Replication::authorized` (fennec-server's
`--replication-token` compared by `ct_eq` first), the node's stream
(`node_stream::handle`), and `History::continues` (a follower's history
judged before it is sent the writes after it). *Not defended*: a token's
holder reads everything and can ask for an image at will.
[replication.md](replication.md#replication).

### PostgreSQL dialed

*Protects* the PostgreSQL password, the importer's memory and time, and
the rows it mirrors. *Against* a hostile server, or one standing in for it.
*Enforced* by fennec-wire's `read_message` (a message held to the
protocol's length before it is allocated), `scram_iterations` (a server's
count held to 2^22), `negotiate` (libpq's `sslmode`) and
`ServerFinal::verify` (the server's signature compared by `ct_eq`). *Not
defended*: under any `sslmode` short of `verify-full` a stand-in gets the
rows; a cleartext-password request is answered under any mode; no channel
binding. Not fuzzed. [replication.md](replication.md#follow).

### A SQLite file

*Protects* the importer's process. *Against* a crafted file. *Enforced* by
`Pager::payload` (a payload's length held to the file's before it is
allocated), `Walk::visit` (each page reached once) and `MIN_USABLE`.
Fuzzed by the `sqlite` target. *Not defended*: an import that stops
halfway is not rolled back. [crates.md](crates.md#a-sqlite-files-lengths).

### The browser module's memory

*Protects* the module's state in the page. *Against* the page's own
script calling its exports with handles and lengths, and an image read
back from storage. *Enforced* by `with_slot` (a handle looked up in the
module's table), `str_from` (text that is not UTF-8 read as lossy text)
and `Database::load` behind `fennec_load`. *Not defended*: the page owns
the module's linear memory -- a pointer and a length are taken as the
page gives them, the same trust domain. No test holds a handle the module
never gave, nor an allocation the page's memory refuses.
[js-client.md](js-client.md#the-module-and-the-client).

### The native library and the sync

*Protects* the app's process and its replica. *Against* a caller's bad
handle or text, and the server a replica syncs with. *Enforced* by
`native` (a handle in a table, never reused), `call` (a panic caught at
the boundary), `text` (a null with a length, or text not UTF-8, refused
as misuse) and the sync's `said` (a server's wait held to its bound).
*Not defended*: a pointer's validity is the app's; the transport is the
binding's. [native-and-sync.md](native-and-sync.md#the-native-library),
[native-and-sync.md](native-and-sync.md#sync-core).

### A page's sealed persistence

*Protects* a page's data at rest in IndexedDB, where `persist` was given a
`cryptoKey`. *Against* whoever reads or edits the browser's storage
without the key. *Enforced* by `seal` (WebCrypto's AES-GCM, a random
12-byte IV a record) under `aad` (the key it is stored under, the image's
random generation and the chunk's place), and `restoreNow` refusing a
sealed record without its key and a clear one with it. *Not defended*:
`openFile`'s file in OPFS, never sealed; the newest chunks can be dropped
and an older image with its chunks put back whole, each opening (the AAD
binds no count of chunks); sizes and record names are visible.
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
before any file is opened). *Not defended*: the user empty keeps root;
the drop itself has no test, since CI never runs as root -- the design
file's Docker run is its check. [integrations.md](integrations.md#the-images-user).

### An install

*Protects* the user's machine. *Against* a damaged or truncated download,
and whoever stands between a release and its installer. *Enforced* by
`install.sh`'s `main` and `install.ps1`'s `Get-FennecAsset`, each checking
the asset's sum before it is unpacked, and the Homebrew formula carrying a
sum for each URL (`tools/homebrew-formula.py`). *Not defended*: the sums
come from the same release, so a release replaced whole is not caught;
the `.deb` and `.rpm` are not signed. [integrations.md](integrations.md#install-channels).

### A release's provenance

*Protects* the release's assets, the image and the published packages.
*Against* a build step or an action running with a write token, and a
token that publishes. *Enforced* by the release's building jobs running
with `contents: read` and a checkout keeping no token (`binaries` and its
siblings), `publish` adding `SHA256SUMS` and never replacing an asset, and
npm's, PyPI's and NuGet's trusted publishing. *Not defended*: no
attestation or SLSA provenance is made for any asset or the image; the
binaries and the Windows zip are unsigned. No test holds a job's
permissions. [integrations.md](integrations.md#install-channels).

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
`lock::hold` (`<file>.lock` held by every writer, a second refused), a
secret's file made its owner's alone -- mode 0600 (`Private::mode_private`
for a tenant's `<t>.hooks`, the shell's `write_private` for a key), on
Windows a DACL set before a byte is written (`owner_only::restrict`). *Not
defended*: the database and its logs are created with the process's
umask (see [Not defended](#not-defended)); a lock is not kept across a
bind mount or a network file system; a user who may read `<file>.lock`
may hold a lock on it. [engine.md](engine.md#single-writer).

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
(`getrandom`, `getentropy`, `BCryptGenRandom`, with no descriptor; `None`
where the system gives nothing, and the seal or the sign-in refused),
and `id_u64`, the only one that falls back to the clock, for ids alone.
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
| `crates/fennec-http/src/crypto.rs` | 556 of 1 308 | SHA-256; HMAC-SHA-256 and `HmacKey`; PBKDF2-HMAC-SHA-256, one block; ChaCha20, Poly1305 and their AEAD; base64 and base64url; `ct_eq`; the system's randomness; RS256 verification (`RsaKey`) | FIPS 180-4; RFC 2104, 4231; RFC 8018 section 5.2; RFC 8439; RFC 4648; RFC 8017 section 8.2.2, RFC 7518 section 3.3 | its test module (`sha256_vectors`, `hmac_rfc4231_all`, `pbkdf2_sha256_vectors`, `aead_rfc8439` and the appendix A tests, `rsa_pkcs1_sha256_as_node_signs`); `crates/fennec-http/tests/wycheproof.rs`, every case of Wycheproof's ChaCha20-Poly1305, HMAC-SHA-256, PBKDF2-HMAC-SHA-256 and RSA PKCS#1 v1.5 SHA-256 at 2048, 3072 and 4096 bits |
| `crates/fennec-http/src/crypto/p256.rs` | 485 of 646 | ES256 verification: P-256's field and scalars in Montgomery form, Jacobian points, Shamir's trick | FIPS 186-5 section 6.4.2; SEC 1 section 4.1.4; RFC 7518 section 3.4 | `crates/fennec-http/tests/es256.rs`: Wycheproof's P1363 file (262 cases), RFC 7515 A.3, tokens node:crypto signed; the curve's identities in the file's tests |
| `crates/fennec-http/src/crypto/unix.rs`, `windows.rs` | 76 and 42 of 76 and 62 | the system's randomness | getrandom(2), getentropy(3), BCryptGenRandom | `random_is_not_constant`, `no_randomness_refuses_nonces_and_names_ids`, `randomness_needs_no_descriptor` |
| `crates/fennec-http/src/seal.rs` | 290 of 586 | sealed files: the head, frames, nonces, a frame bound to its file, place and end | RFC 8439's AEAD; the framing is ours | its test module (nine tests); `crates/fennec-http/tests/archive.rs` |
| `crates/fennec-wire/src/client.rs` | 1 400 of 1 792; SCRAM about 170, `sslmode` and TLS about 150 | the client half of SCRAM-SHA-256; libpq's `sslmode`; the protocol's framing | RFC 5802, RFC 7677; PostgreSQL's protocol 3.0 | `scram_rfc7677_exchange`, held to RFC 7677's messages byte for byte; `scram_iterations_are_held_to_their_range`; `crates/fennec-wire/tests/client.rs`, `tls.rs` |
| `crates/fennec-tls/src/lib.rs` | 1 079 of 1 832 | rustls's use: a server's certificate and key, a client's roots and name check, a record read in parts at a pace | RFC 8446, through rustls | `crates/fennec-http/tests/tls.rs`, `dials.rs`; `crates/fennec-wire/tests/tls.rs` |
| `crates/fennec-http/src/access.rs`, `access/fields.rs` | 4 219 of 5 710, and 938 | bearer and JSON Web Tokens: a token's header, signature and claims (about 700 lines from `Access` to `Access::check`), JWKS files, policies and their rules, field lists | RFC 7515, 7517, 7518, 7519 | `crates/fennec-http/tests/access.rs`, `fields.rs`, `providers.rs`, `claim_values.rs`, `rules_first.rs`, `es256.rs`; the file's tests |
| `crates/fennec-http/src/hooks.rs`, `tenant_hooks.rs` | 1 605 of 2 322, and 1 104 | a webhook's verification: Stripe's, GitHub's, Standard Webhooks' and Svix's schemes; a tenant's secrets | the Standard Webhooks specification; Stripe's and GitHub's documented schemes | `each_scheme_verifies_its_senders_example` (each sender's own example); `crates/fennec-server/tests/hooks.rs`; `crates/fennec-http/tests/tenant_hooks.rs` |
| `crates/fennec-http/src/cors.rs` and `open_refusal` | 290 of 437, and 20 | the open server's guard: `Host` and `Origin`, CORS | RFC 6454; RFC 6761 section 6.3; the Fetch standard's CORS | `crates/fennec-http/tests/open_servers.rs`; `crates/fennec-shard/tests/cors.rs`; the file's tests |
| `web/fennec.js`'s `seal`, `unseal`, `aad`, `persist` and `restore` | about 180 of 3 492 | a page's image and chunks sealed with a `CryptoKey` | WebCrypto's AES-GCM, NIST SP 800-38D | `web/fennec.persist.test.js` |

The cryptography is 1 449 lines of code (2 678 with its tests); with the
SCRAM client, the use of TLS, the tokens, the webhooks, the guard and the
page's sealing, about 11 200, of which the tokens' policies and field
lists are some 4 500 a review of the cryptography and the sign-in can
leave for last. None of it is reached by a fuzz target.

### The fuzzing it has

The six targets of `crates/fennec-fuzz` (`make fuzz` lists them) run
under AddressSanitizer a fixed count of runs on every pull request
(`fuzz-smoke`: `http` 120 000, `json` 70 000, `fennecql` 180 000, `codec`
60 000, `image` 35 000, `sqlite` 80 000) and half an hour each a week
(`fuzz.yml`); [conventions.md](conventions.md#fuzzing) has how. None
reaches the cryptography above, a sealed file's or an archive's framing,
JWT and JWKS reading, a webhook's headers, the SCRAM exchange, the
PostgreSQL wire or the replication stream's framing: SECURITY.md lists
most of those as not fuzzed. The `json` target reaches the parser a
token's claims and a JWKS file are read with, and the `http` target the
request a guard is asked of, not the guard. Miri and AddressSanitizer over
the `unsafe` code are planned before 1.0 (the plan's G50).

### Open questions

Ranked: the first three are where a mistake would cost the most and where
reading did not settle it.

1. **Nonces under long-lived keys.** A sealed archive's every frame takes
   a random 96-bit nonce under one key the operator keeps for years, and
   the archiver seals a frame at each flush, so the count of frames a key
   seals is unbounded and uncounted: past about 2^32 frames the chance of
   one nonce twice passes 2^-32, and a nonce twice gives away both
   frames' keystream and lets their tags be forged. A page's `persist`
   draws a random IV for every record under one AES-GCM key, where a
   repeat gives away GHASH's key. Is a count of frames, a key derived per
   file from its id, or a rekeying owed?
2. **Truncation and rollback.** `seal::open_appended` opens a segment cut
   at any frame's end, as a crash leaves one, so whoever can write the
   bucket can drop its newest whole frames; nothing binds a page's count
   of chunks, so its newest can be dropped; an older archive, or an older
   image with its chunks, opens whole. Is that what an operator expects of
   "sealed", and what would an archive's manifest or the last frame's
   count cost?
3. **SCRAM's downgrades.** The client answers `AuthenticationOk` at any
   step -- before or instead of the SCRAM exchange, its server signature
   then never asked -- and a cleartext-password request under any
   `sslmode`, and offers no channel binding (`SCRAM-SHA-256-PLUS`). Under
   anything short of `verify-full` a stand-in skips the sign-in or
   relays it. Should the client refuse a server that does not prove the
   password where it asked for SCRAM (libpq 16's `require_auth`)?
4. **ECDSA malleability and a token's spellings.** `(r, n - s)` verifies
   as `(r, s)` does (JWS does not ask for low S), and `b64url_decode`
   takes non-canonical trailing bits, so a token has several spellings. A
   server keeps verified tokens by their text (`Access::verified`):
   nothing found keys a security decision on that text, but a reviewer
   should look again.
5. **RSA PKCS#1 v1.5's strictness.** `RsaKey::verify_sha256` builds the
   whole encoded message, its `DigestInfo` with NULL parameters, and
   compares it, never parsing the padding -- so the form without
   parameters is refused. The key: 2048 to 8192 bits, an odd exponent of
   at most 64 bits from 3, `R^2 mod n` by doubling; Montgomery
   multiplication (CIOS) over 32 to 128 limbs.
6. **Constant time.** Claimed only for comparisons of secrets (`ct_eq`, a
   loop through `black_box`, best effort; a length is no secret here).
   ChaCha20 and Poly1305 (26-bit limbs) have no table and no branch on a
   secret; HMAC's key setup branches on the key's length alone. A secret
   read through `b64_decode` (a webhook's `whsec_` secret, at the start)
   meets a branch for each character's class. RSA and P-256 work on
   public values. Is there a comparison or a table lookup on a secret
   left?
7. **The JWT's header.** `alg` must be HS256, RS256 or ES256 and the key's
   kind must match; `kid` only narrows the keys tried (a token naming none
   is tried against all); `crit` is not read, where RFC 7515 section
   4.1.11 has a token naming an extension the reader does not know
   refused; `typ` is not read, so an ID token and an access token from one
   issuer are told apart by `aud` alone.
8. **The clock.** `exp` and `nbf` are compared with no leeway and `iat` is
   not read; a server whose clock runs late takes an expired token for as
   long as it is late. An `exp` past about 1.8e16 overflows a
   multiplication by 1 000 in a release build, which checks none.
9. **JWKS fetching and caching.** None: a JWKS is a file, read again where
   its time changed, at most once a second, the old keys kept where the
   new file does not read. A rotation written within the file system's
   time resolution, or by a tool keeping the time, is missed until the
   next change. Keys a server verified a token with are dropped from its
   cache at a reload.
10. **A webhook's replay window.** A Stripe or Standard Webhooks event is
    taken within the hook's tolerance (1 s to 1 h), its key kept past
    twice that; a GitHub event, which signs no time, is answered by its
    body's digest for `keep` (7 days by default) and taken again after.
11. **PBKDF2's count and SCRAM's nonce.** A server's iteration count is
    held to 1 to 2^22; the client's nonce is 24 random bytes; the server's
    must extend it. `pbkdf2_sha256` computes one block, all SCRAM needs.

## An outside audit

**Decided: fennecdb 1.0 ships without a paid outside audit and does not
wait for one.** It is alpha today, with no budget set aside for an audit,
and the plan's exit for this item (G14) takes either a published review
with its high and critical findings fixed, or this decision written in
SECURITY.md; it is the second, in SECURITY.md's own words: "No outside
audit has been made of this code, and 1.0 does not wait for one".

What stands in for it before 1.0: a review independent of the code's
author goes over the scope [above](#what-was-written-by-hand), starting
from the [open questions](#open-questions), and every finding it makes is
fixed with a regression test in the crate it is in. The vectors (the
plan's G6), the fuzz targets (G5) and this threat model (G2) are what it
is handed.

What keeps the door open: this section and the scope package are kept
current with the code -- a file of the scope growing or a primitive added
updates the table, whose boundaries the check holds to the tree -- so that
an audit can be commissioned from a firm, or funding applied for (OSTIF,
Alpha-Omega), whenever the owner chooses, with nothing to rebuild first.
SECURITY.md's sentences are held by its check: the promise is "no outside
audit has been made", and a sentence saying the code was audited is
refused while none was.
