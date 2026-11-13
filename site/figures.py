"""The figures the site quotes as measurements, each held by `build.py` to
the median of a `(family, cell, metric)` in crates/fennec-bench/results/.

A figure is `(file, pattern, family, cell, metric, scale)`: the pattern's
one group is the number as the page writes it (spaces and commas of a
thousands separator dropped), `scale` what takes the page's unit to the
metric's (a page's `ms` over a `_us` metric is 1000, a `k` over `ops_s` is
1000). A figure the YCSB results hold is a `ycsb:` fact of `CLAIMS` in
build.py instead, and a size of this build a `kb` one; what is here is the
rest. Where no run is recorded behind a figure yet it is held to a `hand`
row (machine `hand-measured`, its commit the one that first wrote the figure
into a page) until a release candidate's night measures it.
"""

import re

NUM = r"([\d][\d   .]*?)"

BH = "site/content/docs/benchmarks.html"
IX = "site/content/index.html"
RM = "README.md"
VP = "site/content/docs/vs-postgres.html"
VS = "site/content/docs/vs-sqlite.html"
VG = "site/content/docs/vs-pglite.html"
BR = "site/content/docs/benchmarks-requests.html"

FIGURES = []


def fig(file, pattern, family, cell, metric, scale=1):
    FIGURES.append((file, pattern, family, cell, metric, scale))


def td(label, *cells):
    """A table row by its first cell's text, up to the cell a figure is in."""
    return rf"<td>{label}</td>" + "".join(cells)


# ---- a request to fennec-server (benchmarks-requests.html#requests)
fig(IX, r"A row by id over HTTP in " + NUM + r" ms", "requests", "http:get_by_id:1_client", "p50_us", 1000)
fig(IX, r"A row by id over HTTP</dt><dd><b>" + NUM + r" ms", "requests", "http:get_by_id:1_client", "p50_us", 1000)
fig(IX, r"Rows by id, eight clients at once</dt><dd><b>" + NUM + r"/s", "requests", "http:get_by_id:8_clients", "ops_s")
for label, cell in (("a row by id", "get_by_id"), ("a filter, limit 10", "filter_limit_10"), ("put a row", "put_row")):
    fig(BH, rf"<td>{label}</td><td class=\"n\"><b>{NUM} ms</b>", "requests", f"http:{cell}:1_client", "p50_us", 1000)
    fig(BH, rf"<td>{label}</td><td class=\"n\"><b>[\d.]+ ms</b></td><td class=\"n\">{NUM}/s", "requests", f"http:{cell}:8_clients", "ops_s")

