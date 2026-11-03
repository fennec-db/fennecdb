package com.fennecdb

/**
 * A named statement as `statements` lists it to the server's token
 * ([FennecRemote.statements], [Fennec.statements]), each column null where it
 * says nothing of the statement: a called one has no [after] and no clock.
 * Times are milliseconds since the epoch, UTC -- `Instant.ofEpochMilli(it)`
 * -- as `java.time` is on Android from API 26 only.
 */
data class NamedStatement(
    val name: String,
    /** The history and change its definition landed at. */
    val version: List<Long>,
    /** `($from int, $to int = ...) claims (sub text)`, as a call is written; `()` for none. */
    val signature: String?,
    val writes: Boolean?,
    val comment: String?,
    /** Why the schema broke it: it runs no more until it reads again. */
    val broken: String?,
    /** A write-bound statement's collection and the writes that run it: `[orders, set or del]`. */
    val after: List<String>?,
    /** A scheduled statement's clause as its text writes it: `every 1h`, `at '0 3 * * *' in 'Europe/Berlin'`. */
    val schedule: String?,
    /** The slot due next: past while one is overdue or held back. */
    val next: Long?,
    /** Its last run, landed or pending; null before the first. */
    val last: LastRun?,
    /** A due slot the server's state holds back -- a tenant's quota, a storage error -- tried again as that changes. */
    val held: HeldSlot?,
) {
    internal companion object {
        /** A row of `statements` as a [NamedStatement]. */
        fun of(row: Row): NamedStatement = NamedStatement(
            name = row.string("name") ?: "",
            version = row.list("version")?.mapNotNull { (it as? Number)?.toLong() } ?: emptyList(),
            signature = row.string("signature"),
            writes = row.bool("writes"),
            comment = row.string("comment"),
            broken = row.string("broken"),
            after = row.list("after")?.mapNotNull { it as? String },
            schedule = row.string("schedule"),
            next = row.string("next")?.let(::epochMillis),
            last = row.row("last")?.let { r ->
                LastRun(
                    slot = epochMillis(r.string("slot") ?: return@let null),
                    outcome = r.string("outcome") ?: "",
                    status = r.int("status"),
                    reason = r.string("reason"),
                    skipped = r.int("skipped") ?: 0,
                )
            },
            held = row.row("held")?.let { r ->
                HeldSlot(
                    slot = epochMillis(r.string("slot") ?: return@let null),
                    status = r.int("status") ?: 0,
                    since = epochMillis(r.string("since") ?: return@let null),
                )
            },
        )
    }
}

/**
 * A scheduled statement's last run, as `statements` lists it: the slot it
 * ran for (what `slot()` read), in milliseconds since the epoch; `ran`, or
 * `refused` -- the statement's own refusal, a `require` not met or a broken
 * statement, which marks the slot as run -- its status and the `reason` its
 * `require` gave; and the slots passed over before it.
 */
data class LastRun(val slot: Long, val outcome: String, val status: Int?, val reason: String?, val skipped: Int)

/**
 * A due slot the server's state holds back, as `statements` lists it: its
 * status (507 a tenant's quota) and since when, in milliseconds since the
 * epoch.
 */
data class HeldSlot(val slot: Long, val status: Int, val since: Long)

/**
 * The server's ISO text of a time -- `2026-11-03T03:00:00Z`, to the
 * millisecond where it has one -- in milliseconds since the epoch. By hand,
 * as [Json.iso] writes one: `java.time` is on Android from API 26 only.
 */
internal fun epochMillis(text: String): Long {
    val m = Regex("""(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)(?:\.(\d{1,3}))?Z""").matchEntire(text)
        ?: throw FennecException(FennecException.Code.CORRUPT, "not a time the server writes: $text")
    val (y0, mo, d, h, mi, s) = m.destructured
    val ms = m.groupValues[7].padEnd(3, '0').toLong()
    // Howard Hinnant's days-from-civil.
    val y = y0.toLong() - (if (mo.toLong() <= 2) 1 else 0)
    val era = (if (y >= 0) y else y - 399) / 400
    val yoe = y - era * 400
    val mp = (mo.toLong() + 9) % 12
    val doy = (153 * mp + 2) / 5 + d.toLong() - 1
    val doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
    val days = era * 146_097 + doe - 719_468
    return days * 86_400_000L + h.toLong() * 3_600_000L + mi.toLong() * 60_000L + s.toLong() * 1000L + ms
}
