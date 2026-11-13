import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'live.dart';
import 'native.dart';
import 'query.dart';
import 'remote.dart';
import 'values.dart';

part 'schema.dart';
part 'sync.dart';

/// What the library or the builder refused, and why. [code] is the kind:
/// the engine's ([FennecCode.notFound], [FennecCode.duplicate] ...), the
/// boundary's ([FennecCode.misuse], [FennecCode.locked], [FennecCode.panic]),
/// or [FennecCode.builder] for a chain the query builder refused before
/// anything ran -- its message the JS builder's, word for word.
class FennecException implements Exception {
  final FennecCode code;
  final String message;

  /// The parameters the library asked for again as JSON (`exact`).
  final List<int>? exact;

  /// The HTTP status a server refused with ([FennecRemote]): 422 is an
  /// idempotency key sent with another request. Null for the library's own.
  final int? status;

  /// The statement of a [FennecRemote.batch] that stopped it, from 0: the
  /// write whose `require` was not met, the put whose id was taken.
  final int? at;

  /// How many statements of a failed batch stayed applied: 0, since a batch
  /// lands whole, but for one holding a `compact`.
  final int? completed;

  /// The tenant's quota a server refused with ([FennecRemote] to a tenant
  /// node): "bytes" and "rows" answer 507, and room is made by deleting;
  /// "streams", "held", "requests", "writes" and "write_bytes" answer 429
  /// with [retryAfter]. Null for any other refusal.
  final String? quota;

  /// How long to wait before sending again, where the server said: a
  /// quota's 429 to the millisecond (`retry_after_ms`), a move's or a
  /// failover's 503 in whole seconds (`Retry-After`).
  final Duration? retryAfter;

  /// What a `require n else '<reason>'` that refused said
  /// ([FennecCode.unmet]): the statement's own words, by which a caller tells
  /// "insufficient funds" from "no such account" -- where [at] moves with
  /// every statement put in front of it. From the library and a server
  /// alike; null where the statement gave none.
  final String? reason;

  /// The statement bound to the write (`create statement ... after`) that
  /// refused it, by name: a guard's `require` not met, a write of its body
  /// refused. From the library and the server's token alike, a scoped token
  /// told the [reason] and no name; null otherwise.
  final String? binding;

  FennecException(this.code, this.message, [this.exact, this.reason, this.binding])
      : status = null,
        at = null,
        completed = null,
        quota = null,
        retryAfter = null;

  /// A server's refusal: its status and, for a batch, where it stopped; for
  /// a tenant's quota, which one and how long to wait; for a `require`, the
  /// reason it gave; for a statement bound to the write, its name.
  FennecException.http(this.code, this.message,
      {required int this.status, this.at, this.completed, this.quota, this.retryAfter, this.reason, this.binding})
      : exact = null;

  @override
  String toString() => 'FennecException(${code.name}): $message';
}

enum FennecCode {
  type(1),
  notFound(2),
  exists(3),
  duplicate(4),
  corrupt(5),
  query(6),
  io(7),
  plugin(8),
  readOnly(9),
  denied(10),
  panic(11),
  misuse(12),
  locked(13),

  /// A write's `require n` not met: it wrote another count, and was put back.
  unmet(14),

  /// A tenant's quota a server holds it to ([FennecException.quota] names it).
  quota(15),
  builder(100);

  final int value;
  const FennecCode(this.value);

  static FennecCode of(int v) => values.firstWhere((c) => c.value == v, orElse: () => panic);
}

/// One value a `facet` counted, and how many matching rows hold it. The
/// value is as JSON reads it: text, a number, a boolean, a list, a map, or
/// null for the rows whose field is null.
class FacetCount {
  final Object? value;
  final int count;
  const FacetCount(this.value, this.count);

  @override
  bool operator ==(Object other) => other is FacetCount && other.value == value && other.count == count;

  @override
  int get hashCode => Object.hash(value, count);

  @override
  String toString() => 'FacetCount($value, $count)';
}

/// What `facet` counted: each field or path in the order asked, its values
/// most first.
typedef Facets = Map<String, List<FacetCount>>;

