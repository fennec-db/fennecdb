/*
 * fennecdb as a native library: the C ABI of crates/fennec-ffi.
 *
 * Written by hand, and held to the library by crates/fennec-ffi/tests/ffi.rs
 * (every function declared here is exported, and every export declared).
 *
 * A handle is a number, safe to use from several threads at once: a read
 * runs beside other reads, a write takes the database to itself. Every call
 * that takes one may block -- for the lock, an fsync, a whole open -- so
 * none belongs on an app's main thread.
 *
 * Every call returns 0 or an error code. Where it has an answer, or an
 * error to tell, it writes NUL-terminated UTF-8 JSON through `out` and its
 * length through `out_len` (either may be NULL); the caller frees it with
 * fennec_free_string. An error is {"kind":"error","message":"..."}.
 */
#ifndef FENNEC_H
#define FENNEC_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The codes a call returns. 1 to 10, 14 and 15 are the engine's errors by kind. */
#define FENNEC_OK 0
#define FENNEC_TYPE 1       /* a value that does not fit its field */
#define FENNEC_NOT_FOUND 2  /* a collection, field or document not there */
#define FENNEC_EXISTS 3     /* a collection there already */
#define FENNEC_DUPLICATE 4  /* an insert over an id held, a @unique value held */
#define FENNEC_CORRUPT 5    /* bytes that are not a fennecdb file */
#define FENNEC_QUERY 6      /* a statement that does not parse or run */
#define FENNEC_IO 7         /* the disk refused; writes stop until a reopen */
#define FENNEC_PLUGIN 8
#define FENNEC_READ_ONLY 9
#define FENNEC_DENIED 10
#define FENNEC_PANIC 11     /* a panic inside the library, caught */
#define FENNEC_MISUSE 12    /* a handle not open, a NULL, text not UTF-8 */
#define FENNEC_LOCKED 13    /* the file is open already, here or elsewhere */
#define FENNEC_UNMET 14     /* a write's `require <n>` not met; its block put back */
#define FENNEC_QUOTA 15     /* a write past a server's quota; the library sets none */

/* fennec_open's flags. */
#define FENNEC_OPEN_NO_SYNC 1  /* writes wait for fennec_sync, not an fsync each */
#define FENNEC_OPEN_IN_MEMORY 2 /* read the file into memory rather than map it */
#define FENNEC_OPEN_NO_AUTO_COMPACT 4 /* no compact on its own once half the file is dead */

/* fennec_query's vectors: a place's top bit, each f32 the number handed. */
#define FENNEC_VECTOR_EXACT 0x80000000u

/* The library's version; static, not to be freed. */
const char *fennec_version(void);

/* Opens the file at path (made when missing) and writes its handle. */
int32_t fennec_open(const uint8_t *path, size_t path_len, uint32_t flags,
                   uint64_t *handle, char **out, size_t *out_len);

/* Opens a database held in memory alone. */
int32_t fennec_open_memory(uint64_t *handle, char **out, size_t *out_len);

/* Waits for the calls in flight, saves the graphs, syncs, lets the file go. */
int32_t fennec_close(uint64_t handle, char **out, size_t *out_len);

/*
 * Runs FennecQL. params: a JSON array for $1, $2 ... (length 0 for none).
 * vectors: the parameters that are vectors, as f32s -- for each, its place
 * and its length as little-endian uint32s, then its values -- where the
 * JSON holds null; NULL for none. A place with its top bit set
 * (FENNEC_VECTOR_EXACT) marks a vector each of whose f32s is the number
 * the caller was handed: an [int], [float] or [timestamp] takes it as it
 * came, never asking for it again. Writes the answer:
 * {"kind":"rows","result":{"columns":[...],"rows":[...]}},
 * {"kind":"affected","count":N}, {"kind":"ok","message":...} or
 * {"kind":"schemas","collections":[...]}. An error holding "exact":[places]
 * asks for those parameters again inside the JSON.
 */
int32_t fennec_query(uint64_t handle, const uint8_t *text, size_t text_len,
                    const uint8_t *params, size_t params_len,
                    const uint8_t *vectors, size_t vectors_len,
                    char **out, size_t *out_len);

/* {"seq":N,"horizon":M,"collections":[...]|null}: what changed since `since`. */
int32_t fennec_changes(uint64_t handle, uint64_t since, char **out, size_t *out_len);

/* fennec_schema's modes. */
#define FENNEC_SCHEMA_PLAN 0    /* what an apply would do; writes nothing */
#define FENNEC_SCHEMA_APPLY 1   /* migrations not yet recorded, then what only adds: one block */
#define FENNEC_SCHEMA_FOLLOW 2  /* compares a database another owns */
#define FENNEC_SCHEMA_DESCRIBE 3 /* the database's schema as a description; text unread */

/*
 * The database and a schema declared in code: text is the JSON description
 * every SDK's declarations compile to, {"format":1,"collections":[...],
 * "migrations":[...]}. Writes {"kind":"schema","applied":..,"ran":..,
 * "migrations":[n...],"statements":[...],"refusals":[{"kind","collection",
 * "field","message","fix"}...]}; nothing is applied while anything is
 * refused.
 */
