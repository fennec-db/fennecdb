import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:fennecdb/fennecdb.dart';
import 'package:fennecdb/src/values.dart' show iso, number, writeJson;
import 'package:test/test.dart';

/// A file of its own in a directory of its own, as an app's documents
/// directory holds one.
String scratch() => '${Directory.systemTemp.createTempSync('fennecdb-dart').path}/app.fennec';

Future<FennecCode?> codeOf(Future<Object?> f) async {
  try {
    await f;
    return null;
  } on FennecException catch (e) {
    return e.code;
  }
}

void main() {
  test('a statement and its rows', () async {
    final db = await Fennec.memory();
    final created = await db.run('create collection notes (title text, stars int, embed vector<3> @hnsw(cosine))');
    expect(created.message, 'collection `notes` created');
    final n = await db.execute('put notes [{title: \$1, stars: 5, embed: \$2}, {title: \$3, stars: 3, embed: \$4}]', [
      'oasis',
      Float32List.fromList([1, 0, 0]),
      'dunes',
      [0, 1, 0],
    ]);
    expect(n, 2);
    final rows = await db.query('get notes select title, stars order stars desc');
    expect(rows.map((r) => r['title']), ['oasis', 'dunes']);
    expect(rows[0]['stars'], 5);
    expect(rows[0].keys, ['title', 'stars']);
    await db.close();
  });

  test('near scores are exact: the vector goes over as its bytes', () async {
    final db = await Fennec.memory();
    await db.execute('create collection v (n int, e vector<2> @hnsw(l2))');
    await db.execute('put v [{n: 1, e: [1, 0]}, {n: 2, e: [0, 1]}, {n: 3, e: [0.5, 0.25]}]');
    final rows = await db.query('get v select n near e \$1 limit 3', [
      Float32List.fromList([1, 0])
    ]);
    expect(rows.map((r) => r['n']), [1, 3, 2]);
    double f32(double x) => (Float32List(1)..[0] = x)[0];
    final third = f32(sqrt(f32(f32(0.25) + f32(0.0625))));
    expect(rows.map((r) => f32((r['_score'] as num).toDouble())), [0, third, f32(sqrt(2))]);
    // A list of numbers is a vector too, as a page's are.
    expect(
        (await db.query('get v select n near e \$1 limit 1', [
          [0.0, 1.0]
        ]))
            .first['n'],
        2);
    expect((await db.query('get v select e where n = 3')).first['e'], [0.5, 0.25]);
    await db.close();
  });

  test('the builder writes and reads', () async {
    final db = await Fennec.memory();
    await db.execute('create collection notes (title text, stars int, at timestamp)');
    final notes = db.from('notes');
    final at = DateTime.fromMillisecondsSinceEpoch(1789821296789, isUtc: true);
    expect(
        await notes.insert([
          {'title': 'a', 'stars': 4, 'at': at},
          {'title': 'b', 'stars': 2},
        ]),
        2);
    expect(notes.toInsert({'title': 'a', 'stars': 4}).text, 'put notes {title: \$1, stars: \$2}');
    expect(notes.toInsert({'at': at.toLocal()}).params, ['2026-09-19T12:34:56.789Z']);
    expect((await notes.order('stars', 'desc').rows()).map((r) => r['title']), ['a', 'b']);
    expect((await notes.where('title', 'a').first())?['stars'], 4);
    expect(await notes.where('title', 'zz').first(), isNull);
    expect(await notes.count(), 2);
    expect(await notes.where('stars', '>', 3).update({'stars': 5}), 1);
    expect(await notes.where('stars', 5).delete(), 1);
    await db.close();
  });

  test('marks answer in the row, facets beside the rows', () async {
    final db = await Fennec.memory();
    await db.execute('create collection docs (body text @text, kind text)');
    await db.execute('put docs [{body: "rust is fast", kind: "a"}, {body: "rust and go", kind: "a"}, '
        '{body: "python", kind: "b"}]');
    final docs = db.from('docs');
    final hits = await docs.highlight('body').match('body', 'rust').rows();
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
    final tagged = await docs.select(['kind']).highlight('body', pre: '[', post: ']').match('body', 'fast').rows();
    expect(tagged.single['highlight(body)'], 'rust is [fast]');
    final snip = await docs.snippet('body', 2).match('body', 'fast').first();
    expect((snip!['snippet(body)'] as Map)['marks'], isNotEmpty);
    expect(hits.facets, isNull);
    final a = await docs.facet('kind').limit(1).answer();
    expect(a.rows, hasLength(1));
    expect(a.facets, {
      'kind': [const FacetCount('a', 2), const FacetCount('b', 1)]
    });
    expect((await docs.where('kind', 'b').facet('kind').rows()).facets, {
      'kind': [const FacetCount('b', 1)]
    });
    // A range's value is its bounds, a list; a disjunctive facet counts
    // past the filter's own condition on its field.
    await db.execute('create collection p (price float, kind text)');
    await db.execute('put p [{price: 5, kind: "a"}, {price: 30, kind: "b"}, {price: 40, kind: "b"}]');
    final f = (await db
            .from('p')
            .where('kind', 'a')
            .facet('price', ranges: [0, 25, 50.5])
            .facet('kind', disjunctive: true)
            .limit(0)
            .answer())
        .facets!;
    expect([
      for (final c in f['price']!) c.value
    ], [
      [0, 25],
      [25, 50.5]
    ]);
    expect([for (final c in f['price']!) c.count], [1, 0]);
    expect([for (final c in f['kind']!) (c.value, c.count)], [('b', 2), ('a', 1)]);
    await db.close();
  });

  test('a json field takes its list as written', () async {
    final db = await Fennec.memory();
    await db.execute('create collection t (meta json)');
    await db.execute('put t {meta: \$1}', [
      [0.1, 0.2]
    ]);
    expect((await db.query('get t select meta')).first['meta'], [0.1, 0.2]);
    await db.close();
  });

  test('errors carry their kind', () async {
    final db = await Fennec.memory();
    await db.execute('create collection t (a int @unique)');
    await db.execute('put t {a: 1}');
    for (final (text, code) in [
      ('get nowhere', FennecCode.notFound),
      ('broken query', FennecCode.query),
      ('create collection t (a int)', FennecCode.exists),
      ('put t {a: 1}', FennecCode.duplicate),
      ('put t {a: "x"}', FennecCode.type),
    ]) {
      expect(await codeOf(db.run(text)), code, reason: text);
    }
    expect(
        () => db.from('t; drop collection t'),
        throwsA(isA<FennecException>()
            .having((e) => e.code, 'code', FennecCode.builder)
            .having((e) => e.message, 'message', 'invalid collection name: "t; drop collection t"')));
    await db.close();
    expect(await codeOf(db.run('get t')), FennecCode.misuse);
    await db.close();
  });

  test("a tenant's quota refusal names the quota and the wait", () {
    final rows = FennecRemote.refused(507, {'error': 'past its quota of 5 rows', 'quota': 'rows'}, null);
    expect((rows.code, rows.status, rows.quota, rows.retryAfter), (FennecCode.quota, 507, 'rows', null));
    // Its header over its body.
    final rate = FennecRemote.refused(429, {'error': 'past', 'quota': 'writes', 'retry_after': 1}, '3');
    expect((rate.quota, rate.retryAfter), ('writes', const Duration(seconds: 3)));
    // To the millisecond where the body says it, over the whole second.
    final soon =
        FennecRemote.refused(429, {'error': 'past', 'quota': 'writes', 'retry_after': 1, 'retry_after_ms': 2}, '1');
    expect((soon.quota, soon.retryAfter), ('writes', const Duration(milliseconds: 2)));
    final memory = FennecRemote.refused(507, {'error': 'data ceiling'}, null);
    expect((memory.code, memory.quota), (FennecCode.io, null));
    // A tenant being moved says the wait in its body as a quota does.
    final moved =
        FennecRemote.refused(503, {'error': 'the tenant is being moved; retry shortly', 'retry_after': 1}, null);
    expect((moved.status, moved.quota, moved.retryAfter), (503, null, const Duration(seconds: 1)));
  });

  test('a require not met is refused, and writes nothing', () async {
    final db = await Fennec.memory();
    await db.execute('create collection accounts (balance int)');
    await db.execute('put accounts {id: 1, balance: 5}');
    final accounts = db.from('accounts');
    expect(await codeOf(accounts.where('id', 2).update({'balance': 0}, require: 1)), FennecCode.unmet);
    expect((await accounts.rows()).map((r) => r['balance']), [5]);
    expect(await accounts.where('id', 1).update({'balance': 3}, require: 1), 1);
    await db.close();
  });

  // The statement's own words, the library's and a server's alike; null
  // where it gave none.
  test('an unmet require says its reason', () async {
    Future<(FennecCode, String?)?> refusal(Future<Object?> f) async {
      try {
        await f;
        return null;
      } on FennecException catch (e) {
        return (e.code, e.reason);
      }
    }

    final db = await Fennec.memory();
    await db.execute('create collection accounts (name text, balance int)');
    await db.execute('put accounts {id: 1, name: "a", balance: 5}');
    final accounts = db.from('accounts');
    expect(
        await refusal(accounts
            .where('name', 'a')
            .where('balance', '>=', 9)
            .update({'balance': Computed.inc(-9)}, require: 1, reason: 'insufficient funds')),
        (FennecCode.unmet, 'insufficient funds'));
    expect(await refusal(accounts.where('id', 2).limit(1).require(1, reason: "it's not there").rows()),
        (FennecCode.unmet, "it's not there"));
    expect(await refusal(accounts.where('id', 2).delete(require: 1)), (FennecCode.unmet, null));
    await db.close();
    final remote = FennecRemote.refused(412,
        {'error': 'no such account (`set accounts` wrote 0 rows, and requires 1)', 'reason': 'no such account'}, null);
    expect((remote.code, remote.reason), (FennecCode.unmet, 'no such account'));
    final batch = FennecRemote.refused(412, {'error': 'unmet: x', 'completed': 0, 'at': 1, 'reason': 'x'}, null);
    expect((batch.at, batch.reason), (1, 'x'));
    expect(FennecRemote.refused(412, {'error': 'unmet'}, null).reason, isNull);
  });

  // The builder's call beside the golden file's cases (golden_test.dart):
  // what a typed language adds, a value to keep and to change after.
  test('a call is a value taken as it is made', () {
    final args = {
      'at': DateTime.utc(2026, 9, 19, 12, 34, 56, 789),
      'embed': Float32List.fromList([0.5, -0.25, 1]),
      'meta': {'lang': 'tr'},
    };
    final claims = {'sub': 'alice'};
    final c = Call('since', args: args, claims: claims);
    // Changed after, the call still makes what it was made with.
    args['at'] = 'late';
    (args['meta'] as Map)['lang'] = 'en';
    claims['sub'] = 'bob';
    final made = c.toCall();
    expect(made.text, 'call since \$1 as \$2');
    expect(made.params[0], {
      'at': '2026-09-19T12:34:56.789Z',
      'embed': Float32List.fromList([0.5, -0.25, 1]),
      'meta': {'lang': 'tr'},
    });
    expect(made.params[1], {'sub': 'alice'});
    // Each text is its own parameters: the one handed out is not the call's.
    (made.params[1] as Map)['sub'] = 'mallory';
    expect(c.toCall().params[1], {'sub': 'alice'});
    expect(c.name, 'since');
    // The pair is a statement of a batch.
    expect(made, isA<Statement>());
    // Not a map, or a map with no way to be a value: refused as a chain is.
    for (final (name, bad) in [
      ('arguments', [1, 2]),
      ('arguments', 'from 1'),
      ('arguments', 5),
      ('arguments', DateTime.utc(2026)),
    ]) {
      expect(
          () => Call('transfer', args: bad),
          throwsA(isA<FennecException>()
              .having((e) => e.code, 'code', FennecCode.builder)
              .having((e) => e.message, 'message', "a call's $name are an object of values by name")),
          reason: '$bad');
    }
    expect(() => Call('mine', claims: 'alice'),
        throwsA(isA<FennecException>().having((e) => e.code, 'code', FennecCode.builder)));
    expect(() => Call('mine', args: {'x': Object()}), throwsA(isA<FennecException>()));
    // The name is the one thing written into the text.
    expect(() => Call('stamp; del docs'), throwsA(isA<FennecException>()));
    expect(Call('stamp').toCall().text, 'call stamp');
    expect(Call('stamp').toCall().params, isEmpty);
  });

  // A statement of the file's own, called from the app: `callStatement` is
  // `call <name> $1` through the same door a text takes.
  test('a call runs a statement in the file, with the claims an app names', () async {
    final db = await Fennec.memory();
    await db.execute('create collection accounts (owner text @hash, balance int)');
    await db.execute('create collection notes (body text, by text @hash)');
    await db.execute('put accounts [{id: 1, owner: "a", balance: 100}, {id: 2, owner: "b", balance: 0}]');
    await db.run('create statement transfer(\$from int, \$to int, \$amount int) public as begin '
        "set accounts {balance: balance - \$amount} where id = \$from and balance >= \$amount require 1 else 'insufficient funds'; "
        "set accounts {balance: balance + \$amount} where id = \$to require 1 else 'no such account'; "
        'get accounts select id, balance where id = \$from end');
    await db.run("create statement stamp() public as insert notes {body: 'z', by: 'x'} returning id");
    await db.run('create statement mine() claims (sub text) as get notes select body where by = \$jwt.sub');

    // The answer is the last statement's, arguments of several types by name.
    final moved = await db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 30});
    expect(moved.rows.single, {'id': 1, 'balance': 70});
    expect(await db.from('accounts').where('id', 2).first(), containsPair('balance', 30));
    // A statement of none, and a write that answers rows.
    expect((await db.callStatement('stamp')).rows.single['id'], isNotNull);
    expect(await db.from('notes').count(), 1);

    // A require refused says its words, and the call lands nothing.
    await expectLater(
        db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 500}),
        throwsA(isA<FennecException>()
            .having((e) => e.code, 'code', FennecCode.unmet)
            .having((e) => e.reason, 'reason', 'insufficient funds')));
    await expectLater(db.callStatement('transfer', args: {'from': 1, 'to': 9, 'amount': 5}),
        throwsA(isA<FennecException>().having((e) => e.reason, 'reason', 'no such account')));
    expect(await db.from('accounts').where('id', 1).first(), containsPair('balance', 70));

    // An argument missing, of another type, or one the statement lacks; a
    // statement that is not there.
    for (final bad in [
      {'from': 1, 'to': 2},
      {'from': 1, 'to': 2, 'amount': 'ten'},
      {'from': 1, 'to': 2, 'amount': 1, 'by': 3},
    ]) {
      expect(await codeOf(db.callStatement('transfer', args: bad)), FennecCode.query, reason: '$bad');
    }
    expect(await codeOf(db.callStatement('nothing')), FennecCode.notFound);

    // `claims` are the `as`: what `$jwt.sub` reads, the embedder's own.
    await db.from('notes').insert({'body': 'mine', 'by': 'alice'});
    expect((await db.callStatement('mine', claims: {'sub': 'alice'})).rows.map((r) => r['body']), ['mine']);
    expect((await db.callStatement('mine', claims: {'sub': 'bob'})).rows, isEmpty);
    // A claim missing, null or of another type is one the call lacks.
    for (final c in [
      null,
      <String, Object?>{},
      {'sub': null},
      {'sub': 5}
    ]) {
      expect(await codeOf(db.callStatement('mine', claims: c)), FennecCode.denied, reason: '$c');
    }
    // A refusal of the builder is thrown before anything is sent.
    await expectLater(db.callStatement('mine; drop statement mine'), throwsA(isA<FennecException>()));
    await db.close();
    expect(await codeOf(db.callStatement('stamp')), FennecCode.misuse);
  });

  test('a file is there again, and open once', () async {
    final path = scratch();
    var db = await Fennec.open(path);
    await db.execute('create collection t (title text, e vector<2> @hnsw(cosine))');
    for (var i = 0; i < 20; i++) {
      await db.execute('put t {title: \$1, e: \$2}', [
        'n$i',
        Float32List.fromList([i / 10, 1])
      ]);
    }
    expect(await codeOf(Fennec.open(path)), FennecCode.locked);
    await db.checkpoint();
    await db.close();
    db = await Fennec.open(path, flags: Fennec.noSync);
    expect(await db.from('t').count(), 20);
    await db.execute('put t {title: "after"}');
    await db.flush();
    await db.sync();
    await db.close();
    db = await Fennec.open(path, flags: Fennec.inMemory);
    expect(await db.from('t').count(), 21);
    final near = await db.from('t').select(['title']).near('e', Float32List.fromList([0, 1])).limit(1).rows();
    expect(near.first['title'], 'n0');
    await db.close();
  });

  test('warm builds what an open left, once', () async {
    final path = scratch();
    var db = await Fennec.open(path);
    await db.execute('create collection docs (body text @text)');
    await db.execute('put docs [{body: "rust is fast"}, {body: "go is simple"}]');
    await db.close();
    db = await Fennec.open(path);
    expect(await db.warm(['docs']), 1);
    final hits = await db.from('docs').select(['body']).match('body', 'rust').rows();
    expect([for (final r in hits) r['body']], ['rust is fast']);
    expect(await db.warm(), 0);
    await db.close();
  });

  test('many calls at once share one database', () async {
    final db = await Fennec.open(scratch());
    await db.execute('create collection t (w int, e vector<2> @hnsw(l2))');
    await Future.wait([
      for (var w = 0; w < 8; w++)
        () async {
          for (var i = 0; i < 20; i++) {
            if (w.isEven) {
              await db.execute('put t {w: \$1, e: \$2}', [
                w,
                [w, i]
              ]);
            } else {
              await db.query('get t near e \$1 limit 3', [
                [0, 1]
              ]);
            }
          }
        }()
    ]);
    expect(await db.from('t').count(), 80);
    await db.close();
  });

  test('values write as JSON the engine reads', () {
    expect(writeJson({'s': 'a"b\n\u0001ç😀', 'n': -1, 'f': 0.5, 'w': 2024.0, 'ok': true, 'none': null}),
        '{"s":"a\\"b\\n\\u0001ç😀","n":-1,"f":0.5,"w":2024,"ok":true,"none":null}');
    expect(number(double.nan), 'null');
    expect(iso(DateTime.fromMillisecondsSinceEpoch(-1, isUtc: true)), '1969-12-31T23:59:59.999Z');
  });

  test('the version is the library\'s', () async {
    expect((await Fennec.version()).split('.'), hasLength(3));
  });
}
