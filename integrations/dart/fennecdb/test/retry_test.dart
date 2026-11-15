// What a connection sends again, and when it gives a request up: only what
// cannot have run -- an answer naming its wait, a connection refused -- or
// may run twice -- a read the builder made or sent near, a write under a key
// -- never after the connection's own timeout, nor once the caller aborts
// (docs/design/js-client.md#retries). Against a scripted server on loopback
// ([Stub]), no fennec-server needed.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:fennecdb/fennecdb.dart';
import 'package:test/test.dart';

/// One act of a [Stub]'s script.
sealed class Act {}

/// An answer over the kept connection.
class Answer extends Act {
  final int status;
  final String body;
  final Map<String, String> headers;
  Answer(this.status, this.body, [this.headers = const {}]);
}

/// The connection closed with a RST once the request is read whole.
class Reset extends Act {}

/// No answer until the client lets the connection go.
class Hold extends Act {}

/// A request read: when, on the stub's clock, and its head, names in lower
/// case.
class Seen {
  final Duration at;
  final Map<String, String> head;
  Seen(this.at, this.head);
}

/// A server on loopback answering from a script, one [Act] a request it
/// reads; past the script's end it answers `[]`. Each request read is noted
/// with the time it came and its head ([seen]), and handed to [onRequest].
class Stub {
  final ServerSocket _server;
  final List<Act> _plan;
  final seen = <Seen>[];
  final _clock = Stopwatch()..start();
  final _conns = <Socket>{};
  void Function(Seen)? onRequest;

  Stub._(this._server, this._plan) {
    _server.listen(_serve);
  }

  static Future<Stub> start(List<Act> plan) async =>
      Stub._(await ServerSocket.bind(InternetAddress.loopbackIPv4, 0), [...plan]);

  String get url => 'http://127.0.0.1:${_server.port}';

  /// The `Idempotency-Key` each request carried, null for none.
  List<String?> get keys => [for (final s in seen) s.head['idempotency-key']];

  void _serve(Socket c) {
    _conns.add(c);
    // An answer written to a connection the client gave up -- aborted,
    // timed out -- fails here, as it would for any server.
    c.done.then((_) {}, onError: (Object _) {});
    var buf = <int>[];
    var holding = false;
    c.listen((data) {
      if (holding) return;
      buf.addAll(data);
      while (true) {
        final end = _end(buf);
        if (end < 0) return;
        final lines = latin1.decode(buf.sublist(0, end)).split('\r\n');
        final head = <String, String>{'': lines.first};
        for (final line in lines.skip(1)) {
          final colon = line.indexOf(':');
          if (colon > 0) head[line.substring(0, colon).trim().toLowerCase()] = line.substring(colon + 1).trim();
        }
        final length = int.parse(head['content-length'] ?? '0');
        if (buf.length < end + 4 + length) return;
        buf = buf.sublist(end + 4 + length);
        final s = Seen(_clock.elapsed, head);
        seen.add(s);
        onRequest?.call(s);
        final act = _plan.isEmpty ? Answer(200, '[]') : _plan.removeAt(0);
        switch (act) {
          case Answer(:final status, :final body, :final headers):
            final bytes = utf8.encode(body);
            final text = StringBuffer('HTTP/1.1 $status X\r\ncontent-type: application/json\r\n')
              ..write('content-length: ${bytes.length}\r\n');
            headers.forEach((k, v) => text.write('$k: $v\r\n'));
            c.add([...utf8.encode('$text\r\n'), ...bytes]);
          case Reset():
            // SO_LINGER of 0: closed with a RST, not a FIN.
            final linger = Uint8List(8)..buffer.asByteData().setInt32(0, 1, Endian.host);
            c.setRawOption(RawSocketOption(
                RawSocketOption.levelSocket, Platform.isLinux || Platform.isAndroid ? 13 : 0x80, linger));
            c.destroy();
            return;
          case Hold():
            holding = true;
            return;
        }
      }
    }, onDone: () {
      _conns.remove(c);
      c.destroy();
    }, onError: (Object _) => _conns.remove(c));
  }

  static int _end(List<int> buf) {
    for (var i = 0; i + 3 < buf.length; i++) {
      if (buf[i] == 13 && buf[i + 1] == 10 && buf[i + 2] == 13 && buf[i + 3] == 10) return i;
    }
    return -1;
  }

  Future<void> close() async {
    await _server.close();
    for (final c in [..._conns]) {
      c.destroy();
    }
  }
}

/// The connects `HttpClient` makes, the first sent to [port] and the rest
/// where they were going, counted; each by `dart:io`'s own.
final class _FirstTo extends IOOverrides {
  final int port;
  var connects = 0;
  _FirstTo(this.port);

