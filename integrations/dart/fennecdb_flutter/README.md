# fennecdb for Flutter

The `fennecdb` package in a Flutter app, with its native library bundled,
each platform's once: `FennecFFI.framework`, a dynamic framework, for the
iOS device and the simulator (iOS 15 on); `libfennec_ffi.dylib`, arm64 and
x86_64, for macOS (macOS 11 on); `libfennec_ffi.so` for arm64-v8a,
armeabi-v7a and x86_64 on Android (Android 5, API 21, on). CocoaPods
embeds the Apple ones in the app, and an app ships its own platform's
alone.

```dart
import 'package:fennecdb_flutter/fennecdb_flutter.dart';
import 'package:path_provider/path_provider.dart';

final dir = await getApplicationDocumentsDirectory();
final db = await Fennec.open('${dir.path}/app.fennec');

StreamBuilder(
  stream: db.live(db.from('todos').where('done', false)),
  builder: (context, snap) => ListView(children: [
    for (final t in snap.data ?? const []) Text(t['title'] as String),
  ]),
);
```

`example/` is a todo list over a live query; its `integration_test/` opens
a file through the library the app embeds. In the repository, `prepare.sh`
copies the libraries in from the Swift and Kotlin builds
(`integrations/swift/build-xcframework.sh --dynamic`,
`integrations/kotlin/build-aar.sh`); `--platforms ios,macos,android`
prepares those alone, the default is all three and a missing one an error.