Facets? _facetsOf(Object? v) => v is Map
    ? {
        for (final e in v.entries)
          e.key as String: [
            for (final c in (e.value as List).cast<Map>()) FacetCount(c['value'], (c['count'] as num).toInt())
          ]
      }
    : null;

/// A query's rows: a list of maps keyed by field, as before, carrying what
/// `facet` counted beside them -- the counts are over every matching row,
/// not the page, so they belong to no row. A list rather than a type that
/// holds one, so that every caller of `rows()` and every live query reads
/// it as it did.
class Rows with ListMixin<Map<String, Object?>> {
  final List<Map<String, Object?>> _rows;

  /// The counts `facet` asked for, or null when it asked for none.
  final Facets? facets;

  const Rows(this._rows, [this.facets]);

  @override
  int get length => _rows.length;
  @override
  set length(int n) => _rows.length = n;
  @override
  Map<String, Object?> operator [](int i) => _rows[i];
  @override
  void operator []=(int i, Map<String, Object?> row) => _rows[i] = row;
  // ListMixin's own grow the list a null at a time, which a list of
  // non-null maps refuses.
  @override
  void add(Map<String, Object?> element) => _rows.add(element);
  @override
  void addAll(Iterable<Map<String, Object?>> iterable) => _rows.addAll(iterable);
}

/// An answer to a statement: rows, a count of what a write wrote, a word
/// that it was done, or the schemas `collections` and `describe` give.
class Answer {
  final String kind;
  final List<String> columns;
  final Rows rows;
  final int affected;
  final String message;
  final List<Map<String, Object?>> schemas;

  /// What `facet` counted beside the rows, or null when the query asked for none.
  Facets? get facets => rows.facets;

  Answer._(this.kind,
      {this.columns = const [],
      this.rows = const Rows([]),
      this.affected = 0,
      this.message = '',
      this.schemas = const []});

  static Answer of(Map<String, Object?> v) => switch (v['kind']) {
        'rows' => Answer._('rows',
            columns: ((v['result'] as Map)['columns'] as List).cast<String>(),
            rows: Rows(((v['result'] as Map)['rows'] as List).cast<Map<String, Object?>>(),
                _facetsOf((v['result'] as Map)['facets']))),
        'affected' => Answer._('affected', affected: v['count'] as int),
        'ok' => Answer._('ok', message: v['message'] as String),
        'schemas' => Answer._('schemas', schemas: (v['collections'] as List).cast<Map<String, Object?>>()),
        _ => throw FennecException(FennecCode.query, 'an answer of no kind the binding knows: $v'),
      };
}

/// A named statement as `statements` lists it to the server's token
/// ([FennecRemote.statements], [Fennec.statements]), each column null where it
/// says nothing of the statement: a called one has no [after] and no clock.
class NamedStatement {
  final String name;

  /// The history and change its definition landed at, as the server writes
  /// them: signed, a history negative where its top bit is set. Compiled for
  /// the web, a history past 2^53 is read rounded, as JavaScript reads it: it
  /// is an identity, so compare the change, not the history.
  final List<int> version;

  /// `($from int, $to int = ...) claims (sub text)`, as a call is written;
  /// `()` for none.
  final String? signature;
  final bool? writes;
  final String? comment;

  /// Why the schema broke it: it runs no more until it reads again.
  final String? broken;

  /// A write-bound statement's collection and the writes that run it:
  /// `['orders', 'set or del']`.
  final List<String>? after;

  /// A scheduled statement's clause as its text writes it: `every 1h`,
  /// `at '0 3 * * *' in 'Europe/Berlin'`.
  final String? schedule;

  /// The slot due next, in UTC: past while one is overdue or held back.
  final DateTime? next;

  /// Its last run, landed or pending; null before the first.
  final LastRun? last;

  /// A due slot the server's state holds back -- a tenant's quota, a storage
  /// error -- tried again as that changes.
  final HeldSlot? held;

  /// A webhook consumer's hook, whose events are sent to `/_hooks/<hook>`.
  final String? hook;

  /// Whether this server -- on a tenant node, the tenant -- holds the hook's
  /// secret: `false` answers every event of it 401. The app's own database
  /// takes no event, and says `false` for a hook.
  final bool? armed;