  @override
  Future<ConnectionTask<Socket>> socketStartConnect(host, int port, {sourceAddress, int sourcePort = 0}) =>
      super.socketStartConnect(host, ++connects == 1 ? this.port : port,
          sourceAddress: sourceAddress, sourcePort: sourcePort);
}

/// What [body] threw, null where it threw nothing.
Future<Object?> failure(Future<Object?> Function() body) async {
  try {
    await body();
    return null;
  } catch (e) {
    return e;
  }
}

const wrote = '{"affected":1}';

void main() {
  // A 503 naming its wait ran nothing: any request goes again, a write
  // without a key too, after that wait and never sooner -- `retry_after_ms`
  // before `Retry-After`.
  test('a 503 naming its wait is sent again no sooner than the wait', () async {
    final s = await Stub.start([
      Answer(503, '{"error":"the tenant is being moved; retry shortly","retry_after":1,"retry_after_ms":300}',
          {'retry-after': '1'}),
      Answer(200, wrote),
    ]);
    addTearDown(s.close);
    expect(await Fennec.connect(s.url).execute('put notes {n: 1}'), 1);
    expect(s.seen, hasLength(2));
    expect(s.seen[1].at - s.seen[0].at, greaterThanOrEqualTo(const Duration(milliseconds: 300)));
  });

  // A 429 that names its wait only in `Retry-After` is sent again after it.
  test('a 429 naming its wait only in the header is sent again after it', () async {
    final s = await Stub.start([
      Answer(429, '{"error":"slow down"}', {'retry-after': '1'}),
      Answer(200, wrote),
    ]);
    addTearDown(s.close);
    expect(await Fennec.connect(s.url).execute('put notes {n: 1}'), 1);
    expect(s.seen, hasLength(2));
    expect(s.seen[1].at - s.seen[0].at, greaterThanOrEqualTo(const Duration(seconds: 1)));
  });

  // A connection refused sent nothing: any request goes again, a write
  // without a key too. The first connect is sent to a port that refuses --
  // the local end of a live connection of the test's own, so no other can
  // take it -- the next to the stub.
  test('a connection refused is tried again whatever the request', () async {
    final s = await Stub.start([Answer(200, wrote)]);
    addTearDown(s.close);
    final l = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final h = await Socket.connect(InternetAddress.loopbackIPv4, l.port);
    addTearDown(() async {
      h.destroy();
      await l.close();
    });
    final db = Fennec.connect(s.url);
    final first = _FirstTo(h.port);
    expect(await IOOverrides.runWithIOOverrides(() => db.execute('put notes {n: 1}'), first), 1);
    expect(first.connects, 2);
    expect(s.seen, hasLength(1));
  });

  // A write without a key may have landed before a reset: it is never sent
  // again, over the connection the client kept or a new one, and the client
  // makes no key. Under a key it goes again, answered from the key.
  test('a write is sent again after a reset only under a key', () async {
    final s = await Stub.start([Answer(200, wrote), Reset(), Answer(200, wrote)]);
    addTearDown(s.close);
    final db = Fennec.connect(s.url);
    expect(await db.execute('put notes {n: 0}'), 1);
    final e = await failure(() => db.execute('put notes {n: 1}'));
    expect(e, isA<FennecException>().having((e) => e.code, 'code', FennecCode.io));
    expect(s.seen, hasLength(2), reason: 'a write that may have landed is never made twice');
    expect(s.keys, [null, null], reason: 'the client makes no key of its own');

    final k = await Stub.start([
      Reset(),
      Answer(200, wrote, {'idempotent-replayed': 'true'})
    ]);
    addTearDown(k.close);
    expect(await Fennec.connect(k.url).withIdempotencyKey('n-1').from('notes').insert({'n': 1}), 1);
    expect(k.keys, ['n-1', 'n-1']);
  });

  // A read the builder made goes again after a reset; a text the caller says
  // reads (`asRead`), sent home, is a claim nothing checks, and goes once.
  // Sent near it is marked (`Fennec-Read: replica`), which every server runs
  // read-only: again.
  test('a builder read is sent again after a reset, a text sent home is not', () async {
    final s = await Stub.start([Reset(), Answer(200, '[{"id":1}]'), Reset(), Answer(200, '[{"id":1}]')]);
    addTearDown(s.close);
    final db = Fennec.connect(s.url);
    expect(await db.from('docs').rows(), [
      {'id': 1}
    ]);
    expect(s.seen, hasLength(2));
    expect(await failure(() => db.asRead().query('get docs')), isA<FennecException>());
    expect(s.seen, hasLength(3));

    final near = await Stub.start([Reset(), Answer(200, '[{"id":2}]')]);
    addTearDown(near.close);
    expect(await Fennec.connect(near.url, reads: Reads.nearest).asRead().query('get docs'), [
      {'id': 2}
    ]);
    expect([for (final r in near.seen) r.head['fennec-read']], ['replica', 'replica']);
  });

  // A batch stopped after some of its statements landed (`completed` above
  // 0) is never sent again, whatever wait it names: sent again whole, they
  // were made twice.
  test('a stop after statements landed is never sent again', () async {
    final s = await Stub.start([
      Answer(503, '{"error":"moved","completed":1,"at":1,"retry_after_ms":50}', {'retry-after': '1'}),
      Answer(200, '{"results":[]}'),
    ]);
    addTearDown(s.close);
    final e = await failure(
        () => Fennec.connect(s.url).batch([(text: 'put notes {n: 1}', params: []), (text: 'compact', params: [])]));
    expect(e, isA<FennecException>().having((e) => e.status, 'status', 503).having((e) => e.completed, 'completed', 1));
    expect(s.seen, hasLength(1));
  });

  // The timeout bounds the wait for the head; past it nothing goes again, a
  // read the builder made neither: the server may still be running it.
  test('nothing goes again after the connection\'s own timeout', () async {
    final s = await Stub.start([Hold(), Answer(200, '[]')]);
    addTearDown(s.close);
    final db = Fennec.connect(s.url, timeout: const Duration(milliseconds: 500));
    final took = Stopwatch()..start();
    final e = await failure(() => db.from('docs').rows());
    expect(e, isA<FennecException>().having((e) => e.message, 'message', contains('timeout')));
    expect(took.elapsed, greaterThanOrEqualTo(const Duration(milliseconds: 500)));
    expect(s.seen, hasLength(1));
  });

  // Aborted, a request is given up -- under way or waiting to go again --
  // and nothing goes again.
  test('an abort gives a request up and nothing goes again', () async {
    final s = await Stub.start([
      Answer(503, '{"error":"busy","retry_after_ms":5000}'),
      Answer(200, '[]'),
    ]);
    addTearDown(s.close);
    final gone = Completer<void>();
    s.onRequest = (_) => gone.complete();
    final e = await failure(() => Fennec.connect(s.url).withAbort(gone.future).from('docs').rows());
    expect(e, isA<FennecException>().having((e) => e.message, 'message', contains('aborted')));
    expect(s.seen, hasLength(1));
  });

  // Three tries unless told, the last answer the caller's; `retry: null`
  // sends nothing again.
  test('the tries are bounded and retry null sends nothing again', () async {
    Answer busy() => Answer(503, '{"error":"too many connections","retry_after":1,"retry_after_ms":5}');
    final s = await Stub.start([busy(), busy(), busy(), Answer(200, '[]')]);
    addTearDown(s.close);
    final e = await failure(() => Fennec.connect(s.url).from('docs').rows());
    expect(
        e,
        isA<FennecException>()
            .having((e) => e.status, 'status', 503)
            .having((e) => e.retryAfter, 'retryAfter', const Duration(milliseconds: 5)));
    expect(s.seen, hasLength(3));

    final four = await Stub.start([busy(), busy(), busy(), Answer(200, '[{"id":1}]')]);
    addTearDown(four.close);
    expect(await Fennec.connect(four.url, retry: const RetryPolicy(attempts: 4)).from('docs').rows(), [
      {'id': 1}
    ]);
    expect(four.seen, hasLength(4));

    final off = await Stub.start([busy(), Answer(200, '[]')]);
    addTearDown(off.close);
    expect(await failure(() => Fennec.connect(off.url, retry: null).from('docs').rows()),
        isA<FennecException>().having((e) => e.status, 'status', 503));
    expect(off.seen, hasLength(1));
    expect(() => Fennec.connect(off.url, retry: const RetryPolicy(attempts: 0)), throwsA(isA<FennecException>()));
  });

  // The code the server named is the error's `errorCode` -- never `code`,
  // the library's kind -- and a refusal naming none has none; neither is
  // sent again.
  test('a refusal names the server\'s code', () async {
    final s = await Stub.start([
      Answer(401, '{"error":"the token has expired","code":"token_expired"}'),
      Answer(401, '{"error":"invalid or missing token"}'),
    ]);
    addTearDown(s.close);
    final db = Fennec.connect(s.url, token: 't');
    expect(
        await failure(() => db.from('docs').rows()),
        isA<FennecException>()
            .having((e) => e.status, 'status', 401)
            .having((e) => e.code, 'code', FennecCode.denied)
            .having((e) => e.errorCode, 'errorCode', 'token_expired'));
    expect(await failure(() => db.from('docs').rows()),
        isA<FennecException>().having((e) => e.status, 'status', 401).having((e) => e.errorCode, 'errorCode', isNull));
    expect(s.seen, hasLength(2));
  });
}
