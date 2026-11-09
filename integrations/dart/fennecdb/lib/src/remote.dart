import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'fennec.dart';
import 'query.dart';
import 'values.dart';

/// What [FennecRemote.batch] answers: each statement's answer in order, the
/// change the batch left the database at, and whether the answer is the one
/// kept for its idempotency key ([replayed]) -- a replayed answer carries
/// no [seq].
class BatchAnswer {
  final List<Answer> results;
  final int? seq;
  final bool replayed;

  const BatchAnswer(this.results, this.seq, this.replayed);
}

/// Where a connection's reads go. [home], the default: every request to the
/// tenant's home. [nearest]: the builder's reads, and a
/// [FennecRemote.asRead] copy's requests, are marked (`Fennec-Read: replica`)
/// so that a fennec-shard router near a region holding the tenant's read
/// replica answers them there, each sent after the furthest position an
/// answer from home named to this connection (`Fennec-After`,
/// `Fennec-History`: the change and its history) -- never older than its own
/// writes, nor a write a failover replaced. Writes go home.
enum Reads { home, nearest }

/// Where an answer from home left the tenant: its change (`Fennec-Seq`) and
/// the history that change is of (`Fennec-History`, `<term>-<id>`; none for a
/// database that never had a replica).
class _Position {
  final int seq;
  final String? history;

  const _Position(this.seq, this.history);

  /// The history's term, one more at each promotion.
  int get term {
    final h = history;
    if (h == null) return 0;
    final dash = h.indexOf('-');
    return int.tryParse(dash < 0 ? h : h.substring(0, dash)) ?? 0;
  }

  /// The history's lineage: its id's top half, which every fork of the
  /// tenant keeps and a tenant made again under its name does not.
  String? get lineage {
    final h = history;
    final dash = h?.indexOf('-') ?? -1;
    return h == null || dash < 0 || h.length < dash + 9 ? null : h.substring(dash + 1, dash + 9);
  }

  /// Whether this is of another lineage than [q]: a tenant made again.
  bool isOfAnotherLineage(_Position q) {
    final a = lineage, b = q.lineage;
    return a != null && b != null && a != b;
  }

  /// Whether this is past [q]: of another lineage -- home's word on a tenant
  /// made again, whose terms start again -- or a later term of the same, or
  /// further in one.
  bool isPast(_Position? q) => q == null || isOfAnotherLineage(q) || term > q.term || (term == q.term && seq > q.seq);
}

/// How a connection sends a request again: only where nothing ran, or where
/// running it again changes nothing (docs/design/js-client.md#retries).
///
/// Nothing ran: a 503, or a quota's 429, naming its wait -- sent after that
/// wait, never sooner, unless its body says statements of a batch were made
/// (`completed`) -- and a connection refused. Running it again changes
/// nothing: a read the builder made, one sent near (`Fennec-Read: replica`,
/// which every server runs read-only), and a write under an
/// `Idempotency-Key` -- these go again after a reset or a dropped connection
/// too. Never after the connection's own timeout, past which the server may
/// still run it, nor once the caller aborts ([FennecRemote.withAbort]), nor
/// after a refusal naming no wait; an [FennecRemote.asRead] text sent home is
/// a claim nothing checks, and goes once.
///
/// [attempts] tries at most, their waits -- the server's, or 250 ms
/// doubling, 30% jitter on top -- within [maxWait]; past either, the last
/// answer or error is the caller's. `retry: null` on [Fennec.connect] sends
/// nothing again.
class RetryPolicy {
  final int attempts;
  final Duration maxWait;

  const RetryPolicy({this.attempts = 3, this.maxWait = const Duration(seconds: 10)});
}

/// What a connection and the copies [FennecRemote.withIdempotencyKey],
/// [FennecRemote.withWait] and [FennecRemote.asRead] make of it share, as Go's
/// and .NET's copies share theirs: the connections, the token and the
/// furthest position.
class _Shared {
  final HttpClient client;
  final Reads reads;
  final Duration timeout;
  final RetryPolicy? retry;
  String? token;