  const NamedStatement(this.name, this.version, this.signature, this.writes, this.comment, this.broken, this.after,
      this.schedule, this.next, this.last, this.held,
      [this.hook, this.armed]);

  @override
  String toString() => 'NamedStatement($name${schedule == null ? '' : ', $schedule, next $next'}'
      '${hook == null ? '' : ', hook $hook${armed == true ? '' : ', not armed'}'})';
}

/// A scheduled statement's last run, as `statements` lists it: the slot it
/// ran for (what `slot()` read); `ran`, or `refused` -- the statement's own
/// refusal, a `require` not met or a broken statement, which marks the slot
/// as run -- its status and the `reason` its `require` gave; and the slots
/// passed over before it.
class LastRun {
  final DateTime slot;
  final String outcome;
  final int? status;
  final String? reason;
  final int skipped;
  const LastRun(this.slot, this.outcome, this.status, this.reason, this.skipped);

  @override
  bool operator ==(Object other) =>
      other is LastRun &&
      other.slot == slot &&
      other.outcome == outcome &&
      other.status == status &&
      other.reason == reason &&
      other.skipped == skipped;

  @override
  int get hashCode => Object.hash(slot, outcome, status, reason, skipped);
}

/// A due slot the server's state holds back, as `statements` lists it: its
/// status (507 a tenant's quota) and since when.
class HeldSlot {
  final DateTime slot;
  final int status;
  final DateTime since;
  const HeldSlot(this.slot, this.status, this.since);

  @override
  bool operator ==(Object other) =>
      other is HeldSlot && other.slot == slot && other.status == status && other.since == since;

  @override
  int get hashCode => Object.hash(slot, status, since);
}

/// A row of `statements` as a [NamedStatement]; the times the server's ISO
/// text, read as UTC.
NamedStatement namedStatement(Map<String, Object?> row) {
  DateTime? at(Object? v) => v is String ? DateTime.parse(v) : null;
  final last = row['last'] as Map?;
  final held = row['held'] as Map?;
  return NamedStatement(
    row['name'] as String? ?? '',
    ((row['version'] as List?) ?? const []).cast<num>().map((n) => n.toInt()).toList(),
    row['signature'] as String?,
    row['writes'] as bool?,
    row['comment'] as String?,
    row['broken'] as String?,
    (row['after'] as List?)?.cast<String>(),
    row['schedule'] as String?,
    at(row['next']),
    last == null
        ? null
        : LastRun(at(last['slot'])!, last['outcome'] as String? ?? '', (last['status'] as num?)?.toInt(),
            last['reason'] as String?, (last['skipped'] as num?)?.toInt() ?? 0),
    held == null ? null : HeldSlot(at(held['slot'])!, (held['status'] as num).toInt(), at(held['since'])!),
    row['hook'] as String?,
    row['armed'] as bool?,
  );
}

/// What [Fennec.changes] answers: the counter now, and the collections
/// written since -- null when it cannot be told which, everything stale.
class Changes {
  final int seq;
  final int horizon;
  final List<String>? collections;
  Changes(this.seq, this.horizon, this.collections);
}

/// A fennecdb database in a file on the device -- or in memory -- through the
/// native library (crates/fennec-ffi): the engine and the file format of the
/// server and the browser.
///
/// ```dart
/// final dir = await getApplicationDocumentsDirectory();   // path_provider
/// final db = await Fennec.open('${dir.path}/app.fennec');
/// await db.execute('create collection todos (title text, done bool @hash)');
/// await db.from('todos').insert({'title': 'milk', 'done': false});
/// final open = await db.from('todos').where('done', false).rows();
/// ```
///
/// Every call runs on an isolate the database keeps for itself: a call may
/// wait for the lock, an fsync, a whole open, and an FFI call blocks the
/// isolate that makes it. Open a file once in a process -- a second open is
/// refused ([FennecCode.locked]), since two databases over one file corrupt
/// it.
/// The places the library asked [db] for as JSON for [text]: for the tests,
/// which import it from here, the package exporting it not.
Set<int>? askedPlaces(Fennec db, String text) => db._asked[text];

class Fennec {
  /// Where the native library is, for a test or a desktop app: else
  /// `FENNEC_LIBRARY`, else the platform's place (see the Flutter plugin).
  static String? library;