# ---- a read by key taken apart (the round-trip table)
# ---- many writers and the readers beside them (benchmarks-requests.html#concurrency)
CW = "concurrency"
fig(BH, r"durable writes/s, 1 / 16 writers</td><td class=\"n\">" + NUM + r" / <b>", CW, "fennec:durable_writes:1_writer", "writes_s")
fig(BH, r"durable writes/s, 1 / 16 writers</td><td class=\"n\">\d+ / <b>" + NUM + r"</b>", CW, "fennec:durable_writes:16_writers", "writes_s")
fig(BH, r"durable writes/s, 1 / 16 writers</td><td class=\"n\">[^<]*<b>[\d ]+</b></td><td class=\"n\">" + NUM + r" / ", CW, "sqlite:durable_writes:1_writer", "writes_s")
fig(BH, r"durable writes/s, 1 / 16 writers</td><td class=\"n\">[^<]*<b>[\d ]+</b></td><td class=\"n\">\d+ / " + NUM + r"</td>", CW, "sqlite:durable_writes:16_writers", "writes_s")
fig(IX, r"<b>" + NUM + r" → \d+/s</b>", CW, "fennec:durable_writes:1_writer", "writes_s")
fig(IX, r"<b>\d+ → " + NUM + r"/s</b>", CW, "fennec:durable_writes:16_writers", "writes_s")
fig(IX, r"SQLite " + NUM + r" → \d+<", CW, "sqlite:durable_writes:1_writer", "writes_s")
fig(IX, r"SQLite \d+ → " + NUM + r"<", CW, "sqlite:durable_writes:16_writers", "writes_s")
fig(BH, r"reads/s from 4 threads, beside 4 writers</td><td class=\"n\"><b>" + NUM + r" k</b>", CW, "fennec:reads_4_threads:beside_4_writers", "reads_s", 1000)
fig(BH, r"reads/s from 4 threads, beside 4 writers</td><td class=\"n\"><b>[\d ]+ k</b></td><td class=\"n\">" + NUM + r" k", CW, "sqlite:reads_4_threads:beside_4_writers", "reads_s", 1000)
fig(BH, r"beside blocks of 1 000 writes</td><td class=\"n\"><b>" + NUM + r" k</b>", CW, "fennec:reads_4_threads:beside_write_blocks", "reads_s", 1000)
fig(BH, r"beside blocks of 1 000 writes</td><td class=\"n\"><b>[\d ]+ k</b></td><td class=\"n\">" + NUM + r" k", CW, "sqlite:reads_4_threads:beside_write_blocks", "reads_s", 1000)
fig(IX, r"Rows read beside four writers</dt><dd><b>" + NUM + r"/s", CW, "fennec:reads_4_threads:beside_4_writers", "reads_s")
fig(IX, r"Rows read beside four writers</dt><dd><b>[\d ]+/s</b><span>SQLite " + NUM + r"</span>", CW, "sqlite:reads_4_threads:beside_4_writers", "reads_s")
fig(VS, r"beside 4 writers</td><td class=\"n\"><b>" + NUM + r" k</b>, p99", CW, "fennec:reads_4_threads:beside_4_writers", "reads_s", 1000)
fig(VS, r"beside 4 writers</td><td class=\"n\"><b>[\d ]+ k</b>, p99 " + NUM + r" µs", CW, "fennec:reads_4_threads:beside_4_writers", "p99_us")
fig(VS, r"beside 4 writers</td><td class=\"n\"><b>[\d ]+ k</b>, p99 [\d.]+ µs</td><td class=\"n\">" + NUM + r" k, p99", CW, "sqlite:reads_4_threads:beside_4_writers", "reads_s", 1000)
fig(VS, r"beside 4 writers</td><td class=\"n\"><b>[\d ]+ k</b>, p99 [\d.]+ µs</td><td class=\"n\">[\d ]+ k, p99 " + NUM + r" µs", CW, "sqlite:reads_4_threads:beside_4_writers", "p99_us")
fig(VS, r"beside blocks of 1 000 writes</td><td class=\"n\"><b>" + NUM + r" k</b>, p99", CW, "fennec:reads_4_threads:beside_write_blocks", "reads_s", 1000)
fig(VS, r"beside blocks of 1 000 writes</td><td class=\"n\"><b>[\d ]+ k</b>, p99 " + NUM + r" µs", CW, "fennec:reads_4_threads:beside_write_blocks", "p99_us")
fig(VS, r"the longest " + NUM + r"[–-][\d.]+ ms</td><td class=\"n\">630", CW, "fennec:reads_4_threads:beside_write_blocks", "longest_min_us", 1000)
fig(VS, r"the longest [\d.]+[–-]" + NUM + r" ms</td><td class=\"n\">630", CW, "fennec:reads_4_threads:beside_write_blocks", "longest_max_us", 1000)
fig(VS, r"630 k, p99 " + NUM + r" µs", CW, "sqlite:reads_4_threads:beside_write_blocks", "p99_us")
fig(VS, r"630 k, p99 [\d.]+ µs, the longest " + NUM + r"[–-]", CW, "sqlite:reads_4_threads:beside_write_blocks", "longest_min_us", 1000)
fig(VS, r"630 k, p99 [\d.]+ µs, the longest [\d.]+[–-]" + NUM + r" ms", CW, "sqlite:reads_4_threads:beside_write_blocks", "longest_max_us", 1000)
fig(VS, r"the block holds the lock about " + NUM + r" ms", CW, "fennec:write_block:lock_held", "us", 1000)