  /// The furthest position an answer from home named, which a read near is
  /// sent after: an answer that arrives late is not the last.
  _Position? at;

  _Shared(this.token, this.reads, this.timeout, this.retry) : client = HttpClient()..connectionTimeout = timeout;
}

/// What a [FennecRemote.withAbort] copy's requests are given up by: the
/// requests under way, aborted as the trigger completes, and the waits
/// between tries, ended.
class _Abort {
  bool fired = false;
  final requests = <HttpClientRequest>{};
  final waits = <Completer<void>>{};

  _Abort(Future<void> trigger) {
    trigger.then<void>((_) => _fire(), onError: (Object _) => _fire());
  }

  void _fire() {
    fired = true;
    for (final r in requests) {
      r.abort(const HttpException('aborted'));
    }
    for (final w in waits) {
      if (!w.isCompleted) w.complete();
    }
  }
}

final _random = Random();

/// ECONNREFUSED as the platform numbers it: a connection refused sent
/// nothing, and any request goes again.
final int _refusedCode = Platform.isLinux || Platform.isAndroid
    ? 111
    : Platform.isWindows
        ? 10061
        : 61;

/// A fennec-server over HTTP, with no file on the device: every query goes
/// to the server (`POST /query`), through the same builder. What
/// [Fennec.connect] makes, as `connect` does in JS.
///
/// ```dart
/// final db = Fennec.connect('https://api.example.com', token: jwt);
/// final open = await db.from('todos').where('done', false).rows();
/// ```
///
/// The requests are `dart:io`'s `HttpClient`'s, so TLS is the platform's. A
/// request goes again only where it cannot have run or may run twice
/// ([RetryPolicy]), and [timeout] bounds the wait for an answer's head and each
/// wait between the pieces of its body.
class FennecRemote {
  final String url;
  final _Shared _shared;
  final String? _idempotencyKey;

  /// How long the server may hold a write that writes nothing ([withWait]).
  final Duration? _wait;

  /// This copy's requests only read ([asRead], the builder's reads).
  final bool _reading;

  /// This copy's requests are the builder's reads ([from]): reads the
  /// builder made, which may run twice -- unlike an [asRead] text, a claim
  /// nothing checks at home.
  final bool _built;

  /// What gives this copy's requests up ([withAbort]), null for nothing.
  final _Abort? _abort;

  FennecRemote(String url,
      {String? token,
      Reads reads = Reads.home,
      Duration timeout = const Duration(seconds: 30),
      RetryPolicy? retry = const RetryPolicy()})
      : url = url.endsWith('/') ? url.substring(0, url.length - 1) : url,
        _shared = _Shared(token, reads, timeout, retry),
        _idempotencyKey = null,
        _wait = null,
        _reading = false,
        _built = false,
        _abort = null {
    if (timeout <= Duration.zero) throw FennecException(FennecCode.misuse, 'bad timeout: $timeout');
    if (retry != null && (retry.attempts < 1 || retry.maxWait.isNegative)) {
      throw FennecException(FennecCode.misuse, 'bad retry: ${retry.attempts} attempts, ${retry.maxWait}');
    }
  }

  FennecRemote._copy(FennecRemote of, String? key, Duration? wait, bool reading, {bool built = false, _Abort? abort})
      : url = of.url,
        _shared = of._shared,
        _idempotencyKey = key,
        _wait = wait,
        _reading = reading,
        _built = built,
        _abort = abort ?? of._abort;

  /// Where this connection's reads go ([Fennec.connect]'s `reads`).
  Reads get reads => _shared.reads;

  /// How long to an answer's head, a [withWait] copy's wait on top, and
  /// between the pieces of its body ([Fennec.connect]'s `timeout`).
  Duration get timeout => _shared.timeout;

  /// How a request goes again, null for never ([Fennec.connect]'s `retry`).
  RetryPolicy? get retry => _shared.retry;

  /// The token the requests carry; set a fresh one for the requests from
  /// here on.
  String? get token => _shared.token;
  set token(String? t) => _shared.token = t;

