#
# To learn more about a Podspec see http://guides.cocoapods.org/syntax/podspec.html.
# Run `pod lib lint rust_lib_headphone_for_all.podspec` to validate before publishing.
#
Pod::Spec.new do |s|
  s.name             = 'rust_lib_headphone_for_all'
  s.version          = '0.0.1'
  s.summary          = 'A new Flutter FFI plugin project.'
  s.description      = <<-DESC
A new Flutter FFI plugin project.
                       DESC
  s.homepage         = 'http://example.com'
  s.license          = { :file => '../LICENSE' }
  s.author           = { 'Your Company' => 'email@example.com' }

  # This will ensure the source files in Classes/ are included in the native
  # builds of apps using this FFI plugin. Podspec does not support relative
  # paths, so Classes contains a forwarder C file that relatively imports
  # `../src/*` so that the C sources can be shared among all target platforms.
  s.source           = { :path => '.' }
  s.source_files     = 'Classes/**/*'
  s.dependency 'FlutterMacOS'

  s.platform = :osx, '10.15'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
  s.swift_version = '5.0'

  # headphone-for-all: a Rust staticlib does not carry the `#[link(kind = "framework")]`
  # directives of its dependencies (objc2-* crates used by hfa-capture / cpal), and the
  # -force_load below pulls in every object, so the pod must link these system frameworks
  # (and libobjc) itself. Keep in sync with `native-static-libs` of hfa_ffi for this target
  # (docs/CONTRACTS.md §8.5).
  s.frameworks = ['CoreAudio', 'AudioToolbox', 'CoreFoundation', 'Foundation']
  s.libraries = ['objc']

  s.script_phase = {
    :name => 'Build Rust library',
    # First argument is the relative path to the Rust crate (core/hfa-ffi), second is the name
    # of the Rust library.
    :script => 'sh "$PODS_TARGET_SRCROOT/../cargokit/build_pod.sh" ../../../core/hfa-ffi hfa_ffi',
    :execution_position => :before_compile,
    :input_files => ['${BUILT_PRODUCTS_DIR}/cargokit_phony'],
    # Let XCode know that the static library referenced in -force_load below is
    # created by this build step.
    :output_files => ["${BUILT_PRODUCTS_DIR}/libhfa_ffi.a"],
  }
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    # Flutter.framework does not contain a i386 slice.
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
    'OTHER_LDFLAGS' => '-force_load ${BUILT_PRODUCTS_DIR}/libhfa_ffi.a',
  }
end