# ---- replicas, tenants and sync
RP = "replication"
fig(BH, r"<code>--sync always</code></td><td class=\"n\"><b>" + NUM + r" ms", RP, "replica:visible_after_primary:sync_always", "p50_us", 1000)
fig(BH, r"standby after the router answered it, p50 \(p99 " + NUM + r" ms\)", RP, "tenant_standby:visible_after_router", "p99_us", 1000)
fig(BH, r"standby after the router answered it, p50 \(p99 [\d.]+ ms\)</td><td class=\"n\"><b>" + NUM + r" ms", RP, "tenant_standby:visible_after_router", "p50_us", 1000)
fig(IX, r"A tenant's copy behind its primary</dt><dd><b>" + NUM + r" ms", RP, "tenant_standby:visible_after_router", "p50_us", 1000)
fig(IX, r"median, p99 " + NUM + r"</span>", RP, "tenant_standby:visible_after_router", "p99_us", 1000)
fig(BH, r"the medians of three rounds</td><td class=\"n\"><b>" + NUM + r"[–-]", RP, "failover:20_tenants", "median_min_ms")
fig(BH, r"the medians of three rounds</td><td class=\"n\"><b>[\d.]+[–-]" + NUM + r" ms", RP, "failover:20_tenants", "median_max_ms")
fig(IX, r"20 in " + NUM + r"[–-][\d.]+ ms", RP, "failover:20_tenants", "median_min_ms")
fig(IX, r"20 in [\d.]+[–-]" + NUM + r" ms", RP, "failover:20_tenants", "median_max_ms")
fig(BH, r"a " + NUM + r" MB tenant moved to another node", RP, "tenant_move", "tenant_mb")
fig(IX, r"A " + NUM + r" MB tenant moved", RP, "tenant_move", "tenant_mb")
fig(BH, r"MB tenant moved to another node</td><td class=\"n\"><b>under " + NUM + r" s", RP, "tenant_move", "under_s")
fig(IX, r"<b>under " + NUM + r" s</b></dd></div>\n      <div><dt>Tenants", RP, "tenant_move", "under_s")
fig(IX, r"<b>" + NUM + r"[–-][\d.]+ s</b><span>10 tenants", RP, "failover:region_cut_off", "min_s")
fig(IX, r"<b>[\d.]+[–-]" + NUM + r" s</b><span>10 tenants", RP, "failover:region_cut_off", "max_s")
fig(BH, r"the same put alone " + NUM + r" µs", RP, "sync_core:put_alone", "us")
fig(BH, r"in process \(the same put alone [\d.]+ µs\)</td><td class=\"n\"><b>" + NUM + r" µs", RP, "sync_core:apply_one_row", "us")
fig(RM, r"Frankfurt to Singapore is " + NUM + r" ms", RP, "read_replica:frankfurt_singapore", "rtt_ms")

# ---- text search (benchmarks-text.html)
TX = "text"
fig(BH, r"takes " + NUM + r" ms, a facet", TX, "match:100000_docs_30_words", "ms")
fig(BH, r"a facet over every row through <code>@hash</code> " + NUM + r" ms", TX, "facet:hash_100000_rows", "ms")
fig(BH, r"FiQA's 135 500 are found in " + NUM + r" µs", TX, "fuzzy:fiqa_one_edit", "us")
fig(IX, r"A <code>match</code> over 100 000 documents</dt><dd><b>" + NUM + r" ms", TX, "match:100000_docs_30_words", "ms")
fig(IX, r"FiQA&rsquo;s 135 500</dt><dd><b>" + NUM + r" µs", TX, "fuzzy:fiqa_one_edit", "us")
fig(IX, r"one edit; " + NUM + r" µs at two", TX, "fuzzy:fiqa_two_edits", "us")
fig(IX, r"A facet over every row, through <code>@hash</code></dt><dd><b>" + NUM + r" ms", TX, "facet:hash_100000_rows", "ms")
fig(IX, r"A row&rsquo;s marks cost\s+" + NUM + r" to [\d.]+ µs", TX, "highlight:marks_per_row", "min_us")
fig(IX, r"A row&rsquo;s marks cost\s+[\d.]+ to " + NUM + r" µs", TX, "highlight:marks_per_row", "max_us")