  /// The furthest change an answer from home -- a write's, a read-only
  /// batch's -- left the database at (`Fennec-Seq`), through this connection
  /// or a copy of it; null before any. Answers arriving out of order do not
  /// move it back, a read near's answer, which may be a replica's, moves it
  /// not, and a read near's 504 sets it to where home says it stands, as
  /// Go's `Seq()` and .NET's `Seq` are.
  int? get seq => _shared.at?.seq;

  /// A copy whose writes -- [run], [batch] and the builder's -- carry [key]
  /// as their `Idempotency-Key`: sent again after a timeout, a write is
  /// answered as it was the first time and not made twice. One key a write:
  /// the same key with another request is refused (status 422). It shares
  /// this connection's token, `seq` and connections.
  ///
  /// ```dart
  /// await db.withIdempotencyKey(orderId).from('orders').insert(order);
  /// ```
  FennecRemote withIdempotencyKey(String key) {
    if (key.isEmpty) throw FennecException(FennecCode.misuse, 'an idempotency key is a text, not empty');
    return FennecRemote._copy(this, key, _wait, _reading);
  }

  /// A copy whose `set` and `del` statements -- [run], [batch] and the
  /// builder's `update`, `delete` and their `Returning` forms -- the server
  /// holds while they write nothing, until they can or for [wait], at most
  /// 30 seconds (`Fennec-Wait`): a worker's claim waits there for a job --
  /// one enqueued, a delayed one come due, a lease lapsed -- rather than
  /// sleeping and claiming again.
  ///
  /// ```dart
  /// final claimed = await db.withWait(const Duration(seconds: 30)).from('jobs')
  ///     .where(Cond.raw('run_at <= now()')).order('run_at').limit(10).updateReturning(claim);
  /// ```
  FennecRemote withWait(Duration wait) {
    if (wait.isNegative) throw FennecException(FennecCode.misuse, 'a wait is not negative');
    return FennecRemote._copy(this, _idempotencyKey, wait, _reading);
  }

  /// A copy whose requests only read: on a connection made with `reads:
  /// Reads.nearest` a router near the tenant's read replica answers them
  /// there, after this connection's writes. The builder's reads go so on
  /// their own; a text the caller knows reads -- a [run], a [batch] of
  /// `toFennecQL()`s -- goes through this, and one that writes is refused
  /// (status 403), never landed. On a connection reading home it changes
  /// nothing. It shares this connection's token, `seq` and connections.
  ///
  /// ```dart
  /// final n = await db.asRead().query(r'get orders where customer = $1 count', ['c-7']);
  /// ```
  FennecRemote asRead() => FennecRemote._copy(this, _idempotencyKey, _wait, true);

  /// A copy whose requests are given up once [trigger] completes: the one
  /// under way is aborted, its connection closed -- a held claim the server
  /// gives up at once -- a wait before another try ends, and nothing goes
  /// again; the call throws a [FennecException] ([FennecCode.io]). Copies
  /// made of it keep the trigger.
  ///
  /// ```dart
  /// final gone = Completer<void>();
  /// final claim = db.withAbort(gone.future).withWait(const Duration(seconds: 30)).run(q);
  /// gone.complete(); // the screen closed
  /// ```
  FennecRemote withAbort(Future<void> trigger) =>
      FennecRemote._copy(this, _idempotencyKey, _wait, _reading, built: _built, abort: _Abort(trigger));

  /// Runs FennecQL on the server with [params] for `$1`, `$2` ... A key for
  /// one write is [withIdempotencyKey]'s: Dart takes no named parameter
  /// beside the optional [params].
  Future<Answer> run(String text, [List<Object?> params = const []]) async {
    final (v, _, _) = await _post(
        '/query', 'application/json', writeJson({'query': text, 'params': params.map(normalize).toList()}), null);
    return _answer(v);
  }

