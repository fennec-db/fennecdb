// Written by site/builder_tabs.py (make docs-builder): do not edit.

// The code of each builder tab on the site, as the page shows it, run
// against a transport that records and held to the statement its case of
// integrations/builder-golden.json makes: a tab shows code this SDK runs.
// make docs-builder writes the file again as the pages' tabs change, and
// site/builder_tabs.py --check fails while it is behind them.

import 'dart:convert';

import 'package:fennecdb/fennecdb.dart';
import 'package:test/test.dart';

/// What the tabs call `db`: queries bound to a recorder, as a database's are to its engine.
class _Recording {
  ({String text, List<Object?> params})? sent;

  Query from(String collection) => Query.from(collection).bind((text, params) async {
        sent = (text: text, params: params);
        if (RegExp(r'^(put|insert|set|del) ').hasMatch(text)) return Answer.of({'kind': 'affected', 'count': 0});
        return Answer.of({
          'kind': 'rows',
          'result': {
            'columns': [],
            'rows': text.endsWith(' count')
                ? [
                    {'count': 0}
                  ]
                : []
          }
        });
      });

  Future<Answer> callStatement(String name, {Object? args, Object? claims}) async {
    sent = Call(name, args: args, claims: claims).toCall();
    return Answer.of({'kind': 'affected', 'count': 0});
  }

  void sends(String text, String params) {
    expect(sent?.text, text);
    final got = jsonDecode(jsonEncode(sent?.params));
    expect(_same(got, jsonDecode(params)), isTrue, reason: 'params $got, want $params');
  }
}

/// JSON compared by value: a number written 1 and 1.0 is one number.
bool _same(Object? a, Object? b) {
  if (a is num && b is num) return a.toDouble() == b.toDouble();
  if (a is List && b is List) {
    return a.length == b.length && [for (var i = 0; i < a.length; i++) i].every((i) => _same(a[i], b[i]));
  }
  if (a is Map && b is Map) return a.length == b.length && a.keys.every((k) => b.containsKey(k) && _same(a[k], b[k]));
  return a == b;
}