# ---- live queries
LV = "live"
fig(IX, r"run again</dt><dd><b>" + NUM + r" ms", LV, "todos_page_50_of_1000_to_3000:rerun", "ms")
fig(IX, r"A write no live query reads</dt><dd><b>about " + NUM + r" µs", LV, "write_no_live_query_reads", "us")

# ---- vectors at scale (benchmarks-vector.html#scale)
VC = "vector"
fig(VP, td("load and index", r"<td class=\"n\"><b>" + NUM + r" s</b>"), VC, "scale_1m_128:fennec-server", "load_s")
fig(VP, td("load and index", r"<td class=\"n\"><b>[\d.]+ s</b></td><td class=\"n\">" + NUM + r" s"), VC, "scale_1m_128:pgvector", "load_s")
fig(VP, td("memory the server holds", r"<td class=\"n\"><b>" + NUM + r" MB</b>"), VC, "scale_1m_128:fennec-server", "memory_mb")
fig(VP, td("memory the server holds", r"<td class=\"n\"><b>[\d ]+ MB</b></td><td class=\"n\">" + NUM + r" MB"), VC, "scale_1m_128:pgvector", "memory_mb")
fig(VP, td("disk", r"<td class=\"n\"><b>" + NUM + r" MB</b>"), VC, "scale_1m_128:fennec-server", "disk_mb")
fig(VP, td("disk", r"<td class=\"n\"><b>[\d ]+ MB</b></td><td class=\"n\">" + NUM + r" MB"), VC, "scale_1m_128:pgvector", "disk_mb")
fig(VP, r"ef 100: recall@10, p50</td><td class=\"n\"><b>" + NUM + r"%", VC, "scale_1m_128:fennec-server:ef100", "recall_pct")
fig(VP, r"ef 100: recall@10, p50</td><td class=\"n\"><b>[\d.]+%, " + NUM + r" ms", VC, "scale_1m_128:fennec-server:ef100", "p50_us", 1000)
fig(VP, r"ef 100: recall@10, p50</td><td class=\"n\"><b>[\d.]+%, [\d.]+ ms</b></td><td class=\"n\">" + NUM + r"%", VC, "scale_1m_128:pgvector:ef100", "recall_pct")
fig(VP, r"ef 100: recall@10, p50</td><td class=\"n\"><b>[\d.]+%, [\d.]+ ms</b></td><td class=\"n\">[\d.]+%, " + NUM + r" ms", VC, "scale_1m_128:pgvector:ef100", "p50_us", 1000)
fig(VP, r"a filter keeping 1%, ef 100</td><td class=\"n\"><b>" + NUM + r"%", VC, "scale_1m_128:fennec-server:filter_1pct", "recall_pct")
fig(VP, r"a filter keeping 1%, ef 100</td><td class=\"n\"><b>[\d.]+%, " + NUM + r" ms", VC, "scale_1m_128:fennec-server:filter_1pct", "p50_us", 1000)
fig(VP, r"a filter keeping 1%, ef 100</td><td class=\"n\"><b>[\d.]+%, [\d.]+ ms</b></td><td class=\"n\">" + NUM + r"%", VC, "scale_1m_128:pgvector:filter_1pct", "recall_pct")
fig(VP, r"a filter keeping 1%, ef 100</td><td class=\"n\"><b>[\d.]+%, [\d.]+ ms</b></td><td class=\"n\">[\d.]+%, " + NUM + r" ms", VC, "scale_1m_128:pgvector:filter_1pct", "p50_us", 1000)
fig(VP, r"8 clients, ef 100</td><td class=\"n\"><b>" + NUM + r" queries/s", VC, "scale_1m_128:fennec-server:8_clients", "queries_s")
fig(VP, r"8 clients, ef 100</td><td class=\"n\"><b>[\d ]+ queries/s</b></td><td class=\"n\">" + NUM + r" queries/s", VC, "scale_1m_128:pgvector:8_clients", "queries_s")
fig(VP, r"<code>COPY</code>\)</td><td class=\"n\"><b>" + NUM + r" rows/s", VC, "load_100000_rows:fennec-server", "rows_s")
fig(VP, r"<code>COPY</code>\)</td><td class=\"n\"><b>[\d ]+ rows/s</b></td><td class=\"n\">" + NUM + r" rows/s", VC, "load_100000_rows:pgvector", "rows_s")
fig(RM, r"nearest ten at " + NUM + r"% recall", VC, "scale_1m_128:fennec-server:ef100", "recall_pct")
fig(RM, r"recall in " + NUM + r" ms,\s+against", VC, "scale_1m_128:fennec-server:ef100", "p50_us", 1000)
fig(RM, r"against pgvector's " + NUM + r"% in", VC, "scale_1m_128:pgvector:ef100", "recall_pct")
fig(RM, r"against pgvector's [\d.]+% in " + NUM + r"\s+\(\[At scale", VC, "scale_1m_128:pgvector:ef100", "p50_us", 1000)
fig(BH, r"answers the nearest ten at " + NUM + r"% recall", VC, "scale_1m_128:fennec-server:ef100", "recall_pct")
fig(BH, r"recall in " + NUM + r" ms, pgvector", VC, "scale_1m_128:fennec-server:ef100", "p50_us", 1000)
fig(BH, r"pgvector\s+at " + NUM + r"% in", VC, "scale_1m_128:pgvector:ef100", "recall_pct")
fig(BH, r"pgvector\s+at [\d.]+% in " + NUM + r" ms", VC, "scale_1m_128:pgvector:ef100", "p50_us", 1000)
fig(VP, r"An empty query's round trip was\s+" + NUM + r" to\s+[\d.]+ ms to PostgreSQL", VC, "empty_query:postgres", "min_us", 1000)
fig(VP, r"An empty query's round trip was\s+[\d.]+ to\s+" + NUM + r" ms to PostgreSQL", VC, "empty_query:postgres", "max_us", 1000)
fig(VP, r"to PostgreSQL and\s+" + NUM + r" to\s+[\d.]+ ms to fennec-server", VC, "empty_query:fennec-server", "min_us", 1000)
fig(VP, r"to PostgreSQL and\s+[\d.]+ to\s+" + NUM + r" ms to fennec-server", VC, "empty_query:fennec-server", "max_us", 1000)