  /// `POST /batch`: the statements in order under one write lock, as one
  /// block -- their writes all land, or at the first error none of them do,
  /// a [FennecException] whose [FennecException.at] is the statement that
  /// stopped it ([FennecCode.unmet] for a write's `require` not met, its
  /// [FennecException.reason] the one the statement gave, which stays put as
  /// statements go in front of it where `at` moves). A statement is what
  /// the builder's `toInsert`, `toUpdate`, `toDelete` and `toFennecQL`
  /// make. With [idempotencyKey] a retry after a timeout is
  /// answered as the first try was and writes nothing twice; a batch of
  /// reads alone takes no key.
  Future<BatchAnswer> batch(List<Statement> statements, {String? idempotencyKey}) async {
    final lines = [
      for (final s in statements) writeJson({'query': s.text, 'params': s.params.map(normalize).toList()})
    ];
    final (v, seq, replayed) = await _post('/batch', 'application/x-ndjson', lines.join('\n'), idempotencyKey);
    final results = v is Map && v['results'] is List ? (v['results'] as List).map(_answer).toList() : <Answer>[];
    return BatchAnswer(results, seq, replayed);
  }

  /// A POST's JSON answer, its `Fennec-Seq` and whether it was replayed; a
  /// refusal as a [FennecException] with its status and, for a batch, where
  /// it stopped.
  Future<(Object?, int?, bool)> _post(String path, String type, String payload, String? key) async {
    // A read near: marked for the tenant's read replica in the router's
    // region, after the furthest position home named to this connection,
    // which the replica waits for -- that change of that history.
    var near = _reading && _shared.reads == Reads.nearest;
    // A position of change 0 is none: nothing to wait for, as Go and .NET
    // send none.
    final at = _shared.at;
    final sent = near && at != null && at.seq > 0 ? at : null;
    // What may run twice, sent again after a reset too: a read the builder
    // made, one sent near, which every server runs read-only
    // (`Fennec-Read`), and a write under a key. An [asRead] text sent home
    // is a claim nothing checks: a statement it calls may have been made to
    // write since.
    final twice = _built || (key ?? _idempotencyKey) != null;
    final bytes = utf8.encode(payload);
    var (status, body, headers) = await _ask(path, type, bytes, key, near, sent, twice || near);
    // 504: no copy holds that write in the wait -- home had none either,
    // lost in a failover. Home says where it stands, which the connection
    // takes as its position, lower or not; asked home again with no mark,
    // the read is answered with what home holds. Taken only over the
    // position this request was sent after -- a write answered meanwhile is
    // further, and set back the next read near would miss it -- and never
    // of an earlier term of the same lineage: a router whose copy of the
    // directory lags a failover sends the read to the old primary, fenced,
    // which names the branch it abandoned. Another lineage's is a tenant
    // made again under the name, which starts again at term 1: refused,
    // every read near waited its whole wait for good.
    if (near && sent != null && status == 504) {
      final home = _decoded(body);
      if (home is Map && home['seq'] is int) {
        final history = home['history'];
        final p = _Position(home['seq'] as int, history is String ? history : null);
        if (identical(_shared.at, sent) && (p.term >= sent.term || p.isOfAnotherLineage(sent))) _shared.at = p;
      }
      near = false;
      (status, body, headers) = await _ask(path, type, bytes, key, false, null, twice);
    }
    final Object? v;
    try {
      v = body.isEmpty ? null : jsonDecode(body);
    } catch (_) {
      throw FennecException(FennecCode.io, 'the server did not answer JSON ($status)');
    }
    if (status < 200 || status > 299) {
      throw refused(status, v is Map ? v : null, headers.value('retry-after'));
    }
    final seq = int.tryParse(headers.value('fennec-seq') ?? '');
    // A position from home alone: a read near's answer may be a replica's,
    // behind home or on a branch a failover left.
    if (seq != null && !near) {
      final p = _Position(seq, headers.value('fennec-history'));
      if (p.isPast(_shared.at)) _shared.at = p;
    }
    return (v, seq, headers.value('idempotent-replayed') == 'true');
  }

