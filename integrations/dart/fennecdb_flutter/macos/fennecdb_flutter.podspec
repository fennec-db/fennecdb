# The native library for macOS: libfennec_ffi.dylib, arm64 and x86_64, the
# dynamic XCFramework's macOS framework binary under a name of its own, which
# integrations/swift/build-xcframework.sh --dynamic makes beside the
# XCFramework and prepare.sh copies into Libraries/. CocoaPods embeds a
# vendored dynamic library in the app's Frameworks and signs it, as it does
# a framework; the app links it by its install name,
# @rpath/libfennec_ffi.dylib, and Dart opens it by name. Not a framework, as
# on iOS: a macOS framework is versioned, its binary reached through
# symbolic links, and pub packs what a link points at -- 0.2.0 shipped that
# binary three times over.
Pod::Spec.new do |s|
  s.name             = 'fennecdb_flutter'
  s.version          = '0.2.0'
  s.summary          = 'fennecdb in a Flutter app: the native library for macOS.'
  s.homepage         = 'https://github.com/fennec-db/fennecdb'
  s.license          = { :type => 'Apache-2.0' }
  s.author           = { 'fennecdb' => 'https://github.com/fennec-db' }
  s.source           = { :path => '.' }
  # The library's own minimum (its LC_BUILD_VERSION): the lowest macOS
  # aarch64-apple-darwin builds for.
  s.platform         = :osx, '11.0'
  s.vendored_libraries = 'Libraries/libfennec_ffi.dylib'
  s.dependency 'FlutterMacOS'
end