# ---- one process against SQLite, 100 000 x 128 (benchmarks-vector.html)
SQ = "vs_sqlite"
fig(VS, r"data write, no index</td><td class=\"n\"><b>" + NUM + r" k rows/s", SQ, "fennec:data_write_no_index", "rows_s", 1000)
fig(VS, r"data write, no index</td><td class=\"n\"><b>[\d ]+ k rows/s</b></td><td class=\"n\">" + NUM + r" k/s", SQ, "sqlite:data_write_no_index", "rows_s", 1000)
fig(VS, r"disk size</td><td class=\"n\"><b>" + NUM + r" MB", SQ, "fennec:disk_size", "mb")
fig(VS, r"disk size</td><td class=\"n\"><b>[\d.]+ MB</b></td><td class=\"n\">" + NUM + r" MB", SQ, "sqlite:disk_size", "mb")
fig(VS, r"scalar filter, indexed</td><td class=\"n\"><b>" + NUM + r" ms", SQ, "fennec:scalar_filter_indexed", "us", 1000)
fig(VS, r"scalar filter, indexed</td><td class=\"n\"><b>[\d.]+ ms</b></td><td class=\"n\">" + NUM + r" ms", SQ, "sqlite:scalar_filter_indexed", "us", 1000)
fig(VS, r"vector top-10, exact</td><td class=\"n\"><b>" + NUM + r" ms", SQ, "fennec:vector_top10_exact", "us", 1000)
fig(VS, r"vector top-10, exact</td><td class=\"n\"><b>[\d.]+ ms</b></td><td class=\"n\">" + NUM + r" ms", SQ, "sqlite:vector_top10_exact", "us", 1000)
fig(VS, r"vector top-10, approximate</td><td class=\"n\"><b>" + NUM + r" ms", SQ, "fennec:vector_top10_approx", "us", 1000)
fig(VS, r"open, then a read by id</td><td class=\"n\">" + NUM + r" ms", SQ, "fennec:open_then_read_by_id", "us", 1000)
fig(VS, r"open, then a read by id</td><td class=\"n\">[\d.]+ ms</td><td class=\"n\"><b>" + NUM + r" ms", SQ, "sqlite:open_then_read_by_id", "us", 1000)
fig(VS, r"open, then a vector top-10</td><td class=\"n\"><b>" + NUM + r" ms", SQ, "fennec:open_then_vector_top10", "us", 1000)
fig(VS, r"open, then a vector top-10</td><td class=\"n\"><b>[\d.]+ ms</b></td><td class=\"n\">" + NUM + r" ms, exact", SQ, "sqlite:open_then_vector_top10", "us", 1000)

