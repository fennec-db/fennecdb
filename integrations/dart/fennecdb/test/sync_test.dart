// A replica against a real fennec-server the tests start, kill and start
// again on its port: the binary `cargo build -p fennec-server` makes
// (FENNEC_SERVER names another), as `make dart-test` builds it.
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:fennecdb/fennecdb.dart';
import 'package:test/test.dart';

class Server {
  final String file;

  /// A tenant node's quota of rows a tenant, its tasks in tenant `acme`;
  /// null for a server over a file.
  final int? tenantRows;

  /// The token a server over a file asks for (`--http-token`), null for
  /// none.
  final String? token;
  Process? _process;
  int port = 0;

  Server._(this.file, this.tenantRows, this.token);

  String get url => 'http://127.0.0.1:$port${tenantRows == null ? '' : '/t/acme'}';

  /// A server over a file of its own, holding `tasks` -- or, given
  /// [tenantRows], a tenant node (`--dir`) holding each tenant to that many
  /// rows, `tasks` in its tenant `acme`, which [url] names.
  static Future<Server> start(String name, {int? tenantRows, String? token}) async {
    final dir = await Directory.systemTemp.createTemp('fennecdb-dart-$name');
    final s = Server._('${dir.path}/server.fennec', tenantRows, token);
    await s._start(0);
    if (tenantRows != null) {
      final http = HttpClient();
      try {
        final req = await http.putUrl(Uri.parse('http://127.0.0.1:${s.port}/_admin/tenants/acme'));
        req.headers.set('authorization', 'Bearer adm');
        final res = await req.close();
        await res.drain<void>();
        if (res.statusCode != 201) throw StateError('the tenant was not made: ${res.statusCode}');
      } finally {
        http.close();
      }
    }
    await s.run('create collection tasks (key text @unique, title text, status text @hash, priority int)');
    await s.run('put tasks [{key: "a", title: "one", status: "open", priority: 1}, '
        '{key: "b", title: "two", status: "open", priority: 5}, '
        '{key: "c", title: "three", status: "closed", priority: 3}]');
    return s;
  }

  Future<void> _start(int at) async {
    final binary =
        Platform.environment['FENNEC_SERVER'] ?? '${Directory.current.path}/../../../target/debug/fennec-server';
    final rows = tenantRows;
    final p = await Process.start(binary, [
      '--http',
      '127.0.0.1:$at',
      '--sync',
      'always',
      ...rows == null ? ['--file', file] : ['--dir', file, '--admin-token', 'adm', '--tenant-max-rows', '$rows'],
      ...token == null ? <String>[] : ['--http-token', token!],
    ]);
    _process = p;
    p.stdout.drain<void>();
    final found = Completer<int>();
    final seen = StringBuffer();
    p.stderr.transform(utf8.decoder).transform(const LineSplitter()).listen((line) {
      seen.writeln(line);
      final m = RegExp(r'listening on: http://127\.0\.0\.1:(\d+) ').firstMatch(line);
      if (m != null && !found.isCompleted) found.complete(int.parse(m.group(1)!));
    }, onDone: () {
      if (!found.isCompleted) found.completeError(StateError('fennec-server did not start: $seen'));
    });
    port = await found.future.timeout(const Duration(seconds: 20));
  }

  /// Killed, as a crash would: no checkpoint, no goodbye.
  Future<void> kill() async {
    final p = _process;
    if (p == null) return;
    p.kill(ProcessSignal.sigkill);
    await p.exitCode;
    _process = null;
  }

  /// Started again over its file, on the port it had.
  Future<void> restart() => _start(port);

  Future<Answer> run(String text) async {
    final r = Fennec.connect(url, token: token);
    try {
      return await r.run(text);
    } finally {
      r.close();
    }
  }
}