  /// The request and its answer's status, body and head, sent again by the
  /// rule (docs/design/js-client.md#retries, [RetryPolicy]): what ran
  /// nothing -- a 503 or 429 naming its wait, after it, unless statements of
  /// a batch landed (`completed`); a connection refused -- and, where
  /// [twice], a reset or a dropped connection too. Never after the connection's own
  /// timeout, which a request may still run past, nor once the caller
  /// aborted. `HttpClient` sends nothing again of its own.
  Future<(int, String, HttpHeaders)> _ask(
      String path, String type, List<int> bytes, String? key, bool near, _Position? after, bool twice) async {
    final r = _shared.retry;
    final abort = _abort;
    var waited = Duration.zero;
    for (var n = 0;; n++) {
      var wait = Duration.zero;
      (int, String, HttpHeaders)? got;
      Object? failure;
      try {
        got = await _once(path, type, bytes, key, near, after);
        final (status, body, headers) = got;
        if (status == 503 || status == 429) {
          final v = _decoded(body);
          final m = v is Map ? v : null;
          final done = m?['completed'];
          if (!(done is num && done > 0)) wait = _waitOf(m, headers.value('retry-after')) ?? Duration.zero;
        }
        if (wait == Duration.zero) return got;
      } catch (e) {
        if (abort != null && abort.fired) throw FennecException(FennecCode.io, 'the request was aborted');
        // The connection's own timeout: a `compact`, a long read may still be
        // running, and three tries ran a read three times.
        if (e is TimeoutException || _timedOut(e)) {
          throw FennecException(FennecCode.io, 'no answer within the timeout of ${_shared.timeout}: $e');
        }
        if (e is! IOException) rethrow;
        if (!(twice || _refused(e))) throw FennecException(FennecCode.io, 'the server could not be reached: $e');
        failure = e;
      }
      // 250 ms doubling, or the server's wait; 30% jitter on top, so a
      // server back up is not met by every client at once.
      final backoff = Duration(milliseconds: 250 << min(n, 30));
      final base = wait > backoff ? wait : backoff;
      if (r == null || n + 1 >= r.attempts || waited + base > r.maxWait) {
        if (failure != null) throw FennecException(FennecCode.io, 'the server could not be reached: $failure');
        return got!;
      }
      final jittered = Duration(microseconds: (base.inMicroseconds * (1 + _random.nextDouble() * 0.3)).round());
      final left = r.maxWait - waited;
      final w = jittered < left ? jittered : left;
      waited += w;
      await _pause(w, abort);
      if (abort != null && abort.fired) throw FennecException(FennecCode.io, 'the request was aborted');
    }
  }

  /// [wait] waited, or less where [abort] fires first.
  static Future<void> _pause(Duration wait, _Abort? abort) async {
    final done = Completer<void>();
    final timer = Timer(wait, () {
      if (!done.isCompleted) done.complete();
    });
    abort?.waits.add(done);
    if (abort != null && abort.fired) done.complete();
    try {
      await done.future;
    } finally {
      timer.cancel();
      abort?.waits.remove(done);
    }
  }

