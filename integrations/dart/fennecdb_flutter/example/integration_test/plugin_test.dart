// The library the plugin bundles, opened inside the app where Flutter's
// build put it -- not the one FENNEC_LIBRARY names, as the plugin's own
// test opens. CI runs it on macOS on the plugin packed as pub.dev gets it:
// `flutter test integration_test -d macos`.
import 'dart:io';

import 'package:fennecdb_flutter/fennecdb_flutter.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('a file through the library the app embeds', (tester) async {
    expect(await Fennec.version(), isNotEmpty);
    final dir = await Directory.systemTemp.createTemp('fennecdb');
    final db = await Fennec.open('${dir.path}/todos.fennec');
    await db.execute('create collection todos (title text, done bool @hash)');
    await db.from('todos').insert({'title': 'milk', 'done': false});
    final open = await db.from('todos').where('done', false).rows();
    expect(open.map((r) => r['title']), ['milk']);
    await db.close();
    await dir.delete(recursive: true);
  });
}