void main() {
  test('docs: aggregates over every order', () async {
    final db = _Recording();
    await db.from('orders').select(['count(*)', 'sum(total)', 'avg(total)', 'min(at)', 'max(at)']).rows();
    db.sends('get orders select count(*), sum(total), avg(total), min(at), max(at)', '[]');
  });

  test('docs: aggregates of each status', () async {
    final db = _Recording();
    await db.from('orders').select(['status', 'count(*)', 'avg(total)']).where('year', 2026).group('status').rows();
    db.sends('get orders select status, count(*), avg(total) where year = \$1 group status', '[2026]');
  });

  test('docs: the ten customers who spent most', () async {
    final db = _Recording();
    await db
        .from('orders')
        .select(['customer', 'sum(total)'])
        .group('customer')
        .order('sum(total)', 'desc')
        .limit(10)
        .rows();
    db.sends('get orders select customer, sum(total) group customer order sum(total) desc limit \$1', '[10]');
  });

  test('docs: distinct users an hour and event', () async {
    final db = _Recording();
    await db
        .from('events')
        .select([Computed.bucket('at', '1h').as('hour'), 'name', Computed.countDistinct('user').as('users')]).group(
            ['hour', 'name']).rows();
    db.sends('get events select bucket(at, 1h) as hour, name, count(distinct user) as users group hour, name', '[]');
  });

  test('docs: one-minute bars of a symbol', () async {
    final db = _Recording();
    await db
        .from('ticks')
        .select([
          Computed.bucket('at', '1m').as('bar'),
          Computed.first('px', 'at').as('open'),
          Computed.expr('max(px)').as('high'),
          Computed.expr('min(px)').as('low'),
          Computed.last('px', 'at').as('close'),
          Computed.expr('sum(qty)').as('volume')
        ])
        .where('sym', 'S07')
        .group('bar')
        .rows();
    db.sends(
        'get ticks select bucket(at, 1m) as bar, first(px by at) as open, max(px) as high, min(px) as low, last(px by at) as close, sum(qty) as volume where sym = \$1 group bar',
        '["S07"]');
  });

  test('docs: vwap of each symbol', () async {
    final db = _Recording();
    await db.from('ticks').select(['sym', Computed.expr('sum(px * qty) / sum(qty)').as('vwap')]).group('sym').rows();
    db.sends('get ticks select sym, sum(px * qty) / sum(qty) as vwap group sym', '[]');
  });

  test('docs: totals of a json path', () async {
    final db = _Recording();
    await db.from('events').select(['props.plan', 'sum(props.v)']).group('props.plan').rows();
    db.sends('get events select props.plan, sum(props.v) group props.plan', '[]');
  });

  test('docs: the three statuses that took most', () async {
    final db = _Recording();
    await db
        .from('orders')
        .select(['status', 'count(*)', 'sum(total)'])
        .where('year', 2026)
        .group('status')
        .order('sum(total)', 'desc')
        .limit(3)
        .rows();
    db.sends(
        'get orders select status, count(*), sum(total) where year = \$1 group status order sum(total) desc limit \$2',
        '[2026, 3]');
  });

  test('docs: one-minute bars since a time', () async {
    final db = _Recording();
    await db
        .from('ticks')
        .select([
          Computed.bucket('at', '1m').as('bar'),
          Computed.first('px', 'at').as('open'),
          Computed.expr('max(px)').as('high'),
          Computed.expr('min(px)').as('low'),
          Computed.last('px', 'at').as('close'),
          Computed.expr('sum(qty)').as('volume')
        ])
        .where('sym', 'S07')
        .where('at', '>=', '2026-05-03T09:00:00Z')
        .group('bar')
        .rows();
    db.sends(
        'get ticks select bucket(at, 1m) as bar, first(px by at) as open, max(px) as high, min(px) as low, last(px by at) as close, sum(qty) as volume where sym = \$1 and at >= \$2 group bar',
        '["S07", "2026-05-03T09:00:00Z"]');
  });

  test('docs: the newest articles', () async {
    final db = _Recording();
    await db.from('articles').order('published', 'desc').limit(20).rows();
    db.sends('get articles order published desc limit \$1', '[20]');
  });

  test('docs: the newest articles since a day', () async {
    final db = _Recording();
    await db.from('articles').where('published', '>=', '2026-09-01').order('published', 'desc').limit(20).rows();
    db.sends('get articles where published >= \$1 order published desc limit \$2', '["2026-09-01", 20]');
  });

  test('docs: articles counted over a range of titles', () async {
    final db = _Recording();
    await db.from('articles').where('title', '>=', 'k').where('title', '<', 'l').count();
    db.sends('get articles where title >= \$1 and title < \$2 count', '["k", "l"]');
  });

  test('docs: the nearest articles since a day', () async {
    final db = _Recording();
    await db.from('articles').where('published', '>=', '2026-01-01').near('embed', [0.9, 0.1, 0]).limit(10).rows();
    db.sends('get articles where published >= \$1 near embed \$2 limit \$3', '["2026-01-01", [0.9, 0.1, 0], 10]');
  });

  test('docs: an owner\'s newest posts', () async {
    final db = _Recording();
    await db.from('posts').where('owner', 7).order('created', 'desc').limit(20).rows();
    db.sends('get posts where owner = \$1 order created desc limit \$2', '[7, 20]');
  });

  test('docs: three owners\' newest posts', () async {
    final db = _Recording();
    await db.from('posts').where('owner', 'in', [7, 8, 9]).order('created', 'desc').limit(20).rows();
    db.sends('get posts where owner in [\$1, \$2, \$3] order created desc limit \$4', '[7, 8, 9, 20]');
  });

  test('docs: an owner\'s posts counted since a time', () async {
    final db = _Recording();
    await db.from('posts').where('owner', 7).where('created', '>=', '2026-10-01').count();
    db.sends('get posts where owner = \$1 and created >= \$2 count', '[7, "2026-10-01"]');
  });

  test('docs: a session seen', () async {
    final db = _Recording();
    await db.from('sessions').insert({'user': 'ada', 'seen': Computed.expr('now()')});
    db.sends('put sessions {user: \$1, seen: now()}', '["ada"]');
  });

  test('docs: a session seen again', () async {
    final db = _Recording();
    await db.from('sessions').where('user', 'ada').update({'seen': Computed.expr('now()')});
    db.sends('set sessions {seen: now()} where user = \$1', '["ada"]');
  });

  test('docs: a user\'s sessions', () async {
    final db = _Recording();
    await db.from('sessions').where('user', 'ada').rows();
    db.sends('get sessions where user = \$1', '["ada"]');
  });

  test('docs: products with their newest reviews', () async {
    final db = _Recording();
    await db
        .from('products')
        .limit(20)
        .lookup('reviews', on: 'product_id', order: [SortKey('created', 'desc')], limit: 3)
        .rows();
    db.sends('get products limit \$1 lookup reviews on product_id order created desc limit \$2', '[20, 3]');
  });

  test('docs: products with their reviews', () async {
    final db = _Recording();
    await db.from('products').lookup('reviews', on: 'product_id').rows();
    db.sends('get products lookup reviews on product_id', '[]');
  });

  test('docs: the names of dear products with their good reviews', () async {
    final db = _Recording();
    await db
        .from('products')
        .select(['name'])
        .where('price', '>=', 10000)
        .lookup('reviews', on: 'product_id', select: ['body'], where: Cond.cmp('stars', 'gte', 4), limit: 3)
        .rows();
    db.sends(
        'get products select name where price >= \$1 lookup reviews on product_id select body where stars >= \$2 limit \$3',
        '[10000, 4, 3]');
  });

  test('docs: products with their tags by sku', () async {
    final db = _Recording();
    await db.from('products').lookup('tags', on: 'sku', parentKey: 'sku').rows();
    db.sends('get products lookup tags on sku = sku', '[]');
  });

  test('docs: shops, their orders and the orders\' lines', () async {
    final db = _Recording();
    await db
        .from('shops')
        .limit(20)
        .lookup('orders', on: 'shop_id', limit: 3)
        .lookup('lines', on: 'order_id', limit: 5)
        .rows();
    db.sends('get shops limit \$1 lookup orders on shop_id limit \$2 lookup lines on order_id limit \$3', '[20, 3, 5]');
  });

  test('docs: orders with their lines by a key of two fields', () async {
    final db = _Recording();
    await db
        .from('orders')
        .where('org', 7)
        .limit(20)
        .lookup('lines', on: ['org', 'order'], parentKey: ['org', 'id'], order: [SortKey('line')], limit: 5)
        .rows();
    db.sends('get orders where org = \$1 limit \$2 lookup lines on (org, order) = (org, id) order line asc limit \$3',
        '[7, 20, 5]');
  });

  test('docs: shops with their orders folded', () async {
    final db = _Recording();
    await db.from('shops').limit(20).lookup('orders',
        on: 'shop', select: [Computed.expr('count(*)').as('n'), Computed.expr('sum(total)').as('revenue')]).rows();
    db.sends('get shops limit \$1 lookup orders on shop select count(*) as n, sum(total) as revenue', '[20]');
  });

  test('docs: couriers with their latest fifty deliveries folded', () async {
    final db = _Recording();
    await db
        .from('couriers')
        .limit(20)
        .lookup('deliveries',
            on: 'courier',
            select: [Computed.expr('count(*)').as('n'), Computed.expr('avg(km)').as('km')],
            order: [SortKey('at', 'desc')],
            limit: 50)
        .rows();
    db.sends(
        'get couriers limit \$1 lookup deliveries on courier select count(*) as n, avg(km) as km order at desc limit \$2',
        '[20, 50]');
  });

  test('docs: each shop\'s three best sellers', () async {
    final db = _Recording();
    await db
        .from('shops')
        .lookup('lines',
            on: 'shop',
            select: ['product', Computed.expr('sum(qty)').as('sold')],
            group: ['product'],
            order: [SortKey('sold', 'desc')],
            limit: 3)
        .rows();
    db.sends('get shops lookup lines on shop select product, sum(qty) as sold group product order sold desc limit \$1',
        '[3]');
  });

  test('docs: the busiest couriers with their names', () async {
    final db = _Recording();
    await db
        .from('orders')
        .select(['courier', Computed.expr('count(*)').as('n')])
        .where('at', '>=', '2026-10-01')
        .group('courier')
        .order('n', 'desc')
        .limit(10)
        .lookup('couriers', on: 'user', parentKey: 'courier', select: ['name'])
        .rows();
    db.sends(
        'get orders select courier, count(*) as n where at >= \$1 group courier order n desc limit \$2 lookup couriers on user = courier select name',
        '["2026-10-01", 10]');
  });

  test('docs: products with a five-star review', () async {
    final db = _Recording();
    await db.from('products').limit(20).lookup('reviews', on: 'product_id', required: true, where: {'stars': 5}).rows();
    db.sends('get products limit \$1 lookup reviews on product_id required where stars = \$2', '[20, 5]');
  });

  test('docs: products with a five-star review counted', () async {
    final db = _Recording();
    await db.from('products').lookup('reviews', on: 'product_id', required: true, where: {'stars': 5}).count();
    db.sends('get products count lookup reviews on product_id required where stars = \$1', '[5]');
  });

  test('docs: orders of customers in a country', () async {
    final db = _Recording();
    await db
        .from('orders')
        .where(Cond.cmp('customer', 'in', db.from('customers').select(['id']).where('country', 'TR')))
        .rows();
    db.sends('get orders where customer in (get customers select id where country = \$1)', '["TR"]');
  });

  test('docs: open orders of customers of a tier', () async {
    final db = _Recording();
    await db.from('orders').where({
      'status': 'open',
      'customer': {
        'in': db.from('customers').select(['id']).where('tier', '>=', 2)
      }
    }).rows();
    db.sends(
        'get orders where status = \$1 and customer in (get customers select id where tier >= \$2)', '["open", 2]');
  });

  test('docs: orders of customers outside a country counted', () async {
    final db = _Recording();
    await db
        .from('orders')
        .where(Cond.not(Cond.cmp('customer', 'in', db.from('customers').select(['id']).where('country', 'TR'))))
        .count();
    db.sends('get orders where not (customer in (get customers select id where country = \$1)) count', '["TR"]');
  });

  test('docs: the sessions of banned users deleted', () async {
    final db = _Recording();
    await db.from('sessions').where('user', 'in', db.from('users').select(['name']).where('banned', true)).delete();
    db.sends('del sessions where user in (get users select name where banned = \$1)', '[true]');
  });

  test('docs: customers with their returned orders', () async {
    final db = _Recording();
    await db
        .from('customers')
        .limit(20)
        .lookup('orders',
            on: 'customer', where: Cond.cmp('id', 'in', db.from('returns').select(['order_id'])), limit: 3)
        .rows();
    db.sends('get customers limit \$1 lookup orders on customer where id in (get returns select order_id) limit \$2',
        '[20, 3]');
  });

  test('docs: orders of customers of an active tier', () async {
    final db = _Recording();
    await db
        .from('orders')
        .where(
            'customer',
            'in',
            db
                .from('customers')
                .select(['id']).where('tier', 'in', db.from('tiers').select(['n']).where('active', true)))
        .rows();
    db.sends(
        'get orders where customer in (get customers select id where tier in (get tiers select n where active = \$1))',
        '[true]');
  });

  test('docs: articles a page at a time', () async {
    final db = _Recording();
    await db.from('articles').where('year', 2024).order('published', 'desc').limit(20).offset(40).rows();
    db.sends('get articles where year = \$1 order published desc limit \$2 offset \$3', '[2024, 20, 40]');
  });

  test('docs: a page of matches with its brands counted', () async {
    final db = _Recording();
    await db.from('products').match('title', 'tent').limit(20).facet('brand', top: 10).rows();
    db.sends('get products match title \$1 limit \$2 facet brand top \$3', '["tent", 20, 10]');
  });

  test('docs: ready jobs claimed a page at a time', () async {
    final db = _Recording();
    await db.from('jobs').where(Cond.raw('run_at <= now()')).order('run_at').limit(10).updateReturning({'owner': 'w1'});
    db.sends('set jobs {owner: \$1} where run_at <= now() order run_at asc limit \$2 returning *', '["w1", 10]');
  });

  test('docs: orders by twice their count', () async {
    final db = _Recording();
    await db.from('orders').order(Computed.expr('n * 2'), 'desc').order('item').limit(10).rows();
    db.sends('get orders order n * 2 desc, item asc limit \$1', '[10]');
  });

  test('docs: one customer\'s orders first', () async {
    final db = _Recording();
    await db.from('orders').order(Computed.expr('case when customer = ? then 0 else 1 end', ['bo'])).order('id').rows();
    db.sends('get orders order case when customer = \$1 then 0 else 1 end asc, id asc', '["bo"]');
  });

  test('docs: a column worked out', () async {
    final db = _Recording();
    await db.from('ticks').select(['sym', Computed.expr('px * qty').as('notional')]).where('qty', '>', 100).rows();
    db.sends('get ticks select sym, px * qty as notional where qty > \$1', '[100]');
  });

  test('docs: columns named and worked out', () async {
    final db = _Recording();
    await db.from('ticks').select([
      Computed.expr('px').as('price'),
      Computed.bucket('at', '1m').as('minute'),
      Computed.expr('greatest(px, ?)', [100]).as('floor')
    ]).rows();
    db.sends('get ticks select px as price, bucket(at, 1m) as minute, greatest(px, \$1) as floor', '[100]');
  });

  test('docs: a column of a case', () async {
    final db = _Recording();
    await db.from('orders').select([
      'id',
      Computed.expr('case when total >= ? then ? else ? end', [100, 'large', 'small']).as('size')
    ]).rows();
    db.sends('get orders select id, case when total >= \$1 then \$2 else \$3 end as size', '[100, "large", "small"]');
  });

  test('docs: articles counted since a year', () async {
    final db = _Recording();
    await db.from('articles').where('year', '>=', 2024).count();
    db.sends('get articles where year >= \$1 count', '[2024]');
  });

  test('docs: a document with a json field', () async {
    final db = _Recording();
    await db.from('docs').insert({
      'title': 'a',
      'meta': {
        'lang': 'tr',
        'source': {'site': 'x', 'rank': 3}
      }
    });
    db.sends('put docs {title: \$1, meta: \$2}', '["a", {"lang": "tr", "source": {"site": "x", "rank": 3}}]');
  });

  test('docs: documents by paths', () async {
    final db = _Recording();
    await db.from('docs').where('meta.lang', 'tr').where('meta.source.rank', '>=', 2).rows();
    db.sends('get docs where meta.lang = \$1 and meta.source.rank >= \$2', '["tr", 2]');
  });

  test('docs: a path selected and ordered by', () async {
    final db = _Recording();
    await db.from('docs').select(['title', 'meta.source.site']).order('meta.source.rank', 'desc').rows();
    db.sends('get docs select title, meta.source.site order meta.source.rank desc', '[]');
  });

  test('docs: a path set', () async {
    final db = _Recording();
    await db.from('docs').where('title', 'a').update({'meta.lang': 'en'});
    db.sends('set docs {meta.lang: \$1} where title = \$2', '["en", "a"]');
  });

  test('docs: an event at a time written out', () async {
    final db = _Recording();
    await db.from('events').insert({'name': 'login', 't': '2026-09-19T12:34:56Z'});
    db.sends('put events {name: \$1, t: \$2}', '["login", "2026-09-19T12:34:56Z"]');
  });

  test('docs: an event at epoch milliseconds', () async {
    final db = _Recording();
    await db.from('events').insert({'name': 'logout', 't': 1758285296000});
    db.sends('put events {name: \$1, t: \$2}', '["logout", 1758285296000]');
  });

  test('docs: events between a day and now', () async {
    final db = _Recording();
    await db.from('events').where('t', '>=', '2026-01-01').where(Cond.raw('t < now()')).rows();
    db.sends('get events where t >= \$1 and t < now()', '["2026-01-01"]');
  });

  test('docs: people in the root collation', () async {
    final db = _Recording();
    await db.from('people').order('name', 'asc', 'und').limit(20).rows();
    db.sends('get people order name collate und asc limit \$1', '[20]');
  });

  test('docs: people in Turkish order', () async {
    final db = _Recording();
    await db.from('people').order('city', 'desc', 'tr').order('name', 'asc', 'tr').rows();
    db.sends('get people order city collate tr desc, name collate tr asc', '[]');
  });

  test('docs: people counted by city in order', () async {
    final db = _Recording();
    await db.from('people').select(['city', 'count(*)']).group('city').order('city', 'asc', 'und').rows();
    db.sends('get people select city, count(*) group city order city collate und asc', '[]');
  });

  test('docs: the page of people after a name', () async {
    final db = _Recording();
    await db.from('people').where('name', '>', 'Ada').order('name').limit(20).rows();
    db.sends('get people where name > \$1 order name asc limit \$2', '["Ada", 20]');
  });

  test('docs: a user', () async {
    final db = _Recording();
    await db.from('users').insert({'email': 'Alice@Example.com', 'name': 'İsmail'});
    db.sends('put users {email: \$1, name: \$2}', '["Alice@Example.com", "İsmail"]');
  });

  test('docs: a user by an email in any case', () async {
    final db = _Recording();
    await db.from('users').where('email', 'alice@example.COM').rows();
    db.sends('get users where email = \$1', '["alice@example.COM"]');
  });

  test('docs: a user whose email is taken', () async {
    final db = _Recording();
    await db.from('users').insert({'email': 'ALICE@example.com'});
    db.sends('put users {email: \$1}', '["ALICE@example.com"]');
  });

  test('docs: a user unless the email is taken', () async {
    final db = _Recording();
    await db.from('users').insert({'email': 'alice@example.com'}, ifAbsent: true);
    db.sends('put users {email: \$1} if absent', '["alice@example.com"]');
  });

  test('docs: a user by a name in any case', () async {
    final db = _Recording();
    await db.from('users').where('name', 'ismail').rows();
    db.sends('get users where name = \$1', '["ismail"]');
  });

  test('docs: users in Turkish order', () async {
    final db = _Recording();
    await db.from('users').select(['name']).order('name').limit(20).rows();
    db.sends('get users select name order name asc limit \$1', '[20]');
  });

  test('docs: articles matching words', () async {
    final db = _Recording();
    await db.from('articles').match('body', 'ownership').limit(10).rows();
    db.sends('get articles match body \$1 limit \$2', '["ownership", 10]');
  });

  test('docs: articles matching words since a year', () async {
    final db = _Recording();
    await db.from('articles').where('year', '>=', 2024).match('body', 'ownership').limit(10).rows();
    db.sends('get articles where year >= \$1 match body \$2 limit \$3', '[2024, "ownership", 10]');
  });

  test('docs: articles matching words reranked', () async {
    final db = _Recording();
    await db
        .from('articles')
        .match('body', 'ownership')
        .rerank('embed', [0.9, 0.1, 0], candidates: 500)
        .limit(10)
        .rows();
    db.sends(
        'get articles match body \$1 rerank embed \$2 candidates 500 limit \$3', '["ownership", [0.9, 0.1, 0], 10]');
  });

  test('docs: products matching a misspelt word', () async {
    final db = _Recording();
    await db.from('products').match('title', 'databse', fuzzy: true).limit(10).rows();
    db.sends('get products match title \$1 fuzzy limit \$2', '["databse", 10]');
  });

  test('docs: products matching a word begun', () async {
    final db = _Recording();
    await db.from('products').match('title', 'datab', prefix: true).limit(10).rows();
    db.sends('get products match title \$1 prefix limit \$2', '["datab", 10]');
  });

  test('docs: products matching a misspelt word begun', () async {
    final db = _Recording();
    await db.from('products').match('title', 'datbas', fuzzy: true, prefix: true).limit(10).rows();
    db.sends('get products match title \$1 fuzzy prefix limit \$2', '["datbas", 10]');
  });

  test('docs: products matching a word one edit away', () async {
    final db = _Recording();
    await db.from('products').match('title', 'databse', fuzzy: 1).limit(10).rows();
    db.sends('get products match title \$1 fuzzy 1 limit \$2', '["databse", 10]');
  });

  test('docs: a search box inside one category', () async {
    final db = _Recording();
    await db
        .from('products')
        .where('category', 'books')
        .match('title', 'datbas', fuzzy: true, prefix: true)
        .limit(20)
        .rows();
    db.sends('get products where category = \$1 match title \$2 fuzzy prefix limit \$3', '["books", "datbas", 20]');
  });

  test('docs: articles ranked by words and meaning', () async {
    final db = _Recording();
    await db.from('articles').match('body', 'rust ownership').near('embed', [0.12, -0.03, 0.4]).fuse().limit(10).rows();
    db.sends('get articles match body \$1 near embed \$2 fuse limit \$3', '["rust ownership", [0.12, -0.03, 0.4], 10]');
  });

  test('docs: articles since a year ranked by words and meaning', () async {
    final db = _Recording();
    await db
        .from('articles')
        .where('year', '>=', 2024)
        .match('body', 'rust ownership')
        .near('embed', [0.12, -0.03, 0.4])
        .fuse()
        .limit(10)
        .rows();
    db.sends('get articles where year >= \$1 match body \$2 near embed \$3 fuse limit \$4',
        '[2024, "rust ownership", [0.12, -0.03, 0.4], 10]');
  });

  test('docs: articles ranked by words and meaning, k and candidates', () async {
    final db = _Recording();
    await db
        .from('articles')
        .match('body', 'rust ownership')
        .near('embed', [0.12, -0.03, 0.4])
        .fuse(k: 60, candidates: 50)
        .limit(10)
        .rows();
    db.sends('get articles match body \$1 near embed \$2 fuse k 60 candidates 50 limit \$3',
        '["rust ownership", [0.12, -0.03, 0.4], 10]');
  });

  test('docs: matches with their marks', () async {
    final db = _Recording();
    await db.from('articles').select(['title']).highlight('body').match('body', 'ownership').limit(10).rows();
    db.sends('get articles select title, highlight(body) match body \$1 limit \$2', '["ownership", 10]');
  });

  test('docs: matches with their marks tagged', () async {
    final db = _Recording();
    await db
        .from('articles')
        .select(['title'])
        .highlight('body', pre: '<mark>', post: '</mark>')
        .match('body', 'ownership')
        .limit(10)
        .rows();
    db.sends('get articles select title, highlight(body, \$1, \$2) match body \$3 limit \$4',
        '["<mark>", "</mark>", "ownership", 10]');
  });

  test('docs: matches with a snippet', () async {
    final db = _Recording();
    await db
        .from('articles')
        .select(['title'])
        .snippet('body', 20, ellipsis: '…')
        .match('body', 'ownership')
        .limit(10)
        .rows();
    db.sends('get articles select title, snippet(body, 20, \$1) match body \$2 limit \$3', '["…", "ownership", 10]');
  });

  test('docs: matches with a snippet tagged', () async {
    final db = _Recording();
    await db
        .from('articles')
        .select(['title'])
        .snippet('body', 20, ellipsis: '…', pre: '<b>', post: '</b>')
        .match('body', 'ownership')
        .limit(10)
        .rows();
    db.sends('get articles select title, snippet(body, 20, \$1, \$2, \$3) match body \$4 limit \$5',
        '["…", "<b>", "</b>", "ownership", 10]');
  });

  test('docs: a page of products with brands and colours counted', () async {
    final db = _Recording();
    await db.from('products').where('price', '<', 500).limit(20).facet('brand').facet('color').rows();
    db.sends('get products where price < \$1 limit \$2 facet brand, color', '[500, 20]');
  });

  test('docs: a page of matches with brands and tags counted', () async {
    final db = _Recording();
    await db
        .from('products')
        .where('price', '<', 500)
        .match('title', 'tent')
        .limit(20)
        .facet('brand', top: 10)
        .facet('tags')
        .rows();
    db.sends(
        'get products where price < \$1 match title \$2 limit \$3 facet brand top \$4, tags', '[500, "tent", 20, 10]');
  });

  test('docs: phones counted by brand', () async {
    final db = _Recording();
    await db.from('products').where('category', 'phones').facet('brand').count();
    db.sends('get products where category = \$1 count facet brand', '["phones"]');
  });

  test('docs: products counted by origin alone', () async {
    final db = _Recording();
    await db.from('products').limit(0).facet('meta.origin', top: 5).rows();
    db.sends('get products limit \$1 facet meta.origin top \$2', '[0, 5]');
  });

  test('docs: red tents of two brands with their facets', () async {
    final db = _Recording();
    await db
        .from('products')
        .where('category', 'tents')
        .where('brand', 'in', ['acme', 'nova'])
        .where('color', 'red')
        .order('price')
        .limit(24)
        .facet('brand', disjunctive: true)
        .facet('color', disjunctive: true)
        .facet('material')
        .rows();
    db.sends(
        'get products where category = \$1 and brand in [\$2, \$3] and color = \$4 order price asc limit \$5 facet brand disjunctive, color disjunctive, material',
        '["tents", "acme", "nova", "red", 24]');
  });

  test('docs: tents counted by price range', () async {
    final db = _Recording();
    await db
        .from('products')
        .where('category', 'tents')
        .limit(24)
        .facet('price', ranges: [0, 2500, 5000, 10000, 25000], disjunctive: true)
        .rows();
    db.sends('get products where category = \$1 limit \$2 facet price ranges [0, 2500, 5000, 10000, 25000] disjunctive',
        '["tents", 24]');
  });

  test('docs: the nearest articles', () async {
    final db = _Recording();
    await db.from('articles').near('embed', [0.9, 0.1, 0]).limit(10).rows();
    db.sends('get articles near embed \$1 limit \$2', '[[0.9, 0.1, 0], 10]');
  });

  test('docs: the nearest articles since a year', () async {
    final db = _Recording();
    await db.from('articles').where('year', '>=', 2024).near('embed', [0.9, 0.1, 0], ef: 200).limit(10).rows();
    db.sends('get articles where year >= \$1 near embed \$2 ef 200 limit \$3', '[2024, [0.9, 0.1, 0], 10]');
  });

  test('docs: the nearest articles exactly', () async {
    final db = _Recording();
    await db.from('articles').near('embed', [0.9, 0.1, 0], exact: true).limit(10).rows();
    db.sends('get articles near embed \$1 exact limit \$2', '[[0.9, 0.1, 0], 10]');
  });

  test('docs: the nearest articles filtered', () async {
    final db = _Recording();
    await db
        .from('articles')
        .select(['title', 'year'])
        .where('year', '>=', 2024)
        .where('tags', 'has', 'rust')
        .where(Cond.not(Cond.cmp('title', '~', 'draft')))
        .near('embed', [0.9, 0.1, 0], ef: 128)
        .limit(10)
        .rows();
    db.sends(
        'get articles select title, year where year >= \$1 and tags has \$2 and not (title ~ \$3) near embed \$4 ef 128 limit \$5',
        '[2024, "rust", "draft", [0.9, 0.1, 0], 10]');
  });

  test('docs: a hit counted and stamped', () async {
    final db = _Recording();
    await db.from('hits').where('key', 'ip1').update({'n': Computed.expr('n + 1'), 'seen': Computed.expr('now()')});
    db.sends('set hits {n: n + 1, seen: now()} where key = \$1', '["ip1"]');
  });

  test('docs: a hit counted from nothing', () async {
    final db = _Recording();
    await db.from('hits').where('key', 'ip1').update({'n': Computed.inc(5)});
    db.sends('set hits {n: coalesce(n, 0) + \$1} where key = \$2', '[5, "ip1"]');
  });

  test('docs: a document written over its version', () async {
    final db = _Recording();
    await db
        .from('docs')
        .where({'id': 7, 'version': 3}).update({'body': 'new text', 'version': Computed.expr('version + 1')});
    db.sends('set docs {body: \$1, version: version + 1} where id = \$2 and version = \$3', '["new text", 7, 3]');
  });

  test('docs: a lock taken unless held', () async {
    final db = _Recording();
    await db.from('locks').insert({'name': 'nightly', 'owner': 'w1', 'at': Computed.expr('now()')}, ifAbsent: true);
    db.sends('put locks {name: \$1, owner: \$2, at: now()} if absent', '["nightly", "w1"]');
  });

  test('docs: a journal entry made', () async {
    final db = _Recording();
    await db.from('journal').insertNew({
      'entry': Computed.expr('? + ":dr"', ['inv-7']),
      'amount': Computed.expr('0 - ?', [300])
    });
    db.sends('insert journal {entry: \$1 + ":dr", amount: 0 - \$2}', '["inv-7", 300]');
  });

  test('docs: an amount taken from a balance that holds it', () async {
    final db = _Recording();
    final amount = Param(300);
    await db.from('accounts').where({
      'name': 'ada',
      'balance': {'gte': amount}
    }).update({
      'balance': Computed.expr('balance - ?', [amount])
    }, require: 1);
    db.sends('set accounts {balance: balance - \$1} where name = \$2 and balance >= \$1 require 1', '[300, "ada"]');
  });

  test('docs: a held hold released', () async {
    final db = _Recording();
    await db.from('holds').where({'order_ref': 'o-17', 'state': 'held'}).delete(require: 1);
    db.sends('del holds where order_ref = \$1 and state = \$2 require 1', '["o-17", "held"]');
  });

  test('docs: a lock taken or refused', () async {
    final db = _Recording();
    await db.from('locks').insert({'name': 'nightly', 'owner': 'w1'}, ifAbsent: true, require: 1);
    db.sends('put locks {name: \$1, owner: \$2} if absent require 1', '["nightly", "w1"]');
  });

  test('docs: a debit that must find the money, or says why', () async {
    final db = _Recording();
    await db.from('accounts').where('name', 'ada').where('balance', '>=', 30).update({
      'balance': Computed.expr('balance - ?', [30])
    }, require: 1, reason: 'insufficient funds');
    db.sends(
        'set accounts {balance: balance - \$1} where name = \$2 and balance >= \$3 require 1 else \'insufficient funds\'',
        '[30, "ada", 30]');
  });

  test('docs: a hold deleted that must be there, or says why', () async {
    final db = _Recording();
    await db
        .from('holds')
        .where('order_ref', 'o-17')
        .where('state', 'held')
        .delete(require: 1, reason: 'it\'s not held');
    db.sends('del holds where order_ref = \$1 and state = \$2 require 1 else \'it\'\'s not held\'', '["o-17", "held"]');
  });

  test('docs: a coupon that must still be usable, or says why', () async {
    final db = _Recording();
    await db
        .from('coupons')
        .where('code', 'X1')
        .where(Cond.raw('uses < max_uses'))
        .limit(1)
        .require(1, reason: 'coupon used up')
        .rows();
    db.sends(
        'get coupons where code = \$1 and uses < max_uses limit \$2 require 1 else \'coupon used up\'', '["X1", 1]');
  });

  test('docs: a product that must be there at its price', () async {
    final db = _Recording();
    await db.from('products').where('sku', 'tee').where('price', 1200).where('stock', '>', 0).require(1).rows();
    db.sends('get products where sku = \$1 and price = \$2 and stock > \$3 require 1', '["tee", 1200, 0]');
  });

  test('docs: a coupon that must still be usable', () async {
    final db = _Recording();
    await db.from('coupons').where('code', 'X1').where(Cond.raw('uses < max_uses')).limit(1).require(1).rows();
    db.sends('get coupons where code = \$1 and uses < max_uses limit \$2 require 1', '["X1", 1]');
  });

  test('docs: no hold that must be there', () async {
    final db = _Recording();
    await db.from('holds').where('order_ref', 'o-17').where('state', 'held').require(0).rows();
    db.sends('get holds where order_ref = \$1 and state = \$2 require 0', '["o-17", "held"]');
  });

  test('docs: two jobs queued', () async {
    final db = _Recording();
    await db.from('jobs').insert([
      {
        'payload': {'to': 'ada'},
        'run_at': Computed.expr('now()'),
        'attempts': 0
      },
      {
        'payload': {'to': 'bo'},
        'run_at': Computed.expr('now()'),
        'attempts': 0
      }
    ]);
    db.sends('put jobs [{payload: \$1, run_at: now(), attempts: \$2}, {payload: \$3, run_at: now(), attempts: \$4}]',
        '[{"to": "ada"}, 0, {"to": "bo"}, 0]');
  });

  test('docs: the ten oldest ready jobs claimed', () async {
    final db = _Recording();
    await db.from('jobs').where(Cond.raw('run_at <= now()')).order('run_at').limit(10).updateReturning({
      'owner': 'w1',
      'run_at': Computed.expr('now() + ?', [30000]),
      'attempts': Computed.expr('attempts + 1')
    });
    db.sends(
        'set jobs {owner: \$1, run_at: now() + \$2, attempts: attempts + 1} where run_at <= now() order run_at asc limit \$3 returning *',
        '["w1", 30000, 10]');
  });

  test('docs: a job acknowledged', () async {
    final db = _Recording();
    await db.from('jobs').where('id', 1).where('owner', 'w1').delete(require: 1);
    db.sends('del jobs where id = \$1 and owner = \$2 require 1', '[1, "w1"]');
  });

  test('docs: ready jobs popped', () async {
    final db = _Recording();
    await db
        .from('jobs')
        .where(Cond.raw('run_at <= now()'))
        .order('run_at')
        .limit(10)
        .deleteReturning(returning: ['id', 'payload.to']);
    db.sends('del jobs where run_at <= now() order run_at asc limit \$1 returning id, payload.to', '[10]');
  });

  test('docs: an article set by id', () async {
    final db = _Recording();
    await db.from('articles').where('id', 42).update({'year': 2025});
    db.sends('set articles {year: \$1} where id = \$2', '[2025, 42]');
  });

  test('docs: an article\'s views counted', () async {
    final db = _Recording();
    await db.from('articles').where('id', 42).update({'views': Computed.inc(1)});
    db.sends('set articles {views: coalesce(views, 0) + \$1} where id = \$2', '[1, 42]');
  });

  test('docs: the articles before a year deleted', () async {
    final db = _Recording();
    await db.from('articles').where('year', '<', 2000).delete();
    db.sends('del articles where year < \$1', '[2000]');
  });

  test('docs: a transfer called', () async {
    final db = _Recording();
    await db.callStatement('transfer', args: {'from': 1, 'to': 2, 'amount': 300});
    db.sends('call transfer \$1', '[{"from": 1, "to": 2, "amount": 300}]');
  });

  test('docs: a statement called for a signed-in user', () async {
    final db = _Recording();
    await db.callStatement('history', args: {'account': 1}, claims: {'sub': 'alice'});
    db.sends('call history \$1 as \$2', '[{"account": 1}, {"sub": "alice"}]');
  });

  test('docs: a page of documents with their comments', () async {
    final db = _Recording();
    await db
        .from('docs')
        .where('year', '>=', 2024)
        .where(Cond.or([
          {'lang': 'tr'},
          Cond.cmp('tags', 'has', 'rust')
        ]))
        .order('title', 'asc', 'tr')
        .limit(20)
        .lookup('comments', on: 'doc_id', limit: 3)
        .rows();
    db.sends(
        'get docs where year >= \$1 and (lang = \$2 or tags has \$3) order title collate tr asc limit \$4 lookup comments on doc_id limit \$5',
        '[2024, "tr", "rust", 20, 3]');
  });

  test('docs: documents matching a misspelt word', () async {
    final db = _Recording();
    await db.from('docs').where('year', '>=', 2024).match('title', 'databse', fuzzy: 2).limit(10).rows();
    db.sends('get docs where year >= \$1 match title \$2 fuzzy 2 limit \$3', '[2024, "databse", 10]');
  });

  test('docs: the document of a slug', () async {
    final db = _Recording();
    await db.from('docs').where('slug', 'intro').first();
    db.sends('get docs where slug = \$1 limit \$2', '["intro", 1]');
  });

  test('docs: documents counted since a year', () async {
    final db = _Recording();
    await db.from('docs').where('year', '>=', 2024).count();
    db.sends('get docs where year >= \$1 count', '[2024]');
  });

  test('docs: a document inserted', () async {
    final db = _Recording();
    await db.from('docs').insert({'slug': 'intro', 'title': 'Ownership in Rust', 'year': 2026});
    db.sends('put docs {slug: \$1, title: \$2, year: \$3}', '["intro", "Ownership in Rust", 2026]');
  });

  test('docs: a document inserted unless its slug is held', () async {
    final db = _Recording();
    await db.from('docs').insert({'slug': 'intro', 'title': 'Ownership in Rust'}, ifAbsent: true);
    db.sends('put docs {slug: \$1, title: \$2} if absent', '["intro", "Ownership in Rust"]');
  });

  test('docs: a document\'s views counted', () async {
    final db = _Recording();
    await db.from('docs').where('slug', 'intro').update({'views': Computed.inc(1)});
    db.sends('set docs {views: coalesce(views, 0) + \$1} where slug = \$2', '[1, "intro"]');
  });

  test('docs: the documents before a year deleted', () async {
    final db = _Recording();
    await db.from('docs').where('year', '<', 2000).delete();
    db.sends('del docs where year < \$1', '[2000]');
  });

  test('docs: a statement of no arguments called', () async {
    final db = _Recording();
    await db.callStatement('stamp');
    db.sends('call stamp', '[]');
  });

  test('escapes: every character a literal escapes, in a filter', () async {
    final db = _Recording();
    await db
        .from('docs')
        .where('title', '\u0000\u0001\u0009\u000a\u000d\u001f\u0085\u2028\u2029 "\'\\\$`\${x}\$1\\(x) …')
        .rows();
    db.sends('get docs where title = \$1',
        '["\\u0000\\u0001\\t\\n\\r\\u001f\u0085\u2028\u2029 \\"\'\\\\\$`\${x}\$1\\\\(x) …"]');
  });

  test('escapes: line ends, in a document', () async {
    final db = _Recording();
    await db.from('docs').where('id', 1).update({
      'title': 'one\u2028two\u000d\u000athree\u0085four\u2029',
      'note': '\u0000\u0001\u0009\u000a\u000d\u001f\u0085\u2028\u2029 "\'\\\$`\${x}\$1\\(x) …'
    });
    db.sends('set docs {title: \$1, note: \$2} where id = \$3',
        '["one\u2028two\\r\\nthree\u0085four\u2029", "\\u0000\\u0001\\t\\n\\r\\u001f\u0085\u2028\u2029 \\"\'\\\\\$`\${x}\$1\\\\(x) …", 1]');
  });
}