  /// The request sent -- marked as a read near after [after] where [near] --
  /// and its answer's status, body and head: the connect within the timeout
  /// (`connectionTimeout`), the head within it, a held write's wait on top,
  /// and each piece of the body within it of the last -- so a long answer is
  /// read for as long as it comes, and one that stops is given up.
  Future<(int, String, HttpHeaders)> _once(
      String path, String type, List<int> bytes, String? key, bool near, _Position? after) async {
    final req = await _shared.client.postUrl(Uri.parse('$url$path'));
    final abort = _abort;
    if (abort != null) {
      if (abort.fired) req.abort(const HttpException('aborted'));
      abort.requests.add(req);
    }
    try {
      req.headers.set('content-type', type);
      final t = _shared.token;
      if (t != null) req.headers.set('authorization', 'Bearer $t');
      final k = key ?? _idempotencyKey;
      if (k != null) req.headers.set('idempotency-key', k);
      final w = _wait;
      if (w != null && w > Duration.zero) req.headers.set('fennec-wait', '${w.inMilliseconds}');
      if (near) {
        req.headers.set('fennec-read', 'replica');
        if (after != null) {
          req.headers.set('fennec-after', '${after.seq}');
          final h = after.history;
          if (h != null) req.headers.set('fennec-history', h);
        }
      }
      // Sent with its length, which every hop takes: the global entry
      // refuses a body past a mebibyte without one.
      req.contentLength = bytes.length;
      req.add(bytes);
      final head = _shared.timeout + (w ?? Duration.zero);
      final res = await req.close().timeout(head, onTimeout: () {
        // The connection closed: past its timeout a request is given up.
        req.abort(TimeoutException('no answer within $head', head));
        throw TimeoutException('no answer within $head', head);
      });
      final body = await res.timeout(_shared.timeout).transform(utf8.decoder).join();
      return (res.statusCode, body, res.headers);
    } finally {
      abort?.requests.remove(req);
    }
  }

  /// A connect past `HttpClient.connectionTimeout`, which it says as a
  /// [SocketException] of no OS error: the connection's own timeout.
  static bool _timedOut(Object e) =>
      e is SocketException && e.osError == null && e.message.startsWith('HTTP connection timed out');

  /// A connection refused: it sent nothing.
  static bool _refused(Object e) => e is SocketException && e.osError?.errorCode == _refusedCode;

  /// How long an answer asks a client to wait, where it names any: to the
  /// millisecond where the body says it (`retry_after_ms`) -- `Retry-After`
  /// is whole seconds, and a bucket of 600 rows a second refills a row in
  /// 1.7 ms -- then the header, then the body's `retry_after`, which a 503 of
  /// the server's carries.
  static Duration? _waitOf(Map? m, String? header) {
    final ms = m?['retry_after_ms'];
    if (ms is num && ms > 0 && ms.isFinite) return Duration(microseconds: (ms * 1000).round());
    for (final said in [double.tryParse(header?.trim() ?? ''), m?['retry_after']]) {
      if (said is num && said > 0 && said.isFinite) return Duration(microseconds: (said * 1000000).round());
    }
    return null;
  }

  static Object? _decoded(String body) {
    try {
      return jsonDecode(body);
    } catch (_) {
      return null;
    }
  }

  /// The rows a statement answers.
  Future<List<Map<String, Object?>>> query(String text, [List<Object?> params = const []]) async =>
      (await run(text, params)).rows;

  /// A write: how many documents it wrote.
  Future<int> execute(String text, [List<Object?> params = const []]) async => (await run(text, params)).affected;

  /// The query builder, its queries run on the server -- its reads as an
  /// [asRead] copy's, near on a connection reading near, and sent again after
  /// a reset as reads the builder made ([RetryPolicy]).
  Query from(String collection) =>
      Query.from(collection).bind(run, read: FennecRemote._copy(this, _idempotencyKey, _wait, true, built: true).run);

  /// Calls the named statement [name] on the server: `call <name> $1` with
  /// [args], a map of values by name, as `$1` -- [Call] makes the text, so
  /// only the name is ever written into it -- over `POST /query`, so the
  /// transport, the errors, the key and the wait are [run]'s. It answers as
  /// [run] does: the rows of a `get` or a `returning`, else how many rows it
  /// wrote. [claims] are the call's `as`, which the server's own token alone
  /// may say (a scoped token's are its token's, and it is refused if it
  /// names them); [idempotencyKey] makes the call once ([withIdempotencyKey]'s,
  /// a call that only reads keeps none); [wait] is [withWait]'s.
  ///
  /// [read] marks a call known to only read, as [asRead] marks a text: on a
  /// connection reading near it goes to the read replica near the app, after
  /// the connection's writes; without it a call goes home, as does every text
  /// not known to read. A call that writes sent with [read] is refused
  /// (status 403), never landed.
  ///
  /// Refusals are a [FennecException] by status: 404 no such statement or
  /// none this token may call, 403 a claim it needs missing, 400 an argument
  /// missing, extra or of another type, 409 a statement broken or a unique
  /// value taken, 412 a `require` not met with its [FennecException.reason].
  /// Named `callStatement` because a method named `call` would make the
  /// connection callable as a function.
  ///
  /// ```dart
  /// final moved = await db.callStatement('transfer',
  ///     args: {'from': 1, 'to': 2, 'amount': 300}, idempotencyKey: 'transfer-42');
  /// final mine = await db.callStatement('history', claims: {'sub': 'alice'}, read: true);
  /// ```
  Future<Answer> callStatement(String name,
      {Object? args, Object? claims, String? idempotencyKey, Duration? wait, bool read = false}) async {
    final s = Call(name, args: args, claims: claims).toCall();
    var c = this;
    if (idempotencyKey != null) c = c.withIdempotencyKey(idempotencyKey);
    if (wait != null) c = c.withWait(wait);
    if (read) c = c.asRead();
    return c.run(s.text, s.params);
  }