  /// Writes wait in a buffer for [sync], [flush] or [close] rather than an
  /// fsync each -- a few milliseconds a write on a phone.
  static const noSync = 1;

  /// The file read into memory rather than mapped: for a file whose pages
  /// may become unreadable while it is open, where a mapped page read would
  /// be the process's end.
  static const inMemory = 2;

  /// No compact on its own. Without it the library compacts the file beside
  /// the calls once half of it is dead records -- versions updates and
  /// deletes left behind -- and at least 64 MB.
  static const noAutoCompact = 4;

  final int _handle;
  final Worker _worker;
  var _closed = false;

  /// The close under way or done, which a second [close] awaits.
  Future<void>? _closing;

  /// Writes under way, which a live query's look waits out.
  var _inflight = 0;

  /// By text, the parameters the library asked to have as JSON (`exact`):
  /// a list of numbers it keeps as written -- a json field's, an `[int]`'s,
  /// a `[float]`'s -- goes so at once from the text's second call on, where
  /// sent apart as `f32`s it was refused and sent again, two of the
  /// library's calls each time: 3.38 us a put of an `[int]` of 8 beside a
  /// 128-dim vector against 2.12 (`ffi_bench lists`). Forgotten past 256
  /// texts, as the JavaScript client's are.
  final _asked = <String, Set<int>>{};
  late final Lives lives = Lives(this);

  /// The replica's sync, when the database is one ([Fennec.openSynced]).
  Replica? get replica => _replica;
  Replica? _replica;

  Fennec._(this._handle, this._worker);

  /// Opens the file at [path], made when missing. [flags]: [noSync],
  /// [inMemory], [noAutoCompact].
  static Future<Fennec> open(String path, {int flags = 0}) async {
    final worker = await Worker.start(library);
    try {
      return Fennec._(int.parse(_answer(await worker.call('open', [path, flags]))), worker);
    } catch (_) {
      worker.stop();
      rethrow;
    }
  }

  /// A replica of [shapes] in the file at [path], kept in step with the
  /// server at [url]: reads and live queries are the file's, a write to a
  /// shape's collection is applied at once and sent to the server -- queued
  /// in the file while it cannot be reached, and sent under an idempotency
  /// key, so it lands once. The same [Fennec]: `from`, `live` and `query`
  /// work as they do over a file of the app's own.
  ///
  /// ```dart
  /// final db = await Fennec.openSynced(
  ///   url: 'https://api.example.com', token: jwt,
  ///   shapes: [Shape('todos', where: {'done': false}, key: 'key')],
  ///   path: '${dir.path}/todos.fennec');
  /// ```
  ///
  /// `openSynced` rather than `sync`: Dart has no static and instance
  /// member of one name, and [sync] is the fsync. The requests and the
  /// change stream are `dart:io`'s `HttpClient`'s, with the platform's TLS:
  /// iOS and Android refuse cleartext HTTP by default. [tokenProvider] is
  /// asked for a token when the server answers 401, told whether the
  /// token's `exp` passed (`expired`: refresh it, and the sync goes on) or
  /// the server refused it otherwise -- a session ended or revoked, where
  /// an app signs out and completes with an error rather than hand over
  /// the same token again.
  static Future<Fennec> openSynced({
    required String url,
    String? token,
    required List<Shape> shapes,
    required String path,
    int flags = 0,
    Future<String> Function(bool expired)? tokenProvider,
  }) =>
      _openSynced(url: url, token: token, shapes: shapes, path: path, flags: flags, tokenProvider: tokenProvider);

  /// A fennec-server over HTTP, with no local file: every query goes to the
  /// server, through the same builder ([FennecRemote]). With `reads:
  /// Reads.nearest` the builder's reads go to the tenant's read replica near
  /// the app, where a fennec-shard router has one, after the app's own writes
  /// ([Reads]). [timeout] bounds the wait for an answer's head and between
  /// the pieces of its body; [retry] is how a request goes again where it
  /// cannot have run or may run twice, null for never ([RetryPolicy]).
  static FennecRemote connect(String url,
          {String? token,
          Reads reads = Reads.home,
          Duration timeout = const Duration(seconds: 30),
          RetryPolicy? retry = const RetryPolicy()}) =>
      FennecRemote(url, token: token, reads: reads, timeout: timeout, retry: retry);

