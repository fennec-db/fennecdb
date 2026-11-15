/*
 * The release's C library for Linux, used as a program uses it.
 * tools/linux-ffi-check.sh compiles this against the tarball's fennec.h,
 * links it to the shared library and to the static one, each through its
 * pkg-config file, and runs both, on the runner and in a container of the
 * oldest glibc the library is said to run on.
 *
 *   linux-ffi-check <version> <folder>
 *
 * It asks the library its version, opens a file in <folder>, writes a row
 * with parameters and reads it back, is refused a second open of the file
 * and a statement that does not parse, closes it and finds the row after
 * an open again; then starts a sync on a database in memory and drives it
 * as a binding would, with no network: the stream it asks for, a seed fed
 * to it, a write queued for the server. Each step prints what it checked;
 * the first that fails prints what it got and exits 1.
 */
#include <fennec.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void fail(const char *what, int32_t code, const char *got)
{
    fprintf(stderr, "linux-ffi-check: %s: code %d, %s\n", what, (int)code,
            got ? got : "(no answer)");
    exit(1);
}

static void ok(const char *what) { printf("ok: %s\n", what); }

/* The answer a call wrote, held to its length; the caller frees it. */
static char *answer(const char *what, int32_t code, char *out, size_t len)
{
    if (out && strlen(out) != len) fail(what, code, "an answer whose length is not out_len");
    return out;
}

/* Runs FennecQL with its parameters ("" for none), wanting `want` back. */
static char *query(uint64_t h, const char *text, const char *params, int32_t want)
{
    char *out = NULL;
    size_t len = 0;
    int32_t code = fennec_query(h, (const uint8_t *)text, strlen(text),
                                (const uint8_t *)params, strlen(params), NULL, 0, &out, &len);
    answer(text, code, out, len);
    if (code != want) fail(text, code, out);
    return out;
}

static void holds(const char *what, const char *text, const char *part)
{
    if (!text || !strstr(text, part)) {
        fprintf(stderr, "linux-ffi-check: %s: wanted %s in %s\n", what, part,
                text ? text : "(no answer)");
        exit(1);
    }
}

static void done(char *text) { fennec_free_string(text); }

static uint64_t open_file(const char *path, int32_t want)
{
    uint64_t h = 0;
    char *out = NULL;
    size_t len = 0;
    int32_t code = fennec_open((const uint8_t *)path, strlen(path), 0, &h, &out, &len);
    answer(path, code, out, len);
    if (code != want) fail("fennec_open", code, out);
    if (want != FENNEC_OK) holds("fennec_open's refusal", out, "\"kind\":\"error\"");
    done(out);
    return h;
}

static void close_handle(uint64_t h)
{
    char *out = NULL;
    int32_t code = fennec_close(h, &out, NULL);
    if (code != FENNEC_OK) fail("fennec_close", code, out);
    done(out);
}

/* Tells the sync what happened and returns the actions now due. */
static char *feed(uint64_t h, uint32_t kind, uint64_t id, int32_t status, const char *bytes)
{
    char *out = NULL;
    size_t len = 0;
    int32_t code = fennec_sync_feed(h, kind, id, status, 0, (const uint8_t *)bytes,
                                    strlen(bytes), &out, &len);
    answer("fennec_sync_feed", code, out, len);
    if (code != FENNEC_OK) fail("fennec_sync_feed", code, out);
    return out;
}

int main(int argc, char **argv)
{
    char path[4096];
    char *r;
    uint64_t h, m;

    if (argc != 3) {
        fprintf(stderr, "usage: %s <version> <folder>\n", argv[0]);
        return 2;
    }
    if (strcmp(fennec_version(), argv[1]) != 0) fail("fennec_version", 0, fennec_version());
    ok("the library is the release's version");

    if (snprintf(path, sizeof path, "%s/check.fennec", argv[2]) >= (int)sizeof path) {
        fprintf(stderr, "linux-ffi-check: the folder's path is too long\n");
        return 2;
    }
    remove(path);
    h = open_file(path, FENNEC_OK);
    done(query(h, "create collection notes (title text, n int @hash)", "", FENNEC_OK));
    r = query(h, "put notes {title: $1, n: $2}", "[\"from C\", 7]", FENNEC_OK);
    holds("a put", r, "{\"kind\":\"affected\",\"count\":1}");
    done(r);
    r = query(h, "get notes select title where n = 7", "", FENNEC_OK);
    holds("a get", r, "{\"title\":\"from C\"}");
    done(r);
    ok("a file opened, a row written with parameters and read back");

    open_file(path, FENNEC_LOCKED);
    ok("a second open of the file refused while it is open");
    r = query(h, "broken query", "", FENNEC_QUERY);
    holds("a statement that does not parse", r, "{\"kind\":\"error\",\"message\":");
    done(r);
    ok("a statement that does not parse refused with its error");

    close_handle(h);
    h = open_file(path, FENNEC_OK);
    r = query(h, "get notes select title, n", "", FENNEC_OK);
    holds("the row after an open again", r, "{\"title\":\"from C\",\"n\":7}");
    done(r);
    close_handle(h);
    ok("the row there after a close and an open again");

    /* The sync core, as fennec-ffi's tests drive it: no network, the
     * binding's part played here. */
    m = 0;
    r = NULL;
    if (fennec_open_memory(&m, &r, NULL) != FENNEC_OK) fail("fennec_open_memory", 0, r);
    done(r);
    done(query(m, "create collection tasks (key text @hash, title text)", "", FENNEC_OK));
    {
        const char *config = "{\"url\":\"https://example.test\",\"token\":\"tk\",\"seed\":\"ab\","
                             "\"shapes\":[{\"collection\":\"tasks\",\"key\":\"key\"}]}";
        size_t len = 0;
        int32_t code;
        r = NULL;
        code = fennec_sync_start(m, (const uint8_t *)config, strlen(config), &r, &len);
        answer("fennec_sync_start", code, r, len);
        if (code != FENNEC_OK) fail("fennec_sync_start", code, r);
    }
    holds("the sync's first action", r,
          "\"do\":\"stream\",\"id\":1,\"url\":\"https://example.test/tasks/changes\"");
    done(r);
    done(feed(m, FENNEC_SYNC_OPENED, 1, 200, ""));
    r = feed(m, FENNEC_SYNC_BYTES, 1, 0,
             "event: seed\ndata: {\"seq\":3,\"rows\":[{\"id\":1,\"key\":\"x\",\"title\":\"one\"}]}\n\n");
    holds("the seed taken", r, "changed");
    done(r);
    r = query(m, "put tasks {title: \"two\"}", "", FENNEC_OK);
    holds("a write to a synced collection", r, "{\"kind\":\"affected\",\"count\":1}");
    done(r);
    r = feed(m, FENNEC_SYNC_POLL, 0, 0, "");
    holds("the write sent to the server", r, "\"do\":\"request\"");
    holds("the write sent to the server", r, "/query");
    done(r);
    r = query(m, "get tasks select title order title", "", FENNEC_OK);
    holds("the seed's row and the write", r, "[{\"title\":\"one\"},{\"title\":\"two\"}]");
    done(r);
    r = NULL;
    if (fennec_sync_status(m, &r, NULL) != FENNEC_OK) fail("fennec_sync_status", 0, r);
    holds("the sync's status", r, "\"pending\":1");
    done(r);
    close_handle(m);
    ok("a sync started, seeded and a write queued for the server");
    return 0;
}
