/// fennecdb in a Flutter app: the `fennecdb` package, its native library
/// bundled by this plugin for iOS (`FennecFFI.framework`, a dynamic
/// framework CocoaPods embeds in the app), macOS (`libfennec_ffi.dylib`,
/// embedded the same way) and Android (`libfennec_ffi.so` for each ABI).
///
/// ```dart
/// final dir = await getApplicationDocumentsDirectory();   // path_provider
/// final db = await Fennec.open('${dir.path}/app.fennec');
///
/// StreamBuilder(
///   stream: db.live(db.from('todos').where('done', false)),
///   builder: (context, snap) => ListView(children: [
///     for (final t in snap.data ?? const []) Text(t['title'] as String),
///   ]),
/// );
/// ```
library;

export 'package:fennecdb/fennecdb.dart';