int32_t fennec_schema(uint64_t handle, const uint8_t *text, size_t text_len, uint32_t mode,
                     char **out, size_t *out_len);

/* Every write so far written and fsynced. */
int32_t fennec_sync(uint64_t handle, char **out, size_t *out_len);

/* Every write so far handed to the system, no fsync: outlives the app, not power. */
int32_t fennec_flush(uint64_t handle, char **out, size_t *out_len);

/* The file written anew as an image, graphs and all. */
int32_t fennec_checkpoint(uint64_t handle, char **out, size_t *out_len);

/*
 * Builds the hash, text, ordered and sparse indexes an open leaves for their
 * first read: those `only` names (collections and collection.fields by
 * commas), or every one when it is empty. Each under the read lock on its
 * own; call it off the main thread after the open. Writes {"built":N}.
 */
int32_t fennec_warm(uint64_t handle, const uint8_t *only, size_t only_len, char **out,
                   size_t *out_len);

/*
 * Sync with a server: a state machine with no I/O of its own. The binding
 * makes the requests and the event stream with its platform's HTTP client
 * (TLS, the system's trust store), feeds what happened, and performs the
 * actions each call writes, a JSON array of
 *   {"do":"request","id":N,"method":..,"url":..,"headers":{..},"body":..|null}
 *   {"do":"stream","id":N,"url":..,"headers":{..}}   an SSE body, fed as it comes
 *   {"do":"cancel","id":N}     {"do":"wait","id":N,"ms":M}
 *   {"do":"token","code":..|null,"had_token":B,"attempt":N}
 *   {"do":"changed"}           {"do":"status"}
 *   {"do":"refused","status":S,"message":..,"query":..,"quota":..,"reason":..}
 * A token action is a 401: nothing is sent until a token is signalled.
 * "code" is the one the server named, null for none -- "token_expired"
 * where the token's exp passed; "had_token" whether the refused request
 * carried a token; "attempt" the asks in a row since the server last took
 * the token held (a 2xx, a stream opened), 2 and on where the token
 * signalled at the last was refused too, an expiry starting it at 1 again
 * for the token the server took last or the first signalled since (any
 * other was minted expired: a clock ahead of the issuer's). Past 1 the
 * action comes after a wait, 1 s doubling to 60 s, which the same token
 * signalled again does not end: a token minted again at once is refused
 * for what minting does not change (a key the server has not fetched yet,
 * an nbf its clock has not reached). An app asks its issuer for a fresh
 * token at every one, whatever the code, and signs out only where the
 * issuer refuses to issue one: the server keeps no list of revoked tokens.
 * A file is one account's -- its path names the account, and it is never
 * opened for another -- and a sign-out keeps it, for that account's next
 * sign-in to send its pending writes. A 401 to a request sent with a token
 * since replaced is sent again with the new one, no action. One ask is out
 * at a time: a binding answers it with {"token":..}, or with
 * {"token_failed":"why"} where its provider threw or handed over none --
 * the status's error says so -- and calls its provider once at a time, a
 * token action meanwhile answered by that call; then {"online":true} asks
 * again, never while an ask is out or during the wait.
 * A binding reading only "do" asks for a token at every one.
 * Once started, a write through fennec_query to a synced collection is
 * applied at once and queued for the server in the same block.
 */
#define FENNEC_SYNC_POLL 0     /* nothing happened: what is due */
#define FENNEC_SYNC_RESPONSE 1 /* a request's answer: status (0: none), seq, body */
#define FENNEC_SYNC_OPENED 2   /* a stream's status; its body when not 200 */
#define FENNEC_SYNC_BYTES 3    /* a piece of a stream's body */
#define FENNEC_SYNC_CLOSED 4   /* a stream ended (or never opened): why */
#define FENNEC_SYNC_TIMER 5    /* a wait ran out */
#define FENNEC_SYNC_SIGNAL 6   /* {"online":bool} | {"token":".."} | {"stop":true}
                                  | {"token_failed":".."}: the provider gave none */

/* config: {"url":..,"token":..,"seed":"<32 hex>","shapes":[{collection,where?,select?,key?}]}. */
int32_t fennec_sync_start(uint64_t handle, const uint8_t *config, size_t config_len,
                         char **out, size_t *out_len);

/* Tells the sync what happened (FENNEC_SYNC_*); writes the actions now due. */
int32_t fennec_sync_feed(uint64_t handle, uint32_t kind, uint64_t id, int32_t status,
                        uint64_t seq, const uint8_t *bytes, size_t len,
                        char **out, size_t *out_len);

/*
 * {"state":"online"|"offline"|"catching_up","pending":N,"error":..,"shapes":[..]},
 * the error null or {"message":..,"status":S,"quota":..|null,"code":..|null,
 * "had_token":B|null,"attempt":N|null}, its code the one the 401 a token is
 * wanted for named ("token_expired"), had_token and attempt the token
 * action's once one was asked for, null while the wait before it runs; its
 * message "the token provider failed: <why>" after {"token_failed":..}.
 */
int32_t fennec_sync_status(uint64_t handle, char **out, size_t *out_len);

/* Frees text a call wrote through `out`. */
void fennec_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* FENNEC_H */