# ---- sizes of what the page loads, and others' files (benchmarks-size.html)
SZ = "size"
fig(VG, td("PGlite 0.5.8: <code>pglite.wasm</code>", r"<td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:pglite.wasm", "brotli_kb")
fig(VG, td("PGlite 0.5.8: <code>pglite.wasm</code>", r"<td class=\"n\">[\d ]+ KB</td><td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:pglite.wasm", "gzip_kb")
fig(VG, td("… <code>pglite.data</code>", r"<td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:pglite.data", "brotli_kb")
fig(VG, td("… <code>pglite.data</code>", r"<td class=\"n\">[\d ]+ KB</td><td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:pglite.data", "gzip_kb")
fig(VG, td("… <code>initdb.wasm</code>", r"<td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:initdb.wasm", "brotli_kb")
fig(VG, td("… <code>initdb.wasm</code>", r"<td class=\"n\">[\d ]+ KB</td><td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:initdb.wasm", "gzip_kb")
fig(VG, td("PGlite 0.5.8, the three", r"<td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:the_three", "brotli_kb")
fig(VG, td("PGlite 0.5.8, the three", r"<td class=\"n\">[\d ]+ KB</td><td class=\"n\">" + NUM + r" KB"), SZ, "pglite_0.5.8:the_three", "gzip_kb")
fig(VG, r"<td>" + NUM + r" KB, before pgvector", SZ, "pglite_0.5.8:the_three", "brotli_kb")
fig(VG, r"pgvector adds</b> a\s+" + NUM + r" KB\s+archive", SZ, "pgvector:archive", "gzip_kb")
fig(VG, r"extensions page gives " + NUM + r" KB", SZ, "pgvector:extensions_page", "kb")
fig(VS, td("SQLite's official build, <code>sqlite3.wasm</code> 3.53.4-build2", r"<td class=\"n\">" + NUM + r" KB"), SZ, "sqlite3.wasm_3.53.4-build2", "brotli_kb")
fig(VS, td("SQLite's official build, <code>sqlite3.wasm</code> 3.53.4-build2", r"<td class=\"n\">[\d ]+ KB</td><td class=\"n\">" + NUM + r" KB"), SZ, "sqlite3.wasm_3.53.4-build2", "gzip_kb")
fig(VS, r"<td>" + NUM + r" KB, without sqlite-vec", SZ, "sqlite3.wasm_3.53.4-build2", "brotli_kb")
fig(VP, r"A " + NUM + r" MB server image", SZ, "fennec-server:container_image", "mb")
fig(RM, r"\*\*Container\.\*\* " + NUM + r" MB", SZ, "fennec-server:container_image", "mb")
fig(RM, r"· " + NUM + r" MB container image", SZ, "fennec-server:container_image", "mb")
fig(RM, r"· " + NUM + r"[–-]\d+ KB binary", SZ, "fennec-binaries", "min_kb")
fig(RM, r"· \d+[–-]" + NUM + r" KB binary", SZ, "fennec-binaries", "max_kb")

# ---- the seed's stream (README)
SD = "seed"
fig(RM, r"a 100 000-row seed in " + NUM + r" s rather", SD, "seed_100000_rows:compressed_50mbit", "s")
fig(RM, r"rather than " + NUM + r" at 50 Mbit", SD, "seed_100000_rows:plain_50mbit", "s")

# ---- a read by key taken apart (benchmarks-requests.html#requests): the
# table's own figures, and the ones `rt:` facts of CLAIMS read back
for col, cell in ((1, "fennec-server-mac:read"), (2, "fennec-server-docker:read"), (3, "postgres-docker:read")):
    skip = r'<td class="n">[^<]*(?:<[^>]+>[^<]*)*?</td>' * (col - 1)
    fig(BR, r'<tr><td>round trip, p50</td>' + skip + r'<td class="n">(?:<b>)?' + NUM + r" ms", "requests", cell, "round_trip_us", 1000)
    fig(BR, r'<tr><td>inside the server</td>' + skip + r'<td class="n">' + NUM + r" ms", "requests", cell, "inside_us", 1000)
    fig(BR, r'<tr><td>the client\'s CPU</td>' + skip + r'<td class="n">' + NUM + r" ms", "requests", cell, "client_cpu_us", 1000)
fig(BR, r'<tr><td>a durable update, p50</td><td class="n">' + NUM + r" ms \(", "requests", "fennec-server-mac:durable_update", "p50_us", 1000)
fig(BR, r'<tr><td>a durable update, p50</td><td class="n">[^\n]*?</td><td class="n"><b>' + NUM + r" ms", "requests", "fennec-server-docker:durable_update", "p50_us", 1000)
fig(BR, r'<tr><td>a durable update, p50</td><td class="n">[^\n]*?</td><td class="n"><b>[^<]*</b>(?:<br>)?<small>[^<]*</small></td><td class="n">' + NUM + r" ms", "requests", "postgres-docker:durable_update", "p50_us", 1000)
fig(BR, r'<tr><td>a scan of 50 records, p50</td><td class="n">&nbsp;</td><td class="n"><b>' + NUM + r"[-–]", "requests", "fennec-server-docker:scan_50", "p50_min_us", 1000)
fig(BR, r'<tr><td>a scan of 50 records, p50</td><td class="n">&nbsp;</td><td class="n"><b>[\d.]+[-–]' + NUM + r" ms", "requests", "fennec-server-docker:scan_50", "p50_max_us", 1000)
fig(BR, r'<tr><td>a scan of 50 records, p50</td><td class="n">&nbsp;</td><td class="n"><b>[^<]*</b><br><small>[^<]*</small></td><td class="n">' + NUM + r" ms", "requests", "postgres-docker:scan_50", "p50_us", 1000)

# ---- YCSB figures, held to the cells of results/ycsb.tsv (a `ycsb:` fact
# is thousands of operations a second, a `ycsbms:` one a latency in ms)
YCSB = []
K = r"([\d.]+) k"


def ycsb(file, pattern, system, mode, wl, threads, fact="ycsb"):
    suffix = ":slowest_p99" if fact == "ycsbms" else ""
    YCSB.append((file, pattern, f"{fact}:{system}:{mode}:{wl}:{threads}{suffix}"))


def cells(file, label, pairs, mode, wl, threads, bold_first=True):
    """A table row of `label`: its k figures, in the order of `pairs`'
    systems, each held to its own cell."""
    cell = r'<td class="n">(?:<b>)?[\d.]+ k(?:</b>)?</td>'
    for i, system in enumerate(pairs):
        ycsb(file, rf'<td>{re.escape(label)}</td>' + cell * i + r'<td class="n">(?:<b>)?' + K, system, mode, wl, threads)


INPROC = ("fennec", "sqlite")
WIRE = ("server-docker", "pg", "mongo")
cells(BH, "B, 95% reads, 16 threads, buffered", INPROC, "buffered", "B", "16")
cells(BH, "C, reads, one thread, after the updates", INPROC, "buffered", "C", "1")
cells(BH, "A, 50% updates, 16 threads, durable", INPROC, "durable", "A", "16")
cells(VS, "B, 95% reads, 16 threads, buffered", INPROC, "buffered", "B", "16")
cells(VS, "C, reads, one thread, after the updates", INPROC, "buffered", "C", "1")
cells(VS, "A, 50% updates, 16 threads, durable", INPROC, "durable", "A", "16")
cells(BH, "B, 95% reads, 16 clients", WIRE, "durable", "B", "16")
cells(BH, "A, 50% updates, 16 clients", WIRE, "durable", "A", "16")
cells(BH, "A, 50% updates, one client", WIRE, "durable", "A", "1")
PGWIRE = ("server-docker", "pg")
cells(VP, "B, 95% reads, 16 clients", PGWIRE, "durable", "B", "16")
cells(VP, "A, 50% updates, 16 clients", PGWIRE, "durable", "A", "16")
cells(VP, "A, 50% updates, one client", PGWIRE, "durable", "A", "1")
cells(VP, "B, 95% reads, one client", PGWIRE, "durable", "B", "1")
cells(VP, "E, scans of 1 to 100 rows, one client", PGWIRE, "durable", "E", "1")
cells(VP, "C, reads alone, one client, buffered", PGWIRE, "buffered", "C", "1")
ycsb(VP, r"its median now\s+" + K, "pg", "buffered", "C", "1")
ycsb(VP, r"fennec-server leads C,\s+" + K + r" against", "server-docker", "buffered", "C", "16")
ycsb(VP, r"fennec-server leads C,\s+[\d.]+ k against " + K, "pg", "buffered", "C", "16")
ycsb(VS, r"SQLite read " + K + r"\.", "sqlite", "buffered", "C", "1")
ycsb(IX, r"<b>" + K + r" ops/s</b><span>p99 [\d.]+ ms; Redis", "server-docker", "buffered", "C", "16")
ycsb(IX, r"<span>p99 " + r"([\d.]+) ms; Redis", "server-docker", "buffered", "C", "16", "ycsbms")
ycsb(IX, r"Redis " + K + r", p99 [\d.]+ ms; both", "redis", "buffered", "C", "16")
ycsb(IX, r"Redis [\d.]+ k, p99 ([\d.]+) ms; both", "redis", "buffered", "C", "16", "ycsbms")
ycsb(IX, r"B over HTTP, 16 clients, durable</dt><dd><b>" + K + r" ops/s", "server-docker", "durable", "B", "16")
ycsb(IX, r"<span>Redis " + K + r", PostgreSQL", "redis", "durable", "B", "16")
ycsb(IX, r"PostgreSQL " + K + r", both in Docker", "pg", "durable", "B", "16")
ycsb(RM, r"workloads in process, fennecdb does B with 16 threads at " + K + r" operations a", "fennec", "buffered", "B", "16")
ycsb(RM, r"second against SQLite's " + K + r", and C", "sqlite", "buffered", "B", "16")
ycsb(RM, r"and C on one thread at " + K + r" against", "fennec", "buffered", "C", "1")
ycsb(RM, r"^" + K + r", its file compacted", "sqlite", "buffered", "C", "1")
ycsb(RM, r"at 16 clients, durable, at " + K + r" against", "server-docker", "durable", "B", "16")
ycsb(RM, r"PostgreSQL's " + K + r", both in Docker", "pg", "durable", "B", "16")