  /// A database in memory alone.
  static Future<Fennec> memory() async {
    final worker = await Worker.start(library);
    return Fennec._(int.parse(_answer(await worker.call('memory', []))), worker);
  }

  /// The library's version.
  static Future<String> version() async {
    final worker = await Worker.start(library);
    try {
      return _answer(await worker.call('version', []));
    } finally {
      worker.stop();
    }
  }

  static String _answer((int, String) r) {
    final (code, text) = r;
    if (code == 0) return text;
    final v = (() {
      try {
        return jsonDecode(text) as Map<String, Object?>;
      } catch (_) {
        return const <String, Object?>{};
      }
    })();
    throw FennecException(FennecCode.of(code), v['message'] as String? ?? 'error $code',
        (v['exact'] as List?)?.cast<int>(), v['reason'] as String?, v['binding'] as String?);
  }

  // ------------------------------------------------------------- running

  /// Runs FennecQL -- one statement or several, which land together -- with
  /// [params] for `$1`, `$2` ...
  Future<Answer> run(String text, [List<Object?> params = const []]) => runQuiet(text, params, quiet: false);

  /// The rows a statement answers.
  Future<List<Map<String, Object?>>> query(String text, [List<Object?> params = const []]) async =>
      (await run(text, params)).rows;

  /// A write: how many documents it wrote.
  Future<int> execute(String text, [List<Object?> params = const []]) async => (await run(text, params)).affected;

  /// The query builder over a collection: `db.from('todos').where('done', false).rows()`.
  Query from(String collection) => Query.from(collection).bind(run);

  /// Calls the named statement [name] in this file: `call <name> $1` with
  /// [args], a map of values by name, as `$1` -- [Call] makes the text, so
  /// only the name is ever written into it -- and what the statement answers,
  /// as [run] does: its rows, or how many it wrote. [claims] are the call's
  /// `as`: the values a statement declaring `claims (sub text)` reads as
  /// `$jwt.sub`, an app's signed-in user, held to the declared types as a
  /// token's are; one missing, null or of another type is refused. There is
  /// no `wait` here, nor a key: those are a server's ([FennecRemote.callStatement]).
  /// A replica ([Fennec.openSynced]) holds its shapes and none of its server's
  /// statements, and refuses a call: it goes to the server through
  /// [Fennec.connect]. Named `callStatement` because a method named `call`
  /// would make the database callable as a function.
  ///
  /// ```dart
  /// await db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 300});
  /// final mine = (await db.callStatement('history', claims: {'sub': user.id})).rows;
  /// ```
  Future<Answer> callStatement(String name, {Object? args, Object? claims}) async {
    final s = Call(name, args: args, claims: claims).toCall();
    return run(s.text, s.params);
  }

  /// The named statements this file holds (`statements`): each one's
  /// signature, whether it writes, its comment, why the schema broke it and
  /// what a binding is bound to. A scheduled one is listed with its clock,
  /// though the library runs none: a server's file opened here keeps its
  /// schedules for the server that next opens it.
  Future<List<NamedStatement>> statements() async => (await query('statements')).map(namedStatement).toList();

  /// [run], the live queries' own runs not asking for a look.
  Future<Answer> runQuiet(String text, List<Object?> params, {required bool quiet}) async {
    if (_closed) throw FennecException(FennecCode.misuse, 'the database was closed');
    final values = params.map(normalize).toList();
    if (!quiet) _inflight++;
    try {
      // Looked up only where a list of numbers is among the parameters, so
      // a call with none pays nothing.
      var asJson = values.any((v) => vectorOf(v) != null) ? (_asked[text] ?? const <int>{}) : const <int>{};
      while (true) {
        try {
          return await _send(text, values, asJson);
        } on FennecException catch (e) {
          // A field keeping a list of numbers as written: those the library
          // asks for go again inside the JSON, and so at once the next time.
          // Never a place sent as JSON already: those it names are new, so
          // this ends.
          if (e.exact == null) rethrow;
          final more = {...asJson, ...e.exact!};
          if (more.length == asJson.length) rethrow;
          asJson = more;
          if (_asked.length >= 256 && !_asked.containsKey(text)) _asked.clear();
          _asked[text] = more;
        }
      }
    } finally {
      if (!quiet) {
        _inflight--;
        // After an error too: a text that failed may follow statements
        // that wrote, and the change ring says what landed.
        lives.touch();
        // A write to a synced collection left a request for the server due.
        _replica?._poll();
      }
    }
  }