  /// The named statements the server holds, as it lists them to its own
  /// token (`statements`) -- a scoped one is refused (status 403): each one's
  /// signature, whether it writes, its comment, why the schema broke it, what
  /// a binding is bound to, a scheduled one's clock, and a webhook consumer's
  /// hook and whether the server holds its secret. [callStatement] sends a
  /// webhook consumer a test event, the event its `args`, run as a delivery
  /// is and keeping no key.
  Future<List<NamedStatement>> statements() async => (await query('statements')).map(namedStatement).toList();

  /// Lets the connections go: this one's, and every copy's.
  void close() => _shared.client.close();

  /// A refusal as the error it is: its status and message, where a batch
  /// stopped, what a `require n else '<reason>'` that refused it said and
  /// the statement bound to the write that refused it, and
  /// where a tenant's quota refused it the quota and how long to wait -- to
  /// the millisecond where the body says it (`retry_after_ms`), else
  /// `Retry-After`, or the same in the body.
  static FennecException refused(int status, Map? m, String? retryAfter) {
    final quota = m?['quota'] as String?;
    final wait = _waitOf(m, retryAfter);
    return FennecException.http(
        quota == null ? _code(status) : FennecCode.quota, m?['error'] as String? ?? 'HTTP $status',
        status: status,
        at: m?['at'] as int?,
        completed: m?['completed'] as int?,
        quota: quota,
        retryAfter: wait,
        reason: m?['reason'] as String?,
        binding: m?['binding'] as String?);
  }

  /// An HTTP status as the error kind the engine would have said; the
  /// status itself is the error's [FennecException.status].
  static FennecCode _code(int status) => switch (status) {
        400 => FennecCode.query,
        401 || 403 => FennecCode.denied,
        404 => FennecCode.notFound,
        409 => FennecCode.duplicate,
        412 => FennecCode.unmet,
        _ => FennecCode.io,
      };

  /// The endpoint's answer as the library's: rows come as an array, or --
  /// when the query asked for facets, which belong to no row, and as a
  /// batch's each read -- as an object holding them beside the rows.
  static Answer _answer(Object? v) {
    if (v is List || (v is Map && v['rows'] is List)) {
      final rows = ((v is List ? v : (v as Map)['rows']) as List).cast<Map<String, Object?>>();
      return Answer.of({
        'kind': 'rows',
        'result': {
          'columns': rows.isEmpty ? <String>[] : rows.first.keys.toList(),
          'rows': rows,
          if (v is Map) 'facets': v['facets'],
        },
      });
    }
    if (v is Map<String, Object?>) {
      if (v['affected'] is int) return Answer.of({'kind': 'affected', 'count': v['affected']});
      if (v['collections'] is List) return Answer.of({'kind': 'schemas', 'collections': v['collections']});
      return Answer.of({'kind': 'ok', 'message': v['message'] as String? ?? jsonEncode(v)});
    }
    return Answer.of({'kind': 'ok', 'message': jsonEncode(v)});
  }
}