/// Waits until [done] holds, or fails after ten seconds.
Future<void> eventually(String what, FutureOr<bool> Function() done) async {
  for (var i = 0; i < 1000; i++) {
    if (await done()) return;
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
  fail('timed out waiting for $what');
}

Future<String> scratch(String name) async =>
    '${(await Directory.systemTemp.createTemp('fennecdb-dart-$name')).path}/app.fennec';

const open = Shape('tasks', where: {'status': 'open'}, key: 'key');

/// The replica offline before the next write: `setOnline` puts the signal
/// on the sync's chain, and an event already on it -- a stream's bytes, a
/// status -- runs the engine first, which sends what a write made meanwhile
/// queued, and the server's answer can put the write back before the read
/// that was to find it: Kotlin's test met it 8 runs in 593 under load. Not
/// met here in 3 041 runs, four at once beside four busy loops -- the
/// engine is told on the one worker isolate in turn with the writes and
/// reads, so the server would have to answer before the worker answers the
/// next write -- but nothing promises that order: `refresh` completes once
/// the chain has reached the signal.
Future<void> offline(Fennec db) async {
  db.replica!.setOnline(false);
  expect((await db.replica!.refresh()).state, SyncState.offline);
}

Future<List<Object?>> titles(Fennec db) async => [
      for (final r in await db.from('tasks').select(['title']).order('title').rows()) r['title']
    ];

void main() {
  final servers = <Server>[];
  Future<Server> server(String name) async => (await Server.start(name))..let(servers.add);
  tearDown(() async {
    for (final s in servers) {
      await s.kill();
    }
    servers.clear();
  });

  // A 401 hands the provider why -- no token sent (`hadToken` false) -- and
  // the status says the same while the sync waits. A provider that failed,
  // its issuer out of reach, is named by the status's error and asked again
  // at `resume()`; the token it hands over then refused too, it is asked
  // once more after a wait, as `attempt` 2.
  test('a token provider is handed why a token is wanted', () async {
    final s = await Server.start('token', token: 'right');
    servers.add(s);
    final asked = <TokenRefusal>[];
    final db = await Fennec.openSynced(
        url: s.url,
        shapes: [open],
        path: await scratch('sync-token'),
        tokenProvider: (refusal) async {
          asked.add(refusal);
          return switch (asked.length) {
            1 => throw const SocketException('the issuer is out of reach'),
            2 => 'wrong',
            _ => 'right',
          };
        });
    const first = TokenRefusal(hadToken: false);
    await eventually('the first ask', () => asked.length == 1);
    await eventually(
        "the status naming the provider's failure",
        () async =>
            (await db.replica!.refresh()).error?.message ==
            'the token provider failed: SocketException: the issuer is out of reach');
    expect(db.replica!.status.error?.tokenRefusal, first);
    db.replica!.resume();
    await db.replica!.ready();
    expect(asked, [first, first, const TokenRefusal(hadToken: true, attempt: 2)]);
    expect((await db.replica!.refresh()).error?.tokenRefusal, isNull);
    expect(asked.where((r) => r.expired || r.errorCode != null), isEmpty);
    expect(const TokenRefusal(errorCode: 'token_expired').expired, isTrue);
    expect(await titles(db), ['one', 'two']);
    await db.close();
  });

  // A provider at work is called once: `resume()` while it is asks nothing,
  // nor does the ask after the app's own token, refused, waited for -- the
  // provider's answer answers it, and the replica goes on.
  test('a token provider at work is called once', () async {
    final s = await Server.start('token-once', token: 'right');
    servers.add(s);
    final asked = <TokenRefusal>[];
    final gate = Completer<void>();
    final db = await Fennec.openSynced(
        url: s.url,
        shapes: [open],
        path: await scratch('sync-token-once'),
        tokenProvider: (refusal) async {
          asked.add(refusal);
          await gate.future;
          return 'right';
        });
    await eventually('the first ask', () => asked.length == 1);
    db.replica!.resume();
    db.replica!.setToken('wrong');
    await eventually(
        'the ask after the wait', () async => (await db.replica!.refresh()).error?.tokenRefusal?.attempt == 2);
    db.replica!.resume();
    gate.complete();
    await db.replica!.ready();
    expect(asked, [const TokenRefusal(hadToken: false)]);
    expect(await titles(db), ['one', 'two']);
    await db.close();
  });

  test('the seed fills the replica with the shape', () async {
    final s = await server('seed');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('seed'));
    await db.replica!.ready();
    expect(await titles(db), ['one', 'two']);
    expect((await db.replica!.refresh()).state, SyncState.online);
    // Reads are the file's: the server gone, they go on.
    await s.kill();
    expect(await db.from('tasks').count(), 2);
    await db.close();
  });

  test('a live query runs again on a server write', () async {
    final s = await server('live');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('live'));
    await db.replica!.ready();
    final seen = <List<Object?>>[];
    final sub = db
        .live(db.from('tasks').select(['title']).order('title'))
        .listen((rows) => seen.add([for (final r in rows) r['title']]));
    await eventually('the first rows', () => seen.isNotEmpty);
    await s.run('put tasks {key: "d", title: "four", status: "open"}');
    await eventually("the server's write", () => seen.isNotEmpty && seen.last.join(',') == 'four,one,two');
    await sub.cancel();
    await db.close();
  });

  test('an optimistic write shows at once and lands', () async {
    final s = await server('optimistic');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('optimistic'));
    await db.replica!.ready();
    // Offline, nothing is sent: the server's answer cannot replace the row
    // before it is read (see 'a refused write is put back').
    await offline(db);
    await db.from('tasks').insert({'title': 'new', 'status': 'open', 'priority': 2});
    // At once, under a temporary id.
    expect((await db.from('tasks').where('title', 'new').first())!['id'] as int, greaterThanOrEqualTo(1 << 52));
    db.replica!.setOnline(true);
    await db.replica!.pushed();
    await eventually("the server's copy in place of the temporary row", () async {
      final rows = await db.from('tasks').where('title', 'new').rows();
      return rows.length == 1 && (rows[0]['id'] as int) < 1 << 52;
    });
    expect((await s.run('get tasks where title = "new"')).rows, hasLength(1));
    // An update and a delete go the same way.
    await db.from('tasks').where('key', 'a').update({'title': 'ONE'});
    await db.from('tasks').where('key', 'b').delete();
    await db.replica!.pushed();
    expect((await s.run('get tasks where title = "ONE"')).rows, hasLength(1));
    expect((await s.run('get tasks where key = "b"')).rows, isEmpty);
    await db.close();
  });

  // `insertNew` is FennecQL's `insert`, which the replica's state machine
  // applies as an insert (integrations/sync-scenarios.json): an id the
  // replica holds is refused here, nothing sent, and one only the server
  // holds is its 409, the row put back.
  test('an insertNew of a held id is refused, and put back where the server holds it', () async {
    final s = await server('insertnew');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('insertnew'));
    await db.replica!.ready();
    final refused = <Refusal>[];
    final sub = db.replica!.refusals.listen(refused.add);
    await offline(db);
    // Row 1 is in the replica: refused at once, where `insert` would write over it.
    await expectLater(db.from('tasks').insertNew({'id': 1, 'key': 'x', 'title': 'again', 'status': 'open'}),
        throwsA(isA<FennecException>()));
    expect(await titles(db), ['one', 'two']);
    expect((await db.replica!.refresh()).pending, 0);
    // Row 3 is the server's alone (closed, outside the shape): taken here, refused there.
    expect(await db.from('tasks').insertNew({'id': 3, 'key': 'y', 'title': 'taken', 'status': 'open'}), 1);
    expect(await titles(db), ['one', 'taken', 'two']);
    db.replica!.setOnline(true);
    await eventually('the refusal', () => refused.isNotEmpty);
    expect(refused.first.status, 409);
    await eventually('the write put back', () async => (await titles(db)).join(',') == 'one,two');
    await sub.cancel();
    await db.close();
  });

  test('a refused write is put back', () async {
    final s = await server('refused');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('refused'));
    await db.replica!.ready();
    final refused = <Refusal>[];
    final sub = db.replica!.refusals.listen(refused.add);
    // Offline, nothing is sent, so the row is read before the server can
    // refuse it: online, the 409 could come back and the row be put back
    // before the read, as it was in the Kotlin test on a loaded runner.
    await offline(db);
    // The server's key is @unique; the replica's a plain hash.
    await db.from('tasks').insert({'key': 'a', 'title': 'dup', 'status': 'open'});
    expect(await titles(db), ['dup', 'one', 'two']);
    db.replica!.setOnline(true);
    await eventually('the refusal', () => refused.isNotEmpty);
    expect(refused.first.status, 409);
    expect(refused.first.quota, isNull);
    await eventually('the write put back', () async => (await titles(db)).join(',') == 'one,two');
    expect((await db.replica!.refresh()).error?.status, 409);
    expect((await db.replica!.refresh()).pending, 0);
    await sub.cancel();
    await db.close();
  });

  // A require's reason goes with its write, and comes back with the refusal
  // that puts it back; one the replica finds unmet says it at once, nothing
  // sent.
  test("a require's reason comes back with its refusal", () async {
    final s = await server('reason');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('reason'));
    await db.replica!.ready();
    final refused = <Refusal>[];
    final sub = db.replica!.refusals.listen(refused.add);
    await expectLater(
        db.from('tasks').where('key', 'zzz').delete(require: 1, reason: 'no such task'),
        throwsA(isA<FennecException>()
            .having((e) => e.code, 'code', FennecCode.unmet)
            .having((e) => e.reason, 'reason', 'no such task')));
    // Offline, the server's row goes while the replica still holds it: the
    // write's count is met here and not there, whichever of the stream and
    // the write the server meets first.
    await offline(db);
    await s.run('del tasks where key = "a"');
    await db.from('tasks').where('key', 'a').update({'title': 'X'}, require: 1, reason: 'the task was taken');
    db.replica!.setOnline(true);
    await eventually('the refusal', () => refused.isNotEmpty);
    expect((refused.first.status, refused.first.reason), (412, 'the task was taken'));
    await eventually('the write put back', () async => (await titles(db)).join(',') == 'two');
    await sub.cancel();
    await db.close();
  });

  test("a write a tenant's quota refuses is put back, and the delete behind it goes", () async {
    // A tenant node holding each tenant to the three rows it has.
    final s = await Server.start('quota', tenantRows: 3);
    servers.add(s);
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('quota'));
    await db.replica!.ready();
    final refused = <Refusal>[];
    final sub = db.replica!.refusals.listen(refused.add);
    // Offline, so both are made before the server answers either.
    await offline(db);
    await db.from('tasks').insert({'key': 'd', 'title': 'four', 'status': 'open'});
    await db.from('tasks').where('key', 'b').delete();
    expect(await titles(db), ['four', 'one']);
    db.replica!.setOnline(true);
    await eventually('the refusal', () => refused.isNotEmpty);
    expect(refused.first.status, 507);
    expect(refused.first.quota, 'rows');
    // The insert put back, and the delete behind it landed.
    await eventually('the queue answered', () async => (await db.replica!.refresh()).pending == 0);
    expect(await titles(db), ['one']);
    expect((await db.replica!.refresh()).error?.quota, 'rows');
    expect((await s.run('get tasks where key = "b"')).rows, isEmpty);
    await sub.cancel();
    await db.close();
  });

  test('a server killed and started again is caught up with', () async {
    final s = await server('restart');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('restart'));
    await db.replica!.ready();
    await s.kill();
    await eventually('offline', () => db.replica!.status.state == SyncState.offline);
    // A write while it is down waits in the file.
    await db.from('tasks').insert({'title': 'while down', 'status': 'open'});
    expect((await db.replica!.refresh()).pending, 1);
    await s.restart();
    await s.run('put tasks {key: "e", title: "after", status: "open"}');
    db.replica!.resume();
    await eventually('caught up both ways', () async {
      return (await titles(db)).join(',') == 'after,one,two,while down' && (await db.replica!.refresh()).pending == 0;
    });
    expect((await s.run('get tasks where title = "while down"')).rows, hasLength(1));
    await db.close();
  });

  test('pending writes outlive a reopen', () async {
    final s = await server('reopen');
    final path = await scratch('reopen');
    var db = await Fennec.openSynced(url: s.url, shapes: [open], path: path);
    await db.replica!.ready();
    await s.kill();
    await db.from('tasks').insert({'title': 'kept', 'status': 'open'});
    await db.from('tasks').where('key', 'a').update({'title': 'ONE'});
    expect((await db.replica!.refresh()).pending, 2);
    await db.close();

    db = await Fennec.openSynced(url: s.url, shapes: [open], path: path);
    expect((await db.replica!.refresh()).pending, 2);
    expect(await titles(db), ['ONE', 'kept', 'two']);
    await s.restart();
    db.replica!.resume();
    await eventually('the queue sent', () async => (await db.replica!.refresh()).pending == 0);
    expect((await s.run('get tasks where title = "kept"')).rows, hasLength(1));
    expect((await s.run('get tasks where title = "ONE"')).rows, hasLength(1));
    await eventually(
        "the server's copies", () async => (await db.from('tasks').rows()).every((r) => (r['id'] as int) < 1 << 52));
    await db.close();
  });

  // The Dart tab's "over HTTP" lines on languages.html, as written there
  // but for the server's address, and the read the home page's Dart session
  // ends with.
  test("the docs' example over HTTP", () async {
    final s = await server('docs');
    await s.run('create collection if not exists docs (title text @text, year int @sorted)');
    await s.run('put docs {title: "Night at the oasis", year: 2024}');

    final remote = Fennec.connect(s.url, token: 'secret');
    await remote.execute(r'put docs {title: $1, year: $2}', ['Dunes', 2021]);
    final recent = await remote
        .from('docs')
        .select(['title', 'year'])
        .where('year', '>=', 2020)
        .order('year', 'desc')
        .limit(5)
        .rows();
    expect([for (final r in recent) r['title']], ['Night at the oasis', 'Dunes']);

    final rows =
        await remote.query(r'get docs select title, year where year >= $1 order year desc limit $2', [2020, 5]);
    expect(rows, recent);
    remote.close();
  });

  // The statements a server holds, typed: a scheduled one with its clause
  // and next slot -- New Year's, so no run of the clock lands while the test
  // looks -- and no run yet, and a called one with no clock; the server's
  // token runs the scheduled one now, marking no slot.
  test('the statements a server holds are listed, a scheduled one with its clock', () async {
    final s = await server('statements');
    final db = Fennec.connect(s.url);
    await db.execute('create collection ticks (n int, at timestamp)');
    await db.execute("create statement tick at '0 3 1 1 *' in 'Europe/Berlin' comment 'A tick a year' "
        'as insert ticks {n: 1, at: slot()}');
    await db.execute('create statement ticked() as get ticks count');
    // Berlin's 03:00 on 1 January is 02:00 UTC: this year's, or next.
    final now = DateTime.now().toUtc();
    final first = DateTime.utc(now.year, 1, 1, 2);
    final newYears = now.isBefore(first) ? first : DateTime.utc(now.year + 1, 1, 1, 2);
    final listed = {for (final x in await db.statements()) x.name: x};
    final tick = listed['tick']!;
    final ticked = listed['ticked']!;
    expect(
        (tick.schedule, tick.next, tick.last, tick.held), ("at '0 3 1 1 *' in 'Europe/Berlin'", newYears, null, null));
    expect((tick.comment, tick.writes, tick.signature, tick.after, tick.broken),
        ('A tick a year', true, '()', null, null));
    expect((ticked.signature, ticked.writes, ticked.schedule, ticked.next), ('()', false, null, null));
    expect((await db.callStatement('tick')).affected, 1);
    final again = (await db.statements()).singleWhere((x) => x.name == 'tick');
    expect((again.next, again.last), (newYears, null));
    db.close();
  });

  // A webhook consumer, listed by its hook -- not armed, this server given no
  // hooks file -- and a called statement by none; the server's token sends
  // one a test event with callStatement, the event its args, run as a
  // delivery is.
  test('a webhook consumer is listed by its hook and sent a test event', () async {
    final s = await server('hooked');
    final db = Fennec.connect(s.url);
    await db.execute(r'create statement paid($key text from event.data.object.metadata.key) on hook stripe '
        r"where event.type = 'payment_intent.succeeded' as set tasks {status: 'paid'} where key = $key require 1");
    await db.execute("create statement open_ones() as get tasks where status = 'open' count");
    final listed = {for (final x in await db.statements()) x.name: x};
    final paid = listed['paid']!;
    expect(
        (paid.hook, paid.armed, paid.signature), ('stripe', false, r'($key text from event.data.object.metadata.key)'));
    expect((listed['open_ones']!.hook, listed['open_ones']!.armed), (null, null));
    final event = {
      'type': 'payment_intent.succeeded',
      'data': {
        'object': {
          'metadata': {'key': 'a'}
        }
      }
    };
    expect((await db.callStatement('paid', args: event)).affected, 1);
    await expectLater(
        db.callStatement('paid', args: {'type': 'charge.refunded'}),
        throwsA(isA<FennecException>()
            .having((e) => e.status, 'status', 400)
            .having((e) => e.message, 'message', contains('passes this event over'))));
    db.close();
  });

  test('connect runs every query on the server', () async {
    final s = await server('connect');
    final db = Fennec.connect(s.url);
    final rows = await db.from('tasks').where('status', 'open').order('priority', 'desc').rows();
    expect([for (final r in rows) r['title']], ['two', 'one']);
    expect(await db.from('tasks').insert({'key': 'z', 'title': 'remote'}), 1);
    expect(await db.from('tasks').count(), 4);
    await expectLater(
      db.from('tasks').insert({'key': 'z', 'title': 'again'}),
      throwsA(isA<FennecException>().having((e) => e.code, 'code', FennecCode.duplicate)),
    );
    db.close();
  });

  test('a batch lands whole, says where it stopped, and lands once under a key', () async {
    final s = await server('batch');
    final db = Fennec.connect(s.url);
    final tasks = db.from('tasks');
    final out = await db.batch([
      tasks.toInsert({'key': 'd', 'title': 'four'}),
      tasks.where('key', 'a').toUpdate({'priority': 9}, require: 1),
      tasks.where('priority', '>=', 5).order('priority').select(['key']).toFennecQL(),
    ]);
    expect(out.results, hasLength(3));
    expect(out.results[0].affected, 1);
    expect([for (final r in out.results[2].rows) r['key']], ['b', 'a']);
    expect(out.seq, isNotNull);
    expect(out.seq, db.seq);
    expect(out.replayed, isFalse);

    await expectLater(
      db.batch([
        tasks.toInsert({'key': 'e', 'title': 'five'}),
        tasks.where('key', 'nobody').toDelete(require: 1, reason: 'nobody there'),
      ]),
      throwsA(isA<FennecException>()
          .having((e) => e.code, 'code', FennecCode.unmet)
          .having((e) => e.status, 'status', 412)
          .having((e) => e.at, 'at', 1)
          .having((e) => e.completed, 'completed', 0)
          // The statement's own words, which stay put where `at` moves.
          .having((e) => e.reason, 'reason', 'nobody there')),
    );
    expect(await tasks.count(), 4);

    final stmts = [
      tasks.toInsert({'key': 'f', 'title': 'six'})
    ];
    expect((await db.batch(stmts, idempotencyKey: 'batch-1')).replayed, isFalse);
    expect((await db.batch(stmts, idempotencyKey: 'batch-1')).replayed, isTrue);
    expect(await tasks.count(), 5);
    // A copy keys every write it runs, the builder's too.
    final keyed = db.withIdempotencyKey('put-1');
    expect(await keyed.from('tasks').insert({'key': 'g'}), 1);
    expect(await keyed.from('tasks').insert({'key': 'g'}), 1);
    expect(await tasks.count(), 6);
    await expectLater(
      keyed.execute('put tasks {key: "i"}'),
      throwsA(isA<FennecException>().having((e) => e.status, 'status', 422).having((e) => e.at, 'at', isNull)),
    );
    db.close();
  });

  // Named statements over `/query`: `callStatement` sends the builder's
  // text and parameters, so the answer, the key, the wait and the refusals
  // are what a text has.
  test('a call runs a named statement on the server, as a text would', () async {
    final s = await server('call');
    await s.run('create collection accounts (balance int)');
    await s.run('create collection notes (body text, by text @hash, stars int, pinned bool, score float)');
    await s.run('put accounts [{id: 1, balance: 100}, {id: 2, balance: 0}]');
    await s.run(r'create statement transfer($from int, $to int, $amount int) public as begin '
        r"set accounts {balance: balance - $amount} where id = $from and balance >= $amount require 1 else 'insufficient funds'; "
        r"set accounts {balance: balance + $amount} where id = $to require 1 else 'no such account'; "
        r'get accounts select id, balance where id = $from end');
    await s.run(r'create statement bump($id int) public as set accounts {balance: balance + 1} where id = $id');
    await s.run(
        r'create statement note($body text, $stars int, $pinned bool, $score float) public as insert notes {body: $body, stars: $stars, pinned: $pinned, score: $score} returning id, body, stars, pinned, score');
    await s.run("create statement stamp() public as insert notes {body: 'z'} returning id");
    await s.run(r'create statement mine() claims (sub text) as get notes select body where by = $jwt.sub');
    final db = Fennec.connect(s.url);
    Future<int?> balance(int id) async => (await db.from('accounts').where('id', id).first())?['balance'] as int?;

    // Arguments of several types, by name; the answer is the last statement's.
    final moved = await db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 30});
    expect(moved.rows.single, {'id': 1, 'balance': 70});
    final noted = await db.callStatement('note', args: {'body': 'ça va', 'stars': 4, 'pinned': true, 'score': 2.5});
    expect(
        noted.rows.single, {'id': noted.rows.single['id'], 'body': 'ça va', 'stars': 4, 'pinned': true, 'score': 2.5});
    // A write that answers none answers how many it wrote; a statement of none takes none.
    expect((await db.callStatement('bump', args: {'id': 2})).affected, 1);
    expect(await balance(2), 31);
    expect((await db.callStatement('stamp')).rows.single['id'], isNotNull);
    // A call is also a statement of a batch.
    final batch = await db.batch([
      Call('bump', args: {'id': 2}).toCall(),
      Call('transfer', args: {'from': 1, 'to': 2, 'amount': 10}).toCall(),
    ]);
    expect((batch.results[0].affected, batch.results[1].rows.single['balance']), (1, 60));

    // Refusals by their status: a `require` with its reason, the name, the arguments.
    Future<void> refusal(Future<Object?> call, int status, FennecCode code, {String? reason}) async {
      try {
        await call;
        fail('a call that was to be refused ($status) was answered');
      } on FennecException catch (e) {
        expect((e.status, e.code), (status, code), reason: e.message);
        expect(e.reason, reason, reason: e.message);
      }
    }

    await refusal(db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 5000}), 412, FennecCode.unmet,
        reason: 'insufficient funds');
    await refusal(db.callStatement('transfer', args: {'from': 1, 'to': 9, 'amount': 5}), 412, FennecCode.unmet,
        reason: 'no such account');
    await refusal(db.callStatement('nothing'), 404, FennecCode.notFound);
    await refusal(db.callStatement('transfer', args: {'from': 1, 'to': 2}), 400, FennecCode.query);
    await refusal(db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 'ten'}), 400, FennecCode.query);
    await refusal(
        db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 1, 'by': 3}), 400, FennecCode.query);
    // The refused calls moved nothing: 100 - 30 - 10.
    expect(await balance(1), 60);
    // The builder's refusal runs nothing, and sends nothing.
    await expectLater(db.callStatement('transfer; drop collection notes'), throwsA(isA<FennecException>()));

    // A key makes the call once; the key with another call is 422.
    final keyed = await db.callStatement('bump', args: {'id': 2}, idempotencyKey: 'bump-1');
    expect(keyed.affected, 1);
    final before = await balance(2);
    expect((await db.callStatement('bump', args: {'id': 2}, idempotencyKey: 'bump-1')).affected, 1);
    expect(await balance(2), before);
    await refusal(db.callStatement('bump', args: {'id': 1}, idempotencyKey: 'bump-1'), 422, FennecCode.io);
    // A copy's key is the same key.
    await db.withIdempotencyKey('bump-2').callStatement('bump', args: {'id': 2});
    await db.withIdempotencyKey('bump-2').callStatement('bump', args: {'id': 2});
    expect(await balance(2), before! + 1);

    // `claims` are the `as`, the server's own token's to name: a statement
    // reading one without it is refused (403), nothing run.
    await db.from('notes').insert([
      {'body': 'one', 'by': 'alice'},
      {'body': 'two', 'by': 'bob'}
    ]);
    expect([
      for (final r in (await db.callStatement('mine', claims: {'sub': 'alice'})).rows) r['body']
    ], [
      'one'
    ]);
    expect((await db.callStatement('mine', claims: {'sub': 'bob'}, read: true)).rows.single['body'], 'two');
    await refusal(db.callStatement('mine'), 403, FennecCode.denied);
    await refusal(db.callStatement('mine', claims: {'sub': null}), 403, FennecCode.denied);
    db.close();
  });

  // `read` marks the call as it marks a text: a server told a request only
  // reads (`Fennec-Read: replica`) answers one that does, and refuses one that
  // writes, which lands nothing.
  test('a call sent as a read is answered if it reads, refused if it writes', () async {
    final s = await server('callread');
    await s.run('create collection notes (body text)');
    await s.run(r'create statement history() public as get notes select body order id');
    await s.run(r'create statement note($body text) public as insert notes {body: $body} returning id');
    final db = Fennec.connect(s.url, reads: Reads.nearest);
    expect((await db.callStatement('note', args: {'body': 'a'})).rows, hasLength(1));
    expect((await db.callStatement('history', read: true)).rows.single['body'], 'a');
    try {
      await db.callStatement('note', args: {'body': 'b'}, read: true);
      fail('a write sent as a read was answered');
    } on FennecException catch (e) {
      expect(e.status, 403);
    }
    expect(await db.from('notes').count(), 1);
    // Unmarked, on the same connection, it writes.
    expect((await db.callStatement('note', args: {'body': 'b'})).rows, hasLength(1));
    expect(await db.from('notes').count(), 2);
    db.close();
  });

  // The wait a copy carries goes with a call as with a text. A call that
  // writes is not held, so it is answered at once.
  test('a call carries the wait a text would', () async {
    final s = await server('callwait');
    await s.run('create collection jobs (kind text, status text @hash, owner text)');
    await s.run(
        r"create statement claim($owner text) public as set jobs {status: 'taken', owner: $owner} where status = 'open' limit 1 returning kind");
    final db = Fennec.connect(s.url);
    await db.execute('put jobs {kind: "mail", status: "open"}');
    final claimed = await db.callStatement('claim', args: {'owner': 'w1'}, wait: const Duration(seconds: 5));
    expect(claimed.rows.single['kind'], 'mail');
    // Nothing left: held no longer than the wait.
    final began = DateTime.now();
    final empty = await db.callStatement('claim', args: {'owner': 'w2'}, wait: const Duration(milliseconds: 300));
    expect(empty.rows, isEmpty);
    expect(DateTime.now().difference(began).inMilliseconds, greaterThanOrEqualTo(290));
    await expectLater(db.callStatement('claim', args: {'owner': 'w3'}, wait: const Duration(seconds: -1)),
        throwsA(isA<FennecException>().having((e) => e.code, 'code', FennecCode.misuse)));
    db.close();
  });

  // A replica holds its shapes and none of its server's statements: its own
  // database refuses a call, in the engine's words, as a FennecException; the
  // server is called through its HTTP client, and what it wrote comes back
  // through the shape's stream.
  test("a synced database refuses a call, and the server's client makes it", () async {
    final s = await server('callrefused');
    await s.run(r'create statement close_task($key text) public as set tasks {status: "closed"} where key = $key');
    final db = await Fennec.openSynced(url: s.url, shapes: [open], path: await scratch('callrefused'));
    await db.replica!.ready();
    await offline(db);
    await expectLater(
        db.callStatement('close_task', args: {'key': 'a'}),
        throwsA(isA<FennecException>().having((e) => e.code, 'code', FennecCode.query).having(
            (e) => e.message,
            'message',
            contains('a synced database runs no named statement: `call`, `create statement` and `drop statement` '
                'go to the server through its HTTP client'))));
    // Refused before anything was queued or sent.
    expect((await db.replica!.refresh()).pending, 0);
    expect(await titles(db), ['one', 'two']);
    db.replica!.setOnline(true);
    await eventually('online', () async => (await db.replica!.refresh()).state == SyncState.online);
    // Through the client the server runs it, and the shape follows.
    final remote = Fennec.connect(s.url);
    expect((await remote.callStatement('close_task', args: {'key': 'a'})).affected, 1);
    remote.close();
    await eventually('the row leaves the shape', () async => (await titles(db)).join(',') == 'two');
    await db.close();
  });

  // A job is ready by an instant the test holds, not by now(): a lease of
  // now() + a minute runs out whenever a minute passes between two claims,
  // and an iOS simulator's run of the Swift twin once stalled 75 s between
  // them, the second claim taking a job the first held. now() writes the
  // leases still, every one of them past that instant.
  test('a claim held at the server takes the job enqueued meanwhile, and ends on time', () async {
    final s = await server('held');
    await s.run('create collection jobs (kind text, run_at timestamp @sorted, owner text)');
    final db = Fennec.connect(s.url);
    // Past every job's run_at (milliseconds since the epoch).
    const ready = 1000;
    Future<Rows> claim(FennecRemote c, String owner) =>
        c.from('jobs').where(Cond.raw('run_at <= ?', [ready])).order('run_at').limit(1).updateReturning({
          'owner': owner,
          'run_at': Computed.expr('now() + ?', [60000])
        }, returning: [
          'kind'
        ]);
    final held = claim(db.withWait(const Duration(seconds: 10)), 'w1');
    await Future<void>.delayed(const Duration(milliseconds: 200));
    await db.execute('put jobs {kind: "mail", run_at: 1}');
    expect([for (final r in await held.timeout(const Duration(seconds: 5))) r['kind']], ['mail']);
    final began = DateTime.now();
    expect(await claim(db.withWait(const Duration(milliseconds: 300)), 'w2'), isEmpty);
    expect(DateTime.now().difference(began).inMilliseconds, greaterThanOrEqualTo(290));
    db.close();
  });

  test('a server answers marks in the row and facets beside the rows', () async {
    final s = await server('search');
    await s.run('create collection docs (body text @text, kind text)');
    await s.run('put docs [{body: "rust is fast", kind: "a"}, {body: "rust and go", kind: "a"}, '
        '{body: "python", kind: "b"}]');
    final db = Fennec.connect(s.url);
    final hits = await db.from('docs').select(['kind']).highlight('body').match('body', 'rust').rows();
    expect([
      for (final r in hits) r['highlight(body)']
    ], [
      [
        [0, 4]
      ],
      [
        [0, 4]
      ]
    ]);
    expect(hits.facets, isNull);
    // `/query` answers an object here: the rows, and the counts of every
    // matching row beside them.
    final a = await db.from('docs').facet('kind').limit(1).answer();
    expect(a.rows, hasLength(1));
    expect(a.facets, {
      'kind': [const FacetCount('a', 2), const FacetCount('b', 1)]
    });
    final rows = await db.from('docs').where('kind', 'a').facet('kind', top: 1).rows();
    expect(rows, hasLength(2));
    expect(rows.facets, {
      'kind': [const FacetCount('a', 2)]
    });
    db.close();
  });
}

extension<T> on T {
  void let(void Function(T) f) => f(this);
}
