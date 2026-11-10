# The native library for macOS: the dynamic XCFramework's macOS slice,
# FennecFFI.framework (prepare.sh copies it into Frameworks/), embedded by
# CocoaPods and opened by name from Dart, as on iOS.
Pod::Spec.new do |s|
  s.name             = 'fennecdb_flutter'
  s.version          = '0.2.0'
  s.summary          = 'fennecdb in a Flutter app: the native library for macOS.'
  s.homepage         = 'https://github.com/fennec-db/fennecdb'
  s.license          = { :type => 'Apache-2.0' }
  s.author           = { 'fennecdb' => 'https://github.com/fennec-db' }
  s.source           = { :path => '.' }
  s.platform         = :osx, '12.0'
  s.vendored_frameworks = 'Frameworks/FennecFFI.xcframework'
  s.dependency 'FlutterMacOS'
end