  bool get writing => _inflight > 0;

  /// Runs [text]: the vectors among the parameters -- a [Float32List], or a
  /// list of finite numbers, which the engine reads as a vector either way
  /// -- go over as their bytes, their place null in the JSON, unless
  /// [asJson] names them. Written out as text and read back, a 128-dim
  /// vector's digits took a put from 4.6 us to 23.3 (`make ffi-bench`).
  Future<Answer> _send(String text, List<Object?> params, Set<int> asJson) async {
    final json = List<Object?>.of(params);
    final vectors = BytesBuilder(copy: false);
    for (var i = 0; i < params.length; i++) {
      if (asJson.contains(i)) continue;
      final f = vectorOf(params[i]);
      if (f == null) continue;
      json[i] = null;
      // Marked exact where each `f32` is the number handed: an `[int]` or a
      // `[float]` takes it as it came, in one call (`fennec_abi::EXACT`).
      final at = exactVector(params[i]) ? i | 1 << 31 : i;
      final b = ByteData(8 + 4 * f.length)
        ..setUint32(0, at, Endian.little)
        ..setUint32(4, f.length, Endian.little);
      for (var k = 0; k < f.length; k++) {
        // A -0 goes as 0, as JSON writes it: either way, one vector.
        b.setFloat32(8 + 4 * k, f[k] == 0 ? 0 : f[k], Endian.little);
      }
      vectors.add(b.buffer.asUint8List());
    }
    final out = _answer(
        await _worker.call('query', [_handle, text, writeJson(json), vectors.isEmpty ? null : vectors.takeBytes()]));
    return Answer.of(jsonDecode(out) as Map<String, Object?>);
  }

  // ------------------------------------------------------------- changes

  /// What changed since [since]: the counter now, and the collections written.
  Future<Changes> changes(int since) async {
    final v = jsonDecode(_answer(await _worker.call('changes', [_handle, since]))) as Map<String, Object?>;
    return Changes(v['seq'] as int, v['horizon'] as int, (v['collections'] as List?)?.cast<String>());
  }

  // ---------------------------------------------------------- durability

  /// Every write so far on disk: written and fsynced, the fsync with no lock held.
  Future<void> sync() async => _answer(await _worker.call('sync', [_handle]));

  /// Every write so far handed to the system, no fsync: it outlives the app
  /// being killed, not the device losing power -- microseconds, what an app
  /// does as it is paused.
  Future<void> flush() async => _answer(await _worker.call('flush', [_handle]));

  /// The file written anew as an image of the database, graphs and all: the
  /// next open links nothing.
  Future<void> checkpoint() async => _answer(await _worker.call('checkpoint', [_handle]));

  /// Builds the hash, text, ordered and sparse indexes an open leaves for
  /// their first read, so the first search does not pay for its index (a
  /// `@text` index of 100 000 products: 141 ms): those [only] names --
  /// collections and `collection.field`s -- or every one. Each under the
  /// read lock on its own, on the database's isolate, so reads go on beside
  /// it. Answers how many it built; call it after the open, as the app
  /// starts.
  Future<int> warm([List<String> only = const []]) async {
    final out = _answer(await _worker.call('warm', [_handle, only.join(',')]));
    return (jsonDecode(out) as Map<String, Object?>)['built'] as int? ?? 0;
  }

  /// Saves the graphs, syncs and lets the file go; the live queries stop.
  /// A close called while another runs waits for it: returning at once, it
  /// let its caller open the file again before the first had let it go.
  Future<void> close() => _closing ??= _close();

  Future<void> _close() async {
    _closed = true;
    await _replica?._stop();
    lives.clear();
    try {
      _answer(await _worker.call('close', [_handle]));
    } finally {
      _worker.stop();
    }
  }
}
